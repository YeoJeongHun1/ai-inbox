//! 폰 답 처리 — 코노티 앱에서 종단간 통로(`relay`)로 들어온 답을 검사하고 세션에 넣는다.
//!
//! 🚨 폰 답은 곧 원격 명령이다(권한 확인을 끈 세션이면 코드 실행). 그래서 겹으로 막는다:
//!   PC 에서 허용한 기기(페어링 때 사용자 확인)의 암호 통로로 온 답만 · 기기별 "답 보내기" ·
//!   세션별 차단 · 전체 멈춤 스위치 · 데스크톱 확인 옵션 · 길이·제어문자 검사 · 전달 기록.
//! 중계 서버는 답을 읽을 수도 만들 수도 없다(암호문만 넘긴다) — 서버를 믿지 않아도 된다.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::deliver::Route;
use crate::{attach, db, text, time};

/// 세션에 넣을 때 폰 답을 감싸는 머리말. 화면은 이 머리말로 "폰에서 온 요청"을 알아본다.
pub const REPLY_HEADER: &str = "폰에서 온 사용자 답 (AI Inbox · 코노티)";
/// 데스크톱 입력창에서 보낸 말의 머리말. 수집할 때 떼고 요청 출처를 `inbox` 로 남긴다(`ingest.rs`).
pub const INBOX_HEADER: &str = "AI Inbox 앱에서 보낸 사용자 메시지";
/// 예약 전송(`sched.rs`)이 시각이 되어 넣는 말의 머리말. 수집할 때 떼고 요청 출처를 `sched` 로 남긴다 — 모델이 "지금 사람이 친 말"이 아님을 알게 한다.
pub const SCHED_HEADER: &str = "AI Inbox 예약 메시지 (사용자가 미리 예약해 둔 시각에 자동으로 전달됨)";
pub const MAX_REPLY_CHARS: usize = 4000;
/// 데스크톱 입력창에서 보낸 말의 `conoti_reply.device`
pub const DESKTOP: &str = "desktop";
/// 데스크톱 확인 모드에서 PC 가 폰 말(폰이 건 예약 포함)을 거절했을 때의 `conoti_reply.note`
pub const DENIED_NOTE: &str = "데스크톱에서 거절";

pub fn flag(conn: &Connection, key: &str) -> bool {
    db::get_meta(conn, key).as_deref() == Some("1")
}

/// 세션에 넣을 글. 원래 요청 제목과 답을 **데이터로** 감싼다.
pub fn wrap_reply(title: &str, kind: &str, reply: &str) -> String {
    let what = if kind == "choice" { format!("선택: {reply}") } else { reply.to_string() };
    format!("{REPLY_HEADER}\n요청: {}\n\n{what}", text::clip(title, 80))
}

/// 데스크톱에서 보낸 말. 사용자가 친 그대로, 머리말만 붙인다.
pub fn wrap_desk(body: &str) -> String {
    format!("{INBOX_HEADER}\n\n{}", body.trim())
}

/// 예약에서 넣는 말. 사용자가 예약할 때 쓴 그대로, 예약 머리말만 붙인다.
pub fn wrap_sched(body: &str) -> String {
    format!("{SCHED_HEADER}\n\n{}", body.trim())
}

/// 데스크톱에서 보낸 말이면 머리말을 뗀 본문
pub fn strip_inbox(s: &str) -> Option<&str> {
    strip_header(s).map(|(rest, _)| rest)
}

/// 앱이 보낸 말(입력창 · 예약)이면 (머리말을 뗀 본문, 요청 출처 `inbox` | `sched`)
pub fn strip_header(s: &str) -> Option<(&str, &'static str)> {
    let (rest, origin) = match s.strip_prefix(INBOX_HEADER) {
        Some(r) => (r, "inbox"),
        None => (s.strip_prefix(SCHED_HEADER)?, "sched"),
    };
    Some((rest.trim_start_matches(['\n', '\r']), origin))
}

pub fn valid_quote(q: &str) -> bool {
    matches!(q, "prompt" | "response")
}

/// 답장 대상 한 줄(`text::quote_line`) — 그 요청의 본문(폰·데스크톱 머리말·앞 답장 줄·첨부 목록을 뗀 것) 또는 결과
pub fn quote_line_for(conn: &Connection, turn_id: i64, part: &str) -> Option<String> {
    let (seq, prompt, response): (i64, Option<String>, Option<String>) = conn
        .query_row(
            "SELECT seq, prompt_text, COALESCE(NULLIF(TRIM(response_text), ''), summary, understanding) FROM turn WHERE id = ?1",
            params![turn_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .ok()
        .flatten()?;
    let src = if part == "response" {
        response.unwrap_or_default()
    } else {
        let p = attach::split_block(&prompt.unwrap_or_default()).0;
        let p = crate::doc::phone_reply(Some(&p)).map(|x| x.1).unwrap_or(p);
        text::split_quote(&p).1.to_string()
    };
    Some(text::quote_line(seq, part, &src))
}

/// 답장이면 본문 앞에 답장 줄을 붙인다
fn with_quote(line: Option<String>, body: &str) -> String {
    match line {
        Some(l) => format!("{l}\n\n{body}"),
        None => body.to_string(),
    }
}

/// 보낸 말 본문 검사 — 이미지를 붙였으면 글은 비어도 된다
pub fn check_body(s: &str, has_images: bool) -> Result<(), String> {
    if has_images && s.trim().is_empty() {
        return Ok(());
    }
    check_reply_text(s)
}

/// 폰 답 본문 검사 — 길이·제어문자
pub fn check_reply_text(s: &str) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err("빈 답".into());
    }
    if s.chars().count() > MAX_REPLY_CHARS {
        return Err("답이 너무 깁니다".into());
    }
    if s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err("제어 문자가 들어 있습니다".into());
    }
    Ok(())
}

// ── 세션에 전달 ──────────────────────────────────────────────────────────────

pub struct Target {
    pub session_id: String,
    pub cwd: Option<String>,
}

/// 전달 방식은 바꿀 수 있게 떼어 둔다. `from_desktop`: PC 앞의 사용자가 보냈다(꺼진 세션 이어서 실행을 따로 묻지 않는다).
pub trait Deliver {
    fn deliver(&self, target: &Target, reply_id: &str, text: &str, from_desktop: bool) -> Outcome;

    /// 예약에서 온 말(`scheduled`)은 **꺼진 세션을 이어서 실행하지 않는다**(사용자 결정: 못 받으면 알림만) — 이 경로를 구현하는 쪽이 지킨다.
    /// 기본 구현은 옛 시그니처로 넘긴다(시험용 가짜들).
    fn deliver_opts(&self, target: &Target, reply_id: &str, text: &str, opts: Opts) -> Outcome {
        self.deliver(target, reply_id, text, opts.from_desktop && !opts.scheduled)
    }
}

#[derive(Clone, Copy)]
pub struct Opts {
    pub from_desktop: bool,
    pub scheduled: bool,
}

pub use crate::deliver::Outcome;

// ── 답을 받을 수 있는가 ──────────────────────────────────────────────────────

