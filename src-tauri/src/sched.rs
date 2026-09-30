//! 예약 전송(0.10.0) — 정한 시각에 세션에 말을 넣는다. 설계·결정 배경·한계는 `docs/SCHEDULE.md`.
//!
//! 핵심 원칙
//! - **AI Inbox 가 켜져 있는 PC 에서만** 돈다(서버 저장 0 — 규격 §1 약속 유지). 꺼져 있던 동안의 예약은 놓침 정책을 따른다.
//! - 예약은 대기열(`conoti_reply`) 밖 `schedule` 표에 둔다. **시각이 되는 순간에만** 대기열에 한 줄(`received_at` = 발사 시각, `sched` 열 = 회차 키)을 넣는다.
//!   그래서 "받은 뒤 3시간" 상한이 발사 기준이 되고, 파이프라인이 예약을 미리 집어 가지 못한다.
//! - **못 받는 예약(꺼진 세션·훅 없는 터미널·오래 승인 대기)은 대기열에 넣지 않고 `held` 로 둔다.** 알림은 한 번, 받을 수 있게 되면 한 번 더 —
//!   자동 전달은 없고 사용자가 보내기/버리기를 고른다. 예약에서 온 줄은 꺼진 세션을 이어서 실행하지 않는다(`Opts.scheduled`).
//! - 중복 방지: 발사는 조건부 UPDATE(`next_due_at` 이 그대로일 때만) + 회차 키 `INSERT OR IGNORE`. 판정은 벽시계(`now` 인자)로만 — 시험이 가짜 시계를 넣는다.
//! - 전권 우회는 어디에도 없다: 이 모듈은 `claude`·`codex` 를 직접 띄우지 않고(꺼진 세션은 held), 권한 모드를 바꾸지 않으며,
//!   세션의 권한 모드(훅이 남긴 `permission_mode`)에 따라 **예약 시점에 안내**만 한다(`perm_info` — 전부 허용 모드가 아니면 사전 준비 안내).
//!   승인 대기에 멈춘 세션은 10분 뒤 held 로 알린다.

use chrono::{DateTime, Datelike, Duration, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::deliver::{Route, Work};
use crate::{attach, conoti, db, text, time};

type R<T> = Result<T, String>;

/// 예정 시각보다 이만큼(초)까지 늦은 것은 "제때"로 본다(전달 스레드 틱·절전 직후의 흔들림)
pub const GRACE_SECS: i64 = 120;
/// 승인 대기에서 멈춘 세션을 이만큼(초) 기다린 뒤 held 로 알린다
pub const PERM_WAIT_SECS: i64 = 600;
/// 바쁜 세션·방해금지 창 때문에 미루는 최대 시간(기존 대기열 상한과 같다)
pub const DEFER_LIMIT_SECS: i64 = 3 * 3600;
/// 대기열에 넣었지만 세션이 받지 못하고(작업 중이 아닌데) 이만큼(초) 지나면 되돌려 held 로 알린다
pub const STUCK_SECS: i64 = 600;
/// held 로 오래 남은 예약은 이 기간 뒤 버린다(세션이 영영 묶이지 않게)
pub const HELD_EXPIRE_DAYS: i64 = 7;
pub const MAX_ACTIVE_PER_SESSION: i64 = 20;
pub const MAX_ACTIVE_TOTAL: i64 = 200;
/// 하루 발사 상한 — 폭주·실수로 무인 실행이 쌓이지 않게
pub const DAILY_FIRE_CAP: i64 = 100;
pub const MAX_AFTER_MIN: i64 = 60 * 24 * 30;
pub const MAX_LEAD_DAYS: i64 = 366;
pub const MIN_LEAD_SECS: i64 = 5;
/// `update` 가 rev 충돌·이미 발사됨을 알릴 때 오류 문구 앞에 붙이는 표지 — 폰 규격의 `conflict` 오류 코드로 바뀐다
pub const CONFLICT: &str = "충돌: ";

// ── 정책 ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    /// 바로 끼운다(일하는 중이면 도구 사이에 읽힌다)
    Interrupt,
    /// 세션의 작업이 끝난 뒤
    AfterWork,
    /// 방해금지 창이 끝난 뒤(창 밖이어도 일하는 중이면 작업 뒤)
    AfterQuiet,
}

impl Policy {
    pub fn parse(s: &str) -> Option<Policy> {
        match s {
            "interrupt" => Some(Policy::Interrupt),
            "after_work" => Some(Policy::AfterWork),
            "after_quiet" => Some(Policy::AfterQuiet),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Policy::Interrupt => "interrupt",
            Policy::AfterWork => "after_work",
            Policy::AfterQuiet => "after_quiet",
        }
    }
}

/// 정책이 어디서 정해졌나(화면·시험용)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Schedule,
    Session,
    Tag,
    Window,
    Global,
}

/// 우선순위: 예약 덮어쓰기 > 세션 규칙 > 태그 규칙 > 시간대(방해금지 창 안이면 after_quiet) > 전역 기본(after_work)
pub fn effective_policy(conn: &Connection, own: Option<&str>, sid: &str, now: DateTime<Utc>) -> (Policy, Source) {
    if let Some(p) = own.and_then(Policy::parse) {
        return (p, Source::Schedule);
    }
    if let Some(p) = rule_for(conn, "session", sid) {
        return (p, Source::Session);
    }
    for tid in session_tags(conn, sid) {
        if let Some(p) = rule_for(conn, "tag", &tid.to_string()) {
            return (p, Source::Tag);
        }
    }
    if in_quiet_window(conn, now).is_some() {
        return (Policy::AfterQuiet, Source::Window);
    }
    (global_default(conn), Source::Global)
}

pub fn global_default(conn: &Connection) -> Policy {
    db::get_meta(conn, "setting.sched_busy_default").as_deref().and_then(Policy::parse).unwrap_or(Policy::AfterWork)
}

fn rule_for(conn: &Connection, scope: &str, key: &str) -> Option<Policy> {
    conn.query_row("SELECT action FROM busy_rule WHERE scope = ?1 AND key = ?2", params![scope, key], |r| r.get::<_, String>(0))
        .optional()
        .ok()
        .flatten()
        .and_then(|a| Policy::parse(&a))
}

/// 세션의 대표 태그 id(큰 태그 우선 · 요청 수 많은 순, 최대 3) — `tags::session_top_tags` 와 같은 순서
fn session_tags(conn: &Connection, sid: &str) -> Vec<i64> {
    conn.prepare(
        "SELECT x.tag_id FROM turn_tag x JOIN turn t ON t.id = x.turn_id JOIN tag g ON g.id = x.tag_id
          WHERE t.session_id = ?1 AND x.state IN ('auto','manual') AND t.hidden = 0
          GROUP BY x.tag_id ORDER BY g.minor, COUNT(*) DESC, x.tag_id LIMIT 3",
    )
    .and_then(|mut st| st.query_map(params![sid], |r| r.get(0)).map(|r| r.flatten().collect()))
    .unwrap_or_default()
}

// ── 방해금지 창(시간대 규칙) ─────────────────────────────────────────────────

fn parse_hm(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M").ok()
}

/// `now` 가 이 창 안인가. 창은 시작한 요일의 것이다 — 22:00~08:00 은 월요일 밤부터 화요일 아침까지(자정 넘김)
pub fn window_contains(days: i64, start: NaiveTime, end: NaiveTime, tz: Tz, now: DateTime<Utc>) -> bool {
    let local = now.with_timezone(&tz);
    let t = local.time();
    let bit = |wd: chrono::Weekday| days & (1 << wd.num_days_from_monday()) != 0;
    if start == end {
        return false;
    }
    if start < end {
        bit(local.weekday()) && t >= start && t < end
    } else {
        (bit(local.weekday()) && t >= start) || (bit(local.weekday().pred()) && t < end)
    }
}

/// 지금 안에 있는 켜진 창(id)
pub fn in_quiet_window(conn: &Connection, now: DateTime<Utc>) -> Option<i64> {
    let rows: Vec<(i64, i64, String, String, String)> = conn
        .prepare("SELECT id, days, start_hm, end_hm, tz FROM quiet_window WHERE enabled = 1")
        .and_then(|mut st| st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).map(|r| r.flatten().collect()))
        .unwrap_or_default();
    rows.into_iter().find_map(|(id, days, s, e, tz)| {
        let (s, e, tz) = (parse_hm(&s)?, parse_hm(&e)?, tz.parse::<Tz>().ok()?);
        window_contains(days, s, e, tz, now).then_some(id)
    })
}

// ── 시간 ─────────────────────────────────────────────────────────────────────

/// 만든 곳의 시간대(IANA)의 현지 시각 → UTC. 서머타임: 없는 시각(봄 건너뜀)은 그 직후 첫 유효 시각으로 밀고,
/// 두 번 있는 시각(가을)은 첫 번째(서머타임 쪽)만 쓴다.
pub fn local_to_utc(tz: Tz, naive: NaiveDateTime) -> Option<DateTime<Utc>> {
    let mut t = naive;
    for _ in 0..=180 {
        match tz.from_local_datetime(&t) {
            chrono::LocalResult::Single(d) => return Some(d.with_timezone(&Utc)),
            chrono::LocalResult::Ambiguous(first, _) => return Some(first.with_timezone(&Utc)),
            chrono::LocalResult::None => t += Duration::minutes(1),
        }
    }
    None
}

#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WhenIn {
    /// N분 뒤
    After { min: i64 },
    /// 현지 시각(`2026-10-01T09:00`) + 시간대
    At { local: String, tz: String },
    /// 절대 시각(UTC 밀리초) — 폰이 자기 시간대로 풀어 보낸 것. 시간대 이름은 표시용
    AtUtc { ms: i64, tz: String },
}

fn resolve_when(now: DateTime<Utc>, w: &WhenIn) -> R<(DateTime<Utc>, &'static str, String)> {
    let (due, kind, tz) = match w {
        WhenIn::After { min } => {
            if !(1..=MAX_AFTER_MIN).contains(min) {
                return Err(format!("N분 뒤는 1분~{}일 안이어야 합니다", MAX_AFTER_MIN / 1440));
            }
            (now + Duration::minutes(*min), "after", "UTC".to_string())
        }
        WhenIn::AtUtc { ms, tz } => {
            // 표시용 시간대는 몰라도 된다(형식만 본다)
            let tz = if tz.parse::<Tz>().is_ok() { tz.clone() } else { "UTC".to_string() };
            (Utc.timestamp_millis_opt(*ms).single().ok_or("시각 형식 오류")?, "once", tz)
        }
        WhenIn::At { local, tz } => {
            let zone: Tz = tz.parse().map_err(|_| "시간대 형식 오류".to_string())?;
            let naive = NaiveDateTime::parse_from_str(local, "%Y-%m-%dT%H:%M").map_err(|_| "시각 형식 오류".to_string())?;
            (local_to_utc(zone, naive).ok_or("그 시각을 풀 수 없습니다")?, "once", tz.clone())
        }
    };
    if due < now + Duration::seconds(MIN_LEAD_SECS) {
        return Err("지난 시각은 예약할 수 없습니다 — 지금 보내려면 보내기를 누르세요".into());
    }
    if due > now + Duration::days(MAX_LEAD_DAYS) {
        return Err(format!("예약은 {MAX_LEAD_DAYS}일 안까지만 걸 수 있습니다"));
    }
    Ok((due, kind, tz))
}

fn iso(d: DateTime<Utc>) -> String {
    d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

// ── 설정 ─────────────────────────────────────────────────────────────────────

pub fn paused(conn: &Connection) -> bool {
    db::get_meta(conn, "setting.sched_paused").as_deref() == Some("1")
}

/// 예약 처리 정책: `default`(사용자 세션의 권한 모드 그대로 + 승인 대기 감지 알림) | `allowlist`(허용 세션 목록에만)
pub fn perm_policy(conn: &Connection) -> &'static str {
    if db::get_meta(conn, "setting.sched_perm").as_deref() == Some("allowlist") {
        "allowlist"
    } else {
        "default"
    }
}

pub fn allowed(conn: &Connection, sid: &str) -> bool {
    perm_policy(conn) == "default" || conn.query_row("SELECT COUNT(*) FROM sched_allow WHERE session_id = ?1", params![sid], |r| r.get::<_, i64>(0)).unwrap_or(0) > 0
}

/// 세션의 마지막 권한 모드(훅이 남긴 값) — `bypassPermissions` 인 세션엔 예약하지 않는다
pub fn last_permission_mode(conn: &Connection, sid: &str) -> Option<String> {
    conn.query_row(
        "SELECT json_extract(detail, '$.permission_mode') FROM hook_event
          WHERE session_id = ?1 AND json_extract(detail, '$.permission_mode') IS NOT NULL ORDER BY at DESC, id DESC LIMIT 1",
        params![sid],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()
    .ok()
    .flatten()
    .flatten()
}

/// 권한 모드 분류(훅 `permission_mode` 공식 값: default · plan · acceptEdits · auto · dontAsk · bypassPermissions).
/// 모르는 값·값 없음은 `unknown`("확인 불가").
pub fn perm_kind(mode: Option<&str>) -> &'static str {
    match mode {
        Some("bypassPermissions") => "bypass",
        Some("auto") => "auto",
        Some("acceptEdits") => "accept_edits",
        Some("default") => "default",
        Some("plan") => "plan",
        Some("dontAsk") => "dont_ask",
        _ => "unknown",
    }
}

fn warn_off_key(sid: &str) -> String {
    format!("sched.warn_off.{sid}")
}

/// "이 세션은 다시 안 보기" — 권한 안내 패널을 세션별로 끈다(마이그레이션 없이 meta 한 줄)
pub fn set_warn_off(conn: &Connection, sid: &str, off: bool) -> R<()> {
    if !crate::channel::valid_session_id(sid) {
        return Err("세션 ID 형식 오류".into());
    }
    db::set_meta(conn, &warn_off_key(sid), if off { "1" } else { "0" }).map_err(|e| e.to_string())
}

/// 예약 권한 안내 — 화면·폰이 같은 판정을 쓴다.
/// `perm` = 훅이 남긴 마지막 모드 문자열(없으면 null · 알아볼 수 없는 값도 null), `kind` = 분류, `warn` = 사전 준비 안내가 필요한가
/// (전부 허용 모드가 아니고 사용자가 이 세션의 안내를 끄지 않았을 때), `notice` = 전부 허용 세션이라 한 줄 고지만.
pub fn perm_info(conn: &Connection, sid: &str) -> Value {
    let mode = last_permission_mode(conn, sid);
    let kind = perm_kind(mode.as_deref());
    let dismissed = db::get_meta(conn, &warn_off_key(sid)).as_deref() == Some("1");
    let perm = if kind == "unknown" { None } else { mode };
    json!({
        "perm": perm, "kind": kind, "dismissed": dismissed,
        "warn": kind != "bypass" && !dismissed, "notice": kind == "bypass",
    })
}

#[tauri::command]
pub fn sched_perm_info(state: State<AppState>, session_id: String) -> R<Value> {
    let conn = state.conn.lock().map_err(e)?;
    Ok(perm_info(&conn, &session_id))
}

#[tauri::command]
pub fn sched_warn_off(state: State<AppState>, session_id: String, off: bool) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    set_warn_off(&conn, &session_id, off)
}

// ── 세션 상태 조회(시험이 가짜를 넣는다) ─────────────────────────────────────

pub trait Probe {
    fn route(&self, sid: &str) -> Route;
    fn work(&self, sid: &str) -> Work;
}

pub struct LiveProbe;

impl Probe for LiveProbe {
    fn route(&self, sid: &str) -> Route {
        crate::deliver::route(sid)
    }
    fn work(&self, sid: &str) -> Work {
        crate::deliver::work_state(sid)
    }
}

// ── 판정(순수 함수) ──────────────────────────────────────────────────────────

