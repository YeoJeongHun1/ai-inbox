//! 운영 코노티 호환 시험 — docs/COMPAT.md 의 근거.
//!
//! "구 폰" = 스토어에 배포된 코노티 앱(1.5.0 · 1.6.0)의 AI 작업 클라이언트. 그 앱은 심사를 거쳐야만 바뀌므로
//! AI Inbox 가 그 앱을 계속 견뎌야 한다. 여기서 세 가지를 고정한다.
//!
//! 1. **기존 필드 불변**: 0.8.0 이 같은 입력에 낸 응답(`tests/fixtures/compat_080_snapshot.json`, 0.8.0 트리에서 만든 것)의
//!    모든 필드가 0.10.0 에서 이름·값이 그대로다. 새로 생긴 필드는 아래 `ADDED` 목록에 있는 것뿐이다 —
//!    목록에 없는 새 필드가 생기면 시험이 깨지므로 폰이 견디는지 사람이 한 번 본다.
//! 2. **폰이 읽은 값이 같다**: 폰이 응답을 읽는 규칙(docs/COMPAT.md §2-2)을 그대로 옮긴 파서로 0.8.0 응답과 0.10.0 응답을
//!    읽으면 결과가 같다(모르는 필드는 폰이 버리므로, 폰 입장에서의 동등성).
//! 3. **형식이 고정된 자리가 안 깨진다**: 폰이 리스트·객체·문자열로만 받는 자리(COMPAT.md §2-2 표)는 타입이 유지된다.
//!    (그 밖의 자리는 폰이 관대하게 — 타입이 다르면 기본값으로 — 읽어 모르는 값·null 에서 예외가 나지 않는다.)
//!
//! 이 시험의 입력(`compat_driver.rs`)은 0.8.0 트리에도 글자 그대로 들어가 기준선을 만든다.

use super::compat_driver::*;
use super::*;
use std::collections::BTreeSet;

const SCHEMA_V10: &str = include_str!("../../tests/fixtures/compat_v10_schema.sql");
const SNAP_080: &str = include_str!("../../tests/fixtures/compat_080_snapshot.json");

/// 0.10.0 이 옛 응답에 더한 필드(전부 선택 필드 — 폰이 읽지 않으면 무시된다). 이 밖의 새 필드는 허용하지 않는다.
const ADDED: &[&str] = &[
    "hello:r.can_schedule",
    "sessions:r.can_schedule",
    "sessions:r.items[].ended",
    "sessions:r.items[].perm",
    "sessions:r.items[].sched_held",
    "sessions:r.items[].sched_n",
    "sessions:r.items[].sched_warn",
    "sessions:r.items[].tags",
    "chat:r.session.sched_held",
    "chat:r.session.sched_n",
    "chat:r.session.tags",
    "chat:r.turns[].tags",
];

fn temp_data_dir() {
    let d = std::env::temp_dir().join(format!("aiinbox-compat-{}", std::process::id()));
    crate::paths::set_data_dir_override(d);
}

/// 0.8.0(v10) 모양의 DB 에 지어낸 데이터를 넣은 뒤 **0.10.0 이 마이그레이션**한 DB — 업그레이드한 사용자와 같다.
fn upgraded_db() -> Connection {
    temp_data_dir();
    // 기준선(0.8.0)은 한국 시간(UTC+9)에서 만들어졌다 — 러너가 UTC 여도 같은 화면 시각이 나오게 고정
    crate::doc::TEST_UTC_OFFSET.with(|o| o.set(Some(9 * 3600)));
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(SCHEMA_V10).unwrap();
    db::set_meta(&c, "schema_version", "10").unwrap();
    seed(&c);
    db::migrate(&c).unwrap();
    assert_eq!(db::get_meta(&c, "schema_version").as_deref(), Some(&*db::SCHEMA_VERSION.to_string()));
    c
}

// ── 1. 기존 필드 불변 ────────────────────────────────────────────────────────

/// 응답 시각처럼 실행마다 달라지는 값 — 값은 같지 않아도 되고 종류(문자열)만 같으면 된다
fn volatile(key: &str) -> bool {
    key == "at"
}

