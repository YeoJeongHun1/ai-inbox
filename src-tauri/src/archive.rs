//! 기록 보관함 — 지난 대화 검색 · 세션 정리(보관·지우기) · 보낸 메시지(데스크톱·폰) · 첨부 이미지 모아 보기와 지우기.
//!
//! 대화(turn)는 Claude Code 대화 기록(~/.claude)에서 모은 사본이다. 세션을 지우면 **이 앱의 사본**만 지우고
//! 원본은 건드리지 않는다 — `session_deleted` 에 지운 시각을 남겨 원본을 다시 읽어도 되살리지 않는다.
//! Claude Code 는 원본을 기본 30일 뒤 스스로 지우므로(cleanupPeriodDays) 오래된 대화는 이 앱에만 남는다.

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::attach::{self, Meta};

type R<T> = Result<T, String>;

fn like_of(q: &str) -> String {
    format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"))
}

const NAME_COLS: &str = "s.live_name, s.title, s.agent_name,
    (SELECT prompt_text FROM turn t0 WHERE t0.session_id = s.id AND t0.hidden = 0 ORDER BY seq LIMIT 1), s.project_dir";

fn name_at(r: &rusqlite::Row, at: usize) -> rusqlite::Result<String> {
    let dir: Option<String> = r.get(at + 4)?;
    Ok(crate::api::display_name(r.get(at)?, r.get(at + 1)?, r.get(at + 2)?, r.get(at + 3)?, &dir).0)
}

// ── 지난 대화 검색 ───────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct TurnHit {
    turn_id: i64,
    session_id: String,
    session_name: String,
    prompt_at: String,
    status: String,
    origin: Option<String>,
    /// 요청 앞부분(첨부 경로 목록은 뗀다)
    prompt: String,
    /// 검색어가 걸린 곳 주변(요청·요약·응답 중 처음 걸린 것)
    snippet: String,
    snippet_in: &'static str,
    atts: Vec<String>,
    /// 보관한 세션의 요청(목록에는 없지만 검색에는 나온다)
    archived: bool,
}

#[derive(Serialize)]
pub struct Page<T> {
    items: Vec<T>,
    has_more: bool,
}

/// 검색어가 걸린 곳 주변 `width` 자
pub fn snippet(text: &str, q: &str, width: usize) -> Option<String> {
    let lower = text.to_lowercase();
    let ql = q.to_lowercase();
    let byte = lower.find(&ql)?;
    // 소문자로 바꿔도 글자 수는 같다고 보고 글자 위치로 옮긴다(한글·영문은 그렇다)
    let at = lower[..byte].chars().count();
    let chars: Vec<char> = text.chars().collect();
    let start = at.saturating_sub(width / 3);
    let end = (at + q.chars().count() + width * 2 / 3).min(chars.len());
    let mut s: String = chars[start.min(chars.len())..end].iter().collect();
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        s.insert(0, '…');
    }
    if end < chars.len() {
        s.push('…');
    }
    Some(s)
}

pub fn search_turns(conn: &Connection, query: &str, before: Option<&str>, limit: i64) -> R<Page<TurnHit>> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Page { items: vec![], has_more: false });
    }
    let limit = limit.clamp(1, 100);
    let like = like_of(q);
    let sql = format!(
        "SELECT t.id, t.session_id, t.prompt_at, t.status, t.origin, t.prompt_text, t.summary, t.response_text, {NAME_COLS}, s.hidden
           FROM turn t JOIN session s ON s.id = t.session_id
          WHERE t.hidden = 0
            AND (?3 IS NULL OR t.prompt_at < ?3)
            AND (t.prompt_text LIKE ?1 ESCAPE '\\' OR t.response_text LIKE ?1 ESCAPE '\\' OR t.summary LIKE ?1 ESCAPE '\\'
                 OR s.live_name LIKE ?1 ESCAPE '\\' OR s.title LIKE ?1 ESCAPE '\\')
          ORDER BY t.prompt_at DESC LIMIT ?2"
    );
    let mut st = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = st
        .query_map(params![like, limit + 1, before], |r| {
            let raw: Option<String> = r.get(5)?;
            let (prompt, atts) = raw.as_deref().map(attach::split_block).unwrap_or_default();
            let prompt = crate::conoti::strip_inbox(&prompt).map(str::to_string).unwrap_or(prompt);
            let summary: Option<String> = r.get(6)?;
            let response: Option<String> = r.get(7)?;
            let (snippet, snippet_in) = snippet(&prompt, q, 160)
                .map(|s| (s, "prompt"))
                .or_else(|| summary.as_deref().and_then(|t| snippet(t, q, 160)).map(|s| (s, "summary")))
                .or_else(|| response.as_deref().and_then(|t| snippet(t, q, 160)).map(|s| (s, "response")))
                .unwrap_or_else(|| (crate::text::clip(response.as_deref().unwrap_or(""), 160), "session"));
            Ok(TurnHit {
                turn_id: r.get(0)?,
                session_id: r.get(1)?,
                session_name: name_at(r, 8)?,
                prompt_at: r.get(2)?,
                status: r.get(3)?,
                origin: r.get(4)?,
                prompt: crate::text::clip(prompt.trim(), 200),
                snippet,
                snippet_in,
                atts,
                archived: r.get::<_, i64>(13)? != 0,
            })
        })
        .map_err(|e| e.to_string())?;
    let mut items: Vec<TurnHit> = rows.flatten().collect();
    let has_more = items.len() as i64 > limit;
    items.truncate(limit as usize);
    Ok(Page { items, has_more })
}