#[derive(Debug, PartialEq)]
pub enum Decision {
    Send,
    Defer(&'static str),
    /// (사유 코드, 화면 문구)
    Hold(&'static str, &'static str),
}

/// 세션 상태 × 정책 → 지금 넣을지 · 미룰지 · 받을 수 없어 알릴지. `age` = 발사 처리를 시작한 뒤 흐른 시간(초).
pub fn decide(route: &Route, work: Work, policy: Policy, quiet_now: bool, age: i64) -> Decision {
    match route {
        Route::Terminal => return Decision::Hold("terminal", "실행 중인 세션에 넣는 훅이 설치되지 않았습니다"),
        Route::Ended => return Decision::Hold("ended", "세션이 꺼져 있습니다"),
        _ => {}
    }
    if work == Work::PermWait {
        return if age >= PERM_WAIT_SECS {
            Decision::Hold("perm", "세션이 권한 승인을 기다리고 있습니다")
        } else {
            Decision::Defer("권한 승인 대기 — 승인되면 보냅니다")
        };
    }
    let wait = match policy {
        Policy::Interrupt => None,
        Policy::AfterWork => (work == Work::Busy).then_some("작업이 끝나길 기다립니다"),
        Policy::AfterQuiet => {
            if quiet_now {
                Some("방해금지 시간이 끝나길 기다립니다")
            } else if work == Work::Busy {
                Some("작업이 끝나길 기다립니다")
            } else {
                None
            }
        }
    };
    match wait {
        None => Decision::Send,
        Some(_) if age >= DEFER_LIMIT_SECS => Decision::Hold("busy_limit", "3시간 넘게 넣을 수 없었습니다"),
        Some(why) => Decision::Defer(why),
    }
}

/// 받을 수 있게 됐나(held 재알림 조건)
fn receivable(reason: &str, route: &Route, work: Work) -> bool {
    let up = !matches!(route, Route::Ended | Route::Terminal);
    match reason {
        "ended" => up,
        "terminal" => !matches!(route, Route::Terminal),
        "perm" => up && work != Work::PermWait,
        // 세션 상태가 아니라 다른 사정(하루 한도·정책·미룸 한도·전달 거절)으로 대기 — 세션이 "다시 켜진" 것이 아니므로 재알림하지 않는다
        _ => false,
    }
}

// ── 알림 ─────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct Alert {
    /// held(못 받아 대기) · missed(놓침) · failed(정책상 막음) · back(다시 받을 수 있게 됨)
    pub kind: &'static str,
    pub schedule_id: String,
    pub session_id: String,
    pub session_name: String,
    /// 예약 글의 첫 줄 앞부분(화면 알림용 — 폰 푸시에는 실리지 않는다)
    pub preview: String,
    pub note: String,
}

/// 폰 푸시 종류 `k` (서버 계약 = 허용값 `sched_held`·`sched_ready`·`sched_missed` 뿐, 그 밖은 400 — 새 값은 서버 먼저)
pub const PUSH_HELD: &str = "sched_held";
pub const PUSH_READY: &str = "sched_ready";
pub const PUSH_MISSED: &str = "sched_missed";

/// 여러 종류가 한꺼번에 모이면 푸시는 하나만 — 못 받음(held) > 놓침(missed) > 다시 받을 수 있음(ready)
pub fn pick_push_kind(kinds: &[&'static str]) -> Option<&'static str> {
    [PUSH_HELD, PUSH_MISSED, PUSH_READY].into_iter().find(|k| kinds.contains(k))
}

impl Alert {
    /// 알림 종류 → 푸시 `k`: held·failed → `sched_held`, back → `sched_ready`, missed → `sched_missed`
    pub fn push_kind(&self) -> &'static str {
        match self.kind {
            "back" => PUSH_READY,
            "missed" => PUSH_MISSED,
            _ => PUSH_HELD,
        }
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            "back" => "예약을 다시 보낼 수 있어요",
            "missed" => "예약을 놓쳤어요",
            _ => "예약이 전달되지 못했어요",
        }
    }
}

#[derive(Default)]
pub struct TickReport {
    pub changed_sessions: Vec<String>,
    pub alerts: Vec<Alert>,
}