fn diff(old: &Value, new: &Value, path: &str, key: &str, added: &mut BTreeSet<String>, bad: &mut Vec<String>) {
    match (old, new) {
        (Value::Object(o), Value::Object(n)) => {
            for (k, v) in o {
                match n.get(k) {
                    None => bad.push(format!("{path}.{k}: 0.8.0 에 있던 필드가 사라짐")),
                    Some(nv) => diff(v, nv, &format!("{path}.{k}"), k, added, bad),
                }
            }
            for k in n.keys().filter(|k| !o.contains_key(*k)) {
                added.insert(format!("{path}.{k}"));
            }
        }
        (Value::Array(o), Value::Array(n)) => {
            if o.len() != n.len() {
                bad.push(format!("{path}: 길이 {} → {}", o.len(), n.len()));
                return;
            }
            for (a, b) in o.iter().zip(n) {
                diff(a, b, &format!("{path}[]"), key, added, bad);
            }
        }
        (a, b) => {
            let same_kind = std::mem::discriminant(a) == std::mem::discriminant(b);
            if volatile(key) {
                if !same_kind {
                    bad.push(format!("{path}: 종류가 바뀜 {a} → {b}"));
                }
            } else if a != b {
                bad.push(format!("{path}: {a} → {b}"));
            }
        }
    }
}

/// 호출 이름 앞부분(hello·sessions·chat…) — 같은 응답 모양끼리 묶어 새 필드를 센다
fn kind_of(call: &str) -> &str {
    call.split('_').next().unwrap_or(call)
}

fn compare_with_snapshot(new: &Value) -> (BTreeSet<String>, Vec<String>) {
    let old: Value = serde_json::from_str(SNAP_080).unwrap();
    let mut added = BTreeSet::new();
    let mut bad = Vec::new();
    for (name, o) in old.as_object().unwrap() {
        let Some(n) = new.get(name) else {
            bad.push(format!("{name}: 호출 결과가 없음"));
            continue;
        };
        let mut a = BTreeSet::new();
        let mut local_bad = Vec::new();
        diff(o, n, "", "", &mut a, &mut local_bad);
        bad.extend(local_bad.into_iter().map(|m| format!("{name}{m}")));
        added.extend(a.into_iter().map(|p| format!("{}:{}", kind_of(name), p.trim_start_matches('.'))));
    }
    (added, bad)
}

#[test]
fn old_phone_script_keeps_every_080_field() {
    let c = upgraded_db();
    let new = run_script(&c);
    let (added, bad) = compare_with_snapshot(&new);
    assert!(bad.is_empty(), "0.8.0 이 준 필드가 바뀜 — 운영 폰이 깨질 수 있다:\n{}", bad.join("\n"));
    let expected: BTreeSet<String> = ADDED.iter().map(|s| s.to_string()).collect();
    assert_eq!(added, expected, "0.10.0 이 더한 필드 목록이 다르다 — 새 필드는 폰이 견디는지 확인하고 ADDED 와 docs/COMPAT.md 에 적는다");
}

// ── 2·3. 폰과 같은 규칙으로 읽기 ──────────────────────────────────────────────

/// 문자열 자리 — 문자열이 아니면 null
fn s(v: &Value) -> Value {
    if v.is_string() { v.clone() } else { Value::Null }
}
/// 정수 자리 — 정수·숫자면 정수, 아니면 0
fn i(v: &Value) -> Value {
    json!(v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).unwrap_or(0))
}
/// 없을 수 있는 정수 자리 — 숫자면 정수, 아니면 null
fn i_null(v: &Value) -> Value {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)).map_or(Value::Null, |n| json!(n))
}
/// 불리언 자리 — true 일 때만 true
fn b(v: &Value) -> Value {
    json!(v == &json!(true))
}
/// `_t` — ISO 시각으로 읽히면 그 값, 아니면 null
fn t(v: &Value) -> Value {
    match v.as_str() {
        Some(x) if chrono::DateTime::parse_from_rfc3339(x).is_ok() => v.clone(),
        _ => Value::Null,
    }
}
fn or(v: Value, d: &str) -> Value {
    if v.is_null() { json!(d) } else { v }
}
/// `_ids` — 문자열만, 최대 5개
fn ids(v: &Value) -> Value {
    Value::Array(v.as_array().map(|a| a.iter().filter(|x| x.as_str().is_some_and(|s| !s.is_empty())).take(5).cloned().collect()).unwrap_or_default())
}
fn agent(v: &Value) -> Value {
    json!(if v == &json!("codex") { "codex" } else { "claude" })
}
/// 폰이 `as List? ?? const []` 로 강제하는 자리 — 리스트가 아닌 값이면 폰이 예외를 낸다
fn strict_list<'a>(v: &'a Value, what: &str, errs: &mut Vec<String>) -> &'a [Value] {
    match v {
        Value::Array(a) => a,
        Value::Null => &[],
        other => {
            errs.push(format!("{what}: 리스트여야 함 ({other})"));
            &[]
        }
    }
}

