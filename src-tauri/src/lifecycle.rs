//! /clear 로 끝난 대화의 수명 — 감지 · 삭제 예약 · 이력 보관 · 만료 삭제.
//!
//! 정본 설명은 docs/CLEAR.md. 요약:
//! - Claude Code 는 /clear 하면 옛 세션에 `SessionEnd(reason=clear)`, 곧바로 **새 세션 ID** 로 `SessionStart(source=clear)` 를 보낸다.
//!   옛 대화 기록 파일은 그대로 남고 `/resume` 으로 다시 열 수 있으며, /rewind 체크포인트(file-history)는 옛 세션 ID 로 남는다.
//!   둘 다 `cleanupPeriodDays`(기본 30일) 뒤 Claude Code 가 지운다.
//! - 그래서 "더는 되돌릴 수 없는 시점" = 마지막 활동 + max(30, cleanupPeriodDays)일 + 유예 7일 — 이 앱의 사본은 그때 지운다.
//!   확실하지 않으면 지우지 않는다(뒤에 이어 쓴 흔적·별표·고정·살아 있는 후속 프로세스·진행 중 작업이 있으면 미룬다).
//! - 사용자가 "보관" 을 고르면 `clear_state = 'keep'` — 이 DB 에 영구히 두고(자동 삭제·정리 제안에서 제외) 이력 탭에서 본다.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::{db, time};

/// Claude Code 의 기본 보존 기간(`cleanupPeriodDays` 기본값). 설정이 이보다 짧아도 이 값을 쓴다(더 오래 기다리는 쪽이 안전하다).
pub const RETENTION_DEFAULT_DAYS: i64 = 30;
/// 보존 기간이 지난 뒤 더 기다리는 유예 — Claude Code 의 정리는 세션을 시작할 때 도는 백그라운드 작업이라 정확한 시각을 알 수 없다.
pub const GRACE_DAYS: i64 = 7;
/// /clear 뒤 이 시간 넘게 지나서 새 요청·입력이 있으면 되살아난 것으로 본다
const REVIVE_SLACK_MS: i64 = 5_000;
/// 후속 세션 연결: SessionEnd 와 SessionStart 가 이 안에 오면 한 쌍으로 본다(실측 ≈ 50ms)
pub const PAIR_WINDOW_MS: i64 = 15_000;

pub const KEEP: &str = "keep";
pub const PURGE: &str = "purge";
pub const ASK: &str = "ask";

/// 설정 `clear_default`: 0 = 매번 묻기(정할 때까지 자동 삭제 없음) · 1 = 삭제 예약(기본) · 2 = 이력으로 보관
pub fn default_state(conn: &Connection) -> &'static str {
    match db::setting_i64(conn, "clear_default", 1) {
        0 => ASK,
        2 => KEEP,
        _ => PURGE,
    }
}

fn ms_of(iso: &str) -> Option<i64> {
    time::ms_of_iso(iso)
}

fn day_ms(days: i64) -> i64 {
    days * 86_400_000
}

// ── 보존 기간 ────────────────────────────────────────────────────────────────

/// settings.json 한 파일의 `cleanupPeriodDays`(정수 ≥1). 없거나 깨졌으면 None
fn cleanup_days_in(path: &Path) -> Option<i64> {
    let bytes = std::fs::read(path).ok().filter(|b| b.len() < 1024 * 1024)?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let n = v.get("cleanupPeriodDays")?;
    n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)).filter(|d| *d >= 1)
}

/// 이 세션에 적용될 수 있는 보존 기간(일) = max(30, 사용자 설정·프로젝트 설정·로컬 설정 중 가장 큰 값).
/// 조직 관리 설정은 읽지 않는다 — 더 짧아도 30 을 쓰고, 더 길면 놓칠 수 있어 이 값이 "확실한 하한"은 아니다(문서에 적음).
pub fn retention_days(project_dir: Option<&str>) -> i64 {
    let mut best = RETENTION_DEFAULT_DAYS;
    let mut files = vec![
        crate::paths::claude_dir().join("settings.json"),
        crate::paths::claude_dir().join("settings.local.json"),
    ];
    if let Some(dir) = project_dir.filter(|d| d.starts_with('/') || d.chars().nth(1) == Some(':')) {
        files.push(Path::new(dir).join(".claude").join("settings.json"));
        files.push(Path::new(dir).join(".claude").join("settings.local.json"));
    }
    for f in files {
        if let Some(d) = cleanup_days_in(&f) {
            best = best.max(d);
        }
    }
    best
}

