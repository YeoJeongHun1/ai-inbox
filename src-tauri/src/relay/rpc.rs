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
    let items = api::list_sessions_on(conn, filter, "").map_err(|e| ("internal", e))?;
    let items: Vec<Value> = items.iter().take(200).map(|i| session_row(&serde_json::to_value(i).unwrap_or_default())).collect();
    let counts = api::counts_of(conn);
    let mut out = json!({
        "items": items, "unread": counts.unread, "attention": counts.attention, "active": counts.active,
        "can_manage": caller.can_manage,
    });
    if caller.can_manage {
        let st = archive::stats(conn);
        out["archived"] = json!(st.archived);
        out["tidy_short"] = json!(st.tidy_short);
        out["tidy_idle"] = json!(st.tidy_idle);
    }
    Ok(out)
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
    let page = api::get_chat_on(conn, sid, before, Some(limit)).map_err(|e| ("internal", e))?;
    let v = serde_json::to_value(&page).unwrap_or_default();
    let turns: Vec<Value> = v["turns"].as_array().map(|a| a.iter().map(bubble).collect()).unwrap_or_default();
    Ok(json!({
        "session": head(conn, &v["session"], caller),
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
        let caller = Caller { pid: "d1", can_reply: true, can_manage: true };
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
    fn reply_quote_is_checked_and_shown_as_its_own_field() {
        let c = mem();
        db::set_meta(&c, "conoti.bg_resume", "1").unwrap();
        c.execute("INSERT INTO relay_device (pid, name, phone_pub, can_reply, created_at) VALUES ('d1', '폰', 'ab', 1, 'x')", []).unwrap();
        let caller = Caller { pid: "d1", can_reply: true, can_manage: true };
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
        let me = Caller { pid: "dev-a", can_reply: true, can_manage: false };
        let off = Caller { pid: "dev-b", can_reply: false, can_manage: false };
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
        let other = Caller { pid: "dev-c", can_reply: true, can_manage: false };
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
        let manager = Caller { pid: "dev-a", can_reply: true, can_manage: true };
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
        let caller = Caller { pid: "d1", can_reply: true, can_manage: true };
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
        let off = Caller { pid: "d1", can_reply: true, can_manage: false };
        let on = Caller { pid: "d1", can_reply: true, can_manage: true };
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
}
