//! 수집 엔진 — 대화 기록(JSONL)을 증분으로 읽어 "요청 1건 = turn 1행" 으로 조립한다.
//! Claude Code(~/.claude/projects) 와 Codex(~/.codex/sessions — 해석은 `ingest_codex.rs`) 를 같은 표로 모은다.
//!
//! Claude Code 쪽 신호는 세 갈래다.
//!   1. 대화 기록 파일(정본) — 요청·중간 보고·도구·토큰·턴 종료(turn_duration)·요약(away_summary)
//!   2. 훅 스풀 — Stop·Notification(권한 대기)·SessionEnd 를 즉시 알려 준다
//!   3. 살아 있는 세션 등록부(~/.claude/sessions/{pid}.json) — busy/idle, 프로세스 생존
//!
//! 요청의 경계: 사람이 친 줄(origin.kind=human) 또는 다른 세션이 보낸 메시지(peer).
//! 백그라운드 작업 완료 알림(task-notification)·한도 리셋 후 이어가기(auto-continuation)는
//! 새 요청이 아니라 **같은 요청의 연장**이다 — 결과 보고는 대개 그 뒤에 나온다.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::{db, paths, text, time};

#[path = "ingest_codex.rs"]
mod cx;

/// 파서 규칙이 바뀌면 올린다 → 다음 실행 때 백필 기간 안의 파일을 처음부터 다시 읽는다
/// (읽음·별표 같은 사용자 상태는 upsert 가 건드리지 않으므로 보존된다).
/// 15: Windows 에서 도구 대상 경로(`turn_touch`)를 드라이브 문자째(`C:/…`) 모은다(`tags::collect_touched`)
/// 16: 앱에서 보낸 말(입력창·예약·폰 답)은 답 없이 끝나도 숨기지 않는다 — 그렇게 사라진 말을 다시 보이게
pub const PARSER_VERSION: &str = "16";

const FINISHED: &[&str] = &["done", "interrupted", "stopped"];
/// 이보다 오래 조용하고 프로세스도 없으면 멈춘 것으로 본다.
const DEAD_QUIET_MS: i64 = 120_000;
/// 앱이 꺼져 있던 동안 끝난 요청까지 한꺼번에 알리지 않는다.
const NOTIFY_WINDOW_MS: i64 = 10 * 60_000;

// ── 대화 기록 한 줄 (필요한 칸만) ──────────────────────────────────────────

#[derive(Deserialize, Default)]
struct Origin {
    kind: Option<String>,
}

#[derive(Deserialize, Default)]
struct Message {
    model: Option<String>,
    content: Option<Value>,
    usage: Option<Value>,
}

#[derive(Deserialize, Default)]
struct Line {
    #[serde(rename = "type")]
    typ: Option<String>,
    subtype: Option<String>,
    uuid: Option<String>,
    timestamp: Option<String>,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: bool,
    #[serde(rename = "isMeta", default)]
    is_meta: bool,
    #[serde(rename = "isCompactSummary", default)]
    is_compact_summary: bool,
    #[serde(rename = "isApiErrorMessage", default)]
    is_api_error: bool,
    origin: Option<Origin>,
    #[serde(rename = "promptSource")]
    prompt_source: Option<String>,
    message: Option<Message>,
    #[serde(rename = "requestId")]
    request_id: Option<String>,
    effort: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "gitBranch")]
    git_branch: Option<String>,
    version: Option<String>,
    #[serde(rename = "customTitle")]
    custom_title: Option<String>,
    #[serde(rename = "agentName")]
    agent_name: Option<String>,
    content: Option<Value>,
    #[serde(rename = "durationMs")]
    duration_ms: Option<i64>,
    #[serde(rename = "pendingBackgroundAgentCount")]
    pending_bg: Option<i64>,
    #[serde(rename = "totalCostUSD")]
    total_cost: Option<f64>,
    #[serde(rename = "totalLinesAdded")]
    lines_added: Option<i64>,
    #[serde(rename = "totalLinesRemoved")]
    lines_removed: Option<i64>,
    /// 작업 도중 들어온 입력(`queued_command`) 등
    attachment: Option<Value>,
}

// ── 요청 하나를 조립하는 누산기 ─────────────────────────────────────────────

#[derive(Clone, Default)]
struct Usage {
    input: i64,
    output: i64,
    thinking: i64,
    cc5m: i64,
    cc1h: i64,
    cache_read: i64,
    web_search: i64,
    web_fetch: i64,
}

impl Usage {
    fn from(v: &Value) -> Usage {
        // 비정상적으로 큰 값이 합계를 넘치게 하지 않도록 호출 하나당 100억 토큰으로 자른다
        let n = |x: Option<&Value>| x.and_then(Value::as_i64).unwrap_or(0).clamp(0, 10_000_000_000);
        let cc = v.get("cache_creation");
        let (cc5m, cc1h) = match cc {
            Some(c) if c.is_object() => (
                n(c.get("ephemeral_5m_input_tokens")),
                n(c.get("ephemeral_1h_input_tokens")),
            ),
            _ => (n(v.get("cache_creation_input_tokens")), 0),
        };
        let stu = v.get("server_tool_use");
        Usage {
            input: n(v.get("input_tokens")),
            output: n(v.get("output_tokens")),
            thinking: n(v.get("output_tokens_details").and_then(|d| d.get("thinking_tokens"))),
            cc5m,
            cc1h,
            cache_read: n(v.get("cache_read_input_tokens")),
            web_search: n(stu.and_then(|s| s.get("web_search_requests"))),
            web_fetch: n(stu.and_then(|s| s.get("web_fetch_requests"))),
        }
    }
    /// 스트리밍 중간본이 같은 requestId 로 여러 줄 온다 → 칸마다 최댓값
    fn merge(&mut self, o: &Usage) {
        self.input = self.input.max(o.input);
        self.output = self.output.max(o.output);
        self.thinking = self.thinking.max(o.thinking);
        self.cc5m = self.cc5m.max(o.cc5m);
        self.cc1h = self.cc1h.max(o.cc1h);
        self.cache_read = self.cache_read.max(o.cache_read);
        self.web_search = self.web_search.max(o.web_search);
        self.web_fetch = self.web_fetch.max(o.web_fetch);
    }
    fn context(&self) -> i64 {
        self.input + self.cc5m + self.cc1h + self.cache_read
    }
}

#[derive(Clone)]
struct Step {
    at: Option<String>,
    kind: &'static str,
    name: Option<String>,
    text: String,
}

#[derive(Clone)]
struct Sub {
    agent_type: String,
    description: String,
    background: bool,
    started_at: Option<String>,
    ended_at: Option<String>,
}

#[derive(Clone)]
struct PlanItem {
    text: String,
    status: String,
}

#[derive(Clone)]
struct TurnAcc {
    uuid: String,
    prompt_at: String,
    prompt_text: String,
    slash: Option<String>,
    origin: String,
    source: Option<String>,
    peer_name: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,

    understanding: Option<String>,
    saw_tool: bool,
    plan: Vec<PlanItem>,
    response: Option<String>,
    summary: Option<String>,
    first_reply_at: Option<String>,
    last_activity_at: Option<String>,
    stopped_at: Option<String>,
    interrupted_at: Option<String>,
    pending_bg: i64,
    active_ms: i64,

    calls: HashMap<String, Usage>,
    call_order: Vec<String>,
    models: BTreeMap<String, i64>,
    efforts: BTreeMap<String, i64>,
    tools: BTreeMap<String, i64>,
    files: BTreeMap<String, i64>,
    /// 읽거나 고친 경로·명령 속 절대경로(태깅 신호, `tags::collect_touched`)
    touched: BTreeSet<String>,
    subs: Vec<Sub>,
    sub_by_tool: HashMap<String, usize>,
    pending_ask: Option<String>,
    errors: i64,
    task_notes: i64,
    steps: Vec<Step>,
    /// 다음 요청이 시작돼 이 요청이 닫혔다
    closed: bool,
    /// 이번 실행에서 DB 에 이미 쓴 step 수 (step 은 append-only 라 새 것만 넣는다)
    steps_saved: usize,
    dirty: bool,
    /// 모델이 일하는 도중에 사용자가 보낸 말 — 원래 요청과 따로 한 쌍으로 보인다(숨기지 않는다)
    side: bool,
    /// 그 말을 받은 직후 모델 응답의 requestId — 이 응답 안의 글만 답으로 친다
    reply_req: Option<String>,
    /// Codex 턴 — 끝남 판정이 다르다(턴 끝 줄이 기록에 분명히 있고, 살아 있음은 잠금 파일로 본다)
    codex: bool,
    /// 대기 훅이 넣은 말 — 첫 답 첫머리의 받은 말 인용(`text::strip_echo`)을 뗀다
    echo: bool,
    /// 작업 중에 보낸 말: 받은 직후 응답이 지나갔다(바로 단 글 모으기 끝)
    answered: bool,
    /// 작업 중에 보낸 말: 그 말을 받은 앞 요청의 상태 — 앞 요청이 끝날 때까지 "작업 중", 끝나면 그 최종 답을 받는다
    follow: Option<String>,
}

impl TurnAcc {
    fn new(uuid: String, at: String, line: &Line, text: String, slash: Option<String>, origin: &str, peer: Option<String>) -> TurnAcc {
        TurnAcc {
            uuid,
            prompt_at: at,
            prompt_text: text,
            slash,
            origin: origin.to_string(),
            source: line.prompt_source.clone(),
            peer_name: peer,
            cwd: line.cwd.clone(),
            branch: line.git_branch.clone(),
            understanding: None,
            saw_tool: false,
            plan: Vec::new(),
            response: None,
            summary: None,
            first_reply_at: None,
            last_activity_at: None,
            stopped_at: None,
            interrupted_at: None,
            pending_bg: 0,
            active_ms: 0,
            calls: HashMap::new(),
            call_order: Vec::new(),
            models: BTreeMap::new(),
            efforts: BTreeMap::new(),
            tools: BTreeMap::new(),
            files: BTreeMap::new(),
            touched: BTreeSet::new(),
            subs: Vec::new(),
            sub_by_tool: HashMap::new(),
            pending_ask: None,
            errors: 0,
            task_notes: 0,
            steps: Vec::new(),
            closed: false,
            steps_saved: 0,
            dirty: true,
            side: false,
            reply_req: None,
            codex: false,
            echo: false,
            answered: false,
            follow: None,
        }
    }

    fn touch(&mut self, at: &Option<String>) {
        if let Some(a) = at {
            if self.last_activity_at.as_deref().map(|l| a.as_str() > l).unwrap_or(true) {
                self.last_activity_at = Some(a.clone());
            }
        }
        self.dirty = true;
    }

    fn step(&mut self, at: &Option<String>, kind: &'static str, name: Option<String>, text: String) {
        self.steps.push(Step { at: at.clone(), kind, name, text });
        self.dirty = true;
    }

    fn api_calls(&self) -> i64 {
        self.calls.len() as i64
    }

    /// 앱(입력창·예약·폰 답)에서 보낸 말
    fn from_app(&self) -> bool {
        matches!(self.origin.as_str(), "inbox" | "sched") || self.prompt_text.starts_with(crate::conoti::REPLY_HEADER)
    }

    /// 모델이 아무것도 내놓지 못한 요청(호출 0·답 없음)
    fn no_answer(&self) -> bool {
        self.api_calls() == 0 && self.response.is_none()
    }

    fn totals(&self) -> Usage {
        let mut t = Usage::default();
        for u in self.calls.values() {
            t.input += u.input;
            t.output += u.output;
            t.thinking += u.thinking;
            t.cc5m += u.cc5m;
            t.cc1h += u.cc1h;
            t.cache_read += u.cache_read;
            t.web_search += u.web_search;
            t.web_fetch += u.web_fetch;
        }
        t
    }

    fn context_tokens(&self) -> i64 {
        self.call_order
            .last()
            .and_then(|id| self.calls.get(id))
            .map(Usage::context)
            .unwrap_or(0)
    }

    fn top(map: &BTreeMap<String, i64>) -> Option<String> {
        map.iter().max_by_key(|(_, n)| **n).map(|(k, _)| k.clone())
    }

    fn quiet_since_stop(&self) -> bool {
        match (&self.stopped_at, &self.last_activity_at) {
            (Some(s), Some(a)) => s.as_str() >= a.as_str(),
            (Some(_), None) => true,
            _ => false,
        }
    }

    fn interrupted_last(&self) -> bool {
        match (&self.interrupted_at, &self.last_activity_at) {
            (Some(i), Some(a)) => i.as_str() >= a.as_str(),
            (Some(_), None) => true,
            _ => false,
        }
    }
}

// ── 세션 쪽 정보 ────────────────────────────────────────────────────────────

#[derive(Default)]
struct SessionPatch {
    title: Option<String>,
    agent_name: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    version: Option<String>,
    model: Option<String>,
    first_at: Option<String>,
    last_at: Option<String>,
    cost_usd: Option<f64>,
    lines_added: Option<i64>,
    lines_removed: Option<i64>,
}

#[derive(Clone, Default)]
struct Live {
    alive: bool,
    status: Option<String>,
    name: Option<String>,
    pid: Option<i64>,
    /// 훅으로 받은 마지막 Stop / Notification
    stop_hook_at: Option<String>,
    notify_at: Option<String>,
    notify_msg: Option<String>,
}

struct FileState {
    session_id: String,
    size: u64,
    mtime_ms: i64,
    offset: u64,
    read_pos: u64,
    skipped: bool,
    primed: bool,
    open: Option<TurnAcc>,
    /// 작업 도중 들어온 사용자 말 — 모델의 다음 글을 답으로 붙이고 닫는다
    side: Vec<TurnAcc>,
    last_status: Option<(String, bool)>,
    /// Codex 기록 파일이면 그쪽 해석 상태
    cx: Option<cx::CxState>,
    /// 앱이 모델을 부르려고 띄운 세션의 기록(작업 폴더가 `llm-scratch`) — 세션으로 만들지 않는다
    internal: bool,
}

impl FileState {
    fn fresh(session_id: String, mtime_ms: i64, offset: u64, skipped: bool, codex: bool) -> FileState {
        FileState {
            session_id,
            // 0 으로 둬야 켜자마자 한 번 다시 읽어 열린 요청을 메모리에 올린다
            size: 0,
            mtime_ms,
            offset,
            read_pos: offset,
            skipped,
            primed: false,
            open: None,
            side: Vec::new(),
            last_status: None,
            cx: codex.then(cx::CxState::default),
            internal: false,
        }
    }

    #[cfg(test)]
    fn for_test(codex: bool) -> FileState {
        FileState::fresh("t".into(), 0, 0, false, codex)
    }
}

// ── 바깥으로 알리는 것 ──────────────────────────────────────────────────────

#[derive(Clone, serde::Serialize)]
pub struct Finished {
    pub turn_id: i64,
    pub session_id: String,
    pub status: String,
    pub needs_input: bool,
    pub text: String,
}

#[derive(Default)]
pub struct Report {
    pub changed: HashSet<String>,
    pub finished: Vec<Finished>,
    /// 시간 예산을 넘겨 아직 못 읽은 파일이 남았다 (첫 백필 중)
    pub working: bool,
    /// 이번 틱에 읽은 파일 수
    pub processed: usize,
    /// 이번 틱에 새로 /clear 로 끝난 것으로 표시한 세션(화면에 결정 안내를 띄운다)
    pub cleared: Vec<String>,
    /// 만료·되살림으로 세션이 지워지거나 표시가 바뀌었다
    pub lifecycle_changed: bool,
    /// 요청 태그가 새로 붙었다(훅 기록 ↔ 대화 기록이 이어짐) — 태그 목록·개수를 다시 읽게 한다
    pub tags_changed: bool,
}

pub struct Ingestor {
    conn: Connection,
    installed_at: String,
    backfill_days: i64,
    files: HashMap<PathBuf, FileState>,
    live: HashMap<String, Live>,
    tick_no: u64,
    /// Codex 기록도 모으나(설정 `codex_enabled`, 기본 켬)
    codex_enabled: bool,
    /// Codex 를 처음 모으기 시작한 때 — 그 전에 끝난 Codex 요청은 본 것으로 친다(업데이트하자마자 안 읽음이 쏟아지지 않게)
    codex_since: String,
    /// Codex 스레드 이름(session_index.jsonl)과 그 파일의 수정 시각
    codex_names: HashMap<String, String>,
    codex_index_mtime: i64,
    /// 지금 열려 있는 Codex 스레드(잠금 파일). None = 이 플랫폼에선 알 수 없다
    codex_live: Option<HashSet<String>>,
    /// 세션 표에 마지막으로 쓴 Codex 세션의 live_status
    codex_written: HashMap<String, Option<String>>,
    /// 방금 SessionEnd(reason=clear) 를 받은 세션과 그 시각(ms) — 뒤따르는 SessionStart(source=clear) 를 잇는다
    last_clear: Option<(String, i64)>,
    /// 앱에서 보낸 말의 첫 답 전 유예(`conoti::empty_grace_ms`) — 그동안은 등록부 idle 을 끝남으로 보지 않고, 답 없는 끝남은 알리지 않는다
    empty_grace_ms: i64,
}