// ── 세션 정리 ────────────────────────────────────────────────────────────────
//
// 보관 = 목록과 폰에서 뺀다(검색에는 남는다, 보관한 뒤에 새 요청·결과가 오면 저절로 목록으로 돌아온다).
// 지우기 = 이 앱의 사본(요청·보낸 메시지·그 메시지의 이미지)을 지운다. 원본 대화 기록은 건드리지 않는다.

#[derive(Serialize)]
pub struct SessionRow {
    id: String,
    name: String,
    project_name: Option<String>,
    turns: i64,
    first_at: Option<String>,
    last_at: Option<String>,
    archived: bool,
    pinned: bool,
    /// Claude Code 프로세스가 살아 있다
    live: bool,
    /// 진행 중인 요청 수
    active: i64,
    unread: i64,
}

/// 세션별 요청 집계 — 요청 표를 한 번만 훑는다
fn agg_sql() -> String {
    format!(
        "SELECT session_id, COUNT(*) AS n, MIN(prompt_at) AS first_at,
                MAX(COALESCE(ended_at, last_activity_at, prompt_at)) AS last_at,
                SUM(status IN ('running','background','waiting')) AS active,
                SUM(read_at IS NULL AND status IN {f}) AS unread
           FROM turn WHERE hidden = 0 GROUP BY session_id",
        f = crate::api::FINISHED_SQL
    )
}

/// 세션 줄 — 요청이 하나도 없는 세션(훅만 온 것)은 목록에 없으므로 여기서도 뺀다. 뒤에 `AND …` 를 붙여 거른다.
fn session_rows_sql() -> String {
    format!(
        "SELECT * FROM (
           SELECT s.id, {NAME_COLS}, s.hidden, s.pinned, s.live_status IS NOT NULL AS live,
                  a.n, a.first_at, a.last_at, a.active, a.unread
             FROM session s JOIN ({agg}) a ON a.session_id = s.id
         ) x WHERE 1 = 1",
        agg = agg_sql()
    )
}

fn session_row(r: &rusqlite::Row) -> rusqlite::Result<SessionRow> {
    let dir: Option<String> = r.get(5)?;
    Ok(SessionRow {
        id: r.get(0)?,
        name: name_at(r, 1)?,
        project_name: crate::api::project_name(&dir),
        archived: r.get::<_, i64>(6)? != 0,
        pinned: r.get::<_, i64>(7)? != 0,
        live: r.get(8)?,
        turns: r.get(9)?,
        first_at: r.get(10)?,
        last_at: r.get(11)?,
        active: r.get(12)?,
        unread: r.get(13)?,
    })
}