fn quote(v: &Value) -> Value {
    let Some(m) = v.as_object() else { return Value::Null };
    let part = m.get("part").and_then(Value::as_str);
    if !matches!(part, Some("prompt" | "response")) {
        return Value::Null;
    }
    let seq = i_null(m.get("seq").unwrap_or(&Value::Null));
    if seq.is_null() {
        return Value::Null;
    }
    json!({"seq": seq, "part": part, "text": s(m.get("text").unwrap_or(&Value::Null))})
}

fn phone_session(j: &Value) -> Value {
    json!({
        "id": or(s(&j["id"]), ""), "name": or(s(&j["name"]), ""), "named": b(&j["named"]), "project": s(&j["project"]),
        "dir": s(&j["dir"]), "branch": s(&j["branch"]), "live": s(&j["live"]), "pinned": b(&j["pinned"]), "model": s(&j["model"]),
        "turns": i(&j["turns"]), "unread": i(&j["unread"]), "attention": i(&j["attention"]), "active": i(&j["active"]),
        "last_status": or(s(&j["last_status"]), "done"), "last_needs_input": b(&j["last_needs_input"]), "preview": or(s(&j["preview"]), ""),
        "preview_ai": b(&j["preview_ai"]), "last_at": t(&j["last_at"]), "archived": b(&j["archived"]), "agent": agent(&j["agent"]),
    })
}

fn phone_session_list(r: &Value, errs: &mut Vec<String>) -> Value {
    let items: Vec<Value> = strict_list(&r["items"], "sessions.items", errs).iter().filter(|x| x.is_object()).map(phone_session).collect();
    json!({
        "items": items, "unread": i(&r["unread"]), "attention": i(&r["attention"]), "active": i(&r["active"]),
        "can_manage": b(&r["can_manage"]), "archived": i(&r["archived"]), "tidy_short": i(&r["tidy_short"]), "tidy_idle": i(&r["tidy_idle"]),
    })
}

fn phone_head(j: &Value) -> Value {
    json!({
        "id": or(s(&j["id"]), ""), "name": or(s(&j["name"]), ""), "named": b(&j["named"]), "project": s(&j["project"]), "dir": s(&j["dir"]),
        "branch": s(&j["branch"]), "live": s(&j["live"]), "model": s(&j["model"]), "cost_usd": if j["cost_usd"].is_number() { j["cost_usd"].clone() } else { Value::Null },
        "turns": i(&j["turns"]), "unread": i(&j["unread"]), "can_reply": b(&j["can_reply"]), "reply_block": or(s(&j["reply_block"]), ""),
        "channel": b(&j["channel"]), "pinned": b(&j["pinned"]), "archived": b(&j["archived"]), "agent": agent(&j["agent"]),
    })
}

fn phone_bubble(j: &Value) -> Value {
    json!({
        "id": i(&j["id"]), "seq": i(&j["seq"]), "origin": s(&j["origin"]), "peer": s(&j["peer"]), "via_phone": b(&j["via_phone"]),
        "mid_turn": b(&j["mid_turn"]), "prompt_at": t(&j["prompt_at"]), "prompt": s(&j["prompt"]), "slash": s(&j["slash"]),
        "status": or(s(&j["status"]), "done"), "needs_input": b(&j["needs_input"]), "pending_bg": i(&j["pending_bg"]),
        "summary": s(&j["summary"]), "response": s(&j["response"]), "step": s(&j["step"]), "ended_at": t(&j["ended_at"]),
        "duration_ms": i_null(&j["duration_ms"]), "tool_calls": i(&j["tool_calls"]), "files": i(&j["files"]), "agents": i(&j["agents"]),
        "out_tokens": i(&j["out_tokens"]), "model": s(&j["model"]), "unread": b(&j["unread"]), "starred": b(&j["starred"]),
        "atts": ids(&j["atts"]), "quote": quote(&j["quote"]),
    })
}