/// 이 기기가 이 세션에 지금 답을 넣을 수 있으면 빈 문자열, 아니면 사유 코드(폰이 번역한다).
pub fn reply_block(conn: &Connection, device_can_reply: bool, sid: &str) -> &'static str {
    if !device_can_reply {
        return "device_off";
    }
    if flag(conn, "conoti.paused") {
        return "paused";
    }
    let row: Option<(i64, i64, Option<String>)> = conn
        .query_row(
            "SELECT COALESCE((SELECT mode FROM conoti_session c WHERE c.session_id = s.id), 0), s.hidden, s.agent FROM session s WHERE s.id = ?1",
            params![sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .ok()
        .flatten();
    let Some((mode, hidden, agent)) = row else { return "no_session" };
    if agent.as_deref() == Some(crate::codex::AGENT) {
        crate::codex::remember(sid); // 전달 경로가 Codex 쪽 길을 고르게
    }
    if hidden != 0 {
        // 보관한 세션은 폰 답을 받지 않는다. 기록 관리를 허용하지 않은 폰이 보관 여부를 알아내지 못하게 "없는 세션"과 같게 답한다
        // (허용한 폰의 대화 머리에는 rpc::head 가 "archived" 를 따로 싣는다)
        return "no_session";
    }
    if mode == 1 {
        return "session_blocked";
    }
    match crate::deliver::route(sid) {
        Route::Channel | Route::Live | Route::Unarmed => "",
        Route::Terminal => "no_channel",
        // 대기 훅 없는 백그라운드 세션·꺼진 세션은 AI Inbox 가 claude 를 띄워 이어서 실행한다 — 사용자가 허락했을 때만
        Route::Idle | Route::Ended if !flag(conn, "conoti.bg_resume") => "offline_no_resume",
        _ => "",
    }
}

/// 폰이 건 예약이 발사된 뒤, 전달 전에 그 기기의 예약 허용이 꺼졌을 때
const SCHED_OFF_TEXT: &str = "PC 에서 이 기기의 예약 허용을 꺼 둠";

fn block_text(code: &str) -> &'static str {
    match code {
        "device_off" => "PC 에서 이 기기의 답 보내기를 꺼 둠",
        "paused" => "PC 에서 폰 답 받기를 멈춤",
        "session_blocked" => "PC 에서 이 세션의 폰 답을 막아 둠",
        "no_session" => "세션을 찾을 수 없음",
        "no_channel" => "실행 중인 세션에 넣는 훅이 설치되지 않음 — PC 설정에서 훅을 다시 설치하세요",
        "offline_no_resume" => "세션이 꺼져 있음 — 설정에서 '꺼진 세션 이어서 실행'을 켜면 자동으로 이어집니다",
        _ => "받을 수 없음",
    }
}

fn reply_out(conn: &Connection, rid: &str) -> Option<Value> {
    conn.query_row(
        "SELECT reply_id, text, state, note, received_at, quote, turn_id FROM conoti_reply WHERE reply_id = ?1",
        params![rid],
        |r| {
            Ok((
                json!({
                    "rid": r.get::<_, String>(0)?,
                    "text": r.get::<_, Option<String>>(1)?,
                    "state": r.get::<_, String>(2)?,
                    "note": r.get::<_, Option<String>>(3)?,
                    "at": r.get::<_, String>(4)?,
                }),
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<i64>>(6)?,
            ))
        },
    )
    .optional()
    .ok()
    .flatten()
    .map(|(mut v, quote, turn)| {
        v["atts"] = json!(attach::for_reply(conn, rid));
        // 답장이면 대상(번호·부분·발췌) — 폰이 앱을 다시 켜도 보낸 말 위에 인용을 그린다
        let q = match (turn, quote.as_deref()) {
            (Some(t), Some(part)) if valid_quote(part) => quote_line_for(conn, t, part),
            _ => None,
        };
        v["quote"] = match q.as_deref().map(text::split_quote) {
            Some((Some(q), _)) => json!({"seq": q.seq, "part": q.part, "text": q.text}),
            _ => Value::Null,
        };
        v
    })
}

/// 세션의 최근 폰 답(폰 화면의 상태 표시용). 요청으로 잡힌 답은 빠진다 — 그때부터는 대화의 요청 카드가 그 말이고,
/// 작업 중이면 "작업 중", 답이 오면 그 카드가 채워진다(보낸 말풍선과 카드가 겹쳐 보이지 않게, 09-28).
/// 예약에서 온 줄은 폰이 건 것·폰이 보내기로 넣은 것(`device`=그 기기)이어도 싣지 않는다 — 예약은 `sched_list` 로 보고, 폰 화면은 예전과 같다.
pub fn replies_for(conn: &Connection, sid: &str, limit: i64) -> Vec<Value> {
    let Ok(mut st) = conn.prepare(&format!(
        "SELECT r.reply_id FROM conoti_reply r WHERE r.session_id = ?1 AND r.device IS NOT NULL AND r.device <> 'desktop' AND NOT {FROM_SCHED}
            AND (r.result_turn IS NULL OR r.state = 'rejected') ORDER BY r.received_at DESC LIMIT ?2"
    )) else {
        return vec![];
    };
    let ids: Vec<String> = st.query_map(params![sid, limit], |r| r.get(0)).map(|r| r.flatten().collect()).unwrap_or_default();
    ids.iter().rev().filter_map(|id| reply_out(conn, id)).collect()
}