impl Ingestor {
    pub fn new(conn: Connection) -> Ingestor {
        if db::get_meta(&conn, "parser_version").as_deref() != Some(PARSER_VERSION) {
            let _ = conn.execute("DELETE FROM source_file", []);
            let _ = db::set_meta(&conn, "parser_version", PARSER_VERSION);
        }
        let installed_at = db::get_meta(&conn, "installed_at").unwrap_or_else(time::now_iso);
        clean_stale_spool();
        let backfill_days = db::setting_i64(&conn, "backfill_days", 7);
        let codex_enabled = db::setting_i64(&conn, "codex_enabled", 1) != 0;
        // 처음 Codex 를 모으기 시작한 때 — 모으기를 꺼 둔 동안은 정하지 않는다(나중에 켜면 그때부터)
        let codex_since = db::get_meta(&conn, "codex_since").unwrap_or_else(|| {
            let now = time::now_iso();
            if codex_enabled {
                let _ = db::set_meta(&conn, "codex_since", &now);
            }
            now
        });
        let codex_root = paths::codex_sessions_dir();
        let mut files = HashMap::new();
        if let Ok(mut st) = conn.prepare("SELECT path, session_id, size, mtime_ms, offset, skipped FROM source_file") {
            let rows = st.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            });
            if let Ok(rows) = rows {
                for (path, sid, size, mtime, offset, skipped) in rows.flatten() {
                    let p = PathBuf::from(&path);
                    let codex = p.starts_with(&codex_root);
                    if codex && !codex_enabled {
                        continue;
                    }
                    let sid = sid.unwrap_or_else(|| stem(&p));
                    let mut st = FileState::fresh(sid, mtime, offset.max(0) as u64, skipped != 0, codex);
                    if let Some(c) = st.cx.as_mut() {
                        // 세션 ID 를 비워 둔 Codex 파일 = 하위 에이전트 스레드(건너뛴다)
                        c.skip = st.session_id.is_empty();
                    }
                    files.insert(p, st);
                    let _ = size;
                }
            }
        }
        // 앞 실행이 남긴 Codex 세션의 "실행 중" 표시는 지운다 — 이번 실행이 잠금 파일로 다시 정한다
        let _ = conn.execute("UPDATE session SET live_status = NULL WHERE agent = 'codex' AND live_status IS NOT NULL", []);
        let empty_grace_ms = crate::conoti::empty_grace_ms(&conn);
        Ingestor {
            conn,
            installed_at,
            backfill_days,
            files,
            live: HashMap::new(),
            tick_no: 0,
            codex_enabled,
            codex_since,
            codex_names: HashMap::new(),
            codex_index_mtime: -1,
            codex_live: None,
            codex_written: HashMap::new(),
            last_clear: None,
            empty_grace_ms,
        }
    }

    pub fn tick(&mut self) -> Report {
        self.tick_no += 1;
        let mut rep = Report::default();
        self.drain_spool(&mut rep);
        self.refresh_registry(&mut rep);
        self.refresh_codex_live();
        self.scan_transcripts(&mut rep);
        self.scan_codex(&mut rep);
        self.refresh_codex(&mut rep);
        self.recheck_open(&mut rep);
        self.notify_deferred(&mut rep);
        // 만료·되살림 검사 — 1분에 한 번(첫 틱 포함)
        if self.tick_no % 40 == 1 {
            crate::tags::sweep_hints(&self.conn, chrono::Utc::now().timestamp_millis());
            let r = crate::lifecycle::sweep(&self.conn, chrono::Utc::now().timestamp_millis());
            if !r.revived.is_empty() || !r.purged.is_empty() {
                rep.lifecycle_changed = true;
                rep.changed.extend(r.revived);
                rep.changed.extend(r.purged);
            }
        }
        rep
    }

    /// 미뤄 둔 "답 없는 끝남" 알림(`flush_turn` 의 `defer`) — 앱에서 보낸 말이 첫 답 전 유예가 지나도 답 없이 끝나 있으면 한 번 알린다.
    /// 그사이 답이 붙었거나(정상 알림이 나간다) 사용자가 읽었으면 알리지 않는다
    fn notify_deferred(&mut self, rep: &mut Report) {
        let now = chrono::Utc::now().timestamp_millis();
        let due = time::iso_from_ms(now - self.empty_grace_ms);
        let recent = time::iso_from_ms(now - NOTIFY_WINDOW_MS - self.empty_grace_ms);
        let rows: Vec<(i64, String, String, String)> = {
            let Ok(mut st) = self.conn.prepare(
                "SELECT id, session_id, status, COALESCE(prompt_text, '') FROM turn
                  WHERE notified = 0 AND hidden = 0 AND read_at IS NULL AND status IN ('done', 'stopped')
                    AND api_calls = 0 AND TRIM(COALESCE(response_text, '')) = ''
                    AND (origin IN ('inbox', 'sched') OR prompt_text LIKE ?3)
                    AND prompt_at <= ?1 AND prompt_at >= ?2
                  LIMIT 20",
            ) else {
                return;
            };
            let Ok(rows) = st.query_map(params![due, recent, format!("{}%", crate::conoti::REPLY_HEADER)], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            }) else {
                return;
            };
            rows.flatten().collect()
        };
        for (turn_id, sid, status, prompt) in rows {
            let _ = self.conn.execute("UPDATE turn SET notified = 1 WHERE id = ?1", params![turn_id]);
            rep.finished.push(Finished { turn_id, session_id: sid.clone(), status, needs_input: false, text: no_answer_text(&prompt) });
            rep.changed.insert(sid);
        }
    }

    // ── 훅 스풀 ──────────────────────────────────────────────────────────

    fn drain_spool(&mut self, rep: &mut Report) {
        let dir = paths::spool_dir();
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        let mut entries: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
            .collect();
        if entries.is_empty() {
            return;
        }
        entries.sort();
        let began = self.conn.execute_batch("BEGIN").is_ok();
        for path in entries {
            // 훅이 만드는 파일은 수 KB — 그보다 훨씬 크면 우리 것이 아니다
            if std::fs::metadata(&path).map(|m| m.len() > 1024 * 1024).unwrap_or(true) {
                let _ = std::fs::remove_file(&path);
                continue;
            }
            let Ok(bytes) = std::fs::read(&path) else { continue };
            let _ = std::fs::remove_file(&path);
            let Ok(v) = serde_json::from_slice::<Value>(&bytes) else { continue };
            self.apply_hook(&v, rep);
        }
        if began {
            let _ = self.conn.execute_batch("COMMIT");
        }
    }

    /// 훅 이벤트 한 건을 DB 에 반영한다(스풀 파일에서 읽은 JSON).
    fn apply_hook(&mut self, v: &Value, rep: &mut Report) {
        let event = v.get("hook_event_name").and_then(Value::as_str).unwrap_or("?").to_string();
        let sid = v.get("session_id").and_then(Value::as_str).map(str::to_string);
        let at = v
            .get("received_at_ms")
            .and_then(Value::as_i64)
            .map(time::iso_from_ms)
            .unwrap_or_else(time::now_iso);
        let mut detail = serde_json::Map::new();
        for k in ["message", "notification_type", "title", "source", "reason", "agent_type", "permission_mode"] {
            match v.get(k) {
                Some(Value::String(x)) => {
                    detail.insert(k.into(), Value::String(text::safe(x, 1000)));
                }
                Some(x @ (Value::Bool(_) | Value::Number(_))) => {
                    detail.insert(k.into(), x.clone());
                }
                _ => {}
            }
        }
        let _ = self.conn.execute(
            "INSERT INTO hook_event (session_id, event, at, detail) VALUES (?1, ?2, ?3, ?4)",
            params![sid, event, at, Value::Object(detail.clone()).to_string()],
        );
        let _ = db::set_meta(&self.conn, "hook_last_at", &at);
        let Some(sid) = sid else { return };
        self.ensure_session(&sid, v.get("transcript_path").and_then(Value::as_str));
        let live = self.live.entry(sid.clone()).or_default();
        match event.as_str() {
            "Stop" => live.stop_hook_at = Some(at.clone()),
            "Notification" => {
                let msg = text::safe(detail.get("message").and_then(Value::as_str).unwrap_or(""), 500);
                live.notify_at = Some(at.clone());
                live.notify_msg = Some(msg.clone());
                let _ = self.conn.execute(
                    "UPDATE session SET notify_at = ?2, notify_msg = ?3 WHERE id = ?1",
                    params![sid, at, msg],
                );
            }
            "SessionEnd" => {
                let reason = detail.get("reason").and_then(Value::as_str).map(str::to_string);
                let _ = self.conn.execute(
                    "UPDATE session SET ended_at = ?2, end_reason = ?3 WHERE id = ?1",
                    params![sid, at, reason],
                );
                if reason.as_deref() == Some("clear") {
                    if crate::lifecycle::on_clear(&self.conn, &sid, &at) {
                        rep.cleared.push(sid.clone());
                    }
                    self.last_clear = Some((sid.clone(), time::ms_of_iso(&at).unwrap_or(0)));
                }
            }
            "UserPromptSubmit" => {
                // 요청 시점 태깅: 재료(#태그·지문·저장소 폴더)로 태그를 정해 남기고, 같은 요청이 이미 기록에 있으면 바로 잇는다
                if let Some(hp) = crate::tags::HookPrompt::from_spool(v) {
                    let (_, _, attached) = crate::tags::record_prompt(&self.conn, &hp);
                    rep.tags_changed = true;
                    let _ = attached;
                }
            }
            "SessionStart" if detail.get("source").and_then(Value::as_str) == Some("clear") => {
                // 방금 /clear 된 세션의 후속 — SessionEnd 와 몇십 ms 안에 온다
                if let Some((old, ms)) = self.last_clear.take() {
                    let now = time::ms_of_iso(&at).unwrap_or(0);
                    if (now - ms).abs() <= crate::lifecycle::PAIR_WINDOW_MS {
                        crate::lifecycle::link_successor(&self.conn, &old, &sid);
                    }
                }
            }
            _ => {}
        }
        rep.changed.insert(sid);
    }

    fn ensure_session(&self, sid: &str, transcript: Option<&str>) {
        let _ = self.conn.execute(
            "INSERT INTO session (id, transcript_path) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET transcript_path = COALESCE(session.transcript_path, excluded.transcript_path)",
            params![sid, transcript],
        );
    }

    // ── 살아 있는 세션 등록부 ────────────────────────────────────────────────

    fn refresh_registry(&mut self, rep: &mut Report) {
        let mut seen: HashMap<String, (i64, Option<String>, Option<String>)> = HashMap::new();
        if let Ok(rd) = std::fs::read_dir(paths::registry_dir()) {
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()) != Some("json") {
                    continue;
                }
                let Ok(bytes) = std::fs::read(&p) else { continue };
                let Ok(v) = serde_json::from_slice::<Value>(&bytes) else { continue };
                let (Some(sid), Some(pid)) = (
                    v.get("sessionId").and_then(Value::as_str),
                    v.get("pid").and_then(Value::as_i64),
                ) else {
                    continue;
                };
                if !pid_alive(pid) {
                    continue;
                }
                let name = if v.get("nameSource").and_then(Value::as_str) == Some("user") {
                    v.get("name").and_then(Value::as_str).map(str::to_string)
                } else {
                    None
                };
                // 앱이 모델을 부르려고 띄운 `claude -p`(llm.rs) — 사용자 세션이 아니다
                if v.get("cwd").and_then(Value::as_str).is_some_and(paths::is_internal_cwd) {
                    continue;
                }
                let status = crate::deliver::input_status(v.get("status").and_then(Value::as_str).map(str::to_string));
                seen.insert(sid.to_string(), (pid, status, name));
            }
        }
        let known: Vec<String> = self.live.keys().cloned().collect();
        let mut all: HashSet<String> = known.into_iter().collect();
        all.extend(seen.keys().cloned());
        for sid in all {
            let entry = self.live.entry(sid.clone()).or_default();
            let (alive, pid, status, name) = match seen.get(&sid) {
                Some((pid, st, nm)) => (true, Some(*pid), st.clone(), nm.clone()),
                None => (false, None, None, None),
            };
            let changed = entry.alive != alive || entry.status != status || entry.name != name || entry.pid != pid;
            if !changed {
                continue;
            }
            entry.alive = alive;
            entry.status = status.clone();
            entry.pid = pid;
            entry.name = name.clone();
            if alive {
                self.ensure_session_quiet(&sid);
            }
            let _ = self.conn.execute(
                "UPDATE session SET live_status = ?2, live_pid = ?3, live_name = COALESCE(?4, live_name) WHERE id = ?1",
                params![sid, status, pid, name],
            );
            rep.changed.insert(sid);
        }
    }

    fn ensure_session_quiet(&self, sid: &str) {
        let _ = self.conn.execute("INSERT OR IGNORE INTO session (id) VALUES (?1)", params![sid]);
    }

    // ── 대화 기록 파일 ──────────────────────────────────────────────────────

    fn scan_transcripts(&mut self, rep: &mut Report) {
        let root = paths::projects_dir();
        let Ok(dirs) = std::fs::read_dir(&root) else { return };
        let cutoff_ms = now_ms() - self.backfill_days.max(0) * 86_400_000;
        let mut todo: Vec<(PathBuf, u64, i64)> = Vec::new();
        for d in dirs.flatten() {
            let dp = d.path();
            if !dp.is_dir() {
                continue;
            }
            let Ok(files) = std::fs::read_dir(&dp) else { continue };
            for f in files.flatten() {
                let p = f.path();
                if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Ok(md) = f.metadata() else { continue };
                let size = md.len();
                let mtime = md
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                match self.files.get(&p) {
                    Some(st) if st.size == size && st.mtime_ms == mtime && st.primed => {}
                    Some(_) => todo.push((p, size, mtime)),
                    None => {
                        let sid = stem(&p);
                        let skipped = mtime < cutoff_ms;
                        let offset = if skipped { size } else { 0 };
                        self.files.insert(p.clone(), FileState::fresh(sid, mtime, offset, skipped, false));
                        todo.push((p, size, mtime));
                    }
                }
            }
        }
        // 최근 파일부터 — 켜자마자 지금 돌고 있는 세션이 먼저 보이게
        todo.sort_by_key(|(_, _, m)| -m);
        // 첫 백필이 길어도 화면이 먼저 채워지도록 한 번에 1.2초까지만 — 남은 파일은 다음 틱에
        let budget = std::time::Instant::now();
        for (p, size, mtime) in todo {
            if budget.elapsed() > std::time::Duration::from_millis(1200) {
                rep.working = true;
                break;
            }
            rep.processed += 1;
            if let Some(sid) = self.process_file(&p, size, mtime, rep) {
                rep.changed.insert(sid);
            }
        }
    }

    // ── Codex ───────────────────────────────────────────────────────────────

    /// Codex 기록: sessions/YYYY/MM/DD/rollout-<시각>-<스레드 ID>.jsonl. 규칙은 Claude 쪽과 같다
    /// (백필 기간 밖 파일은 그 뒤로 늘어난 부분만, 최근 파일부터, 한 번에 시간 예산만큼).
    fn scan_codex(&mut self, rep: &mut Report) {
        if !self.codex_enabled {
            return;
        }
        let cutoff_ms = now_ms() - self.backfill_days.max(0) * 86_400_000;
        let mut found: Vec<(PathBuf, u64, i64)> = Vec::new();
        walk_rollouts(&paths::codex_sessions_dir(), 0, &mut found);
        let mut todo: Vec<(PathBuf, u64, i64)> = Vec::new();
        for (p, size, mtime) in found {
            match self.files.get(&p) {
                Some(st) if st.size == size && st.mtime_ms == mtime && st.primed => {}
                Some(_) => todo.push((p, size, mtime)),
                None => {
                    let Some(sid) = cx::thread_id_of(&p) else { continue };
                    let skipped = mtime < cutoff_ms;
                    let offset = if skipped { size } else { 0 };
                    let mut st = FileState::fresh(sid, mtime, offset, skipped, true);
                    // 늘어난 부분만 따라갈 파일도 첫 줄(session_meta)로 하위 에이전트 스레드인지는 먼저 본다
                    if skipped && codex_subagent_file(&p) {
                        if let Some(c) = st.cx.as_mut() {
                            c.skip = true;
                        }
                    }
                    self.files.insert(p.clone(), st);
                    todo.push((p, size, mtime));
                }
            }
        }
        todo.sort_by_key(|(_, _, m)| -m);
        let budget = std::time::Instant::now();
        for (p, size, mtime) in todo {
            if budget.elapsed() > std::time::Duration::from_millis(800) {
                rep.working = true;
                break;
            }
            rep.processed += 1;
            if let Some(sid) = self.process_file(&p, size, mtime, rep) {
                rep.changed.insert(sid);
            }
        }
    }

    /// 지금 열려 있는 Codex 스레드 — 상태 판정 전에(작업 중 / 멈춤)
    fn refresh_codex_live(&mut self) {
        if !self.codex_enabled {
            return;
        }
        self.codex_live = if cfg!(windows) { None } else { Some(crate::codex::live_threads()) };
    }

    /// Codex 스레드 이름(session_index.jsonl)과 세션 표의 live_status(busy · idle · 없음)
    fn refresh_codex(&mut self, rep: &mut Report) {
        if !self.codex_enabled {
            return;
        }
        let idx = paths::codex_index_path();
        let mtime = std::fs::metadata(&idx)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        if mtime != self.codex_index_mtime {
            self.codex_index_mtime = mtime;
            let mut names: HashMap<String, String> = HashMap::new();
            let small = std::fs::metadata(&idx).map(|m| m.len() < 16 * 1024 * 1024).unwrap_or(false);
            if small {
                if let Ok(body) = std::fs::read_to_string(&idx) {
                    for line in body.lines() {
                        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
                        let (Some(id), Some(name)) = (v.get("id").and_then(Value::as_str), v.get("thread_name").and_then(Value::as_str)) else {
                            continue;
                        };
                        let name = text::safe(name.trim(), 120);
                        if crate::channel::valid_session_id(id) && !name.is_empty() {
                            names.insert(id.to_string(), name); // 뒤 줄이 최신
                        }
                    }
                }
            }
            for (id, name) in &names {
                if self.codex_names.get(id) == Some(name) {
                    continue;
                }
                let n = self
                    .conn
                    .execute(
                        "UPDATE session SET title = ?2 WHERE id = ?1 AND agent = 'codex' AND COALESCE(title, '') <> ?2",
                        params![id, name],
                    )
                    .unwrap_or(0);
                if n > 0 {
                    rep.changed.insert(id.clone());
                }
            }
            self.codex_names = names;
        }
        let Some(live) = self.codex_live.clone() else { return };
        let mut sids: HashSet<String> = self.codex_written.keys().cloned().collect();
        sids.extend(live.iter().cloned());
        for sid in sids {
            let want = if live.contains(&sid) {
                let busy = self
                    .files
                    .values()
                    .find(|st| st.cx.is_some() && st.session_id == sid)
                    .and_then(|st| st.last_status.as_ref())
                    .is_some_and(|(s, _)| matches!(s.as_str(), "running" | "waiting" | "background"));
                Some(if busy { "busy" } else { "idle" }.to_string())
            } else {
                None
            };
            if self.codex_written.get(&sid) == Some(&want) {
                continue;
            }
            let n = self
                .conn
                .execute("UPDATE session SET live_status = ?2, live_pid = NULL WHERE id = ?1 AND agent = 'codex'", params![sid, want])
                .unwrap_or(0);
            if n == 0 && want.is_some() {
                continue; // 세션 행이 아직 없다(기록을 읽기 전) — 다음 틱에
            }
            self.codex_written.insert(sid.clone(), want);
            rep.changed.insert(sid);
        }
    }

    fn process_file(&mut self, path: &Path, size: u64, mtime: i64, rep: &mut Report) -> Option<String> {
        let mut st = self.files.remove(path)?;
        if !st.primed {
            st.read_pos = st.offset;
            st.open = None;
            st.side.clear();
            st.primed = true;
        }
        if size < st.read_pos {
            // 파일이 줄었다(다시 쓰였다) — 처음부터
            st.read_pos = 0;
            st.offset = 0;
            st.open = None;
            st.side.clear();
        }
        let mut closed: Vec<TurnAcc> = Vec::new();
        let mut patch = SessionPatch::default();
        let codex = st.cx.is_some();
        let result = read_lines(path, st.read_pos, |line_start, raw| {
            if codex {
                cx::feed(&mut st, line_start, raw, &mut closed, &mut patch);
            } else {
                feed(&mut st, line_start, raw, &mut closed, &mut patch);
            }
        });
        if let Ok(end) = result {
            st.read_pos = end;
        }
        if st.open.is_none() {
            st.offset = st.read_pos;
        }
        st.size = size;
        st.mtime_ms = mtime;

        if patch.cwd.as_deref().is_some_and(paths::is_internal_cwd) {
            st.internal = true;
        }
        if st.internal || st.cx.as_ref().is_some_and(|c| c.skip) {
            // Codex 하위 에이전트 스레드(또는 앱이 모델을 부르려고 띄운 세션 — llm.rs) — 위치만 기억하고 세션으로 만들지 않는다(세션 ID 를 비워 표시)
            st.session_id.clear();
            st.open = None;
            st.side.clear();
            st.offset = st.read_pos;
            let _ = self.conn.execute(
                "INSERT INTO source_file (path, session_id, size, mtime_ms, offset, skipped)
                 VALUES (?1, '', ?2, ?3, ?4, ?5)
                 ON CONFLICT(path) DO UPDATE SET session_id = '', size = excluded.size, mtime_ms = excluded.mtime_ms,
                     offset = excluded.offset, skipped = excluded.skipped",
                params![path.to_string_lossy(), size as i64, mtime, st.offset as i64, st.skipped as i64],
            );
            self.files.insert(path.to_path_buf(), st);
            return None;
        }

        let sid = st.session_id.clone();
        let tx = self.conn.unchecked_transaction().ok();
        // 요청이 하나도 없는 Codex 기록(가져온 대화의 사본뿐·아직 말이 없는 새 스레드)은 세션으로 만들지 않는다
        let exists = self.session_exists(&sid);
        let empty = codex && closed.is_empty() && st.open.is_none() && st.side.is_empty() && !exists;
        if codex && !empty && !exists && patch.version.is_none() {
            // 이번 읽기가 첫 줄부터가 아니었다(요청 없던 스레드에 턴이 붙음) — 버전·폴더·브랜치는 첫 줄에서
            if let Some(m) = codex_meta(path) {
                let st_of = |k: &str| m.get(k).and_then(Value::as_str).filter(|x| !x.is_empty()).map(str::to_string);
                patch.version = st_of("cli_version");
                patch.cwd = patch.cwd.take().or_else(|| st_of("cwd"));
                patch.branch = patch.branch.take().or_else(|| m.get("git").and_then(|g| g.get("branch")).and_then(Value::as_str).map(str::to_string));
            }
        }
        if !empty {
            self.upsert_session(&sid, path, &patch, codex);
        }
        for acc in closed.iter_mut() {
            self.flush_turn(&sid, acc, rep);
        }
        let mut last = None;
        if let Some(acc) = st.open.as_mut() {
            if acc.dirty {
                last = self.flush_turn(&sid, acc, rep);
            }
        }
        // 아직 답을 못 받은 "작업 중에 보낸 말"도 바로 보이게 — 상태·답은 앞 요청을 따라간다
        self.follow_sides(&sid, &mut st);
        for acc in st.side.iter_mut() {
            if acc.dirty {
                self.flush_turn(&sid, acc, rep);
            }
        }
        if last.is_some() {
            st.last_status = last;
        }
        let _ = self.conn.execute(
            "INSERT INTO source_file (path, session_id, size, mtime_ms, offset, skipped)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET session_id = excluded.session_id, size = excluded.size,
                 mtime_ms = excluded.mtime_ms, offset = excluded.offset, skipped = excluded.skipped",
            params![path.to_string_lossy(), sid, size as i64, mtime, st.offset as i64, st.skipped as i64],
        );
        if let Some(tx) = tx {
            let _ = tx.commit();
        }
        self.files.insert(path.to_path_buf(), st);
        Some(sid)
    }

    /// 파일은 그대로인데 바깥 신호(프로세스 종료·권한 대기·Stop 훅)로 상태가 바뀌는 요청
    fn recheck_open(&mut self, rep: &mut Report) {
        let keys: Vec<PathBuf> = self
            .files
            .iter()
            .filter(|(_, st)| st.open.is_some())
            .map(|(k, _)| k.clone())
            .collect();
        for k in keys {
            let Some(mut st) = self.files.remove(&k) else { continue };
            let sid = st.session_id.clone();
            if let Some(acc) = st.open.as_mut() {
                let now = self.status_of(&sid, acc);
                if st.last_status.as_ref() != Some(&now) {
                    let tx = self.conn.unchecked_transaction().ok();
                    acc.dirty = true;
                    st.last_status = self.flush_turn(&sid, acc, rep);
                    if let Some(tx) = tx {
                        let _ = tx.commit();
                    }
                    rep.changed.insert(sid.clone());
                }
            }
            if self.follow_sides(&sid, &mut st) {
                let tx = self.conn.unchecked_transaction().ok();
                for acc in st.side.iter_mut().filter(|a| a.dirty) {
                    self.flush_turn(&sid, acc, rep);
                }
                if let Some(tx) = tx {
                    let _ = tx.commit();
                }
                rep.changed.insert(sid.clone());
            }
            self.files.insert(k, st);
        }
    }

    fn session_exists(&self, sid: &str) -> bool {
        self.conn
            .query_row("SELECT EXISTS(SELECT 1 FROM session WHERE id = ?1)", params![sid], |r| r.get(0))
            .unwrap_or(false)
    }

    fn upsert_session(&self, sid: &str, path: &Path, p: &SessionPatch, codex: bool) {
        let project_dir = p.cwd.clone();
        // Codex 스레드 이름은 기록 파일이 아니라 session_index.jsonl 에 있다
        let title = if codex { p.title.clone().or_else(|| self.codex_names.get(sid).cloned()) } else { p.title.clone() };
        if codex {
            crate::codex::remember(sid);
        }
        let _ = self.conn.execute(
            "INSERT INTO session (id, transcript_path, project_dir, title, agent_name, git_branch, cc_version, model,
                                  first_at, last_at, cost_usd, lines_added, lines_removed, agent)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(id) DO UPDATE SET
                agent         = COALESCE(excluded.agent, session.agent),
                transcript_path = excluded.transcript_path,
                project_dir   = COALESCE(session.project_dir, excluded.project_dir),
                title         = COALESCE(excluded.title, session.title),
                agent_name    = COALESCE(excluded.agent_name, session.agent_name),
                git_branch    = COALESCE(excluded.git_branch, session.git_branch),
                cc_version    = COALESCE(excluded.cc_version, session.cc_version),
                model         = COALESCE(excluded.model, session.model),
                first_at      = CASE WHEN session.first_at IS NULL OR excluded.first_at < session.first_at
                                     THEN COALESCE(excluded.first_at, session.first_at) ELSE session.first_at END,
                last_at       = CASE WHEN session.last_at IS NULL OR excluded.last_at > session.last_at
                                     THEN COALESCE(excluded.last_at, session.last_at) ELSE session.last_at END,
                cost_usd      = COALESCE(excluded.cost_usd, session.cost_usd),
                lines_added   = COALESCE(excluded.lines_added, session.lines_added),
                lines_removed = COALESCE(excluded.lines_removed, session.lines_removed)",
            params![
                sid,
                path.to_string_lossy(),
                project_dir,
                title,
                p.agent_name,
                p.branch,
                p.version,
                p.model,
                p.first_at,
                p.last_at,
                p.cost_usd,
                p.lines_added,
                p.lines_removed,
                codex.then_some(crate::codex::AGENT)
            ],
        );
    }

    /// Codex 턴의 (status, needs_input). 턴 끝(task_complete)·중단(turn_aborted) 줄이 기록에 분명히 남으므로
    /// 그게 없으면 "작업 중" — 단, 스레드를 연 프로세스가 없고(잠금 파일) 한동안 조용하면 멈춘 것으로 본다.
    fn codex_status(&self, sid: &str, acc: &TurnAcc) -> (String, bool) {
        let has_work = acc.api_calls() > 0 || acc.response.is_some();
        let status = if acc.side && acc.closed {
            "done"
        } else if acc.interrupted_last() {
            "interrupted"
        } else if acc.stopped_at.is_some() {
            if has_work { "done" } else { "stopped" }
        } else if acc.closed {
            // 끝 줄 없이 다음 턴이 시작됐다 — 프로세스가 끊겼다
            "interrupted"
        } else {
            let alive = self.codex_live.as_ref().is_some_and(|l| l.contains(sid));
            // 열려 있는지 알 수 없는 플랫폼이면 조용한 시간으로만(길게) 본다
            let limit = if self.codex_live.is_some() { DEAD_QUIET_MS } else { 30 * 60_000 };
            let quiet = acc
                .last_activity_at
                .as_deref()
                .or(Some(acc.prompt_at.as_str()))
                .and_then(time::age_ms)
                .map(|a| a > limit)
                .unwrap_or(true);
            if !alive && quiet { "stopped" } else { "running" }
        };
        let needs_input = status == "done" && acc.response.as_deref().map(text::asks_user).unwrap_or(false);
        (status.to_string(), needs_input)
    }

    /// 작업 중에 보낸 말들을 앞 요청의 상태에 맞춘다 — 앞 요청이 끝나면 그 최종 답을 받는다. 바뀐 게 있으면 true
    fn follow_sides(&self, sid: &str, st: &mut FileState) -> bool {
        let Some(main) = st.open.as_ref() else { return false };
        let (ms, _) = self.status_of(sid, main);
        let finished = FINISHED.contains(&ms.as_str()) || ms == "background";
        let mut any = false;
        for side in st.side.iter_mut() {
            let before = (side.follow.clone(), side.response.clone(), side.stopped_at.clone());
            side.follow = Some(ms.clone());
            if finished {
                settle_side(side, main);
            }
            if before != (side.follow.clone(), side.response.clone(), side.stopped_at.clone()) {
                side.dirty = true;
                any = true;
            }
        }
        any
    }

    /// (status, needs_input)
    fn status_of(&self, sid: &str, acc: &TurnAcc) -> (String, bool) {
        if acc.codex {
            return self.codex_status(sid, acc);
        }
        let live = self.live.get(sid).cloned().unwrap_or_default();
        let has_work = acc.api_calls() > 0 || acc.response.is_some();
        let mut status: &str;
        if acc.side && !acc.closed {
            if let Some(f) = acc.follow.as_deref() {
                // 앞 요청을 따라간다 — 끝나면(백그라운드 대기 포함) 끝, 아니면 작업 중
                let done = FINISHED.contains(&f) || f == "background";
                let needs = done && acc.response.as_deref().map(text::asks_user).unwrap_or(false);
                return (if done { "done" } else { "running" }.into(), needs);
            }
        }
        if acc.side && acc.closed {
            // 작업 중에 보낸 말은 답 글이 없어도 "멈춤"이 아니다 — 앞 요청 안에서 이어졌다
            return ("done".into(), acc.response.as_deref().map(text::asks_user).unwrap_or(false));
        }
        if acc.closed {
            status = if acc.interrupted_last() {
                "interrupted"
            } else if has_work {
                "done"
            } else {
                "stopped"
            };
        } else if acc.pending_ask.is_some() {
            status = "waiting";
        } else if acc.interrupted_last() {
            status = "interrupted";
        } else if acc.quiet_since_stop() {
            status = if acc.pending_bg > 0 { "background" } else { "done" };
        } else {
            status = "running";
            // 이 요청의 마지막 활동 — 모델이 아직 한 마디도 안 했으면 요청 시각. 그보다 앞선 신호는 앞 요청의 것이다:
            // 세션을 끄기 전에 받은 Stop 훅 · 이어서 띄운 직후의 등록부 idle 을 "끝남"으로 읽으면 전달 스레드가 그 세션을
            // 모델이 움직이기 전에 멈추고 답 없는 요청은 숨김이 됐다(0.10.1 Windows 점검 문제 1 — 0.6.0 부터 있던 결함)
            let since = acc.last_activity_at.as_deref().unwrap_or(acc.prompt_at.as_str());
            // Stop 훅이 마지막 활동 뒤에 왔다 = 모델이 멈췄다
            if live.stop_hook_at.as_deref().is_some_and(|t| t >= since) {
                status = "done";
            }
            // 등록부가 idle = 입력을 기다린다. 앱에서 보낸 말이 아직 아무 활동도 없으면 첫 답 전 유예만큼 기다린다 — 이어서 띄운 세션이
            // 기동하는 동안 등록부가 idle 로 보이면(Windows 의 느린 기동) 5초 만에 끝남·가짜 알림이 됐다(리뷰 필수 2)
            let idle_wait = if acc.from_app() && acc.last_activity_at.is_none() { self.empty_grace_ms } else { 5_000 };
            if live.alive && live.status.as_deref() == Some("idle") && time::age_ms(since).map(|a| a > idle_wait).unwrap_or(true) {
                status = "done";
            }
        }
        if !acc.closed && matches!(status, "running" | "background") {
            // 권한 승인 대기 (Notification 훅)
            if let (Some(at), Some(msg)) = (&live.notify_at, &live.notify_msg) {
                let newer = acc.last_activity_at.as_deref().map(|a| at.as_str() >= a).unwrap_or(true);
                let m = msg.to_lowercase();
                if newer && (m.contains("permission") || m.contains("approve") || m.contains("allow")) {
                    status = "waiting";
                }
            }
        }
        if !acc.closed && matches!(status, "running" | "background" | "waiting") && !live.alive {
            let quiet = acc
                .last_activity_at
                .as_deref()
                .or(Some(acc.prompt_at.as_str()))
                .and_then(time::age_ms)
                .map(|a| a > DEAD_QUIET_MS)
                .unwrap_or(true);
            if quiet {
                status = if has_work { "done" } else { "stopped" };
                if acc.pending_bg > 0 || status == "stopped" {
                    status = "stopped";
                }
            }
        }
        let needs_input = status == "done" && acc.response.as_deref().map(text::asks_user).unwrap_or(false);
        (status.to_string(), needs_input)
    }

    /// 기록에서 지운 요청인가(지운 뒤에 새 활동이 붙지 않은 것). 줄 위치로 만든 ID(pos-…)는 같은 세션에서만 맞춘다.
    fn deleted_before(&self, sid: &str, acc: &TurnAcc) -> bool {
        let deleted_at: Option<String> = self
            .conn
            .query_row(
                "SELECT MAX(deleted_at) FROM turn_deleted
                  WHERE prompt_uuid = ?1 AND (session_id = ?2 OR prompt_uuid NOT LIKE 'pos-%')",
                params![acc.uuid, sid],
                |r| r.get(0),
            )
            .ok()
            .flatten();
        let Some(d) = deleted_at else { return false };
        !acc.last_activity_at.as_deref().is_some_and(|a| a > d.as_str())
    }

    /// 요청 하나를 DB 에 쓴다. 반환: 이번에 계산된 (status, needs_input)
    fn flush_turn(&self, sid: &str, acc: &mut TurnAcc, rep: &mut Report) -> Option<(String, bool)> {
        let (status, needs_input) = self.status_of(sid, acc);
        // 기록에서 지운 요청 — 원본을 다시 읽어도, 이어서 실행한 복사본 세션에 같은 요청이 있어도 쓰지 않는다.
        // 지운 뒤에 새 활동(백그라운드 작업 완료 등)이 붙었으면 다시 받는다. 상태는 돌려준다 — None 이면
        // 부르는 쪽이 "상태가 바뀌었다"로 보고 틱마다 다시 쓰려 한다.
        if self.deleted_before(sid, acc) {
            return Some((status, needs_input));
        }
        let finished = FINISHED.contains(&status.as_str());
        let t = acc.totals();
        let ended_at = if finished || status == "background" {
            match (&acc.stopped_at, &acc.last_activity_at) {
                (Some(s), Some(a)) => Some(if s > a { s.clone() } else { a.clone() }),
                (Some(s), None) => Some(s.clone()),
                (None, Some(a)) => Some(a.clone()),
                (None, None) => Some(acc.prompt_at.clone()),
            }
        } else {
            None
        };
        let span_end = ended_at.clone().or_else(|| acc.last_activity_at.clone());
        let duration_ms = span_end.as_deref().and_then(|e| time::diff_ms(&acc.prompt_at, e));
        let ttfr = acc.first_reply_at.as_deref().and_then(|f| time::diff_ms(&acc.prompt_at, f));
        // 앱(입력창·예약·폰 답)에서 보낸 말은 답 없이 끝나도 숨기지 않는다 — 숨기면 보낸 말이 화면에서 사라진다(사용자가 "응답 없이 끝남"을 봐야 한다)
        let from_app = acc.from_app();
        let hidden = !acc.side
            && !from_app
            && acc.api_calls() == 0
            && acc.response.is_none()
            && (finished || (acc.slash.is_some() && acc.prompt_text.is_empty()));
        let plan_json = if acc.plan.is_empty() {
            None
        } else {
            Some(
                Value::Array(acc.plan.iter().map(|p| json!({"text": p.text, "status": p.status})).collect())
                    .to_string(),
            )
        };

        // (id, 상태, 알렸나, 답이 없었나)
        let old: Option<(i64, String, i64, bool)> = self
            .conn
            .query_row(
                "SELECT id, status, notified, api_calls = 0 AND TRIM(COALESCE(response_text, '')) = '' FROM turn WHERE session_id = ?1 AND prompt_uuid = ?2",
                params![sid, acc.uuid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
            .ok()
            .flatten();

        // 설치 전에 끝난 요청은 이미 본 것으로 친다 (백필이 안 읽음 수백 개를 만들지 않게)
        let cutoff = if acc.codex { self.codex_since.as_str() } else { self.installed_at.as_str() };
        let backfilled = finished && ended_at.as_deref().map(|e| e < cutoff).unwrap_or(false);
        let read_at: Option<String> = if backfilled { ended_at.clone() } else { None };

        let res = self.conn.execute(
            "INSERT INTO turn (session_id, prompt_uuid, seq, origin, prompt_source, peer_name, prompt_at, prompt_text,
                slash_command, understanding, plan_json, summary, response_text, first_reply_at, last_activity_at,
                stopped_at, ended_at, duration_ms, active_ms, ttfr_ms, status, needs_input, hidden, error_count,
                pending_bg, model, effort, api_calls, input_tokens, output_tokens, thinking_tokens, cache_create_5m,
                cache_create_1h, cache_read, web_search, web_fetch, context_tokens, tool_calls, files_changed,
                subagent_count, task_notifications, cwd, git_branch, read_at, notified, updated_at)
             VALUES (?1, ?2, (SELECT COALESCE(MAX(seq), 0) + 1 FROM turn WHERE session_id = ?1), ?3, ?4, ?5, ?6, ?7,
                ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
                ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42, ?43, ?44, ?45)
             ON CONFLICT(session_id, prompt_uuid) DO UPDATE SET
                origin = excluded.origin, prompt_source = excluded.prompt_source, peer_name = excluded.peer_name,
                prompt_at = excluded.prompt_at, prompt_text = excluded.prompt_text, slash_command = excluded.slash_command,
                understanding = excluded.understanding, plan_json = excluded.plan_json, summary = excluded.summary,
                response_text = excluded.response_text, first_reply_at = excluded.first_reply_at,
                last_activity_at = excluded.last_activity_at, stopped_at = excluded.stopped_at,
                ended_at = excluded.ended_at, duration_ms = excluded.duration_ms, active_ms = excluded.active_ms,
                ttfr_ms = excluded.ttfr_ms, status = excluded.status, needs_input = excluded.needs_input,
                hidden = excluded.hidden, error_count = excluded.error_count, pending_bg = excluded.pending_bg,
                model = excluded.model, effort = excluded.effort, api_calls = excluded.api_calls,
                input_tokens = excluded.input_tokens, output_tokens = excluded.output_tokens,
                thinking_tokens = excluded.thinking_tokens, cache_create_5m = excluded.cache_create_5m,
                cache_create_1h = excluded.cache_create_1h, cache_read = excluded.cache_read,
                web_search = excluded.web_search, web_fetch = excluded.web_fetch,
                context_tokens = excluded.context_tokens, tool_calls = excluded.tool_calls,
                files_changed = excluded.files_changed, subagent_count = excluded.subagent_count,
                task_notifications = excluded.task_notifications, cwd = excluded.cwd, git_branch = excluded.git_branch,
                updated_at = excluded.updated_at",
            params![
                sid,
                acc.uuid,
                acc.origin,
                acc.source,
                acc.peer_name,
                acc.prompt_at,
                acc.prompt_text,
                acc.slash,
                acc.understanding,
                plan_json,
                acc.summary,
                acc.response,
                acc.first_reply_at,
                acc.last_activity_at,
                acc.stopped_at,
                ended_at,
                duration_ms,
                acc.active_ms,
                ttfr,
                status,
                needs_input as i64,
                hidden as i64,
                acc.errors,
                acc.pending_bg,
                LocalTop(&acc.models).get(),
                LocalTop(&acc.efforts).get(),
                acc.api_calls(),
                t.input,
                t.output,
                t.thinking,
                t.cc5m,
                t.cc1h,
                t.cache_read,
                t.web_search,
                t.web_fetch,
                acc.context_tokens(),
                acc.tools.values().sum::<i64>(),
                acc.files.len() as i64,
                acc.subs.len() as i64,
                acc.task_notes,
                acc.cwd,
                acc.branch,
                read_at,
                backfilled as i64,
                time::now_iso(),
            ],
        );
        if let Err(err) = res {
            eprintln!("[ai-inbox] 요청 저장 실패 {}: {err}", acc.uuid);
            return None;
        }
        let turn_id: i64 = self
            .conn
            .query_row(
                "SELECT id FROM turn WHERE session_id = ?1 AND prompt_uuid = ?2",
                params![sid, acc.uuid],
                |r| r.get(0),
            )
            .ok()?;

        // 자식 표 — 도구·파일·서브에이전트는 작아서 통째로 바꾼다
        let _ = self.conn.execute("DELETE FROM turn_tool WHERE turn_id = ?1", params![turn_id]);
        for (name, n) in &acc.tools {
            let _ = self.conn.execute(
                "INSERT INTO turn_tool (turn_id, tool_name, calls) VALUES (?1, ?2, ?3)",
                params![turn_id, name, n],
            );
        }
        let _ = self.conn.execute("DELETE FROM turn_file WHERE turn_id = ?1", params![turn_id]);
        for (path, n) in &acc.files {
            let _ = self.conn.execute(
                "INSERT INTO turn_file (turn_id, path, edits) VALUES (?1, ?2, ?3)",
                params![turn_id, path, n],
            );
        }
        crate::tags::save_touched(&self.conn, turn_id, &acc.touched);
        // 요청 시점 훅이 정해 둔 태그가 있으면 잇는다(Codex 는 훅이 없다)
        if !acc.codex && crate::tags::attach_for_turn(&self.conn, sid, turn_id, &acc.prompt_text, &acc.prompt_at) {
            rep.tags_changed = true;
        }
        // 폴더 규칙(0.10.1): 이 요청이 실제로 다룬 경로(위에서 저장한 turn_touch)가 경로 규칙에 들 때만 — 다룬 경로는 작업 중에 늘어나므로 쓸 때마다 본다
        if !acc.codex && !acc.touched.is_empty() && crate::tags::attach_folder(&self.conn, turn_id) {
            rep.tags_changed = true;
        }
        let _ = self.conn.execute("DELETE FROM turn_subagent WHERE turn_id = ?1", params![turn_id]);
        for (i, s) in acc.subs.iter().enumerate() {
            let dur = match (&s.started_at, &s.ended_at) {
                (Some(a), Some(b)) => time::diff_ms(a, b),
                _ => None,
            };
            let _ = self.conn.execute(
                "INSERT INTO turn_subagent (turn_id, seq, agent_type, description, background, started_at, ended_at, duration_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![turn_id, i as i64, s.agent_type, s.description, s.background as i64, s.started_at, s.ended_at, dur],
            );
        }
        // 작업 과정은 append-only — 이번 실행에서 처음 쓰는 요청이면 비우고 전부, 아니면 새 것만
        if acc.steps_saved == 0 {
            let _ = self.conn.execute("DELETE FROM turn_step WHERE turn_id = ?1", params![turn_id]);
        }
        for (i, s) in acc.steps.iter().enumerate().skip(acc.steps_saved) {
            let _ = self.conn.execute(
                "INSERT OR REPLACE INTO turn_step (turn_id, seq, at, kind, name, text) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![turn_id, i as i64, s.at, s.kind, s.name, s.text],
            );
        }
        acc.steps_saved = acc.steps.len();
        acc.dirty = false;
        // 작업 중에 보낸 말은 원래 요청보다 늦게 줄에 오른다 — 이미 뒤 요청이 있으면(다시 읽기 등)
        // 순서 번호를 보낸 시각 순으로 다시 매겨 대화에서 제자리에 오게 한다
        if acc.side && old.is_none() {
            let later: bool = self
                .conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM turn WHERE session_id = ?1 AND prompt_at > ?2)",
                    params![sid, acc.prompt_at],
                    |r| r.get(0),
                )
                .unwrap_or(false);
            if later {
                let _ = self.conn.execute(
                    "UPDATE turn SET seq = (SELECT COUNT(*) FROM turn t2 WHERE t2.session_id = turn.session_id
                         AND (t2.prompt_at < turn.prompt_at OR (t2.prompt_at = turn.prompt_at AND t2.id <= turn.id)))
                      WHERE session_id = ?1",
                    params![sid],
                );
            }
        }

        // 알림 후보: 방금 끝났거나(✅) 사용자를 기다리게 됐다(❓·권한)
        let was = old.as_ref().map(|(_, s, _, _)| s.as_str()).unwrap_or("");
        let already = old.as_ref().map(|(_, _, n, _)| *n != 0).unwrap_or(false) || backfilled;
        let attention = finished || status == "waiting";
        // 알리지 않고 둔 "답 없는 끝남"에 이제 답이 붙었으면 새 끝남이다(그 답으로 알린다)
        let answered_now = old.as_ref().is_some_and(|(_, _, n, empty)| *n == 0 && *empty) && !acc.no_answer();
        let was_attention = (FINISHED.contains(&was) || was == "waiting") && !answered_now;
        // 앱에서 보낸 말이 답 없이 끝남 — 이어서 띄운 직후의 오판일 수 있어 알림은 첫 답 전 유예 뒤로(`notify_deferred`).
        // 숨기지는 않는다(리뷰 필수 2: 느린 기동에서 보낸 말 본문이 "끝남" 알림으로 뜨고 답이 오면 또 왔다)
        let defer = from_app && acc.no_answer() && time::age_ms(&acc.prompt_at).is_some_and(|a| a < self.empty_grace_ms);
        // 사용자가 직접 중단한 앱 발송 말(답 없음)은 알릴 결과가 없다 — 터미널에서 친 말이 답 없이 중단되면 숨겨져 알리지 않는 것과 같게
        let stopped_by_user = from_app && acc.no_answer() && status == "interrupted";
        let recent = ended_at
            .as_deref()
            .or(acc.last_activity_at.as_deref())
            .and_then(time::age_ms)
            .map(|a| a < NOTIFY_WINDOW_MS)
            .unwrap_or(false);
        // 따로 답한 글이 없는 "작업 중에 보낸 말"은 결과가 아니다 — 안 읽음·알림을 만들지 않는다
        let no_result_side = acc.side && acc.closed && acc.response.is_none();
        if no_result_side {
            let _ = self.conn.execute(
                "UPDATE turn SET read_at = COALESCE(read_at, ended_at, prompt_at), notified = 1 WHERE id = ?1",
                params![turn_id],
            );
        }
        if attention && !was_attention && !already && !hidden && recent && !no_result_side && !defer && !stopped_by_user {
            let body = acc
                .summary
                .clone()
                .or_else(|| acc.response.clone())
                .unwrap_or_else(|| acc.prompt_text.clone());
            let text = if from_app && finished && acc.no_answer() { no_answer_text(&body) } else { text::clip(text::first_line(&body), 140) };
            rep.finished.push(Finished { turn_id, session_id: sid.to_string(), status: status.clone(), needs_input, text });
            let _ = self.conn.execute("UPDATE turn SET notified = 1 WHERE id = ?1", params![turn_id]);
        }
        if status == "waiting" || !finished {
            // 다시 돌기 시작하면 다음 완료 때 또 알릴 수 있게
            if was_attention && !attention {
                let _ = self.conn.execute("UPDATE turn SET notified = 0 WHERE id = ?1", params![turn_id]);
            }
        }
        // 보관한 뒤에 새 요청이 오거나 하던 일이 끝나면 목록으로 되돌린다 — 보관 때문에 새 결과를 놓치지 않게.
        // 시각으로 가른다(앱이 꺼져 있던 동안 온 것도 되돌린다). 다시 읽기로 옛 요청을 다시 쓰는 것은 되돌리지 않는다.
        if !backfilled && !hidden {
            let since: Option<Option<String>> = self
                .conn
                .query_row("SELECT archived_at FROM session WHERE id = ?1 AND hidden = 1", params![sid], |r| r.get(0))
                .optional()
                .ok()
                .flatten();
            if let Some(since) = since {
                let since = since.unwrap_or_default();
                let fresh_prompt = old.is_none() && acc.prompt_at.as_str() > since.as_str();
                let done_at = ended_at.as_deref().or(acc.last_activity_at.as_deref()).unwrap_or("");
                let fresh_result = attention && !was_attention && done_at > since.as_str();
                if fresh_prompt || fresh_result {
                    let _ = self
                        .conn
                        .execute("UPDATE session SET hidden = 0, archived_at = NULL WHERE id = ?1", params![sid]);
                }
            }
        }
        rep.changed.insert(sid.to_string());
        Some((status, needs_input))
    }
}