/// 폰의 `RelayReply` — `at` 은 응답 시각이라 실행마다 다르다(있는지만 본다)
fn phone_reply(j: &Value) -> Value {
    json!({
        "rid": or(s(&j["rid"]), ""), "text": or(s(&j["text"]), ""), "state": or(s(&j["state"]), "delivering"), "note": or(s(&j["note"]), ""),
        "has_at": !t(&j["at"]).is_null(), "atts": ids(&j["atts"]), "quote": quote(&j["quote"]),
    })
}

fn phone_chat(r: &Value, errs: &mut Vec<String>) -> Value {
    if !(r["session"].is_object() || r["session"].is_null()) {
        errs.push("chat.session: Map 이어야 함".into());
    }
    let turns: Vec<Value> = strict_list(&r["turns"], "chat.turns", errs).iter().filter(|x| x.is_object()).map(phone_bubble).collect();
    let replies: Vec<Value> = strict_list(&r["replies"], "chat.replies", errs).iter().filter(|x| x.is_object()).map(phone_reply).collect();
    json!({"session": phone_head(&r["session"]), "turns": turns, "has_more": b(&r["has_more"]), "replies": replies})
}

fn phone_turn_doc(r: &Value) -> Value {
    json!({
        "id": i(&r["id"]), "sid": or(s(&r["sid"]), ""), "title": or(s(&r["title"]), ""), "markdown": or(s(&r["markdown"]), ""),
        "status": or(s(&r["status"]), "done"), "needs_input": b(&r["needs_input"]), "prev_id": i_null(&r["prev_id"]), "next_id": i_null(&r["next_id"]),
    })
}

fn phone_manage(r: &Value, errs: &mut Vec<String>) -> Value {
    let strs = |v: &Value, w: &str, e: &mut Vec<String>| -> Vec<Value> { strict_list(v, w, e).iter().filter(|x| x.is_string()).cloned().collect() };
    let n = i(&r["n"]);
    let deleted = strs(&r["deleted"], "manage.deleted", errs);
    let skipped = strs(&r["skipped"], "manage.skipped", errs);
    json!({"n": n, "deleted": deleted, "skipped": skipped})
}

/// 응답 봉투를 폰이 읽는 대로 — `id` 는 int, `ok == true` 면 `r`(Map 아니면 빈 맵), 아니면 오류(`err`·`msg` 문자열화)
fn phone_envelope(name: &str, resp: &Value, errs: &mut Vec<String>) -> Value {
    if !resp["id"].is_i64() {
        errs.push(format!("{name}: 봉투 id 가 int 가 아님"));
    }
    if resp["ok"] != json!(true) {
        return json!({"error": resp["err"].as_str().unwrap_or("error"), "msg": resp["msg"].as_str().unwrap_or("")});
    }
    let r = if resp["r"].is_object() { resp["r"].clone() } else { json!({}) };
    if name.starts_with("hello") {
        if !(r["desktop"].is_string() || r["desktop"].is_null()) {
            errs.push("hello.desktop: String 이어야 함".into());
        }
        return json!({"desktop": s(&r["desktop"]), "can_reply": b(&r["can_reply"]), "can_manage": b(&r["can_manage"])});
    }
    if name.starts_with("sessions") {
        return phone_session_list(&r, errs);
    }
    if name.starts_with("chat") {
        return phone_chat(&r, errs);
    }
    if name.starts_with("turn") {
        return phone_turn_doc(&r);
    }
    if name.starts_with("reply") {
        return phone_reply(&r);
    }
    if name.starts_with("manage") {
        return phone_manage(&r, errs);
    }
    if name == "att_up" {
        return json!({"id": or(s(&r["id"]), ""), "w": i(&r["w"]), "h": i(&r["h"]), "bytes": i(&r["bytes"])});
    }
    if name.starts_with("att_get") {
        use base64::Engine;
        let bytes = r["data"].as_str().and_then(|d| base64::engine::general_purpose::STANDARD.decode(d.trim_end_matches('=')).ok().or_else(|| base64::engine::general_purpose::STANDARD.decode(d).ok()));
        return json!({"id": or(s(&r["id"]), ""), "mime": s(&r["mime"]), "w": i(&r["w"]), "h": i(&r["h"]), "len": bytes.map(|b| b.len())});
    }
    if name.starts_with("tidy") {
        return json!({"n": i(&r["n"])});
    }
    json!({"ok": true})
}