pub fn valid_rid(rid: &str) -> bool {
    (8..=64).contains(&rid.len()) && rid.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// 폰에서 온 답을 받아 기록한다. 같은 `rid` 는 처음 결과를 돌려준다(재전송 대비).
/// 실제 전달은 `Pipeline::tick` 이 한다 — 여기서는 검사하고 상태만 정한다.
#[allow(clippy::too_many_arguments)]
pub fn accept_reply(
    conn: &Connection,
    device: &str,
    device_can_reply: bool,
    sid: &str,
    turn_id: Option<i64>,
    body: &str,
    rid: &str,
    atts: &[String],
    quote: Option<&str>,
) -> Result<Value, String> {
    if !valid_rid(rid) {
        return Err("rid 형식 오류".into());
    }
    // 답장은 고른 요청이 있어야 한다 — 없는 요청에 단 답장을 마지막 요청으로 바꿔 넣지 않는다
    if quote.is_some_and(|q| !valid_quote(q)) || (quote.is_some() && turn_id.is_none()) {
        return Err("답장 대상 형식 오류".into());
    }
    if let Some(dev) = conn
        .query_row("SELECT COALESCE(device, '') FROM conoti_reply WHERE reply_id = ?1", params![rid], |r| r.get::<_, String>(0))
        .optional()
        .map_err(|e| e.to_string())?
    {
        if dev != device {
            return Err("다른 기기의 답 번호".into());
        }
        return reply_out(conn, rid).ok_or_else(|| "기록을 읽지 못함".into());
    }
    // 이미지는 이 기기가 같은 답 번호로 올린 것만
    attach::check_phone_ids(conn, device, rid, atts)?;
    // 답을 단 요청(없으면 세션의 마지막 요청)
    let turn: Option<i64> = match turn_id {
        Some(t) => conn
            .query_row("SELECT id FROM turn WHERE id = ?1 AND session_id = ?2", params![t, sid], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?,
        None => conn
            .query_row("SELECT id FROM turn WHERE session_id = ?1 AND hidden = 0 ORDER BY seq DESC LIMIT 1", params![sid], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?,
    };
    let block = reply_block(conn, device_can_reply, sid);
    let verdict: Result<(), String> = if !block.is_empty() {
        Err(block_text(block).to_string())
    } else if turn.is_none() {
        Err("요청을 찾을 수 없음".into())
    } else {
        check_body(body, !atts.is_empty())
    };
    let (state, note) = match &verdict {
        Ok(()) if flag(conn, "conoti.confirm") => ("confirm", None),
        Ok(()) => ("delivering", None),
        Err(why) => ("rejected", Some(why.clone())),
    };
    let now = time::now_iso();
    conn.execute(
        "INSERT INTO conoti_reply (reply_id, card_id, request_id, turn_id, session_id, kind, text, created_at, received_at, state, note, device, quote)
         VALUES (?1, NULL, NULL, ?2, ?3, 'text', ?4, ?5, ?5, ?6, ?7, ?8, ?9)",
        params![rid, turn, sid, text::clip(&text::redact(body), MAX_REPLY_CHARS + 10), now, state, note, device, quote],
    )
    .map_err(|e| e.to_string())?;
    if state == "rejected" {
        // 받지 않은 답의 이미지는 남기지 않는다(기기별 한도를 비우고 영구 보관되는 길이 되지 않게).
        // 폰이 올린 뒤에 같은 이미지(같은 해시)를 데스크톱 초안에 붙였으면 남긴다 — 올린 시각 기준
        let uploaded: Option<String> = conn
            .query_row("SELECT MIN(at) FROM att_upload WHERE device = ?1 AND rid = ?2", params![device, rid], |r| r.get(0))
            .ok()
            .flatten();
        let _ = conn.execute("DELETE FROM att_upload WHERE device = ?1 AND rid = ?2", params![device, rid]);
        let since = uploaded.unwrap_or_else(|| now.clone());
        for id in atts {
            attach::delete_if_orphan_since(conn, id, &since);
        }
    } else {
        attach::link(conn, rid, atts)?;
        record_tag_hint(conn, sid, body);
    }
    reply_out(conn, rid).ok_or_else(|| "기록을 읽지 못함".into())
}

/// 앱이 보낸 말(입력창·폰)은 세션에 채널·대기 훅으로 들어가 UserPromptSubmit 훅이 안 불릴 수 있다 — 보내는 시점에 태그 재료(`#태그`·폴더)를 직접 기록한다.
/// 대화 기록에서 이 요청을 읽으면 지문이 같을 때 이어진다(`tags::attach_for_turn`). 실패해도 전달을 막지 않는다.
pub(crate) fn record_tag_hint(conn: &Connection, sid: &str, body: &str) {
    let sent = text::clip(&text::redact(body), MAX_REPLY_CHARS + 10);
    if sent.trim().is_empty() {
        return;
    }
    let hp = crate::tags::HookPrompt::from_app(conn, sid, &sent);
    let _ = crate::tags::record_prompt(conn, &hp);
}

/// 데스크톱 입력창에서 보낸 말을 기록한다. 사용자가 PC 앞에 있으므로 폰용 겹(기기 허용·멈춤·세션 차단·확인)은
/// 거치지 않는다. 지금 넣을 수 없는 세션(터미널에서 채널 없이 실행 중)이면 기록하지 않고 이유를 돌려준다.
pub fn accept_desktop(conn: &Connection, sid: &str, body: &str, atts: &[String], quote: Option<(i64, &str)>) -> Result<Value, String> {
    check_body(body, !atts.is_empty())?;
    if quote.is_some_and(|(_, q)| !valid_quote(q)) {
        return Err("답장 대상 형식 오류".into());
    }
    attach::check_desktop_ids(conn, atts)?;
    // 보관한 세션도 PC 앞에서는 보낼 수 있다(보내면 목록으로 되돌린다) — 폰 경로는 보관 세션을 없는 것으로 본다
    let exists: bool = conn
        .query_row("SELECT COUNT(*) FROM session WHERE id = ?1", params![sid], |r| r.get::<_, i64>(0))
        .map(|n| n > 0)
        .map_err(|e| e.to_string())?;
    if !exists || !crate::channel::valid_session_id(sid) {
        return Err("세션을 찾을 수 없습니다".into());
    }
    if crate::deliver::route(sid) == Route::Terminal {
        return Err(crate::deliver::TERMINAL_TEXT.into());
    }
    let turn = reply_turn(conn, sid, quote)?;
    let rid = format!("dk{}", crate::relay::hex(&crate::relay::random::<8>()));
    let now = time::now_iso();
    conn.execute(
        "INSERT INTO conoti_reply (reply_id, turn_id, session_id, kind, text, created_at, received_at, state, device, quote)
         VALUES (?1, ?2, ?3, 'text', ?4, ?5, ?5, 'delivering', ?6, ?7)",
        params![rid, turn, sid, text::clip(&text::redact(body), MAX_REPLY_CHARS + 10), now, DESKTOP, quote.map(|q| q.1)],
    )
    .map_err(|e| e.to_string())?;
    attach::link(conn, &rid, atts)?;
    record_tag_hint(conn, sid, body);
    // 보관한 세션에 말을 보냈다 = 다시 쓰는 세션 — 목록으로 되돌린다
    conn.execute("UPDATE session SET hidden = 0, archived_at = NULL WHERE id = ?1 AND hidden = 1", params![sid])
        .map_err(|e| e.to_string())?;
    reply_out(conn, &rid).ok_or_else(|| "기록을 읽지 못함".into())
}

/// 보낸 말을 붙일 요청: 답장이면 그 요청(없으면 오류), 아니면 세션의 마지막 요청
fn reply_turn(conn: &Connection, sid: &str, quote: Option<(i64, &str)>) -> Result<Option<i64>, String> {
    match quote {
        Some((t, _)) => {
            let found = conn
                .query_row("SELECT id FROM turn WHERE id = ?1 AND session_id = ?2", params![t, sid], |r| r.get(0))
                .optional()
                .map_err(|e| e.to_string())?;
            Ok(Some(found.ok_or("답장할 요청을 찾을 수 없습니다")?))
        }
        None => conn
            .query_row("SELECT id FROM turn WHERE session_id = ?1 AND hidden = 0 ORDER BY seq DESC LIMIT 1", params![sid], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string()),
    }
}

/// 폰이 누른 예약 "보내기"(`sched_act send`) — 받지 못해 대기 중인 예약의 글을 **그 기기의 폰 답 줄**(`device`=pid, 머리말도 폰 답)로 넣는다.
/// 그래서 전달 직전의 기기 허용·전체 멈춤·세션 차단·데스크톱 확인 재검사와 기기 해제 때의 회수를 폰 답과 똑같이 탄다.
/// 확인 모드면 처음부터 확인 대기 — **상태까지 INSERT 한 번에**(넣은 뒤에 바꾸면 그 사이 전달 스레드가 다른 연결로 집어 갈 수 있다).
/// 누를 때의 검사(`reply_block`)·태그 기록은 부르는 쪽(`sched::act_by`)이 한다. 돌려주는 값 = 줄 번호(폰 rid 가 쓸 수 없는 `_` 를 넣는다).
pub fn accept_phone_send(conn: &Connection, device: &str, sid: &str, body: &str, atts: &[String], quote: Option<(i64, &str)>) -> Result<String, String> {
    check_body(body, !atts.is_empty())?;
    if quote.is_some_and(|(_, q)| !valid_quote(q)) {
        return Err("답장 대상 형식 오류".into());
    }
    // 예약에 묶여 이미 저장된 이미지(만들 때 이 기기·PC 의 것으로 검사했다)
    attach::check_desktop_ids(conn, atts)?;
    let turn = reply_turn(conn, sid, quote)?;
    let rid = format!("ps_{}", crate::relay::hex(&crate::relay::random::<8>()));
    let state = if flag(conn, "conoti.confirm") { "confirm" } else { "delivering" };
    let now = time::now_iso();
    conn.execute(
        "INSERT INTO conoti_reply (reply_id, turn_id, session_id, kind, text, created_at, received_at, state, device, quote)
         VALUES (?1, ?2, ?3, 'text', ?4, ?5, ?5, ?6, ?7, ?8)",
        params![rid, turn, sid, text::clip(&text::redact(body), MAX_REPLY_CHARS + 10), now, state, device, quote.map(|q| q.1)],
    )
    .map_err(|e| e.to_string())?;
    attach::link(conn, &rid, atts)?;
    Ok(rid)
}

/// 예약에서 온 줄인가 — 발사한 줄(`sched` 표식) 또는 대기 예약을 "보내기"로 넣은 줄(회차가 이 줄을 가리킨다)
const FROM_SCHED: &str = "(r.sched IS NOT NULL OR EXISTS (SELECT 1 FROM schedule_run x WHERE x.reply_id = r.reply_id))";

/// 새 작업을 띄운 기록 — 첫 요청이 끝나면 백그라운드 세션을 멈추도록 `link_results` 가 이어받는다.
pub fn record_started(conn: &Connection, sid: Option<&str>, body: &str, how: &str, atts: &[String]) -> Result<(), String> {
    let rid = format!("dk{}", crate::relay::hex(&crate::relay::random::<8>()));
    let now = time::now_iso();
    conn.execute(
        "INSERT INTO conoti_reply (reply_id, session_id, kind, text, created_at, received_at, state, note, delivered_at, device)
         VALUES (?1, ?2, 'text', ?3, ?4, ?4, 'delivered', ?5, ?4, ?6)",
        params![rid, sid, text::clip(&text::redact(body), MAX_REPLY_CHARS + 10), now, how, DESKTOP],
    )
    .map_err(|e| e.to_string())?;
    attach::link(conn, &rid, atts)?;
    Ok(())
}

/// 채팅 화면 아래에 보일 "보낸 말" — 아직 요청으로 잡히지 않은 것(대기·전달 중·전달됐지만 기록 전·거절)
pub fn outbox_for(conn: &Connection, sid: &str) -> Vec<Value> {
    let since = time::iso_from_ms(chrono::Utc::now().timestamp_millis() - 15 * 60_000);
    let Ok(mut st) = conn.prepare(&format!(
        "SELECT r.reply_id, r.text, r.state, r.note, r.received_at, r.device, r.quote, t.seq, {FROM_SCHED} FROM conoti_reply r
           LEFT JOIN turn t ON t.id = r.turn_id
          WHERE r.session_id = ?1 AND r.result_turn IS NULL
            AND (r.state IN ('confirm', 'delivering') OR (r.state IN ('delivered', 'rejected') AND r.received_at >= ?2))
          ORDER BY r.received_at LIMIT 20"
    )) else {
        return vec![];
    };
    st.query_map(params![sid, since], |r| {
        Ok(json!({
            "rid": r.get::<_, String>(0)?,
            "text": r.get::<_, Option<String>>(1)?,
            "state": r.get::<_, String>(2)?,
            "note": r.get::<_, Option<String>>(3)?,
            "at": r.get::<_, String>(4)?,
            "from_phone": r.get::<_, Option<String>>(5)?.as_deref() != Some(DESKTOP),
            "sched": r.get::<_, i64>(8)? != 0,
            // 답장이면 대상 요청 번호·부분(화면이 보낸 말 위에 인용으로)
            "quote": match (r.get::<_, Option<String>>(6)?, r.get::<_, Option<i64>>(7)?) {
                (Some(part), Some(seq)) => json!({"seq": seq, "part": part}),
                _ => Value::Null,
            },
        }))
    })
    .map(|rows| {
        rows.flatten()
            .map(|mut v: Value| {
                let rid = v["rid"].as_str().unwrap_or("").to_string();
                v["atts"] = json!(attach::metas(conn, &attach::for_reply(conn, &rid)));
                v
            })
            .collect()
    })
    .unwrap_or_default()
}

/// 데스크톱 확인 모드에서 PC 가 폰 말(폰이 건 예약·폰이 누른 예약 보내기 포함)을 전달하거나 거절한다.
/// 허용 = `acked='approved'`(전달 직전에 "확인 모드인데 확인을 거쳤나"를 다시 본다) + `wait_from`=지금(못 넣은 시간은 허용한 때부터 센다).
/// 거절 문구는 상수 — 예약 회차는 이 문구를 보고 끝낸다(`sched::sync_fired`).
pub fn decide(conn: &Connection, reply_id: &str, approve: bool) -> Result<(), String> {
    let (next, note) = if approve { ("delivering", None) } else { ("rejected", Some(DENIED_NOTE)) };
    let n = conn
        .execute(
            "UPDATE conoti_reply SET state = ?2, note = COALESCE(?3, note), acked = CASE WHEN ?2 = 'delivering' THEN 'approved' ELSE acked END,
                    wait_from = CASE WHEN ?2 = 'delivering' THEN ?4 ELSE wait_from END
              WHERE reply_id = ?1 AND state = 'confirm'",
            params![reply_id, next, note, time::now_iso()],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("이미 처리된 답입니다".into());
    }
    Ok(())
}

/// 보낸 말 지우기(대기·거절된 것만) — 사용자가 마음을 바꿨을 때. 예약에서 온 줄은 폰이 건 것·폰이 보내기로 넣은 것도 PC 에서 거둘 수 있다
/// (그 줄들이 그 기기의 줄로 들어가게 된 뒤에도 PC 화면의 "취소"가 예전처럼 듣게)
pub fn cancel_desktop(conn: &Connection, rid: &str) -> Result<(), String> {
    let mine = format!("r.reply_id = ?1 AND (r.device = ?2 OR {FROM_SCHED})");
    let n = conn
        .execute(&format!("UPDATE conoti_reply AS r SET state = 'rejected', note = '보내기 취소' WHERE {mine} AND r.state = 'delivering'"), params![rid, DESKTOP])
        .map_err(|e| e.to_string())?;
    if n == 0 {
        let gone = conn
            .execute(&format!("DELETE FROM conoti_reply AS r WHERE {mine} AND r.state = 'rejected'"), params![rid, DESKTOP])
            .map_err(|e| e.to_string())?;
        // 이미지 연결만 푼다 — 파일은 "다시 쓰기"로 다시 붙일 수 있게 두고, 7일 안에 안 쓰면 정리된다(attach::gc)
        if gone > 0 {
            attach::touch(conn, &attach::for_reply(conn, rid));
            conn.execute("DELETE FROM reply_attachment WHERE reply_id = ?1", params![rid]).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

// ── 전달 ─────────────────────────────────────────────────────────────────────

pub struct Pipeline {
    conn: Connection,
    /// 방금 claude 를 띄운 세션 — 등록부에 올라오기 전에 한 번 더 띄우지 않게
    launched: std::collections::HashMap<String, std::time::Instant>,
    /// 보내지 않은 첨부 이미지 정리(10분마다)
    last_gc: Option<std::time::Instant>,
}

#[derive(Default)]
pub struct PipelineReport {
    pub changed_sessions: Vec<String>,
    pub needs_confirm: usize,
}

impl Pipeline {
    pub fn new(conn: Connection) -> Pipeline {
        Pipeline { conn, launched: Default::default(), last_gc: None }
    }

    #[cfg(test)]
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    /// 예약 전송 틱(`sched.rs`) — 시각이 된 예약을 대기열에 넣고 결과를 따라간다. 파이프라인 틱 앞에 부른다(넣은 줄을 같은 틱에 전달)
    pub fn tick_schedule(&mut self, now: chrono::DateTime<chrono::Utc>, probe: &dyn crate::sched::Probe) -> crate::sched::TickReport {
        crate::sched::tick(&self.conn, now, probe)
    }

    pub fn tick(&mut self, deliverer: &dyn Deliver) -> PipelineReport {
        let mut rep = PipelineReport::default();
        if self.last_gc.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(600)) {
            attach::gc(&self.conn);
            self.last_gc = Some(std::time::Instant::now());
        }
        self.deliver_ready(deliverer, &mut rep);
        self.link_results(&mut rep);
        rep.needs_confirm = self
            .conn
            .query_row("SELECT COUNT(*) FROM conoti_reply WHERE state = 'confirm'", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as usize;
        rep
    }

    fn deliver_ready(&mut self, deliverer: &dyn Deliver, rep: &mut PipelineReport) {
        // 오래 묵은 말은 넣지 않는다 — 상황이 바뀐 뒤 늦게 도착한 명령이 실행되지 않게.
        // 폰 · 데스크톱 모두 앞 작업이 끝나길 기다리는 대기열이라 받은 뒤 3시간까지는 기다린다.
        // 폰 말은 여기에 더해, 세션이 받을 수 있는데도 못 넣은 시간이 10분을 넘으면 넣지 않는다 —
        // 세션이 일하는 동안(Outcome::Busy)은 wait_from 이 밀려 이 시계가 멈춘다(09-25 실사고: 18분짜리 작업 뒤에 줄 선 폰 말이 버려졌다)
        // 예약 줄(`sched`)은 폰이 건 것이어도 10분 규칙 대신 예약 쪽 정체 규칙(sched::sync_fired — 10분 못 받으면 held)을 따른다
        let now_ms = chrono::Utc::now().timestamp_millis();
        let stale_phone = time::iso_from_ms(now_ms - 10 * 60_000);
        let stale_all = time::iso_from_ms(now_ms - 3 * 3_600_000);
        const STALE: &str =
            "state = 'delivering' AND (received_at < ?2 OR (COALESCE(device, '') <> 'desktop' AND sched IS NULL AND COALESCE(wait_from, received_at) < ?1))";
        let stale_sids: Vec<String> = self
            .conn
            .prepare(&format!("SELECT DISTINCT session_id FROM conoti_reply WHERE {STALE}"))
            .and_then(|mut st| st.query_map(params![stale_phone, stale_all], |r| r.get(0)).map(|r| r.flatten().collect()))
            .unwrap_or_default();
        if !stale_sids.is_empty() {
            let _ = self.conn.execute(
                &format!("UPDATE conoti_reply SET state = 'rejected', note = '오래 기다려도 전달하지 못해 넣지 않음' WHERE {STALE}"),
                params![stale_phone, stale_all],
            );
            rep.changed_sessions.extend(stale_sids);
        }
        self.launched.retain(|_, at| at.elapsed() < std::time::Duration::from_secs(20));
        let confirm_on = flag(&self.conn, "conoti.confirm");
        type Row = (String, String, String, String, Option<i64>, Option<String>, Option<String>, Option<String>, Option<String>);
        let rows: Vec<Row> = {
            let Ok(mut st) = self.conn.prepare(
                "SELECT r.reply_id, r.session_id, r.kind, r.text, r.turn_id, r.device, r.acked, r.note, r.sched FROM conoti_reply r
                  WHERE r.state = 'delivering' ORDER BY r.received_at LIMIT 20",
            ) else {
                return;
            };
            let Ok(rows) = st.query_map([], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?, x.get(4)?, x.get(5)?, x.get(6)?, x.get(7)?, x.get(8)?)))
            else {
                return;
            };
            rows.flatten().collect()
        };
        // 한 세션에는 한 번에 하나씩, 앞의 것이 들어가 요청이 시작된 뒤에 다음 것
        let mut busy: std::collections::HashSet<String> = self.launched.keys().cloned().collect();
        for (reply_id, sid, kind, reply_text, turn_id, device, acked, old_note, sched) in rows {
            let desk = device.as_deref() == Some(DESKTOP);
            if busy.contains(&sid) {
                // 같은 세션의 앞 말 뒤에 줄 서 있다 — 앞 말이 들어가거나 세션이 일하는 동안 이 말의 시계도 멈춘다
                if !desk {
                    let _ = self.conn.execute("UPDATE conoti_reply SET wait_from = ?2 WHERE reply_id = ?1", params![reply_id, time::now_iso()]);
                }
                continue;
            }
            if desk {
                let exists: bool = self
                    .conn
                    .query_row("SELECT COUNT(*) FROM session WHERE id = ?1", params![sid], |r| r.get::<_, i64>(0))
                    .map(|n| n > 0)
                    .unwrap_or(false);
                if !exists {
                    let _ = self.conn.execute("UPDATE conoti_reply SET state = 'rejected', note = '세션을 찾을 수 없음' WHERE reply_id = ?1", params![reply_id]);
                    rep.changed_sessions.push(sid);
                    continue;
                }
            } else {
                // 확인 모드를 그사이 켰다면, 데스크톱에서 허용하지 않은 답은 확인 대기로 되돌린다
                if confirm_on && acked.as_deref() != Some("approved") {
                    let _ = self.conn.execute("UPDATE conoti_reply SET state = 'confirm' WHERE reply_id = ?1", params![reply_id]);
                    rep.changed_sessions.push(sid);
                    continue;
                }
                // 전달 직전에 한 번 더: 기기 허용·세션 차단·전체 멈춤·전달 경로 (폰이 건 예약이면 그 기기의 예약 허용까지)
                let (can_reply, can_schedule): (bool, bool) = device
                    .as_deref()
                    .and_then(|d| {
                        self.conn
                            .query_row("SELECT can_reply, can_schedule FROM relay_device WHERE pid = ?1", params![d], |r| {
                                Ok((r.get::<_, i64>(0)? != 0, r.get::<_, i64>(1)? != 0))
                            })
                            .optional()
                            .ok()
                            .flatten()
                    })
                    .unwrap_or((false, false));
                // 예약에서 온 줄(폰이 건 예약의 발사 · 폰이 누른 대기 예약 보내기)은 그 기기의 예약 허용도 본다
                let from_sched = sched.is_some()
                    || self
                        .conn
                        .query_row("SELECT EXISTS (SELECT 1 FROM schedule_run WHERE reply_id = ?1)", params![reply_id], |r| r.get::<_, bool>(0))
                        .unwrap_or(false);
                let note = match reply_block(&self.conn, can_reply, &sid) {
                    "" if from_sched && !can_schedule => Some(SCHED_OFF_TEXT),
                    "" => None,
                    block => Some(block_text(block)),
                };
                if let Some(note) = note {
                    let _ = self.conn.execute("UPDATE conoti_reply SET state = 'rejected', note = ?2 WHERE reply_id = ?1", params![reply_id, note]);
                    rep.changed_sessions.push(sid);
                    continue;
                }
            }
            let cwd: Option<String> = self
                .conn
                .query_row("SELECT project_dir FROM session WHERE id = ?1", params![sid], |x| x.get(0))
                .optional()
                .ok()
                .flatten()
                .flatten();
            let quote: Option<String> = self
                .conn
                .query_row("SELECT quote FROM conoti_reply WHERE reply_id = ?1", params![reply_id], |x| x.get(0))
                .optional()
                .ok()
                .flatten()
                .flatten();
            let quote_line = match (turn_id, quote.as_deref()) {
                (Some(t), Some(q)) if valid_quote(q) => quote_line_for(&self.conn, t, q),
                _ => None,
            };
            let reply_text = with_quote(quote_line, &reply_text);
            let wrapped = if sched.is_some() {
                wrap_sched(&reply_text)
            } else if desk {
                wrap_desk(&reply_text)
            } else {
                let title: String = turn_id
                    .and_then(|t| {
                        self.conn
                            .query_row("SELECT COALESCE(prompt_text, '') FROM turn WHERE id = ?1", params![t], |x| x.get(0))
                            .optional()
                            .ok()
                            .flatten()
                    })
                    .unwrap_or_default();
                // 앞 요청이 이미지를 붙인 말이면 경로 목록을 떼고 본다
                let title = attach::split_block(&title).0;
                let title = crate::doc::phone_reply(Some(&title)).map(|p| p.1).filter(|b| !b.trim().is_empty()).unwrap_or(title);
                wrap_reply(text::first_line(&title), &kind, &reply_text)
            };
            let block = attach::block(&self.conn, &attach::for_reply(&self.conn, &reply_id));
            if reply_text.trim().is_empty() && block.is_none() {
                // 이미지만 붙인 말인데 그 이미지를 지웠다 — 빈 말은 넣지 않는다
                let _ = self.conn.execute(
                    "UPDATE conoti_reply SET state = 'rejected', note = '보낼 내용이 없음(붙인 이미지가 지워짐)' WHERE reply_id = ?1",
                    params![reply_id],
                );
                rep.changed_sessions.push(sid);
                continue;
            }
            let wrapped = attach::append_block(&wrapped, block);
            // 앞 말을 전달하는 동안(백그라운드 세션 멈추기 등 수십 초) 지우거나 취소했을 수 있다 — 넣기 직전에 한 번 더
            let still: bool = self
                .conn
                .query_row("SELECT state = 'delivering' FROM conoti_reply WHERE reply_id = ?1", params![reply_id], |r| r.get(0))
                .unwrap_or(false);
            if !still {
                continue;
            }
            let target = Target { session_id: sid.clone(), cwd };
            busy.insert(sid.clone());
            match deliverer.deliver_opts(&target, &reply_id, &wrapped, Opts { from_desktop: desk, scheduled: sched.is_some() }) {
                Outcome::Done(how) => {
                    let _ = self.conn.execute(
                        "UPDATE conoti_reply SET state = 'delivered', delivered_at = ?2, note = ?3 WHERE reply_id = ?1",
                        params![reply_id, time::now_iso(), how],
                    );
                    self.launched.insert(sid.clone(), std::time::Instant::now());
                }
                Outcome::Wait(why) => {
                    if old_note.as_deref() == Some(why.as_str()) {
                        continue;
                    }
                    let _ = self.conn.execute("UPDATE conoti_reply SET note = ?2 WHERE reply_id = ?1", params![reply_id, why]);
                }
                Outcome::Busy(why) => {
                    // 세션이 앞 작업을 하는 중 — 폰 말의 10분 시계를 멈춘다(받은 뒤 3시간 상한은 그대로)
                    if !desk {
                        let _ = self.conn.execute("UPDATE conoti_reply SET wait_from = ?2 WHERE reply_id = ?1", params![reply_id, time::now_iso()]);
                    }
                    if old_note.as_deref() == Some(why.as_str()) {
                        continue;
                    }
                    let _ = self.conn.execute("UPDATE conoti_reply SET note = ?2 WHERE reply_id = ?1", params![reply_id, why]);
                }
                Outcome::Fail(why) => {
                    let _ = self.conn.execute("UPDATE conoti_reply SET state = 'rejected', note = ?2 WHERE reply_id = ?1", params![reply_id, why]);
                }
            }
            rep.changed_sessions.push(sid);
        }
    }

    /// 전달한 답으로 시작된 요청을 대화 기록에서 찾고, 끝나면 handled 로 표시한다.
    fn link_results(&mut self, rep: &mut PipelineReport) {
        let rows: Vec<(String, String, String, Option<i64>)> = {
            let Ok(mut st) = self.conn.prepare(
                "SELECT reply_id, session_id, delivered_at, result_turn FROM conoti_reply
                  WHERE state = 'delivered' AND delivered_at >= ?1",
            ) else {
                return;
            };
            let since = time::iso_from_ms(chrono::Utc::now().timestamp_millis() - 3 * 86_400_000);
            let Ok(rows) = st.query_map(params![since], |x| Ok((x.get(0)?, x.get(1)?, x.get(2)?, x.get(3)?))) else { return };
            rows.flatten().collect()
        };
        for (reply_id, sid, delivered_at, result_turn) in rows {
            let turn = match result_turn {
                Some(t) => Some(t),
                None => {
                    let from = time::parse(&delivered_at)
                        .map(|d| (d - chrono::Duration::seconds(5)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
                        .unwrap_or(delivered_at.clone());
                    let found: Option<i64> = self
                        .conn
                        .query_row(
                            "SELECT id FROM turn WHERE prompt_at >= ?2 AND (prompt_text LIKE ?3 OR origin IN ('inbox', 'sched'))
                               AND (session_id = ?1 OR session_id IN (SELECT id FROM session WHERE first_at >= ?2))
                               AND id NOT IN (SELECT result_turn FROM conoti_reply WHERE result_turn IS NOT NULL)
                             ORDER BY prompt_at LIMIT 1",
                            params![sid, from, format!("{REPLY_HEADER}%")],
                            |x| x.get(0),
                        )
                        .optional()
                        .ok()
                        .flatten();
                    if let Some(t) = found {
                        let _ = self.conn.execute("UPDATE conoti_reply SET result_turn = ?2 WHERE reply_id = ?1", params![reply_id, t]);
                        rep.changed_sessions.push(sid.clone());
                    }
                    found
                }
            };
            let Some(t) = turn else { continue };
            let done: Option<(String, String)> = self
                .conn
                .query_row("SELECT status, session_id FROM turn WHERE id = ?1", params![t], |x| Ok((x.get(0)?, x.get(1)?)))
                .optional()
                .ok()
                .flatten();
            if let Some((status, tsid)) = done {
                if crate::doc::is_finished(&status) {
                    let _ = self.conn.execute("UPDATE conoti_reply SET state = 'handled' WHERE reply_id = ?1", params![reply_id]);
                    // 백그라운드로 이어서 띄운 세션이면 멈춘다(계속 살아 있으면 다음 답이 막힌다)
                    let bg_key = format!("conoti.bg.{tsid}");
                    if let Some(short) = db::get_meta(&self.conn, &bg_key) {
                        crate::deliver::stop_background(&short);
                        let _ = self.conn.execute("DELETE FROM meta WHERE key = ?1", params![bg_key]);
                    }
                    rep.changed_sessions.push(tsid);
                }
            }
        }
    }
}

/// v3(평문 카드) 시절의 설정값을 지운다. 옛 연결 키(키체인 `com.yeojeonghun.ai-inbox.conoti`)는 건드리지 않는다 —
/// 앱이 다시 서명되면 옛 항목에 손대는 순간 macOS 허용 창이 떠서 이 스레드가 멈춘다. 쓰이지 않는 항목일 뿐이다.
pub fn cleanup_legacy(conn: &Connection) {
    if db::get_meta(conn, "conoti.endpoint").is_none() && db::get_meta(conn, "conoti.connected").is_none() {
        return;
    }
    let _ = conn.execute(
        "DELETE FROM meta WHERE key IN ('conoti.endpoint', 'conoti.connected', 'conoti.connected_at', 'conoti.since',
             'conoti.key_hint', 'conoti.key_version', 'conoti.last_error', 'conoti.last_poll_at', 'conoti.body',
             'conoti.body_blocked')",
        [],
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        db::migrate(&c).unwrap();
        c.execute(
            "INSERT INTO session (id, project_dir, first_at, last_at) VALUES ('s1', '/tmp', '2026-09-24T00:00:00Z', '2026-09-24T00:00:00Z')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, status) VALUES ('s1', 'u1', 1, '2026-09-24T00:00:00Z', '캐시를 지울까요?', 'done')",
            [],
        )
        .unwrap();
        c
    }

    #[test]
    fn reply_checks() {
        assert!(check_reply_text("계속 진행해").is_ok());
        assert!(check_reply_text("  ").is_err());
        assert!(check_reply_text("a\u{1b}[31m").is_err());
        assert!(check_reply_text(&"가".repeat(4001)).is_err());
        let w = wrap_reply("캐시를 지울까요?", "choice", "1. 지워");
        assert!(w.starts_with(REPLY_HEADER));
        assert!(w.contains("선택: 1. 지워"));
    }

    #[test]
    fn layered_blocks() {
        let c = mem();
        // 세션이 꺼져 있고 이어서 실행이 꺼져 있으면 받지 않는다
        assert_eq!(reply_block(&c, true, "s1"), "offline_no_resume");
        db::set_meta(&c, "conoti.bg_resume", "1").unwrap();
        assert_eq!(reply_block(&c, true, "s1"), "");
        assert_eq!(reply_block(&c, false, "s1"), "device_off");
        c.execute("INSERT INTO conoti_session (session_id, mode) VALUES ('s1', 1)", []).unwrap();
        assert_eq!(reply_block(&c, true, "s1"), "session_blocked");
        c.execute("UPDATE conoti_session SET mode = 0", []).unwrap();
        db::set_meta(&c, "conoti.paused", "1").unwrap();
        assert_eq!(reply_block(&c, true, "s1"), "paused");
        assert_eq!(reply_block(&c, true, "nope"), "paused");
        db::set_meta(&c, "conoti.paused", "0").unwrap();
        assert_eq!(reply_block(&c, true, "nope"), "no_session");
    }

    struct Fake;
    impl Deliver for Fake {
        fn deliver(&self, t: &Target, _id: &str, text: &str, from_desktop: bool) -> Outcome {
            if from_desktop {
                assert_eq!(t.session_id, DESK_SID);
                assert_eq!(text, format!("{INBOX_HEADER}\n\n이어서 테스트도 돌려 줘"));
            } else {
                assert_eq!(t.session_id, "s1");
                assert!(text.starts_with(REPLY_HEADER) && text.contains("요청: 캐시를 지울까요?") && text.ends_with("지워 줘"));
            }
            Outcome::Done("시험 전달".into())
        }
    }

    /// 앞 작업이 끝나길 기다리는 세션
    struct Busy;
    impl Deliver for Busy {
        fn deliver(&self, _t: &Target, _id: &str, _text: &str, _d: bool) -> Outcome {
            Outcome::Busy("앞 작업이 끝나면 보냅니다".into())
        }
    }

    /// 세션은 쉬는데 아직 못 넣는 중(연결을 잇는 중 등)
    struct Stuck;
    impl Deliver for Stuck {
        fn deliver(&self, _t: &Target, _id: &str, _text: &str, _d: bool) -> Outcome {
            Outcome::Wait("세션 연결을 잇는 중".into())
        }
    }

    /// 넣은 글을 모아 둔다
    struct Capture(std::sync::Mutex<Vec<String>>);
    impl Deliver for Capture {
        fn deliver(&self, _t: &Target, _id: &str, text: &str, _d: bool) -> Outcome {
            self.0.lock().unwrap().push(text.to_string());
            Outcome::Done("시험 전달".into())
        }
    }

    #[test]
    fn reply_to_a_chosen_turn_carries_a_quote_line() {
        // 한 세션에 요청이 여럿 — 폰에서 앞 요청의 결과를 골라 답장한다
        let c = mem();
        db::set_meta(&c, "conoti.bg_resume", "1").unwrap();
        c.execute("UPDATE turn SET response_text = '**캐시 12개**를 찾았어요.\n지울까요?' WHERE seq = 1", []).unwrap();
        c.execute(
            "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, status, response_text)
             VALUES ('s1', 'u2', 2, '2026-09-24T00:05:00Z', '로그도 봐 줘', 'running', NULL)",
            [],
        )
        .unwrap();
        c.execute("INSERT INTO relay_device (pid, name, phone_pub, can_reply, created_at) VALUES ('d1', '폰', 'ab', 1, 'x')", []).unwrap();
        let first: i64 = c.query_row("SELECT id FROM turn WHERE seq = 1", [], |r| r.get(0)).unwrap();
        // 형식이 틀리거나 대상 없는 답장은 받지 않는다
        assert!(accept_reply(&c, "d1", true, "s1", Some(first), "x", "rid-00000031", &[], Some("title")).is_err());
        assert!(accept_reply(&c, "d1", true, "s1", None, "x", "rid-00000032", &[], Some("response")).is_err());
        accept_reply(&c, "d1", true, "s1", Some(first), "그거 지워 줘", "rid-00000033", &[], Some("response")).unwrap();
        let mut p = Pipeline::new(c);
        let cap = Capture(Default::default());
        p.tick(&cap);
        let sent = cap.0.lock().unwrap().clone();
        assert_eq!(
            sent,
            vec![format!("{REPLY_HEADER}\n요청: 캐시를 지울까요?\n\n답장: #1 결과 「캐시 12개를 찾았어요. 지울까요?」\n\n그거 지워 줘")]
        );
        // 화면이 쓰는 폰 답 풀이 → 답장 줄 풀이
        let (_, body) = crate::doc::phone_reply(Some(&sent[0])).unwrap();
        let (q, rest) = text::split_quote(&body);
        assert_eq!((q.map(|q| (q.seq, q.part)), rest), (Some((1, "response")), "그거 지워 줘"));
    }

    #[test]
    fn desktop_reply_quotes_a_request() {
        let c = mem();
        c.execute(
            "INSERT INTO session (id, project_dir, first_at, last_at) VALUES (?1, '/tmp', '2026-09-24T00:00:00Z', '2026-09-24T00:00:00Z')",
            params![DESK_SID],
        )
        .unwrap();
        c.execute(
            "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, status) VALUES (?1, 'd1', 4, '2026-09-24T00:00:00Z', ?2, 'done')",
            params![DESK_SID, format!("{}\n\n그 전 요청", text::quote_line(2, "prompt", "더 앞 요청"))],
        )
        .unwrap();
        let t: i64 = c.query_row("SELECT id FROM turn WHERE prompt_uuid = 'd1'", [], |r| r.get(0)).unwrap();
        // 다른 세션의 요청에는 답장할 수 없다
        let other: i64 = c.query_row("SELECT id FROM turn WHERE session_id = 's1'", [], |r| r.get(0)).unwrap();
        assert!(accept_desktop(&c, DESK_SID, "이것도", &[], Some((other, "prompt"))).is_err());
        accept_desktop(&c, DESK_SID, "이것도", &[], Some((t, "prompt"))).unwrap();
        let mut p = Pipeline::new(c);
        let cap = Capture(Default::default());
        p.tick(&cap);
        // 앞 요청의 답장 줄은 발췌에 넣지 않는다
        assert_eq!(cap.0.lock().unwrap().clone(), vec![format!("{INBOX_HEADER}\n\n답장: #4 요청 「그 전 요청」\n\n이것도")]);
    }

    #[test]
    fn phone_reply_waits_through_a_long_job() {
        // 09-25 실사고: 18분 걸린 작업 뒤에 폰에서 이어서 시킨 말이 "오래 기다려도 전달하지 못해"로 버려졌다
        let c = mem();
        db::set_meta(&c, "conoti.bg_resume", "1").unwrap();
        c.execute("INSERT INTO relay_device (pid, name, phone_pub, can_reply, created_at) VALUES ('d1', '폰', 'ab', 1, 'x')", []).unwrap();
        accept_reply(&c, "d1", true, "s1", None, "지워 줘", "rid-00000011", &[], None).unwrap();
        accept_reply(&c, "d1", true, "s1", None, "그다음 이것도", "rid-00000012", &[], None).unwrap();
        let mut p = Pipeline::new(c);
        p.tick(&Busy);
        // 받은 지 30분 — 그동안 세션은 계속 일했다
        let ago = |min: i64| time::iso_from_ms(chrono::Utc::now().timestamp_millis() - min * 60_000);
        p.conn.execute("UPDATE conoti_reply SET received_at = ?1", params![ago(30)]).unwrap();
        p.tick(&Busy);
        let st = |p: &Pipeline, rid: &str| -> String {
            p.conn.query_row("SELECT state FROM conoti_reply WHERE reply_id = ?1", params![rid], |r| r.get(0)).unwrap()
        };
        assert_eq!(st(&p, "rid-00000011"), "delivering", "일하는 동안 기다린 시간은 10분에 넣지 않는다");
        assert_eq!(st(&p, "rid-00000012"), "delivering", "앞 말 뒤에 줄 선 말도 같이 기다린다");
        // 작업이 끝나면 들어간다
        p.tick(&Fake);
        assert_eq!(st(&p, "rid-00000011"), "delivered");
    }

    #[test]
    fn phone_reply_expires_when_session_is_free_but_unreachable_or_after_three_hours() {
        let c = mem();
        db::set_meta(&c, "conoti.bg_resume", "1").unwrap();
        c.execute("INSERT INTO relay_device (pid, name, phone_pub, can_reply, created_at) VALUES ('d1', '폰', 'ab', 1, 'x')", []).unwrap();
        accept_reply(&c, "d1", true, "s1", None, "지워 줘", "rid-00000013", &[], None).unwrap();
        let ago = |min: i64| time::iso_from_ms(chrono::Utc::now().timestamp_millis() - min * 60_000);
        let mut p = Pipeline::new(c);
        // 세션이 일을 마친 지 11분, 그 뒤로 계속 못 넣었다
        p.conn.execute("UPDATE conoti_reply SET received_at = ?1, wait_from = ?2", params![ago(40), ago(11)]).unwrap();
        p.tick(&Stuck);
        let st: String = p.conn.query_row("SELECT state FROM conoti_reply", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "rejected");
        // 세션이 계속 일해도 받은 뒤 3시간이 넘으면 넣지 않는다
        p.conn.execute("UPDATE conoti_reply SET state = 'delivering', received_at = ?1, wait_from = ?2", params![ago(181), ago(0)]).unwrap();
        p.tick(&Busy);
        let st: String = p.conn.query_row("SELECT state FROM conoti_reply", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "rejected");
    }

    /// 입력창은 실제 세션 ID 형식만 받는다(인자로 claude 에 넘긴다)
    const DESK_SID: &str = "0f0e0d0c-0000-4000-8000-000000000001";

    fn mem_desk() -> Connection {
        let c = mem();
        c.execute(
            "INSERT INTO session (id, project_dir, first_at, last_at) VALUES (?1, '/tmp', '2026-09-24T00:00:00Z', '2026-09-24T00:00:00Z')",
            params![DESK_SID],
        )
        .unwrap();
        c
    }

    #[test]
    fn desktop_message_skips_phone_layers_and_queues() {
        let c = mem_desk();
        // 폰 겹(멈춤·확인·이어서 실행 꺼짐)이 켜져 있어도 데스크톱에서 보낸 말은 들어간다
        db::set_meta(&c, "conoti.paused", "1").unwrap();
        db::set_meta(&c, "conoti.confirm", "1").unwrap();
        assert!(accept_desktop(&c, DESK_SID, "  ", &[], None).is_err());
        assert!(accept_desktop(&c, "0f0e0d0c-0000-4000-8000-00000000ffff", "해 줘", &[], None).is_err());
        assert!(accept_desktop(&c, "s1", "형식이 틀린 세션 ID", &[], None).is_err());
        let r = accept_desktop(&c, DESK_SID, "이어서 테스트도 돌려 줘", &[], None).unwrap();
        assert_eq!(r["state"], "delivering");
        let rid = r["rid"].as_str().unwrap().to_string();
        let mut p = Pipeline::new(c);
        // 세션이 일하는 중이면 기다린다(상태 그대로, 사유만)
        p.tick(&Busy);
        let (st, note): (String, Option<String>) =
            p.conn.query_row("SELECT state, note FROM conoti_reply WHERE reply_id = ?1", params![rid], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((st.as_str(), note.as_deref()), ("delivering", Some("앞 작업이 끝나면 보냅니다")));
        assert_eq!(outbox_for(&p.conn, DESK_SID).len(), 1);
        p.tick(&Fake);
        let st: String = p.conn.query_row("SELECT state FROM conoti_reply WHERE reply_id = ?1", params![rid], |r| r.get(0)).unwrap();
        assert_eq!(st, "delivered");
        // 폰 화면의 "내 답" 목록에는 데스크톱에서 보낸 말이 섞이지 않는다
        assert!(replies_for(&p.conn, DESK_SID, 10).is_empty());
        // 요청으로 잡히면(origin = inbox) 보낸 말 목록에서 빠진다
        p.conn
            .execute(
                "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, status, origin) VALUES (?2, 'u2', 2, ?1, '이어서 테스트도 돌려 줘', 'done', 'inbox')",
                params![time::now_iso(), DESK_SID],
            )
            .unwrap();
        p.tick(&Fake);
        assert!(outbox_for(&p.conn, DESK_SID).is_empty());
        let st: String = p.conn.query_row("SELECT state FROM conoti_reply WHERE reply_id = ?1", params![rid], |r| r.get(0)).unwrap();
        assert_eq!(st, "handled");
    }

    #[test]
    fn one_message_per_session_per_tick() {
        let c = mem_desk();
        accept_desktop(&c, DESK_SID, "이어서 테스트도 돌려 줘", &[], None).unwrap();
        accept_desktop(&c, DESK_SID, "이어서 테스트도 돌려 줘", &[], None).unwrap();
        let mut p = Pipeline::new(c);
        p.tick(&Fake);
        let n: i64 = p.conn.query_row("SELECT COUNT(*) FROM conoti_reply WHERE state = 'delivered'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "두 번째 말은 첫 말이 요청으로 잡힌 뒤에");
    }

    #[test]
    fn inbox_header_round_trip() {
        let w = wrap_desk("  고쳐 줘\n");
        assert_eq!(strip_inbox(&w), Some("고쳐 줘"));
        assert_eq!(strip_inbox("그냥 말"), None);
    }

    #[test]
    fn accept_is_idempotent_and_delivers() {
        let c = mem();
        db::set_meta(&c, "conoti.bg_resume", "1").unwrap();
        c.execute("INSERT INTO relay_device (pid, name, phone_pub, can_reply, created_at) VALUES ('d1', '폰', 'ab', 1, 'x')", []).unwrap();
        let r = accept_reply(&c, "d1", true, "s1", None, "지워 줘", "rid-00000001", &[], None).unwrap();
        assert_eq!(r["state"], "delivering");
        // 같은 rid 다시 → 같은 결과, 다른 기기가 같은 rid → 거절
        assert_eq!(accept_reply(&c, "d1", true, "s1", None, "딴말", "rid-00000001", &[], None).unwrap()["text"], "지워 줘");
        assert!(accept_reply(&c, "d2", true, "s1", None, "딴말", "rid-00000001", &[], None).is_err());
        assert!(accept_reply(&c, "d1", true, "s1", None, "x", "bad rid!", &[], None).is_err());
        // 전달
        let mut p = Pipeline::new(c);
        let rep = p.tick(&Fake);
        assert_eq!(rep.changed_sessions, vec!["s1".to_string()]);
        let st: String = p.conn.query_row("SELECT state FROM conoti_reply WHERE reply_id = 'rid-00000001'", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "delivered");
        assert_eq!(replies_for(&p.conn, "s1", 10).len(), 1);
    }

    #[test]
    fn stale_and_hidden_are_not_delivered() {
        let c = mem();
        db::set_meta(&c, "conoti.bg_resume", "1").unwrap();
        c.execute("INSERT INTO relay_device (pid, name, phone_pub, can_reply, created_at) VALUES ('d1', '폰', 'ab', 1, 'x')", []).unwrap();
        accept_reply(&c, "d1", true, "s1", None, "지워 줘", "rid-00000009", &[], None).unwrap();
        c.execute("UPDATE conoti_reply SET received_at = '2026-01-01T00:00:00.000Z'", []).unwrap();
        let mut p = Pipeline::new(c);
        p.tick(&Fake);
        let st: String = p.conn.query_row("SELECT state FROM conoti_reply", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "rejected");
        p.conn.execute("UPDATE session SET hidden = 1", []).unwrap();
        assert_eq!(reply_block(&p.conn, true, "s1"), "no_session");
    }

    #[test]
    fn blocked_reply_is_recorded_as_rejected() {
        let c = mem();
        let r = accept_reply(&c, "d1", false, "s1", None, "해 줘", "rid-00000002", &[], None).unwrap();
        assert_eq!(r["state"], "rejected");
        assert!(r["note"].as_str().unwrap().contains("기기"));
    }

    #[test]
    fn confirm_mode_waits_and_device_revocation_blocks_at_delivery() {
        let c = mem();
        db::set_meta(&c, "conoti.bg_resume", "1").unwrap();
        db::set_meta(&c, "conoti.confirm", "1").unwrap();
        c.execute("INSERT INTO relay_device (pid, name, phone_pub, can_reply, created_at) VALUES ('d1', '폰', 'ab', 1, 'x')", []).unwrap();
        assert_eq!(accept_reply(&c, "d1", true, "s1", None, "지워 줘", "rid-00000003", &[], None).unwrap()["state"], "confirm");
        // 확인 모드에서 허용 없이 delivering 이 된 답은 확인 대기로 되돌린다
        c.execute("UPDATE conoti_reply SET state = 'delivering'", []).unwrap();
        let mut p = Pipeline::new(c);
        p.tick(&Fake);
        let st: String = p.conn.query_row("SELECT state FROM conoti_reply", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "confirm");
        let c = p.conn;
        // 허용한 뒤 전달 직전에 기기 허용을 껐다면 막는다
        c.execute("UPDATE conoti_reply SET state = 'delivering', acked = 'approved'", []).unwrap();
        c.execute("UPDATE relay_device SET can_reply = 0", []).unwrap();
        let mut p = Pipeline::new(c);
        p.tick(&Fake);
        let st: String = p.conn.query_row("SELECT state FROM conoti_reply", [], |r| r.get(0)).unwrap();
        assert_eq!(st, "rejected");
    }
}