struct LocalTop<'a>(&'a BTreeMap<String, i64>);
impl LocalTop<'_> {
    fn get(&self) -> Option<String> {
        TurnAcc::top(self.0)
    }
}

// ── 줄 해석 ──────────────────────────────────────────────────────────────────

/// 완결된 줄만 읽는다(쓰는 중인 마지막 줄은 다음 번에). 반환: 다음 읽기 위치
const MAX_LINE: usize = 64 * 1024 * 1024;

fn read_lines(path: &Path, start: u64, mut f: impl FnMut(u64, &[u8])) -> std::io::Result<u64> {
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut rd = BufReader::with_capacity(1 << 20, file);
    let mut pos = start;
    let mut buf: Vec<u8> = Vec::with_capacity(64 * 1024);
    loop {
        buf.clear();
        // 한 줄이 64MB 를 넘으면(비정상) 메모리에 모으지 않고 줄 끝까지 흘려보낸다
        let mut len = 0usize;
        let mut oversized = false;
        let complete = loop {
            let chunk = rd.fill_buf()?;
            if chunk.is_empty() {
                break false;
            }
            let (take, done) = match chunk.iter().position(|&b| b == b'\n') {
                Some(i) => (i + 1, true),
                None => (chunk.len(), false),
            };
            if !oversized {
                if len + take > MAX_LINE {
                    oversized = true;
                    buf.clear();
                } else {
                    buf.extend_from_slice(&chunk[..take]);
                }
            }
            len += take;
            rd.consume(take);
            if done {
                break true;
            }
        };
        if !complete {
            break; // 쓰는 중인 마지막 줄 — 다음 번에
        }
        if !oversized {
            f(pos, &buf);
        }
        pos += len as u64;
    }
    Ok(pos)
}