#[test]
fn old_phone_reads_the_same_values_from_080_and_0100() {
    let c = upgraded_db();
    let new = run_script(&c);
    let old: Value = serde_json::from_str(SNAP_080).unwrap();
    let mut errs_old = Vec::new();
    let mut errs_new = Vec::new();
    for (name, o) in old.as_object().unwrap() {
        if name.ends_with("_closes") {
            assert_eq!(o, &new[name], "{name}");
            continue;
        }
        let po = phone_envelope(name, o, &mut errs_old);
        let pn = phone_envelope(name, &new[name], &mut errs_new);
        assert_eq!(po, pn, "{name}: 폰이 읽은 값이 0.8.0 과 다르다\n 0.8.0: {po}\n 0.10.0: {pn}");
    }
    assert!(errs_old.is_empty(), "기준선(0.8.0)이 이미 폰의 형변환 규칙을 어김: {errs_old:?}");
    assert!(errs_new.is_empty(), "0.10.0 이 폰의 딱딱한 형변환(List·Map·String)을 깰 수 있다: {errs_new:?}");
}

#[test]
fn old_phone_replies_and_permissions_behave_like_080() {
    let c = upgraded_db();
    let out = run_script(&c);
    // 폰이 부르는 것 중 새 권한이 필요한 것은 없다 — 예약 허용(기본 끔)은 옛 폰이 아예 모른다
    assert_eq!(out["hello"]["r"]["can_schedule"], false);
    assert_eq!(out["hello"]["r"]["can_reply"], true);
    assert_eq!(out["reply_plain"]["r"]["state"], "delivering");
    assert_eq!(out["reply_quote"]["r"]["quote"]["part"], "response");
    assert_eq!(out["reply_again"]["r"]["rid"], "rid-fixture-0001", "같은 rid 재전송은 처음 결과");
    assert_eq!(out["reply_bad_rid"]["ok"], false);
    assert_eq!(out["reply_bad_rid"]["err"], "bad_request");
    assert_eq!(out["sessions_archived_denied"]["err"], "rejected");
    assert_eq!(out["unknown_method"]["err"], "unknown_method");
    assert_eq!(out["unpaired_device"]["err"], "unpaired");
    assert_eq!(out["unpaired_device_closes"], true);
    // 폰이 보낸 답이 예약과 섞이지 않는다 — 대기열의 폰 답은 예약 열이 비어 있다
    let sched_rows: i64 = c.query_row("SELECT COUNT(*) FROM conoti_reply WHERE sched IS NOT NULL", [], |r| r.get(0)).unwrap();
    assert_eq!(sched_rows, 0);
}

// ── 0.10.0 에서 새로 생긴 상태가 옛 폰 화면에 미치는 영향 ─────────────────────

/// 폰 화면의 내 말풍선 — `origin` 은 peer·channel 만 따로 쓰고 나머지(sched 포함)는 "나"
fn phone_who(origin: Option<&str>) -> &'static str {
    match origin {
        Some("peer") => "peer",
        Some("channel") => "channel",
        _ => "me",
    }
}

#[test]
fn scheduled_turn_origin_is_harmless_to_old_phone() {
    let c = upgraded_db();
    c.execute(
        "INSERT INTO turn (session_id, prompt_uuid, seq, origin, prompt_at, prompt_text, status) VALUES ('sess-alpha-0001', 'u-sched-1', 9, 'sched', '2026-08-10T04:00:00.000Z', '예약해 둔 말', 'done')",
        [],
    )
    .unwrap();
    let out = run_script(&c);
    let mut errs = Vec::new();
    let chat = phone_envelope("chat_alpha", &out["chat_alpha"], &mut errs);
    assert!(errs.is_empty(), "{errs:?}");
    let last = chat["turns"].as_array().unwrap().iter().find(|t| t["origin"] == "sched").expect("예약 요청이 대화에 보임");
    assert_eq!(last["prompt"], "예약해 둔 말");
    assert_eq!(phone_who(last["origin"].as_str()), "me", "옛 폰은 모르는 출처를 내 말로 그린다");
}

