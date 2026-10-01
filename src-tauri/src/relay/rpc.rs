//! 폰이 부를 수 있는 것 — 규격 docs/RELAY.md §4 표가 전부다. 파일 읽기·명령 실행 통로는 없다.
//! 화면(api.rs)과 같은 조회 함수를 쓰고, 폰에 필요한 모양으로만 줄여서 보낸다.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::{api, archive, attach, conoti, doc, text};

pub struct Caller<'a> {
    pub pid: &'a str,
    pub can_reply: bool,
    /// 세션 관리(보관함 보기·보관·고정·기록에서 지우기) — PC 가 기기별로 끌 수 있다
    pub can_manage: bool,
    /// 예약 전송 만들기·고치기·취소·처리(0.10.0) — PC 가 기기별로 켠다(기본 끔)
    pub can_schedule: bool,
}

const MANAGE_OFF: &str = "PC 에서 이 기기의 기록 관리를 꺼 둠";

/// 이 기기가 보관한 세션을 볼 수 있는가(기록 관리를 허용한 기기만 — 끄면 보관한 세션은 폰에 없는 것으로 한다)
fn visible(hidden: Option<i64>, caller: &Caller) -> bool {
    match hidden {
        Some(0) => true,
        Some(_) => caller.can_manage,
        None => false,
    }
}

pub type RpcResult = Result<Value, (&'static str, String)>;

fn bad(msg: &str) -> (&'static str, String) {
    ("bad_request", msg.to_string())
}

fn clip(v: Option<&str>, n: usize) -> Option<String> {
    v.map(|s| text::clip(s, n))
}

fn str_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn valid_sid(sid: &str) -> bool {
    (8..=64).contains(&sid.len()) && sid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn session_row(item: &Value) -> Value {
    json!({
        "id": item["id"], "name": item["name"], "named": item["named"],
        "project": item["project_name"],
        "dir": str_of(item, "project_dir").map(doc::tilde_path),
        "branch": item["git_branch"], "live": item["live_status"], "pinned": item["pinned"],
        "model": item["model"], "turns": item["turns"], "unread": item["unread"],
        "attention": item["attention"], "active": item["active"],
        "last_status": item["last_status"], "last_needs_input": item["last_needs_input"],
        "preview": item["last_preview"], "preview_ai": item["last_from_ai"], "last_at": item["last_at"],
        "archived": item["archived"], "agent": item["agent"],
        // 0.9.0(선택 필드): /clear 로 끝난 대화면 { cleared_at, state: purge|keep|ask, purge_at } — 폰이 흐리게 그릴 때 쓴다. 옛 폰은 무시한다
        "ended": item["ended"],
    })
}

/// `filter`: all | unread | attention | active | archived(보관함 — 기록 관리를 허용한 기기만).
/// 기록 관리가 허용된 기기에는 보관함 수와 정리 제안 수도 싣는다.
pub fn sessions(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    let filter = str_of(p, "filter").unwrap_or("all");
    if !matches!(filter, "all" | "unread" | "attention" | "active" | "archived") {
        return Err(bad("filter"));
    }
    if filter == "archived" && !caller.can_manage {
        return Err(("rejected", MANAGE_OFF.into()));
    }
    let mut items = api::list_sessions_on(conn, filter, "").map_err(|e| ("internal", e))?;
    // 0.10.0(선택 인자): `tag` = 태그 이름(대소문자 무시) — 그 태그가 붙은 요청이 하나라도 있는 세션만. 없는 태그면 빈 목록. 모르는 인자는 옛 앱이 무시한다
    if let Some(tag) = str_of(p, "tag").map(str::trim).filter(|t| !t.is_empty()) {
        let with: std::collections::HashSet<String> = conn
            .prepare(
                "SELECT DISTINCT t.session_id FROM turn_tag x JOIN turn t ON t.id = x.turn_id JOIN tag g ON g.id = x.tag_id
                  WHERE g.name = ?1 AND x.state IN ('auto','manual') AND t.hidden = 0",
            )
            .and_then(|mut st| st.query_map(params![tag], |r| r.get(0)).map(|r| r.flatten().collect()))
            .unwrap_or_default();
        items.retain(|i| serde_json::to_value(i).ok().and_then(|v| v["id"].as_str().map(str::to_string)).is_some_and(|id| with.contains(&id)));
    }
    let names = crate::tags::lookup(conn);
    let sched = crate::sched::counts_by_session(conn);
    let items: Vec<Value> = items
        .iter()
        .take(200)
        .map(|i| {
            let v = serde_json::to_value(i).unwrap_or_default();
            let mut row = session_row(&v);
            // 0.9.0(선택 필드): 세션의 대표 태그 [{n: 이름, c: 색}] — 요청 태그에서 파생. 옛 폰은 무시한다
            row["tags"] = tag_refs(&names, v["tags"].as_array().map(|a| a.iter().filter_map(Value::as_i64).collect::<Vec<_>>()).unwrap_or_default());
            // 0.10.0(선택 필드): 걸려 있는 예약 수 · 그중 받지 못해 처리를 기다리는 수
            let (n, held) = v["id"].as_str().and_then(|id| sched.get(id)).copied().unwrap_or((0, 0));
            row["sched_n"] = json!(n);
            row["sched_held"] = json!(held);
            // 0.10.0(선택 필드): 세션의 권한 모드(훅이 남긴 값, 모르면 null) · 예약할 때 사전 준비 안내가 필요한가
            let pi = v["id"].as_str().map(|id| crate::sched::perm_info(conn, id)).unwrap_or_default();
            row["perm"] = pi["perm"].clone();
            row["sched_warn"] = pi["warn"].clone();
            row
        })
        .collect();
    let counts = api::counts_on(conn);
    let mut out = json!({
        "items": items, "unread": counts.unread, "attention": counts.attention, "active": counts.active,
        "can_manage": caller.can_manage,
        "can_schedule": caller.can_schedule,
    });
    if caller.can_manage {
        let st = archive::stats(conn);
        out["archived"] = json!(st.archived);
        out["tidy_short"] = json!(st.tidy_short);
        out["tidy_idle"] = json!(st.tidy_idle);
    }
    Ok(out)
}

/// 태그 id → 폰에 보낼 [{n, c}] (없는 태그는 뺀다)
fn tag_refs(names: &std::collections::HashMap<i64, (String, String)>, ids: Vec<i64>) -> Value {
    Value::Array(ids.iter().filter_map(|id| names.get(id)).map(|(n, c)| json!({"n": n, "c": c})).collect())
}

fn bubble(t: &Value) -> Value {
    let status = str_of(t, "status").unwrap_or("done");
    let live = matches!(status, "running" | "background" | "waiting");
    let prompt = str_of(t, "prompt_text");
    let phone = doc::phone_reply(prompt);
    // 답장 줄은 떼서 따로(`quote`) — 폰이 말풍선 위에 인용으로 그린다
    let (quote, body) = match phone.as_ref().map(|p| p.1.as_str()).or(prompt) {
        Some(b) => {
            let (q, rest) = text::split_quote(b);
            (q, Some(rest))
        }
        None => (None, None),
    };
    let step = if live {
        t.get("last_step").and_then(Value::as_array).map(|s| {
            let kind = s.first().and_then(Value::as_str).unwrap_or("");
            let name = s.get(1).and_then(Value::as_str);
            let body = s.get(2).and_then(Value::as_str).unwrap_or("");
            match kind {
                "tool" => format!("{} — {}", doc::tool_name(name), text::first_line(body)),
                "task" => format!("백그라운드 알림 — {}", text::first_line(body)),
                _ => text::first_line(body).to_string(),
            }
        })
    } else {
        None
    };
    let unread = t["read_at"].is_null() && doc::is_finished(status);
    let response = str_of(t, "response_text").map(str::trim).filter(|s| !s.is_empty()).or_else(|| if live { str_of(t, "understanding") } else { None });
    json!({
        "id": t["id"], "seq": t["seq"], "origin": t["origin"], "peer": t["peer_name"],
        "via_phone": phone.is_some(),
        "mid_turn": str_of(t, "prompt_source") == Some("mid-turn"),
        "prompt_at": t["prompt_at"],
        "prompt": clip(body, 4000),
        "quote": quote.map(|q| json!({"seq": q.seq, "part": q.part, "text": q.text})),
        "slash": t["slash_command"],
        "status": status, "needs_input": t["needs_input"], "pending_bg": t["pending_bg"],
        "summary": clip(str_of(t, "summary"), 1000),
        "response": clip(response, 6000),
        "step": step.map(|s| text::clip(&s, 200)),
        "ended_at": t["ended_at"], "duration_ms": t["duration_ms"],
        "tool_calls": t["tool_calls"], "files": t["files_changed"], "agents": t["subagent_count"],
        "out_tokens": t["output_tokens"], "model": t["model"],
        "unread": unread, "starred": t["starred"],
        "atts": t.get("atts").cloned().unwrap_or_else(|| json!([])),
    })
}

fn head(conn: &Connection, s: &Value, caller: &Caller) -> Value {
    let sid = str_of(s, "id").unwrap_or("");
    // 보관한 세션은 읽기만(목록으로 되돌리면 답할 수 있다) — 여기까지 온 기기는 기록 관리를 허용받은 기기다
    let block = if s["archived"] == true { "archived" } else { conoti::reply_block(conn, caller.can_reply, sid) };
    json!({
        "id": sid, "name": s["name"], "named": s["named"], "project": s["project_name"],
        "dir": str_of(s, "project_dir").map(doc::tilde_path), "branch": s["git_branch"], "live": s["live_status"],
        "model": s["model"], "cost_usd": s["cost_usd"], "turns": s["turns"], "unread": s["unread"],
        "can_reply": block.is_empty(),
        "reply_block": block,
        "channel": s["channel_live"],
        "pinned": s["pinned"],
        "archived": s["archived"],
        "agent": s["agent"],
    })
}

/// `tags` · `any` → 요청 태그 필터(없으면 None = 전체)
fn tag_filter_of(conn: &Connection, p: &Value) -> Result<Option<crate::tags::TagFilter>, (&'static str, String)> {
    let Some(a) = p.get("tags").filter(|v| !v.is_null()) else { return Ok(None) };
    let a = a.as_array().filter(|a| a.len() <= 10).ok_or_else(|| bad("tags — 이름 배열(10개까지)"))?;
    if a.is_empty() {
        return Ok(None);
    }
    let any = p.get("any").and_then(Value::as_bool).unwrap_or(false);
    let mut ids: Vec<i64> = Vec::new();
    for n in a {
        let name = n.as_str().map(str::trim).filter(|s| !s.is_empty() && s.chars().count() <= 24).ok_or_else(|| bad("tags — 이름 배열(10개까지)"))?;
        let id: Option<i64> = conn.query_row("SELECT id FROM tag WHERE name = ?1", params![name], |r| r.get(0)).optional().map_err(|e| ("internal", e.to_string()))?;
        match id {
            Some(i) if !ids.contains(&i) => ids.push(i),
            Some(_) => {}
            None if !any => ids.push(-1), // 모두-조건: 없는 태그가 하나라도 있으면 통과하는 요청이 없다
            None => {}
        }
    }
    if ids.is_empty() {
        ids.push(-1); // 하나라도-조건인데 아는 이름이 하나도 없다 → 빈 결과(전체가 나오지 않게)
    }
    Ok(Some(crate::tags::TagFilter { tags: ids, untagged: false, all: !any }))
}

pub fn chat(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    let sid = str_of(p, "sid").filter(|s| valid_sid(s)).ok_or_else(|| bad("sid"))?;
    let before = p.get("before").and_then(Value::as_i64);
    let limit = p.get("limit").and_then(Value::as_i64).unwrap_or(30).clamp(1, 60);
    let hidden: Option<i64> = conn
        .query_row("SELECT hidden FROM session WHERE id = ?1", params![sid], |r| r.get(0))
        .optional()
        .map_err(|e| ("internal", e.to_string()))?;
    if !visible(hidden, caller) {
        // 보관한 세션은 기록 관리를 허용한 기기만 본다(읽기만 — reply_block = archived)
        return Err(("not_found", "세션이 없습니다".into()));
    }
    // 0.10.0(선택 인자): `tags` = 태그 이름 배열, `any` = true 면 하나라도 · 아니면(기본) 모두 가진 요청만. 이름은 대소문자 무시, 없는 이름은
    // 모두-조건에서는 아무 요청도 통과시키지 않고 하나라도-조건에서는 무시한다. 옛 앱이 이 인자를 몰라도 전체 대화가 온다.
    let filter = tag_filter_of(conn, p)?;
    let page = api::get_chat_filtered(conn, sid, before, Some(limit), filter.as_ref()).map_err(|e| ("internal", e))?;
    let v = serde_json::to_value(&page).unwrap_or_default();
    let names = crate::tags::lookup(conn);
    let turns: Vec<Value> = v["turns"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|t| {
                    let mut b = bubble(t);
                    // 0.9.0(선택 필드): 요청 태그 [{n, c}] — 모델 제안(받아들이기 전)은 싣지 않는다
                    let ids = t["tags"].as_array().map(|x| x.iter().filter(|g| g["state"] != "ai").filter_map(|g| g["id"].as_i64()).collect()).unwrap_or_default();
                    b["tags"] = tag_refs(&names, ids);
                    b
                })
                .collect()
        })
        .unwrap_or_default();
    let mut session = head(conn, &v["session"], caller);
    // 0.10.0(선택 필드): 예약 수·그중 처리를 기다리는 수 — `sessions` 항목과 같은 값, 옛 폰은 무시한다
    let (sn, sh) = crate::sched::counts_by_session(conn).get(sid).copied().unwrap_or((0, 0));
    session["sched_n"] = json!(sn);
    session["sched_held"] = json!(sh);
    let st = crate::tags::session_tags(conn, sid);
    // 세션 안 태그 요약 [{n, c, k: 요청 수}]
    session["tags"] = Value::Array(st.tags.iter().take(12).filter_map(|(id, k)| names.get(id).map(|(n, c)| json!({"n": n, "c": c, "k": k}))).collect());
    Ok(json!({
        "session": session,
        "turns": turns,
        "has_more": v["has_more"],
        "replies": conoti::replies_for(conn, sid, 20),
    }))
}