fn feed(st: &mut FileState, line_start: u64, raw: &[u8], closed: &mut Vec<TurnAcc>, patch: &mut SessionPatch) {
    let Ok(line) = serde_json::from_slice::<Line>(raw) else { return };
    let typ = line.typ.as_deref().unwrap_or("");
    let at = line.timestamp.as_deref().and_then(time::normalize);

    match typ {
        "custom-title" => {
            if let Some(t) = line.custom_title.as_ref().filter(|t| !t.trim().is_empty()) {
                patch.title = Some(t.trim().to_string());
            }
            return;
        }
        "agent-name" => {
            if let Some(t) = line.agent_name.as_ref().filter(|t| !t.trim().is_empty()) {
                patch.agent_name = Some(t.trim().to_string());
            }
            return;
        }
        "cost-state" => {
            patch.cost_usd = line.total_cost.or(patch.cost_usd);
            patch.lines_added = line.lines_added.or(patch.lines_added);
            patch.lines_removed = line.lines_removed.or(patch.lines_removed);
            return;
        }
        "user" | "assistant" | "system" | "attachment" => {}
        _ => return,
    }
    if line.is_sidechain {
        return;
    }
    if let Some(a) = &at {
        if patch.first_at.as_deref().map(|f| a.as_str() < f).unwrap_or(true) {
            patch.first_at = Some(a.clone());
        }
        if patch.last_at.as_deref().map(|l| a.as_str() > l).unwrap_or(true) {
            patch.last_at = Some(a.clone());
        }
    }
    if let Some(c) = &line.cwd {
        if patch.cwd.is_none() {
            patch.cwd = Some(c.clone());
        }
    }
    if line.git_branch.is_some() {
        patch.branch = line.git_branch.clone();
    }
    if line.version.is_some() {
        patch.version = line.version.clone();
    }

    match typ {
        "user" => on_user(st, line_start, &line, at, closed),
        "attachment" => on_attachment(st, line_start, &line, at, closed),
        "assistant" => {
            if let Some(acc) = st.open.as_mut() {
                on_assistant(acc, &line, at.clone(), patch);
            }
            answer_side(st, &line, at, closed);
        }
        "system" => {
            if let Some(acc) = st.open.as_mut() {
                on_system(acc, &line, at);
            }
        }
        _ => {}
    }
}

/// 작업 도중 들어온 입력. Claude Code 는 모델이 일하는 동안 보낸 말을 `user` 줄이 아니라
/// `attachment{type: queued_command}` 로 대화에 끼워 넣는다 — 안 읽으면 그 질문이 통째로 사라진다.
fn on_attachment(st: &mut FileState, line_start: u64, line: &Line, at: Option<String>, closed: &mut Vec<TurnAcc>) {
    let Some(a) = &line.attachment else { return };
    if a.get("type").and_then(Value::as_str) != Some("queued_command") {
        return;
    }
    let prompt = a.get("prompt").map(text::text_of).unwrap_or_default();
    if prompt.trim().is_empty() {
        return;
    }
    let mode = a.get("commandMode").and_then(Value::as_str).unwrap_or("prompt");
    let kind = a.get("origin").and_then(|o| o.get("kind")).and_then(Value::as_str).unwrap_or("");
    let when = a
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(time::normalize)
        .or(at.clone())
        .unwrap_or_else(time::now_iso);
    let uuid = a
        .get("source_uuid")
        .and_then(Value::as_str)
        .or(line.uuid.as_deref())
        .filter(|u| !u.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("pos-{line_start}"));
    let rewake = if mode == "task-notification" { text::rewake_message(&prompt) } else { None };
    let echo = rewake.is_some();
    let (mode, kind, prompt) = match rewake {
        Some(msg) => ("prompt", "human", msg),
        None => (mode, kind, prompt),
    };
    match (mode, kind) {
        ("prompt", "human") | ("prompt", "") | ("prompt", "channel") => {
            let c = if kind == "channel" {
                text::clean_prompt(&text::strip_channel_tag(&prompt))
            } else {
                text::clean_prompt(&prompt)
            };
            if c.text.is_empty() && c.slash.is_none() {
                return;
            }
            let (body, origin) = inbox_or(&c.text, if kind == "channel" { "channel" } else { "human" });
            let body = text::redact(&body);
            let Some(main) = st.open.as_mut() else {
                // 돌던 요청이 없으면 평범한 새 요청이다
                let mut acc = TurnAcc::new(uuid, when, line, body, c.slash, origin, None);
                acc.echo = echo;
                start_turn(st, line_start, acc, closed);
                return;
            };
            main.step(&Some(when.clone()), "ask", None, text::clip(&body, 2000));
            main.touch(&at);
            let mut side = TurnAcc::new(uuid, when, line, body, c.slash, origin, None);
            side.source = Some("mid-turn".into());
            side.side = true;
            side.echo = echo;
            side.cwd = side.cwd.clone().or(main.cwd.clone());
            side.branch = side.branch.clone().or(main.branch.clone());
            st.side.push(side);
        }
        ("prompt", "peer") => {
            if let Some(main) = st.open.as_mut() {
                let (peer, body) = text::parse_peer(&prompt);
                main.step(&Some(when), "ask", Some(peer.unwrap_or_else(|| "다른 세션".into())), text::safe(&body, 2000));
                main.touch(&at);
            }
        }
        ("task-notification", _) => {
            if let Some(main) = st.open.as_mut() {
                let (tool_id, status, summary) = text::parse_task_notification(&prompt);
                if let Some(i) = tool_id.as_deref().and_then(|t| main.sub_by_tool.get(t)).copied() {
                    main.subs[i].ended_at = Some(when.clone());
                }
                main.task_notes += 1;
                main.step(&Some(when), "task", status, text::safe(&summary, 1000));
                main.touch(&at);
            }
        }
        _ => {}
    }
}