/// /clear 로 끝난 세션·이력 보관 세션(0.9.0) — 폰 목록엔 그대로 나오고 세션 행에 선택 필드 `ended` 만 붙는다.
/// 폰의 안 읽음·확인 필요·진행 수 배지는 목록 항목과 어긋나지 않아야 한다(옛 폰은 항목 수와 배지를 나란히 그린다).
#[test]
fn kept_sessions_stay_in_phone_list_and_counts_stay_consistent() {
    let c = upgraded_db();
    c.execute(
        "UPDATE session SET cleared_at = '2026-08-11T00:00:00.000Z', clear_state = 'keep', clear_asked = 1 WHERE id = 'sess-beta-00002'",
        [],
    )
    .unwrap();
    let out = run_script(&c);
    let mut errs = Vec::new();
    for f in ["all", "unread", "attention", "active"] {
        let name = format!("sessions_{f}");
        let list = phone_envelope(&name, &out[&name], &mut errs);
        let items = list["items"].as_array().unwrap();
        let sum = |k: &str| items.iter().map(|x| x[k].as_i64().unwrap()).sum::<i64>();
        if f == "all" {
            assert!(items.iter().any(|x| x["id"] == "sess-beta-00002"), "이력 보관 세션도 폰 목록에 있다");
            assert_eq!(list["unread"].as_i64().unwrap(), sum("unread"), "안 읽음 배지 = 항목 합");
            assert_eq!(list["attention"].as_i64().unwrap(), sum("attention"), "확인 필요 배지 = 항목 합");
            assert_eq!(list["active"].as_i64().unwrap(), sum("active"), "진행 중 배지 = 항목 합");
        }
    }
    assert!(errs.is_empty(), "{errs:?}");
    let all = &out["sessions_all"]["r"];
    let beta = all["items"].as_array().unwrap().iter().find(|x| x["id"] == "sess-beta-00002").unwrap();
    assert_eq!(beta["ended"]["state"], "keep");
    let alpha = all["items"].as_array().unwrap().iter().find(|x| x["id"] == "sess-alpha-0001").unwrap();
    assert!(alpha["ended"].is_null());
}

#[test]
fn sessions_call_stays_fast_with_many_sessions() {
    let c = upgraded_db();
    for n in 0..200 {
        let sid = format!("sess-bulk-{n:04}");
        c.execute("INSERT INTO session (id, project_dir, first_at, last_at, live_name) VALUES (?1, '/Users/x/bulk', '2026-08-01T00:00:00.000Z', '2026-08-01T00:00:00.000Z', ?2)", params![sid, format!("bulk-{n}")]).unwrap();
        for seq in 1..=3 {
            c.execute(
                "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, response_text, status) VALUES (?1, ?2, ?3, '2026-08-01T00:00:00.000Z', '요청', '결과', 'done')",
                params![sid, format!("u-{n}-{seq}"), seq],
            )
            .unwrap();
        }
    }
    let caller = rpc::Caller { pid: PID_MANAGE, can_reply: true, can_manage: true, can_schedule: false };
    let t0 = std::time::Instant::now();
    let r = rpc::sessions(&c, &json!({"filter": "all"}), &caller).unwrap();
    let ms = t0.elapsed().as_millis();
    assert_eq!(r["items"].as_array().unwrap().len(), 200);
    assert!(ms < 3000, "sessions 응답이 {ms}ms — 폰의 응답 대기(20초)와 프레임 한도 안이어야 한다");
    let bytes = serde_json::to_vec(&r).unwrap().len();
    assert!(bytes < 400 * 1024, "sessions 응답 {bytes}B — 60초 4MiB 전송 한도의 한 덩이로 너무 크다");
}

/// 결과 푸시(예약 아님)는 옛 서버·옛 폰과 같은 본문 `{"ticket"}` 이어야 한다 — `k` 는 예약 알림에만 붙는다
#[test]
fn result_push_carries_no_kind() {
    assert_eq!(crate::sched::pick_push_kind(&[]), None);
    assert_eq!(crate::sched::pick_push_kind(&[crate::sched::PUSH_READY]), Some("sched_ready"));
    assert_eq!(crate::sched::pick_push_kind(&[crate::sched::PUSH_READY, crate::sched::PUSH_HELD]), Some("sched_held"));
}