pub fn turn(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    let id = p.get("id").and_then(Value::as_i64).ok_or_else(|| bad("id"))?;
    let row: Option<(i64, i64)> = conn
        .query_row("SELECT t.hidden, s.hidden FROM turn t JOIN session s ON s.id = t.session_id WHERE t.id = ?1", params![id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(|e| ("internal", e.to_string()))?;
    if !row.is_some_and(|(turn_hidden, session_hidden)| turn_hidden == 0 && visible(Some(session_hidden), caller)) {
        return Err(("not_found", "요청이 없습니다".into()));
    }
    let d = api::get_turn_on(conn, id).map_err(|e| ("internal", e))?;
    let v = serde_json::to_value(&d).unwrap_or_default();
    let t = &v["turn"];
    let mut md = doc::build(&v);
    const MAX_DOC: usize = 600_000;
    if md.len() > MAX_DOC {
        let mut cut = MAX_DOC;
        while !md.is_char_boundary(cut) {
            cut -= 1;
        }
        md.truncate(cut);
        md.push_str("\n\n…(문서가 길어 폰에는 앞부분만 보냈습니다. 전체는 PC 에서 보세요.)");
    }
    Ok(json!({
        "id": id, "sid": v["session"]["id"], "title": doc::title_of(&v), "markdown": md,
        "status": t["status"], "needs_input": t["needs_input"],
        "prev_id": v["prev_id"], "next_id": v["next_id"],
    }))
}

/// 읽음 표시. 바뀐 줄 수. 이 기기가 볼 수 없는 세션(기록 관리를 허용받지 않은 기기의 보관 세션)은 건드리지 않는다.
pub fn read(conn: &Connection, p: &Value, caller: &Caller) -> Result<usize, (&'static str, String)> {
    if let Some(ids) = p.get("ids").and_then(Value::as_array) {
        let ids: Vec<i64> = ids
            .iter()
            .filter_map(Value::as_i64)
            .take(500)
            .filter(|id| {
                let hidden: Option<i64> = conn
                    .query_row("SELECT s.hidden FROM turn t JOIN session s ON s.id = t.session_id WHERE t.id = ?1", params![id], |r| r.get(0))
                    .optional()
                    .ok()
                    .flatten();
                visible(hidden, caller)
            })
            .collect();
        return api::mark_read_on(conn, &ids).map_err(|e| ("internal", e));
    }
    if let Some(sid) = str_of(p, "sid").filter(|s| valid_sid(s)) {
        let hidden: Option<i64> = conn
            .query_row("SELECT hidden FROM session WHERE id = ?1", params![sid], |r| r.get(0))
            .optional()
            .map_err(|e| ("internal", e.to_string()))?;
        if !visible(hidden, caller) {
            return Err(("not_found", "세션이 없습니다".into()));
        }
        return api::mark_session_read_on(conn, sid).map_err(|e| ("internal", e));
    }
    Err(bad("ids 또는 sid"))
}

/// `atts` — id 문자열 배열(없으면 빈 배열)
fn ids_of(v: Option<&Value>) -> Result<Vec<String>, (&'static str, String)> {
    let Some(v) = v.filter(|v| !v.is_null()) else { return Ok(vec![]) };
    let a = v.as_array().filter(|a| a.len() <= attach::MAX_PER_MESSAGE).ok_or_else(|| bad("atts"))?;
    a.iter().map(|x| x.as_str().filter(|s| attach::valid_id(s)).map(str::to_string).ok_or_else(|| bad("atts"))).collect()
}

pub fn reply(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    let sid = str_of(p, "sid").filter(|s| valid_sid(s)).ok_or_else(|| bad("sid"))?;
    let atts = ids_of(p.get("atts"))?;
    // 이미지만 보낼 때는 글이 없어도 된다
    let body = match str_of(p, "text") {
        Some(t) => t,
        None if !atts.is_empty() => "",
        None => return Err(bad("text")),
    };
    let rid = str_of(p, "rid").filter(|r| conoti::valid_rid(r)).ok_or_else(|| bad("rid"))?;
    let turn_id = p.get("turn_id").and_then(Value::as_i64);
    // 메신저식 답장(§4) — turn_id 의 요청(prompt)·결과(response)에 단 말
    let quote = str_of(p, "quote");
    if quote.is_some_and(|q| !conoti::valid_quote(q)) || (quote.is_some() && turn_id.is_none()) {
        return Err(bad("quote — 'prompt'·'response' 이고 turn_id 가 있어야 합니다"));
    }
    if !atts.is_empty() && attach::check_phone_ids(conn, caller.pid, rid, &atts).is_err() {
        return Err(bad("atts — 이 기기가 같은 답 번호로 올린 이미지만 붙일 수 있습니다"));
    }
    conoti::accept_reply(conn, caller.pid, caller.can_reply, sid, turn_id, body, rid, &atts, quote).map_err(|e| ("rejected", e))
}

/// 이미지 한 장 올리기(docs/RELAY.md §4-1) — 답보다 먼저, 한 장씩
pub fn att(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    if !caller.can_reply {
        return Err(("rejected", "PC 에서 이 기기의 답 보내기를 꺼 둠".into()));
    }
    // 답을 멈춘 동안에는 그 답에 붙일 사진도 받지 않는다(디스크에 쌓이지 않게)
    if conoti::flag(conn, "conoti.paused") {
        return Err(("rejected", "PC 에서 폰 답 받기를 멈춤".into()));
    }
    let rid = str_of(p, "rid").filter(|r| conoti::valid_rid(r)).ok_or_else(|| bad("rid"))?;
    let i = p.get("i").and_then(Value::as_i64).ok_or_else(|| bad("i"))?;
    let data = str_of(p, "data").ok_or_else(|| bad("data"))?;
    let m = attach::phone_upload(conn, caller.pid, rid, i, data)?;
    Ok(json!({"id": m.id, "w": m.width, "h": m.height, "bytes": m.bytes}))
}

/// 긴 변이 `side` 를 넘으면 줄인 크기
fn fit(w: i64, h: i64, side: i64) -> (i64, i64) {
    let long = w.max(h);
    if long <= side || long == 0 {
        return (w, h);
    }
    ((w * side + long / 2) / long, (h * side + long / 2) / long)
}

/// 이미지 받기 — 숨기지 않은 세션의 메시지에 붙은 것만
pub fn att_get(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    let id = str_of(p, "id").filter(|s| attach::valid_id(s)).ok_or_else(|| bad("id"))?;
    let size = str_of(p, "size").unwrap_or("thumb");
    if !matches!(size, "thumb" | "view") {
        return Err(bad("size"));
    }
    if !attach::phone_can_see(conn, id, caller.can_manage) {
        return Err(("not_found", "이미지가 없습니다".into()));
    }
    let m = attach::meta(conn, id).ok_or_else(|| ("not_found", "이미지가 없습니다".to_string()))?;
    let (bytes, mime) = attach::read(conn, id, size).map_err(|e| ("not_found", e))?;
    let side = if size == "thumb" { attach::THUMB_SIDE } else { attach::VIEW_SIDE };
    let (w, h) = fit(m.width, m.height, i64::from(side));
    use base64::Engine;
    Ok(json!({"id": id, "mime": mime, "w": w, "h": h, "data": base64::engine::general_purpose::STANDARD.encode(bytes)}))
}

/// 관리(바꾸기)를 받아도 되나 — 기기별 허용 + PC 의 "폰 답 받기 멈춤"(폰이 PC 에 손대는 것을 한 번에 멈추는 스위치)
fn check_manage(conn: &Connection, caller: &Caller) -> Result<(), (&'static str, String)> {
    if !caller.can_manage {
        return Err(("rejected", MANAGE_OFF.into()));
    }
    if conoti::flag(conn, "conoti.paused") {
        return Err(("rejected", "PC 에서 폰 답 받기를 멈춤 — 관리도 멈춘다".into()));
    }
    Ok(())
}

/// `sids` — 세션 id 배열(1~200개, 중복 제거)
fn sids_of(v: Option<&Value>) -> Result<Vec<String>, (&'static str, String)> {
    let a = v.and_then(Value::as_array).filter(|a| !a.is_empty() && a.len() <= 200).ok_or_else(|| bad("sids"))?;
    let mut out: Vec<String> = a
        .iter()
        .map(|x| x.as_str().filter(|s| valid_sid(s)).map(str::to_string).ok_or_else(|| bad("sids")))
        .collect::<Result<_, _>>()?;
    out.sort();
    out.dedup();
    Ok(out)
}

/// 세션 관리(docs/RELAY.md §4-2) — 보관·되돌리기·고정·기록에서 지우기. 데이터는 PC 에만 있고 폰은 요청만 한다.
/// 지우기는 PC 화면의 "기록에서 지우기"와 같다: 이 앱의 사본만, 진행 중·전달 대기 세션은 남기고, 폰 답 막기 설정은 둔다.
pub fn manage(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    check_manage(conn, caller)?;
    let op = str_of(p, "op").ok_or_else(|| bad("op"))?;
    let sids = sids_of(p.get("sids"))?;
    let internal = |e: String| ("internal", e);
    match op {
        "archive" | "unarchive" => Ok(json!({"n": archive::set_archived(conn, &sids, op == "archive").map_err(internal)?})),
        "pin" | "unpin" => {
            let mut n = 0;
            for sid in &sids {
                n += conn
                    .execute("UPDATE session SET pinned = ?2 WHERE id = ?1 AND pinned <> ?2", params![sid, (op == "pin") as i64])
                    .map_err(|e| ("internal", e.to_string()))?;
            }
            Ok(json!({"n": n}))
        }
        "delete" => {
            let d = archive::delete_sessions(conn, &sids).map_err(internal)?;
            Ok(json!({"n": d.deleted.len(), "deleted": d.deleted, "skipped": d.skipped}))
        }
        _ => Err(bad("op")),
    }
}

/// 정리 제안대로 한 번에 보관 — `kind`: short(요청 1개 이하) | idle(30일 넘게 조용함). 고정·실행 중·안 읽은 결과가 있는 세션은 빠진다.
pub fn tidy(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    check_manage(conn, caller)?;
    let ids = match str_of(p, "kind") {
        Some("short") => archive::tidy_candidates(conn, true, None),
        Some("idle") => archive::tidy_candidates(conn, false, Some(archive::TIDY_IDLE_DAYS)),
        _ => return Err(bad("kind")),
    }
    .map_err(|e| ("internal", e))?;
    let n = archive::set_archived(conn, &ids, true).map_err(|e| ("internal", e))?;
    Ok(json!({"n": n}))
}

// ── 예약 전송(0.10.0, docs/RELAY.md §4-3) ───────────────────────────────────

const SCHED_OFF: &str = "PC 에서 이 기기의 예약 허용을 꺼 둠";

/// 예약을 만들고 고치고 취소하고 처리해도 되나 — 기기별 예약 허용 + 답 보내기 허용 + PC 의 "폰 답 받기 멈춤"
fn check_sched(conn: &Connection, caller: &Caller) -> Result<(), (&'static str, String)> {
    if !caller.can_schedule || !caller.can_reply {
        return Err(("rejected", SCHED_OFF.into()));
    }
    if conoti::flag(conn, "conoti.paused") {
        return Err(("rejected", "PC 에서 폰 답 받기를 멈춤 — 예약도 멈춘다".into()));
    }
    Ok(())
}

fn sched_err(e: String) -> (&'static str, String) {
    match e.strip_prefix(crate::sched::CONFLICT) {
        Some(rest) => ("conflict", rest.to_string()),
        None => ("rejected", e),
    }
}

/// `when`: `{after_min}` | `{at: "2026-10-01T00:00:00Z"[, tz]}` (절대 시각 — 폰이 자기 시간대로 풀어 보낸다) | `{local: "2026-10-01T09:00", tz}` (PC 가 그 시간대로 푼다)
fn when_of(v: Option<&Value>) -> Result<crate::sched::WhenIn, (&'static str, String)> {
    use crate::sched::WhenIn;
    let v = v.filter(|v| v.is_object()).ok_or_else(|| bad("when"))?;
    if let Some(m) = v.get("after_min").and_then(Value::as_i64) {
        return Ok(WhenIn::After { min: m });
    }
    if let Some(a) = str_of(v, "at") {
        let ms = chrono::DateTime::parse_from_rfc3339(a).map_err(|_| bad("when.at"))?.timestamp_millis();
        return Ok(WhenIn::AtUtc { ms, tz: str_of(v, "tz").unwrap_or("UTC").to_string() });
    }
    match (str_of(v, "local"), str_of(v, "tz")) {
        (Some(l), Some(z)) => Ok(WhenIn::At { local: l.to_string(), tz: z.to_string() }),
        _ => Err(bad("when — after_min · at · local+tz 중 하나")),
    }
}

/// 화면용 예약 항목 → 폰 규격 `Sched`
fn sched_out(v: &Value, pid: &str) -> Value {
    let atts: Vec<Value> = v["atts"].as_array().map(|a| a.iter().map(|m| m["id"].clone()).collect()).unwrap_or_default();
    let run = if v["run"].is_null() {
        Value::Null
    } else {
        json!({"at": v["run"]["occurrence_at"], "state": v["run"]["state"], "reason": v["run"]["reason"], "note": v["run"]["note"]})
    };
    json!({
        "id": v["id"], "sid": v["session_id"], "name": v["session_name"], "text": v["text"],
        "quote": v["quote"], "turn_id": v["turn_id"], "atts": atts,
        "at": v["next_due_at"], "kind": v["kind"], "tz": v["tz"],
        "on_missed": v["on_missed"], "within_min": v["missed_within_min"], "busy": v["busy_policy"],
        "state": v["state"], "rev": v["rev"], "created_at": v["created_at"],
        "mine": str_of(v, "created_by") == Some(pid), "run": run,
        "perm": v["perm"], "sched_warn": v["sched_warn"],
    })
}

/// 예약을 고르고 만든 쪽(`created_by` — `desktop` 또는 기기 pid)을 돌려준다. 없거나 이 기기가 볼 수 없는 세션(보관 · 기록 관리 꺼짐)의 예약이면 `not_found`(목록과 같은 규칙)
fn sched_owner(conn: &Connection, id: &str, caller: &Caller) -> Result<String, (&'static str, String)> {
    let row: Option<(String, Option<i64>)> = conn
        .query_row("SELECT sc.created_by, s.hidden FROM schedule sc LEFT JOIN session s ON s.id = sc.session_id WHERE sc.id = ?1", params![id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(|e| ("internal", e.to_string()))?;
    match row {
        Some((by, hidden)) if visible(hidden, caller) => Ok(by),
        _ => Err(("not_found", "예약이 없습니다".into())),
    }
}

const NOT_MINE: &str = "이 기기가 만든 예약만 고치거나 취소할 수 있습니다 — PC 나 다른 기기가 만든 예약은 그곳에서 고치세요";

/// 고치기·취소는 **그 기기가 만든 예약(`mine`)만** — PC·다른 기기가 만든 예약을 폰이 고치면 발사 때의 기기 재검사(`device_block`)와
/// 기기 해제 때의 회수(`cancel_device`)가 만든 쪽 기준이라 모두 비켜 간다(10-01 보안 점검). 거절은 `rejected`.
fn check_mine(by: &str, caller: &Caller) -> Result<(), (&'static str, String)> {
    if by != caller.pid {
        return Err(("rejected", NOT_MINE.into()));
    }
    Ok(())
}

fn sched_id(p: &Value) -> Result<&str, (&'static str, String)> {
    str_of(p, "id").filter(|s| s.len() <= 32 && s.starts_with("sc") && s.chars().all(|c| c.is_ascii_alphanumeric())).ok_or_else(|| bad("id"))
}

pub fn sched_add(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    check_sched(conn, caller)?;
    let sid = str_of(p, "sid").filter(|s| valid_sid(s)).ok_or_else(|| bad("sid"))?;
    let rid = str_of(p, "rid").filter(|r| conoti::valid_rid(r)).ok_or_else(|| bad("rid"))?;
    let atts = ids_of(p.get("atts"))?;
    let text = match str_of(p, "text") {
        Some(t) => t,
        None if !atts.is_empty() => "",
        None => return Err(bad("text")),
    };
    let hidden: Option<i64> = conn.query_row("SELECT hidden FROM session WHERE id = ?1", params![sid], |r| r.get(0)).optional().map_err(|e| ("internal", e.to_string()))?;
    if !visible(hidden, caller) {
        return Err(("not_found", "세션이 없습니다".into()));
    }
    // 폰 답과 같은 겹: 세션 차단·전체 멈춤·기기 허용은 이 자리에서도 본다(발사 때 한 번 더)
    if matches!(conoti::reply_block(conn, caller.can_reply, sid), "device_off" | "paused" | "session_blocked" | "no_session") {
        return Err(("rejected", "이 세션은 폰에서 예약을 받을 수 없습니다(PC 에서 막아 둠)".into()));
    }
    let quote_part = str_of(p, "quote").map(str::to_string);
    let turn_id = p.get("turn_id").and_then(Value::as_i64);
    if quote_part.as_deref().is_some_and(|q| !conoti::valid_quote(q)) || (quote_part.is_some() && turn_id.is_none()) {
        return Err(bad("quote — 'prompt'·'response' 이고 turn_id 가 있어야 합니다"));
    }
    // 이미 만든 rid 의 재전송이면 처음 결과를 돌려준다(이미지 검사보다 먼저 — 올린 기록이 지나도 같은 답)
    if !atts.is_empty() && attach::check_phone_ids(conn, caller.pid, rid, &atts).is_err() {
        let dup: i64 = conn.query_row("SELECT COUNT(*) FROM schedule WHERE created_by = ?1 AND rid = ?2", params![caller.pid, rid], |r| r.get(0)).unwrap_or(0);
        if dup == 0 {
            return Err(bad("atts — 이 기기가 같은 rid 로 올린 이미지만 붙일 수 있습니다"));
        }
    }
    let new = crate::sched::NewSchedule {
        session_id: sid.to_string(),
        text: text.to_string(),
        atts,
        quote_turn: turn_id.filter(|_| quote_part.is_some()),
        quote_part,
        when: when_of(p.get("when"))?,
        on_missed: str_of(p, "on_missed").unwrap_or("within").to_string(),
        missed_within_min: p.get("within_min").and_then(Value::as_i64),
        busy_policy: str_of(p, "busy").map(str::to_string),
        rid: Some(rid.to_string()),
    };
    let v = crate::sched::add(conn, chrono::Utc::now(), caller.pid, &new, &crate::sched::LiveProbe).map_err(sched_err)?;
    let mut out = sched_out(&v, caller.pid);
    out["warnings"] = v["warnings"].clone();
    Ok(out)
}

/// "이 세션은 예약 안내 다시 안 보기" — `{sid, off?: bool(기본 true)}`. 예약 권한과 같은 검사
pub fn sched_warn_off(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    check_sched(conn, caller)?;
    let sid = str_of(p, "sid").filter(|s| valid_sid(s)).ok_or_else(|| bad("sid"))?;
    let off = p.get("off").and_then(Value::as_bool).unwrap_or(true);
    crate::sched::set_warn_off(conn, sid, off).map_err(sched_err)?;
    Ok(json!({}))
}

pub fn sched_list(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    let sid = match p.get("sid").filter(|v| !v.is_null()) {
        Some(v) => Some(v.as_str().filter(|s| valid_sid(s)).ok_or_else(|| bad("sid"))?),
        None => None,
    };
    let items: Vec<Value> = crate::sched::list(conn, sid)
        .iter()
        .filter(|v| {
            let hidden: Option<i64> = v["session_id"].as_str().and_then(|s| conn.query_row("SELECT hidden FROM session WHERE id = ?1", params![s], |r| r.get(0)).optional().ok().flatten());
            visible(hidden, caller)
        })
        .take(100)
        .map(|v| {
            let mut out = sched_out(v, caller.pid);
            // 예약 권한(예약 허용 + 답 보내기)이 없는 기기는 목록만 본다 — PC·다른 기기가 걸어 둔 말(아직 세션에 들어가지 않은 프롬프트)과
            // 이미지는 비운다. 필드·타입은 그대로(옛 폰 파서 호환), 값만 줄인다. 자기가 만든 예약은 그대로 보인다.
            if !(caller.can_schedule && caller.can_reply) && out["mine"] != true {
                out["text"] = json!("");
                out["atts"] = json!([]);
            }
            out
        })
        .collect();
    let (active, held) = crate::sched::counts(conn);
    Ok(json!({"items": items, "active": active, "held": held, "can_schedule": caller.can_schedule}))
}

pub fn sched_edit(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    check_sched(conn, caller)?;
    let id = sched_id(p)?;
    check_mine(&sched_owner(conn, id, caller)?, caller)?;
    let rev = p.get("rev").and_then(Value::as_i64).ok_or_else(|| bad("rev"))?;
    let atts = ids_of(p.get("atts"))?;
    // 새 이미지는 이 기기가 rid 로 올린 것만, 이미 붙어 있던 이미지는 그대로 남길 수 있다
    let current: Vec<String> = conn
        .prepare("SELECT att_id FROM schedule_att WHERE schedule_id = ?1")
        .and_then(|mut st| st.query_map(params![id], |r| r.get(0)).map(|r| r.flatten().collect()))
        .unwrap_or_default();
    let fresh: Vec<String> = atts.iter().filter(|a| !current.contains(a)).cloned().collect();
    if !fresh.is_empty() {
        let rid = str_of(p, "rid").filter(|r| conoti::valid_rid(r)).ok_or_else(|| bad("rid — 새 이미지를 붙일 때 필요"))?;
        if attach::check_phone_ids(conn, caller.pid, rid, &fresh).is_err() {
            return Err(bad("atts — 새 이미지는 이 기기가 같은 rid 로 올린 것만 붙일 수 있습니다"));
        }
    }
    let text = match str_of(p, "text") {
        Some(t) => t,
        None if !atts.is_empty() => "",
        None => return Err(bad("text")),
    };
    let edit = crate::sched::EditSchedule {
        rev,
        text: text.to_string(),
        atts,
        when: when_of(p.get("when"))?,
        on_missed: str_of(p, "on_missed").unwrap_or("within").to_string(),
        missed_within_min: p.get("within_min").and_then(Value::as_i64),
        busy_policy: str_of(p, "busy").map(str::to_string),
    };
    let v = crate::sched::update(conn, chrono::Utc::now(), id, &edit).map_err(sched_err)?;
    Ok(sched_out(&v, caller.pid))
}

pub fn sched_cancel(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    check_sched(conn, caller)?;
    let id = sched_id(p)?;
    check_mine(&sched_owner(conn, id, caller)?, caller)?;
    crate::sched::cancel(conn, chrono::Utc::now(), id).map_err(sched_err)?;
    Ok(json!({}))
}

/// 받지 못해 처리를 기다리는 예약을 `send`(지금 보낸다 — 폰 답과 같은 검사) · `drop`(버린다).
/// 누가 만든 예약이든 된다(규격 §4-3 — 못 받은 예약은 알림을 받은 폰이 처리하도록 설계됐다): 보내기는 이 기기의 폰 답과 같은 겹
/// (답 보내기 허용·전체 멈춤·세션 차단·이어서 실행 설정)과 데스크톱 확인을 거치므로 폰이 직접 답을 보내는 것 이상의 힘이 없고, 버리기는 실행을 만들지 않는다.
pub fn sched_act(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    check_sched(conn, caller)?;
    let id = sched_id(p)?;
    sched_owner(conn, id, caller)?;
    let op = str_of(p, "op").filter(|o| matches!(*o, "send" | "drop")).ok_or_else(|| bad("op"))?;
    crate::sched::act_by(conn, chrono::Utc::now(), id, op, Some((caller.pid, caller.can_reply))).map_err(sched_err)?;
    Ok(json!({}))
}

// ── 요청 태그(0.10.0, §4-4) ──────────────────────────────────────────────────

/// 전체 태그 `{items:[{n, c, minor, k}]}` — k = 그 태그가 붙은 요청 수. 페어링된 기기면 누구나.
/// 기록 관리를 허용하지 않은 기기는 보관한 세션을 볼 수 없으므로, 보관한 세션의 요청에만 붙은 태그(폴더 이름으로 생긴 프로젝트 태그 등)는 빼고 `k` 도 보이는 세션만 센다.
pub fn tag_list(conn: &Connection, _p: &Value, caller: &Caller) -> RpcResult {
    let ov = crate::tags::overview(conn);
    let seen: Option<std::collections::HashMap<i64, i64>> = (!caller.can_manage).then(|| {
        conn.prepare(
            "SELECT x.tag_id, COUNT(*) FROM turn_tag x JOIN turn t ON t.id = x.turn_id JOIN session s ON s.id = t.session_id
              WHERE x.state IN ('auto','manual') AND t.hidden = 0 AND s.hidden = 0 GROUP BY x.tag_id",
        )
        .and_then(|mut st| st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map(|r| r.flatten().collect()))
        .unwrap_or_default()
    });
    let items: Vec<Value> = ov
        .tags
        .iter()
        .filter_map(|t| {
            let k = match &seen {
                None => t.turns,
                Some(m) => match m.get(&t.id).copied().unwrap_or(0) {
                    // 요청이 있는데 보이는 세션에는 하나도 없다 = 보관 세션에서만 쓰인 태그
                    0 if t.turns > 0 => return None,
                    k => k,
                },
            };
            Some(json!({"n": t.name, "c": t.color, "minor": t.minor, "k": k}))
        })
        .collect();
    Ok(json!({"items": items}))
}

const MAX_TAGS_TOTAL: i64 = 300;

/// 요청 하나의 태그를 바꾼다. `add` = 직접 붙임(manual, 없는 이름은 새로 만든다·대소문자 무시로 재사용) · `remove` = 뗌(off). 기록 관리 허용이 필요하다.
pub fn tag_set(conn: &Connection, p: &Value, caller: &Caller) -> RpcResult {
    check_manage(conn, caller)?;
    let turn_id = p.get("turn_id").and_then(Value::as_i64).ok_or_else(|| bad("turn_id"))?;
    let names = |k: &str| -> Result<Vec<String>, (&'static str, String)> {
        let Some(v) = p.get(k).filter(|v| !v.is_null()) else { return Ok(vec![]) };
        let a = v.as_array().filter(|a| a.len() <= 10).ok_or_else(|| bad(k))?;
        a.iter().map(|x| x.as_str().map(str::trim).filter(|s| !s.is_empty() && s.chars().count() <= 24 && !s.chars().any(char::is_control)).map(str::to_string).ok_or_else(|| bad(k))).collect()
    };
    let (add, remove) = (names("add")?, names("remove")?);
    if add.is_empty() && remove.is_empty() {
        return Err(bad("add 또는 remove"));
    }
    let row: Option<(i64, i64)> = conn
        .query_row("SELECT t.hidden, s.hidden FROM turn t JOIN session s ON s.id = t.session_id WHERE t.id = ?1", params![turn_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .map_err(|e| ("internal", e.to_string()))?;
    if !row.is_some_and(|(th, sh)| th == 0 && visible(Some(sh), caller)) {
        return Err(("not_found", "요청이 없습니다".into()));
    }
    let find = |n: &str| -> Option<i64> { conn.query_row("SELECT id FROM tag WHERE name = ?1", params![n], |r| r.get(0)).optional().ok().flatten() };
    for n in &add {
        let tid = match find(n) {
            Some(i) => i,
            None => {
                let total: i64 = conn.query_row("SELECT COUNT(*) FROM tag", [], |r| r.get(0)).unwrap_or(0);
                if total >= MAX_TAGS_TOTAL {
                    return Err(("rejected", format!("태그가 너무 많습니다({MAX_TAGS_TOTAL}개까지)")));
                }
                crate::tags::create_tag(conn, n, "", false).map_err(|e| ("rejected", e))?
            }
        };
        crate::tags::set_turn_tag(conn, turn_id, tid, true).map_err(|e| ("rejected", e))?;
    }
    for n in &remove {
        if let Some(tid) = find(n) {
            crate::tags::set_turn_tag(conn, turn_id, tid, false).map_err(|e| ("rejected", e))?;
        }
    }
    let lookup = crate::tags::lookup(conn);
    let ids: Vec<i64> = crate::tags::tags_of_turns(conn, &[turn_id]).remove(&turn_id).unwrap_or_default().iter().filter(|g| g.state != "ai" && g.state != "off").map(|g| g.id).collect();
    Ok(json!({"tags": tag_refs(&lookup, ids)}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        db::migrate(&c).unwrap();
        c.execute(
            "INSERT INTO session (id, project_dir, first_at, last_at, live_name) VALUES ('sess-0001', '/Users/x/proj', '2026-09-24T00:00:00Z', '2026-09-24T00:00:00Z', 'api-refactor')",
            [],
        )
        .unwrap();
        for (i, st) in [(1, "done"), (2, "running")] {
            c.execute(
                "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, response_text, status, summary)
                 VALUES ('sess-0001', ?1, ?2, '2026-09-24T00:00:00Z', ?3, '끝났어요', ?4, '요약')",
                params![format!("u{i}"), i, format!("요청 {i}"), st],
            )
            .unwrap();
        }
        c
    }

    #[test]
    fn sessions_and_chat_shapes() {
        let c = mem();
        let caller = Caller { pid: "d1", can_reply: true, can_manage: true, can_schedule: false };
        let s = sessions(&c, &json!({}), &caller).unwrap();
        assert_eq!(s["items"][0]["name"], "api-refactor");
        assert_eq!(s["items"][0]["dir"], "~/proj");
        assert_eq!(s["unread"], 1);
        assert!(sessions(&c, &json!({"filter": "zzz"}), &caller).is_err());

        let ch = chat(&c, &json!({"sid": "sess-0001"}), &caller).unwrap();
        assert_eq!(ch["turns"].as_array().unwrap().len(), 2);
        assert_eq!(ch["turns"][0]["unread"], true);
        assert_eq!(ch["turns"][1]["unread"], false); // 진행 중은 안 읽음이 아니다
        assert_eq!(ch["session"]["reply_block"], "offline_no_resume");
        assert!(chat(&c, &json!({"sid": "../../etc"}), &caller).is_err());
        assert_eq!(chat(&c, &json!({"sid": "nope-nope"}), &caller).unwrap_err().0, "not_found");
        assert_eq!(s["items"][0]["agent"], "claude");
        assert_eq!(ch["session"]["agent"], "claude");
        // Codex 세션은 agent 로 알린다(폰이 표시한다)
        c.execute("UPDATE session SET agent = 'codex' WHERE id = 'sess-0001'", []).unwrap();
        let s = sessions(&c, &json!({}), &caller).unwrap();
        assert_eq!(s["items"][0]["agent"], "codex");
        assert_eq!(chat(&c, &json!({"sid": "sess-0001"}), &caller).unwrap()["session"]["agent"], "codex");
    }

    #[test]
    fn tags_ride_along_as_optional_fields_without_content() {
        let c = mem();
        let caller = Caller { pid: "d1", can_reply: true, can_manage: true, can_schedule: false };
        let tag = crate::tags::create_tag(&c, "알파", "#112233", false).unwrap();
        let other = crate::tags::create_tag(&c, "제안뿐", "#445566", false).unwrap();
        let first: i64 = c.query_row("SELECT id FROM turn WHERE session_id = 'sess-0001' ORDER BY seq LIMIT 1", [], |r| r.get(0)).unwrap();
        crate::tags::set_turn_tag(&c, first, tag, true).unwrap();
        c.execute("INSERT INTO turn_tag (turn_id, tag_id, state, at) VALUES (?1, ?2, 'ai', 't')", rusqlite::params![first, other]).unwrap();
        let s = sessions(&c, &json!({}), &caller).unwrap();
        assert_eq!(s["items"][0]["tags"], json!([{"n": "알파", "c": "#112233"}]));
        let ch = chat(&c, &json!({"sid": "sess-0001"}), &caller).unwrap();
        assert_eq!(ch["turns"][0]["tags"], json!([{"n": "알파", "c": "#112233"}]), "모델 제안은 받아들이기 전엔 폰에 보이지 않는다");
        assert_eq!(ch["turns"][1]["tags"], json!([]));
        assert_eq!(ch["session"]["tags"], json!([{"n": "알파", "c": "#112233", "k": 1}]));
    }

    #[test]
    fn reply_quote_is_checked_and_shown_as_its_own_field() {
        let c = mem();
        db::set_meta(&c, "conoti.bg_resume", "1").unwrap();
        c.execute("INSERT INTO relay_device (pid, name, phone_pub, can_reply, created_at) VALUES ('d1', '폰', 'ab', 1, 'x')", []).unwrap();
        let caller = Caller { pid: "d1", can_reply: true, can_manage: true, can_schedule: false };
        let first: i64 = c.query_row("SELECT id FROM turn WHERE seq = 1", [], |r| r.get(0)).unwrap();
        // 답장은 prompt·response 만, 대상 요청이 있어야 한다
        let bad_part = json!({"sid": "sess-0001", "turn_id": first, "text": "x", "rid": "rid-bbbb0001", "quote": "all"});
        assert_eq!(reply(&c, &bad_part, &caller).unwrap_err().0, "bad_request");
        let no_turn = json!({"sid": "sess-0001", "text": "x", "rid": "rid-bbbb0002", "quote": "response"});
        assert_eq!(reply(&c, &no_turn, &caller).unwrap_err().0, "bad_request");
        let ok = json!({"sid": "sess-0001", "turn_id": first, "text": "그거 다시", "rid": "rid-bbbb0003", "quote": "response"});
        let r = reply(&c, &ok, &caller).unwrap();
        assert_eq!(r["state"], "delivering");
        // 보낸 답에도 답장 대상(폰이 다시 켜도 인용을 그린다)
        assert_eq!(r["quote"], json!({"seq": 1, "part": "response", "text": "끝났어요"}));
        let q: Option<String> = c.query_row("SELECT quote FROM conoti_reply WHERE reply_id = 'rid-bbbb0003'", [], |r| r.get(0)).unwrap();
        assert_eq!(q.as_deref(), Some("response"));
        // 세션에 들어간 폰 답 요청: 대화에서 답장 줄은 quote 로 떼어 준다(폰 답·데스크톱 말 둘 다)
        let phone = format!(
            "{}\n요청: 요청 1\n\n{}\n\n그거 다시",
            crate::conoti::REPLY_HEADER,
            text::quote_line(1, "response", "끝났어요")
        );
        let desk = format!("{}\n\n이것도", text::quote_line(2, "prompt", "요청 2"));
        for (i, p) in [(3, phone), (4, desk)] {
            c.execute(
                "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, status) VALUES ('sess-0001', ?1, ?2, '2026-09-24T00:09:00Z', ?3, 'done')",
                params![format!("u{i}"), i, p],
            )
            .unwrap();
        }
        let ch = chat(&c, &json!({"sid": "sess-0001"}), &caller).unwrap();
        let t = ch["turns"].as_array().unwrap();
        assert_eq!(t.len(), 4);
        assert_eq!(t[2]["prompt"], "그거 다시");
        assert_eq!(t[2]["via_phone"], true);
        assert_eq!(t[2]["quote"], json!({"seq": 1, "part": "response", "text": "끝났어요"}));
        assert_eq!(t[3]["prompt"], "이것도");
        assert_eq!(t[3]["quote"], json!({"seq": 2, "part": "prompt", "text": "요청 2"}));
        assert!(t[0]["quote"].is_null());
    }

    #[test]
    fn image_calls_are_scoped() {
        use base64::Engine;
        let d = std::env::temp_dir().join(format!("aiinbox-rpc-att-{}", std::process::id()));
        crate::paths::set_data_dir_override(d);
        let c = mem();
        db::set_meta(&c, "conoti.bg_resume", "1").unwrap();
        let png = |w: u32| {
            let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(w, 7, image::Rgb([9, 8, 7])));
            let mut b = std::io::Cursor::new(Vec::new());
            img.write_to(&mut b, image::ImageFormat::Png).unwrap();
            base64::engine::general_purpose::STANDARD.encode(b.into_inner())
        };
        let me = Caller { pid: "dev-a", can_reply: true, can_manage: false, can_schedule: false };
        let off = Caller { pid: "dev-b", can_reply: false, can_manage: false, can_schedule: false };
        // 답 보내기가 꺼진 기기는 못 올린다
        assert_eq!(att(&c, &json!({"rid": "rid-aaaa0001", "i": 0, "data": png(31)}), &off).unwrap_err().0, "rejected");
        assert_eq!(att(&c, &json!({"rid": "bad rid", "i": 0, "data": png(31)}), &me).unwrap_err().0, "bad_request");
        let up = att(&c, &json!({"rid": "rid-aaaa0001", "i": 0, "data": png(31)}), &me).unwrap();
        let id = up["id"].as_str().unwrap().to_string();
        // 답에 붙기 전에는 받을 수 없다 · 원본 크기는 내주지 않는다
        assert_eq!(att_get(&c, &json!({"id": id, "size": "thumb"}), &me).unwrap_err().0, "not_found");
        assert_eq!(att_get(&c, &json!({"id": id, "size": "orig"}), &me).unwrap_err().0, "bad_request");
        assert_eq!(att_get(&c, &json!({"id": "../../etc/passwd", "size": "thumb"}), &me).unwrap_err().0, "bad_request");
        // 다른 답 번호·다른 기기로는 못 붙인다
        let other = Caller { pid: "dev-c", can_reply: true, can_manage: false, can_schedule: false };
        assert_eq!(reply(&c, &json!({"sid": "sess-0001", "rid": "rid-aaaa0002", "atts": [id]}), &me).unwrap_err().0, "bad_request");
        assert_eq!(reply(&c, &json!({"sid": "sess-0001", "rid": "rid-aaaa0001", "atts": [id]}), &other).unwrap_err().0, "bad_request");
        assert_eq!(reply(&c, &json!({"sid": "sess-0001", "rid": "rid-aaaa0001", "atts": "x"}), &me).unwrap_err().0, "bad_request");
        // 글 없이 이미지만
        let r = reply(&c, &json!({"sid": "sess-0001", "rid": "rid-aaaa0001", "atts": [id]}), &me).unwrap();
        assert_eq!(r["atts"], json!([id]));
        assert_ne!(r["state"], "rejected");
        let got = att_get(&c, &json!({"id": id, "size": "thumb"}), &me).unwrap();
        assert_eq!(got["mime"], "image/jpeg");
        // 보관한 세션의 이미지는 기록 관리를 허용한 기기만 본다
        c.execute("UPDATE session SET hidden = 1", []).unwrap();
        assert_eq!(att_get(&c, &json!({"id": id, "size": "view"}), &me).unwrap_err().0, "not_found");
        let manager = Caller { pid: "dev-a", can_reply: true, can_manage: true, can_schedule: false };
        assert!(att_get(&c, &json!({"id": id, "size": "view"}), &manager).is_ok());
        c.execute("UPDATE session SET hidden = 0", []).unwrap();
        // 거절된 답(없는 세션)에 붙인 이미지는 남기지 않는다
        let up2 = att(&c, &json!({"rid": "rid-aaaa0003", "i": 0, "data": png(33)}), &me).unwrap();
        let id2 = up2["id"].as_str().unwrap().to_string();
        let r = reply(&c, &json!({"sid": "nope-nope-1", "rid": "rid-aaaa0003", "atts": [id2]}), &me).unwrap();
        assert_eq!(r["state"], "rejected");
        assert_eq!(r["atts"], json!([]));
        assert!(attach::meta(&c, &id2).is_none());
    }

    #[test]
    fn turn_has_markdown_and_read_marks() {
        let c = mem();
        let id: i64 = c.query_row("SELECT id FROM turn WHERE seq = 1", [], |r| r.get(0)).unwrap();
        let caller = Caller { pid: "d1", can_reply: true, can_manage: true, can_schedule: false };
        let t = turn(&c, &json!({"id": id}), &caller).unwrap();
        assert!(t["markdown"].as_str().unwrap().starts_with("# 요청 1"));
        assert_eq!(t["title"], "요청 1");
        assert!(t["next_id"].is_i64());
        assert_eq!(read(&c, &json!({"ids": [id]}), &caller).unwrap(), 1);
        assert_eq!(read(&c, &json!({"ids": [id]}), &caller).unwrap(), 0);
        assert_eq!(sessions(&c, &json!({}), &caller).unwrap()["unread"], 0);
        c.execute("UPDATE turn SET hidden = 1 WHERE id = ?1", [id]).unwrap();
        assert!(turn(&c, &json!({"id": id}), &caller).is_err());
    }

    #[test]
    fn manage_calls_are_scoped_to_the_device_permission() {
        let c = mem();
        let off = Caller { pid: "d1", can_reply: true, can_manage: false, can_schedule: false };
        let on = Caller { pid: "d1", can_reply: true, can_manage: true, can_schedule: false };
        let sid = json!(["sess-0001"]);
        // 기록 관리를 끈 기기: 보관함·관리·정리 모두 거절, 정리 제안 수도 안 싣는다
        assert_eq!(sessions(&c, &json!({"filter": "archived"}), &off).unwrap_err().0, "rejected");
        assert_eq!(manage(&c, &json!({"op": "archive", "sids": sid}), &off).unwrap_err().0, "rejected");
        assert_eq!(tidy(&c, &json!({"kind": "short"}), &off).unwrap_err().0, "rejected");
        let s = sessions(&c, &json!({}), &off).unwrap();
        assert_eq!(s["can_manage"], false);
        assert!(s.get("tidy_short").is_none());
        // 잘못된 요청
        assert_eq!(manage(&c, &json!({"op": "archive", "sids": []}), &on).unwrap_err().0, "bad_request");
        assert_eq!(manage(&c, &json!({"op": "archive", "sids": ["../../etc"]}), &on).unwrap_err().0, "bad_request");
        assert_eq!(manage(&c, &json!({"op": "rm -rf", "sids": sid}), &on).unwrap_err().0, "bad_request");
        let many: Vec<String> = (0..201).map(|i| format!("sess-{i:04}")).collect();
        assert_eq!(manage(&c, &json!({"op": "archive", "sids": many}), &on).unwrap_err().0, "bad_request");
        assert_eq!(tidy(&c, &json!({"kind": "all"}), &on).unwrap_err().0, "bad_request");

        // 보관 → 목록에서 빠지고 보관함에 나온다
        assert_eq!(manage(&c, &json!({"op": "archive", "sids": sid}), &on).unwrap()["n"], 1);
        assert!(sessions(&c, &json!({}), &on).unwrap()["items"].as_array().unwrap().is_empty());
        let a = sessions(&c, &json!({"filter": "archived"}), &on).unwrap();
        assert_eq!(a["items"][0]["archived"], true);
        assert_eq!(a["archived"], 1);
        // 보관한 세션은 허용한 기기만 읽는다 — 답은 막힌다(되돌린 뒤에)
        assert_eq!(chat(&c, &json!({"sid": "sess-0001"}), &off).unwrap_err().0, "not_found");
        let ch = chat(&c, &json!({"sid": "sess-0001"}), &on).unwrap();
        assert_eq!(ch["session"]["archived"], true);
        assert_eq!(ch["session"]["reply_block"], "archived");
        assert_eq!(ch["session"]["can_reply"], false);
        let tid: i64 = c.query_row("SELECT id FROM turn WHERE seq = 1", [], |r| r.get(0)).unwrap();
        assert_eq!(turn(&c, &json!({"id": tid}), &off).unwrap_err().0, "not_found");
        assert!(turn(&c, &json!({"id": tid}), &on).is_ok());
        let r = reply(&c, &json!({"sid": "sess-0001", "rid": "rid-bbbb0001", "text": "해 줘"}), &on).unwrap();
        assert_eq!(r["state"], "rejected");
        // 허용받지 않은 기기는 보관 세션을 "없는 세션"과 구별할 수 없다 · 읽음도 못 바꾼다
        let hid = reply(&c, &json!({"sid": "sess-0001", "rid": "rid-bbbb0002", "text": "해 줘"}), &off).unwrap();
        let none = reply(&c, &json!({"sid": "nope-nope-1", "rid": "rid-bbbb0003", "text": "해 줘"}), &off).unwrap();
        assert_eq!(hid["note"], none["note"]);
        assert_eq!(read(&c, &json!({"sid": "sess-0001"}), &off).unwrap_err().0, "not_found");
        assert_eq!(read(&c, &json!({"ids": [tid]}), &off).unwrap(), 0);
        // PC 의 "폰 답 받기 멈춤"이면 관리도 멈춘다
        db::set_meta(&c, "conoti.paused", "1").unwrap();
        assert_eq!(manage(&c, &json!({"op": "unarchive", "sids": sid}), &on).unwrap_err().0, "rejected");
        assert_eq!(tidy(&c, &json!({"kind": "short"}), &on).unwrap_err().0, "rejected");
        db::set_meta(&c, "conoti.paused", "0").unwrap();
        // 되돌리기 · 고정
        assert_eq!(manage(&c, &json!({"op": "unarchive", "sids": sid}), &on).unwrap()["n"], 1);
        assert_eq!(manage(&c, &json!({"op": "pin", "sids": ["sess-0001", "sess-0001"]}), &on).unwrap()["n"], 1);
        assert_eq!(sessions(&c, &json!({}), &on).unwrap()["items"][0]["pinned"], true);
        // 지우기: 진행 중인 요청이 있으면 남기고 이름을 알려 준다
        let d = manage(&c, &json!({"op": "delete", "sids": sid}), &on).unwrap();
        assert_eq!(d["n"], 0);
        assert_eq!(d["skipped"], json!(["api-refactor"]));
        c.execute("UPDATE turn SET status = 'done'", []).unwrap();
        let d = manage(&c, &json!({"op": "delete", "sids": sid}), &on).unwrap();
        assert_eq!(d["deleted"], json!(["sess-0001"]));
        assert_eq!(chat(&c, &json!({"sid": "sess-0001"}), &on).unwrap_err().0, "not_found");
    }

    // ── 0.10.0: 예약 · 태그 규격 ──

    fn dev(pid: &'static str, reply: bool, manage: bool, sched: bool) -> Caller<'static> {
        Caller { pid, can_reply: reply, can_manage: manage, can_schedule: sched }
    }

    fn add_dev(c: &Connection, pid: &str, reply: i64, sched: i64) {
        c.execute(
            "INSERT INTO relay_device (pid, name, phone_pub, can_reply, can_manage, can_schedule, created_at) VALUES (?1, '폰', 'ff', ?2, ?2, ?3, 't')",
            params![pid, reply, sched],
        )
        .unwrap();
    }

    fn sched_body(rid: &str, when: Value) -> Value {
        json!({"rid": rid, "sid": "sess-0001", "text": "테스트 돌려 줘", "when": when})
    }

    #[test]
    fn sessions_reports_can_schedule_and_per_session_schedule_counts() {
        let c = mem();
        let s = sessions(&c, &json!({}), &dev("d1", true, false, false)).unwrap();
        assert_eq!(s["can_schedule"], false);
        assert_eq!((s["items"][0]["sched_n"].as_i64(), s["items"][0]["sched_held"].as_i64()), (Some(0), Some(0)));
        let on = dev("d1", true, false, true);
        assert_eq!(sessions(&c, &json!({}), &on).unwrap()["can_schedule"], true);
        sched_add(&c, &sched_body("rid-sched-0001", json!({"after_min": 30})), &on).unwrap();
        let s = sessions(&c, &json!({}), &on).unwrap();
        assert_eq!(s["items"][0]["sched_n"], 1);
        // chat 세션 머리에도 같은 값
        let h = chat(&c, &json!({"sid": "sess-0001"}), &on).unwrap();
        assert_eq!((h["session"]["sched_n"].as_i64(), h["session"]["sched_held"].as_i64()), (Some(1), Some(0)));
    }

    #[test]
    fn sessions_and_sched_carry_perm_fields_and_warn_off_needs_schedule_permission() {
        let c = mem();
        let on = dev("d1", true, false, true);
        let s = sessions(&c, &json!({}), &on).unwrap();
        let sid = s["items"][0]["id"].as_str().unwrap().to_string();
        // 모드를 모르면 perm null · 안내 필요
        assert!(s["items"][0]["perm"].is_null());
        assert_eq!(s["items"][0]["sched_warn"], true);
        c.execute(
            "INSERT INTO hook_event (session_id, event, at, detail) VALUES (?1, 'UserPromptSubmit', '2026-09-30T00:00:00.000Z', ?2)",
            params![sid, json!({"permission_mode": "default"}).to_string()],
        )
        .unwrap();
        let s = sessions(&c, &json!({}), &on).unwrap();
        assert_eq!((s["items"][0]["perm"].as_str(), s["items"][0]["sched_warn"].as_bool()), (Some("default"), Some(true)));
        let made = sched_add(&c, &json!({"rid": "rid-sched-0031", "sid": sid, "text": "x", "when": {"after_min": 30}}), &on).unwrap();
        assert_eq!((made["perm"].as_str(), made["sched_warn"].as_bool()), (Some("default"), Some(true)));
        assert_eq!(sched_warn_off(&c, &json!({"sid": sid}), &dev("d1", true, false, false)).unwrap_err().0, "rejected", "예약 허용이 꺼진 기기");
        sched_warn_off(&c, &json!({"sid": sid}), &on).unwrap();
        assert_eq!(sessions(&c, &json!({}), &on).unwrap()["items"][0]["sched_warn"], false);
        assert_eq!(sched_warn_off(&c, &json!({"sid": "../x"}), &on).unwrap_err().0, "bad_request");
        sched_warn_off(&c, &json!({"sid": sid, "off": false}), &on).unwrap();
        assert_eq!(sessions(&c, &json!({}), &on).unwrap()["items"][0]["sched_warn"], true);
    }

    #[test]
    fn sched_add_requires_permission_and_is_idempotent_by_rid() {
        let c = mem();
        let body = sched_body("rid-sched-0001", json!({"after_min": 30}));
        assert_eq!(sched_add(&c, &body, &dev("d1", true, true, false)).unwrap_err().0, "rejected", "예약 허용이 꺼진 기기");
        assert_eq!(sched_add(&c, &body, &dev("d1", false, false, true)).unwrap_err().0, "rejected", "답 보내기가 꺼진 기기");
        let ok = sched_add(&c, &body, &dev("d1", true, false, true)).unwrap();
        assert_eq!((ok["sid"].as_str(), ok["state"].as_str(), ok["mine"].as_bool(), ok["kind"].as_str()), (Some("sess-0001"), Some("active"), Some(true), Some("after")));
        assert_eq!((ok["on_missed"].as_str(), ok["within_min"].as_i64(), ok["busy"].clone(), ok["rev"].as_i64()), (Some("within"), Some(60), Value::Null, Some(1)), "기본 놓침 정책은 60분 안이면 실행");
        assert!(ok["at"].as_str().unwrap().ends_with('Z') && ok["atts"].as_array().unwrap().is_empty() && ok["run"].is_null());
        // 같은 rid 로 다시 → 처음 것, 새로 만들지 않는다
        let again = sched_add(&c, &body, &dev("d1", true, false, true)).unwrap();
        assert_eq!(again["id"], ok["id"]);
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM schedule", [], |r| r.get(0)).unwrap(), 1);
        // 다른 기기의 같은 rid 는 별개
        assert_ne!(sched_add(&c, &body, &dev("d2", true, false, true)).unwrap()["id"], ok["id"]);
        // 전체 멈춤이면 거절
        db::set_meta(&c, "conoti.paused", "1").unwrap();
        assert_eq!(sched_add(&c, &sched_body("rid-sched-0002", json!({"after_min": 5})), &dev("d1", true, false, true)).unwrap_err().0, "rejected");
        db::set_meta(&c, "conoti.paused", "0").unwrap();
        // 세션 차단(폰 답 막기)이면 거절
        c.execute("INSERT INTO conoti_session (session_id, mode) VALUES ('sess-0001', 1)", []).unwrap();
        assert_eq!(sched_add(&c, &sched_body("rid-sched-0003", json!({"after_min": 5})), &dev("d1", true, false, true)).unwrap_err().0, "rejected");
        c.execute("DELETE FROM conoti_session", []).unwrap();
        // 입력 검사
        let d = dev("d1", true, false, true);
        assert_eq!(sched_add(&c, &json!({"rid": "rid-sched-0004", "sid": "no-such-sess", "text": "x", "when": {"after_min": 5}}), &d).unwrap_err().0, "not_found");
        for w in [json!({}), json!({"after_min": 0}), json!({"local": "내일"}), json!({"at": "어제"}), json!(null)] {
            assert!(sched_add(&c, &sched_body("rid-sched-0005", w.clone()), &d).is_err(), "{w}");
        }
        let mut b = sched_body("rid-sched-0006", json!({"after_min": 5}));
        b["on_missed"] = json!("later");
        assert!(sched_add(&c, &b, &d).is_err());
        b = sched_body("rid-sched-0007", json!({"after_min": 5}));
        b["busy"] = json!("bypass");
        assert!(sched_add(&c, &b, &d).is_err());
        b = sched_body("rid-x", json!({"after_min": 5}));
        assert_eq!(sched_add(&c, &b, &d).unwrap_err().0, "bad_request", "rid 형식");
    }

    #[test]
    fn sched_when_forms_and_policy_fields() {
        let c = mem();
        let d = dev("d1", true, false, true);
        // 절대 시각(UTC) — 폰이 자기 시간대로 풀어 보낸다
        let at = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut b = sched_body("rid-sched-0011", json!({"at": at, "tz": "Asia/Seoul"}));
        b["on_missed"] = json!("skip");
        b["busy"] = json!("interrupt");
        let v = sched_add(&c, &b, &d).unwrap();
        assert_eq!((v["kind"].as_str(), v["tz"].as_str(), v["on_missed"].as_str(), v["busy"].as_str()), (Some("once"), Some("Asia/Seoul"), Some("skip"), Some("interrupt")));
        assert_eq!(v["at"].as_str().unwrap()[..16], at[..16]);
        // 현지 시각 + 시간대는 PC 가 푼다
        let local = (chrono::Utc::now() + chrono::Duration::days(3)).format("%Y-%m-%dT09:00").to_string();
        let v = sched_add(&c, &sched_body("rid-sched-0012", json!({"local": local, "tz": "America/New_York"})), &d).unwrap();
        assert_eq!(v["tz"], "America/New_York");
        // within 분
        let mut b = sched_body("rid-sched-0013", json!({"after_min": 5}));
        b["on_missed"] = json!("within");
        b["within_min"] = json!(15);
        assert_eq!(sched_add(&c, &b, &d).unwrap()["within_min"], 15);
    }

    #[test]
    fn sched_list_edit_cancel_act_and_conflict() {
        let c = mem();
        let d = dev("d1", true, false, true);
        let v = sched_add(&c, &sched_body("rid-sched-0021", json!({"after_min": 30})), &d).unwrap();
        let id = v["id"].as_str().unwrap().to_string();
        // 목록은 예약 허용이 없는 기기도 볼 수 있다(읽기만) — 다른 기기가 만든 것은 mine=false
        let l = sched_list(&c, &json!({}), &dev("d9", true, false, false)).unwrap();
        assert_eq!((l["items"].as_array().unwrap().len(), l["items"][0]["mine"].as_bool(), l["can_schedule"].as_bool(), l["active"].as_i64()), (1, Some(false), Some(false), Some(1)));
        assert_eq!(sched_list(&c, &json!({"sid": "sess-0001"}), &d).unwrap()["items"][0]["mine"], true);
        assert!(sched_list(&c, &json!({"sid": "bad sid"}), &d).is_err());
        // 고치기: rev 가 맞으면 통과하고 rev 가 오른다, 낡은 rev 는 conflict
        let edit = |rev: i64, text: &str| json!({"id": id, "rev": rev, "text": text, "when": {"after_min": 45}, "on_missed": "run_once", "busy": "after_work"});
        let e1 = sched_edit(&c, &edit(1, "고친 말"), &d).unwrap();
        assert_eq!((e1["rev"].as_i64(), e1["text"].as_str(), e1["on_missed"].as_str(), e1["busy"].as_str()), (Some(2), Some("고친 말"), Some("run_once"), Some("after_work")));
        assert_eq!(sched_edit(&c, &edit(1, "낡은 화면"), &d).unwrap_err().0, "conflict");
        assert_eq!(sched_edit(&c, &json!({"id": "scnope", "rev": 1, "text": "x", "when": {"after_min": 5}}), &d).unwrap_err().0, "not_found");
        assert_eq!(sched_edit(&c, &edit(2, "x"), &dev("d1", true, false, false)).unwrap_err().0, "rejected");
        // 취소
        assert_eq!(sched_cancel(&c, &json!({"id": id}), &dev("d1", true, false, false)).unwrap_err().0, "rejected");
        sched_cancel(&c, &json!({"id": id}), &d).unwrap();
        assert_eq!(sched_list(&c, &json!({}), &d).unwrap()["items"][0]["state"], "cancelled");
        assert_eq!(sched_cancel(&c, &json!({"id": id}), &d).unwrap_err().0, "rejected", "두 번 취소");
        assert_eq!(sched_cancel(&c, &json!({"id": "scnope"}), &d).unwrap_err().0, "not_found");
        // 발사된 뒤에는 edit 도 conflict
        let v2 = sched_add(&c, &sched_body("rid-sched-0022", json!({"after_min": 1})), &d).unwrap();
        let id2 = v2["id"].as_str().unwrap().to_string();
        c.execute("UPDATE schedule SET next_due_at = NULL WHERE id = ?1", params![id2]).unwrap();
        assert_eq!(sched_edit(&c, &json!({"id": id2, "rev": 1, "text": "늦음", "when": {"after_min": 5}}), &d).unwrap_err().0, "conflict");
        assert!(sched_act(&c, &json!({"id": id2, "op": "kill"}), &d).is_err());
        assert_eq!(sched_act(&c, &json!({"id": id2, "op": "send"}), &d).unwrap_err().0, "rejected", "대기 중인 회차가 없다");
    }

    #[test]
    fn phone_send_of_held_run_uses_phone_reply_checks_and_confirm_mode() {
        let c = mem();
        let d = dev("d1", true, false, true);
        add_dev(&c, "d1", 1, 1);
        let v = sched_add(&c, &sched_body("rid-sched-0031", json!({"after_min": 1})), &d).unwrap();
        let id = v["id"].as_str().unwrap().to_string();
        // 받지 못한 예약(대기)을 만든다
        c.execute("UPDATE schedule SET next_due_at = NULL WHERE id = ?1", params![id]).unwrap();
        c.execute("INSERT INTO schedule_run (schedule_id, occurrence_at, created_at, state, reason, note, notified_at) VALUES (?1, '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z', 'held', 'ended', '세션이 꺼져 있습니다', 't')", params![id]).unwrap();
        let l = sched_list(&c, &json!({}), &d).unwrap();
        assert_eq!((l["items"][0]["run"]["state"].as_str(), l["items"][0]["run"]["reason"].as_str(), l["held"].as_i64()), (Some("held"), Some("ended"), Some(1)));
        // 꺼진 세션 + 이어서 실행 설정 없음 → 폰 답과 같은 규칙으로 거절(세션은 그대로 held)
        let e = sched_act(&c, &json!({"id": id, "op": "send"}), &d).unwrap_err();
        assert_eq!(e.0, "rejected");
        assert!(e.1.contains("꺼져"), "{e:?}");
        assert_eq!(c.query_row::<String, _, _>("SELECT state FROM schedule_run WHERE schedule_id = ?1", params![id], |r| r.get(0)).unwrap(), "held");
        // 버리기는 항상 된다
        sched_act(&c, &json!({"id": id, "op": "drop"}), &d).unwrap();
        assert_eq!(c.query_row::<String, _, _>("SELECT state FROM schedule_run WHERE schedule_id = ?1", params![id], |r| r.get(0)).unwrap(), "cancelled");
    }

    #[test]
    fn phone_made_schedules_are_rechecked_at_fire_and_removed_on_unpair() {
        use crate::sched::{tick, Probe};
        use crate::deliver::{Route, Work};
        struct Up;
        impl Probe for Up {
            fn route(&self, _: &str) -> Route {
                Route::Live
            }
            fn work(&self, _: &str) -> Work {
                Work::Idle
            }
        }
        let due = |c: &Connection| {
            c.execute("UPDATE schedule SET next_due_at = '2026-10-01T00:00:00.000Z'", []).unwrap();
        };
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-01T00:00:10Z").unwrap().with_timezone(&chrono::Utc);
        // 1) 정상: 발사됨
        let c = mem();
        add_dev(&c, "d1", 1, 1);
        sched_add(&c, &sched_body("rid-sched-0041", json!({"after_min": 5})), &dev("d1", true, false, true)).unwrap();
        due(&c);
        tick(&c, at, &Up);
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM conoti_reply WHERE sched IS NOT NULL", [], |r| r.get(0)).unwrap(), 1);
        // 2) 그사이 예약 허용을 껐다 → 발사하지 않고 알림
        let c = mem();
        add_dev(&c, "d1", 1, 1);
        sched_add(&c, &sched_body("rid-sched-0042", json!({"after_min": 5})), &dev("d1", true, false, true)).unwrap();
        c.execute("UPDATE relay_device SET can_schedule = 0", []).unwrap();
        due(&c);
        let rep = tick(&c, at, &Up);
        assert_eq!((c.query_row::<i64, _, _>("SELECT COUNT(*) FROM conoti_reply", [], |r| r.get(0)).unwrap(), rep.alerts.len()), (0, 1));
        // 3) 전체 멈춤
        let c = mem();
        add_dev(&c, "d1", 1, 1);
        sched_add(&c, &sched_body("rid-sched-0043", json!({"after_min": 5})), &dev("d1", true, false, true)).unwrap();
        db::set_meta(&c, "conoti.paused", "1").unwrap();
        due(&c);
        tick(&c, at, &Up);
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM conoti_reply", [], |r| r.get(0)).unwrap(), 0);
        // 4) 기기 연결 해제 → 그 기기가 건 예약이 거두어진다
        let c = mem();
        add_dev(&c, "d1", 1, 1);
        add_dev(&c, "d2", 1, 1);
        sched_add(&c, &sched_body("rid-sched-0044", json!({"after_min": 5})), &dev("d1", true, false, true)).unwrap();
        sched_add(&c, &sched_body("rid-sched-0045", json!({"after_min": 5})), &dev("d2", true, false, true)).unwrap();
        crate::sched::cancel_device(&c, "d1");
        let states: Vec<(String, String)> = c.prepare("SELECT created_by, state FROM schedule ORDER BY created_by").unwrap().query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().flatten().collect();
        assert_eq!(states, vec![("d1".to_string(), "cancelled".to_string()), ("d2".to_string(), "active".to_string())]);
        // 5) 기기 행이 사라진 채(비정상) 발사 시각이 오면 막는다
        let c = mem();
        add_dev(&c, "d1", 1, 1);
        sched_add(&c, &sched_body("rid-sched-0046", json!({"after_min": 5})), &dev("d1", true, false, true)).unwrap();
        c.execute("DELETE FROM relay_device", []).unwrap();
        due(&c);
        assert_eq!(tick(&c, at, &Up).alerts.len(), 1);
    }

    // ── 보안 점검(10-01): 폰 예약이 폰 답과 같은 통제(데스크톱 확인 · 세션 차단 · 기기 해제/허용 끔)를 타는가 ──

    struct UpProbe;
    impl crate::sched::Probe for UpProbe {
        fn route(&self, _: &str) -> crate::deliver::Route {
            crate::deliver::Route::Live
        }
        fn work(&self, _: &str) -> crate::deliver::Work {
            crate::deliver::Work::Idle
        }
    }

    /// 세션에 넣은 말을 적어 두는 가짜 전달기
    #[derive(Default)]
    struct Sink(std::cell::RefCell<Vec<String>>);
    impl conoti::Deliver for Sink {
        fn deliver(&self, _t: &conoti::Target, id: &str, _text: &str, _d: bool) -> conoti::Outcome {
            self.0.borrow_mut().push(id.to_string());
            conoti::Outcome::Done("전달".into())
        }
    }

    /// 폰 d1(답·예약 허용)이 건 예약을 지금 발사한다. 파이프라인의 3시간 상한은 실제 현재 시각으로 보므로 발사 시각도 현재 기준.
    /// 시험 세션은 실제로 떠 있지 않다(`deliver::route` = 꺼짐) — 폰 경로의 "꺼진 세션" 거절을 피하려고 이어서 실행 설정을 켠다(가짜 전달기라 실행은 없다).
    fn fired_phone_schedule(confirm: bool) -> (Connection, String) {
        let c = mem();
        add_dev(&c, "d1", 1, 1);
        db::set_meta(&c, "conoti.bg_resume", "1").unwrap();
        db::set_meta(&c, "conoti.confirm", if confirm { "1" } else { "0" }).unwrap();
        let v = sched_add(&c, &sched_body("rid-sched-0061", json!({"after_min": 1})), &dev("d1", true, false, true)).unwrap();
        let now = chrono::Utc::now();
        c.execute("UPDATE schedule SET next_due_at = ?1", params![(now - chrono::Duration::seconds(1)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)]).unwrap();
        crate::sched::tick(&c, now, &UpProbe);
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM conoti_reply WHERE sched IS NOT NULL", [], |r| r.get(0)).unwrap(), 1, "발사됨");
        (c, v["id"].as_str().unwrap().to_string())
    }

    fn line_of(c: &Connection) -> (String, String) {
        c.query_row("SELECT state, COALESCE(device, '') FROM conoti_reply WHERE sched IS NOT NULL", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap()
    }

    #[test]
    fn phone_schedule_waits_for_desktop_confirm_like_a_phone_reply() {
        let (c, id) = fired_phone_schedule(true);
        let mut pipe = conoti::Pipeline::new(c);
        let sink = Sink::default();
        pipe.tick(&sink);
        assert!(sink.0.borrow().is_empty(), "데스크톱 확인 모드인데 PC 허용 없이 세션에 넣었다");
        assert_eq!(line_of(pipe.conn()), ("confirm".to_string(), "d1".to_string()), "폰이 건 예약은 그 기기의 줄로 확인 대기");
        // 폰 화면의 보낸 말(replies)에는 예약 줄이 끼지 않는다(예약은 sched_list 로 본다 — 예전과 같다)
        assert!(conoti::replies_for(pipe.conn(), "sess-0001", 20).is_empty());
        // PC 가 거절하면 회차는 끝(대기 → 재알림으로 돌지 않는다)
        pipe.conn().execute("UPDATE conoti_reply SET state = 'rejected', note = ?1 WHERE state = 'confirm'", params![conoti::DENIED_NOTE]).unwrap();
        let rep = crate::sched::tick(pipe.conn(), chrono::Utc::now(), &UpProbe);
        assert!(rep.alerts.is_empty(), "PC 가 거절한 것을 '전달되지 못했어요'로 다시 알리지 않는다");
        let st: (String, String) = pipe.conn().query_row("SELECT r.state, s.state FROM schedule_run r JOIN schedule s ON s.id = r.schedule_id WHERE s.id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(st, ("cancelled".to_string(), "done".to_string()));
        // PC 가 허용하면 들어간다
        let (c, _) = fired_phone_schedule(true);
        let mut pipe = conoti::Pipeline::new(c);
        pipe.tick(&sink);
        pipe.conn().execute("UPDATE conoti_reply SET state = 'delivering', acked = 'approved' WHERE state = 'confirm'", []).unwrap();
        pipe.tick(&sink);
        assert_eq!(sink.0.borrow().len(), 1);
        assert_eq!(line_of(pipe.conn()).0, "delivered");
    }

    #[test]
    fn phone_schedule_is_rechecked_at_delivery_session_block_and_device_permissions() {
        // 확인 모드가 아니어도, 발사 뒤 전달 전에 PC 가 막으면 넣지 않는다(바쁜 세션 뒤에서 최대 3시간 기다릴 수 있다)
        type Setup = fn(&Connection);
        let cases: [(&str, Setup); 4] = [
            ("세션 차단(폰 답 막기)", |c| {
                c.execute("INSERT INTO conoti_session (session_id, mode) VALUES ('sess-0001', 1)", []).unwrap();
            }),
            ("답 보내기 끔", |c| {
                c.execute("UPDATE relay_device SET can_reply = 0", []).unwrap();
            }),
            ("예약 허용 끔", |c| {
                c.execute("UPDATE relay_device SET can_schedule = 0", []).unwrap();
            }),
            ("기기 행이 사라짐", |c| {
                c.execute("DELETE FROM relay_device", []).unwrap();
            }),
        ];
        for (what, setup) in cases {
            let (c, id) = fired_phone_schedule(false);
            setup(&c);
            let mut pipe = conoti::Pipeline::new(c);
            let sink = Sink::default();
            pipe.tick(&sink);
            assert!(sink.0.borrow().is_empty(), "{what}: 막았는데 세션에 넣었다");
            assert_eq!(line_of(pipe.conn()).0, "rejected", "{what}");
            // 예약 쪽은 받지 못한 것으로(held) — PC 앞의 사용자가 보내기/버리기
            crate::sched::tick(pipe.conn(), chrono::Utc::now(), &UpProbe);
            let run: String = pipe.conn().query_row("SELECT state FROM schedule_run WHERE schedule_id = ?1", params![id], |r| r.get(0)).unwrap();
            assert_eq!(run, "held", "{what}");
        }
        // 막는 것이 없으면 그대로 들어간다(예약 머리말·예약 표식 유지)
        let (c, _) = fired_phone_schedule(false);
        let mut pipe = conoti::Pipeline::new(c);
        let sink = Sink::default();
        pipe.tick(&sink);
        assert_eq!((sink.0.borrow().len(), line_of(pipe.conn()).0), (1, "delivered".to_string()));
    }

    #[test]
    fn unpairing_takes_back_a_phone_schedule_waiting_for_confirm() {
        let (c, id) = fired_phone_schedule(true);
        let mut pipe = conoti::Pipeline::new(c);
        let sink = Sink::default();
        pipe.tick(&sink);
        assert_eq!(line_of(pipe.conn()).0, "confirm");
        // 기기 해제(remove_device 가 하는 DB 일) — 확인 대기 줄까지 거둔다
        crate::sched::cancel_device(pipe.conn(), "d1");
        pipe.conn().execute("DELETE FROM relay_device WHERE pid = 'd1'", []).unwrap();
        assert_eq!(line_of(pipe.conn()).0, "rejected");
        let st: (String, String) = pipe.conn().query_row("SELECT r.state, s.state FROM schedule_run r JOIN schedule s ON s.id = r.schedule_id WHERE s.id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(st, ("cancelled".to_string(), "done".to_string()));
        // PC 화면에 남은 확인 단추를 눌러도(이미 처리됨) 들어가지 않는다
        assert_eq!(pipe.conn().execute("UPDATE conoti_reply SET state = 'delivering', acked = 'approved' WHERE state = 'confirm'", []).unwrap(), 0);
        pipe.tick(&sink);
        assert!(sink.0.borrow().is_empty());
    }

    #[test]
    fn pc_can_still_cancel_a_fired_phone_schedule_from_the_chat() {
        let (c, id) = fired_phone_schedule(false);
        let rid: String = c.query_row("SELECT reply_id FROM conoti_reply WHERE sched IS NOT NULL", [], |r| r.get(0)).unwrap();
        conoti::cancel_desktop(&c, &rid).unwrap();
        assert_eq!(line_of(&c).0, "rejected");
        crate::sched::tick(&c, chrono::Utc::now(), &UpProbe);
        assert_eq!(c.query_row::<String, _, _>("SELECT state FROM schedule_run WHERE schedule_id = ?1", params![id], |r| r.get(0)).unwrap(), "cancelled");
        // 폰 답(예약 아님)은 여전히 PC 의 "보내기 취소" 대상이 아니다
        add_dev(&c, "d3", 1, 0);
        conoti::accept_reply(&c, "d3", true, "sess-0001", None, "폰 답", "rid-plain-0001", &[], None).unwrap();
        conoti::cancel_desktop(&c, "rid-plain-0001").unwrap();
        assert_ne!(c.query_row::<String, _, _>("SELECT COALESCE(note, '') FROM conoti_reply WHERE reply_id = 'rid-plain-0001'", [], |r| r.get(0)).unwrap(), "보내기 취소");
    }

    #[test]
    fn a_phone_reply_cannot_squat_the_queue_key_of_a_schedule() {
        // 회차의 대기열 번호 = `<예약 id>-<예정 시각 ms>` — 둘 다 sched_list 로 보인다. 폰이 그 번호로 답을 먼저 넣어 두면
        // 발사가 INSERT OR IGNORE 로 조용히 그 답에 묶여(PC 예약 대신 폰의 글이 그 예약의 결과가 된다) 안 된다
        let c = mem();
        add_dev(&c, "d1", 1, 1);
        db::set_meta(&c, "conoti.bg_resume", "1").unwrap();
        let id = desktop_schedule(&c, "PC 예약 원문");
        let now = chrono::Utc::now();
        let due = (now - chrono::Duration::seconds(1)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        c.execute("UPDATE schedule SET next_due_at = ?1", params![due]).unwrap();
        let key = format!("{id}-{}", chrono::DateTime::parse_from_rfc3339(&due).unwrap().timestamp_millis());
        assert_eq!(reply(&c, &json!({"sid": "sess-0001", "text": "폰이 끼워 넣은 말", "rid": key}), &dev("d1", true, false, false)).unwrap()["state"], "delivering");
        crate::sched::tick(&c, now, &UpProbe);
        let (state, linked): (String, Option<String>) = c.query_row("SELECT state, reply_id FROM schedule_run WHERE schedule_id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(state, "held", "남의 답에 묶여 발사된 것으로 처리됐다");
        assert_ne!(linked.as_deref(), Some(key.as_str()));
        // 폰의 답은 그 답대로(예약 표식이 붙지 않는다)
        assert!(c.query_row::<Option<String>, _, _>("SELECT sched FROM conoti_reply WHERE reply_id = ?1", params![key], |r| r.get(0)).unwrap().is_none());
    }

    fn desktop_schedule(c: &Connection, text: &str) -> String {
        let n = crate::sched::NewSchedule {
            session_id: "sess-0001".into(),
            text: text.into(),
            atts: vec![],
            quote_turn: None,
            quote_part: None,
            when: crate::sched::WhenIn::After { min: 30 },
            on_missed: "within".into(),
            missed_within_min: None,
            busy_policy: None,
            rid: None,
        };
        crate::sched::add(c, chrono::Utc::now(), conoti::DESKTOP, &n, &UpProbe).unwrap()["id"].as_str().unwrap().to_string()
    }

    #[test]
    fn phone_can_edit_and_cancel_only_its_own_schedules() {
        let c = mem();
        add_dev(&c, "d1", 1, 1);
        add_dev(&c, "d2", 1, 1);
        let (d1, d2) = (dev("d1", true, false, true), dev("d2", true, false, true));
        let pc = desktop_schedule(&c, "PC 에서 건 예약");
        let other = sched_add(&c, &sched_body("rid-sched-0071", json!({"after_min": 30})), &d2).unwrap()["id"].as_str().unwrap().to_string();
        let mine = sched_add(&c, &sched_body("rid-sched-0072", json!({"after_min": 30})), &d1).unwrap()["id"].as_str().unwrap().to_string();
        let edit = |id: &str| json!({"id": id, "rev": 1, "text": "rm -rf 로 바꿔 줘", "when": {"after_min": 2}});
        // PC 가 만든 예약 · 다른 폰이 만든 예약 → 고치기·취소 거절(rejected), 내용·상태 그대로
        for id in [&pc, &other] {
            let e = sched_edit(&c, &edit(id), &d1).unwrap_err();
            assert_eq!(e.0, "rejected", "{e:?}");
            assert!(e.1.contains("이 기기가 만든 예약만"), "{e:?}");
            assert_eq!(sched_cancel(&c, &json!({"id": id}), &d1).unwrap_err().0, "rejected");
            let (text, state, rev): (String, String, i64) = c.query_row("SELECT text, state, rev FROM schedule WHERE id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap();
            assert!(!text.contains("rm -rf") && state == "active" && rev == 1, "{text} {state} {rev}");
        }
        // 자기 예약은 예전처럼 고치고 취소한다(옛 폰의 정상 흐름)
        assert_eq!(sched_edit(&c, &edit(&mine), &d1).unwrap()["rev"], 2);
        sched_cancel(&c, &json!({"id": mine}), &d1).unwrap();
        // 없는 예약은 여전히 not_found
        assert_eq!(sched_edit(&c, &edit("scnope"), &d1).unwrap_err().0, "not_found");
        // PC 가 만든 예약이 받지 못해 대기(held)면 폰도 처리(보내기·버리기)할 수 있다 — 규격 §4-3(보내기는 폰 답과 같은 검사·확인)
        c.execute("UPDATE schedule SET next_due_at = NULL WHERE id = ?1", params![pc]).unwrap();
        c.execute("INSERT INTO schedule_run (schedule_id, occurrence_at, created_at, state, reason, notified_at) VALUES (?1, '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z', 'held', 'ended', 't')", params![pc]).unwrap();
        sched_act(&c, &json!({"id": pc, "op": "drop"}), &d1).unwrap();
        assert_eq!(c.query_row::<String, _, _>("SELECT state FROM schedule_run WHERE schedule_id = ?1", params![pc], |r| r.get(0)).unwrap(), "cancelled");
    }

    #[test]
    fn sched_list_hides_bodies_from_devices_without_schedule_permission() {
        let c = mem();
        add_dev(&c, "d1", 1, 1);
        desktop_schedule(&c, "PC 의 비밀 계획");
        let full = sched_list(&c, &json!({}), &dev("d1", true, false, true)).unwrap();
        assert_eq!(full["items"][0]["text"], "PC 의 비밀 계획");
        for who in [dev("d9", true, false, false), dev("d9", false, false, true), dev("d9", false, false, false)] {
            let l = sched_list(&c, &json!({}), &who).unwrap();
            let it = &l["items"][0];
            assert_eq!(it["text"], "", "예약 권한 없는 기기에 본문");
            assert_eq!(it["atts"], json!([]));
            // 응답 모양(필드·타입)은 그대로 — 값만 줄인다
            let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
            assert_eq!(keys(it), keys(&full["items"][0]));
            assert_eq!((l["active"].as_i64(), it["state"].as_str()), (Some(1), Some("active")));
        }
    }

    #[test]
    fn tag_list_leaves_out_tags_seen_only_in_archived_sessions_for_devices_without_manage() {
        let c = tagged_mem();
        // 보관한 세션(sess-0002)에만 붙은 태그 하나
        let hidden_tag = crate::tags::create_tag(&c, "감춘프로젝트", "", false).unwrap();
        let t2: i64 = c.query_row("SELECT id FROM turn WHERE session_id = 'sess-0002'", [], |r| r.get(0)).unwrap();
        crate::tags::set_turn_tag(&c, t2, hidden_tag, true).unwrap();
        archive::set_archived(&c, &["sess-0002".to_string()], true).unwrap();
        let names = |l: &Value| l["items"].as_array().unwrap().iter().map(|t| (t["n"].as_str().unwrap().to_string(), t["k"].as_i64().unwrap())).collect::<std::collections::HashMap<_, _>>();
        let plain = names(&tag_list(&c, &json!({}), &dev("d1", true, false, false)).unwrap());
        assert!(!plain.contains_key("감춘프로젝트"), "보관 세션을 볼 수 없는 기기에 보관 세션의 태그");
        assert_eq!(plain.get("베타"), Some(&1), "개수도 보이는 세션만");
        assert_eq!(plain.get("알파"), Some(&2));
        let managed = names(&tag_list(&c, &json!({}), &dev("d1", true, true, false)).unwrap());
        assert_eq!((managed.get("감춘프로젝트"), managed.get("베타")), (Some(&1), Some(&2)), "기록 관리 허용 기기는 전체");
    }

    #[test]
    fn a_phone_cannot_grow_the_schedule_table_without_bound() {
        let c = mem();
        let d = dev("d1", true, false, true);
        for i in 0..crate::sched::MAX_DEVICE_ADDS_PER_DAY {
            let v = sched_add(&c, &sched_body(&format!("rid-sched-{i:05}"), json!({"after_min": 30})), &d).unwrap();
            sched_cancel(&c, &json!({"id": v["id"]}), &d).unwrap();
        }
        let e = sched_add(&c, &sched_body("rid-sched-99999", json!({"after_min": 30})), &d).unwrap_err();
        assert_eq!(e.0, "rejected", "{e:?}");
        // 같은 rid 재전송은 새로 만드는 것이 아니라 그대로 처음 결과
        assert!(sched_add(&c, &sched_body("rid-sched-00000", json!({"after_min": 30})), &d).is_ok());
        // 다른 기기·PC 는 따로 센다
        assert!(sched_add(&c, &sched_body("rid-sched-99999", json!({"after_min": 30})), &dev("d2", true, false, true)).is_ok());
        desktop_schedule(&c, "PC 예약");
    }

    #[test]
    fn phone_uploaded_images_are_protected_by_the_schedule_and_visible_in_the_list() {
        use base64::Engine;
        let dir = std::env::temp_dir().join(format!("aiinbox-rpc-sched-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        crate::paths::set_data_dir_override(dir.clone()); // 먼저 정해진 것이 이기지만 어느 쪽이든 시험용 임시 폴더다
        let c = mem();
        let d = dev("d1", true, true, true);
        add_dev(&c, "d1", 1, 1);
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(8, 8, |x, y| image::Rgb([x as u8 * 9, y as u8 * 9, 40])));
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let b64 = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
        let up = att(&c, &json!({"rid": "rid-sched-0051", "i": 0, "data": b64}), &d).unwrap();
        let aid = up["id"].as_str().unwrap().to_string();
        // 다른 rid 로는 못 붙인다
        let mut bad_body = sched_body("rid-sched-0052", json!({"after_min": 30}));
        bad_body["atts"] = json!([aid.clone()]);
        assert_eq!(sched_add(&c, &bad_body, &d).unwrap_err().0, "bad_request");
        // 같은 rid 로 올린 이미지는 붙는다
        let mut body = sched_body("rid-sched-0051", json!({"after_min": 30}));
        body["atts"] = json!([aid.clone()]);
        let v = sched_add(&c, &body, &d).unwrap();
        assert_eq!(v["atts"], json!([aid.clone()]));
        // 폰이 올린 지 1시간이 넘어도(정리 시계) 예약이 걸려 있는 동안은 지우지 않는다
        c.execute("UPDATE attachment SET touched_at = '2026-01-01T00:00:00.000Z' WHERE id = ?1", params![aid]).unwrap();
        c.execute("DELETE FROM att_upload", []).unwrap();
        assert_eq!(crate::attach::gc(&c), 0);
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM attachment WHERE id = ?1", params![aid], |r| r.get(0)).unwrap(), 1);
        // 폰이 목록의 이미지를 미리볼 수 있다
        assert!(att_get(&c, &json!({"id": aid, "size": "thumb"}), &d).is_ok());
        // 재전송(같은 rid)은 이미지 기록이 사라진 뒤에도 처음 결과
        let again = sched_add(&c, &body, &d).unwrap();
        assert_eq!(again["id"], v["id"]);
        // 취소하면 예약 보호가 풀린다(다음 정리 때 지워짐)
        sched_cancel(&c, &json!({"id": v["id"]}), &d).unwrap();
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM schedule_att", [], |r| r.get(0)).unwrap(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tagged_mem() -> Connection {
        let c = mem();
        // 요청 1 = 알파 · 요청 2 = 알파+베타 · 두 번째 세션의 요청 = 베타
        c.execute("INSERT INTO session (id, project_dir, first_at, last_at, live_name) VALUES ('sess-0002', '/w/b', '2026-09-24T00:00:00Z', '2026-09-24T00:00:00Z', '다른 세션')", []).unwrap();
        c.execute("INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, status) VALUES ('sess-0002', 'x1', 1, '2026-09-24T00:00:00Z', '베타 요청', 'done')", []).unwrap();
        let (a, b) = (crate::tags::create_tag(&c, "알파", "", false).unwrap(), crate::tags::create_tag(&c, "베타", "", false).unwrap());
        let tid = |sid: &str, seq: i64| c.query_row("SELECT id FROM turn WHERE session_id = ?1 AND seq = ?2", params![sid, seq], |r| r.get::<_, i64>(0)).unwrap();
        crate::tags::set_turn_tag(&c, tid("sess-0001", 1), a, true).unwrap();
        crate::tags::set_turn_tag(&c, tid("sess-0001", 2), a, true).unwrap();
        crate::tags::set_turn_tag(&c, tid("sess-0001", 2), b, true).unwrap();
        crate::tags::set_turn_tag(&c, tid("sess-0002", 1), b, true).unwrap();
        c
    }

    #[test]
    fn tag_list_gives_every_tag_with_counts_to_any_paired_device() {
        let c = tagged_mem();
        let l = tag_list(&c, &json!({}), &dev("d1", false, false, false)).unwrap();
        let items = l["items"].as_array().unwrap();
        let find = |n: &str| items.iter().find(|t| t["n"] == n).unwrap().clone();
        assert_eq!((find("알파")["k"].as_i64(), find("베타")["k"].as_i64()), (Some(2), Some(2)));
        assert!(find("알파")["c"].as_str().unwrap().starts_with('#') && find("알파")["minor"] == false);
        assert!(items.iter().any(|t| t["minor"] == true), "기본 종류 태그(작은 태그)도 전체 목록에 있다");
    }

    #[test]
    fn tag_set_adds_manual_creates_missing_reuses_case_insensitively_and_removes() {
        let c = tagged_mem();
        let t1: i64 = c.query_row("SELECT id FROM turn WHERE session_id = 'sess-0001' AND seq = 1", [], |r| r.get(0)).unwrap();
        let m = dev("d1", true, true, false);
        // 없는 이름은 새로 만들고 붙인다, 있는 이름은 대소문자 무시로 재사용(새로 만들지 않는다)
        let before: i64 = c.query_row("SELECT COUNT(*) FROM tag", [], |r| r.get(0)).unwrap();
        let r = tag_set(&c, &json!({"turn_id": t1, "add": ["Gamma", "베타"]}), &m).unwrap();
        let names: Vec<String> = r["tags"].as_array().unwrap().iter().map(|t| t["n"].as_str().unwrap().to_string()).collect();
        assert!(names.contains(&"알파".to_string()) && names.contains(&"베타".to_string()) && names.contains(&"Gamma".to_string()), "{names:?}");
        assert!(r["tags"][0]["c"].as_str().unwrap().starts_with('#'));
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM tag", [], |r| r.get(0)).unwrap(), before + 1);
        let r = tag_set(&c, &json!({"turn_id": t1, "add": ["gamma"]}), &m).unwrap();
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM tag", [], |r| r.get(0)).unwrap(), before + 1, "gamma 는 Gamma 를 재사용");
        let _ = r;
        let state: String = c.query_row("SELECT x.state FROM turn_tag x JOIN tag g ON g.id = x.tag_id WHERE x.turn_id = ?1 AND g.name = 'gamma'", params![t1], |r| r.get(0)).unwrap();
        assert_eq!(state, "manual");
        // remove = off: 결과에서 빠지고 자동이 다시 붙이지 않는 표식(off)
        let r = tag_set(&c, &json!({"turn_id": t1, "remove": ["알파", "없는태그"]}), &m).unwrap();
        let names: Vec<&str> = r["tags"].as_array().unwrap().iter().map(|t| t["n"].as_str().unwrap()).collect();
        assert!(!names.contains(&"알파"));
        let off: String = c.query_row("SELECT x.state FROM turn_tag x JOIN tag g ON g.id = x.tag_id WHERE x.turn_id = ?1 AND g.name = '알파'", params![t1], |r| r.get(0)).unwrap();
        assert_eq!(off, "off");
        // 검사
        assert_eq!(tag_set(&c, &json!({"turn_id": t1, "add": ["x"]}), &dev("d1", true, false, false)).unwrap_err().0, "rejected", "기록 관리 허용이 필요");
        db::set_meta(&c, "conoti.paused", "1").unwrap();
        assert_eq!(tag_set(&c, &json!({"turn_id": t1, "add": ["x"]}), &m).unwrap_err().0, "rejected", "폰 답 받기 멈춤이면 거절");
        db::set_meta(&c, "conoti.paused", "0").unwrap();
        assert_eq!(tag_set(&c, &json!({"turn_id": 99999, "add": ["x"]}), &m).unwrap_err().0, "not_found");
        c.execute("UPDATE turn SET hidden = 1 WHERE id = ?1", params![t1]).unwrap();
        assert_eq!(tag_set(&c, &json!({"turn_id": t1, "add": ["x"]}), &m).unwrap_err().0, "not_found", "숨긴 요청");
        c.execute("UPDATE turn SET hidden = 0 WHERE id = ?1", params![t1]).unwrap();
        assert_eq!(tag_set(&c, &json!({"turn_id": t1}), &m).unwrap_err().0, "bad_request");
        assert_eq!(tag_set(&c, &json!({"turn_id": t1, "add": ["가".repeat(30)]}), &m).unwrap_err().0, "bad_request", "24자 넘는 이름");
        assert_eq!(tag_set(&c, &json!({"turn_id": t1, "add": (0..11).map(|i| format!("t{i}")).collect::<Vec<_>>()}), &m).unwrap_err().0, "bad_request");
        // 보관한 세션의 요청은 기록 관리를 허용한 기기만
        c.execute("UPDATE session SET hidden = 1 WHERE id = 'sess-0001'", []).unwrap();
        assert_eq!(tag_set(&c, &json!({"turn_id": t1, "add": ["x"]}), &m).is_ok(), true);
    }

    #[test]
    fn sessions_tag_filter_and_chat_tags_filter_with_any_and_all() {
        let c = tagged_mem();
        let m = dev("d1", true, true, false);
        let ids = |v: &Value| -> Vec<String> { v["items"].as_array().unwrap().iter().map(|i| i["id"].as_str().unwrap().to_string()).collect() };
        // sessions {tag}: 그 태그가 붙은 요청이 하나라도 있는 세션(대소문자 무시), 없는 태그는 빈 목록
        assert_eq!(ids(&sessions(&c, &json!({"tag": "알파"}), &m).unwrap()), vec!["sess-0001"]);
        let mut both = ids(&sessions(&c, &json!({"tag": "베타"}), &m).unwrap());
        both.sort();
        assert_eq!(both, vec!["sess-0001", "sess-0002"]);
        assert!(ids(&sessions(&c, &json!({"tag": "없는태그"}), &m).unwrap()).is_empty());
        assert_eq!(ids(&sessions(&c, &json!({"tag": "  "}), &m).unwrap()).len(), 2, "빈 값은 무시");
        // chat {tags}: 기본 = 모두 가진 요청만
        let seqs = |v: &Value| -> Vec<i64> { v["turns"].as_array().unwrap().iter().map(|t| t["seq"].as_i64().unwrap()).collect() };
        assert_eq!(seqs(&chat(&c, &json!({"sid": "sess-0001", "tags": ["알파"]}), &m).unwrap()), vec![1, 2]);
        assert_eq!(seqs(&chat(&c, &json!({"sid": "sess-0001", "tags": ["알파", "베타"]}), &m).unwrap()), vec![2], "모두 가진 요청");
        assert_eq!(seqs(&chat(&c, &json!({"sid": "sess-0001", "tags": ["알파", "베타"], "any": true}), &m).unwrap()), vec![1, 2], "하나라도");
        assert_eq!(seqs(&chat(&c, &json!({"sid": "sess-0001", "tags": ["베타"]}), &m).unwrap()), vec![2]);
        assert_eq!(seqs(&chat(&c, &json!({"sid": "sess-0001", "tags": ["ALPHA"]}), &m).unwrap()), Vec::<i64>::new(), "없는 이름 — 모두 조건은 빈 결과");
        assert_eq!(seqs(&chat(&c, &json!({"sid": "sess-0001", "tags": ["알파", "없음"]}), &m).unwrap()), Vec::<i64>::new());
        assert_eq!(seqs(&chat(&c, &json!({"sid": "sess-0001", "tags": ["없음"], "any": true}), &m).unwrap()), Vec::<i64>::new(), "아는 이름이 하나도 없으면 전체가 아니라 빈 결과");
        assert_eq!(seqs(&chat(&c, &json!({"sid": "sess-0001", "tags": ["알파", "없음"], "any": true}), &m).unwrap()), vec![1, 2], "하나라도 — 없는 이름은 무시");
        // 인자가 없거나 비었으면 전체(옛 앱과 같다)
        assert_eq!(seqs(&chat(&c, &json!({"sid": "sess-0001"}), &m).unwrap()), vec![1, 2]);
        assert_eq!(seqs(&chat(&c, &json!({"sid": "sess-0001", "tags": []}), &m).unwrap()), vec![1, 2]);
        assert!(chat(&c, &json!({"sid": "sess-0001", "tags": "알파"}), &m).is_err());
        assert!(chat(&c, &json!({"sid": "sess-0001", "tags": (0..11).map(|i| format!("t{i}")).collect::<Vec<_>>()}), &m).is_err());
        // before/limit 과 함께 쓴다
        let v = chat(&c, &json!({"sid": "sess-0001", "tags": ["알파"], "limit": 1}), &m).unwrap();
        assert_eq!((seqs(&v), v["has_more"].clone()), (vec![2], json!(true)));
        // 모델 제안(ai)·뗀(off) 표식은 걸러지지 않는다
        let t1: i64 = c.query_row("SELECT id FROM turn WHERE session_id = 'sess-0001' AND seq = 1", [], |r| r.get(0)).unwrap();
        let a: i64 = c.query_row("SELECT id FROM tag WHERE name = '알파'", [], |r| r.get(0)).unwrap();
        crate::tags::set_turn_tag(&c, t1, a, false).unwrap();
        assert_eq!(seqs(&chat(&c, &json!({"sid": "sess-0001", "tags": ["알파"]}), &m).unwrap()), vec![2]);
    }
}