/// 작업 도중 보낸 말의 답: **그 말을 받은 직후 모델 응답 하나(같은 requestId)** 안의 글만 답으로 붙인다.
/// 그 응답에 글이 없으면(바로 도구만 부름) 억지로 붙이지 않는다 — 뒤의 진행 안내가 답처럼 보이는 것을 막는다.
/// (Claude Code 는 도구 호출 앞의 짧은 글을 대화 기록에 남기지 않을 때가 있다 — 실측 2026-09-24)
fn answer_side(st: &mut FileState, line: &Line, at: Option<String>, _closed: &mut Vec<TurnAcc>) {
    if line.is_api_error {
        return;
    }
    let rid = line.request_id.clone().unwrap_or_default();
    for side in st.side.iter_mut() {
        if side.answered {
            continue;
        }
        match &side.reply_req {
            None => side.reply_req = Some(rid.clone()),
            Some(r) if *r == rid => {}
            Some(_) => {
                // 다음 응답이 시작됐다 — 이 말에 바로 단 글 모으기는 끝. 요청은 닫지 않는다: 앞 요청이 끝날 때까지
                // "작업 중"이고, 끝나면 그 최종 답을 받는다(09-28 사용자: "앞 요청에 이어졌다가 아니라 각 요청에 맞는 응답을")
                side.answered = true;
                side.dirty = true;
                continue;
            }
        }
        if side.first_reply_at.is_none() {
            side.first_reply_at = at.clone();
        }
        side.touch(&at);
        if let Some(Value::Array(blocks)) = line.message.as_ref().and_then(|m| m.content.as_ref()) {
            for t in blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .map(str::trim)
                .filter(|t| !t.is_empty())
            {
                let t = if std::mem::take(&mut side.echo) { text::strip_echo(t) } else { t };
                if t.is_empty() {
                    continue;
                }
                let t = text::redact(t);
                // 바로 단 글은 "지금 하는 일"로 보인다 — 최종 답은 앞 요청이 끝날 때(`settle_side`)
                side.understanding = Some(match side.understanding.take() {
                    Some(prev) => text::clip(&format!("{prev}\n\n{t}"), 4000),
                    None => text::clip(&t, 4000),
                });
                side.step(&at, "text", None, text::clip(&t, 12_000));
            }
        }
        return;
    }
}

/// 작업 중에 보낸 말의 답 = 그 말을 받은 뒤 앞 요청이 낸 최종 글(Claude 는 도구 사이의 짧은 글을 기록에 남기지 않을 때가 있어
/// 바로 단 글만으로는 답이 비기 쉽다 — 09-28 실측). 그 뒤에 쓴 글이 없으면 바로 단 글.
fn settle_side(s: &mut TurnAcc, main: &TurnAcc) {
    let wrote_after = main.steps.iter().any(|x| x.kind == "text" && x.at.as_deref().is_some_and(|a| a > s.prompt_at.as_str()));
    let fin = if wrote_after { main.response.clone() } else { None };
    s.response = fin.or_else(|| s.understanding.clone());
    s.stopped_at = main.stopped_at.clone().or_else(|| main.last_activity_at.clone());
}

fn close_sides(st: &mut FileState, closed: &mut Vec<TurnAcc>) {
    for mut s in st.side.drain(..) {
        s.closed = true;
        s.dirty = true;
        closed.push(s);
    }
}

fn start_turn(st: &mut FileState, line_start: u64, acc: TurnAcc, closed: &mut Vec<TurnAcc>) {
    if let Some(prev) = st.open.as_mut() {
        // 모델이 한 마디도 하기 전에 이어서 친 말(대기열 입력 등) → 같은 요청으로 합친다
        let untouched = prev.api_calls() == 0 && prev.response.is_none() && prev.steps.is_empty();
        if untouched && prev.slash.is_none() && acc.slash.is_none() && prev.origin == "human" && acc.origin == "human" && !acc.prompt_text.is_empty() {
            let add = acc.prompt_text.trim();
            if !add.is_empty() && !prev.prompt_text.contains(add) {
                prev.prompt_text = format!("{}\n\n{}", prev.prompt_text.trim_end(), add);
            }
            prev.dirty = true;
            return;
        }
    }
    if let Some(prev) = st.open.as_ref() {
        for side in st.side.iter_mut() {
            settle_side(side, prev);
        }
    }
    if let Some(mut prev) = st.open.take() {
        prev.closed = true;
        prev.dirty = true;
        closed.push(prev);
    }
    close_sides(st, closed);
    st.open = Some(acc);
    st.offset = line_start;
}

/// AI Inbox 입력창에서 보낸 말이면 머리말을 떼고 출처를 `inbox` 로 남긴다
fn inbox_or<'a>(body: &str, origin: &'a str) -> (String, &'a str) {
    match crate::conoti::strip_header(body) {
        Some((rest, o)) => (rest.to_string(), o),
        None => (body.to_string(), origin),
    }
}

fn on_user(st: &mut FileState, line_start: u64, line: &Line, at: Option<String>, closed: &mut Vec<TurnAcc>) {
    if line.is_compact_summary {
        return;
    }
    let Some(msg) = &line.message else { return };
    let content = msg.content.clone().unwrap_or(Value::Null);
    let tool_results: Vec<&Value> = content
        .as_array()
        .map(|a| a.iter().filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result")).collect())
        .unwrap_or_default();

    if !tool_results.is_empty() {
        let Some(acc) = st.open.as_mut() else { return };
        for tr in tool_results {
            let id = tr.get("tool_use_id").and_then(Value::as_str).unwrap_or("");
            if acc.pending_ask.as_deref() == Some(id) {
                acc.pending_ask = None;
            }
            if let Some(&i) = acc.sub_by_tool.get(id) {
                let body = text::text_of(tr.get("content").unwrap_or(&Value::Null));
                if body.starts_with("Async agent launched") || body.contains("running in the background") {
                    acc.subs[i].background = true;
                } else {
                    acc.subs[i].ended_at = at.clone();
                }
            }
        }
        acc.touch(&at);
        return;
    }

    let raw = text::text_of(&content);
    let kind = line.origin.as_ref().and_then(|o| o.kind.clone());
    let uuid = line
        .uuid
        .clone()
        .filter(|u| !u.is_empty())
        .unwrap_or_else(|| format!("pos-{line_start}"));
    let when = at.clone().unwrap_or_else(time::now_iso);

    match kind.as_deref() {
        Some("human") => {
            let c = text::clean_prompt(&raw);
            let (body, origin) = inbox_or(&c.text, "human");
            let acc = TurnAcc::new(uuid, when, line, text::redact(&body), c.slash, origin, None);
            start_turn(st, line_start, acc, closed);
        }
        Some("channel") => {
            let (body, origin) = inbox_or(&text::strip_channel_tag(&raw), "channel");
            let acc = TurnAcc::new(uuid, when, line, text::redact(&body), None, origin, None);
            start_turn(st, line_start, acc, closed);
        }
        Some("peer") => {
            let (peer, body) = text::parse_peer(&raw);
            let acc = TurnAcc::new(uuid, when, line, text::redact(&body), None, "peer", peer);
            start_turn(st, line_start, acc, closed);
        }
        Some("task-notification") if text::rewake_message(&raw).is_some() => {
            // AI Inbox 대기 훅이 쉬던 세션을 깨워 넣은 말 → 새 요청
            let msg = text::rewake_message(&raw).unwrap_or_default();
            let (body, origin) = inbox_or(&msg, "human");
            let mut acc = TurnAcc::new(uuid, when, line, text::redact(&body), None, origin, None);
            acc.echo = true;
            start_turn(st, line_start, acc, closed);
        }
        Some("task-notification") => {
            if let Some(acc) = st.open.as_mut() {
                let (tool_id, status, summary) = text::parse_task_notification(&raw);
                if let Some(i) = tool_id.as_deref().and_then(|t| acc.sub_by_tool.get(t)).copied() {
                    acc.subs[i].ended_at = at.clone();
                }
                acc.task_notes += 1;
                acc.step(&at, "task", status, text::safe(&summary, 1000));
                acc.touch(&at);
            }
        }
        Some(other) => {
            // auto-continuation 등: 같은 요청의 연장
            if let Some(acc) = st.open.as_mut() {
                let label = if other == "auto-continuation" { "사용량 한도가 풀려 이어서 진행".to_string() } else { text::safe(text::first_line(&raw), 200) };
                acc.step(&at, "continue", Some(other.to_string()), label);
                acc.touch(&at);
            }
        }
        None => {
            // 옛 형식(origin 없음): 사람이 친 줄 vs 중단 표시 vs 메타
            if raw.starts_with("[Request interrupted") {
                if let Some(acc) = st.open.as_mut() {
                    acc.interrupted_at = at.clone().or_else(|| Some(time::now_iso()));
                    acc.step(&at, "interrupt", None, "사용자가 중단함".into());
                    acc.dirty = true;
                }
                return;
            }
            if line.is_meta || raw.trim_start().starts_with("<local-command-stdout>") {
                return;
            }
            if raw.trim_start().starts_with("<channel ") {
                let (body, origin) = inbox_or(&text::strip_channel_tag(&raw), "channel");
                let acc = TurnAcc::new(uuid, when, line, text::redact(&body), None, origin, None);
                start_turn(st, line_start, acc, closed);
                return;
            }
            let c = text::clean_prompt(&raw);
            if c.text.is_empty() && c.slash.is_none() {
                return;
            }
            let (body, origin) = inbox_or(&c.text, "human");
            let acc = TurnAcc::new(uuid, when, line, text::redact(&body), c.slash, origin, None);
            start_turn(st, line_start, acc, closed);
        }
    }
}

fn on_assistant(acc: &mut TurnAcc, line: &Line, at: Option<String>, patch: &mut SessionPatch) {
    let Some(msg) = &line.message else { return };
    if acc.first_reply_at.is_none() {
        acc.first_reply_at = at.clone();
    }
    acc.touch(&at);

    if line.is_api_error {
        acc.errors += 1;
        let body = text::text_of(msg.content.as_ref().unwrap_or(&Value::Null));
        acc.step(&at, "error", None, text::safe(&body, 400));
        return;
    }
    if let Some(m) = msg.model.as_ref().filter(|m| !m.starts_with('<')) {
        *acc.models.entry(m.clone()).or_default() += 1;
        patch.model = Some(m.clone());
    }
    if let Some(e) = &line.effort {
        *acc.efforts.entry(e.clone()).or_default() += 1;
    }
    if let (Some(rid), Some(u)) = (&line.request_id, &msg.usage) {
        let usage = Usage::from(u);
        match acc.calls.get_mut(rid) {
            Some(old) => old.merge(&usage),
            None => {
                acc.calls.insert(rid.clone(), usage);
                acc.call_order.push(rid.clone());
            }
        }
    }

    let Some(Value::Array(blocks)) = &msg.content else { return };
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                let t = b.get("text").and_then(Value::as_str).unwrap_or("").trim();
                if t.is_empty() {
                    continue;
                }
                let t = if std::mem::take(&mut acc.echo) { text::strip_echo(t) } else { t };
                if t.is_empty() {
                    continue;
                }
                let t = text::redact(t);
                if !acc.saw_tool && acc.understanding.is_none() {
                    acc.understanding = Some(text::clip(&t, 4000));
                }
                acc.response = Some(text::clip(&t, 120_000));
                acc.step(&at, "text", None, text::clip(&t, 12_000));
            }
            Some("tool_use") => {
                acc.saw_tool = true;
                let name = b.get("name").and_then(Value::as_str).unwrap_or("?").to_string();
                let input = b.get("input").cloned().unwrap_or(Value::Null);
                let id = b.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                *acc.tools.entry(name.clone()).or_default() += 1;
                crate::tags::collect_touched(&mut acc.touched, &name, &input);
                if matches!(name.as_str(), "Edit" | "Write" | "NotebookEdit" | "MultiEdit") {
                    if let Some(p) = input.get("file_path").or_else(|| input.get("notebook_path")).and_then(Value::as_str) {
                        *acc.files.entry(p.to_string()).or_default() += 1;
                    }
                }
                match name.as_str() {
                    "TodoWrite" => {
                        if let Some(todos) = input.get("todos").and_then(Value::as_array) {
                            acc.plan = todos
                                .iter()
                                .filter_map(|t| {
                                    let text = t.get("content").and_then(Value::as_str)?;
                                    Some(PlanItem {
                                        text: text::safe(text, 500),
                                        status: t.get("status").and_then(Value::as_str).unwrap_or("pending").to_string(),
                                    })
                                })
                                .collect();
                        }
                    }
                    "TaskCreate" => {
                        if let Some(s) = input.get("subject").and_then(Value::as_str) {
                            acc.plan.push(PlanItem { text: text::safe(s, 500), status: "pending".into() });
                        }
                    }
                    "Agent" | "Task" => {
                        acc.sub_by_tool.insert(id.clone(), acc.subs.len());
                        acc.subs.push(Sub {
                            agent_type: input
                                .get("subagent_type")
                                .and_then(Value::as_str)
                                .unwrap_or("general-purpose")
                                .to_string(),
                            description: text::safe(input.get("description").and_then(Value::as_str).unwrap_or(""), 300),
                            background: input.get("run_in_background").and_then(Value::as_bool).unwrap_or(false),
                            started_at: at.clone(),
                            ended_at: None,
                        });
                    }
                    "AskUserQuestion" => acc.pending_ask = Some(id.clone()),
                    _ => {}
                }
                let summary = text::tool_summary(&name, &input, acc.cwd.as_deref());
                acc.step(&at, "tool", Some(name), summary);
            }
            _ => {}
        }
    }
}

fn on_system(acc: &mut TurnAcc, line: &Line, at: Option<String>) {
    match line.subtype.as_deref() {
        Some("turn_duration") => {
            acc.active_ms = acc.active_ms.saturating_add(line.duration_ms.unwrap_or(0).clamp(0, 7 * 86_400_000));
            acc.pending_bg = line.pending_bg.unwrap_or(0).max(0);
            acc.stopped_at = at;
            acc.dirty = true;
        }
        Some("away_summary") => {
            if let Some(Value::String(s)) = &line.content {
                let s = text::redact(s.trim());
                acc.summary = Some(s.clone());
                acc.step(&at, "summary", None, s);
            }
        }
        Some("compact_boundary") => acc.step(&at, "compact", None, "대화가 길어 앞부분을 압축함".into()),
        Some("api_error") => {
            acc.errors += 1;
            acc.step(&at, "error", None, "API 오류".into());
        }
        _ => {}
    }
}

// ── 잡동사니 ─────────────────────────────────────────────────────────────────

/// 앱에서 보낸 말이 답 없이 끝났을 때의 알림 글 — "답을 받지 못했습니다 — <보낸 말 첫 줄>"(폰 답 머리말·첨부 목록·답장 줄은 뗀다)
fn no_answer_text(prompt: &str) -> String {
    let p = crate::attach::split_block(prompt).0;
    let p = crate::doc::phone_reply(Some(&p)).map(|x| x.1).unwrap_or(p);
    let line = text::first_line(text::split_quote(&p).1.trim()).to_string();
    text::clip(&format!("답을 받지 못했습니다 — {line}"), 140)
}

/// 훅이 쓰다 만 임시 파일(.tmp)이 한 시간 넘게, 앱이 안 읽어 간 훅 파일(.json — 요청 글 앞부분이 들어 있다)이 2주 넘게 남아 있으면 지운다.
fn clean_stale_spool() {
    let Ok(rd) = std::fs::read_dir(paths::spool_dir()) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let limit = match p.extension().and_then(|x| x.to_str()) {
            Some("tmp") => 3600,
            Some("json") => 14 * 24 * 3600,
            _ => continue,
        };
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| m.elapsed().ok())
            .map(|d| d.as_secs() > limit)
            .unwrap_or(false);
        if old {
            let _ = std::fs::remove_file(&p);
        }
    }
}

/// sessions/ 아래 rollout-*.jsonl (연/월/일 폴더 — 깊이 4까지)
fn walk_rollouts(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, u64, i64)>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth < 4 {
                walk_rollouts(&p, depth + 1, out);
            }
            continue;
        }
        let name = e.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        let mtime = md.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
        out.push((p, md.len(), mtime));
    }
}

/// Codex 기록 첫 줄(session_meta)의 payload — 첫 줄은 지시문이 들어 있어 크다(8MB 까지만 본다)
fn codex_meta(path: &Path) -> Option<Value> {
    use std::io::Read;
    let f = File::open(path).ok()?;
    let mut rd = BufReader::new(f).take(8 * 1024 * 1024);
    let mut line = Vec::new();
    rd.read_until(b'\n', &mut line).ok()?;
    let v = serde_json::from_slice::<Value>(&line).ok()?;
    (v.get("type").and_then(Value::as_str) == Some("session_meta")).then(|| v.get("payload").cloned()).flatten()
}

/// 하위 에이전트 스레드인가
fn codex_subagent_file(path: &Path) -> bool {
    let Some(p) = codex_meta(path) else { return false };
    p.get("source").and_then(|s| s.get("subagent")).is_some() || p.get("thread_source").and_then(Value::as_str) == Some("subagent")
}

fn stem(p: &Path) -> String {
    p.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(unix)]