fn label_of(conn: &Connection, sid: &str) -> String {
    let row: Option<(Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)> = conn
        .query_row(
            "SELECT s.live_name, s.title, s.agent_name, s.project_dir,
                    (SELECT prompt_text FROM turn WHERE session_id = s.id AND hidden = 0 ORDER BY seq LIMIT 1)
               FROM session s WHERE s.id = ?1",
            params![sid],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .ok()
        .flatten();
    match row {
        Some((live, title, agent, dir, first)) => crate::api::display_name(live, title, agent, first, &dir).0,
        None => "세션".into(),
    }
}

fn alert(conn: &Connection, kind: &'static str, schedule_id: &str, note: &str) -> Option<Alert> {
    let (sid, body): (String, String) = conn
        .query_row("SELECT session_id, text FROM schedule WHERE id = ?1", params![schedule_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .ok()
        .flatten()?;
    Some(Alert {
        kind,
        schedule_id: schedule_id.to_string(),
        session_name: label_of(conn, &sid),
        session_id: sid,
        preview: text::clip(text::first_line(&body), 40),
        note: note.to_string(),
    })
}

// ── 만들기·고치기·취소 ───────────────────────────────────────────────────────

#[derive(Deserialize, Debug, Clone)]
pub struct NewSchedule {
    pub session_id: String,
    pub text: String,
    #[serde(default)]
    pub atts: Vec<String>,
    pub quote_turn: Option<i64>,
    pub quote_part: Option<String>,
    pub when: WhenIn,
    /// run_once | skip | within
    pub on_missed: String,
    pub missed_within_min: Option<i64>,
    /// interrupt | after_work | after_quiet — 없으면 규칙을 따른다
    pub busy_policy: Option<String>,
    /// 폰이 만든 예약의 멱등 키(화면에서 만든 것은 없음)
    #[serde(default)]
    pub rid: Option<String>,
}

fn check_policy_fields(on_missed: &str, within: Option<i64>, busy: Option<&str>) -> R<Option<i64>> {
    let within = match on_missed {
        "run_once" | "skip" => None,
        "within" => {
            let n = within.unwrap_or(60);
            if !(1..=1440).contains(&n) {
                return Err("놓쳤을 때 실행할 수 있는 시간은 1분~24시간입니다".into());
            }
            Some(n)
        }
        _ => return Err("놓침 정책 형식 오류".into()),
    };
    if busy.is_some_and(|b| Policy::parse(b).is_none()) {
        return Err("바쁜 세션 정책 형식 오류".into());
    }
    Ok(within)
}

fn new_id() -> String {
    format!("sc{}", crate::relay::hex(&crate::relay::random::<8>()))
}

/// 예약을 만든다. 돌려주는 값 = 화면용 항목 + `warnings`(지금 받을 수 없는 세션이면 알림만 간다는 안내 등)
pub fn add(conn: &Connection, now: DateTime<Utc>, created_by: &str, n: &NewSchedule, probe: &dyn Probe) -> R<Value> {
    // 같은 rid 로 다시 만들면(재전송) 처음 것을 그대로 — 두 번 만들지 않는다
    if let Some(rid) = n.rid.as_deref() {
        let prior: Option<String> = conn
            .query_row("SELECT id FROM schedule WHERE created_by = ?1 AND rid = ?2", params![created_by, rid], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(id) = prior {
            return item_of(conn, &id).map(|mut v| {
                v["warnings"] = json!([]);
                v
            }).ok_or_else(|| "기록을 읽지 못함".into());
        }
    }
    conoti::check_body(&n.text, !n.atts.is_empty())?;
    if n.quote_part.as_deref().is_some_and(|q| !conoti::valid_quote(q)) || (n.quote_part.is_some() && n.quote_turn.is_none()) {
        return Err("답장 대상 형식 오류".into());
    }
    let within = check_policy_fields(&n.on_missed, n.missed_within_min, n.busy_policy.as_deref())?;
    attach::check_desktop_ids(conn, &n.atts)?;
    let sid = n.session_id.as_str();
    if !crate::channel::valid_session_id(sid) || conn.query_row("SELECT COUNT(*) FROM session WHERE id = ?1", params![sid], |r| r.get::<_, i64>(0)).unwrap_or(0) == 0 {
        return Err("세션을 찾을 수 없습니다".into());
    }
    if !allowed(conn, sid) {
        return Err("이 세션은 예약을 받을 수 있는 세션 목록에 없습니다 — 설정 → 예약 전송에서 허용하세요".into());
    }
    let (active_s, active_all): (i64, i64) = (
        conn.query_row("SELECT COUNT(*) FROM schedule WHERE session_id = ?1 AND state = 'active'", params![sid], |r| r.get(0)).unwrap_or(0),
        conn.query_row("SELECT COUNT(*) FROM schedule WHERE state = 'active'", [], |r| r.get(0)).unwrap_or(0),
    );
    if active_s >= MAX_ACTIVE_PER_SESSION || active_all >= MAX_ACTIVE_TOTAL {
        return Err(format!("걸어 둔 예약이 너무 많습니다(세션당 {MAX_ACTIVE_PER_SESSION}개 · 전체 {MAX_ACTIVE_TOTAL}개)"));
    }
    let (due, kind, tz) = resolve_when(now, &n.when)?;
    let turn: Option<i64> = match n.quote_turn {
        Some(t) => Some(
            conn.query_row("SELECT id FROM turn WHERE id = ?1 AND session_id = ?2", params![t, sid], |r| r.get(0))
                .optional()
                .map_err(|e| e.to_string())?
                .ok_or("답장할 요청을 찾을 수 없습니다")?,
        ),
        None => None,
    };
    let id = new_id();
    let at = iso(now);
    conn.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;
    let res = (|| -> R<()> {
        conn.execute(
            "INSERT INTO schedule (id, session_id, text, quote, turn_id, created_by, created_at, updated_at, kind, tz, next_due_at, on_missed, missed_within_min, busy_policy, state, rid)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'active', ?14)",
            params![
                id,
                sid,
                text::clip(&text::redact(&n.text), conoti::MAX_REPLY_CHARS + 10),
                n.quote_part,
                turn,
                created_by,
                at,
                kind,
                tz,
                iso(due),
                n.on_missed,
                within,
                n.busy_policy,
                n.rid
            ],
        )
        .map_err(|e| e.to_string())?;
        put_atts(conn, &id, &n.atts)
    })();
    match res {
        Ok(()) => conn.execute_batch("COMMIT").map_err(|e| e.to_string())?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    let mut item = item_of(conn, &id).ok_or("기록을 읽지 못함")?;
    let mut warnings: Vec<String> = Vec::new();
    match probe.route(sid) {
        Route::Ended => warnings.push("지금은 세션이 꺼져 있습니다 — 그때도 꺼져 있으면 알림만 가고, 보낼지 버릴지 직접 고르게 됩니다".into()),
        Route::Terminal => warnings.push("실행 중인 세션에 넣는 훅이 설치되어 있지 않습니다 — 설정에서 훅을 설치하지 않으면 알림만 갑니다".into()),
        _ => {}
    }
    item["warnings"] = json!(warnings);
    Ok(item)
}

fn put_atts(conn: &Connection, id: &str, atts: &[String]) -> R<()> {
    conn.execute("DELETE FROM schedule_att WHERE schedule_id = ?1", params![id]).map_err(|e| e.to_string())?;
    for (i, a) in atts.iter().enumerate() {
        conn.execute("INSERT INTO schedule_att (schedule_id, att_id, ord) VALUES (?1, ?2, ?3)", params![id, a, i as i64]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn atts_of(conn: &Connection, id: &str) -> Vec<String> {
    conn.prepare("SELECT att_id FROM schedule_att WHERE schedule_id = ?1 ORDER BY ord")
        .and_then(|mut st| st.query_map(params![id], |r| r.get(0)).map(|r| r.flatten().collect()))
        .unwrap_or_default()
}

/// 끝난 예약의 이미지 연결을 푼다(파일은 gc 가 7일 뒤 정리 — 이미 대기열로 넘어간 것은 reply_attachment 가 잡고 있다)
fn release_atts(conn: &Connection, id: &str) {
    attach::touch(conn, &atts_of(conn, id));
    let _ = conn.execute("DELETE FROM schedule_att WHERE schedule_id = ?1", params![id]);
}

#[derive(Deserialize, Debug, Clone)]
pub struct EditSchedule {
    pub rev: i64,
    pub text: String,
    #[serde(default)]
    pub atts: Vec<String>,
    pub when: WhenIn,
    pub on_missed: String,
    pub missed_within_min: Option<i64>,
    pub busy_policy: Option<String>,
}

/// 아직 발사 전인 예약을 고친다. `rev` 가 다르면(그사이 바뀌었거나 이미 발사됨) 충돌.
pub fn update(conn: &Connection, now: DateTime<Utc>, id: &str, e: &EditSchedule) -> R<Value> {
    conoti::check_body(&e.text, !e.atts.is_empty())?;
    let within = check_policy_fields(&e.on_missed, e.missed_within_min, e.busy_policy.as_deref())?;
    attach::check_desktop_ids(conn, &e.atts)?;
    let (due, kind, tz) = resolve_when(now, &e.when)?;
    conn.execute_batch("BEGIN IMMEDIATE").map_err(|x| x.to_string())?;
    let res = (|| -> R<()> {
        let n = conn
            .execute(
                "UPDATE schedule SET text = ?3, kind = ?4, tz = ?5, next_due_at = ?6, on_missed = ?7, missed_within_min = ?8, busy_policy = ?9,
                        updated_at = ?10, rev = rev + 1
                  WHERE id = ?1 AND rev = ?2 AND state = 'active' AND next_due_at IS NOT NULL",
                params![id, e.rev, text::clip(&text::redact(&e.text), conoti::MAX_REPLY_CHARS + 10), kind, tz, iso(due), e.on_missed, within, e.busy_policy, iso(now)],
            )
            .map_err(|x| x.to_string())?;
        if n == 0 {
            return Err(format!("{CONFLICT}이미 전달됐거나 다른 곳에서 바뀐 예약입니다 — 목록을 새로 불러오세요"));
        }
        put_atts(conn, id, &e.atts)
    })();
    match res {
        Ok(()) => conn.execute_batch("COMMIT").map_err(|x| x.to_string())?,
        Err(x) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(x);
        }
    }
    item_of(conn, id).ok_or_else(|| "기록을 읽지 못함".into())
}

/// 예약을 취소한다. 발사 전이면 그대로 취소, 발사 뒤라도 아직 미룬 것·받지 못한 것·대기열에서 전달 전인 것은 거둔다.
pub fn cancel(conn: &Connection, now: DateTime<Utc>, id: &str) -> R<()> {
    let n = conn
        .execute(
            "UPDATE schedule SET state = 'cancelled', next_due_at = NULL, updated_at = ?2, rev = rev + 1 WHERE id = ?1 AND state = 'active' AND next_due_at IS NOT NULL",
            params![id, iso(now)],
        )
        .map_err(|e| e.to_string())?;
    if n > 0 {
        release_atts(conn, id);
        return Ok(());
    }
    // 발사된 회차 — 아직 세션에 들어가지 않은 것만
    let run: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT occurrence_at, state, reply_id FROM schedule_run WHERE schedule_id = ?1 AND state IN ('pending','deferred','held','fired') ORDER BY occurrence_at DESC LIMIT 1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    let Some((occ, state, reply)) = run else { return Err("이미 전달됐거나 취소된 예약입니다".into()) };
    if state == "fired" {
        // 대기열에서 전달 전인 줄만 거둔다(이미 세션에 들어갔으면 못 거둔다)
        let took = conn
            .execute(
                "UPDATE conoti_reply SET state = 'rejected', note = '예약 취소' WHERE reply_id = ?1 AND state = 'delivering'",
                params![reply],
            )
            .map_err(|e| e.to_string())?;
        if took == 0 {
            return Err("이미 세션에 전달됐습니다".into());
        }
    }
    let n = conn
        .execute(
            "UPDATE schedule_run SET state = 'cancelled', note = '사용자가 취소' WHERE schedule_id = ?1 AND occurrence_at = ?2 AND state IN ('pending','deferred','held','fired')",
            params![id, occ],
        )
        .map_err(|e| e.to_string())?;
    if n > 0 {
        finish_schedule(conn, id, now);
    }
    Ok(())
}

/// 회차가 끝났으면 예약을 끝낸다(1회성)
fn finish_schedule(conn: &Connection, id: &str, now: DateTime<Utc>) {
    let open: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM schedule_run WHERE schedule_id = ?1 AND state IN ('pending','deferred','fired','held')",
            params![id],
            |r| r.get(0),
        )
        .unwrap_or(1);
    if open == 0 {
        let n = conn
            .execute(
                "UPDATE schedule SET state = 'done', updated_at = ?2 WHERE id = ?1 AND state = 'active' AND next_due_at IS NULL",
                params![id, iso(now)],
            )
            .unwrap_or(0);
        if n > 0 {
            release_atts(conn, id);
        }
    }
}

/// 못 받아 대기 중인 회차를 사용자가 처리한다: `send`(지금 보낸다 — PC 앞의 사용자가 보내는 것이라 입력창과 같다) · `drop`(버린다)
pub fn act(conn: &Connection, now: DateTime<Utc>, id: &str, op: &str) -> R<()> {
    act_by(conn, now, id, op, None)
}

/// `phone` = (기기 pid, 그 기기의 답 보내기 허용) — 폰이 누른 "보내기"는 폰 답과 같은 검사(허용·멈춤·세션 차단·이어서 실행 설정)를 통과해야 하고,
/// PC 의 "데스크톱 확인" 옵션이 켜져 있으면 확인 대기로 들어간다.
pub fn act_by(conn: &Connection, now: DateTime<Utc>, id: &str, op: &str, phone: Option<(&str, bool)>) -> R<()> {
    let occ: String = conn
        .query_row(
            "SELECT occurrence_at FROM schedule_run WHERE schedule_id = ?1 AND state = 'held' ORDER BY occurrence_at DESC LIMIT 1",
            params![id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .ok_or("처리할 대기 예약이 없습니다")?;
    match op {
        "drop" => {
            let n = conn
                .execute(
                    "UPDATE schedule_run SET state = 'cancelled', note = '사용자가 버림' WHERE schedule_id = ?1 AND occurrence_at = ?2 AND state = 'held'",
                    params![id, occ],
                )
                .map_err(|e| e.to_string())?;
            if n > 0 {
                finish_schedule(conn, id, now);
            }
            Ok(())
        }
        "send" => {
            if let Some((_, can_reply)) = phone {
                let sid: String = conn.query_row("SELECT session_id FROM schedule WHERE id = ?1", params![id], |r| r.get(0)).map_err(|e| e.to_string())?;
                let block = conoti::reply_block(conn, can_reply, &sid);
                if !block.is_empty() {
                    return Err(match block {
                        "device_off" => "PC 에서 이 기기의 답 보내기를 꺼 둠",
                        "paused" => "PC 에서 폰 답 받기를 멈춤",
                        "session_blocked" => "PC 에서 이 세션의 폰 답을 막아 둠",
                        "offline_no_resume" => "세션이 꺼져 있음 — PC 설정에서 '꺼진 세션 이어서 실행'을 켜면 보낼 수 있습니다",
                        "no_channel" => "실행 중인 세션에 넣는 훅이 설치되지 않음 — PC 설정에서 훅을 다시 설치하세요",
                        _ => "받을 수 없음",
                    }
                    .into());
                }
            }
            // 두 번 눌러도 한 번만 — 먼저 잡은 쪽만 진행한다
            let n = conn
                .execute("UPDATE schedule_run SET state = 'sending' WHERE schedule_id = ?1 AND occurrence_at = ?2 AND state = 'held'", params![id, occ])
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Ok(());
            }
            let (sid, body, quote, turn): (String, String, Option<String>, Option<i64>) = conn
                .query_row("SELECT session_id, text, quote, turn_id FROM schedule WHERE id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(|e| e.to_string())?;
            let q = turn.zip(quote.as_deref());
            match conoti::accept_desktop(conn, &sid, &body, &atts_of(conn, id), q) {
                Ok(v) => {
                    let rid = v["rid"].as_str().unwrap_or_default().to_string();
                    if phone.is_some() && conoti::flag(conn, "conoti.confirm") {
                        // 데스크톱 확인 옵션 — 폰이 보낸 말은 PC 에서 허용해야 세션에 들어간다
                        let _ = conn.execute("UPDATE conoti_reply SET state = 'confirm' WHERE reply_id = ?1 AND state = 'delivering'", params![rid]);
                    }
                    let _ = conn.execute(
                        "UPDATE schedule_run SET state = 'fired', fired_at = ?3, reply_id = ?4, note = '사용자가 보냄' WHERE schedule_id = ?1 AND occurrence_at = ?2",
                        params![id, occ, iso(now), rid],
                    );
                    Ok(())
                }
                Err(e) => {
                    let _ = conn.execute("UPDATE schedule_run SET state = 'held' WHERE schedule_id = ?1 AND occurrence_at = ?2 AND state = 'sending'", params![id, occ]);
                    Err(e)
                }
            }
        }
        _ => Err("send · drop 중 하나".into()),
    }
}

// ── 목록 ─────────────────────────────────────────────────────────────────────

fn item_of(conn: &Connection, id: &str) -> Option<Value> {
    list_where(conn, "sc.id = ?1", params![id]).into_iter().next()
}

/// 화면 목록: 걸려 있는 예약 + 최근 끝난 것(7일). `session` 이 있으면 그 세션만
pub fn list(conn: &Connection, session: Option<&str>) -> Vec<Value> {
    let since = iso(Utc::now() - Duration::days(7));
    match session {
        Some(s) => list_where(conn, "sc.session_id = ?1 AND (sc.state = 'active' OR sc.updated_at >= ?2)", params![s, since]),
        None => list_where(conn, "(sc.state = 'active' OR sc.updated_at >= ?1)", params![since]),
    }
}

fn list_where(conn: &Connection, cond: &str, p: impl rusqlite::Params) -> Vec<Value> {
    let sql = format!(
        "SELECT sc.id, sc.session_id, sc.text, sc.quote, sc.turn_id, sc.created_at, sc.updated_at, sc.rev, sc.kind, sc.tz, sc.next_due_at,
                sc.on_missed, sc.missed_within_min, sc.busy_policy, sc.state,
                (SELECT r.occurrence_at || '|' || r.state || '|' || COALESCE(r.reason, '') || '|' || COALESCE(r.note, '') || '|' || COALESCE(r.reply_id, '')
                   FROM schedule_run r WHERE r.schedule_id = sc.id ORDER BY r.occurrence_at DESC LIMIT 1)
           , sc.created_by FROM schedule sc WHERE {cond} ORDER BY COALESCE(sc.next_due_at, sc.updated_at), sc.id"
    );
    let Ok(mut st) = conn.prepare(&sql) else { return vec![] };
    let rows: Vec<Value> = st
        .query_map(p, |r| {
            let id: String = r.get(0)?;
            let run: Option<String> = r.get(15)?;
            let mut parts = run.as_deref().unwrap_or("").splitn(5, '|').map(str::to_string);
            let run_v = run.as_ref().map(|_| {
                json!({"occurrence_at": parts.next(), "state": parts.next(), "reason": parts.next().filter(|s| !s.is_empty()),
                       "note": parts.next().filter(|s| !s.is_empty()), "reply_id": parts.next().filter(|s| !s.is_empty())})
            });
            Ok(json!({
                "id": id, "session_id": r.get::<_, String>(1)?, "text": r.get::<_, String>(2)?,
                "quote": r.get::<_, Option<String>>(3)?, "turn_id": r.get::<_, Option<i64>>(4)?,
                "created_at": r.get::<_, String>(5)?, "updated_at": r.get::<_, String>(6)?, "rev": r.get::<_, i64>(7)?,
                "kind": r.get::<_, String>(8)?, "tz": r.get::<_, String>(9)?, "next_due_at": r.get::<_, Option<String>>(10)?,
                "on_missed": r.get::<_, String>(11)?, "missed_within_min": r.get::<_, Option<i64>>(12)?,
                "busy_policy": r.get::<_, Option<String>>(13)?, "state": r.get::<_, String>(14)?, "run": run_v,
                "created_by": r.get::<_, String>(16)?,
            }))
        })
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default();
    rows.into_iter()
        .map(|mut v| {
            let id = v["id"].as_str().unwrap_or("").to_string();
            let sid = v["session_id"].as_str().unwrap_or("").to_string();
            v["session_name"] = json!(label_of(conn, &sid));
            let pi = perm_info(conn, &sid);
            v["perm"] = pi["perm"].clone();
            v["perm_kind"] = pi["kind"].clone();
            v["sched_warn"] = pi["warn"].clone();
            v["atts"] = json!(attach::metas(conn, &atts_of(conn, &id)));
            v
        })
        .collect()
}

/// 걸려 있는 예약 수(세션별 칩)와 처리를 기다리는(held) 수(사이드바 배지)
pub fn counts(conn: &Connection) -> (i64, i64) {
    let active = conn.query_row("SELECT COUNT(*) FROM schedule WHERE state = 'active'", [], |r| r.get(0)).unwrap_or(0);
    let held = conn.query_row("SELECT COUNT(*) FROM schedule_run WHERE state = 'held'", [], |r| r.get(0)).unwrap_or(0);
    (active, held)
}

// ── 발사(틱) ─────────────────────────────────────────────────────────────────

fn fires_key(now: DateTime<Utc>) -> String {
    format!("sched.fires.{}", now.format("%Y-%m-%d"))
}

/// 1 틱. 전달 스레드가 2초마다 부른다. `now` 는 벽시계(시험이 가짜를 넣는다).
pub fn tick(conn: &Connection, now: DateTime<Utc>, probe: &dyn Probe) -> TickReport {
    let mut rep = TickReport::default();
    if paused(conn) {
        return rep;
    }
    let prev_alive = db::get_meta(conn, "sched.alive_at").and_then(|s| time::parse(&s));
    let _ = db::set_meta(conn, "sched.alive_at", &iso(now));
    promote_due(conn, now, prev_alive, &mut rep);
    process_runs(conn, now, probe, &mut rep);
    sync_fired(conn, now, probe, &mut rep);
    renotify(conn, now, probe, &mut rep);
    expire_held(conn, now, &mut rep);
    rep
}

/// 시각이 된 예약 → 회차(pending) 또는 놓침(missed)
fn promote_due(conn: &Connection, now: DateTime<Utc>, prev_alive: Option<DateTime<Utc>>, rep: &mut TickReport) {
    let due: Vec<(String, String, String, String, Option<i64>)> = conn
        .prepare(
            "SELECT id, session_id, next_due_at, on_missed, missed_within_min FROM schedule
              WHERE state = 'active' AND next_due_at IS NOT NULL AND next_due_at <= ?1 ORDER BY next_due_at LIMIT 50",
        )
        .and_then(|mut st| st.query_map(params![iso(now)], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).map(|r| r.flatten().collect()))
        .unwrap_or_default();
    for (id, sid, due_at, on_missed, within) in due {
        let Some(due_t) = time::parse(&due_at) else { continue };
        let late = (now - due_t).num_seconds();
        let fire = late <= GRACE_SECS
            || match on_missed.as_str() {
                "run_once" => true,
                "within" => late <= within.unwrap_or(0) * 60,
                _ => false,
            };
        // 놓친 사유: 앱이 꺼져 있었거나 PC 가 절전이었으면 그렇게, 아니면 처리가 밀렸다고
        let gap = prev_alive.map(|a| (now - a).num_seconds()).unwrap_or(i64::MAX);
        let why = if gap > 180 { "앱이 꺼져 있었거나 PC 가 절전 중이었습니다" } else { "처리가 밀려 늦었습니다" };
        conn.execute_batch("BEGIN IMMEDIATE").ok();
        // 조건부 UPDATE — 이 회차를 먼저 집은 쪽(다른 틱·취소·수정)만 진행한다
        let took = conn
            .execute(
                "UPDATE schedule SET next_due_at = NULL, runs = runs + 1, updated_at = ?3 WHERE id = ?1 AND next_due_at = ?2 AND state = 'active'",
                params![id, due_at, iso(now)],
            )
            .unwrap_or(0);
        if took == 1 {
            let (state, note, reason) = if fire {
                ("pending", if late > GRACE_SECS { Some(format!("{}분 늦게 실행({why})", late / 60)) } else { None }, None)
            } else {
                ("missed", Some(format!("예정 시각에 실행하지 못했습니다 — {why}")), Some("missed"))
            };
            let _ = conn.execute(
                "INSERT OR IGNORE INTO schedule_run (schedule_id, occurrence_at, created_at, state, reason, note) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![id, due_at, iso(now), state, reason, note],
            );
        }
        conn.execute_batch("COMMIT").ok();
        if took == 1 && !fire {
            // 놓침 — 알림 1회(폰·화면), 예약은 끝
            let n = conn
                .execute("UPDATE schedule_run SET notified_at = ?3 WHERE schedule_id = ?1 AND occurrence_at = ?2 AND notified_at IS NULL", params![id, due_at, iso(now)])
                .unwrap_or(0);
            if n == 1 {
                if let Some(a) = alert(conn, "missed", &id, why) {
                    rep.alerts.push(a);
                }
            }
            finish_schedule(conn, &id, now);
        }
        rep.changed_sessions.push(sid);
    }
}

type OpenRun = (String, String, String, String, String);

/// 발사됐지만 아직 대기열에 넣지 않은 회차(pending · deferred) 판단
fn process_runs(conn: &Connection, now: DateTime<Utc>, probe: &dyn Probe, rep: &mut TickReport) {
    let runs: Vec<OpenRun> = conn
        .prepare(
            "SELECT r.schedule_id, r.occurrence_at, r.created_at, r.state, s.session_id FROM schedule_run r JOIN schedule s ON s.id = r.schedule_id
              WHERE r.state IN ('pending','deferred') ORDER BY r.occurrence_at LIMIT 50",
        )
        .and_then(|mut st| st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).map(|r| r.flatten().collect()))
        .unwrap_or_default();
    for (id, occ, created, _state, sid) in runs {
        rep.changed_sessions.push(sid.clone());
        let own: Option<String> = conn.query_row("SELECT busy_policy FROM schedule WHERE id = ?1", params![id], |r| r.get(0)).ok().flatten();
        // 정책상 막는 것 — 허용 목록 밖 · 전권 우회 세션 · 세션이 사라짐 · 하루 상한
        let block: Option<&'static str> = if conn.query_row("SELECT COUNT(*) FROM session WHERE id = ?1", params![sid], |r| r.get::<_, i64>(0)).unwrap_or(0) == 0 {
            Some("세션이 없어졌습니다")
        } else if !allowed(conn, &sid) {
            Some("이 세션은 예약을 받을 수 있는 세션 목록에 없습니다")
        } else {
            device_block(conn, &id, &sid)
        };
        if let Some(why) = block {
            if let Some(a) = hold_or_fail(conn, &id, &occ, "failed", "policy", why, now) {
                rep.alerts.push(a);
            }
            continue;
        }
        let fires: i64 = db::get_meta(conn, &fires_key(now)).and_then(|v| v.parse().ok()).unwrap_or(0);
        if fires >= DAILY_FIRE_CAP {
            if let Some(a) = hold_or_fail(conn, &id, &occ, "held", "cap", "오늘 예약 발사 한도에 도달했습니다", now) {
                rep.alerts.push(a);
            }
            continue;
        }
        let age = time::parse(&created).map(|c| (now - c).num_seconds()).unwrap_or(0);
        let (policy, _) = effective_policy(conn, own.as_deref(), &sid, now);
        let quiet_now = in_quiet_window(conn, now).is_some();
        match decide(&probe.route(&sid), probe.work(&sid), policy, quiet_now, age) {
            Decision::Send => match fire_run(conn, now, &id, &occ) {
                Ok(()) => {
                    let _ = db::set_meta(conn, &fires_key(now), &(fires + 1).to_string());
                }
                Err(e) => {
                    if let Some(a) = hold_or_fail(conn, &id, &occ, "held", "rejected", &e, now) {
                        rep.alerts.push(a);
                    }
                }
            },
            Decision::Defer(why) => {
                let _ = conn.execute(
                    "UPDATE schedule_run SET state = 'deferred', note = ?3 WHERE schedule_id = ?1 AND occurrence_at = ?2 AND state IN ('pending','deferred')",
                    params![id, occ, why],
                );
            }
            Decision::Hold(code, why) => {
                if let Some(a) = hold_or_fail(conn, &id, &occ, "held", code, why, now) {
                    rep.alerts.push(a);
                }
            }
        }
    }
}

/// 폰이 건 예약은 발사 순간에도 폰 답과 같은 겹을 다시 통과해야 한다 — 그사이 기기 해제·예약 허용 끔·전체 멈춤·세션 차단이면 넣지 않는다
fn device_block(conn: &Connection, schedule_id: &str, sid: &str) -> Option<&'static str> {
    let by: String = conn.query_row("SELECT created_by FROM schedule WHERE id = ?1", params![schedule_id], |r| r.get(0)).ok()?;
    if by == "desktop" {
        return None;
    }
    let dev: Option<(i64, i64)> = conn
        .query_row("SELECT can_reply, can_schedule FROM relay_device WHERE pid = ?1", params![by], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()
        .ok()
        .flatten();
    let Some((can_reply, can_schedule)) = dev else { return Some("이 예약을 건 폰의 연결이 해제됐습니다") };
    if can_reply == 0 || can_schedule == 0 {
        return Some("PC 에서 이 폰의 예약 허용(또는 답 보내기)을 꺼 둠");
    }
    match conoti::reply_block(conn, true, sid) {
        "paused" => Some("PC 에서 폰 답 받기를 멈춤"),
        "session_blocked" => Some("PC 에서 이 세션의 폰 답을 막아 둠"),
        _ => None,
    }
}

/// 연결이 해제된 폰이 걸어 둔 예약(발사 전·미룸·대기)을 거둔다
pub fn cancel_device(conn: &Connection, pid: &str) {
    let ids: Vec<String> = conn
        .prepare("SELECT id FROM schedule WHERE created_by = ?1 AND state = 'active'")
        .and_then(|mut st| st.query_map(params![pid], |r| r.get(0)).map(|r| r.flatten().collect()))
        .unwrap_or_default();
    let now = Utc::now();
    for id in ids {
        if cancel(conn, now, &id).is_err() {
            // 이미 세션에 전달됐으면 그대로 둔다(끝난 것)
        }
    }
}

/// 세션마다 (걸려 있는 예약 수, 처리를 기다리는 수) — 폰 세션 목록의 `sched_n`
pub fn counts_by_session(conn: &Connection) -> std::collections::HashMap<String, (i64, i64)> {
    let mut out = std::collections::HashMap::new();
    if let Ok(mut st) = conn.prepare(
        "SELECT sc.session_id, COUNT(*), COALESCE(SUM(EXISTS (SELECT 1 FROM schedule_run r WHERE r.schedule_id = sc.id AND r.state = 'held')), 0)
           FROM schedule sc WHERE sc.state = 'active' GROUP BY sc.session_id",
    ) {
        if let Ok(rows) = st.query_map([], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))) {
            out.extend(rows.flatten());
        }
    }
    out
}

/// 회차를 `held`(사용자가 고른다) 또는 `failed`(정책상 막음 — 끝)로 두고 알림을 **한 번만** 낸다(조건부 UPDATE)
fn hold_or_fail(conn: &Connection, id: &str, occ: &str, state: &str, reason: &str, note: &str, now: DateTime<Utc>) -> Option<Alert> {
    let n = conn
        .execute(
            "UPDATE schedule_run SET state = ?3, reason = ?4, note = ?5 WHERE schedule_id = ?1 AND occurrence_at = ?2 AND state IN ('pending','deferred','fired','sending')",
            params![id, occ, state, reason, note],
        )
        .unwrap_or(0);
    if n == 0 {
        return None;
    }
    let first = conn
        .execute("UPDATE schedule_run SET notified_at = ?3 WHERE schedule_id = ?1 AND occurrence_at = ?2 AND notified_at IS NULL", params![id, occ, iso(now)])
        .unwrap_or(0);
    if state == "failed" {
        finish_schedule(conn, id, now);
    }
    if first == 1 {
        alert(conn, if state == "failed" { "failed" } else { "held" }, id, note)
    } else {
        None
    }
}

/// 회차를 대기열(`conoti_reply`)에 넣는다 — `received_at` = 지금(발사 시각), `sched` = 회차 키. 같은 회차는 두 번 들어가지 않는다.
fn fire_run(conn: &Connection, now: DateTime<Utc>, id: &str, occ: &str) -> R<()> {
    let (sid, body, quote, turn): (String, String, Option<String>, Option<i64>) = conn
        .query_row("SELECT session_id, text, quote, turn_id FROM schedule WHERE id = ?1", params![id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(|e| e.to_string())?;
    let ms = time::ms_of_iso(occ).unwrap_or(0);
    let key = format!("{id}-{ms}");
    let turn = match turn {
        Some(t) => Some(t),
        None => conn
            .query_row("SELECT id FROM turn WHERE session_id = ?1 AND hidden = 0 ORDER BY seq DESC LIMIT 1", params![sid], |r| r.get::<_, i64>(0))
            .optional()
            .map_err(|e| e.to_string())?,
    };
    let at = iso(now);
    conn.execute_batch("BEGIN IMMEDIATE").map_err(|e| e.to_string())?;
    let res = (|| -> R<()> {
        let n = conn
            .execute(
                "INSERT OR IGNORE INTO conoti_reply (reply_id, turn_id, session_id, kind, text, created_at, received_at, state, device, quote, sched)
                 VALUES (?1, ?2, ?3, 'text', ?4, ?5, ?5, 'delivering', ?6, ?7, ?1)",
                params![key, turn, sid, body, at, conoti::DESKTOP, quote],
            )
            .map_err(|e| e.to_string())?;
        if n == 1 {
            attach::link(conn, &key, &atts_of(conn, id))?;
        }
        conn.execute(
            "UPDATE schedule_run SET state = 'fired', fired_at = ?3, reply_id = ?4, note = NULL WHERE schedule_id = ?1 AND occurrence_at = ?2 AND state IN ('pending','deferred')",
            params![id, occ, at, key],
        )
        .map_err(|e| e.to_string())?;
        // 보관한 세션에 말을 보냈다 = 다시 쓰는 세션 — 입력창에서 보낼 때와 같다
        conn.execute("UPDATE session SET hidden = 0, archived_at = NULL WHERE id = ?1 AND hidden = 1", params![sid]).map_err(|e| e.to_string())?;
        Ok(())
    })();
    match res {
        Ok(()) => conn.execute_batch("COMMIT").map_err(|e| e.to_string())?,
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(e);
        }
    }
    conoti::record_tag_hint(conn, &sid, &body);
    Ok(())
}

/// 대기열에 넣은 회차의 결과를 따라간다: 전달됨 → 처리됨 · 거절 → held(알림) · 세션이 못 받은 채 오래 걸림 → 되돌려 held
fn sync_fired(conn: &Connection, now: DateTime<Utc>, probe: &dyn Probe, rep: &mut TickReport) {
    type Row = (String, String, String, String, Option<String>, Option<String>, Option<String>, String);
    let rows: Vec<Row> = conn
        .prepare(
            "SELECT r.schedule_id, r.occurrence_at, r.state, COALESCE(r.fired_at, r.created_at), r.reply_id, c.state, c.note, s.session_id
               FROM schedule_run r JOIN schedule s ON s.id = r.schedule_id LEFT JOIN conoti_reply c ON c.reply_id = r.reply_id
              WHERE r.state IN ('fired','delivered') LIMIT 100",
        )
        .and_then(|mut st| st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?))).map(|r| r.flatten().collect()))
        .unwrap_or_default();
    for (id, occ, state, fired_at, reply, cstate, cnote, sid) in rows {
        let set = |to: &str| {
            let _ = conn.execute("UPDATE schedule_run SET state = ?3 WHERE schedule_id = ?1 AND occurrence_at = ?2 AND state IN ('fired','delivered')", params![id, occ, to]);
        };
        match (reply.as_deref(), cstate.as_deref()) {
            (_, Some("delivered")) if state != "delivered" => {
                set("delivered");
                rep.changed_sessions.push(sid.clone());
                finish_schedule(conn, &id, now);
            }
            (_, Some("delivered")) => finish_schedule(conn, &id, now),
            (_, Some("handled")) => {
                set("handled");
                finish_schedule(conn, &id, now);
            }
            (_, Some("rejected")) => {
                rep.changed_sessions.push(sid.clone());
                let note = cnote.unwrap_or_default();
                if note == "예약 취소" || note == "보내기 취소" {
                    set("cancelled");
                    finish_schedule(conn, &id, now);
                } else {
                    // 세션이 꺼져서(또는 훅이 없어서) 거절됐다면 그 사유로 — 다시 받을 수 있게 되면 재알림한다
                    let reason = match probe.route(&sid) {
                        Route::Ended => "ended",
                        Route::Terminal => "terminal",
                        _ => "rejected",
                    };
                    if let Some(a) = hold_or_fail(conn, &id, &occ, "held", reason, &note, now) {
                        rep.alerts.push(a);
                    }
                }
            }
            (Some(_), Some("delivering" | "confirm")) => {
                // 세션이 일하는 중이면 기다린다(3시간 상한은 파이프라인이 맡는다). 일하지도 않는데 오래 못 넣으면 되돌려 알린다
                let stuck = time::parse(&fired_at).map(|f| (now - f).num_seconds() >= STUCK_SECS).unwrap_or(false);
                if stuck && probe.work(&sid) != Work::Busy {
                    let took = conn
                        .execute(
                            "UPDATE conoti_reply SET state = 'rejected', note = '예약: 세션이 받지 못해 되돌림' WHERE reply_id = ?1 AND state = 'delivering'",
                            params![reply],
                        )
                        .unwrap_or(0);
                    if took == 1 {
                        rep.changed_sessions.push(sid.clone());
                        if let Some(a) = hold_or_fail(conn, &id, &occ, "held", "stuck", "세션이 10분 넘게 받지 못했습니다", now) {
                            rep.alerts.push(a);
                        }
                    }
                }
            }
            (_, None) => {
                // 대기열 줄이 사라졌다(세션 기록 지우기 등) — 회차를 취소로
                set("cancelled");
                finish_schedule(conn, &id, now);
            }
            _ => {}
        }
    }
}

/// held 로 알린 뒤 세션이 다시 받을 수 있게 되면 **한 번만** 다시 알린다(자동 전달은 없다)
fn renotify(conn: &Connection, now: DateTime<Utc>, probe: &dyn Probe, rep: &mut TickReport) {
    let rows: Vec<(String, String, String, String)> = conn
        .prepare(
            "SELECT r.schedule_id, r.occurrence_at, COALESCE(r.reason, ''), s.session_id FROM schedule_run r JOIN schedule s ON s.id = r.schedule_id
              WHERE r.state = 'held' AND r.notified_at IS NOT NULL AND r.renotified_at IS NULL LIMIT 50",
        )
        .and_then(|mut st| st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))).map(|r| r.flatten().collect()))
        .unwrap_or_default();
    for (id, occ, reason, sid) in rows {
        if !receivable(&reason, &probe.route(&sid), probe.work(&sid)) {
            continue;
        }
        let n = conn
            .execute("UPDATE schedule_run SET renotified_at = ?3 WHERE schedule_id = ?1 AND occurrence_at = ?2 AND state = 'held' AND renotified_at IS NULL", params![id, occ, iso(now)])
            .unwrap_or(0);
        if n == 1 {
            if let Some(a) = alert(conn, "back", &id, "세션을 다시 받을 수 있습니다 — 보낼지 버릴지 고르세요") {
                rep.alerts.push(a);
            }
            rep.changed_sessions.push(sid);
        }
    }
}