/// 대화 기록 파일의 수정 시각(ms). 없으면 None
fn mtime_ms(path: Option<&str>) -> Option<i64> {
    let m = std::fs::metadata(path?).ok()?.modified().ok()?;
    m.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_millis() as i64)
}

struct Basis {
    /// 마지막 활동 시각(ms) — /clear 시각·세션 마지막 시각·대화 기록 파일 수정 시각 중 가장 늦은 것
    last_ms: i64,
    project_dir: Option<String>,
}

fn basis(conn: &Connection, sid: &str) -> Option<Basis> {
    let (cleared, last, tp, dir): (Option<String>, Option<String>, Option<String>, Option<String>) = conn
        .query_row("SELECT cleared_at, last_at, transcript_path, project_dir FROM session WHERE id = ?1", params![sid], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .optional()
        .ok()??;
    let last_turn: Option<String> = conn
        .query_row(
            "SELECT MAX(COALESCE(ended_at, last_activity_at, prompt_at)) FROM turn WHERE session_id = ?1",
            params![sid],
            |r| r.get(0),
        )
        .ok()
        .flatten();
    let last_ms = [cleared.as_deref().and_then(ms_of), last.as_deref().and_then(ms_of), last_turn.as_deref().and_then(ms_of), mtime_ms(tp.as_deref())]
        .into_iter()
        .flatten()
        .max()?;
    Some(Basis { last_ms, project_dir: dir })
}

/// 삭제 예정 시각(ms) = 마지막 활동 + max(30, cleanupPeriodDays)일 + 유예
pub fn due_ms(last_ms: i64, retention: i64) -> i64 {
    last_ms + day_ms(retention + GRACE_DAYS)
}

// ── 감지 ─────────────────────────────────────────────────────────────────────

/// SessionEnd(reason=clear) — 이 세션은 /clear 로 끝났다. 기본 처리(설정)를 적용한다. 새로 표시했으면 true
pub fn on_clear(conn: &Connection, sid: &str, at: &str) -> bool {
    let state: Option<(Option<String>, Option<String>)> = conn
        .query_row("SELECT cleared_at, clear_state FROM session WHERE id = ?1", params![sid], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .ok()
        .flatten();
    let Some((cleared, prev)) = state else { return false };
    // 같은 /clear 를 두 번 받으면(재읽기) 처음 것을 그대로 둔다
    if cleared.is_some() && ms_of(cleared.as_deref().unwrap_or("")).zip(ms_of(at)).map(|(a, b)| (a - b).abs() < 2_000).unwrap_or(false) {
        return false;
    }
    // 예전에 '보관' 을 골랐던 세션이 다시 /clear 되면 보관을 그대로 잇는다
    let state = if prev.as_deref() == Some(KEEP) { KEEP } else { default_state(conn) };
    let _ = conn.execute(
        "UPDATE session SET cleared_at = ?2, cleared_to = NULL, clear_state = ?3, purge_at = NULL, clear_asked = 0,
                kept_at = CASE WHEN ?3 = 'keep' THEN COALESCE(kept_at, ?2) ELSE NULL END
          WHERE id = ?1",
        params![sid, at, state],
    );
    if state == PURGE {
        schedule(conn, sid);
    }
    true
}

/// SessionStart(source=clear) — 방금 /clear 된 세션(`old`)의 후속 세션
pub fn link_successor(conn: &Connection, old: &str, new: &str) {
    if old != new {
        let _ = conn.execute("UPDATE session SET cleared_to = ?2 WHERE id = ?1 AND cleared_at IS NOT NULL", params![old, new]);
    }
}

/// `purge_at` 를 계산해 넣는다(삭제 예약 상태일 때)
pub fn schedule(conn: &Connection, sid: &str) {
    let Some(b) = basis(conn, sid) else { return };
    let retention = retention_days(b.project_dir.as_deref());
    let at = time::iso_from_ms(due_ms(b.last_ms, retention));
    let _ = conn.execute("UPDATE session SET purge_at = ?2 WHERE id = ?1 AND clear_state = 'purge'", params![sid, at]);
}

/// 사용자의 선택. `decision`: keep(이력으로 보관) · purge(삭제 예약) · ask(보류 — 자동 삭제 없음). /clear 된 세션만 바꾼다. 바뀐 수
pub fn decide(conn: &Connection, ids: &[String], decision: &str) -> Result<usize, String> {
    if !matches!(decision, KEEP | PURGE | ASK) {
        return Err(format!("알 수 없는 선택: {decision}"));
    }
    let now = time::now_iso();
    let mut n = 0;
    for id in ids {
        n += conn
            .execute(
                "UPDATE session SET clear_state = ?2, clear_asked = 1,
                        kept_at = CASE WHEN ?2 = 'keep' THEN COALESCE(kept_at, ?3) ELSE NULL END,
                        purge_at = NULL
                  WHERE id = ?1 AND cleared_at IS NOT NULL",
                params![id, decision, now],
            )
            .map_err(|e| e.to_string())?;
        if decision == PURGE {
            schedule(conn, id);
        }
    }
    Ok(n)
}

/// 이 앱이 보여 줄 "끝난 대화" 정보
#[derive(Serialize, Clone, Debug, Default)]
pub struct Ended {
    pub cleared_at: String,
    /// purge | keep | ask
    pub state: String,
    pub purge_at: Option<String>,
    /// 사용자가 이미 확인·결정했나(false 면 "결정해 주세요" 안내 대상)
    pub asked: bool,
}

pub fn ended_of(cleared_at: Option<String>, state: Option<String>, purge_at: Option<String>, asked: i64) -> Option<Ended> {
    Some(Ended { cleared_at: cleared_at?, state: state.unwrap_or_else(|| ASK.into()), purge_at, asked: asked != 0 })
}

// ── 만료 삭제 ────────────────────────────────────────────────────────────────

#[derive(Default, Debug, PartialEq)]
pub struct SweepReport {
    /// /clear 뒤 다시 쓰여서 끝난 표시를 푼 세션
    pub revived: Vec<String>,
    /// 만료되어 지운 세션 id
    pub purged: Vec<String>,
    pub purged_turns: i64,
    /// 시각은 됐지만 안전 조건이 안 맞아 미룬 세션 수
    pub held: usize,
}

/// /clear 뒤 이어 쓴 흔적이 있으면 끝난 표시를 푼다 — 이 세션은 다시 살아 있는 대화다(rewind 가 아니라 /resume 으로 되살린 경우).
fn revive_used(conn: &Connection, rep: &mut SweepReport) {
    let ids: Vec<String> = conn
        .prepare("SELECT id FROM session WHERE cleared_at IS NOT NULL")
        .and_then(|mut st| Ok(st.query_map([], |r| r.get::<_, String>(0))?.flatten().collect()))
        .unwrap_or_default();
    for id in ids {
        if used_after_clear(conn, &id) {
            let _ = conn.execute(
                "UPDATE session SET cleared_at = NULL, cleared_to = NULL, clear_state = NULL, purge_at = NULL, clear_asked = 0, kept_at = NULL WHERE id = ?1",
                params![id],
            );
            rep.revived.push(id);
        }
    }
}

/// /clear 시각 뒤에 이 세션에서 새 요청이 시작됐거나 입력·재개 훅이 왔나
fn used_after_clear(conn: &Connection, sid: &str) -> bool {
    let Some(cleared): Option<String> = conn.query_row("SELECT cleared_at FROM session WHERE id = ?1", params![sid], |r| r.get(0)).ok().flatten() else {
        return false;
    };
    let Some(c_ms) = ms_of(&cleared) else { return false };
    let after = time::iso_from_ms(c_ms + REVIVE_SLACK_MS);
    let turn: i64 = conn
        .query_row("SELECT COUNT(*) FROM turn WHERE session_id = ?1 AND hidden = 0 AND prompt_at > ?2", params![sid, after], |r| r.get(0))
        .unwrap_or(0);
    let hook: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM hook_event WHERE session_id = ?1 AND at > ?2
               AND (event = 'UserPromptSubmit' OR (event = 'SessionStart' AND json_extract(detail, '$.source') IN ('resume','startup')))",
            params![sid, after],
            |r| r.get(0),
        )
        .unwrap_or(0);
    turn + hook > 0
}

/// 만료 검사 한 번. `now_ms` 를 받아 시험에서 시각을 옮긴다.
/// 지우는 조건(전부): 삭제 예약 상태 · 시각(마지막 활동 + 보존 + 유예)이 지남 · 후속 세션 프로세스 없음 · 자신도 실행 중 아님 ·
/// 별표·고정 없음 · 이어 쓴 흔적 없음. `archive::delete_sessions` 가 진행 중·전달 대기 말이 있는 세션을 다시 걸러 남긴다.
pub fn sweep(conn: &Connection, now_ms: i64) -> SweepReport {
    let mut rep = SweepReport::default();
    revive_used(conn, &mut rep);

    let due: Vec<(String, Option<String>)> = conn
        .prepare("SELECT id, purge_at FROM session WHERE clear_state = 'purge' AND cleared_at IS NOT NULL AND purge_at IS NOT NULL")
        .and_then(|mut st| Ok(st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.flatten().collect()))
        .unwrap_or_default();
    let mut ids = Vec::new();
    for (id, purge_at) in due {
        // 저장해 둔 시각이 아직이면 미룬다. 저장 뒤 대화 기록 파일이 갱신됐을 수 있어 아래에서 다시 계산한다
        if purge_at.as_deref().and_then(ms_of).map(|p| p > now_ms).unwrap_or(true) {
            continue;
        }
        let Some(b) = basis(conn, &id) else { continue };
        let fresh = due_ms(b.last_ms, retention_days(b.project_dir.as_deref()));
        if fresh > now_ms {
            // 그새 늦춰야 할 이유가 생겼다 — 예정 시각만 고친다
            let _ = conn.execute("UPDATE session SET purge_at = ?2 WHERE id = ?1", params![id, time::iso_from_ms(fresh)]);
            continue;
        }
        let blocked: i64 = conn
            .query_row(
                "SELECT
                   (SELECT COUNT(*) FROM session n WHERE n.id = s.cleared_to AND n.live_status IS NOT NULL)
                 + (s.live_status IS NOT NULL) + s.pinned
                 + (SELECT COUNT(*) FROM turn t WHERE t.session_id = s.id AND t.starred = 1)
                   FROM session s WHERE s.id = ?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap_or(1);
        if blocked > 0 {
            rep.held += 1;
            continue;
        }
        ids.push(id);
    }
    if ids.is_empty() {
        return rep;
    }
    let turn_counts: Vec<(String, i64)> = ids
        .iter()
        .map(|id| (id.clone(), conn.query_row("SELECT COUNT(*) FROM turn WHERE session_id = ?1", params![id], |r| r.get::<_, i64>(0)).unwrap_or(0)))
        .collect();
    match crate::archive::delete_sessions(conn, &ids) {
        Ok(d) => {
            // 값 없이 건수만 남긴다
            let gone = d.deleted.len() as i64;
            rep.held += d.skipped.len();
            let turns: i64 = turn_counts.iter().filter(|(id, _)| d.deleted.contains(id)).map(|(_, n)| n).sum();
            if gone > 0 {
                let _ = conn.execute(
                    "INSERT INTO purge_log (at, sessions, turns, reason) VALUES (?1, ?2, ?3, 'clear-expired')",
                    params![time::now_iso(), gone, turns],
                );
                eprintln!("[lifecycle] /clear 만료 삭제 — 세션 {gone}개 · 요청 {turns}개");
            }
            rep.purged_turns = turns;
            rep.purged = d.deleted;
        }
        Err(e) => eprintln!("[lifecycle] 삭제 실패: {}", crate::text::clip(&e, 80)),
    }
    rep
}

/// 삭제 예약이 걸린 세션 수·이력 보관 수·결정 대기 수 — 설정 화면용
#[derive(Serialize, Default)]
pub struct Overview {
    pub purge: i64,
    pub keep: i64,
    pub ask: i64,
    pub undecided: i64,
    /// 가장 이른 삭제 예정 시각
    pub next_purge_at: Option<String>,
    pub retention_days: i64,
    pub grace_days: i64,
    pub default_state: String,
    pub purged_sessions: i64,
}

pub fn overview(conn: &Connection) -> Overview {
    let n = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
    Overview {
        purge: n("SELECT COUNT(*) FROM session WHERE clear_state = 'purge'"),
        keep: n("SELECT COUNT(*) FROM session WHERE clear_state = 'keep'"),
        ask: n("SELECT COUNT(*) FROM session WHERE clear_state = 'ask'"),
        undecided: n("SELECT COUNT(*) FROM session WHERE cleared_at IS NOT NULL AND clear_asked = 0"),
        next_purge_at: conn.query_row("SELECT MIN(purge_at) FROM session WHERE clear_state = 'purge'", [], |r| r.get(0)).ok().flatten(),
        retention_days: retention_days(None),
        grace_days: GRACE_DAYS,
        default_state: default_state(conn).to_string(),
        purged_sessions: n("SELECT COALESCE(SUM(sessions), 0) FROM purge_log"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        db::migrate(&c).unwrap();
        c
    }

    const DAY: i64 = 86_400_000;

    fn t0() -> i64 {
        ms_of("2026-01-01T00:00:00.000Z").unwrap()
    }

    fn seed(c: &Connection, sid: &str, last_iso: &str, turns: usize) {
        c.execute("INSERT INTO session (id, project_dir, last_at, first_at) VALUES (?1, '/nonexistent/proj', ?2, ?2)", params![sid, last_iso]).unwrap();
        for i in 0..turns {
            c.execute(
                "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, response_text, status, ended_at)
                 VALUES (?1, ?2, ?3, ?4, '요청', '결과', 'done', ?4)",
                params![sid, format!("u{i}"), i as i64 + 1, last_iso],
            )
            .unwrap();
        }
    }

    fn count(c: &Connection, sql: &str) -> i64 {
        c.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn clear_is_detected_then_reserved_then_deleted_only_after_the_window() {
        let c = mem();
        seed(&c, "s1-aaaaaaaa", "2026-01-01T00:00:00.000Z", 2);
        assert!(on_clear(&c, "s1-aaaaaaaa", "2026-01-01T00:00:00.000Z"));
        let (state, purge_at): (String, String) = c.query_row("SELECT clear_state, purge_at FROM session WHERE id = 's1-aaaaaaaa'", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!(state, "purge", "기본값은 삭제 예약");
        // 시험 환경엔 ~/.claude 설정이 있을 수 있어 보존 기간은 30일 이상이기만 하면 된다
        let due = ms_of(&purge_at).unwrap();
        assert!(due >= t0() + 37 * DAY, "30일 + 유예 7일 전엔 예정 시각이 올 수 없다");

        // 유예가 끝나기 하루 전 — 아직
        let r = sweep(&c, t0() + 36 * DAY);
        assert!(r.purged.is_empty());
        assert_eq!(count(&c, "SELECT COUNT(*) FROM turn"), 2);
        // 예정 시각 뒤 — 지운다(요청 ID 표식이 남아 다시 읽어도 되살지 않는다)
        let r = sweep(&c, due + 1);
        assert_eq!(r.purged, vec!["s1-aaaaaaaa".to_string()]);
        assert_eq!(r.purged_turns, 2);
        assert_eq!(count(&c, "SELECT COUNT(*) FROM session"), 0);
        assert_eq!(count(&c, "SELECT COUNT(*) FROM turn"), 0);
        assert_eq!(count(&c, "SELECT COUNT(*) FROM turn_deleted"), 2);
        assert_eq!(count(&c, "SELECT sessions FROM purge_log"), 1);
        assert_eq!(count(&c, "SELECT turns FROM purge_log"), 2);
    }

    #[test]
    fn keep_choice_is_never_purged_and_survives_a_second_clear() {
        let c = mem();
        seed(&c, "s2-bbbbbbbb", "2026-01-01T00:00:00.000Z", 1);
        on_clear(&c, "s2-bbbbbbbb", "2026-01-01T00:00:00.000Z");
        assert_eq!(decide(&c, &["s2-bbbbbbbb".into()], KEEP).unwrap(), 1);
        let r = sweep(&c, t0() + 4000 * DAY);
        assert!(r.purged.is_empty());
        assert_eq!(count(&c, "SELECT COUNT(*) FROM session WHERE clear_state = 'keep' AND purge_at IS NULL AND kept_at IS NOT NULL"), 1);
        // 이어 쓰다 다시 /clear — 보관이 그대로 이어진다(기본 삭제 예약으로 돌아가지 않는다)
        on_clear(&c, "s2-bbbbbbbb", "2026-02-01T00:00:00.000Z");
        assert_eq!(count(&c, "SELECT COUNT(*) FROM session WHERE clear_state = 'keep'"), 1);
        // 마음을 바꿔 삭제 예약
        decide(&c, &["s2-bbbbbbbb".into()], PURGE).unwrap();
        assert_eq!(count(&c, "SELECT COUNT(*) FROM session WHERE clear_state = 'purge' AND purge_at IS NOT NULL AND kept_at IS NULL"), 1);
    }

    #[test]
    fn undecided_ask_state_never_auto_deletes() {
        let c = mem();
        db::set_meta(&c, "setting.clear_default", "0").unwrap();
        seed(&c, "s3-cccccccc", "2026-01-01T00:00:00.000Z", 1);
        on_clear(&c, "s3-cccccccc", "2026-01-01T00:00:00.000Z");
        assert_eq!(count(&c, "SELECT COUNT(*) FROM session WHERE clear_state = 'ask' AND purge_at IS NULL"), 1);
        assert!(sweep(&c, t0() + 4000 * DAY).purged.is_empty());
    }

    #[test]
    fn default_keep_setting_keeps_immediately() {
        let c = mem();
        db::set_meta(&c, "setting.clear_default", "2").unwrap();
        seed(&c, "s4-dddddddd", "2026-01-01T00:00:00.000Z", 1);
        on_clear(&c, "s4-dddddddd", "2026-01-01T00:00:00.000Z");
        assert_eq!(count(&c, "SELECT COUNT(*) FROM session WHERE clear_state = 'keep'"), 1);
    }

    #[test]
    fn resumed_session_is_revived_and_not_deleted() {
        let c = mem();
        seed(&c, "s5-eeeeeeee", "2026-01-01T00:00:00.000Z", 1);
        on_clear(&c, "s5-eeeeeeee", "2026-01-01T00:00:00.000Z");
        // /resume 으로 되살려 새 요청을 했다
        c.execute(
            "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, status) VALUES ('s5-eeeeeeee', 'late', 2, '2026-01-05T00:00:00.000Z', 'done')",
            [],
        )
        .unwrap();
        let r = sweep(&c, t0() + 4000 * DAY);
        assert_eq!(r.revived, vec!["s5-eeeeeeee".to_string()]);
        assert!(r.purged.is_empty());
        assert_eq!(count(&c, "SELECT COUNT(*) FROM session WHERE cleared_at IS NULL AND clear_state IS NULL"), 1);
    }

    #[test]
    fn resume_hook_after_clear_revives() {
        let c = mem();
        seed(&c, "s6-ffffffff", "2026-01-01T00:00:00.000Z", 1);
        on_clear(&c, "s6-ffffffff", "2026-01-01T00:00:00.000Z");
        c.execute("INSERT INTO hook_event (session_id, event, at, detail) VALUES ('s6-ffffffff', 'SessionStart', '2026-01-02T00:00:00.000Z', '{\"source\":\"resume\"}')", []).unwrap();
        assert_eq!(sweep(&c, t0() + 4000 * DAY).revived.len(), 1);
        // 같은 순간 온 SessionStart(source=clear)는 되살림이 아니다
        seed(&c, "s7-gggggggg", "2026-01-01T00:00:00.000Z", 1);
        on_clear(&c, "s7-gggggggg", "2026-01-01T00:00:00.000Z");
        c.execute("INSERT INTO hook_event (session_id, event, at, detail) VALUES ('s7-gggggggg', 'SessionStart', '2026-01-01T00:00:00.050Z', '{\"source\":\"clear\"}')", []).unwrap();
        assert!(sweep(&c, t0() + 1 * DAY).revived.is_empty());
    }

    #[test]
    fn safety_holds_defer_deletion() {
        let c = mem();
        let far = t0() + 4000 * DAY;
        // 별표
        seed(&c, "h1-11111111", "2026-01-01T00:00:00.000Z", 1);
        c.execute("UPDATE turn SET starred = 1 WHERE session_id = 'h1-11111111'", []).unwrap();
        // 고정
        seed(&c, "h2-22222222", "2026-01-01T00:00:00.000Z", 1);
        c.execute("UPDATE session SET pinned = 1 WHERE id = 'h2-22222222'", []).unwrap();
        // 후속 세션이 아직 살아 있다(/rewind 의 "이전 세션" 항목이 남아 있을 수 있다)
        seed(&c, "h3-33333333", "2026-01-01T00:00:00.000Z", 1);
        seed(&c, "h3n-nnnnnnnn", "2026-01-01T00:00:00.100Z", 0);
        c.execute("UPDATE session SET live_status = 'idle', live_pid = 1 WHERE id = 'h3n-nnnnnnnn'", []).unwrap();
        // 아직 전달하지 않은 말이 있다
        seed(&c, "h4-44444444", "2026-01-01T00:00:00.000Z", 1);
        c.execute("INSERT INTO conoti_reply (reply_id, session_id, state, received_at) VALUES ('r1', 'h4-44444444', 'delivering', '2026-01-01T00:00:00.000Z')", []).unwrap();
        for id in ["h1-11111111", "h2-22222222", "h3-33333333", "h4-44444444"] {
            on_clear(&c, id, "2026-01-01T00:00:00.000Z");
        }
        link_successor(&c, "h3-33333333", "h3n-nnnnnnnn");
        // 살아 있는 후속 세션은 되살림 검사(live_status)에 걸리지 않는다 — 후속 세션은 h3 자신이 아니다
        let r = sweep(&c, far);
        assert!(r.purged.is_empty(), "{r:?}");
        assert_eq!(count(&c, "SELECT COUNT(*) FROM session WHERE clear_state = 'purge'"), 4);
        assert!(r.held >= 4);
        // 후속 프로세스가 끝나고 별표·고정을 풀면 지워진다
        c.execute("UPDATE session SET live_status = NULL, live_pid = NULL WHERE id = 'h3n-nnnnnnnn'", []).unwrap();
        c.execute("UPDATE turn SET starred = 0", []).unwrap();
        c.execute("UPDATE session SET pinned = 0", []).unwrap();
        let r = sweep(&c, far);
        assert!(r.purged.contains(&"h1-11111111".to_string()) && r.purged.contains(&"h2-22222222".to_string()) && r.purged.contains(&"h3-33333333".to_string()));
        assert!(!r.purged.contains(&"h4-44444444".to_string()), "전달 대기 말이 있는 세션은 남긴다");
    }

    #[test]
    fn migration_marks_legacy_cleared_sessions_without_scheduling_deletion() {
        let c = Connection::open_in_memory().unwrap();
        db::migrate(&c).unwrap();
        // v10 모양으로 되돌린다
        c.execute_batch(
            "DROP INDEX session_clear_state;
             ALTER TABLE session DROP COLUMN cleared_at; ALTER TABLE session DROP COLUMN cleared_to; ALTER TABLE session DROP COLUMN clear_state;
             ALTER TABLE session DROP COLUMN purge_at; ALTER TABLE session DROP COLUMN clear_asked; ALTER TABLE session DROP COLUMN kept_at;",
        )
        .unwrap();
        db::set_meta(&c, "schema_version", "10").unwrap();
        c.execute("INSERT INTO session (id, ended_at, end_reason) VALUES ('old-1', '2026-01-01T00:00:00.000Z', 'clear'), ('old-2', '2026-01-01T00:00:00.000Z', 'resume')", []).unwrap();
        db::migrate(&c).unwrap();
        assert_eq!(count(&c, "SELECT COUNT(*) FROM session WHERE clear_state = 'ask' AND purge_at IS NULL AND cleared_at IS NOT NULL"), 1);
        assert_eq!(count(&c, "SELECT COUNT(*) FROM session WHERE cleared_at IS NULL"), 1);
        assert!(sweep(&c, t0() + 4000 * DAY).purged.is_empty(), "소급 삭제 금지");
    }

    #[test]
    fn retention_reads_configured_days_but_never_below_default() {
        let dir = std::env::temp_dir().join(format!("aiinbox-ret-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        std::fs::write(dir.join(".claude/settings.json"), r#"{"cleanupPeriodDays": 3650}"#).unwrap();
        assert!(retention_days(Some(&dir.to_string_lossy())) >= 3650);
        std::fs::write(dir.join(".claude/settings.json"), r#"{"cleanupPeriodDays": 3}"#).unwrap();
        assert!(retention_days(Some(&dir.to_string_lossy())) >= RETENTION_DEFAULT_DAYS);
        std::fs::write(dir.join(".claude/settings.json"), "깨진 json").unwrap();
        assert!(retention_days(Some(&dir.to_string_lossy())) >= RETENTION_DEFAULT_DAYS);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