pub fn pid_alive(pid: i64) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: 시그널 0 은 보내지 않고 존재만 확인한다.
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
pub fn pid_alive(pid: i64) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    if pid <= 0 {
        return false;
    }
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid as u32);
        if h.is_null() {
            return false;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(h, &mut code) != 0;
        CloseHandle(h);
        ok && code == STILL_ACTIVE as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[Value]) -> (Vec<TurnAcc>, Option<TurnAcc>) {
        let (closed, st) = run_state(lines);
        (closed, st.open)
    }

    /// 닫힌 요청 + 읽은 뒤의 상태(열린 요청·아직 앞 요청을 따라가는 "작업 중에 보낸 말")
    fn run_state(lines: &[Value]) -> (Vec<TurnAcc>, FileState) {
        let mut st = FileState::fresh("s".into(), 0, 0, false, false);
        st.primed = true;
        let mut closed = Vec::new();
        let mut patch = SessionPatch::default();
        let mut pos = 0u64;
        for l in lines {
            let raw = serde_json::to_vec(l).unwrap();
            feed(&mut st, pos, &raw, &mut closed, &mut patch);
            pos += raw.len() as u64 + 1;
        }
        (closed, st)
    }

    fn human(uuid: &str, at: &str, text: &str) -> Value {
        json!({"type":"user","uuid":uuid,"timestamp":at,"origin":{"kind":"human"},"promptSource":"typed",
               "message":{"role":"user","content":text}})
    }

    fn said(at: &str, text: &str) -> Value {
        json!({"type":"assistant","timestamp":at,"requestId":format!("r-{at}"),
               "message":{"content":[{"type":"text","text":text}],"usage":{"output_tokens":1}}})
    }

    /// 실제 데이터 폴더를 건드리지 않는 수집기(메모리 DB)
    fn ingestor() -> Ingestor {
        let conn = Connection::open_in_memory().unwrap();
        db::migrate(&conn).unwrap();
        Ingestor {
            conn,
            installed_at: "2000-01-01T00:00:00.000Z".into(),
            backfill_days: 7,
            files: HashMap::new(),
            live: HashMap::new(),
            tick_no: 0,
            codex_enabled: true,
            codex_since: "2000-01-01T00:00:00.000Z".into(),
            codex_names: HashMap::new(),
            codex_index_mtime: -1,
            codex_live: Some(HashSet::new()),
            codex_written: HashMap::new(),
            last_clear: None,
            empty_grace_ms: 60_000,
        }
    }

    /// 앱이 모델을 부르려고 띄운 세션(작업 폴더 `llm-scratch`)은 사용자 세션으로 모이지 않는다 — 되먹임(가짜 세션·태그·이력 검색 대상) 방지
    #[test]
    fn internal_model_call_transcripts_are_not_collected() {
        let scratch = paths::llm_scratch_dir().join("x");
        let dir = std::env::temp_dir().join(format!("aiinbox-internal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut lines = Vec::new();
        for (sid, cwd) in [("internal-sess-1", scratch.to_string_lossy().to_string()), ("normal-sess-1", "/w/proj".to_string())] {
            let mut u = human("u1", "2026-09-30T01:00:00.000Z", "발췌 질문");
            u["cwd"] = json!(cwd);
            u["sessionId"] = json!(sid);
            let mut a = said("2026-09-30T01:00:05.000Z", "답");
            a["cwd"] = json!(cwd);
            let f = dir.join(format!("{sid}.jsonl"));
            std::fs::write(&f, format!("{}\n{}\n", u, a)).unwrap();
            lines.push((sid, f));
        }
        let mut ing = ingestor();
        let mut rep = Report::default();
        for (sid, f) in &lines {
            let md = std::fs::metadata(f).unwrap();
            ing.files.insert(f.clone(), FileState::fresh(sid.to_string(), 0, 0, false, false));
            ing.process_file(f, md.len(), 0, &mut rep);
        }
        let n = |sid: &str| ing.conn.query_row("SELECT COUNT(*) FROM turn WHERE session_id = ?1", [sid], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(n("internal-sess-1"), 0, "내부 호출 세션의 요청은 모으지 않는다");
        assert_eq!(ing.conn.query_row("SELECT COUNT(*) FROM session WHERE id = 'internal-sess-1'", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        assert_eq!(n("normal-sess-1"), 1, "보통 세션은 그대로");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// /clear 훅 쌍(SessionEnd reason=clear → SessionStart source=clear)이 옛 세션을 "끝난 대화"로 표시하고 후속 세션을 잇는다
    #[test]
    fn clear_hooks_mark_the_old_session_and_link_the_successor() {
        let mut ing = ingestor();
        ing.conn.execute("INSERT INTO session (id, last_at) VALUES ('old-session-1', '2026-09-01T00:00:00.000Z')", []).unwrap();
        ing.conn
            .execute(
                "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, status) VALUES ('old-session-1', 'u1', 1, '2026-09-01T00:00:00.000Z', 'done')",
                [],
            )
            .unwrap();
        let mut rep = Report::default();
        let ms = 1_790_000_000_000i64;
        ing.apply_hook(&json!({"hook_event_name":"SessionEnd","session_id":"old-session-1","reason":"clear","received_at_ms":ms}), &mut rep);
        ing.apply_hook(&json!({"hook_event_name":"SessionStart","session_id":"new-session-1","source":"clear","received_at_ms":ms + 50}), &mut rep);
        let (cleared, state, to): (Option<String>, Option<String>, Option<String>) = ing
            .conn
            .query_row("SELECT cleared_at, clear_state, cleared_to FROM session WHERE id = 'old-session-1'", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap();
        assert!(cleared.is_some());
        assert_eq!(state.as_deref(), Some("purge"), "기본은 삭제 예약");
        assert_eq!(to.as_deref(), Some("new-session-1"));
        assert_eq!(rep.cleared, vec!["old-session-1".to_string()]);
        // 새 세션은 끝난 대화가 아니다
        let n: i64 = ing.conn.query_row("SELECT COUNT(*) FROM session WHERE id = 'new-session-1' AND cleared_at IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        // clear 가 아닌 종료(prompt_input_exit)는 표시하지 않는다
        ing.conn.execute("INSERT INTO session (id) VALUES ('exit-session-1')", []).unwrap();
        ing.apply_hook(&json!({"hook_event_name":"SessionEnd","session_id":"exit-session-1","reason":"prompt_input_exit","received_at_ms":ms}), &mut rep);
        let n: i64 = ing.conn.query_row("SELECT COUNT(*) FROM session WHERE id = 'exit-session-1' AND cleared_at IS NOT NULL", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0);
        // 화면 목록: 끝난 대화 정보가 실려 나가고, 이력으로 보관하면 전체에서 빠져 이력 탭에만 나온다(폰 목록엔 남는다)
        let all = crate::api::list_sessions_for(&ing.conn, "all", "", false).unwrap();
        let hit = all.iter().find(|s| serde_json::to_value(s).unwrap()["id"] == "old-session-1").unwrap();
        assert_eq!(serde_json::to_value(hit).unwrap()["ended"]["state"], "purge");
        crate::lifecycle::decide(&ing.conn, &["old-session-1".to_string()], "keep").unwrap();
        assert!(crate::api::list_sessions_for(&ing.conn, "all", "", false).unwrap().is_empty());
        assert_eq!(crate::api::list_sessions_for(&ing.conn, "history", "", false).unwrap().len(), 1);
        assert_eq!(crate::api::list_sessions_on(&ing.conn, "all", "").unwrap().len(), 1, "폰 목록엔 이력 보관 세션도 남는다");
        let c = crate::api::counts_of(&ing.conn);
        assert_eq!((c.kept, c.undecided), (1, 0));
    }

    /// 실제 ~/.codex 기록을 메모리 DB 로 읽어 요약만 찍는다(내용은 찍지 않는다).
    /// `cargo test codex_real_ingest -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn codex_real_ingest() {
        let mut ing = ingestor();
        ing.backfill_days = std::env::var("DAYS").ok().and_then(|d| d.parse().ok()).unwrap_or(7);
        for _ in 0..50 {
            let mut rep = Report::default();
            ing.refresh_codex_live();
            ing.scan_codex(&mut rep);
            ing.refresh_codex(&mut rep);
            if !rep.working {
                break;
            }
        }
        let q = |sql: &str| ing.conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap();
        println!(
            "codex sessions={} turns={} visible={} running={} unread={} with_title={} live={:?}",
            q("SELECT COUNT(*) FROM session WHERE agent = 'codex'"),
            q("SELECT COUNT(*) FROM turn"),
            q("SELECT COUNT(*) FROM turn WHERE hidden = 0"),
            q("SELECT COUNT(*) FROM turn WHERE status IN ('running','waiting','background')"),
            q("SELECT COUNT(*) FROM turn WHERE hidden = 0 AND read_at IS NULL AND status IN ('done','interrupted','stopped')"),
            q("SELECT COUNT(*) FROM session WHERE agent = 'codex' AND title IS NOT NULL"),
            ing.codex_live.as_ref().map(|l| l.len()),
        );
        let mut st = ing
            .conn
            .prepare(
                "SELECT s.id, s.cc_version, s.model, s.live_status, COUNT(t.id), SUM(t.tool_calls), SUM(t.api_calls), SUM(t.output_tokens),
                        GROUP_CONCAT(t.status), SUM(LENGTH(t.prompt_text) > 0), SUM(t.response_text IS NOT NULL)
                   FROM session s LEFT JOIN turn t ON t.session_id = s.id WHERE s.agent = 'codex' GROUP BY s.id ORDER BY s.last_at",
            )
            .unwrap();
        let rows = st
            .query_map([], |r| {
                Ok(format!(
                    "{:.8} v{:?} {:?} live={:?} turns={} tools={:?} calls={:?} out={:?} prompts={:?} replies={:?} [{}]",
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, Option<i64>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, Option<i64>>(9)?,
                    r.get::<_, Option<i64>>(10)?,
                    r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                ))
            })
            .unwrap();
        for r in rows.flatten() {
            println!("  {r}");
        }
    }

    fn at(offset_min: i64) -> String {
        (chrono::Utc::now() + chrono::Duration::minutes(offset_min)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    }

    fn turns_of(ing: &Ingestor) -> i64 {
        ing.conn.query_row("SELECT COUNT(*) FROM turn WHERE session_id = 's'", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn deleted_requests_stay_deleted_but_new_activity_comes_in() {
        let ing = ingestor();
        let mut rep = Report::default();
        ing.conn.execute("INSERT INTO session (id) VALUES ('s')", []).unwrap();
        let (mut old, _) = run(&[human("u1", &at(-30), "하나"), said(&at(-29), "끝"), human("u2", &at(-20), "둘")]);
        assert!(ing.flush_turn("s", &mut old[0], &mut rep).is_some());
        assert_eq!(turns_of(&ing), 1);

        let out = crate::archive::delete_sessions(&ing.conn, &["s".into()]).unwrap();
        assert_eq!(out.deleted, vec!["s".to_string()]);
        assert_eq!(turns_of(&ing), 0);
        // 원본을 다시 읽어도 쓰지 않는다 — 상태는 돌려준다(None 이면 틱마다 다시 쓰려 한다) · 바뀜도 만들지 않는다
        ing.conn.execute("INSERT OR IGNORE INTO session (id) VALUES ('s')", []).unwrap();
        let mut rep = Report::default();
        assert!(ing.flush_turn("s", &mut old[0], &mut rep).is_some());
        assert_eq!(turns_of(&ing), 0);
        assert!(rep.changed.is_empty());
        // 이어서 실행해 생긴 복사본 세션에 같은 요청이 있어도 되살리지 않는다
        ing.conn.execute("INSERT INTO session (id) VALUES ('copy')", []).unwrap();
        ing.flush_turn("copy", &mut old[0], &mut rep);
        let copied: i64 = ing.conn.query_row("SELECT COUNT(*) FROM turn WHERE session_id = 'copy'", [], |r| r.get(0)).unwrap();
        assert_eq!(copied, 0);

        // 지운 뒤에 새 요청은 들어온다(수집 전에 친 요청도 — 시각이 아니라 ID 로 가린다)
        let (mut new, _) = run(&[human("u3", &at(-25), "셋"), said(&at(-24), "완료"), human("u4", &at(3), "넷")]);
        assert!(ing.flush_turn("s", &mut new[0], &mut rep).is_some());
        assert_eq!(turns_of(&ing), 1);
        // 지운 요청에 새 활동(백그라운드 작업 완료)이 붙으면 다시 받는다
        let (mut cont, _) = run(&[
            human("u1", &at(-30), "하나"),
            said(&at(-29), "끝"),
            json!({"type":"user","timestamp":at(1),"origin":{"kind":"task-notification"},
                   "message":{"role":"user","content":"<task-notification><status>completed</status><summary>셸 끝</summary></task-notification>"}}),
            said(&at(2), "백그라운드 결과"),
            human("u5", &at(4), "다섯"),
        ]);
        assert!(ing.flush_turn("s", &mut cont[0], &mut rep).is_some());
        assert_eq!(turns_of(&ing), 2);
    }

    #[test]
    fn hook_tags_attach_when_the_request_is_read_and_history_is_never_backfilled() {
        let mut ing = ingestor();
        ing.conn.execute("INSERT INTO session (id) VALUES ('s')", []).unwrap();
        let alpha = crate::tags::create_tag(&ing.conn, "알파", "", false).unwrap();
        crate::tags::add_rule(&ing.conn, alpha, "keyword", "알파", "user").unwrap();
        // 옛 요청(훅 기록 없음)과 지금 요청(훅이 먼저 도착) — 같은 낱말이 있어도 옛 요청은 미분류로 남는다
        let (mut closed, _) = run(&[
            human("u0", &at(-4000), "알파 옛 요청"),
            said(&at(-3999), "끝"),
            human("u1", &at(0), "알파 화면 고쳐줘"),
            said(&at(1), "고쳤습니다"),
            human("u2", &at(5), "다음 요청"),
        ]);
        let mut rep = Report::default();
        let entry = crate::hook::spool_entry(
            &json!({"hook_event_name":"UserPromptSubmit","session_id":"s","cwd":"/nowhere","prompt":"알파 화면 고쳐줘"}),
            chrono::Utc::now().timestamp_millis() as u64,
        )
        .unwrap();
        ing.apply_hook(&entry, &mut rep);
        assert!(rep.tags_changed);
        for acc in closed.iter_mut() {
            ing.flush_turn("s", acc, &mut rep);
        }
        let tagged = |uuid: &str| -> Vec<(i64, String, Option<String>)> {
            ing.conn
                .prepare("SELECT x.tag_id, x.state, x.src FROM turn_tag x JOIN turn t ON t.id = x.turn_id WHERE t.prompt_uuid = ?1")
                .unwrap()
                .query_map(params![uuid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .flatten()
                .collect()
        };
        assert!(tagged("u0").is_empty(), "훅이 없던 옛 요청은 백필로 분류하지 않는다");
        assert_eq!(tagged("u1"), vec![(alpha, "auto".into(), Some("hook".into()))]);
        // 다시 읽기(PARSER_VERSION 재파싱·틱마다의 재기록)에도 중복이 없고 옛 요청은 여전히 미분류
        for acc in closed.iter_mut() {
            ing.flush_turn("s", acc, &mut rep);
        }
        assert_eq!(tagged("u1").len(), 1);
        assert!(tagged("u0").is_empty());
        // Ingestor::new 가 파서 버전이 달라 다시 읽게 해도 태그를 붙이지 않는다
        ing.conn.execute("DELETE FROM source_file", []).unwrap();
        assert_eq!(ing.conn.query_row("SELECT COUNT(*) FROM turn_tag", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }

    #[test]
    fn folder_tags_follow_the_paths_a_request_touched_not_the_session_folder() {
        // 0.10.1 A안: 작업 폴더(git 최상위)의 프로젝트 태그는 그 요청이 실제로 그 폴더 경로를 다뤘을 때만 — 수집기가 다룬 경로를 확정한 뒤 붙인다
        let mut ing = ingestor();
        let base = std::env::temp_dir().join(format!("aiinbox-ingest-folder-{}", std::process::id()));
        let repo = base.join("kappa");
        let other = base.join("lambda");
        for r in [&repo, &other] {
            std::fs::create_dir_all(r.join(".git")).unwrap();
        }
        let cwd = repo.to_string_lossy().into_owned();
        ing.conn.execute("INSERT INTO session (id, project_dir) VALUES ('s', ?1)", params![cwd]).unwrap();
        let tool = |at: &str, id: &str, name: &str, input: Value| {
            json!({"type":"assistant","timestamp":at,"requestId":format!("r-{id}"),
                   "message":{"content":[{"type":"tool_use","id":id,"name":name,"input":input}],"usage":{"output_tokens":1}}})
        };
        let (texts, now) = (
            ["오늘 서울 날씨가 어떤지 우산을 챙겨야 하는지 알려 줘", "옆 저장소의 설정 파일을 열어서 무슨 값이 있는지 읽어 줘", "이 저장소의 로그인 화면이 어색하니 코드를 고쳐서 손봐 줘"],
            chrono::Utc::now().timestamp_millis(),
        );
        let mut rep = Report::default();
        for (i, t) in texts.iter().enumerate() {
            let entry = crate::hook::spool_entry(
                &json!({"hook_event_name":"UserPromptSubmit","session_id":"s","cwd":cwd,"prompt":t}),
                (now + i as i64 * 10 * 60_000) as u64,
            )
            .unwrap();
            ing.apply_hook(&entry, &mut rep);
        }
        let (mut closed, _) = run(&[
            human("u1", &at(0), texts[0]),
            said(&at(1), "맑아요"),
            human("u2", &at(10), texts[1]),
            tool(&at(11), "t1", "Read", json!({ "file_path": other.join("conf.toml").to_string_lossy() })),
            said(&at(12), "읽었습니다"),
            human("u3", &at(20), texts[2]),
            tool(&at(21), "t2", "Edit", json!({ "file_path": repo.join("src/login.rs").to_string_lossy() })),
            said(&at(22), "고쳤습니다"),
            human("u4", &at(30), "다음"),
        ]);
        for acc in closed.iter_mut() {
            ing.flush_turn("s", acc, &mut rep);
        }
        let names = |uuid: &str| -> Vec<String> {
            ing.conn
                .prepare("SELECT g.name FROM turn_tag x JOIN tag g ON g.id = x.tag_id JOIN turn t ON t.id = x.turn_id WHERE t.prompt_uuid = ?1 AND x.state = 'auto'")
                .unwrap()
                .query_map(params![uuid], |r| r.get(0))
                .unwrap()
                .flatten()
                .collect()
        };
        assert!(names("u1").is_empty(), "경로를 안 다룬 요청은 미분류");
        assert!(names("u2").is_empty(), "다른 저장소만 다룬 요청엔 작업 폴더 태그가 안 붙는다");
        assert_eq!(names("u3"), vec!["kappa".to_string()]);
        // 다시 써도(틱마다·재수집) 중복 없음
        for acc in closed.iter_mut() {
            ing.flush_turn("s", acc, &mut rep);
        }
        assert_eq!(ing.conn.query_row("SELECT COUNT(*) FROM turn_tag", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn deleted_open_request_does_not_churn_every_tick() {
        let mut ing = ingestor();
        ing.conn.execute("INSERT INTO session (id) VALUES ('s')", []).unwrap();
        let (_, open) = run(&[
            human("u1", &at(-5), "하나"),
            said(&at(-4), "끝"),
            json!({"type":"system","subtype":"turn_duration","timestamp":at(-3),"durationMs":1000,"pendingBackgroundAgentCount":0}),
        ]);
        let mut acc = open.unwrap();
        ing.flush_turn("s", &mut acc, &mut Report::default());
        assert_eq!(crate::archive::delete_sessions(&ing.conn, &["s".into()]).unwrap().deleted.len(), 1);
        acc.dirty = true;
        let mut st = FileState::fresh("s".into(), 0, 0, false, false);
        st.primed = true;
        st.open = Some(acc);
        ing.files.insert(PathBuf::from("/nowhere/s.jsonl"), st);
        let mut first = Report::default();
        ing.recheck_open(&mut first);
        let mut second = Report::default();
        ing.recheck_open(&mut second);
        assert!(second.changed.is_empty(), "지운 요청이 틱마다 '바뀜'을 만들면 안 된다");
        assert_eq!(turns_of(&ing), 0);
    }

    #[test]
    fn pos_ids_are_matched_only_within_their_session() {
        let ing = ingestor();
        let mut rep = Report::default();
        for sid in ["a", "b"] {
            ing.conn.execute("INSERT INTO session (id) VALUES (?1)", params![sid]).unwrap();
        }
        let mut acc = run(&[human("u1", &at(-5), "x"), said(&at(-4), "y"), human("u2", &at(-3), "z")]).0.remove(0);
        acc.uuid = "pos-100".into();
        ing.flush_turn("a", &mut acc, &mut rep);
        crate::archive::delete_sessions(&ing.conn, &["a".into()]).unwrap();
        // 다른 세션의 같은 줄 위치 ID 는 다른 요청이다
        ing.flush_turn("b", &mut acc, &mut rep);
        let n: i64 = ing.conn.query_row("SELECT COUNT(*) FROM turn WHERE session_id = 'b'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn archived_session_comes_back_on_activity_after_archiving() {
        let ing = ingestor();
        let mut rep = Report::default();
        ing.conn.execute("INSERT INTO session (id) VALUES ('s')", []).unwrap();
        let (mut first, _) = run(&[human("u1", &at(-300), "하나"), said(&at(-299), "끝"), human("u2", &at(-298), "둘")]);
        ing.flush_turn("s", &mut first[0], &mut rep);
        crate::archive::set_archived(&ing.conn, &["s".into()], true).unwrap();
        // 보관 시각을 두 시간 전으로 — 앱이 꺼져 있던 동안 온 결과를 흉내 낸다
        ing.conn.execute("UPDATE session SET archived_at = ?1 WHERE id = 's'", params![at(-120)]).unwrap();
        let hidden = |ing: &Ingestor| -> i64 { ing.conn.query_row("SELECT hidden FROM session WHERE id = 's'", [], |r| r.get(0)).unwrap() };

        // 이미 있던 요청을 다시 써도(재수집) 보관은 그대로
        ing.flush_turn("s", &mut first[0], &mut rep);
        assert_eq!(hidden(&ing), 1);
        // 보관 전에 친 요청이 늦게 들어와도 그대로
        let (mut late, _) = run(&[human("u3", &at(-200), "셋"), said(&at(-199), "끝"), human("u4", &at(-198), "넷")]);
        ing.flush_turn("s", &mut late[0], &mut rep);
        assert_eq!(hidden(&ing), 1);
        // 보관한 뒤에 온 요청(한 시간 전 — "최근" 창 밖)은 목록으로 되돌린다
        let (mut next, _) = run(&[human("u5", &at(-60), "다섯"), said(&at(-59), "완료"), human("u6", &at(-58), "여섯")]);
        ing.flush_turn("s", &mut next[0], &mut rep);
        assert_eq!(hidden(&ing), 0);
        let since: Option<String> = ing.conn.query_row("SELECT archived_at FROM session WHERE id = 's'", [], |r| r.get(0)).unwrap();
        assert!(since.is_none());
    }

    #[test]
    fn tool_targets_become_tag_signals_and_survive_recollection() {
        let ing = ingestor();
        let mut rep = Report::default();
        ing.conn.execute("INSERT INTO session (id) VALUES ('s')", []).unwrap();
        let tool = |at: &str, id: &str, name: &str, input: Value| {
            json!({"type":"assistant","timestamp":at,"requestId":format!("r-{id}"),
                   "message":{"content":[{"type":"tool_use","id":id,"name":name,"input":input}],"usage":{"output_tokens":1}}})
        };
        let (mut closed, _) = run(&[
            human("u1", &at(-30), "알파 화면 고쳐줘"),
            tool(&at(-29), "t1", "Read", json!({"file_path": "/w/alpha/lib/main.dart"})),
            tool(&at(-28), "t2", "Bash", json!({"command": "cd /w/alpha && ls /usr/bin /tmp/x /w/beta/notes.md"})),
            tool(&at(-27), "t3", "Grep", json!({"pattern": "x", "path": "/w/alpha/lib"})),
            said(&at(-26), "끝"),
            human("u2", &at(-20), "다음"),
        ]);
        let acc = &mut closed[0];
        assert!(acc.touched.contains("/w/alpha/lib/main.dart"));
        assert!(acc.touched.contains("/w/alpha"), "cd 뒤 경로");
        assert!(acc.touched.contains("/w/beta/notes.md"));
        assert!(!acc.touched.iter().any(|p| p.starts_with("/usr") || p.starts_with("/tmp")), "시스템 경로는 신호가 아니다");
        ing.flush_turn("s", acc, &mut rep);
        let tid: i64 = ing.conn.query_row("SELECT id FROM turn WHERE session_id = 's'", [], |r| r.get(0)).unwrap();
        let saved: String = ing.conn.query_row("SELECT paths FROM turn_touch WHERE turn_id = ?1", [tid], |r| r.get(0)).unwrap();
        assert!(saved.contains("/w/alpha/lib/main.dart"));
        // 사용자가 붙인 태그는 같은 요청을 다시 써도(재수집) 남는다 — 요청 ID 가 그대로라서
        ing.conn.execute("INSERT INTO tag (name, created_at) VALUES ('수동', 't')", []).unwrap();
        ing.conn.execute("INSERT INTO turn_tag (turn_id, tag_id, state, at) VALUES (?1, 1, 'manual', 't')", [tid]).unwrap();
        ing.flush_turn("s", acc, &mut rep);
        let (again, n): (i64, i64) = ing
            .conn
            .query_row("SELECT id, (SELECT COUNT(*) FROM turn_tag WHERE turn_id = turn.id AND state = 'manual') FROM turn WHERE session_id = 's'", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((again, n), (tid, 1));
    }

    #[test]
    fn task_notification_extends_turn() {
        let (closed, open) = run(&[
            human("u1", "2026-09-23T01:00:00.000Z", "조사해줘"),
            json!({"type":"assistant","timestamp":"2026-09-23T01:00:05.000Z","requestId":"r1",
                   "message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"조사를 시작합니다"}],
                   "usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100}}}),
            json!({"type":"assistant","timestamp":"2026-09-23T01:00:06.000Z","requestId":"r1",
                   "message":{"model":"claude-opus-5-5","content":[{"type":"tool_use","id":"t1","name":"Agent",
                   "input":{"description":"깃허브 조사","subagent_type":"research","run_in_background":true}}],
                   "usage":{"input_tokens":10,"output_tokens":20,"cache_read_input_tokens":100}}}),
            json!({"type":"system","subtype":"turn_duration","timestamp":"2026-09-23T01:00:07.000Z","durationMs":7000,"pendingBackgroundAgentCount":1}),
            json!({"type":"user","timestamp":"2026-09-23T01:05:00.000Z","origin":{"kind":"task-notification"},
                   "message":{"role":"user","content":"<task-notification><tool-use-id>t1</tool-use-id><status>completed</status><summary>Agent \"깃허브 조사\" finished</summary></task-notification>"}}),
            json!({"type":"assistant","timestamp":"2026-09-23T01:05:10.000Z","requestId":"r2",
                   "message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"결과입니다. 진행할까요?"}],
                   "usage":{"input_tokens":3,"output_tokens":50,"cache_read_input_tokens":300}}}),
            json!({"type":"system","subtype":"turn_duration","timestamp":"2026-09-23T01:05:11.000Z","durationMs":11000,"pendingBackgroundAgentCount":0}),
        ]);
        assert!(closed.is_empty());
        let acc = open.unwrap();
        assert_eq!(acc.api_calls(), 2);
        assert_eq!(acc.totals().output, 70); // r1 은 최댓값 20, r2 50
        assert_eq!(acc.context_tokens(), 303);
        assert_eq!(acc.response.as_deref(), Some("결과입니다. 진행할까요?"));
        assert_eq!(acc.understanding.as_deref(), Some("조사를 시작합니다"));
        assert_eq!(acc.subs.len(), 1);
        assert!(acc.subs[0].ended_at.is_some());
        assert_eq!(acc.task_notes, 1);
        assert_eq!(acc.active_ms, 18000);
        assert_eq!(acc.pending_bg, 0);
        assert!(acc.quiet_since_stop());
    }

    #[test]
    fn new_prompt_closes_previous() {
        let (closed, open) = run(&[
            human("u1", "2026-09-23T01:00:00.000Z", "하나"),
            json!({"type":"assistant","timestamp":"2026-09-23T01:00:05.000Z","requestId":"r1",
                   "message":{"content":[{"type":"text","text":"끝"}],"usage":{"output_tokens":1}}}),
            human("u2", "2026-09-23T01:10:00.000Z", "둘"),
        ]);
        assert_eq!(closed.len(), 1);
        assert!(closed[0].closed);
        assert_eq!(open.unwrap().prompt_text, "둘");
    }

    #[test]
    fn interrupt_marks_turn() {
        let (_, open) = run(&[
            human("u1", "2026-09-23T01:00:00.000Z", "하나"),
            json!({"type":"assistant","timestamp":"2026-09-23T01:00:05.000Z","requestId":"r1",
                   "message":{"content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"ls"}}],"usage":{"output_tokens":1}}}),
            json!({"type":"user","timestamp":"2026-09-23T01:00:09.000Z","message":{"role":"user","content":[{"type":"text","text":"[Request interrupted by user]"}]}}),
        ]);
        assert!(open.unwrap().interrupted_last());
    }

    fn queued(at: &str, src: &str, mode: &str, kind: &str, prompt: &str) -> Value {
        json!({"type":"attachment","uuid":format!("att-{src}"),"timestamp":at,
               "attachment":{"type":"queued_command","prompt":prompt,"source_uuid":src,"commandMode":mode,
                             "origin":{"kind":kind},"timestamp":at,"humanTurn":kind == "human"}})
    }

    #[test]
    fn message_sent_while_working_becomes_its_own_pair() {
        let (closed, st) = run_state(&[
            human("u1", "2026-09-24T01:00:00.000Z", "중계 만들어 줘"),
            json!({"type":"assistant","timestamp":"2026-09-24T01:00:05.000Z","requestId":"r1",
                   "message":{"model":"claude-opus-5-5","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}],
                   "usage":{"output_tokens":5}}}),
            queued("2026-09-24T01:05:00.000Z", "q1", "prompt", "human", "키체인뭐야? 이게 왜 필요해?"),
            queued("2026-09-24T01:05:30.000Z", "q2", "prompt", "peer", "<cross-session-message from=\"uds:/tmp/x.sock\" from-name=\"mini-01\">알려 드려요</cross-session-message>"),
            json!({"type":"assistant","timestamp":"2026-09-24T01:06:00.000Z","requestId":"r2",
                   "message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"키체인은 macOS 비밀번호 금고입니다."},
                   {"type":"tool_use","id":"t2","name":"Bash","input":{"command":"ls"}}],"usage":{"output_tokens":9}}}),
            queued("2026-09-24T01:07:00.000Z", "q3", "task-notification", "task-notification",
                   "<task-notification><tool-use-id>t9</tool-use-id><status>completed</status><summary>CI 끝</summary></task-notification>"),
            json!({"type":"assistant","timestamp":"2026-09-24T01:30:00.000Z","requestId":"r3",
                   "message":{"model":"claude-opus-5-5","content":[{"type":"text","text":"중계 완성"}],"usage":{"output_tokens":3}}}),
        ]);
        // 작업 중에 보낸 말 → 따로 한 쌍. 앞 요청이 끝날 때까지 열려 있고(작업 중), 바로 단 글(r2)은 "지금 하는 일"
        assert!(closed.is_empty());
        let side = &st.side[0];
        assert!(side.side && !side.closed && side.answered);
        assert_eq!(side.uuid, "q1");
        assert_eq!(side.prompt_text, "키체인뭐야? 이게 왜 필요해?");
        assert_eq!(side.prompt_at, "2026-09-24T01:05:00.000Z");
        assert_eq!(side.source.as_deref(), Some("mid-turn"));
        assert_eq!(side.understanding.as_deref(), Some("키체인은 macOS 비밀번호 금고입니다."));
        assert_eq!(side.response, None);
        // 원래 요청은 그대로 이어지고, 과정에 끼어든 말이 남는다
        let main = st.open.clone().unwrap();
        assert_eq!(main.response.as_deref(), Some("중계 완성"));
        let asks: Vec<_> = main.steps.iter().filter(|s| s.kind == "ask").collect();
        assert_eq!(asks.len(), 2);
        assert_eq!(asks[1].name.as_deref(), Some("mini-01"));
        assert_eq!(main.task_notes, 1);
    }

    #[test]
    fn unanswered_side_closes_with_next_prompt() {
        let (closed, _) = run(&[
            human("u1", "2026-09-24T01:00:00.000Z", "하나"),
            json!({"type":"assistant","timestamp":"2026-09-24T01:00:05.000Z","requestId":"r1",
                   "message":{"content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"ls"}}],"usage":{"output_tokens":1}}}),
            queued("2026-09-24T01:01:00.000Z", "q1", "prompt", "human", "그것도 해 줘"),
            human("u2", "2026-09-24T01:10:00.000Z", "둘"),
        ]);
        assert_eq!(closed.len(), 2);
        assert!(closed.iter().any(|c| c.side && c.prompt_text == "그것도 해 줘" && c.closed));
    }

    #[test]
    fn side_answer_is_only_the_immediate_response() {
        // 질문 직후 응답(r2)이 도구만 부르고 글이 없으면 → 답 없음. 뒤 응답(r3)의 진행 안내를 답으로 붙이지 않는다
        let lines = [
            human("u1", "2026-09-24T01:00:00.000Z", "하나"),
            json!({"type":"assistant","timestamp":"2026-09-24T01:00:05.000Z","requestId":"r1",
                   "message":{"content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"ls"}}],"usage":{"output_tokens":1}}}),
            queued("2026-09-24T01:01:00.000Z", "q1", "prompt", "human", "키체인뭐야?"),
            json!({"type":"assistant","timestamp":"2026-09-24T01:01:05.000Z","requestId":"r2",
                   "message":{"content":[{"type":"thinking","thinking":""}],"usage":{"output_tokens":1}}}),
            json!({"type":"assistant","timestamp":"2026-09-24T01:01:06.000Z","requestId":"r2",
                   "message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"ls"}}],"usage":{"output_tokens":1}}}),
            json!({"type":"assistant","timestamp":"2026-09-24T01:01:30.000Z","requestId":"r3",
                   "message":{"content":[{"type":"text","text":"다시 빌드합니다."}],"usage":{"output_tokens":1}}}),
        ];
        // 바로 단 글이 없다(r2 는 도구만) — 뒤 응답(r3)의 진행 안내를 "바로 단 글"로 붙이지 않는다
        let (closed, st) = run_state(&lines);
        let side = &st.side[0];
        assert!(side.answered && !side.closed);
        assert_eq!(side.understanding, None);
        assert_eq!(closed.iter().filter(|c| c.side).count(), 0, "앞 요청이 끝나기 전에는 닫지 않는다");
    }

    #[test]
    fn side_follows_the_running_request_and_gets_its_final_answer() {
        // 09-28 실기기: 작업 중에 보낸 말이 곧바로 "완료 — 앞 요청 안에서 이어졌어요"로 닫혔다(바로 단 글이 기록에 없었다)
        let mut ing = ingestor();
        ing.conn.execute("INSERT INTO session (id) VALUES ('s')", []).unwrap();
        // 판정 경계(조용한 2분)에서 멀리 — 초 단위
        let at = |sec: i64| (chrono::Utc::now() + chrono::Duration::seconds(sec)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let lines = vec![
            human("u1", &at(-400), "배포해 줘"),
            json!({"type":"assistant","timestamp":at(-390),"requestId":"r1",
                   "message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"ls"}}],"usage":{"output_tokens":1}}}),
            queued(&at(-300), "q1", "prompt", "human", "끝나면 운영도 올려 줘"),
            json!({"type":"assistant","timestamp":at(-290),"requestId":"r2",
                   "message":{"content":[{"type":"tool_use","id":"t2","name":"Bash","input":{"command":"ls"}}],"usage":{"output_tokens":1}}}),
            json!({"type":"assistant","timestamp":at(-10),"requestId":"r3",
                   "message":{"content":[{"type":"tool_use","id":"t3","name":"Bash","input":{"command":"ls"}}],"usage":{"output_tokens":1}}}),
        ];
        let (closed, mut st) = run_state(&lines);
        assert!(closed.is_empty());
        let status = |ing: &Ingestor, uuid: &str| -> (String, Option<String>) {
            ing.conn
                .query_row("SELECT status, response_text FROM turn WHERE prompt_uuid = ?1", params![uuid], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
        };
        // 앞 요청이 일하는 동안: 작업 중 — 이 말을 받은 지 5분이 지나 자기 활동은 조용해도(프로세스 정보 없음) 앞 요청을 따른다
        let mut rep = Report::default();
        ing.follow_sides("s", &mut st);
        for acc in st.side.iter_mut() {
            ing.flush_turn("s", acc, &mut rep);
        }
        assert_eq!(status(&ing, "q1"), ("running".into(), None));
        // 앞 요청이 최종 보고를 쓰고 끝남 → 그 보고가 이 말의 답
        let fin = "dev 배포 끝, 운영도 올렸습니다.";
        for l in [said(&at(-5), fin), json!({"type":"system","subtype":"turn_duration","timestamp":at(-4),"durationMs":1000})] {
            let raw = serde_json::to_vec(&l).unwrap();
            let mut closed = Vec::new();
            feed(&mut st, 0, &raw, &mut closed, &mut SessionPatch::default());
        }
        st.open.as_mut().unwrap().dirty = true;
        ing.files.insert(PathBuf::from("/nowhere/s.jsonl"), st);
        ing.recheck_open(&mut Report::default());
        assert_eq!(status(&ing, "q1"), ("done".into(), Some(fin.into())));
    }

    #[test]
    fn queued_when_nothing_runs_is_a_normal_turn() {
        let (_, open) = run(&[queued("2026-09-24T01:01:00.000Z", "q1", "prompt", "human", "새 요청")]);
        let o = open.unwrap();
        assert!(!o.side);
        assert_eq!(o.prompt_text, "새 요청");
    }

    /// 꺼진 세션에 입력창으로 보낸 말(`claude --bg --resume`) — 모델이 아직 한 마디도 안 한 동안은 끝난 것이 아니다.
    /// 세션을 끄기 전에 받은 Stop 훅·띄운 직후의 등록부 idle 을 "끝남"으로 읽으면 전달 스레드가 그 백그라운드 세션을
    /// 모델이 움직이기 전에 멈추고(`conoti::link_results`), 답 없는 요청은 숨김이 되어 보낸 말이 화면에서 사라졌다(0.10.1 Windows 점검 문제 1).
    #[test]
    fn resumed_request_is_not_finished_by_an_earlier_stop_hook_or_a_fresh_idle_registry() {
        let secs = |s: i64| (chrono::Utc::now() + chrono::Duration::seconds(s)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut ing = ingestor();
        ing.conn.execute("INSERT INTO session (id) VALUES ('s')", []).unwrap();
        let row = |ing: &Ingestor, uuid: &str| -> (String, i64, String) {
            ing.conn
                .query_row("SELECT status, hidden, origin FROM turn WHERE prompt_uuid = ?1", params![uuid], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
        };
        // 터미널에서 요청을 처리하고(그 끝의 Stop 훅을 앱이 받았다) /exit 로 끈 세션에, 1초 전 입력창으로 보낸 말
        ing.live.insert("s".into(), Live { stop_hook_at: Some(secs(-60)), ..Default::default() });
        let (_, open) = run(&[
            human("u1", &secs(-120), "앞 요청"),
            said(&secs(-118), "앞 답"),
            json!({"type":"system","subtype":"turn_duration","timestamp":secs(-117),"durationMs":3000}),
            human("u2", &secs(-1), &crate::conoti::wrap_desk("3+3 의 답을 숫자 하나로만")),
        ]);
        let mut sent = open.unwrap();
        let mut rep = Report::default();
        ing.flush_turn("s", &mut sent, &mut rep);
        assert_eq!(row(&ing, "u2"), ("running".into(), 0, "inbox".into()), "그 말보다 앞선 Stop 훅은 이 요청의 끝이 아니다");
        // 이어서 띄운 백그라운드 세션이 등록부에 잠깐 idle 로 올라와도 — 보낸 지 5초가 안 됐으면 아직 작업 중
        let live = ing.live.get_mut("s").unwrap();
        live.alive = true;
        live.status = Some("idle".into());
        ing.flush_turn("s", &mut sent, &mut rep);
        assert_eq!(row(&ing, "u2").0, "running");
        // 그 말 뒤의 Stop 훅은 끝 — 답 없이 끝나도 앱에서 보낸 말은 숨기지 않는다(사용자가 "답 없이 끝남"을 봐야 한다)
        ing.live.get_mut("s").unwrap().stop_hook_at = Some(secs(0));
        ing.flush_turn("s", &mut sent, &mut rep);
        assert_eq!(row(&ing, "u2"), ("done".into(), 0, "inbox".into()));
        // 터미널에서 친 말이 답 없이 끝난 것(로컬 명령 등)은 예전처럼 숨긴다
        let (_, open) = run(&[human("u3", &secs(-30), "터미널에서 친 말")]);
        let mut typed = open.unwrap();
        ing.flush_turn("s", &mut typed, &mut rep);
        assert_eq!(row(&ing, "u3").1, 1);
        // 정말 쉬는 세션(아무 활동 없음 + 등록부 idle)은 끝으로 본다 — 앱에서 보낸 말은 첫 답 전 유예(60초)가 지난 뒤에
        ing.live.get_mut("s").unwrap().stop_hook_at = Some(secs(-600));
        let (_, open) = run(&[human("u4", &secs(-10), &crate::conoti::wrap_desk("하나 더"))]);
        ing.flush_turn("s", &mut open.unwrap(), &mut rep);
        assert_eq!(row(&ing, "u4").0, "running");
        let (_, open) = run(&[human("u5", &secs(-70), &crate::conoti::wrap_desk("또 하나"))]);
        ing.flush_turn("s", &mut open.unwrap(), &mut rep);
        assert_eq!(row(&ing, "u5"), ("done".into(), 0, "inbox".into()));
        // 터미널에서 친 말은 예전처럼 5초
        let (_, open) = run(&[human("u6", &secs(-10), "터미널 말")]);
        ing.flush_turn("s", &mut open.unwrap(), &mut rep);
        assert_eq!(row(&ing, "u6").0, "done");
        // 경계: Stop 훅이 마지막 활동과 같은 밀리초면 끝(그 활동 뒤의 멈춤)
        ing.live.get_mut("s").unwrap().alive = false;
        let t = secs(-3);
        ing.live.get_mut("s").unwrap().stop_hook_at = Some(t.clone());
        let (_, open) = run(&[human("u7", &secs(-8), "경계"), said(&t, "끝")]);
        ing.flush_turn("s", &mut open.unwrap(), &mut rep);
        assert_eq!(row(&ing, "u7").0, "done", "같은 밀리초의 Stop 훅");
    }

    /// 답 없는 앱 발송 말에 가짜 "끝남" 알림을 내지 않는다(리뷰 필수 2) — 이어서 띄운 세션이 등록부에 idle 로 보여도(Windows 의 느린 기동)
    /// 첫 답 전 유예 안에는 끝남이 아니고, 답 없이 끝났어도 알림은 유예가 지난 뒤 한 번만("답을 받지 못했습니다 — …").
    /// 유예 안에 진짜 답이 오면 그 답으로 한 번만 알린다.
    #[test]
    fn app_message_without_an_answer_is_not_announced_before_the_grace() {
        let secs = |s: i64| (chrono::Utc::now() + chrono::Duration::seconds(s)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let mut ing = ingestor();
        ing.conn.execute("INSERT INTO session (id) VALUES ('s')", []).unwrap();
        let status = |ing: &Ingestor, uuid: &str| -> String {
            ing.conn.query_row("SELECT status FROM turn WHERE prompt_uuid = ?1", params![uuid], |r| r.get(0)).unwrap()
        };
        // 느린 기동: 등록부 idle, 보낸 지 6초, 아직 답 없음 → 작업 중(끝남도 알림도 없음)
        ing.live.insert("s".into(), Live { alive: true, status: Some("idle".into()), ..Default::default() });
        let (_, open) = run(&[human("u1", &secs(-6), &crate::conoti::wrap_desk("느린 기동"))]);
        let mut a = open.unwrap();
        let mut rep = Report::default();
        ing.flush_turn("s", &mut a, &mut rep);
        assert_eq!(status(&ing, "u1"), "running", "보낸 지 몇 초 안 된 앱 발송 말은 등록부가 idle 이어도 작업 중");
        assert!(rep.finished.is_empty());
        // 그 말 뒤의 Stop 훅 — 답 없이 끝남(API 오류 등). 숨기지 않지만 알림은 유예 뒤로
        ing.live.get_mut("s").unwrap().stop_hook_at = Some(secs(0));
        ing.flush_turn("s", &mut a, &mut rep);
        assert_eq!(status(&ing, "u1"), "done");
        assert!(rep.finished.is_empty(), "답 없는 끝남은 바로 알리지 않는다");
        ing.notify_deferred(&mut rep);
        assert!(rep.finished.is_empty(), "유예 안에는 미룬다");
        // 유예가 지나도 답이 없으면 한 번 알린다
        ing.conn.execute("UPDATE turn SET prompt_at = ?1 WHERE prompt_uuid = 'u1'", [secs(-120)]).unwrap();
        ing.notify_deferred(&mut rep);
        assert_eq!(rep.finished.len(), 1);
        assert!(rep.finished[0].text.starts_with("답을 받지 못했습니다"), "{}", rep.finished[0].text);
        ing.notify_deferred(&mut rep);
        assert_eq!(rep.finished.len(), 1, "한 번만");
        // 사용자가 직접 중단한 앱 발송 말은 유예가 지나도 알리지 않는다
        let (_, open) = run(&[
            human("u9", &secs(-120), &crate::conoti::wrap_desk("멈출 말")),
            json!({"type":"user","timestamp":secs(-119),"message":{"role":"user","content":"[Request interrupted by user]"}}),
        ]);
        ing.flush_turn("s", &mut open.unwrap(), &mut rep);
        assert_eq!(status(&ing, "u9"), "interrupted");
        ing.notify_deferred(&mut rep);
        assert_eq!(rep.finished.len(), 1, "중단은 알림 없음");
        // 유예 안에 진짜 답이 오면(가짜 끝남 → 답 붙은 끝남) 그 답으로 한 번만
        let mut rep = Report::default();
        ing.live.get_mut("s").unwrap().stop_hook_at = Some(secs(-4));
        let (_, open) = run(&[human("u2", &secs(-5), &crate::conoti::wrap_desk("둘째"))]);
        ing.flush_turn("s", &mut open.unwrap(), &mut rep);
        assert_eq!(status(&ing, "u2"), "done");
        assert!(rep.finished.is_empty());
        let (_, open) = run(&[
            human("u2", &secs(-5), &crate::conoti::wrap_desk("둘째")),
            said(&secs(-2), "6"),
            json!({"type":"system","subtype":"turn_duration","timestamp":secs(-1),"durationMs":3000}),
        ]);
        ing.flush_turn("s", &mut open.unwrap(), &mut rep);
        assert_eq!(rep.finished.len(), 1, "답이 붙어 끝나면 알린다");
        assert_eq!(rep.finished[0].text, "6");
        ing.notify_deferred(&mut rep);
        assert_eq!(rep.finished.len(), 1);
    }

    /// 실제 claude 로 위 결함을 끝까지: 꺼진 세션(앱이 그 세션의 Stop 훅을 받아 둔 상태 — 터미널에서 요청을 처리하고 끈 세션과 같다)에
    /// 입력창으로 보내 `--bg --resume` 으로 이어서 띄우고, 수집기(이 세션 기록 한 파일만 — 사용자의 다른 기록은 읽지 않는다)와
    /// 전달 스레드를 앱과 같은 주기로 함께 돌린다. 답이 기록에 남고 · 그 요청이 숨김 없이 끝나고 · 보낸 말이 처리됨이 되는가.
    /// 신뢰한 폴더가 필요하다(새 세션 1번 + 보내기 1번). `AI_INBOX_LIVE_SID=<앞서 만든 세션>` 이면 새 세션을 만들지 않는다.
    /// `AI_INBOX_LIVE_DIR=<신뢰한 폴더> cargo test live_ended_session_send -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_ended_session_send() {
        use crate::{conoti, deliver};
        use std::time::{Duration, Instant};
        let dir = PathBuf::from(std::env::var("AI_INBOX_LIVE_DIR").expect("AI_INBOX_LIVE_DIR"));
        let data = std::env::temp_dir().join(format!("aiinbox-live-ended-{}", std::process::id()));
        paths::set_data_dir_override(data.clone());
        let conn = db::open(&paths::db_path()).unwrap();
        db::migrate(&conn).unwrap();
        let transcript = |sid: &str| -> Option<PathBuf> {
            std::fs::read_dir(paths::projects_dir()).ok()?.flatten().map(|d| d.path().join(format!("{sid}.jsonl"))).find(|p| p.is_file())
        };
        let answered_after = |sid: &str, marker: &str| -> bool {
            // 그 말이 처음 나온 줄 뒤의 어시스턴트 줄(파일 끝의 요약 줄에도 같은 글이 있다)
            let body = transcript(sid).and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
            body.find(marker).is_some_and(|i| body[i..].lines().skip(1).any(|l| l.contains("\"type\":\"assistant\"")))
        };
        let wait = |what: &str, secs: u64, mut ok: Box<dyn FnMut() -> bool + '_>| {
            let until = Instant::now() + Duration::from_secs(secs);
            while !ok() {
                assert!(Instant::now() < until, "시간 초과: {what}");
                std::thread::sleep(Duration::from_secs(1));
            }
        };
        // 1. 세션 하나 — 백그라운드 새 작업으로 만들고 답을 받은 뒤 끈다
        let sid = match std::env::var("AI_INBOX_LIVE_SID") {
            Ok(s) => s,
            Err(_) => {
                let (_, sid) = deliver::start_session(&dir, "ok1", &conoti::wrap_desk("ok1 이라고만 답해")).expect("새 작업");
                let sid = sid.expect("세션 ID");
                wait("첫 답", 150, Box::new(|| answered_after(&sid, "ok1 이라고만 답해")));
                wait("쉬는 상태", 60, Box::new(|| deliver::route(&sid) == deliver::Route::Idle));
                sid
            }
        };
        println!("session {sid}");
        deliver::stop_session(&sid);
        wait("세션 꺼짐", 30, Box::new(|| deliver::route(&sid) == deliver::Route::Ended));
        conn.execute("INSERT OR IGNORE INTO session (id, project_dir) VALUES (?1, ?2)", params![sid, dir.to_string_lossy()]).unwrap();

        // 2. 앱이 이 세션의 마지막 Stop 훅을 받아 둔 상태 — 수집기는 이 세션 기록만 읽는다
        let path = transcript(&sid).expect("대화 기록");
        let mut ing = ingestor();
        ing.conn = db::open(&paths::db_path()).unwrap();
        ing.files.insert(path.clone(), FileState::fresh(sid.clone(), 0, 0, false, false));
        let mut rep = Report::default();
        ing.apply_hook(&json!({"hook_event_name": "Stop", "session_id": sid, "received_at_ms": now_ms()}), &mut rep);
        let step = |ing: &mut Ingestor| {
            let mut rep = Report::default();
            ing.drain_spool(&mut rep);
            ing.refresh_registry(&mut rep);
            let md = std::fs::metadata(&path).unwrap();
            let mtime = md.modified().unwrap().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64;
            ing.process_file(&path, md.len(), mtime, &mut rep);
            ing.recheck_open(&mut rep);
        };
        step(&mut ing);

        // 3. 입력창으로 보낸다 — 전달 스레드(2초)·수집기(1.5초)를 번갈아
        let text = format!("{} 더하기 {} 의 답을 숫자 하나로만", chrono::Utc::now().timestamp() % 89 + 10, 3);
        let rid = conoti::accept_desktop(&conn, &sid, &text, &[], None).expect("받기")["rid"].as_str().unwrap().to_string();
        let deliverer = deliver::LocalDeliver { bg_resume: false };
        let mut pipe = conoti::Pipeline::new(db::open(&paths::db_path()).unwrap());
        let until = Instant::now() + Duration::from_secs(150);
        let (mut answered, mut handled) = (false, false);
        while Instant::now() < until && !(answered && handled) {
            pipe.tick(&deliverer);
            step(&mut ing);
            let (st, note): (String, Option<String>) =
                conn.query_row("SELECT state, note FROM conoti_reply WHERE reply_id = ?1", params![rid], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            let turn: Option<(String, i64, i64)> = conn
                .query_row(
                    "SELECT status, hidden, api_calls FROM turn WHERE session_id = ?1 AND origin = 'inbox' AND prompt_text = ?2 ORDER BY seq DESC LIMIT 1",
                    params![sid, text],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .unwrap();
            let reg = deliver::live_entry(&sid).map(|l| l.status.unwrap_or_default());
            answered = answered_after(&sid, &text);
            handled = st == "handled";
            println!("  reply={st} {note:?} · turn={turn:?} · registry={reg:?} · answered={answered}");
            std::thread::sleep(Duration::from_millis(1500));
        }
        deliver::stop_session(&sid);
        let _ = std::fs::remove_dir_all(&data);
        assert!(answered, "꺼진 세션에 보낸 말의 답이 기록되지 않음");
        assert!(handled, "답이 왔는데 처리됨이 되지 않음");
        println!("ok — {}", path.display());
    }

    #[test]
    fn inbox_messages_lose_header_and_keep_origin() {
        let first = crate::conoti::wrap_desk("테스트 돌려 줘");
        let chan = format!("<channel source=\"ai-inbox\" reply_id=\"dk1\">\n{}\n</channel>", crate::conoti::wrap_desk("하나 더"));
        let (closed, open) = run(&[
            human("u1", "2026-09-24T01:00:00.000Z", &first),
            json!({"type":"assistant","timestamp":"2026-09-24T01:00:05.000Z","requestId":"r1",
                   "message":{"content":[{"type":"text","text":"네"}],"usage":{"output_tokens":1}}}),
            json!({"type":"user","uuid":"u2","timestamp":"2026-09-24T01:02:00.000Z","origin":{"kind":"channel"},
                   "message":{"role":"user","content":chan}}),
            json!({"type":"assistant","timestamp":"2026-09-24T01:02:05.000Z","requestId":"r2",
                   "message":{"content":[{"type":"tool_use","id":"t","name":"Bash","input":{"command":"ls"}}],"usage":{"output_tokens":1}}}),
            queued("2026-09-24T01:03:00.000Z", "q1", "prompt", "human", &crate::conoti::wrap_desk("그것도")),
        ]);
        assert_eq!((closed[0].origin.as_str(), closed[0].prompt_text.as_str()), ("inbox", "테스트 돌려 줘"));
        let o = open.unwrap();
        assert_eq!((o.origin.as_str(), o.prompt_text.as_str()), ("inbox", "하나 더"));
        // 작업 중에 보낸 말도 머리말 없이
        let asks: Vec<_> = o.steps.iter().filter(|s| s.kind == "ask").collect();
        assert_eq!(asks[0].text, "그것도");
    }

    #[test]
    fn rewake_from_waiter_is_a_new_inbox_request() {
        let wake = format!(
            "<task-notification>\n<summary>Stop hook feedback</summary>\n</task-notification>\n<system-reminder>\nStop hook blocking error from command \"Stop\": {}\n\n{}\n</system-reminder>",
            crate::wake::NOTE,
            crate::conoti::wrap_desk("이어서 배포해 줘")
        );
        let (closed, open) = run(&[
            human("u1", "2026-09-24T01:00:00.000Z", "빌드해 줘"),
            json!({"type":"assistant","timestamp":"2026-09-24T01:00:05.000Z","requestId":"r1",
                   "message":{"content":[{"type":"text","text":"빌드했습니다"}],"usage":{"output_tokens":1}}}),
            json!({"type":"user","uuid":"w1","timestamp":"2026-09-24T01:10:00.000Z","origin":{"kind":"task-notification"},
                   "promptSource":"system","message":{"role":"user","content":wake}}),
            // 터미널에 보이라고 답 첫머리에 옮겨 적은 받은 말 — 앱에는 요청이 따로 있으니 뗀다
            json!({"type":"assistant","timestamp":"2026-09-24T01:10:03.000Z","requestId":"r2",
                   "message":{"content":[{"type":"text","text":"> 📥 AI Inbox 앱에서 보낸 사용자 메시지\n>\n> 이어서 배포해 줘\n\n배포합니다."}],"usage":{"output_tokens":1}}}),
            json!({"type":"assistant","timestamp":"2026-09-24T01:10:09.000Z","requestId":"r3",
                   "message":{"content":[{"type":"text","text":"> 📥 인용은 첫 글만 뗀다\n\n배포했습니다"}],"usage":{"output_tokens":1}}}),
        ]);
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].response.as_deref(), Some("빌드했습니다"));
        let o = open.unwrap();
        assert_eq!((o.origin.as_str(), o.prompt_text.as_str()), ("inbox", "이어서 배포해 줘"));
        assert_eq!(o.understanding.as_deref(), Some("배포합니다."));
        assert_eq!(o.response.as_deref(), Some("> 📥 인용은 첫 글만 뗀다\n\n배포했습니다"));
    }

    #[test]
    fn typed_prompt_keeps_echo_like_quote() {
        let (_, open) = run(&[
            human("u1", "2026-09-24T01:00:00.000Z", "인용해 줘"),
            json!({"type":"assistant","timestamp":"2026-09-24T01:00:05.000Z","requestId":"r1",
                   "message":{"content":[{"type":"text","text":"> 📥 받은 글\n\n끝"}],"usage":{"output_tokens":1}}}),
        ]);
        assert_eq!(open.unwrap().response.as_deref(), Some("> 📥 받은 글\n\n끝"));
    }
}