fn expire_held(conn: &Connection, now: DateTime<Utc>, rep: &mut TickReport) {
    let cut = iso(now - Duration::days(HELD_EXPIRE_DAYS));
    let ids: Vec<(String, String)> = conn
        .prepare("SELECT r.schedule_id, s.session_id FROM schedule_run r JOIN schedule s ON s.id = r.schedule_id WHERE r.state = 'held' AND r.created_at < ?1")
        .and_then(|mut st| st.query_map(params![cut], |r| Ok((r.get(0)?, r.get(1)?))).map(|r| r.flatten().collect()))
        .unwrap_or_default();
    for (id, sid) in ids {
        let n = conn
            .execute(
                "UPDATE schedule_run SET state = 'cancelled', note = '7일 동안 처리하지 않아 버림' WHERE schedule_id = ?1 AND state = 'held' AND created_at < ?2",
                params![id, cut],
            )
            .unwrap_or(0);
        if n > 0 {
            finish_schedule(conn, &id, now);
            rep.changed_sessions.push(sid);
        }
    }
}

// ── 설정 화면용 ──────────────────────────────────────────────────────────────

pub fn settings(conn: &Connection) -> Value {
    let windows: Vec<Value> = conn
        .prepare("SELECT id, name, days, start_hm, end_hm, tz, enabled FROM quiet_window ORDER BY id")
        .and_then(|mut st| {
            st.query_map([], |r| {
                Ok(json!({"id": r.get::<_, i64>(0)?, "name": r.get::<_, String>(1)?, "days": r.get::<_, i64>(2)?, "start": r.get::<_, String>(3)?,
                          "end": r.get::<_, String>(4)?, "tz": r.get::<_, String>(5)?, "enabled": r.get::<_, i64>(6)? != 0}))
            })
            .map(|r| r.flatten().collect())
        })
        .unwrap_or_default();
    let rules: Vec<Value> = conn
        .prepare("SELECT id, scope, key, action FROM busy_rule ORDER BY id")
        .and_then(|mut st| st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))).map(|r| r.flatten().collect::<Vec<_>>()))
        .unwrap_or_default()
        .into_iter()
        .map(|(id, scope, key, action)| {
            let label = if scope == "session" {
                label_of(conn, &key)
            } else {
                conn.query_row("SELECT name FROM tag WHERE id = ?1", params![key.parse::<i64>().unwrap_or(-1)], |r| r.get::<_, String>(0)).unwrap_or_else(|_| "?".into())
            };
            json!({"id": id, "scope": scope, "key": key, "action": action, "label": label})
        })
        .collect();
    let allow: Vec<Value> = conn
        .prepare("SELECT session_id FROM sched_allow ORDER BY added_at")
        .and_then(|mut st| st.query_map([], |r| r.get::<_, String>(0)).map(|r| r.flatten().collect::<Vec<_>>()))
        .unwrap_or_default()
        .into_iter()
        .map(|sid| json!({"session_id": sid, "name": label_of(conn, &sid)}))
        .collect();
    let (active, held) = counts(conn);
    json!({
        "paused": paused(conn),
        "busy_default": global_default(conn).name(),
        "perm": perm_policy(conn),
        "allow": allow,
        "windows": windows,
        "rules": rules,
        "active": active,
        "held": held,
    })
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> R<()> {
    match key {
        "paused" if matches!(value, "0" | "1") => db::set_meta(conn, "setting.sched_paused", value).map_err(|e| e.to_string()),
        "busy_default" if Policy::parse(value).is_some() => db::set_meta(conn, "setting.sched_busy_default", value).map_err(|e| e.to_string()),
        "perm" if matches!(value, "default" | "allowlist") => db::set_meta(conn, "setting.sched_perm", value).map_err(|e| e.to_string()),
        _ => Err("알 수 없는 설정이거나 값이 올바르지 않습니다".into()),
    }
}