fn cutoff_of(idle_days: Option<i64>) -> Option<String> {
    idle_days
        .filter(|d| *d > 0)
        .map(|d| (chrono::Utc::now() - chrono::Duration::days(d)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

/// `scope`: visible(목록에 있음) | archived(보관함) | all. `short`: 요청 1개 이하만. `idle_days`: 그 기간 넘게 조용한 것만.
pub fn sessions(conn: &Connection, query: &str, scope: &str, short: bool, idle_days: Option<i64>, offset: i64, limit: i64) -> R<Page<SessionRow>> {
    let q = query.trim();
    let limit = limit.clamp(1, 500);
    let like = like_of(q);
    let sql = format!(
        "{base}
           AND (?1 = 'all' OR (?1 = 'visible' AND x.hidden = 0) OR (?1 = 'archived' AND x.hidden = 1))
           AND (?2 = '' OR x.live_name LIKE ?3 ESCAPE '\\' OR x.title LIKE ?3 ESCAPE '\\' OR x.agent_name LIKE ?3 ESCAPE '\\'
                OR x.project_dir LIKE ?3 ESCAPE '\\'
                OR EXISTS (SELECT 1 FROM turn t WHERE t.session_id = x.id AND t.hidden = 0 AND t.prompt_text LIKE ?3 ESCAPE '\\'))
           AND (?4 = 0 OR x.n <= 1)
           AND (?5 IS NULL OR x.last_at < ?5)
         ORDER BY x.last_at DESC, x.id LIMIT ?6 OFFSET ?7",
        base = session_rows_sql()
    );
    let mut st = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut items: Vec<SessionRow> = st
        .query_map(params![scope, q, like, short as i64, cutoff_of(idle_days), limit + 1, offset.max(0)], session_row)
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    let has_more = items.len() as i64 > limit;
    items.truncate(limit as usize);
    Ok(Page { items, has_more })
}

/// 보관하거나(true) 목록으로 되돌린다(false). 바뀐 세션 수. 보관 시각은 "이 뒤에 온 것이면 되돌린다"의 기준이다.
pub fn set_archived(conn: &Connection, ids: &[String], archived: bool) -> R<usize> {
    let now = crate::time::now_iso();
    let mut n = 0;
    for id in ids {
        n += if archived {
            conn.execute("UPDATE session SET hidden = 1, archived_at = ?2 WHERE id = ?1 AND hidden = 0", params![id, now])
        } else {
            conn.execute("UPDATE session SET hidden = 0, archived_at = NULL WHERE id = ?1 AND hidden = 1", params![id])
        }
        .map_err(|e| e.to_string())?;
    }
    Ok(n)
}

/// 한 번에 정리해도 되는 세션 — 목록에 있고, 고정하지 않았고, 실행 중이 아니고, 진행 중·안 읽은 결과·전달 대기 말이 없는 것
const SAFE_TO_TIDY: &str = "x.hidden = 0 AND x.pinned = 0 AND x.live = 0 AND x.active = 0 AND x.unread = 0
    AND NOT EXISTS (SELECT 1 FROM conoti_reply r WHERE r.session_id = x.id AND r.state IN ('confirm','delivering'))";

/// 정리 제안: `short` 요청 1개 이하인 세션 · `idle_days` 그 기간 넘게 조용한 세션 — 한 번에 정리해도 되는 것만
pub fn tidy_candidates(conn: &Connection, short: bool, idle_days: Option<i64>) -> R<Vec<String>> {
    let sql = format!(
        "{base} AND {SAFE_TO_TIDY} AND (?1 = 0 OR x.n <= 1) AND (?2 IS NULL OR x.last_at < ?2) ORDER BY x.last_at",
        base = session_rows_sql()
    );
    let mut st = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let ids = st
        .query_map(params![short as i64, cutoff_of(idle_days)], |r| r.get::<_, String>(0))
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    Ok(ids)
}

#[derive(Serialize, Debug)]
pub struct Deleted {
    /// 지운 세션 id
    pub deleted: Vec<String>,
    /// 진행 중이거나 전달 대기 말이 있어 남긴 세션 이름
    pub skipped: Vec<String>,
}

/// 이 앱의 기록에서 세션을 지운다 — 요청(단계·도구·파일 포함)·보낸 메시지·그 메시지에만 붙은 이미지·훅 기록.
/// 진행 중인 요청·일하는 중인 프로세스·아직 전달하지 않은 말이 있는 세션은 남긴다(지우면 그 결과·말이 사라진다).
/// 세션별 "폰 답 막기" 설정은 남긴다 — 지운 세션이 이어서 실행되면 막아 둔 그대로여야 한다.
/// 검사와 지우기는 한 쓰기 잠금 안에서 한다(수집·전달 스레드는 다른 연결로 쓴다).
pub fn delete_sessions(conn: &Connection, ids: &[String]) -> R<Deleted> {
    conn.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;
    let res = delete_locked(conn, ids);
    match &res {
        Ok(_) => {
            if let Err(e) = conn.execute_batch("COMMIT") {
                // 커밋이 실패하면 트랜잭션이 열린 채 남아 다른 쓰기를 막는다 — 되돌리고 알린다
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e.to_string());
            }
        }
        Err(_) => {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
    let (out, orphans) = res?;
    for (att, sent) in orphans {
        // 그 뒤로 입력창 초안에 다시 붙인 같은 이미지는 남긴다(7일 유예는 gc 가 맡는다)
        attach::delete_if_orphan_since(conn, &att, &sent);
    }
    Ok(out)
}

/// (지운 결과, (이미지 id, 그 이미지를 보낸 시각) — 커밋 뒤 고아면 지운다)
fn delete_locked(conn: &Connection, ids: &[String]) -> R<(Deleted, Vec<(String, String)>)> {
    let now = crate::time::now_iso();
    let mut out = Deleted { deleted: vec![], skipped: vec![] };
    let mut orphans: Vec<(String, String)> = Vec::new();
    for id in ids {
        let row: Option<(i64, String)> = conn
            .query_row(
                &format!(
                    // 바쁨: 도는 요청 · 아직 안 넘긴 말 · 넘겼지만 그 말로 시작된 요청을 아직 못 찾은 말(3일 — link_results 가 찾는 범위)
                    //       · 일하거나(busy) 허락을 기다리는(waiting) 프로세스
                    "SELECT (SELECT COUNT(*) FROM turn WHERE session_id = s.id AND status IN ('running','background','waiting'))
                          + (SELECT COUNT(*) FROM conoti_reply WHERE session_id = s.id
                              AND (state IN ('confirm','delivering') OR (state = 'delivered' AND result_turn IS NULL AND delivered_at >= ?2)))
                          + COALESCE(s.live_status IN ('busy','waiting'), 0),
                            {NAME_COLS}
                       FROM session s WHERE s.id = ?1"
                ),
                params![id, crate::time::iso_from_ms(chrono::Utc::now().timestamp_millis() - 3 * 86_400_000)],
                |r| Ok((r.get::<_, Option<i64>>(0)?.unwrap_or(0), name_at(r, 1)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        let Some((busy, name)) = row else { continue };
        if busy > 0 {
            out.skipped.push(name);
            continue;
        }
        let rids: Vec<(String, String)> = {
            let mut st = conn
                .prepare("SELECT reply_id, received_at FROM conoti_reply WHERE session_id = ?1")
                .map_err(|e| e.to_string())?;
            let v = st.query_map(params![id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(|e| e.to_string())?.flatten().collect();
            v
        };
        for (rid, sent) in &rids {
            orphans.extend(attach::for_reply(conn, rid).into_iter().map(|a| (a, sent.clone())));
            conn.execute("DELETE FROM reply_attachment WHERE reply_id = ?1", params![rid]).map_err(|e| e.to_string())?;
        }
        conn.execute("DELETE FROM conoti_reply WHERE session_id = ?1", params![id]).map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM hook_event WHERE session_id = ?1", params![id]).map_err(|e| e.to_string())?;
        // 지운 요청의 ID — 원본을 다시 읽어도, 복사본 세션에 같은 요청이 있어도 되살리지 않는다
        conn.execute(
            "INSERT INTO turn_deleted (session_id, prompt_uuid, deleted_at)
             SELECT session_id, prompt_uuid, ?2 FROM turn WHERE session_id = ?1
             ON CONFLICT(session_id, prompt_uuid) DO UPDATE SET deleted_at = excluded.deleted_at",
            params![id, now],
        )
        .map_err(|e| e.to_string())?;
        // 요청과 그 자식 표(단계·도구·파일·서브에이전트)는 외래 키로 함께 지워진다
        conn.execute("DELETE FROM session WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
        out.deleted.push(id.clone());
    }
    Ok((out, orphans))
}

// ── 보낸 메시지 ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct Message {
    rid: String,
    session_id: Option<String>,
    session_name: Option<String>,
    text: String,
    state: String,
    note: Option<String>,
    at: String,
    from_phone: bool,
    device_name: Option<String>,
    result_turn: Option<i64>,
    atts: Vec<Meta>,
}

/// `device`: all | desktop | phone. `images_only`: 이미지가 붙은 것만.
pub fn messages(conn: &Connection, query: &str, device: &str, images_only: bool, before: Option<&str>, limit: i64) -> R<Page<Message>> {
    let q = query.trim();
    let limit = limit.clamp(1, 200);
    let like = like_of(q);
    let sql = format!(
        "SELECT r.reply_id, r.session_id, r.text, r.state, r.note, r.received_at, r.device, d.name, r.result_turn,
                s.id IS NOT NULL, {NAME_COLS}
           FROM conoti_reply r
           LEFT JOIN session s ON s.id = r.session_id
           LEFT JOIN relay_device d ON d.pid = r.device
          WHERE (?1 = '' OR r.text LIKE ?2 ESCAPE '\\' OR s.live_name LIKE ?2 ESCAPE '\\' OR s.title LIKE ?2 ESCAPE '\\'
                 OR EXISTS (SELECT 1 FROM reply_attachment ra JOIN attachment a ON a.id = ra.att_id
                             WHERE ra.reply_id = r.reply_id AND a.name LIKE ?2 ESCAPE '\\'))
            AND (?3 = 'all' OR (?3 = 'desktop' AND r.device = 'desktop') OR (?3 = 'phone' AND COALESCE(r.device, '') <> 'desktop'))
            AND (?4 = 0 OR EXISTS (SELECT 1 FROM reply_attachment ra WHERE ra.reply_id = r.reply_id))
            AND (?5 IS NULL OR r.received_at < ?5)
          ORDER BY r.received_at DESC LIMIT ?6"
    );
    let mut st = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let mut items: Vec<Message> = st
        .query_map(params![q, like, device, images_only as i64, before, limit + 1], |r| {
            let has_session: bool = r.get(9)?;
            let dev: Option<String> = r.get(6)?;
            Ok(Message {
                    rid: r.get(0)?,
                    session_id: r.get(1)?,
                    session_name: if has_session { Some(name_at(r, 10)?) } else { None },
                    text: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    state: r.get(3)?,
                    note: r.get(4)?,
                    at: r.get(5)?,
                    from_phone: dev.as_deref() != Some(crate::conoti::DESKTOP),
                    device_name: r.get(7)?,
                    result_turn: r.get(8)?,
                    atts: vec![],
                })
        })
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    let has_more = items.len() as i64 > limit;
    items.truncate(limit as usize);
    for m in &mut items {
        m.atts = attach::metas(conn, &attach::for_reply(conn, &m.rid));
    }
    Ok(Page { items, has_more })
}

/// 보낸 메시지 기록을 지운다. 이미지는 다른 메시지가 쓰지 않으면 파일까지 지운다. 지운 개수.
/// 아직 전달하지 않은 것(확인 대기·전달 중)을 지우면 보내지 않는다.
pub fn delete_messages(conn: &Connection, rids: &[String]) -> R<usize> {
    let mut n = 0;
    for rid in rids.iter().take(500) {
        let atts = attach::for_reply(conn, rid);
        let sent: String = conn
            .query_row("SELECT received_at FROM conoti_reply WHERE reply_id = ?1", params![rid], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?
            .unwrap_or_default();
        conn.execute("DELETE FROM reply_attachment WHERE reply_id = ?1", params![rid]).map_err(|e| e.to_string())?;
        n += conn.execute("DELETE FROM conoti_reply WHERE reply_id = ?1", params![rid]).map_err(|e| e.to_string())?;
        for id in atts {
            // 그 뒤로 입력창 초안에 다시 붙인 같은 이미지는 남긴다
            attach::delete_if_orphan_since(conn, &id, &sent);
        }
    }
    Ok(n)
}

// ── 이미지 ───────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct UsedIn {
    rid: String,
    session_id: Option<String>,
    session_name: Option<String>,
    at: String,
    text: String,
    from_phone: bool,
}

#[derive(Serialize)]
pub struct Image {
    #[serde(flatten)]
    meta: Meta,
    used_in: Vec<UsedIn>,
}

/// 보낸 이미지(메시지에 붙은 것) — 최근 것부터. 검색어는 파일 이름·메시지 글·세션 이름에 건다.
pub fn images(conn: &Connection, query: &str, before: Option<&str>, limit: i64) -> R<Page<Image>> {
    let q = query.trim();
    let limit = limit.clamp(1, 300);
    let like = like_of(q);
    let mut st = conn
        .prepare(
            "SELECT a.id FROM attachment a
              WHERE EXISTS (SELECT 1 FROM reply_attachment ra WHERE ra.att_id = a.id)
                AND (?1 = '' OR a.name LIKE ?2 ESCAPE '\\'
                     OR EXISTS (SELECT 1 FROM reply_attachment ra JOIN conoti_reply r ON r.reply_id = ra.reply_id
                                LEFT JOIN session s ON s.id = r.session_id
                                WHERE ra.att_id = a.id
                                  AND (r.text LIKE ?2 ESCAPE '\\' OR s.live_name LIKE ?2 ESCAPE '\\' OR s.title LIKE ?2 ESCAPE '\\')))
                AND (?3 IS NULL OR a.created_at < ?3)
              ORDER BY a.created_at DESC LIMIT ?4",
        )
        .map_err(|e| e.to_string())?;
    let ids: Vec<String> = st
        .query_map(params![q, like, before, limit + 1], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    let has_more = ids.len() as i64 > limit;
    let mut items = Vec::new();
    for id in ids.iter().take(limit as usize) {
        let Some(meta) = attach::meta(conn, id) else { continue };
        items.push(Image { meta, used_in: used_in(conn, id)? });
    }
    Ok(Page { items, has_more })
}

fn used_in(conn: &Connection, id: &str) -> R<Vec<UsedIn>> {
    let mut st = conn
        .prepare(&format!(
            "SELECT r.reply_id, r.session_id, r.received_at, r.text, r.device, s.id IS NOT NULL, {NAME_COLS}
               FROM reply_attachment ra JOIN conoti_reply r ON r.reply_id = ra.reply_id
               LEFT JOIN session s ON s.id = r.session_id
              WHERE ra.att_id = ?1 ORDER BY r.received_at DESC LIMIT 20"
        ))
        .map_err(|e| e.to_string())?;
    let rows = st
        .query_map(params![id], |r| {
            let has_session: bool = r.get(5)?;
            Ok(UsedIn {
                rid: r.get(0)?,
                session_id: r.get(1)?,
                session_name: if has_session { Some(name_at(r, 6)?) } else { None },
                at: r.get(2)?,
                text: crate::text::clip(&r.get::<_, Option<String>>(3)?.unwrap_or_default(), 120),
                from_phone: r.get::<_, Option<String>>(4)?.as_deref() != Some(crate::conoti::DESKTOP),
            })
        })
        .map_err(|e| e.to_string())?;
    Ok(rows.flatten().collect())
}

pub fn delete_images(conn: &Connection, ids: &[String]) -> R<usize> {
    // 아직 전달하지 않은 말에 붙은 이미지를 지우면 그 말이 이미지 없이(또는 빈 말로) 들어간다
    for id in ids {
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM reply_attachment ra JOIN conoti_reply r ON r.reply_id = ra.reply_id
                  WHERE ra.att_id = ?1 AND r.state IN ('confirm', 'delivering')",
                params![id],
                |r| r.get(0),
            )
            .unwrap_or(0);
        if pending > 0 {
            return Err("아직 전달하지 않은 메시지에 붙은 이미지입니다 — 그 메시지를 취소하거나 전달된 뒤에 지우세요".into());
        }
    }
    let mut n = 0;
    for id in ids.iter().take(1000) {
        if attach::meta(conn, id).is_some() {
            attach::delete(conn, id)?;
            n += 1;
        }
    }
    Ok(n)
}

#[derive(Serialize)]
pub struct Stats {
    messages: i64,
    images: i64,
    image_bytes: i64,
    /// 목록에 있는 세션 · 보관한 세션
    pub sessions: i64,
    pub archived: i64,
    /// 한 번에 정리해도 되는 것 중 요청 1개 이하 · 30일 넘게 조용한 세션
    pub tidy_short: i64,
    pub tidy_idle: i64,
}

/// 정리 제안의 "오래 조용한" 기준
pub const TIDY_IDLE_DAYS: i64 = 30;

pub fn stats(conn: &Connection) -> Stats {
    let one = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, Option<i64>>(0)).optional().ok().flatten().flatten().unwrap_or(0);
    // 세션 수·정리 제안은 요청 표를 한 번만 훑어 센다(tidy_candidates 와 같은 조건)
    let safe = "s.hidden = 0 AND s.pinned = 0 AND s.live_status IS NULL AND a.active = 0 AND a.unread = 0
        AND NOT EXISTS (SELECT 1 FROM conoti_reply r WHERE r.session_id = s.id AND r.state IN ('confirm','delivering'))";
    let (sessions, archived, tidy_short, tidy_idle) = conn
        .query_row(
            &format!(
                "SELECT COALESCE(SUM(s.hidden = 0), 0), COALESCE(SUM(s.hidden = 1), 0),
                        COALESCE(SUM({safe} AND a.n <= 1), 0), COALESCE(SUM({safe} AND a.last_at < ?1), 0)
                   FROM session s JOIN ({agg}) a ON a.session_id = s.id",
                agg = agg_sql()
            ),
            params![cutoff_of(Some(TIDY_IDLE_DAYS))],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)),
        )
        .unwrap_or((0, 0, 0, 0));
    Stats {
        messages: one("SELECT COUNT(*) FROM conoti_reply"),
        images: one("SELECT COUNT(*) FROM attachment a WHERE EXISTS (SELECT 1 FROM reply_attachment ra WHERE ra.att_id = a.id)"),
        image_bytes: one("SELECT SUM(bytes) FROM attachment"),
        sessions,
        archived,
        tidy_short,
        tidy_idle,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::migrate(&c).unwrap();
        c.execute(
            "INSERT INTO session (id, project_dir, first_at, last_at, live_name) VALUES ('s1', '/tmp/p', '2026-09-24T00:00:00Z', '2026-09-24T00:00:00Z', 'api-refactor')",
            [],
        )
        .unwrap();
        for (i, (p, resp)) in [("로그인 버튼 색을 바꿔 줘", "바꿨습니다"), ("테스트 돌려 줘", "Deploy 준비 끝")].iter().enumerate() {
            c.execute(
                "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, response_text, status) VALUES ('s1', ?1, ?2, ?3, ?4, ?5, 'done')",
                params![format!("u{i}"), i as i64, format!("2026-09-24T0{i}:00:00Z"), p, resp],
            )
            .unwrap();
        }
        for (rid, dev, text, at) in [("dk1", "desktop", "색 바꿔 줘", "2026-09-24T01:00:00Z"), ("rid-0001", "pid1", "폰에서 보냄", "2026-09-24T02:00:00Z")] {
            c.execute(
                "INSERT INTO conoti_reply (reply_id, session_id, kind, text, received_at, state, device) VALUES (?1, 's1', 'text', ?2, ?3, 'handled', ?4)",
                params![rid, text, at, dev],
            )
            .unwrap();
        }
        c
    }

    #[test]
    fn snippets_center_on_the_hit() {
        assert_eq!(snippet("앞 부분 내용 로그인 버튼 뒤 부분", "로그인", 12).unwrap(), "…내용 로그인 버튼 뒤 부분");
        assert_eq!(snippet("앞 부분 내용 로그인 버튼 뒤 부분 그리고 더 긴 꼬리", "로그인", 12).unwrap(), "…내용 로그인 버튼 뒤 부분…");
        assert_eq!(snippet("Deploy ready", "deploy", 40).unwrap(), "Deploy ready");
        assert!(snippet("없음", "zzz", 10).is_none());
    }

    #[test]
    fn turn_search_finds_prompt_and_response() {
        let c = mem();
        let p = search_turns(&c, "로그인", None, 20).unwrap();
        assert_eq!(p.items.len(), 1);
        assert_eq!(p.items[0].session_name, "api-refactor");
        assert_eq!(p.items[0].snippet_in, "prompt");
        let p = search_turns(&c, "deploy", None, 20).unwrap();
        assert_eq!(p.items[0].snippet_in, "response");
        assert!(search_turns(&c, "  ", None, 20).unwrap().items.is_empty());
        // LIKE 특수문자는 글자 그대로
        assert!(search_turns(&c, "%", None, 20).unwrap().items.is_empty());
        let p = search_turns(&c, "줘", None, 1).unwrap();
        assert!(p.has_more);
    }

    fn add_session(c: &Connection, id: &str, turns: &[(&str, &str)], live: bool) {
        c.execute(
            "INSERT INTO session (id, project_dir, live_name, live_status) VALUES (?1, '/tmp/p', ?1, ?2)",
            params![id, if live { Some("idle") } else { None }],
        )
        .unwrap();
        for (i, (at, status)) in turns.iter().enumerate() {
            c.execute(
                "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, ended_at, prompt_text, response_text, status, read_at)
                 VALUES (?1, ?2, ?3, ?4, ?4, '요청', '응답', ?5, ?4)",
                params![id, format!("{id}-{i}"), i as i64, at, status],
            )
            .unwrap();
        }
    }

    #[test]
    fn session_list_filters_and_tidy_candidates() {
        let c = mem(); // s1: 요청 2개(2026-09-24) · 읽지 않음
        let old = "2026-01-01T00:00:00.000Z";
        let now = crate::time::now_iso();
        add_session(&c, "short-old", &[(old, "done")], false);
        add_session(&c, "short-new", &[(&now, "done")], false);
        add_session(&c, "long-old", &[(old, "done"), (old, "done")], false);
        add_session(&c, "short-live", &[(old, "done")], true);
        add_session(&c, "short-pinned", &[(old, "done")], false);
        c.execute("UPDATE session SET pinned = 1 WHERE id = 'short-pinned'", []).unwrap();
        add_session(&c, "empty", &[], false);

        let all = sessions(&c, "", "all", false, None, 0, 100).unwrap();
        assert_eq!(all.items.len(), 6, "요청 없는 세션은 목록에 없다");
        assert_eq!(all.items[0].id, "short-new", "최근 활동 순");
        let short = sessions(&c, "", "visible", true, None, 0, 100).unwrap();
        assert_eq!(short.items.len(), 4);
        let idle = sessions(&c, "", "visible", false, Some(30), 0, 100).unwrap();
        assert!(idle.items.iter().all(|r| r.id != "short-new"));
        assert_eq!(sessions(&c, "long", "all", false, None, 0, 100).unwrap().items.len(), 1);
        assert!(sessions(&c, "", "all", false, None, 0, 2).unwrap().has_more);

        // 한 번에 정리: 고정·실행 중·안 읽은 결과·전달 대기 말이 있는 세션은 빼고
        c.execute(
            "INSERT INTO conoti_reply (reply_id, session_id, kind, text, received_at, state, device) VALUES ('rid-pend0001', 'short-new', 'text', '보낼 말', '2026-09-24T00:00:00Z', 'delivering', 'pid1')",
            [],
        )
        .unwrap();
        assert_eq!(tidy_candidates(&c, true, None).unwrap(), vec!["short-old".to_string()]);
        c.execute("UPDATE conoti_reply SET state = 'handled' WHERE reply_id = 'rid-pend0001'", []).unwrap();
        let mut t = tidy_candidates(&c, true, None).unwrap();
        t.sort();
        assert_eq!(t, vec!["short-new".to_string(), "short-old".to_string()]);
        let mut t = tidy_candidates(&c, false, Some(TIDY_IDLE_DAYS)).unwrap();
        t.sort();
        assert_eq!(t, vec!["long-old".to_string(), "short-old".to_string()]);
        let st = stats(&c);
        assert_eq!((st.sessions, st.archived, st.tidy_short, st.tidy_idle), (6, 0, 2, 2));

        assert_eq!(set_archived(&c, &["short-old".into(), "long-old".into()], true).unwrap(), 2);
        assert_eq!(set_archived(&c, &["short-old".into()], true).unwrap(), 0, "이미 보관됨");
        assert_eq!(sessions(&c, "", "archived", false, None, 0, 100).unwrap().items.len(), 2);
        assert_eq!(sessions(&c, "", "visible", false, None, 0, 100).unwrap().items.len(), 4);
        let st = stats(&c);
        assert_eq!((st.sessions, st.archived), (4, 2));
    }

    #[test]
    fn archived_sessions_stay_searchable() {
        let c = mem();
        set_archived(&c, &["s1".into()], true).unwrap();
        let p = search_turns(&c, "로그인", None, 20).unwrap();
        assert_eq!(p.items.len(), 1);
        assert!(p.items[0].archived);
    }

    #[test]
    fn delete_session_removes_copies_and_skips_busy() {
        let c = mem();
        let dir = std::env::temp_dir().join(format!("aiinbox-del-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        crate::paths::set_data_dir_override(dir.clone());
        add_session(&c, "other", &[("2026-09-20T00:00:00.000Z", "done")], false);
        // 아직 전달하지 않은 말이 있으면 남긴다(이름을 알려 준다) — 같이 고른 다른 세션은 지운다
        c.execute("UPDATE conoti_reply SET state = 'confirm' WHERE reply_id = 'dk1'", []).unwrap();
        let out = delete_sessions(&c, &["s1".into(), "other".into()]).unwrap();
        assert_eq!(out.deleted, vec!["other".to_string()]);
        assert_eq!(out.skipped, vec!["api-refactor".to_string()]);
        c.execute("UPDATE conoti_reply SET state = 'handled' WHERE reply_id = 'dk1'", []).unwrap();
        // 진행 중인 요청 · 일하는 중인 프로세스도 남긴다
        c.execute("UPDATE turn SET status = 'running' WHERE prompt_uuid = 'u1'", []).unwrap();
        assert!(delete_sessions(&c, &["s1".into()]).unwrap().deleted.is_empty());
        c.execute("UPDATE turn SET status = 'done' WHERE prompt_uuid = 'u1'", []).unwrap();
        c.execute("UPDATE session SET live_status = 'busy' WHERE id = 's1'", []).unwrap();
        assert!(delete_sessions(&c, &["s1".into()]).unwrap().deleted.is_empty());
        // 허락을 기다리는 프로세스도(waiting)
        c.execute("UPDATE session SET live_status = 'waiting' WHERE id = 's1'", []).unwrap();
        assert!(delete_sessions(&c, &["s1".into()]).unwrap().deleted.is_empty());
        c.execute("UPDATE session SET live_status = 'idle' WHERE id = 's1'", []).unwrap();
        // 방금 넘겼는데 그 말로 시작된 요청을 아직 못 찾은 말 — 지우면 이어 실행한 세션의 표식이 고아가 된다
        let recent = crate::time::now_iso();
        c.execute("UPDATE conoti_reply SET state = 'delivered', delivered_at = ?1, result_turn = NULL WHERE reply_id = 'dk1'", params![recent]).unwrap();
        assert!(delete_sessions(&c, &["s1".into()]).unwrap().deleted.is_empty());
        // 3일이 지난 것(더는 찾지 않는다)은 막지 않는다
        c.execute("UPDATE conoti_reply SET delivered_at = '2026-01-01T00:00:00.000Z' WHERE reply_id = 'dk1'", []).unwrap();
        c.execute("UPDATE conoti_reply SET state = 'handled' WHERE reply_id = 'dk1'", []).unwrap();

        c.execute("INSERT INTO hook_event (session_id, event, at) VALUES ('s1', 'Stop', '2026-09-24T00:00:00Z')", []).unwrap();
        c.execute("INSERT INTO conoti_session (session_id, mode) VALUES ('s1', 1)", []).unwrap();
        c.execute("INSERT INTO turn_step (turn_id, seq, at, kind) SELECT id, 0, prompt_at, 'text' FROM turn WHERE session_id = 's1'", []).unwrap();
        let n = |sql: &str| c.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(n("SELECT COUNT(*) FROM turn_step"), 2);
        let out = delete_sessions(&c, &["s1".into(), "없는-세션".into()]).unwrap();
        assert_eq!(out.deleted, vec!["s1".to_string()]);
        assert_eq!(n("SELECT COUNT(*) FROM turn"), 0);
        assert_eq!(n("SELECT COUNT(*) FROM turn_step"), 0);
        assert_eq!(n("SELECT COUNT(*) FROM conoti_reply"), 0);
        assert_eq!(n("SELECT COUNT(*) FROM hook_event"), 0);
        // 세션별 "폰 답 막기"는 남긴다 — 지운 세션이 이어서 실행돼도 막아 둔 그대로
        assert_eq!(n("SELECT mode FROM conoti_session WHERE session_id = 's1'"), 1);
        assert_eq!(n("SELECT COUNT(*) FROM turn_deleted WHERE session_id = 's1'"), 2);
        assert!(search_turns(&c, "로그인", None, 20).unwrap().items.is_empty());
    }

    #[test]
    fn deleting_keeps_an_image_reattached_to_a_draft() {
        let c = mem();
        let dir = std::env::temp_dir().join(format!("aiinbox-del-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        crate::paths::set_data_dir_override(dir);
        let (old, draft) = ("ab".repeat(32), "cd".repeat(32));
        for (ord, (id, touched)) in [(&old, "2026-09-24T01:00:00Z"), (&draft, "2026-09-25T09:00:00Z")].into_iter().enumerate() {
            c.execute(
                "INSERT INTO attachment (id, mime, ext, bytes, width, height, source, created_at, touched_at, touched_by)
                 VALUES (?1, 'image/png', 'png', 10, 1, 1, 'desktop', ?2, ?2, 'desktop')",
                params![id, touched],
            )
            .unwrap();
            c.execute("INSERT INTO reply_attachment (reply_id, att_id, ord) VALUES ('dk1', ?1, ?2)", params![id, ord as i64]).unwrap();
        }
        // dk1 을 보낸 뒤(09-24 01:00) 같은 이미지를 입력창 초안에 다시 붙였다(touched_at 09-25) → 남는다
        assert_eq!(delete_sessions(&c, &["s1".into()]).unwrap().deleted.len(), 1);
        let n = |id: &str| c.query_row("SELECT COUNT(*) FROM attachment WHERE id = ?1", params![id], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(n(&old), 0);
        assert_eq!(n(&draft), 1);
    }

    /// 실제 DB 복사본으로 세션 탭·정리 제안 확인(원본을 건드리지 않게 복사본만 받는다):
    /// `sqlite3 inbox.db ".backup /tmp/x.db"` 뒤 `AI_INBOX_DB_COPY=/tmp/x.db cargo test live_db_sessions -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_db_sessions() {
        let path = std::env::var("AI_INBOX_DB_COPY").expect("AI_INBOX_DB_COPY");
        // 이 시험은 보관을 쓰고 이관을 돈다 — 실제 DB 는 받지 않는다
        assert!(!path.ends_with("inbox.db"), "실제 inbox.db 말고 .backup 으로 만든 복사본을 넘겨라");
        let c = Connection::open(path).unwrap();
        crate::db::migrate(&c).unwrap();
        let st = stats(&c);
        println!("세션 {} · 보관 {} · 정리 제안: 짧은 {} · 30일 조용 {}", st.sessions, st.archived, st.tidy_short, st.tidy_idle);
        for (scope, short, idle) in [("visible", false, None), ("visible", true, None), ("archived", false, None), ("all", false, Some(30))] {
            let p = sessions(&c, "", scope, short, idle, 0, 500).unwrap();
            println!("{scope} short={short} idle={idle:?} → {}개", p.items.len());
        }
        for r in sessions(&c, "", "visible", false, None, 0, 5).unwrap().items {
            println!("  {} · 요청 {} · 마지막 {:?} · 안읽음 {} · 실행 {}", r.name.chars().take(30).collect::<String>(), r.turns, r.last_at, r.unread, r.live);
        }
        let n = tidy_candidates(&c, true, None).unwrap().len();
        let archived = set_archived(&c, &tidy_candidates(&c, true, None).unwrap(), true).unwrap();
        assert_eq!(n, archived);
        println!("복사본에서 짧은 세션 {archived}개 보관 → 목록 {}개", stats(&c).sessions);
    }

    #[test]
    fn messages_filter_and_delete() {
        let c = mem();
        assert_eq!(messages(&c, "", "all", false, None, 50).unwrap().items.len(), 2);
        let ph = messages(&c, "", "phone", false, None, 50).unwrap();
        assert_eq!(ph.items.len(), 1);
        assert!(ph.items[0].from_phone);
        assert_eq!(messages(&c, "색", "desktop", false, None, 50).unwrap().items.len(), 1);
        assert!(messages(&c, "", "all", true, None, 50).unwrap().items.is_empty());
        assert_eq!(messages(&c, "", "all", false, Some("2026-09-24T02:00:00Z"), 50).unwrap().items.len(), 1);
        assert_eq!(delete_messages(&c, &["dk1".into()]).unwrap(), 1);
        assert_eq!(stats(&c).messages, 1);
    }
}