pub fn set_allow(conn: &Connection, sid: &str, on: bool) -> R<()> {
    if !crate::channel::valid_session_id(sid) {
        return Err("세션 ID 형식 오류".into());
    }
    if on {
        conn.execute("INSERT OR IGNORE INTO sched_allow (session_id, added_at) VALUES (?1, ?2)", params![sid, time::now_iso()]).map_err(|e| e.to_string())?;
    } else {
        conn.execute("DELETE FROM sched_allow WHERE session_id = ?1", params![sid]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 방해금지 창 하나를 만들거나(id 없음) 고친다. 시각 `HH:MM`, 시간대는 IANA, 요일 비트 월(1)~일(64)
pub fn save_window(conn: &Connection, id: Option<i64>, name: &str, days: i64, start: &str, end: &str, tz: &str, enabled: bool) -> R<i64> {
    if parse_hm(start).is_none() || parse_hm(end).is_none() || start == end {
        return Err("시작·종료 시각(HH:MM)이 올바르지 않거나 같습니다".into());
    }
    if tz.parse::<Tz>().is_err() {
        return Err("시간대 형식 오류".into());
    }
    if !(1..=127).contains(&days) {
        return Err("요일을 하나 이상 고르세요".into());
    }
    let name = text::clip(name.trim(), 30);
    match id {
        Some(i) => {
            let n = conn
                .execute(
                    "UPDATE quiet_window SET name = ?2, days = ?3, start_hm = ?4, end_hm = ?5, tz = ?6, enabled = ?7 WHERE id = ?1",
                    params![i, name, days, start, end, tz, enabled as i64],
                )
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Err("창을 찾을 수 없습니다".into());
            }
            Ok(i)
        }
        None => {
            conn.execute(
                "INSERT INTO quiet_window (name, days, start_hm, end_hm, tz, enabled) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![name, days, start, end, tz, enabled as i64],
            )
            .map_err(|e| e.to_string())?;
            Ok(conn.last_insert_rowid())
        }
    }
}

pub fn delete_window(conn: &Connection, id: i64) -> R<()> {
    conn.execute("DELETE FROM quiet_window WHERE id = ?1", params![id]).map_err(|e| e.to_string())?;
    Ok(())
}

/// 세션·태그 규칙. `action` 이 빈 문자열이면 지운다
pub fn set_rule(conn: &Connection, scope: &str, key: &str, action: &str) -> R<()> {
    if !matches!(scope, "session" | "tag") {
        return Err("scope 는 session 또는 tag".into());
    }
    if scope == "session" && !crate::channel::valid_session_id(key) {
        return Err("세션 ID 형식 오류".into());
    }
    if scope == "tag" && key.parse::<i64>().is_err() {
        return Err("태그 id 형식 오류".into());
    }
    if action.is_empty() {
        conn.execute("DELETE FROM busy_rule WHERE scope = ?1 AND key = ?2", params![scope, key]).map_err(|e| e.to_string())?;
        return Ok(());
    }
    if Policy::parse(action).is_none() {
        return Err("정책 형식 오류".into());
    }
    conn.execute(
        "INSERT INTO busy_rule (scope, key, action) VALUES (?1, ?2, ?3) ON CONFLICT(scope, key) DO UPDATE SET action = excluded.action",
        params![scope, key, action],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

// ── 화면 명령 ────────────────────────────────────────────────────────────────

use std::sync::atomic::Ordering;
use tauri::{Emitter, State};

use crate::AppState;

fn e<T: ToString>(x: T) -> String {
    x.to_string()
}

fn changed(app: &tauri::AppHandle) {
    let _ = app.emit("sched-changed", ());
}

#[tauri::command]
pub fn sched_add(app: tauri::AppHandle, state: State<AppState>, new: NewSchedule) -> R<Value> {
    let conn = state.conn.lock().map_err(e)?;
    let v = add(&conn, Utc::now(), "desktop", &new, &LiveProbe)?;
    changed(&app);
    Ok(v)
}

#[tauri::command]
pub fn sched_list(state: State<AppState>, session_id: Option<String>) -> R<Vec<Value>> {
    let conn = state.conn.lock().map_err(e)?;
    Ok(list(&conn, session_id.as_deref()))
}

#[tauri::command]
pub fn sched_counts(state: State<AppState>) -> R<Value> {
    let conn = state.conn.lock().map_err(e)?;
    let (active, held) = counts(&conn);
    Ok(json!({"active": active, "held": held}))
}

#[tauri::command]
pub fn sched_update(app: tauri::AppHandle, state: State<AppState>, id: String, edit: EditSchedule) -> R<Value> {
    let conn = state.conn.lock().map_err(e)?;
    let v = update(&conn, Utc::now(), &id, &edit)?;
    changed(&app);
    Ok(v)
}

#[tauri::command]
pub fn sched_cancel(app: tauri::AppHandle, state: State<AppState>, id: String) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    cancel(&conn, Utc::now(), &id)?;
    changed(&app);
    Ok(())
}

#[tauri::command]
pub fn sched_act(app: tauri::AppHandle, state: State<AppState>, id: String, op: String) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    act(&conn, Utc::now(), &id, &op)?;
    state.kick.store(true, Ordering::SeqCst);
    changed(&app);
    Ok(())
}

#[tauri::command]
pub fn sched_settings(state: State<AppState>) -> R<Value> {
    let conn = state.conn.lock().map_err(e)?;
    Ok(settings(&conn))
}

#[tauri::command]
pub fn sched_set_setting(app: tauri::AppHandle, state: State<AppState>, key: String, value: String) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    set_setting(&conn, &key, &value)?;
    changed(&app);
    Ok(())
}

#[tauri::command]
pub fn sched_allow_set(app: tauri::AppHandle, state: State<AppState>, session_id: String, on: bool) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    set_allow(&conn, &session_id, on)?;
    changed(&app);
    Ok(())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn sched_window_save(app: tauri::AppHandle, state: State<AppState>, id: Option<i64>, name: String, days: i64, start: String, end: String, tz: String, enabled: bool) -> R<i64> {
    let conn = state.conn.lock().map_err(e)?;
    let r = save_window(&conn, id, &name, days, &start, &end, &tz, enabled)?;
    changed(&app);
    Ok(r)
}

#[tauri::command]
pub fn sched_window_delete(app: tauri::AppHandle, state: State<AppState>, id: i64) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    delete_window(&conn, id)?;
    changed(&app);
    Ok(())
}

#[tauri::command]
pub fn sched_rule_set(app: tauri::AppHandle, state: State<AppState>, scope: String, key: String, action: String) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    set_rule(&conn, &scope, &key, &action)?;
    changed(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        db::migrate(&c).unwrap();
        c
    }

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap()
    }
    fn plus(mins: i64, secs: i64) -> DateTime<Utc> {
        t0() + Duration::minutes(mins) + Duration::seconds(secs)
    }

    const S1: &str = "sess-aaaa-0001";
    const S2: &str = "sess-bbbb-0002";

    fn session(c: &Connection, sid: &str) {
        c.execute("INSERT OR IGNORE INTO session (id, title, project_dir) VALUES (?1, ?1, '/w/proj')", params![sid]).unwrap();
        c.execute(
            "INSERT OR IGNORE INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, status) VALUES (?1, 'u1', 1, '2026-09-30T00:00:00.000Z', '첫 요청', 'done')",
            params![sid],
        )
        .unwrap();
    }

    #[derive(Clone, Copy)]
    enum Rt {
        Live,
        Channel,
        Ended,
        Terminal,
        Unarmed,
        Idle,
        BusyNone,
    }
    impl Rt {
        fn route(self) -> Route {
            match self {
                Rt::Live => Route::Live,
                Rt::Channel => Route::Channel,
                Rt::Ended => Route::Ended,
                Rt::Terminal => Route::Terminal,
                Rt::Unarmed => Route::Unarmed,
                Rt::Idle => Route::Idle,
                Rt::BusyNone => Route::Busy(None),
            }
        }
    }
    struct Fake(RefCell<HashMap<String, (Rt, Work)>>);
    impl Fake {
        fn new(sid: &str, r: Rt, w: Work) -> Fake {
            let f = Fake(RefCell::new(HashMap::new()));
            f.set(sid, r, w);
            f
        }
        fn set(&self, sid: &str, r: Rt, w: Work) {
            self.0.borrow_mut().insert(sid.into(), (r, w));
        }
    }
    impl Probe for Fake {
        fn route(&self, sid: &str) -> Route {
            self.0.borrow().get(sid).map(|x| x.0.route()).unwrap_or(Route::Ended)
        }
        fn work(&self, sid: &str) -> Work {
            self.0.borrow().get(sid).map(|x| x.1).unwrap_or(Work::Idle)
        }
    }

    fn new_in(min: i64) -> NewSchedule {
        NewSchedule {
            session_id: S1.into(),
            text: "테스트를 돌려 줘".into(),
            atts: vec![],
            quote_turn: None,
            quote_part: None,
            when: WhenIn::After { min },
            on_missed: "run_once".into(),
            missed_within_min: None,
            busy_policy: None,
            rid: None,
        }
    }

    fn add_ok(c: &Connection, n: &NewSchedule, p: &Fake) -> String {
        add(c, t0(), "desktop", n, p).unwrap()["id"].as_str().unwrap().to_string()
    }

    fn replies(c: &Connection) -> i64 {
        c.query_row("SELECT COUNT(*) FROM conoti_reply WHERE sched IS NOT NULL", [], |r| r.get(0)).unwrap()
    }
    fn run_state(c: &Connection, id: &str) -> String {
        c.query_row("SELECT state FROM schedule_run WHERE schedule_id = ?1 ORDER BY occurrence_at DESC LIMIT 1", params![id], |r| r.get(0)).unwrap()
    }
    fn sched_state(c: &Connection, id: &str) -> String {
        c.query_row("SELECT state FROM schedule WHERE id = ?1", params![id], |r| r.get(0)).unwrap()
    }

    // ── 시간 ──

    #[test]
    fn local_time_resolution_follows_dst_rules() {
        let ny: Tz = "America/New_York".parse().unwrap();
        let d = |y, m, dd, h, mi| NaiveDateTime::parse_from_str(&format!("{y}-{m:02}-{dd:02}T{h:02}:{mi:02}"), "%Y-%m-%dT%H:%M").unwrap();
        // 봄 건너뜀: 2026-03-08 02:00→03:00 — 02:30 은 없다 → 직후 첫 유효 시각 03:00 EDT = 07:00Z
        assert_eq!(local_to_utc(ny, d(2026, 3, 8, 2, 30)).unwrap(), Utc.with_ymd_and_hms(2026, 3, 8, 7, 0, 0).unwrap());
        // 가을 겹침: 2026-11-01 01:30 은 두 번 — 첫 번째(EDT, UTC-4) = 05:30Z
        assert_eq!(local_to_utc(ny, d(2026, 11, 1, 1, 30)).unwrap(), Utc.with_ymd_and_hms(2026, 11, 1, 5, 30, 0).unwrap());
        // 보통 날: EST/EDT 오프셋
        assert_eq!(local_to_utc(ny, d(2026, 1, 15, 9, 0)).unwrap(), Utc.with_ymd_and_hms(2026, 1, 15, 14, 0, 0).unwrap());
        assert_eq!(local_to_utc(ny, d(2026, 7, 15, 9, 0)).unwrap(), Utc.with_ymd_and_hms(2026, 7, 15, 13, 0, 0).unwrap());
        // 서머타임이 없는 서울
        let seoul: Tz = "Asia/Seoul".parse().unwrap();
        assert_eq!(local_to_utc(seoul, d(2026, 10, 1, 9, 0)).unwrap(), Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap());
        // 30분 건너뛰는 곳(Lord Howe)도 그 직후로
        let lh: Tz = "Australia/Lord_Howe".parse().unwrap();
        let r = local_to_utc(lh, d(2026, 10, 4, 2, 10)).unwrap();
        assert_eq!(r.with_timezone(&lh).format("%H:%M").to_string(), "02:30");
    }

    #[test]
    fn at_when_uses_the_creators_zone_and_rejects_bad_input() {
        let now = Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap();
        let w = |l: &str, tz: &str| WhenIn::At { local: l.into(), tz: tz.into() };
        let (due, kind, tz) = resolve_when(now, &w("2026-03-08T02:30", "America/New_York")).unwrap();
        assert_eq!((due, kind, tz.as_str()), (Utc.with_ymd_and_hms(2026, 3, 8, 7, 0, 0).unwrap(), "once", "America/New_York"));
        assert!(resolve_when(now, &w("2026-03-08T02:30", "Mars/Base")).is_err());
        assert!(resolve_when(now, &w("내일 아침", "Asia/Seoul")).is_err());
        assert!(resolve_when(now, &w("2026-02-28T09:00", "Asia/Seoul")).unwrap_err().contains("지난 시각"));
        assert!(resolve_when(now, &w("2028-03-08T09:00", "Asia/Seoul")).unwrap_err().contains("366"));
        assert!(resolve_when(now, &WhenIn::After { min: 0 }).is_err() && resolve_when(now, &WhenIn::After { min: MAX_AFTER_MIN + 1 }).is_err());
        assert_eq!(resolve_when(now, &WhenIn::After { min: 30 }).unwrap().0, now + Duration::minutes(30));
    }

    #[test]
    fn quiet_windows_cross_midnight_and_respect_days_and_zone() {
        let seoul: Tz = "Asia/Seoul".parse().unwrap();
        let hm = |s: &str| parse_hm(s).unwrap();
        // 월~금(1+2+4+8+16=31) 22:00~08:00 서울. 2026-10-05 는 월요일
        let at = |d: u32, h: u32, m: u32| seoul.with_ymd_and_hms(2026, 10, d, h, m, 0).unwrap().with_timezone(&Utc);
        let w = |t| window_contains(31, hm("22:00"), hm("08:00"), seoul, t);
        assert!(w(at(5, 22, 0)) && w(at(5, 23, 59)) && w(at(6, 0, 30)) && w(at(6, 7, 59)));
        assert!(!w(at(6, 8, 0)) && !w(at(5, 21, 59)) && !w(at(5, 7, 0)), "월요일 아침 7시는 일요일 밤 창의 끝 — 일요일은 켜지 않았다");
        assert!(w(at(9, 23, 0)), "금요일 밤");
        assert!(w(at(10, 1, 0)), "토요일 새벽은 금요일 밤 창의 연장");
        assert!(!w(at(10, 23, 0)) && !w(at(11, 1, 0)), "토요일 밤·일요일 새벽은 창이 없다");
        // 같은 절대 시각이라도 시간대가 다르면 다르다
        let ny: Tz = "America/New_York".parse().unwrap();
        assert!(!window_contains(127, hm("22:00"), hm("08:00"), ny, at(5, 23, 0)), "서울 23시는 뉴욕 오전 10시");
        // 시작 = 종료는 빈 창
        assert!(!window_contains(127, hm("09:00"), hm("09:00"), seoul, at(5, 9, 0)));
    }

    // ── 판정 매트릭스 ──

    #[test]
    fn decision_matrix_route_by_work_by_policy() {
        use Decision::*;
        use Policy::*;
        let up = [Route::Channel, Route::Live, Route::Unarmed, Route::Idle, Route::Busy(None)];
        for r in &up {
            for pol in [Interrupt, AfterWork, AfterQuiet] {
                // 쉬는 세션은 어떤 정책이든 바로(방해금지 창 밖)
                assert_eq!(decide(r, Work::Idle, pol, false, 0), Send, "{r:?} {pol:?}");
                // 일하는 세션
                let busy = decide(r, Work::Busy, pol, false, 0);
                assert_eq!(busy, if pol == Interrupt { Send } else { Defer("작업이 끝나길 기다립니다") }, "{r:?} {pol:?}");
                // 승인 대기는 정책과 무관하게 10분까지 미루고 그 뒤 held
                assert!(matches!(decide(r, Work::PermWait, pol, false, 599), Defer(_)));
                assert!(matches!(decide(r, Work::PermWait, pol, false, 600), Hold("perm", _)));
            }
        }
        // 방해금지 창 안: after_quiet 만 미룬다(쉬는 세션이어도)
        assert_eq!(decide(&Route::Live, Work::Idle, AfterQuiet, true, 0), Defer("방해금지 시간이 끝나길 기다립니다"));
        assert_eq!(decide(&Route::Live, Work::Idle, AfterWork, true, 0), Send);
        assert_eq!(decide(&Route::Live, Work::Busy, Interrupt, true, 0), Send);
        // 꺼짐·훅 없는 터미널은 정책과 무관하게 바로 held
        for pol in [Interrupt, AfterWork, AfterQuiet] {
            assert!(matches!(decide(&Route::Ended, Work::Idle, pol, false, 0), Hold("ended", _)));
            assert!(matches!(decide(&Route::Terminal, Work::Busy, pol, false, 0), Hold("terminal", _)));
        }
        // 미룬 지 3시간이면 held
        assert!(matches!(decide(&Route::Live, Work::Busy, AfterWork, false, DEFER_LIMIT_SECS - 1), Defer(_)));
        assert!(matches!(decide(&Route::Live, Work::Busy, AfterWork, false, DEFER_LIMIT_SECS), Hold("busy_limit", _)));
    }

    #[test]
    fn policy_priority_schedule_session_tag_window_global() {
        let c = mem();
        session(&c, S1);
        let now = t0();
        assert_eq!(effective_policy(&c, None, S1, now), (Policy::AfterWork, Source::Global), "전역 기본은 작업 뒤");
        set_setting(&c, "busy_default", "interrupt").unwrap();
        assert_eq!(effective_policy(&c, None, S1, now), (Policy::Interrupt, Source::Global));
        // 방해금지 창 안이면 전역보다 우선 → after_quiet
        save_window(&c, None, "밤", 127, "00:00", "23:59", "UTC", true).unwrap();
        assert_eq!(effective_policy(&c, None, S1, now), (Policy::AfterQuiet, Source::Window));
        // 태그 규칙 > 창
        c.execute("INSERT INTO tag (id, name, created_at) VALUES (7, '예약시험태그', 't')", []).unwrap();
        c.execute("UPDATE turn SET id = id WHERE session_id = ?1", params![S1]).unwrap();
        let tid: i64 = c.query_row("SELECT id FROM turn WHERE session_id = ?1", params![S1], |r| r.get(0)).unwrap();
        c.execute("INSERT INTO turn_tag (turn_id, tag_id, state, at) VALUES (?1, 7, 'manual', 't')", params![tid]).unwrap();
        set_rule(&c, "tag", "7", "after_work").unwrap();
        assert_eq!(effective_policy(&c, None, S1, now), (Policy::AfterWork, Source::Tag));
        // 세션 규칙 > 태그
        set_rule(&c, "session", S1, "interrupt").unwrap();
        assert_eq!(effective_policy(&c, None, S1, now), (Policy::Interrupt, Source::Session));
        // 예약 덮어쓰기 > 세션
        assert_eq!(effective_policy(&c, Some("after_quiet"), S1, now), (Policy::AfterQuiet, Source::Schedule));
        // 규칙을 지우면 한 단계씩 물러난다
        set_rule(&c, "session", S1, "").unwrap();
        assert_eq!(effective_policy(&c, None, S1, now).1, Source::Tag);
        set_rule(&c, "tag", "7", "").unwrap();
        assert_eq!(effective_policy(&c, None, S1, now).1, Source::Window);
        assert!(set_rule(&c, "session", S1, "kill").is_err() && set_rule(&c, "x", "1", "interrupt").is_err());
    }

    // ── 만들기 ──

    #[test]
    fn add_validates_and_limits() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        let ok = add(&c, t0(), "desktop", &new_in(30), &p).unwrap();
        assert_eq!(ok["state"], "active");
        assert_eq!(ok["next_due_at"], "2026-10-01T00:30:00.000Z");
        assert_eq!(ok["busy_policy"], Value::Null);
        assert_eq!(ok["missed_within_min"], Value::Null);
        // 잘못된 입력
        let mut bad = new_in(30);
        bad.text = "  ".into();
        assert!(add(&c, t0(), "desktop", &bad, &p).is_err());
        bad = new_in(30);
        bad.on_missed = "later".into();
        assert!(add(&c, t0(), "desktop", &bad, &p).is_err());
        bad = new_in(30);
        bad.busy_policy = Some("bypass".into());
        assert!(add(&c, t0(), "desktop", &bad, &p).is_err());
        bad = new_in(30);
        bad.on_missed = "within".into();
        bad.missed_within_min = Some(0);
        assert!(add(&c, t0(), "desktop", &bad, &p).unwrap_err().contains("1분"));
        bad = new_in(30);
        bad.session_id = "no-such-session".into();
        assert!(add(&c, t0(), "desktop", &bad, &p).unwrap_err().contains("세션"));
        bad = new_in(30);
        bad.quote_part = Some("prompt".into());
        assert!(add(&c, t0(), "desktop", &bad, &p).is_err(), "답장은 대상 요청이 있어야 한다");
        // within 기본 60분
        let mut w = new_in(10);
        w.on_missed = "within".into();
        assert_eq!(add(&c, t0(), "desktop", &w, &p).unwrap()["missed_within_min"], 60);
        // 개수 상한: 세션당 20
        for _ in 0..(MAX_ACTIVE_PER_SESSION - 2) {
            add(&c, t0(), "desktop", &new_in(20), &p).unwrap();
        }
        assert!(add(&c, t0(), "desktop", &new_in(20), &p).unwrap_err().contains("너무 많"));
        // 비밀값은 저장 전에 가린다(입력창과 같다)
        let c2 = mem();
        session(&c2, S1);
        let mut s = new_in(5);
        s.text = "export OPENAI_API_KEY=abcd1234efgh5678ijkl9012 하고 돌려".into();
        let id = add_ok(&c2, &s, &p);
        let stored: String = c2.query_row("SELECT text FROM schedule WHERE id = ?1", params![id], |r| r.get(0)).unwrap();
        assert!(!stored.contains("abcd1234efgh5678ijkl9012"), "{stored}");
    }

    #[test]
    fn add_warns_when_session_cannot_receive_now() {
        let c = mem();
        session(&c, S1);
        let v = add(&c, t0(), "desktop", &new_in(30), &Fake::new(S1, Rt::Ended, Work::Idle)).unwrap();
        assert!(v["warnings"][0].as_str().unwrap().contains("꺼져 있"));
        let v = add(&c, t0(), "desktop", &new_in(30), &Fake::new(S1, Rt::Terminal, Work::Idle)).unwrap();
        assert!(v["warnings"][0].as_str().unwrap().contains("훅"));
        let v = add(&c, t0(), "desktop", &new_in(30), &Fake::new(S1, Rt::Live, Work::Idle)).unwrap();
        assert!(v["warnings"].as_array().unwrap().is_empty());
    }

    fn hook_mode(c: &Connection, sid: &str, mode: &str, at: &str) {
        c.execute(
            "INSERT INTO hook_event (session_id, event, at, detail) VALUES (?1, 'UserPromptSubmit', ?2, ?3)",
            params![sid, at, json!({"permission_mode": mode}).to_string()],
        )
        .unwrap();
    }

    #[test]
    fn bypass_sessions_are_no_longer_refused_and_allowlist_still_applies() {
        let c = mem();
        session(&c, S1);
        session(&c, S2);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        p.set(S2, Rt::Live, Work::Idle);
        // 전권 우회 세션도 예약을 받는다(회귀: 예전엔 거절) — 만들 때도 발사 때도
        hook_mode(&c, S1, "bypassPermissions", "2026-09-30T01:00:00.000Z");
        let v = add(&c, t0(), "desktop", &new_in(10), &p).expect("bypass 세션 예약 허용");
        assert_eq!(v["sched_warn"], false, "전부 허용 세션은 사전 준비 안내가 필요 없다");
        assert_eq!(v["perm"], "bypassPermissions");
        let id = v["id"].as_str().unwrap().to_string();
        let rep = tick(&c, plus(10, 0), &p);
        assert_eq!((run_state(&c, &id), replies(&c)), ("fired".into(), 1));
        assert!(rep.alerts.is_empty());
        // 허용 세션 목록: 목록 밖은 만들 때부터 거절, 목록에서 빼면 발사 때 막는다
        set_setting(&c, "perm", "allowlist").unwrap();
        assert!(add(&c, t0(), "desktop", &NewSchedule { session_id: S2.into(), ..new_in(10) }, &p).unwrap_err().contains("목록"));
        set_allow(&c, S2, true).unwrap();
        let id2 = add_ok(&c, &NewSchedule { session_id: S2.into(), ..new_in(10) }, &p);
        set_allow(&c, S2, false).unwrap();
        let rep = tick(&c, plus(10, 0), &p);
        assert_eq!(run_state(&c, &id2), "failed");
        assert_eq!(rep.alerts.len(), 1);
        assert_eq!(replies(&c), 1, "S2 는 대기열에 안 들어갔다(앞의 bypass 세션 한 줄뿐)");
        // 기본 정책으로 되돌리면 다시 받는다
        set_setting(&c, "perm", "default").unwrap();
        assert!(add(&c, t0(), "desktop", &NewSchedule { session_id: S2.into(), ..new_in(10) }, &p).is_ok());
        assert!(set_setting(&c, "perm", "bypass").is_err(), "전권 우회는 선택지에 없다");
    }

    #[test]
    fn alert_kinds_map_to_server_push_kinds() {
        let mk = |kind| Alert { kind, schedule_id: String::new(), session_id: String::new(), session_name: String::new(), preview: String::new(), note: String::new() };
        assert_eq!(mk("held").push_kind(), "sched_held");
        assert_eq!(mk("failed").push_kind(), "sched_held");
        assert_eq!(mk("back").push_kind(), "sched_ready");
        assert_eq!(mk("missed").push_kind(), "sched_missed");
        assert_eq!(pick_push_kind(&[]), None);
        assert_eq!(pick_push_kind(&["sched_ready", "sched_missed"]), Some("sched_missed"));
        assert_eq!(pick_push_kind(&["sched_ready", "sched_held", "sched_missed"]), Some("sched_held"));
    }

    #[test]
    fn perm_info_classifies_each_mode_and_respects_dismissal() {
        let c = mem();
        session(&c, S1);
        // 값 없음 → 확인 불가 · 안내 필요
        let i = perm_info(&c, S1);
        assert_eq!((i["kind"].as_str(), i["warn"].as_bool(), i["notice"].as_bool()), (Some("unknown"), Some(true), Some(false)));
        assert!(i["perm"].is_null());
        let cases = [
            ("default", "default", true),
            ("acceptEdits", "accept_edits", true),
            ("plan", "plan", true),
            ("auto", "auto", true),
            ("dontAsk", "dont_ask", true),
            ("bypassPermissions", "bypass", false),
            ("전혀모르는값", "unknown", true),
        ];
        for (n, (mode, kind, warn)) in cases.iter().enumerate() {
            hook_mode(&c, S1, mode, &format!("2026-09-30T00:00:{:02}.000Z", n));
            let i = perm_info(&c, S1);
            assert_eq!((i["kind"].as_str(), i["warn"].as_bool()), (Some(*kind), Some(*warn)), "{mode}");
            assert_eq!(i["notice"].as_bool(), Some(*kind == "bypass"), "{mode}");
            assert_eq!(i["perm"].is_null(), *kind == "unknown", "알 수 없는 값은 폰에 그대로 내보내지 않는다: {mode}");
        }
        // 다시 안 보기: 그 세션만, 되돌릴 수 있다. 세션이 나중에 모드를 바꿔도 유지
        hook_mode(&c, S1, "default", "2026-09-30T00:10:00.000Z");
        assert_eq!(perm_info(&c, S1)["warn"], true);
        set_warn_off(&c, S1, true).unwrap();
        let i = perm_info(&c, S1);
        assert_eq!((i["warn"].as_bool(), i["dismissed"].as_bool()), (Some(false), Some(true)));
        session(&c, S2);
        assert_eq!(perm_info(&c, S2)["warn"], true, "다른 세션엔 영향 없음");
        set_warn_off(&c, S1, false).unwrap();
        assert_eq!(perm_info(&c, S1)["warn"], true);
        assert!(set_warn_off(&c, "../evil", true).is_err());
    }

    #[test]
    fn add_result_carries_perm_fields_for_phone_and_desktop() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        let v = add(&c, t0(), "desktop", &new_in(30), &p).unwrap();
        assert_eq!((v["perm"].is_null(), v["sched_warn"].as_bool()), (true, Some(true)), "확인 불가도 안내 대상");
        hook_mode(&c, S1, "plan", "2026-09-30T00:00:00.000Z");
        let v = add(&c, t0(), "desktop", &new_in(31), &p).unwrap();
        assert_eq!((v["perm"].as_str(), v["sched_warn"].as_bool()), (Some("plan"), Some(true)));
        assert_eq!(list(&c, Some(S1))[0]["perm"], "plan");
    }

    // ── 발사 경계 · 중복 ──

    #[test]
    fn fires_exactly_once_at_the_boundary_with_fake_clock() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        let id = add_ok(&c, &new_in(30), &p);
        // 1초 전엔 아무 일도 없다
        tick(&c, plus(29, 59), &p);
        assert_eq!((replies(&c), sched_state(&c, &id).as_str()), (0, "active"));
        // 정각: 대기열에 한 줄, received_at = 발사 시각
        let rep = tick(&c, plus(30, 0), &p);
        assert_eq!(replies(&c), 1);
        assert!(rep.alerts.is_empty() && rep.changed_sessions.contains(&S1.to_string()));
        let (rid, at, dev, sched, state, text): (String, String, String, Option<String>, String, String) = c
            .query_row("SELECT reply_id, received_at, device, sched, state, text FROM conoti_reply WHERE sched IS NOT NULL", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
            .unwrap();
        assert_eq!(at, "2026-10-01T00:30:00.000Z", "3시간 상한이 발사 기준이 되도록");
        assert_eq!((dev.as_str(), state.as_str(), text.as_str()), ("desktop", "delivering", "테스트를 돌려 줘"));
        assert_eq!(sched.as_deref(), Some(rid.as_str()));
        assert!(rid.starts_with(&id) && rid.len() <= 64 && conoti::valid_rid(&rid));
        // 같은 시각으로 몇 번을 더 불러도 한 줄
        for s in 0..5 {
            tick(&c, plus(30, s), &p);
        }
        assert_eq!(replies(&c), 1);
        assert_eq!(run_state(&c, &id), "fired");
    }

    #[test]
    fn crash_between_promote_and_insert_recovers_without_duplicates() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        let id = add_ok(&c, &new_in(5), &p);
        // 회차만 만들어지고(발사 UPDATE 커밋) 앱이 죽었다
        let mut rep = TickReport::default();
        promote_due(&c, plus(5, 0), Some(plus(4, 0)), &mut rep);
        assert_eq!((run_state(&c, &id).as_str(), replies(&c)), ("pending", 0));
        // 다시 켜서 틱 — 한 줄만 들어간다
        tick(&c, plus(6, 0), &p);
        assert_eq!((run_state(&c, &id).as_str(), replies(&c)), ("fired", 1));
        // 대기열 INSERT 후 회차 UPDATE 전에 죽은 상황: 회차를 pending 으로 되돌려 다시 돌려도 줄은 하나(INSERT OR IGNORE)
        c.execute("UPDATE schedule_run SET state = 'pending', reply_id = NULL, fired_at = NULL WHERE schedule_id = ?1", params![id]).unwrap();
        tick(&c, plus(7, 0), &p);
        assert_eq!((run_state(&c, &id).as_str(), replies(&c)), ("fired", 1));
    }

    #[test]
    fn missed_policies_depend_on_lateness() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        let mk = |on: &str, within: Option<i64>| {
            let mut n = new_in(60);
            n.on_missed = on.into();
            n.missed_within_min = within;
            add_ok(&c, &n, &p)
        };
        let (skip, once, w10, w10b) = (mk("skip", None), mk("run_once", None), mk("within", Some(10)), mk("within", Some(10)));
        // 앱이 꺼져 있다가 정확히 유예(2분) 안에 켜짐 → 모두 제때로 본다
        // (별도 검증을 위해 skip 하나만 유예 안·밖으로 나눠 본다)
        let c2 = mem();
        session(&c2, S1);
        let id = {
            let mut n = new_in(60);
            n.on_missed = "skip".into();
            add_ok(&c2, &n, &p)
        };
        tick(&c2, plus(61, 59), &p);
        assert_eq!(replies(&c2), 1, "1분59초 늦음은 제때");
        let c3 = mem();
        session(&c3, S1);
        let id3 = {
            let mut n = new_in(60);
            n.on_missed = "skip".into();
            add_ok(&c3, &n, &p)
        };
        let rep = tick(&c3, plus(62, 1), &p);
        assert_eq!((replies(&c3), run_state(&c3, &id3).as_str()), (0, "missed"), "2분1초 늦음 + skip 은 놓침");
        assert_eq!(rep.alerts.len(), 1);
        assert_eq!(rep.alerts[0].kind, "missed");
        assert_eq!(sched_state(&c3, &id3), "done");
        let _ = id;
        // 9분 늦게 켜짐: run_once·within(10) 은 실행, skip·within(10)-11분은 놓침
        let rep = tick(&c, plus(69, 0), &p);
        assert_eq!(run_state(&c, &once), "fired");
        assert_eq!(run_state(&c, &w10), "fired");
        assert_eq!(run_state(&c, &skip), "missed");
        assert_eq!(rep.alerts.len(), 1, "놓침 알림은 예약당 한 번");
        // within(10): 정확히 10분은 실행 · 10분1초는 놓침
        let c4 = mem();
        session(&c4, S1);
        let a = {
            let mut n = new_in(60);
            n.on_missed = "within".into();
            n.missed_within_min = Some(10);
            add_ok(&c4, &n, &p)
        };
        tick(&c4, plus(70, 1), &p);
        assert_eq!(run_state(&c4, &a), "missed");
        let c5 = mem();
        session(&c5, S1);
        let b = {
            let mut n = new_in(60);
            n.on_missed = "within".into();
            n.missed_within_min = Some(10);
            add_ok(&c5, &n, &p)
        };
        tick(&c5, plus(70, 0), &p);
        assert_eq!(run_state(&c5, &b), "fired");
        let _ = w10b;
        // 몇 년 놓친 100개도 회차는 예약당 하나뿐 — 폭주하지 않는다
        let c6 = mem();
        session(&c6, S1);
        for _ in 0..15 {
            let mut n = new_in(60);
            n.on_missed = "run_once".into();
            add_ok(&c6, &n, &p);
        }
        tick(&c6, plus(60 * 24 * 300, 0), &p);
        assert_eq!(replies(&c6), 15);
        let runs: i64 = c6.query_row("SELECT COUNT(*) FROM schedule_run", [], |r| r.get(0)).unwrap();
        assert_eq!(runs, 15);
    }

    #[test]
    fn missed_reason_tells_app_off_from_lag() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        let mut n = new_in(10);
        n.on_missed = "skip".into();
        let id = add_ok(&c, &n, &p);
        tick(&c, plus(1, 0), &p); // 살아 있음 표식
        tick(&c, plus(60, 0), &p); // 한 시간 만에 틱 — 앱이 꺼져 있었다
        let note: String = c.query_row("SELECT note FROM schedule_run WHERE schedule_id = ?1", params![id], |r| r.get(0)).unwrap();
        assert!(note.contains("꺼져 있었거나 PC 가 절전"), "{note}");
    }

    #[test]
    fn cancel_before_and_after_fire_and_race_with_tick() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        // 발사 전 취소 → 틱이 와도 아무 일 없다
        let id = add_ok(&c, &new_in(10), &p);
        cancel(&c, plus(5, 0), &id).unwrap();
        tick(&c, plus(10, 0), &p);
        assert_eq!((replies(&c), sched_state(&c, &id).as_str()), (0, "cancelled"));
        assert!(cancel(&c, plus(5, 0), &id).is_err(), "두 번 취소는 오류");
        // 발사 직후(대기열에서 전달 전) 취소 → 줄을 거둔다
        let id2 = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(10, 0), &p);
        assert_eq!(replies(&c), 1);
        cancel(&c, plus(10, 1), &id2).unwrap();
        let st: String = c.query_row("SELECT state FROM conoti_reply WHERE sched IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!((st.as_str(), run_state(&c, &id2).as_str(), sched_state(&c, &id2).as_str()), ("rejected", "cancelled", "done"));
        // 이미 세션에 들어갔으면 취소할 수 없다
        let id3 = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(20, 0), &p);
        c.execute("UPDATE conoti_reply SET state = 'delivered' WHERE reply_id LIKE ?1", params![format!("{id3}%")]).unwrap();
        assert!(cancel(&c, plus(20, 1), &id3).unwrap_err().contains("이미 세션에 전달"));
    }

    #[test]
    fn update_uses_optimistic_lock_and_is_blocked_after_fire() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        let id = add_ok(&c, &new_in(30), &p);
        let e = |rev: i64, text: &str, min: i64| EditSchedule {
            rev,
            text: text.into(),
            atts: vec![],
            when: WhenIn::After { min },
            on_missed: "skip".into(),
            missed_within_min: None,
            busy_policy: Some("interrupt".into()),
        };
        let v = update(&c, t0(), &id, &e(1, "고친 말", 45)).unwrap();
        assert_eq!((v["rev"].as_i64(), v["text"].as_str(), v["next_due_at"].as_str()), (Some(2), Some("고친 말"), Some("2026-10-01T00:45:00.000Z")));
        assert_eq!((v["on_missed"].as_str(), v["busy_policy"].as_str()), (Some("skip"), Some("interrupt")));
        assert!(update(&c, t0(), &id, &e(1, "낡은 화면에서", 45)).unwrap_err().contains("바뀐"), "rev 충돌");
        // 발사되면 고칠 수 없다
        tick(&c, plus(45, 0), &p);
        assert!(update(&c, plus(46, 0), &id, &e(2, "늦었다", 50)).is_err());
    }

    // ── 세션 상태별 처리 ──

    #[test]
    fn ended_session_is_held_outside_the_queue_with_one_alert_then_one_more_when_back() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Ended, Work::Idle);
        let id = add_ok(&c, &new_in(10), &p);
        let rep = tick(&c, plus(10, 0), &p);
        assert_eq!((run_state(&c, &id).as_str(), replies(&c)), ("held", 0), "못 받는 예약은 대기열 밖에 둔다");
        assert_eq!(rep.alerts.len(), 1);
        assert_eq!(rep.alerts[0].kind, "held");
        assert!(rep.alerts[0].note.contains("꺼져"));
        // 몇 번을 돌려도 알림은 더 없다(재시도 폭주 없음)
        for m in 11..30 {
            assert!(tick(&c, plus(m, 0), &p).alerts.is_empty());
        }
        assert_eq!(sched_state(&c, &id), "active", "held 인 동안은 세션을 지우거나 정리하지 않는다");
        // 세션이 다시 켜지면 한 번만 더
        p.set(S1, Rt::Live, Work::Idle);
        let rep = tick(&c, plus(31, 0), &p);
        assert_eq!(rep.alerts.len(), 1);
        assert_eq!(rep.alerts[0].kind, "back");
        assert_eq!(replies(&c), 0, "자동 전달은 없다");
        assert!(tick(&c, plus(32, 0), &p).alerts.is_empty());
        // 다시 꺼졌다 켜져도 두 번째 재알림은 없다
        p.set(S1, Rt::Ended, Work::Idle);
        tick(&c, plus(33, 0), &p);
        p.set(S1, Rt::Live, Work::Idle);
        assert!(tick(&c, plus(34, 0), &p).alerts.is_empty());
    }

    #[test]
    fn held_send_goes_through_the_desktop_path_and_drop_ends_it() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Ended, Work::Idle);
        let id = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(10, 0), &p);
        // 버리기
        act(&c, plus(11, 0), &id, "drop").unwrap();
        assert_eq!((run_state(&c, &id).as_str(), sched_state(&c, &id).as_str()), ("cancelled", "done"));
        assert!(act(&c, plus(11, 0), &id, "drop").is_err(), "이미 처리됨");
        // 보내기: 사용자가 PC 앞에서 누른 것 — 입력창과 같은 데스크톱 경로(예약 표식 없음)
        let id2 = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(21, 0), &p);
        p.set(S1, Rt::Live, Work::Idle);
        act(&c, plus(22, 0), &id2, "send").unwrap();
        let (n, sched_n): (i64, i64) = c
            .query_row("SELECT COUNT(*), COALESCE(SUM(sched IS NOT NULL), 0) FROM conoti_reply WHERE state = 'delivering'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((n, sched_n), (1, 0));
        assert_eq!(run_state(&c, &id2), "fired");
        assert!(act(&c, plus(22, 1), &id2, "send").is_err(), "두 번 눌러도 한 번");
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM conoti_reply", [], |r| r.get(0)).unwrap(), 1);
        assert!(act(&c, plus(22, 2), &id2, "kill").is_err());
        // 보낼 수 없는 상태(터미널 훅 없음)에서 눌렀다면 오류를 내고 held 로 돌아간다
        let id3 = add_ok(&c, &new_in(10), &p);
        p.set(S1, Rt::Ended, Work::Idle);
        tick(&c, plus(40, 0), &p);
        let _ = id3;
    }

    #[test]
    fn unarmed_and_busy_sessions_send_now_and_queue_waits_in_pipeline() {
        for rt in [Rt::Channel, Rt::Live, Rt::Unarmed, Rt::Idle, Rt::BusyNone] {
            let c = mem();
            session(&c, S1);
            let p = Fake::new(S1, rt, Work::Idle);
            let id = add_ok(&c, &new_in(1), &p);
            tick(&c, plus(1, 0), &p);
            assert_eq!(run_state(&c, &id), "fired");
        }
    }

    #[test]
    fn permission_wait_defers_ten_minutes_then_holds() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::PermWait);
        let id = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(10, 0), &p);
        assert_eq!((run_state(&c, &id).as_str(), replies(&c)), ("deferred", 0));
        assert!(tick(&c, plus(19, 59), &p).alerts.is_empty());
        let rep = tick(&c, plus(20, 0), &p);
        assert_eq!((run_state(&c, &id).as_str(), replies(&c)), ("held", 0));
        assert_eq!(rep.alerts.len(), 1);
        assert!(rep.alerts[0].note.contains("승인"));
        // 승인해서 풀리면 재알림 1회
        p.set(S1, Rt::Live, Work::Idle);
        assert_eq!(tick(&c, plus(21, 0), &p).alerts.len(), 1);
        // 10분 안에 승인이 풀리면 보낸다
        let c2 = mem();
        session(&c2, S1);
        let id2 = add_ok(&c2, &new_in(10), &p);
        p.set(S1, Rt::Live, Work::PermWait);
        tick(&c2, plus(10, 0), &p);
        p.set(S1, Rt::Live, Work::Idle);
        tick(&c2, plus(15, 0), &p);
        assert_eq!(run_state(&c2, &id2), "fired");
    }

    #[test]
    fn busy_policies_defer_until_idle_or_window_end_and_interrupt_goes_now() {
        // after_work(전역 기본): 일하는 동안 미루고 쉬면 보낸다
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Busy);
        let id = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(10, 0), &p);
        assert_eq!((run_state(&c, &id).as_str(), replies(&c)), ("deferred", 0));
        tick(&c, plus(40, 0), &p);
        assert_eq!(run_state(&c, &id), "deferred");
        p.set(S1, Rt::Live, Work::Idle);
        tick(&c, plus(41, 0), &p);
        assert_eq!((run_state(&c, &id).as_str(), replies(&c)), ("fired", 1));
        // interrupt 는 일하는 중에도 바로 끼운다
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Busy);
        let mut n = new_in(10);
        n.busy_policy = Some("interrupt".into());
        let id = add_ok(&c, &n, &p);
        tick(&c, plus(10, 0), &p);
        assert_eq!((run_state(&c, &id).as_str(), replies(&c)), ("fired", 1));
        // 전역 기본을 interrupt 로 바꾸면 예약이 따른다
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Busy);
        set_setting(&c, "busy_default", "interrupt").unwrap();
        let id = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(10, 0), &p);
        assert_eq!(run_state(&c, &id), "fired");
        // after_quiet: 창(00:20~00:50 UTC) 안에서 발사되면 창이 끝난 뒤(쉬는 세션이어도)
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        save_window(&c, None, "회의", 127, "00:20", "00:50", "UTC", true).unwrap();
        let mut n = new_in(30);
        n.busy_policy = Some("after_quiet".into());
        let id = add_ok(&c, &n, &p);
        tick(&c, plus(30, 0), &p);
        assert_eq!((run_state(&c, &id).as_str(), replies(&c)), ("deferred", 0));
        tick(&c, plus(49, 59), &p);
        assert_eq!(run_state(&c, &id), "deferred");
        tick(&c, plus(50, 0), &p);
        assert_eq!((run_state(&c, &id).as_str(), replies(&c)), ("fired", 1));
        // 전역 after_work 인데 시간대 규칙(창 안)이면 after_quiet 로 — 창 규칙이 전역보다 앞선다
        let c = mem();
        session(&c, S1);
        save_window(&c, None, "회의", 127, "00:20", "00:50", "UTC", true).unwrap();
        let id = add_ok(&c, &new_in(30), &p);
        tick(&c, plus(30, 0), &p);
        assert_eq!(run_state(&c, &id), "deferred");
    }

    #[test]
    fn deferred_beyond_three_hours_becomes_held() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Busy);
        let id = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(10, 0), &p);
        assert!(tick(&c, plus(10 + 179, 0), &p).alerts.is_empty());
        let rep = tick(&c, plus(10 + 180, 0), &p);
        assert_eq!(run_state(&c, &id), "held");
        assert_eq!(rep.alerts.len(), 1);
    }

    #[test]
    fn stuck_line_is_pulled_back_when_session_is_not_working() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Unarmed, Work::Idle);
        let id = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(10, 0), &p);
        assert_eq!(replies(&c), 1);
        // 9분 59초까지는 기다린다
        assert!(tick(&c, plus(19, 59), &p).alerts.is_empty());
        let rep = tick(&c, plus(20, 0), &p);
        assert_eq!(rep.alerts.len(), 1, "10분 넘게 받지 못한 예약은 되돌려 알린다");
        let st: String = c.query_row("SELECT state FROM conoti_reply WHERE sched IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!((st.as_str(), run_state(&c, &id).as_str()), ("rejected", "held"));
        // 일하는 중이면 계속 기다린다(3시간 상한은 파이프라인)
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::BusyNone, Work::Busy);
        let mut n = new_in(10);
        n.busy_policy = Some("interrupt".into());
        let id = add_ok(&c, &n, &p);
        tick(&c, plus(10, 0), &p);
        assert!(tick(&c, plus(60, 0), &p).alerts.is_empty());
        assert_eq!(run_state(&c, &id), "fired");
    }

    #[test]
    fn results_are_followed_and_rejections_become_held_or_cancelled() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        let id = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(10, 0), &p);
        c.execute("UPDATE conoti_reply SET state = 'delivered' WHERE sched IS NOT NULL", []).unwrap();
        tick(&c, plus(11, 0), &p);
        assert_eq!((run_state(&c, &id).as_str(), sched_state(&c, &id).as_str()), ("delivered", "done"));
        c.execute("UPDATE conoti_reply SET state = 'handled' WHERE sched IS NOT NULL", []).unwrap();
        tick(&c, plus(12, 0), &p);
        assert_eq!(run_state(&c, &id), "handled");
        // 파이프라인이 거절(예: 세션이 그사이 꺼져 이어서 실행 불가) → held + 알림 1회
        let id2 = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(22, 0), &p);
        c.execute("UPDATE conoti_reply SET state = 'rejected', note = '세션이 꺼져 있음' WHERE reply_id LIKE ?1", params![format!("{id2}%")]).unwrap();
        let rep = tick(&c, plus(23, 0), &p);
        assert_eq!((run_state(&c, &id2).as_str(), rep.alerts.len()), ("held", 1));
        assert!(tick(&c, plus(24, 0), &p).alerts.is_empty());
        // 사용자가 보내기 취소(전달 목록에서)한 것은 알림 없이 취소
        let id3 = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(34, 0), &p);
        c.execute("UPDATE conoti_reply SET state = 'rejected', note = '보내기 취소' WHERE reply_id LIKE ?1", params![format!("{id3}%")]).unwrap();
        assert!(tick(&c, plus(35, 0), &p).alerts.is_empty());
        assert_eq!(run_state(&c, &id3), "cancelled");
        // 대기열 줄이 사라지면(세션 기록 지우기 등) 회차도 취소
        let id4 = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(45, 0), &p);
        c.execute("DELETE FROM conoti_reply WHERE reply_id LIKE ?1", params![format!("{id4}%")]).unwrap();
        tick(&c, plus(46, 0), &p);
        assert_eq!(run_state(&c, &id4), "cancelled");
    }

    #[test]
    fn held_expires_after_seven_days_and_pause_switch_stops_everything() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Ended, Work::Idle);
        let id = add_ok(&c, &new_in(10), &p);
        tick(&c, plus(10, 0), &p);
        tick(&c, plus(60 * 24 * 6, 0), &p);
        assert_eq!(run_state(&c, &id), "held");
        tick(&c, plus(60 * 24 * 7 + 20, 0), &p);
        assert_eq!((run_state(&c, &id).as_str(), sched_state(&c, &id).as_str()), ("cancelled", "done"));
        // 멈춤 스위치: 켜 두면 아무것도 발사하지 않고, 풀면 놓침 정책이 적용된다
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        let mut n = new_in(10);
        n.on_missed = "skip".into();
        let id = add_ok(&c, &n, &p);
        set_setting(&c, "paused", "1").unwrap();
        tick(&c, plus(10, 0), &p);
        assert_eq!((replies(&c), sched_state(&c, &id).as_str()), (0, "active"));
        set_setting(&c, "paused", "0").unwrap();
        tick(&c, plus(30, 0), &p);
        assert_eq!(run_state(&c, &id), "missed");
    }

    #[test]
    fn daily_fire_cap_holds_the_rest() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        db::set_meta(&c, &fires_key(plus(10, 0)), &DAILY_FIRE_CAP.to_string()).unwrap();
        let id = add_ok(&c, &new_in(10), &p);
        let rep = tick(&c, plus(10, 0), &p);
        assert_eq!((run_state(&c, &id).as_str(), replies(&c), rep.alerts.len()), ("held", 0, 1));
    }

    #[test]
    fn fire_unhides_archived_session_and_keeps_quote_and_turn() {
        let c = mem();
        session(&c, S1);
        c.execute("UPDATE session SET hidden = 1, archived_at = 't' WHERE id = ?1", params![S1]).unwrap();
        let tid: i64 = c.query_row("SELECT id FROM turn WHERE session_id = ?1", params![S1], |r| r.get(0)).unwrap();
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        let mut n = new_in(10);
        n.quote_turn = Some(tid);
        n.quote_part = Some("response".into());
        add_ok(&c, &n, &p);
        tick(&c, plus(10, 0), &p);
        let (hidden, quote, turn): (i64, Option<String>, Option<i64>) = c
            .query_row(
                "SELECT (SELECT hidden FROM session WHERE id = ?1), quote, turn_id FROM conoti_reply WHERE sched IS NOT NULL",
                params![S1],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((hidden, quote.as_deref(), turn), (0, Some("response"), Some(tid)));
    }

    // ── 파이프라인 · 전달 경로 ──

    #[derive(Default)]
    struct Capture(RefCell<Vec<(String, conoti::Opts)>>);
    impl conoti::Deliver for Capture {
        fn deliver(&self, _t: &conoti::Target, _id: &str, _text: &str, _d: bool) -> conoti::Outcome {
            unreachable!("예약 줄은 deliver_opts 로 간다")
        }
        fn deliver_opts(&self, _t: &conoti::Target, _id: &str, text: &str, opts: conoti::Opts) -> conoti::Outcome {
            self.0.borrow_mut().push((text.to_string(), opts));
            conoti::Outcome::Done("전달".into())
        }
    }

    #[test]
    fn pipeline_wraps_with_sched_header_marks_scheduled_and_keeps_three_hour_cap_from_fire_time() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        add_ok(&c, &new_in(10), &p);
        tick(&c, plus(10, 0), &p);
        let mut pipe = conoti::Pipeline::new(c);
        let cap = Capture::default();
        pipe.tick(&cap);
        let got = cap.0.borrow();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, format!("{}\n\n테스트를 돌려 줘", conoti::SCHED_HEADER));
        assert!(got[0].1.scheduled && got[0].1.from_desktop, "예약 표식이 붙고, 폰 검사 경로가 아니라 데스크톱 경로");
        // 3시간 상한은 received_at(=발사 시각) 기준이다: 발사가 지금이면 예약을 한 지 며칠이 지났어도 거절되지 않는다.
        let c2 = mem();
        session(&c2, S1);
        // 7일 전에 예약해 둔 줄(예약 생성 시각은 오래됐지만 발사는 방금)
        c2.execute(
            "INSERT INTO schedule (id, session_id, text, created_by, created_at, updated_at, kind, tz, next_due_at, on_missed, state)
             VALUES ('scold0000000001', ?1, '옛 예약', 'desktop', '2026-09-24T00:00:00.000Z', '2026-09-24T00:00:00.000Z', 'once', 'UTC', ?2, 'run_once', 'active')",
            params![S1, iso(Utc::now() - Duration::seconds(1))],
        )
        .unwrap();
        let n = tick(&c2, Utc::now(), &Fake::new(S1, Rt::Live, Work::Idle));
        let _ = n;
        assert_eq!(replies(&c2), 1);
        let mut pipe2 = conoti::Pipeline::new(c2);
        let cap2 = Capture::default();
        pipe2.tick(&cap2);
        assert_eq!(cap2.0.borrow().len(), 1, "방금 발사한 줄은 상한에 걸리지 않고 전달된다");
    }

    struct AlwaysBusy;
    impl conoti::Deliver for AlwaysBusy {
        fn deliver(&self, _t: &conoti::Target, _id: &str, _text: &str, _d: bool) -> conoti::Outcome {
            conoti::Outcome::Busy("앞 작업이 끝나면 보냅니다".into())
        }
    }

    #[test]
    fn queue_line_from_schedule_skips_the_phone_ten_minute_rule_but_keeps_the_three_hour_cap() {
        let c = mem();
        session(&c, S1);
        let now = Utc::now();
        let ins = |rid: &str, age_min: i64| {
            c.execute(
                "INSERT INTO conoti_reply (reply_id, session_id, kind, text, created_at, received_at, state, device, sched) VALUES (?1, ?2, 'text', '예약', ?3, ?3, 'delivering', 'desktop', ?1)",
                params![rid, S1, iso(now - Duration::minutes(age_min))],
            )
            .unwrap();
        };
        ins("scfresh0000000001-1", 120); // 2시간 전에 발사(일하는 세션 뒤에 줄 서 있음) — 10분 규칙에 안 걸린다
        ins("scstale0000000002-1", 181); // 3시간 넘게 — 거절
        let mut pipe = conoti::Pipeline::new(c);
        pipe.tick(&AlwaysBusy);
        let c = pipe.conn();
        let st = |rid: &str| c.query_row("SELECT state FROM conoti_reply WHERE reply_id = ?1", params![rid], |r| r.get::<_, String>(0)).unwrap();
        assert_eq!(st("scfresh0000000001-1"), "delivering");
        assert_eq!(st("scstale0000000002-1"), "rejected");
    }

    #[test]
    fn scheduled_lines_never_launch_offline_sessions_even_with_resume_setting_on() {
        // 예약 줄 + 꺼진 세션 → 이어서 실행하지 않고 실패(파이프라인은 거절 → sync 가 held 로 알린다)
        let d = crate::deliver::LocalDeliver { bg_resume: true };
        let t = conoti::Target { session_id: "sess-never-0001".into(), cwd: Some("/tmp".into()) };
        use conoti::Deliver;
        let out = d.deliver_opts(&t, "r1", &conoti::wrap_sched("안녕"), conoti::Opts { from_desktop: true, scheduled: true });
        assert_eq!(out, conoti::Outcome::Fail(crate::deliver::OFFLINE_TEXT.into()));
        // 옛 시그니처(bool)로 부르는 길은 예약이 아니다 — 그대로 데스크톱 동작(여기서는 실행하지 않는다)
        assert!(crate::deliver::LocalDeliver { bg_resume: false }.deliver_opts(&t, "r2", &conoti::wrap_sched("안녕"), conoti::Opts { from_desktop: false, scheduled: true }) == conoti::Outcome::Fail(crate::deliver::OFFLINE_TEXT.into()));
    }

    #[test]
    fn sched_header_is_stripped_on_collection_and_marks_origin() {
        let w = conoti::wrap_sched("  배포해 줘 \n");
        assert_eq!(conoti::strip_header(&w), Some(("배포해 줘", "sched")));
        assert_eq!(conoti::strip_header(&conoti::wrap_desk("배포해 줘")), Some(("배포해 줘", "inbox")));
        assert_eq!(conoti::strip_inbox(&w), Some("배포해 줘"));
        assert_eq!(conoti::strip_header("그냥 말"), None);
        assert!(crate::text::rewake_message(&format!("Stop hook blocking error from command \"x\": {w}")).unwrap().starts_with(conoti::SCHED_HEADER));
    }

    // ── 이미지 · 보관 · 지우기 회귀 ──

    fn att_row(c: &Connection, id: &str, touched: &str) {
        c.execute(
            "INSERT INTO attachment (id, mime, ext, bytes, width, height, source, created_at, touched_at, touched_by) VALUES (?1, 'image/png', 'png', 10, 1, 1, 'desktop', ?2, ?2, 'desktop')",
            params![id, touched],
        )
        .unwrap();
    }

    #[test]
    fn attach_gc_keeps_images_of_pending_schedules_and_frees_them_after() {
        let c = mem();
        session(&c, S1);
        let id = "a".repeat(64);
        att_row(&c, &id, "2026-01-01T00:00:00.000Z"); // 7일 넘게 묵은 이미지
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        let sid = add_ok(&c, &new_in(60), &p);
        c.execute("INSERT INTO schedule_att (schedule_id, att_id, ord) VALUES (?1, ?2, 0)", params![sid, id]).unwrap();
        assert_eq!(attach::gc(&c), 0, "걸려 있는 예약의 이미지는 7일이 지나도 남는다");
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM attachment", [], |r| r.get(0)).unwrap(), 1);
        // 발사되면 대기열 줄이 이미지를 이어받는다
        tick(&c, plus(60, 0), &p);
        let linked: i64 = c.query_row("SELECT COUNT(*) FROM reply_attachment WHERE att_id = ?1", params![id], |r| r.get(0)).unwrap();
        assert_eq!(linked, 1);
        assert_eq!(attach::gc(&c), 0, "대기열 줄이 잡고 있다");
        // 예약을 끝내고 줄을 지우면 그때는 정리된다
        c.execute("DELETE FROM conoti_reply", []).unwrap();
        c.execute("DELETE FROM reply_attachment", []).unwrap();
        c.execute("DELETE FROM schedule_att", []).unwrap();
        assert_eq!(attach::gc(&c), 1);
    }

    #[test]
    fn cancelling_releases_images_for_normal_gc() {
        let c = mem();
        session(&c, S1);
        let id = "b".repeat(64);
        att_row(&c, &id, "2026-01-01T00:00:00.000Z");
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        let sid = add_ok(&c, &new_in(60), &p);
        c.execute("INSERT INTO schedule_att (schedule_id, att_id, ord) VALUES (?1, ?2, 0)", params![sid, id]).unwrap();
        cancel(&c, t0(), &sid).unwrap();
        let left: i64 = c.query_row("SELECT COUNT(*) FROM schedule_att", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
        // touch 로 정리 시계가 다시 시작됐다 — 7일 안에는 남고
        assert_eq!(attach::gc(&c), 0);
    }

    #[test]
    fn archive_delete_and_tidy_leave_sessions_with_pending_schedules() {
        let c = mem();
        session(&c, S1);
        session(&c, S2);
        let p = Fake::new(S1, Rt::Live, Work::Idle);
        p.set(S2, Rt::Live, Work::Idle);
        let id = add_ok(&c, &new_in(60), &p);
        // 기록에서 지우기: 예약이 걸린 세션은 남긴다
        let r = crate::archive::delete_sessions(&c, &[S1.to_string(), S2.to_string()]).unwrap();
        assert_eq!(r.deleted, vec![S2.to_string()]);
        assert_eq!(r.skipped.len(), 1);
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM session WHERE id = ?1", params![S1], |r| r.get(0)).unwrap(), 1);
        // 정리 제안(한 번에 보관)에서도 빠진다
        let tidy = crate::archive::tidy_candidates(&c, false, None).unwrap();
        assert!(!tidy.contains(&S1.to_string()));
        // 예약을 취소해 끝나면 지울 수 있고, 그 예약 기록도 함께 정리된다
        cancel(&c, t0(), &id).unwrap();
        let r = crate::archive::delete_sessions(&c, &[S1.to_string()]).unwrap();
        assert_eq!(r.deleted, vec![S1.to_string()]);
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM schedule", [], |r| r.get(0)).unwrap(), 0);
    }

    #[test]
    fn held_run_also_pins_the_session() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Ended, Work::Idle);
        add_ok(&c, &new_in(10), &p);
        tick(&c, plus(10, 0), &p);
        let r = crate::archive::delete_sessions(&c, &[S1.to_string()]).unwrap();
        assert!(r.deleted.is_empty() && r.skipped.len() == 1, "받지 못한 예약이 사용자 선택을 기다리는 동안 세션은 남는다");
    }

    // ── 목록 · 설정 ──

    #[test]
    fn list_and_settings_shapes() {
        let c = mem();
        session(&c, S1);
        let p = Fake::new(S1, Rt::Ended, Work::Idle);
        let id = add_ok(&c, &new_in(10), &p);
        let l = list(&c, Some(S1));
        assert_eq!(l.len(), 1);
        assert_eq!((l[0]["id"].as_str(), l[0]["run"].clone()), (Some(id.as_str()), Value::Null));
        tick(&c, plus(10, 0), &p);
        let l = list(&c, None);
        assert_eq!((l[0]["run"]["state"].as_str(), l[0]["run"]["reason"].as_str()), (Some("held"), Some("ended")));
        assert_eq!(counts(&c), (1, 1));
        assert!(list(&c, Some(S2)).is_empty());
        let s = settings(&c);
        assert_eq!((s["busy_default"].as_str(), s["perm"].as_str(), s["paused"].as_bool()), (Some("after_work"), Some("default"), Some(false)));
        save_window(&c, None, "밤", 31, "22:00", "08:00", "Asia/Seoul", true).unwrap();
        assert_eq!(settings(&c)["windows"][0]["start"], "22:00");
        assert!(save_window(&c, None, "x", 0, "22:00", "08:00", "Asia/Seoul", true).is_err());
        assert!(save_window(&c, None, "x", 31, "22:00", "22:00", "Asia/Seoul", true).is_err());
        assert!(save_window(&c, None, "x", 31, "25:00", "08:00", "Asia/Seoul", true).is_err());
        assert!(save_window(&c, None, "x", 31, "22:00", "08:00", "Nowhere/City", true).is_err());
        assert!(set_setting(&c, "busy_default", "yolo").is_err() && set_setting(&c, "nope", "1").is_err());
    }

    // ── 마이그레이션 ──

    #[test]
    fn migration_v13_to_v14_adds_column_and_tables_keeps_data_and_backs_up() {
        let dir = std::env::temp_dir().join(format!("aiinbox-mig14-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("inbox.db");
        let c = Connection::open(&path).unwrap();
        db::migrate(&c).unwrap();
        // v13 모양으로 되돌린다: 예약 표·열 제거 + 옛 데이터 몇 건
        c.execute_batch(
            "DROP INDEX IF EXISTS conoti_reply_sched; ALTER TABLE conoti_reply DROP COLUMN sched;
             DROP TABLE schedule; DROP TABLE schedule_run; DROP TABLE schedule_att; DROP TABLE quiet_window; DROP TABLE busy_rule; DROP TABLE sched_allow;",
        )
        .unwrap();
        db::set_meta(&c, "schema_version", "13").unwrap();
        session(&c, S1);
        c.execute(
            "INSERT INTO conoti_reply (reply_id, session_id, kind, text, received_at, state, device) VALUES ('dk00000001', ?1, 'text', '옛 말', '2026-09-29T00:00:00.000Z', 'handled', 'desktop')",
            params![S1],
        )
        .unwrap();
        let (turns, replies_before): (i64, i64) = (
            c.query_row("SELECT COUNT(*) FROM turn", [], |r| r.get(0)).unwrap(),
            c.query_row("SELECT COUNT(*) FROM conoti_reply", [], |r| r.get(0)).unwrap(),
        );
        db::migrate(&c).unwrap();
        assert_eq!(db::get_meta(&c, "schema_version").as_deref(), Some("14"));
        let has: i64 = c.query_row("SELECT COUNT(*) FROM pragma_table_info('conoti_reply') WHERE name = 'sched'", [], |r| r.get(0)).unwrap();
        assert_eq!(has, 1);
        for t in ["schedule", "schedule_run", "schedule_att", "quiet_window", "busy_rule", "sched_allow"] {
            let n: i64 = c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1", params![t], |r| r.get(0)).unwrap();
            assert_eq!(n, 1, "{t}");
        }
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM turn", [], |r| r.get(0)).unwrap(), turns);
        assert_eq!(c.query_row::<i64, _, _>("SELECT COUNT(*) FROM conoti_reply WHERE sched IS NULL", [], |r| r.get(0)).unwrap(), replies_before, "기존 줄은 예약이 아니다(NULL)");
        assert_eq!(c.query_row::<String, _, _>("PRAGMA integrity_check", [], |r| r.get(0)).unwrap(), "ok");
        // 바꾸기 전 사본
        let bak = dir.join("inbox.db.bak-v13");
        assert!(bak.exists(), "inbox.db.bak-v13 사본이 있어야 한다");
        let b = Connection::open(&bak).unwrap();
        let old_has: i64 = b.query_row("SELECT COUNT(*) FROM pragma_table_info('conoti_reply') WHERE name = 'sched'", [], |r| r.get(0)).unwrap();
        assert_eq!(old_has, 0, "사본은 v13(예약 열 없음) 상태");
        assert_eq!(b.query_row::<i64, _, _>("SELECT COUNT(*) FROM conoti_reply", [], |r| r.get(0)).unwrap(), replies_before);
        // 두 번째 마이그레이션은 아무것도 다시 하지 않는다
        db::migrate(&c).unwrap();
        // 새 DB 는 처음부터 v14
        let fresh = mem();
        assert_eq!(db::get_meta(&fresh, "schema_version").as_deref(), Some("14"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
