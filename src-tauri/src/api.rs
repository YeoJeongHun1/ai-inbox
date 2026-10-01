//! 화면(React)이 부르는 명령. 읽기는 UI 전용 연결 하나로, 쓰기는 읽음·별표 같은 사용자 상태뿐이다.

use std::sync::atomic::Ordering;

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;
use serde_json::Value;
use tauri::State;

use crate::{db, install, paths, relay, time, AppState};

type R<T> = Result<T, String>;

fn e<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}

pub(crate) const FINISHED_SQL: &str = "('done','interrupted','stopped')";

// ── 세션 목록 ────────────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct SessionItem {
    id: String,
    name: String,
    named: bool,
    project_dir: Option<String>,
    project_name: Option<String>,
    git_branch: Option<String>,
    live_status: Option<String>,
    pinned: bool,
    cost_usd: Option<f64>,
    model: Option<String>,
    turns: i64,
    unread: i64,
    attention: i64,
    active: i64,
    last_turn_id: i64,
    last_status: String,
    last_needs_input: bool,
    last_origin: Option<String>,
    last_preview: String,
    last_from_ai: bool,
    last_at: Option<String>,
    /// 보관한 세션(`filter = "archived"` 로만 나온다)
    archived: bool,
    /// claude | codex
    agent: &'static str,
    /// /clear 로 끝난 대화면 그 처리(삭제 예약·보관·미정) — 화면이 흐리게 그린다
    ended: Option<crate::lifecycle::Ended>,
    /// 대표 태그 id(요청 태그에서 파생 — 큰 태그·요청 수 순, 최대 3)
    tags: Vec<i64>,
}

/// 세션 표의 agent 값 → 화면·폰에 싣는 이름
pub(crate) fn agent_of(v: Option<String>) -> &'static str {
    if v.as_deref() == Some(crate::codex::AGENT) { "codex" } else { "claude" }
}

pub(crate) fn project_name(dir: &Option<String>) -> Option<String> {
    dir.as_ref().and_then(|d| {
        let d = d.trim_end_matches(['/', '\\']);
        d.rsplit(['/', '\\']).next().map(str::to_string)
    })
}

pub(crate) fn display_name(live: Option<String>, title: Option<String>, agent: Option<String>, first_prompt: Option<String>, dir: &Option<String>) -> (String, bool) {
    if let Some(n) = live.or(title).or(agent).filter(|s| !s.trim().is_empty()) {
        return (n, true);
    }
    if let Some(p) = first_prompt.filter(|s| !s.trim().is_empty()) {
        let line = crate::text::first_line(&p).to_string();
        return (crate::text::clip(&line, 28), false);
    }
    (project_name(dir).unwrap_or_else(|| "이름 없는 세션".into()), false)
}

fn preview(status: &str, prompt: Option<String>, response: Option<String>, summary: Option<String>) -> (String, bool) {
    let from_ai = matches!(status, "done" | "interrupted" | "stopped" | "background") && (response.is_some() || summary.is_some());
    let body = if from_ai { summary.or(response) } else { prompt };
    let mut body = body.unwrap_or_default();
    // 폰에서 온 답: 머리말·원래 요청 줄을 떼고 "폰 답:" 으로
    if let Some(rest) = body.strip_prefix(crate::conoti::REPLY_HEADER) {
        let reply = rest.split_once("\n\n").map(|(_, r)| r).unwrap_or(rest);
        body = format!("폰 답: {}", crate::text::split_quote(reply).1.trim());
    } else if let (Some(_), rest) = crate::text::split_quote(&body) {
        body = rest.to_string();
    }
    let flat: String = body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .replace("**", "")
        .replace('`', "");
    (crate::text::clip(&flat, 120), from_ai)
}

#[tauri::command]
pub fn list_sessions(state: State<AppState>, filter: String, query: String) -> R<Vec<SessionItem>> {
    let conn = state.conn.lock().map_err(e)?;
    list_sessions_for(&conn, &filter, &query, false)
}

/// 폰(코노티)이 보는 목록 — 이력으로 보관한 세션도 보통 세션처럼 들어 있다(폰은 이력 탭이 없다)
pub fn list_sessions_on(conn: &Connection, filter: &str, query: &str) -> R<Vec<SessionItem>> {
    list_sessions_for(conn, filter, query, true)
}

/// `filter`: all | unread | attention | active | archived(보관함) | history(/clear 뒤 이력으로 보관한 것).
/// `with_kept` 가 false 면 이력 보관 세션은 history 에서만 나온다.
pub fn list_sessions_for(conn: &Connection, filter: &str, query: &str, with_kept: bool) -> R<Vec<SessionItem>> {
    let q = query.trim().to_string();
    let like = format!("%{}%", q.replace('%', "\\%").replace('_', "\\_"));
    let sql = format!(
        "SELECT s.id, s.live_name, s.title, s.agent_name, s.project_dir, s.git_branch, s.live_status, s.pinned,
                s.cost_usd, s.model,
                (SELECT COUNT(*) FROM turn t WHERE t.session_id = s.id AND t.hidden = 0),
                (SELECT COUNT(*) FROM turn t WHERE t.session_id = s.id AND t.hidden = 0 AND t.read_at IS NULL
                     AND t.status IN {f}),
                (SELECT COUNT(*) FROM turn t WHERE t.session_id = s.id AND t.hidden = 0
                     AND (t.status = 'waiting' OR (t.needs_input = 1 AND t.read_at IS NULL AND t.status = 'done'))),
                (SELECT COUNT(*) FROM turn t WHERE t.session_id = s.id AND t.hidden = 0
                     AND t.status IN ('running','background','waiting')),
                lt.id, lt.status, lt.needs_input, lt.origin, lt.prompt_text, lt.response_text, lt.summary,
                COALESCE(lt.ended_at, lt.last_activity_at, lt.prompt_at),
                (SELECT prompt_text FROM turn t WHERE t.session_id = s.id AND t.hidden = 0 ORDER BY seq LIMIT 1),
                s.hidden, s.agent, s.cleared_at, s.clear_state, s.purge_at, s.clear_asked
           FROM session s
           JOIN turn lt ON lt.id = (SELECT id FROM turn t WHERE t.session_id = s.id AND t.hidden = 0 ORDER BY seq DESC LIMIT 1)
          WHERE ((?3 = 'history' AND s.clear_state = 'keep')
                 OR (?3 = 'archived' AND s.hidden = 1)
                 OR (?3 NOT IN ('history','archived') AND s.hidden = 0 AND (?4 = 1 OR COALESCE(s.clear_state, '') <> 'keep')))
            AND (?1 = '' OR s.live_name LIKE ?2 ESCAPE '\\' OR s.title LIKE ?2 ESCAPE '\\'
                 OR s.agent_name LIKE ?2 ESCAPE '\\' OR s.project_dir LIKE ?2 ESCAPE '\\'
                 OR EXISTS (SELECT 1 FROM turn t WHERE t.session_id = s.id AND t.hidden = 0
                            AND (t.prompt_text LIKE ?2 ESCAPE '\\' OR t.response_text LIKE ?2 ESCAPE '\\')))
          ORDER BY s.pinned DESC, COALESCE(lt.ended_at, lt.last_activity_at, lt.prompt_at) DESC
          LIMIT 400",
        f = FINISHED_SQL
    );
    let mut st = conn.prepare(&sql).map_err(e)?;
    let rows = st
        .query_map(params![q, like, filter, with_kept as i64], |r| {
            let dir: Option<String> = r.get(4)?;
            let (name, named) = display_name(r.get(1)?, r.get(2)?, r.get(3)?, r.get(22)?, &dir);
            let status: String = r.get(15)?;
            let (pv, from_ai) = preview(&status, r.get(18)?, r.get(19)?, r.get(20)?);
            Ok(SessionItem {
                id: r.get(0)?,
                name,
                named,
                project_name: project_name(&dir),
                project_dir: dir,
                git_branch: r.get(5)?,
                live_status: r.get(6)?,
                pinned: r.get::<_, i64>(7)? != 0,
                cost_usd: r.get(8)?,
                model: r.get(9)?,
                turns: r.get(10)?,
                unread: r.get(11)?,
                attention: r.get(12)?,
                active: r.get(13)?,
                last_turn_id: r.get(14)?,
                last_status: status,
                last_needs_input: r.get::<_, i64>(16)? != 0,
                last_origin: r.get(17)?,
                last_preview: pv,
                last_from_ai: from_ai,
                last_at: r.get(21)?,
                archived: r.get::<_, i64>(23)? != 0,
                agent: agent_of(r.get(24)?),
                ended: crate::lifecycle::ended_of(r.get(25)?, r.get(26)?, r.get(27)?, r.get(28)?),
                tags: Vec::new(),
            })
        })
        .map_err(e)?;
    let mut out: Vec<SessionItem> = rows.flatten().collect();
    let top = crate::tags::session_top_tags(conn, 3);
    for s in &mut out {
        if let Some(t) = top.get(&s.id) {
            s.tags = t.clone();
        }
    }
    match filter {
        "unread" => out.retain(|s| s.unread > 0),
        "attention" => out.retain(|s| s.attention > 0),
        "active" => out.retain(|s| s.active > 0),
        _ => {}
    }
    Ok(out)
}

// ── 채팅(요청 목록) ──────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct TurnBubble {
    id: i64,
    seq: i64,
    origin: Option<String>,
    prompt_source: Option<String>,
    peer_name: Option<String>,
    prompt_at: String,
    prompt_text: Option<String>,
    slash_command: Option<String>,
    understanding: Option<String>,
    summary: Option<String>,
    response_text: Option<String>,
    status: String,
    needs_input: bool,
    ended_at: Option<String>,
    last_activity_at: Option<String>,
    duration_ms: Option<i64>,
    active_ms: Option<i64>,
    ttfr_ms: Option<i64>,
    model: Option<String>,
    effort: Option<String>,
    api_calls: i64,
    input_tokens: i64,
    output_tokens: i64,
    thinking_tokens: i64,
    cache_create_5m: i64,
    cache_create_1h: i64,
    cache_read: i64,
    web_search: i64,
    web_fetch: i64,
    context_tokens: i64,
    tool_calls: i64,
    files_changed: i64,
    subagent_count: i64,
    task_notifications: i64,
    error_count: i64,
    pending_bg: i64,
    cwd: Option<String>,
    git_branch: Option<String>,
    plan_json: Option<String>,
    read_at: Option<String>,
    starred: bool,
    /// 진행 중 표시용: 마지막 작업 과정 (kind, name, text)
    last_step: Option<(String, Option<String>, Option<String>)>,
    /// 요청에 붙은 이미지 id — 본문 끝의 경로 목록을 떼어 낸 것(`attach::split_block`)
    atts: Vec<String>,
    /// 요청 태그(뗀 것 제외) — 수동·자동·모델 제안 순
    tags: Vec<crate::tags::TurnTag>,
}

const BUBBLE_COLS: &str = "id, seq, origin, prompt_source, peer_name, prompt_at, prompt_text, slash_command, understanding,
    summary, response_text, status, needs_input, ended_at, last_activity_at, duration_ms, active_ms, ttfr_ms, model,
    effort, api_calls, input_tokens, output_tokens, thinking_tokens, cache_create_5m, cache_create_1h, cache_read,
    web_search, web_fetch, context_tokens, tool_calls, files_changed, subagent_count, task_notifications, error_count,
    pending_bg, cwd, git_branch, plan_json, read_at, starred,
    (SELECT json_array(s.kind, s.name, substr(s.text, 1, 300)) FROM turn_step s WHERE s.turn_id = turn.id ORDER BY s.seq DESC LIMIT 1)";

fn bubble(r: &Row) -> rusqlite::Result<TurnBubble> {
    let raw: Option<String> = r.get(6)?;
    let (prompt_text, atts) = match raw {
        Some(t) => {
            let (body, ids) = crate::attach::split_block(&t);
            (Some(body), ids)
        }
        None => (None, vec![]),
    };
    Ok(TurnBubble {
        id: r.get(0)?,
        seq: r.get(1)?,
        origin: r.get(2)?,
        prompt_source: r.get(3)?,
        peer_name: r.get(4)?,
        prompt_at: r.get(5)?,
        prompt_text,
        slash_command: r.get(7)?,
        understanding: r.get(8)?,
        summary: r.get(9)?,
        response_text: r.get(10)?,
        status: r.get(11)?,
        needs_input: r.get::<_, i64>(12)? != 0,
        ended_at: r.get(13)?,
        last_activity_at: r.get(14)?,
        duration_ms: r.get(15)?,
        active_ms: r.get(16)?,
        ttfr_ms: r.get(17)?,
        model: r.get(18)?,
        effort: r.get(19)?,
        api_calls: r.get(20)?,
        input_tokens: r.get(21)?,
        output_tokens: r.get(22)?,
        thinking_tokens: r.get(23)?,
        cache_create_5m: r.get(24)?,
        cache_create_1h: r.get(25)?,
        cache_read: r.get(26)?,
        web_search: r.get(27)?,
        web_fetch: r.get(28)?,
        context_tokens: r.get(29)?,
        tool_calls: r.get(30)?,
        files_changed: r.get(31)?,
        subagent_count: r.get(32)?,
        task_notifications: r.get(33)?,
        error_count: r.get(34)?,
        pending_bg: r.get(35)?,
        cwd: r.get(36)?,
        git_branch: r.get(37)?,
        plan_json: r.get(38)?,
        read_at: r.get(39)?,
        starred: r.get::<_, i64>(40)? != 0,
        last_step: r
            .get::<_, Option<String>>(41)?
            .and_then(|j| serde_json::from_str::<(String, Option<String>, Option<String>)>(&j).ok()),
        atts,
        tags: Vec::new(),
    })
}

#[derive(Serialize)]
pub struct SessionHeader {
    id: String,
    name: String,
    named: bool,
    project_dir: Option<String>,
    project_name: Option<String>,
    git_branch: Option<String>,
    live_status: Option<String>,
    cc_version: Option<String>,
    model: Option<String>,
    cost_usd: Option<f64>,
    lines_added: Option<i64>,
    lines_removed: Option<i64>,
    first_at: Option<String>,
    last_at: Option<String>,
    transcript_path: Option<String>,
    pinned: bool,
    /// 보관한 세션(목록·폰에 없다)
    archived: bool,
    turns: i64,
    unread: i64,
    active_ms: i64,
    output_tokens: i64,
    input_total: i64,
    api_calls: i64,
    resume_command: String,
    /// Windows 에서만: 셸별 이어가기 명령(PowerShell · 명령 프롬프트 · Git Bash). 다른 OS 는 비어 있다
    resume_shells: Vec<ShellCommand>,
    /// 폰 답: 0 받음(기본) · 1 막음
    conoti_mode: i64,
    /// AI Inbox 채널과 함께 실행 중
    channel_live: bool,
    /// 입력창에서 보내면 어떻게 들어가나: live(바로) · queue(일하는 중) · approve(권한 승인 대기) · connecting(다시 잇는 중)
    /// · resume(꺼진 세션 이어서 실행) · terminal(대기 훅 미설치 — 못 넣음)
    send_mode: &'static str,
    /// 실행 중인 백그라운드 세션이면 터미널에서 여는 명령
    attach_command: Option<String>,
    /// claude | codex
    agent: &'static str,
    /// /clear 로 끝난 대화면 그 처리
    ended: Option<crate::lifecycle::Ended>,
}

fn send_mode_of(sid: &str) -> (&'static str, Option<String>) {
    use crate::deliver::{route, short_of, Route};
    if crate::codex::is_codex(sid) {
        let mode = match route(sid) {
            Route::Live => "live",
            Route::Busy(_) => "queue",
            _ => "resume",
        };
        return (mode, None);
    }
    let bg = crate::deliver::live_entry(sid).map(|l| l.background).unwrap_or(false);
    let attach = bg.then(|| format!("claude attach {}", short_of(sid)));
    let mode = match route(sid) {
        Route::Channel | Route::Live => "live",
        Route::Busy(Some(_)) => "approve",
        Route::Busy(None) => "queue",
        Route::Unarmed => "connecting",
        Route::Idle | Route::Ended => "resume",
        Route::Terminal => "terminal",
    };
    (mode, attach)
}

fn session_header(conn: &Connection, sid: &str) -> R<SessionHeader> {
    let mut h = session_header_row(conn, sid)?;
    let valid = crate::channel::valid_session_id(&h.id);
    let (mode, attach) = if valid { send_mode_of(&h.id) } else { ("terminal", None) };
    h.send_mode = mode;
    h.attach_command = attach;
    Ok(h)
}

fn session_header_row(conn: &Connection, sid: &str) -> R<SessionHeader> {
    conn.query_row(
        &format!(
            "SELECT s.id, s.live_name, s.title, s.agent_name, s.project_dir, s.git_branch, s.live_status, s.cc_version,
                    s.model, s.cost_usd, s.lines_added, s.lines_removed, s.first_at, s.last_at, s.transcript_path, s.pinned,
                    (SELECT prompt_text FROM turn t WHERE t.session_id = s.id AND t.hidden = 0 ORDER BY seq LIMIT 1),
                    (SELECT COUNT(*) FROM turn t WHERE t.session_id = s.id AND t.hidden = 0),
                    (SELECT COUNT(*) FROM turn t WHERE t.session_id = s.id AND t.hidden = 0 AND t.read_at IS NULL AND t.status IN {f}),
                    (SELECT CAST(TOTAL(active_ms) AS INTEGER) FROM turn t WHERE t.session_id = s.id),
                    (SELECT CAST(TOTAL(output_tokens) AS INTEGER) FROM turn t WHERE t.session_id = s.id),
                    (SELECT CAST(TOTAL(input_tokens + cache_create_5m + cache_create_1h + cache_read) AS INTEGER) FROM turn t WHERE t.session_id = s.id),
                    (SELECT CAST(TOTAL(api_calls) AS INTEGER) FROM turn t WHERE t.session_id = s.id),
                    (SELECT CASE WHEN mode = 1 THEN 1 ELSE 0 END FROM conoti_session c WHERE c.session_id = s.id),
                    s.hidden, s.agent, s.cleared_at, s.clear_state, s.purge_at, s.clear_asked
               FROM session s WHERE s.id = ?1",
            f = FINISHED_SQL
        ),
        params![sid],
        |r| {
            let dir: Option<String> = r.get(4)?;
            let (name, named) = display_name(r.get(1)?, r.get(2)?, r.get(3)?, r.get(16)?, &dir);
            let id: String = r.get(0)?;
            let agent = agent_of(r.get(25)?);
            let base = if agent == "codex" { "codex resume" } else { "claude --resume" };
            let resume = if agent == "codex" { resume_command_with(dir.as_deref(), &id, base) } else { resume_command(dir.as_deref(), &id) };
            let resume_shells = if cfg!(windows) { resume_commands_windows(dir.as_deref(), &id, base) } else { vec![] };
            Ok(SessionHeader {
                name,
                named,
                project_name: project_name(&dir),
                project_dir: dir,
                git_branch: r.get(5)?,
                live_status: r.get(6)?,
                cc_version: r.get(7)?,
                model: r.get(8)?,
                cost_usd: r.get(9)?,
                lines_added: r.get(10)?,
                lines_removed: r.get(11)?,
                first_at: r.get(12)?,
                last_at: r.get(13)?,
                transcript_path: r.get(14)?,
                pinned: r.get::<_, i64>(15)? != 0,
                turns: r.get(17)?,
                unread: r.get(18)?,
                active_ms: r.get(19)?,
                output_tokens: r.get(20)?,
                input_total: r.get(21)?,
                api_calls: r.get(22)?,
                resume_command: resume,
                resume_shells,
                conoti_mode: r.get::<_, Option<i64>>(23)?.unwrap_or(0),
                archived: r.get::<_, i64>(24)? != 0,
                channel_live: crate::channel::channel_alive(&id),
                send_mode: "",
                attach_command: None,
                agent,
                ended: crate::lifecycle::ended_of(r.get(26)?, r.get(27)?, r.get(28)?, r.get(29)?),
                id,
            })
        },
    )
    .map_err(e)
}

/// 셸에 붙여 넣을 이어가기 명령. 경로·ID 는 대화 기록에서 온 **신뢰할 수 없는 값**이므로
/// 경로는 작은따옴표로 감싸 안의 문자를 전부 글자로 만들고, 세션 ID 는 형식이 맞을 때만 넣는다.
pub fn resume_command(dir: Option<&str>, id: &str) -> String {
    resume_command_with(dir, id, "claude --resume")
}

/// `base` 는 이 앱이 정한 고정 문자열(`claude --resume` · `codex resume`)만 넘긴다
pub fn resume_command_with(dir: Option<&str>, id: &str, base: &str) -> String {
    let resume = resume_part(id, base);
    let Some(d) = usable_dir(dir) else { return resume };
    if cfg!(windows) {
        in_powershell(d, &resume)
    } else {
        in_sh(d, &resume)
    }
}

fn resume_part(id: &str, base: &str) -> String {
    let id_ok = (8..=64).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    if id_ok { format!("{base} {id}") } else { base.to_string() }
}

fn usable_dir(dir: Option<&str>) -> Option<&str> {
    dir.filter(|d| !d.is_empty() && !d.chars().any(char::is_control))
}

/// PowerShell 이 작은따옴표로 치는 글자 — ASCII `'` 말고도 ‘ ’ ‚ ‛(U+2018~201B). 어느 것이든 작은따옴표 문자열을 닫는다
fn ps_single_quote(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}')
}

/// PowerShell: 작은따옴표 안에서는 작은따옴표(위 다섯 글자)만 특별하다 — 두 번 쓰면 그 글자 하나가 된다
fn in_powershell(d: &str, resume: &str) -> String {
    let mut q = String::with_capacity(d.len() + 8);
    for c in d.chars() {
        if ps_single_quote(c) {
            q.push(c);
        }
        q.push(c);
    }
    format!("Set-Location -LiteralPath '{q}'; {resume}")
}

/// sh/bash/zsh: 작은따옴표 안에서는 아무것도 해석되지 않는다. ' 는 '\'' 로 끊어 넣는다
fn in_sh(d: &str, resume: &str) -> String {
    format!("cd '{}' && {resume}", d.replace('\'', "'\\''"))
}

#[derive(Serialize)]
pub struct ShellCommand {
    /// powershell | cmd | bash
    shell: &'static str,
    command: String,
}

/// Windows: 셸마다 문법이 달라 셸별 이어가기 명령 — PowerShell(기본) · 명령 프롬프트 · Git Bash.
/// 명령 프롬프트는 큰따옴표 안에서도 `%이름%` 을 풀어 버리므로 `%`·`"` 가 든 경로면 빼고, 안전하게 못 만드는 셸은 목록에 없다
pub fn resume_commands_windows(dir: Option<&str>, id: &str, base: &str) -> Vec<ShellCommand> {
    let resume = resume_part(id, base);
    let Some(d) = usable_dir(dir) else {
        return ["powershell", "cmd", "bash"].into_iter().map(|shell| ShellCommand { shell, command: resume.clone() }).collect();
    };
    let mut out = vec![ShellCommand { shell: "powershell", command: in_powershell(d, &resume) }];
    if !d.contains(['%', '"']) {
        out.push(ShellCommand { shell: "cmd", command: format!("cd /d \"{d}\" && {resume}") });
    }
    // Git Bash 는 `C:/…` 를 받는다 — 역슬래시는 bash 에서도 작은따옴표 안이라 글자지만, 슬래시가 어디서나 통한다
    out.push(ShellCommand { shell: "bash", command: in_sh(&d.replace('\\', "/"), &resume) });
    out
}

#[derive(Serialize)]
pub struct ChatPage {
    session: SessionHeader,
    turns: Vec<TurnBubble>,
    has_more: bool,
}

#[tauri::command]
pub fn get_chat(state: State<AppState>, session_id: String, before_seq: Option<i64>, limit: Option<i64>) -> R<ChatPage> {
    let conn = state.conn.lock().map_err(e)?;
    get_chat_on(&conn, &session_id, before_seq, limit)
}

pub fn get_chat_on(conn: &Connection, session_id: &str, before_seq: Option<i64>, limit: Option<i64>) -> R<ChatPage> {
    get_chat_filtered(conn, session_id, before_seq, limit, None)
}

/// 태그로 거른 대화(고른 태그가 붙은 요청만). `filter` 가 없거나 비었으면 전체.
#[tauri::command]
pub fn get_chat_tagged(state: State<AppState>, session_id: String, filter: crate::tags::TagFilter, before_seq: Option<i64>, limit: Option<i64>) -> R<ChatPage> {
    let conn = state.conn.lock().map_err(e)?;
    get_chat_filtered(&conn, &session_id, before_seq, limit, Some(&filter))
}

pub fn get_chat_filtered(conn: &Connection, session_id: &str, before_seq: Option<i64>, limit: Option<i64>, filter: Option<&crate::tags::TagFilter>) -> R<ChatPage> {
    let limit = limit.unwrap_or(80).clamp(1, 500);
    let before = before_seq.unwrap_or(i64::MAX);
    let extra = filter.and_then(|f| f.sql("turn")).map(|w| format!(" AND {w}")).unwrap_or_default();
    let mut st = conn
        .prepare(&format!(
            "SELECT {BUBBLE_COLS} FROM turn WHERE session_id = ?1 AND hidden = 0 AND seq < ?2{extra} ORDER BY seq DESC LIMIT ?3"
        ))
        .map_err(e)?;
    let mut turns: Vec<TurnBubble> = st
        .query_map(params![session_id, before, limit + 1], bubble)
        .map_err(e)?
        .flatten()
        .collect();
    let has_more = turns.len() as i64 > limit;
    turns.truncate(limit as usize);
    turns.reverse();
    fill_tags(conn, &mut turns);
    Ok(ChatPage { session: session_header(conn, session_id)?, turns, has_more })
}

fn fill_tags(conn: &Connection, turns: &mut [TurnBubble]) {
    let ids: Vec<i64> = turns.iter().map(|t| t.id).collect();
    let mut map = crate::tags::tags_of_turns(conn, &ids);
    for t in turns {
        t.tags = map.remove(&t.id).unwrap_or_default();
    }
}

// ── 요청 하나 상세 ──────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct StepRow {
    seq: i64,
    at: Option<String>,
    kind: String,
    name: Option<String>,
    text: Option<String>,
}

#[derive(Serialize)]
pub struct SubRow {
    agent_type: Option<String>,
    description: Option<String>,
    background: bool,
    started_at: Option<String>,
    ended_at: Option<String>,
    duration_ms: Option<i64>,
}

#[derive(Serialize)]
pub struct HookRow {
    event: String,
    at: String,
    detail: Value,
}

#[derive(Serialize)]
pub struct TurnDetail {
    turn: TurnBubble,
    session: SessionHeader,
    steps: Vec<StepRow>,
    tools: Vec<(String, i64)>,
    files: Vec<(String, i64)>,
    subagents: Vec<SubRow>,
    hooks: Vec<HookRow>,
    prev_id: Option<i64>,
    next_id: Option<i64>,
    /// 이어진 요청(같은 세션의 다음 요청) 본문 — 문서의 "이어진 요청" 절
    next_prompt: Option<String>,
    next_at: Option<String>,
}

#[tauri::command]
pub fn get_turn(state: State<AppState>, turn_id: i64) -> R<TurnDetail> {
    let conn = state.conn.lock().map_err(e)?;
    get_turn_on(&conn, turn_id)
}

pub fn get_turn_on(conn: &Connection, turn_id: i64) -> R<TurnDetail> {
    let sid: String = conn
        .query_row("SELECT session_id FROM turn WHERE id = ?1", params![turn_id], |r| r.get(0))
        .map_err(e)?;
    let mut turn = conn
        .query_row(&format!("SELECT {BUBBLE_COLS} FROM turn WHERE id = ?1"), params![turn_id], bubble)
        .map_err(e)?;
    fill_tags(conn, std::slice::from_mut(&mut turn));

    let mut st = conn
        .prepare("SELECT seq, at, kind, name, text FROM turn_step WHERE turn_id = ?1 ORDER BY seq")
        .map_err(e)?;
    let steps = st
        .query_map(params![turn_id], |r| {
            Ok(StepRow { seq: r.get(0)?, at: r.get(1)?, kind: r.get(2)?, name: r.get(3)?, text: r.get(4)? })
        })
        .map_err(e)?
        .flatten()
        .collect();
    let mut st = conn
        .prepare("SELECT tool_name, calls FROM turn_tool WHERE turn_id = ?1 ORDER BY calls DESC, tool_name")
        .map_err(e)?;
    let tools = st.query_map(params![turn_id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?.flatten().collect();
    let mut st = conn
        .prepare("SELECT path, edits FROM turn_file WHERE turn_id = ?1 ORDER BY path")
        .map_err(e)?;
    let files = st.query_map(params![turn_id], |r| Ok((r.get(0)?, r.get(1)?))).map_err(e)?.flatten().collect();
    let mut st = conn
        .prepare(
            "SELECT agent_type, description, background, started_at, ended_at, duration_ms
               FROM turn_subagent WHERE turn_id = ?1 ORDER BY seq",
        )
        .map_err(e)?;
    let subagents = st
        .query_map(params![turn_id], |r| {
            Ok(SubRow {
                agent_type: r.get(0)?,
                description: r.get(1)?,
                background: r.get::<_, i64>(2)? != 0,
                started_at: r.get(3)?,
                ended_at: r.get(4)?,
                duration_ms: r.get(5)?,
            })
        })
        .map_err(e)?
        .flatten()
        .collect();

    // 이 요청이 도는 동안 들어온 훅 이벤트
    let until = turn
        .ended_at
        .clone()
        .or(turn.last_activity_at.clone())
        .map(|t| time::parse(&t).map(|d| (d + chrono::Duration::seconds(90)).to_rfc3339()).unwrap_or(t))
        .unwrap_or_else(|| "9999".into());
    let from = time::parse(&turn.prompt_at)
        .map(|d| (d - chrono::Duration::seconds(5)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_else(|| turn.prompt_at.clone());
    let mut st = conn
        .prepare("SELECT event, at, detail FROM hook_event WHERE session_id = ?1 AND at >= ?2 AND at <= ?3 ORDER BY at LIMIT 200")
        .map_err(e)?;
    let hooks = st
        .query_map(params![sid, from, until], |r| {
            let d: Option<String> = r.get(2)?;
            Ok(HookRow {
                event: r.get(0)?,
                at: r.get(1)?,
                detail: d.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null),
            })
        })
        .map_err(e)?
        .flatten()
        .collect();

    let prev_id = conn
        .query_row(
            "SELECT id FROM turn WHERE session_id = ?1 AND hidden = 0 AND seq < ?2 ORDER BY seq DESC LIMIT 1",
            params![sid, turn.seq],
            |r| r.get(0),
        )
        .optional()
        .map_err(e)?;
    let next: Option<(i64, Option<String>, String)> = conn
        .query_row(
            "SELECT id, prompt_text, prompt_at FROM turn WHERE session_id = ?1 AND hidden = 0 AND seq > ?2 ORDER BY seq LIMIT 1",
            params![sid, turn.seq],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(e)?;
    let next_id = next.as_ref().map(|n| n.0);
    let next_prompt = next.as_ref().and_then(|n| n.1.clone());
    let next_at = next.as_ref().map(|n| n.2.clone());

    Ok(TurnDetail {
        session: session_header(conn, &sid)?,
        turn,
        steps,
        tools,
        files,
        subagents,
        hooks,
        prev_id,
        next_id,
        next_prompt,
        next_at,
    })
}

#[cfg(test)]
mod tests {
    use super::resume_command;

    #[test]
    #[cfg(unix)]
    fn resume_is_quoted() {
        let id = "00000000-1111-4222-8333-444455556666";
        assert_eq!(resume_command(Some("/tmp/a b"), id), format!("cd '/tmp/a b' && claude --resume {id}"));
        let evil = resume_command(Some("/tmp/proj\"; touch X; echo \"$(id)`id`'x"), id);
        assert_eq!(evil, format!("cd '/tmp/proj\"; touch X; echo \"$(id)`id`'\\''x' && claude --resume {id}"));
        // 실제 셸에 넣어 봐도 인자 하나로 남는지
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("printf '%s' {}", &evil[3..evil.find(" && ").unwrap()]))
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "/tmp/proj\"; touch X; echo \"$(id)`id`'x");
        assert_eq!(resume_command(Some("/x"), "abc; rm -rf ~"), "cd '/x' && claude --resume");
        assert_eq!(resume_command(Some("/x\n/y"), id), format!("claude --resume {id}"));
    }

    #[test]
    fn windows_resume_commands_per_shell() {
        use super::resume_commands_windows;
        let id = "00000000-1111-4222-8333-444455556666";
        let get = |v: &[super::ShellCommand], s: &str| v.iter().find(|c| c.shell == s).map(|c| c.command.clone());
        let v = resume_commands_windows(Some(r"C:\work\it's mine"), id, "claude --resume");
        assert_eq!(get(&v, "powershell").unwrap(), format!(r"Set-Location -LiteralPath 'C:\work\it''s mine'; claude --resume {id}"));
        assert_eq!(get(&v, "cmd").unwrap(), format!(r#"cd /d "C:\work\it's mine" && claude --resume {id}"#));
        assert_eq!(get(&v, "bash").unwrap(), format!(r"cd 'C:/work/it'\''s mine' && claude --resume {id}"));
        // 명령 프롬프트는 %이름% 을 따옴표 안에서도 푼다 — 그런 경로면 명령 프롬프트용은 없다
        let v = resume_commands_windows(Some(r"C:\a%PATH%b"), id, "claude --resume");
        assert!(get(&v, "cmd").is_none() && get(&v, "powershell").is_some());
        // 폴더를 모르면 이어가기 명령만
        let v = resume_commands_windows(None, id, "codex resume");
        assert!(v.iter().all(|c| c.command == format!("codex resume {id}")));
    }

    /// PowerShell 토크나이저의 작은따옴표 문자열 읽기(ScanStringLiteral)를 흉내 낸다 — 여는 따옴표부터 읽어 (문자열 값, 닫는 따옴표 뒤)
    fn ps_literal(s: &str) -> Option<(String, &str)> {
        use super::ps_single_quote;
        let mut it = s.char_indices().peekable();
        if !ps_single_quote(it.next()?.1) {
            return None;
        }
        let mut out = String::new();
        while let Some((i, c)) = it.next() {
            if ps_single_quote(c) {
                match it.peek() {
                    // 따옴표 둘 = 뒤의 글자 하나
                    Some(&(_, n)) if ps_single_quote(n) => {
                        it.next();
                        out.push(n);
                        continue;
                    }
                    _ => return Some((out, &s[i + c.len_utf8()..])),
                }
            }
            out.push(c);
        }
        None // 닫히지 않았다
    }

    #[test]
    fn powershell_path_cannot_escape_with_curly_single_quotes() {
        use super::resume_commands_windows;
        let id = "00000000-1111-4222-8333-444455556666";
        // PowerShell 은 ‘ ’ ‚ ‛ 도 작은따옴표로 친다 — ASCII ' 만 겹쳐 쓰면 `’; calc; ’` 가 명령으로 샌다
        for d in [r"C:\w\x’; calc; ’", r"C:\w\x‘; calc; ‘", r"C:\w\‚a‛'b’’c", r"C:\it's", r"C:\plain"] {
            let v = resume_commands_windows(Some(d), id, "claude --resume");
            let ps = v.iter().find(|c| c.shell == "powershell").unwrap().command.clone();
            let rest = ps.strip_prefix("Set-Location -LiteralPath ").unwrap_or_else(|| panic!("{ps}"));
            let (lit, after) = ps_literal(rest).unwrap_or_else(|| panic!("닫히지 않은 문자열: {ps}"));
            assert_eq!(lit, d, "{ps}");
            assert_eq!(after, format!("; claude --resume {id}"), "경로가 명령으로 새지 않는다: {ps}");
        }
        assert_eq!(super::in_powershell("C:\\w\\x’; calc; ’", "claude"), "Set-Location -LiteralPath 'C:\\w\\x’’; calc; ’’'; claude");
    }

    #[test]
    #[cfg(windows)]
    fn powershell_curly_quote_folder_is_a_literal_on_windows() {
        use super::resume_commands_windows;
        let dir = std::env::temp_dir().join(format!("aiinbox-ps ’; Write-Output PWNED; ’ {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("here.txt"), "").unwrap();
        let d = dir.to_string_lossy().into_owned();
        let v = resume_commands_windows(Some(&d), "x", "Test-Path here.txt");
        let ps = v.iter().find(|c| c.shell == "powershell").unwrap();
        let out = std::process::Command::new("powershell").args(["-NoProfile", "-Command", &ps.command]).output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(stdout.trim(), "True", "{}", String::from_utf8_lossy(&out.stderr));
        assert!(!stdout.contains("PWNED"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(windows)]
    fn windows_resume_commands_land_in_the_folder() {
        use super::resume_commands_windows;
        use std::os::windows::process::CommandExt;
        let dir = std::env::temp_dir().join(format!("aiinbox-resume it's {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("here.txt"), "").unwrap();
        let d = dir.to_string_lossy().into_owned();
        // 세션 ID 가 형식에 안 맞으면 base 만 붙는다 — base 자리에 "이 폴더에 왔나" 확인을 넣어 본다
        let v = resume_commands_windows(Some(&d), "x", "Test-Path here.txt");
        let ps = v.iter().find(|c| c.shell == "powershell").unwrap();
        let out = std::process::Command::new("powershell").args(["-NoProfile", "-Command", &ps.command]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "True", "{}", String::from_utf8_lossy(&out.stderr));
        let v = resume_commands_windows(Some(&d), "x", "if exist here.txt echo yes");
        let cmd = v.iter().find(|c| c.shell == "cmd").unwrap();
        let out = std::process::Command::new("cmd").raw_arg(format!("/C {}", cmd.command)).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "yes", "{}", cmd.command);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

// ── 사용자 상태 ──────────────────────────────────────────────────────────────

/// 읽음이 바뀌었다 — 트레이·Dock 배지와 연결된 폰(목록 배지·읽음 표시)에 알린다
fn read_changed(app: &tauri::AppHandle) {
    use tauri::Manager;
    app.state::<std::sync::Arc<relay::Shared>>().bump_changed();
    crate::refresh_badges(app);
}

#[tauri::command]
pub fn mark_read(app: tauri::AppHandle, state: State<AppState>, turn_ids: Vec<i64>) -> R<()> {
    let n = {
        let conn = state.conn.lock().map_err(e)?;
        mark_read_on(&conn, &turn_ids)?
    };
    if n > 0 {
        read_changed(&app);
    }
    Ok(())
}

/// 읽음 표시. 바뀐 줄 수를 돌려준다.
pub fn mark_read_on(conn: &Connection, turn_ids: &[i64]) -> R<usize> {
    let now = time::now_iso();
    let mut n = 0;
    for id in turn_ids {
        n += conn
            .execute(
                &format!("UPDATE turn SET read_at = ?2 WHERE id = ?1 AND read_at IS NULL AND status IN {FINISHED_SQL}"),
                params![id, now],
            )
            .map_err(e)?;
    }
    Ok(n)
}

pub fn mark_session_read_on(conn: &Connection, sid: &str) -> R<usize> {
    mark_all_read_on(conn, Some(sid)).map(|b| b.ids.len())
}

/// 한꺼번에 읽음으로 바꾼 것 — 되돌리기(`restore_unread`)에 그대로 넘긴다
#[derive(Serialize, Debug)]
pub struct ReadBatch {
    pub ids: Vec<i64>,
    /// 이번에 찍은 읽은 시각 — 되돌릴 때 이 시각인 것만 되돌린다(그사이 따로 읽은 것은 건드리지 않게)
    pub at: String,
}

/// 끝난 안 읽은 결과를 모두 읽음으로. `sid` 가 없으면 모든 세션(트레이 메뉴 "모두 읽음"과 같다).
pub fn mark_all_read_on(conn: &Connection, sid: Option<&str>) -> R<ReadBatch> {
    let at = time::now_iso();
    let mut st = conn
        .prepare(&format!(
            "UPDATE turn SET read_at = ?1
             WHERE read_at IS NULL AND status IN {FINISHED_SQL} AND (?2 IS NULL OR session_id = ?2)
             RETURNING id"
        ))
        .map_err(e)?;
    let ids = st
        .query_map(params![at, sid], |r| r.get::<_, i64>(0))
        .map_err(e)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(e)?;
    Ok(ReadBatch { ids, at })
}

/// 방금 한 "모두 읽음"을 되돌린다 — 그 시각으로 읽음이 찍힌 것만. 되돌린 줄 수.
pub fn restore_unread_on(conn: &Connection, ids: &[i64], at: &str) -> R<usize> {
    let mut n = 0;
    for id in ids {
        n += conn.execute("UPDATE turn SET read_at = NULL WHERE id = ?1 AND read_at = ?2", params![id, at]).map_err(e)?;
    }
    Ok(n)
}

#[tauri::command]
pub fn mark_unread(app: tauri::AppHandle, state: State<AppState>, turn_id: i64) -> R<()> {
    {
        let conn = state.conn.lock().map_err(e)?;
        conn.execute("UPDATE turn SET read_at = NULL WHERE id = ?1", params![turn_id]).map_err(e)?;
    }
    read_changed(&app);
    Ok(())
}

/// 세션 하나(`session_id`) 또는 모든 세션(없으면)의 안 읽은 결과를 모두 읽음으로
#[tauri::command]
pub fn mark_session_read(app: tauri::AppHandle, state: State<AppState>, session_id: Option<String>) -> R<ReadBatch> {
    let batch = {
        let conn = state.conn.lock().map_err(e)?;
        mark_all_read_on(&conn, session_id.as_deref())?
    };
    if !batch.ids.is_empty() {
        read_changed(&app);
    }
    Ok(batch)
}

/// "모두 읽음" 되돌리기 — `mark_session_read` 가 돌려준 그대로
#[tauri::command]
pub fn restore_unread(app: tauri::AppHandle, state: State<AppState>, ids: Vec<i64>, at: String) -> R<usize> {
    let n = {
        let conn = state.conn.lock().map_err(e)?;
        let tx = conn.unchecked_transaction().map_err(e)?;
        let n = restore_unread_on(&tx, &ids, &at)?;
        tx.commit().map_err(e)?;
        n
    };
    if n > 0 {
        read_changed(&app);
    }
    Ok(n)
}

// ── 세션 안 요청 목차 ────────────────────────────────────────────────────────

/// 목차 한 줄 — 대화 전체를 받지 않고도 긴 세션의 요청을 훑고 골라 가게
#[derive(Serialize, Debug)]
pub struct OutlineRow {
    pub id: i64,
    pub seq: i64,
    pub prompt_at: String,
    /// 요청 본문 앞부분(첨부 경로 목록을 뗀 것, 최대 `OUTLINE_HEAD` 글자)
    pub head: String,
    pub slash_command: Option<String>,
    pub origin: Option<String>,
    pub peer_name: Option<String>,
    pub status: String,
    pub needs_input: bool,
    pub unread: bool,
    pub summary: Option<String>,
    /// 붙인 이미지 수
    pub atts: usize,
    /// 요청 태그(뗀 것 제외)
    pub tags: Vec<crate::tags::TurnTag>,
}

const OUTLINE_HEAD: usize = 240;

#[tauri::command]
pub fn session_outline(state: State<AppState>, session_id: String) -> R<Vec<OutlineRow>> {
    let conn = state.conn.lock().map_err(e)?;
    session_outline_on(&conn, &session_id)
}

/// 대화(`get_chat_on`)와 같은 요청들을 같은 순서로 — 가린 요청은 뺀다
pub fn session_outline_on(conn: &Connection, sid: &str) -> R<Vec<OutlineRow>> {
    let mut st = conn
        .prepare(&format!(
            "SELECT id, seq, prompt_at, prompt_text, slash_command, origin, peer_name, status, needs_input,
                    read_at IS NULL AND status IN {FINISHED_SQL}, substr(summary, 1, 200)
             FROM turn WHERE session_id = ?1 AND hidden = 0 ORDER BY seq"
        ))
        .map_err(e)?;
    let rows = st
        .query_map(params![sid], |r| {
            let raw: Option<String> = r.get(3)?;
            let (body, ids) = raw.map(|t| crate::attach::split_block(&t)).unwrap_or_default();
            Ok(OutlineRow {
                id: r.get(0)?,
                seq: r.get(1)?,
                prompt_at: r.get(2)?,
                head: body.trim_start().chars().take(OUTLINE_HEAD).collect(),
                slash_command: r.get(4)?,
                origin: r.get(5)?,
                peer_name: r.get(6)?,
                status: r.get(7)?,
                needs_input: r.get::<_, i64>(8)? != 0,
                unread: r.get::<_, i64>(9)? != 0,
                summary: r.get(10)?,
                atts: ids.len(),
                tags: Vec::new(),
            })
        })
        .map_err(e)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(e)?;
    let mut rows = rows;
    let ids: Vec<i64> = rows.iter().map(|r| r.id).collect();
    let mut map = crate::tags::tags_of_turns(conn, &ids);
    for r in &mut rows {
        r.tags = map.remove(&r.id).unwrap_or_default();
    }
    Ok(rows)
}

#[cfg(test)]
mod read_tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        db::migrate(&c).unwrap();
        for sid in ["s1", "s2"] {
            c.execute(
                "INSERT INTO session (id, project_dir, first_at, last_at) VALUES (?1, '/tmp/p', '2026-09-29T00:00:00Z', '2026-09-29T00:00:00Z')",
                params![sid],
            )
            .unwrap();
        }
        // s1: 끝남 셋(하나는 이미 읽음) · 진행 중 하나 · 가린 요청 하나, s2: 끝남 둘
        let rows: [(&str, i64, &str, Option<&str>, i64, &str); 7] = [
            ("s1", 1, "done", None, 0, "첫 요청\n둘째 줄"),
            ("s1", 2, "done", Some("2026-09-29T01:00:00.000Z"), 0, "둘째 요청"),
            ("s1", 3, "interrupted", None, 0, "셋째 요청"),
            ("s1", 4, "running", None, 0, "넷째 요청"),
            ("s1", 5, "done", None, 1, "/clear"),
            ("s2", 1, "done", None, 0, "다른 세션 1"),
            ("s2", 2, "stopped", None, 0, "다른 세션 2"),
        ];
        for (sid, seq, status, read_at, hidden, text) in rows {
            c.execute(
                "INSERT INTO turn (session_id, prompt_uuid, seq, prompt_at, prompt_text, status, read_at, hidden)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![sid, format!("{sid}-{seq}"), seq, format!("2026-09-29T0{seq}:00:00Z"), text, status, read_at, hidden],
            )
            .unwrap();
        }
        c
    }

    fn unread(c: &Connection, sid: Option<&str>) -> i64 {
        c.query_row(
            &format!("SELECT COUNT(*) FROM turn WHERE read_at IS NULL AND status IN {FINISHED_SQL} AND (?1 IS NULL OR session_id = ?1)"),
            params![sid],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn session_read_all_touches_only_that_session_and_finished_turns() {
        let c = mem();
        let b = mark_all_read_on(&c, Some("s1")).unwrap();
        assert_eq!(b.ids.len(), 3, "끝난 안 읽음 셋(가린 요청 포함, 진행 중 제외)");
        assert_eq!(unread(&c, Some("s1")), 0);
        assert_eq!(unread(&c, Some("s2")), 2, "다른 세션은 그대로");
        let running: Option<String> = c.query_row("SELECT read_at FROM turn WHERE session_id = 's1' AND seq = 4", [], |r| r.get(0)).unwrap();
        assert!(running.is_none(), "진행 중인 요청은 읽음으로 찍지 않는다");
        assert_eq!(mark_session_read_on(&c, "s1").unwrap(), 0, "두 번째는 바꿀 게 없다");
    }

    #[test]
    fn global_read_all_then_undo_restores_only_that_batch() {
        let c = mem();
        let b = mark_all_read_on(&c, None).unwrap();
        assert_eq!(b.ids.len(), 5);
        assert_eq!(unread(&c, None), 0);
        // 되돌리기 전에 따로 다시 읽은 것(다른 시각)은 되돌리지 않는다
        c.execute("UPDATE turn SET read_at = '2026-09-29T09:00:00.000Z' WHERE session_id = 's2' AND seq = 1", []).unwrap();
        assert_eq!(restore_unread_on(&c, &b.ids, &b.at).unwrap(), 4);
        assert_eq!(unread(&c, None), 4);
        let kept: Option<String> = c.query_row("SELECT read_at FROM turn WHERE session_id = 's1' AND seq = 2", [], |r| r.get(0)).unwrap();
        assert_eq!(kept.as_deref(), Some("2026-09-29T01:00:00.000Z"), "원래 읽었던 것은 그대로 읽음");
    }

    #[test]
    fn outline_lists_visible_turns_in_order_with_unread_flags() {
        let c = mem();
        let rows = session_outline_on(&c, "s1").unwrap();
        assert_eq!(rows.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![1, 2, 3, 4], "가린 요청은 빠진다");
        assert_eq!(rows[0].head, "첫 요청\n둘째 줄");
        assert_eq!(rows.iter().map(|r| r.unread).collect::<Vec<_>>(), vec![true, false, true, false], "진행 중은 안 읽음이 아니다");
        assert_eq!(rows[3].status, "running");
        let long = "가".repeat(OUTLINE_HEAD + 50);
        c.execute("UPDATE turn SET prompt_text = ?1 WHERE session_id = 's1' AND seq = 1", params![long]).unwrap();
        assert_eq!(session_outline_on(&c, "s1").unwrap()[0].head.chars().count(), OUTLINE_HEAD);
    }
}

#[tauri::command]
pub fn set_starred(state: State<AppState>, turn_id: i64, starred: bool) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    conn.execute("UPDATE turn SET starred = ?2 WHERE id = ?1", params![turn_id, starred as i64]).map_err(e)?;
    Ok(())
}

#[tauri::command]
pub fn set_pinned(state: State<AppState>, session_id: String, pinned: bool) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    conn.execute("UPDATE session SET pinned = ?2 WHERE id = ?1", params![session_id, pinned as i64]).map_err(e)?;
    Ok(())
}

#[tauri::command]
pub fn set_hidden(app: tauri::AppHandle, state: State<AppState>, session_id: String, hidden: bool) -> R<()> {
    {
        let conn = state.conn.lock().map_err(e)?;
        crate::archive::set_archived(&conn, std::slice::from_ref(&session_id), hidden)?;
    }
    crate::sessions_changed(&app, vec![session_id]);
    Ok(())
}

#[derive(Serialize, Clone, Default)]
pub struct Counts {
    pub unread: i64,
    pub attention: i64,
    pub active: i64,
    /// /clear 뒤 이력으로 보관한 세션 수(이력 탭)
    pub kept: i64,
    /// /clear 됐는데 아직 확인·결정하지 않은 세션 수
    pub undecided: i64,
}

pub fn counts_of(conn: &Connection) -> Counts {
    counts_for(conn, false)
}

/// 폰(코노티)이 보는 개수 — 폰 목록엔 이력으로 보관한 세션도 보통 세션처럼 들어 있으므로(`list_sessions_on`)
/// 배지 수도 그 세션을 센다. 0.8.0 과 같은 값이다(옛 폰은 항목 합과 배지를 나란히 그린다).
pub fn counts_on(conn: &Connection) -> Counts {
    counts_for(conn, true)
}

fn counts_for(conn: &Connection, with_kept: bool) -> Counts {
    let keep = if with_kept { "" } else { "AND COALESCE(s.clear_state, '') <> 'keep'" };
    conn.query_row(
        &format!(
            "SELECT
               (SELECT COUNT(*) FROM turn t JOIN session s ON s.id = t.session_id
                 WHERE s.hidden = 0 {keep} AND t.hidden = 0 AND t.read_at IS NULL AND t.status IN {f}),
               (SELECT COUNT(*) FROM turn t JOIN session s ON s.id = t.session_id
                 WHERE s.hidden = 0 {keep} AND t.hidden = 0
                   AND (t.status = 'waiting' OR (t.needs_input = 1 AND t.read_at IS NULL AND t.status = 'done'))),
               (SELECT COUNT(*) FROM turn t JOIN session s ON s.id = t.session_id
                 WHERE s.hidden = 0 {keep} AND t.hidden = 0 AND t.status IN ('running','background','waiting')),
               (SELECT COUNT(*) FROM session s WHERE s.clear_state = 'keep'
                  AND EXISTS (SELECT 1 FROM turn t WHERE t.session_id = s.id AND t.hidden = 0)),
               (SELECT COUNT(*) FROM session s WHERE s.cleared_at IS NOT NULL AND s.clear_asked = 0 AND s.hidden = 0
                  AND EXISTS (SELECT 1 FROM turn t WHERE t.session_id = s.id AND t.hidden = 0))",
            f = FINISHED_SQL
        ),
        [],
        |r| Ok(Counts { unread: r.get(0)?, attention: r.get(1)?, active: r.get(2)?, kept: r.get(3)?, undecided: r.get(4)? }),
    )
    .unwrap_or_default()
}

#[tauri::command]
pub fn get_counts(state: State<AppState>) -> R<Counts> {
    let conn = state.conn.lock().map_err(e)?;
    Ok(counts_of(&conn))
}

// ── 설정·훅·앱 정보 ──────────────────────────────────────────────────────────

#[tauri::command]
pub fn hook_status() -> R<install::HookStatus> {
    install::status()
}

#[tauri::command]
pub fn install_hooks() -> R<install::HookStatus> {
    let st = install::install()?;
    // 쓴 직후의 변경은 새 훅을 부르지 않는다 — 잠시 뒤 수정 시각만 한 번 더 갱신해 실행 중인 세션들이 대기 훅을 띄우게
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(2));
        install::rearm();
    });
    Ok(st)
}

#[tauri::command]
pub fn uninstall_hooks() -> R<install::HookStatus> {
    install::uninstall()
}

/// 화면 한쪽에 늘 보이는 "버전 · 빌드 시각" 과 정보 창 — 문제 신고에 쓰는 값만(경로는 사용자 화면용)
#[derive(Serialize)]
pub struct About {
    /// tauri.conf.json 의 version (Cargo.toml·package.json 과 같아야 한다 — `about_versions_agree` 시험)
    pub version: String,
    /// 컴파일 시각 "YYYY-MM-DD HH:mm" — UTC(`build.rs`). 화면은 사용자 시간대로 바꿔 보여 준다
    pub build_time: String,
    /// 열려 있는 DB 의 스키마 버전
    pub schema_version: i64,
    pub data_dir: String,
}

pub const BUILD_TIME: &str = env!("AI_INBOX_BUILD_AT");

#[tauri::command]
pub fn about(app: tauri::AppHandle, state: State<AppState>) -> R<About> {
    let schema_version = {
        let conn = state.conn.lock().map_err(e)?;
        db::get_meta(&conn, "schema_version").and_then(|v| v.parse().ok()).unwrap_or(db::SCHEMA_VERSION)
    };
    Ok(About {
        version: app.package_info().version.to_string(),
        build_time: BUILD_TIME.to_string(),
        schema_version,
        data_dir: paths::data_dir().to_string_lossy().into_owned(),
    })
}

#[cfg(test)]
mod about_tests {
    /// 버전의 원천은 tauri.conf.json — Cargo.toml·package.json 이 어긋나면 화면·업데이트 판정이 갈린다
    #[test]
    fn about_versions_agree() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let pkg: serde_json::Value = serde_json::from_str(include_str!("../../package.json")).unwrap();
        let v = conf["version"].as_str().unwrap();
        assert_eq!(v, env!("CARGO_PKG_VERSION"), "tauri.conf.json ≠ Cargo.toml");
        assert_eq!(v, pkg["version"].as_str().unwrap(), "tauri.conf.json ≠ package.json");
    }

    #[test]
    fn build_time_shape() {
        let t = super::BUILD_TIME;
        assert_eq!(t.len(), 16, "{t}");
        assert!(t.as_bytes()[4] == b'-' && t.as_bytes()[10] == b' ' && t.as_bytes()[13] == b':', "{t}");
    }
}

#[derive(Serialize)]
pub struct AppInfo {
    version: String,
    data_dir: String,
    db_path: String,
    db_bytes: u64,
    projects_dir: String,
    sessions: i64,
    turns: i64,
    installed_at: Option<String>,
    hook_last_at: Option<String>,
    backfill_days: i64,
    notify: bool,
    notify_body: bool,
    notify_min_sec: i64,
}

#[tauri::command]
pub fn app_info(app: tauri::AppHandle, state: State<AppState>) -> R<AppInfo> {
    let conn = state.conn.lock().map_err(e)?;
    let count = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(0);
    let db_path = paths::db_path();
    let db_bytes = ["", "-wal"]
        .iter()
        .map(|s| std::fs::metadata(format!("{}{}", db_path.display(), s)).map(|m| m.len()).unwrap_or(0))
        .sum();
    Ok(AppInfo {
        version: app.package_info().version.to_string(),
        data_dir: paths::data_dir().to_string_lossy().into_owned(),
        db_path: db_path.to_string_lossy().into_owned(),
        db_bytes,
        projects_dir: paths::projects_dir().to_string_lossy().into_owned(),
        sessions: count("SELECT COUNT(*) FROM session"),
        turns: count("SELECT COUNT(*) FROM turn WHERE hidden = 0"),
        installed_at: db::get_meta(&conn, "installed_at"),
        hook_last_at: db::get_meta(&conn, "hook_last_at"),
        backfill_days: db::setting_i64(&conn, "backfill_days", 7),
        notify: db::setting_i64(&conn, "notify", 1) != 0,
        notify_body: db::setting_i64(&conn, "notify_body", 1) != 0,
        notify_min_sec: db::setting_i64(&conn, "notify_min_sec", 0),
    })
}

#[tauri::command]
pub fn set_setting(state: State<AppState>, key: String, value: i64) -> R<()> {
    let ok = match key.as_str() {
        "backfill_days" => (1..=365).contains(&value),
        "notify" | "notify_body" => value == 0 || value == 1,
        "notify_min_sec" => (0..=86_400).contains(&value),
        "codex_enabled" => value == 0 || value == 1,
        // /clear 된 대화의 기본 처리: 0 매번 묻기(정할 때까지 자동 삭제 없음) · 1 삭제 예약(기본) · 2 이력으로 보관
        "clear_default" => (0..=2).contains(&value),
        _ => return Err(format!("알 수 없는 설정: {key}")),
    };
    if !ok {
        return Err(format!("허용 범위를 벗어난 값: {key}={value}"));
    }
    let conn = state.conn.lock().map_err(e)?;
    db::set_meta(&conn, &format!("setting.{key}"), &value.to_string()).map_err(e)
}

#[derive(Serialize)]
pub struct CodexStatus {
    /// Codex 기록도 모으나(설정)
    enabled: bool,
    /// codex 실행 파일(없으면 새 작업·꺼진 세션 이어서 실행을 못 한다 — 모으기는 된다)
    bin: Option<String>,
    version: Option<String>,
    sessions_dir: String,
    /// 기록 폴더가 있나
    found: bool,
    sessions: i64,
    /// 지금 열려 있는 Codex 세션 수(macOS·Linux — Windows 는 null)
    live: Option<usize>,
}

/// 설정 화면의 Codex 절. 실행 파일 찾기·버전 확인에 수백 ms 걸릴 수 있어 DB 잠금 밖에서
#[tauri::command]
pub async fn codex_status(app: tauri::AppHandle) -> R<CodexStatus> {
    use tauri::Manager;
    let (enabled, sessions) = {
        let state = app.state::<AppState>();
        let conn = state.conn.lock().map_err(e)?;
        (
            db::setting_i64(&conn, "codex_enabled", 1) != 0,
            conn.query_row("SELECT COUNT(*) FROM session WHERE agent = 'codex'", [], |r| r.get::<_, i64>(0)).unwrap_or(0),
        )
    };
    tauri::async_runtime::spawn_blocking(move || {
        let dir = paths::codex_sessions_dir();
        let bin = crate::codex::find_codex();
        Ok(CodexStatus {
            enabled,
            version: bin.as_ref().and_then(|_| crate::codex::version()),
            bin: bin.map(|p| p.to_string_lossy().into_owned()),
            found: dir.is_dir(),
            sessions_dir: dir.to_string_lossy().into_owned(),
            sessions,
            live: (!cfg!(windows)).then(|| crate::codex::live_threads().len()),
        })
    })
    .await
    .map_err(e)?
}

/// 수집을 처음부터 다시 (읽음·별표는 보존)
#[tauri::command]
pub fn rescan(state: State<AppState>) -> R<()> {
    state.rescan.store(true, Ordering::SeqCst);
    Ok(())
}

/// 요청 문서를 .md 로 저장. 경로는 **사용자가 저장 대화상자에서 고른 곳뿐**이다
/// (화면 쪽에 임의 경로 쓰기 권한을 주지 않기 위해 대화상자도 여기서 띄운다).
#[tauri::command]
pub async fn save_markdown(app: tauri::AppHandle, default_name: String, content: String) -> R<bool> {
    use tauri_plugin_dialog::DialogExt;
    if content.len() > 32 * 1024 * 1024 {
        return Err("문서가 너무 큽니다".into());
    }
    let mut name: String = default_name
        .chars()
        .map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { ' ' } else { c })
        .collect::<String>()
        .trim()
        .chars()
        .take(120)
        .collect();
    if name.is_empty() {
        name = "요청.md".into();
    }
    if !name.to_lowercase().ends_with(".md") {
        name.push_str(".md");
    }
    let picked = app
        .dialog()
        .file()
        .set_file_name(&name)
        .add_filter("Markdown", &["md"])
        .blocking_save_file();
    let Some(picked) = picked else { return Ok(false) };
    let mut path = picked.into_path().map_err(e)?;
    if path.extension().and_then(|x| x.to_str()).map(|x| !x.eq_ignore_ascii_case("md")).unwrap_or(true) {
        // 확장자를 붙여 이름이 바뀌었으면, 대화상자가 확인하지 않은 기존 파일을 덮지 않는다
        let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
        name.push(".md");
        path.set_file_name(name);
        if path.exists() {
            return Err(format!("같은 이름의 파일이 이미 있습니다: {}", path.display()));
        }
    }
    std::fs::write(&path, content).map_err(e)?;
    Ok(true)
}

/// 데이터 폴더를 Finder/탐색기에서 연다 (이 앱의 DB 위치만)
#[tauri::command]
pub fn reveal_data_dir(app: tauri::AppHandle) -> R<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener().reveal_item_in_dir(paths::db_path()).map_err(e)
}

// ── 폰 연결(선택 기능) ────────────────────────────────────────────────────────

#[derive(Serialize)]
pub struct ReplyRow {
    reply_id: String,
    session_id: Option<String>,
    session_name: Option<String>,
    card_title: Option<String>,
    kind: Option<String>,
    text: Option<String>,
    received_at: String,
    state: String,
    note: Option<String>,
    device: Option<String>,
    /// 붙은 이미지 — 확인 막대에서 무엇을 전달하는지 보이게
    atts: Vec<String>,
}

fn reply_rows(conn: &Connection, where_sql: &str, p: &[&dyn rusqlite::ToSql]) -> R<Vec<ReplyRow>> {
    let sql = format!(
        "SELECT r.reply_id, r.session_id, COALESCE(s.live_name, s.title, s.agent_name), t.prompt_text, r.kind, r.text,
                r.received_at, r.state, r.note, d.name
           FROM conoti_reply r
      LEFT JOIN session s ON s.id = r.session_id
      LEFT JOIN turn t ON t.id = r.turn_id
      LEFT JOIN relay_device d ON d.pid = r.device
          WHERE {where_sql}
          ORDER BY r.received_at DESC LIMIT 30"
    );
    let mut st = conn.prepare(&sql).map_err(e)?;
    let rows = st
        .query_map(p, |r| {
            let title: Option<String> = r.get(3)?;
            Ok(ReplyRow {
                reply_id: r.get(0)?,
                session_id: r.get(1)?,
                session_name: r.get(2)?,
                card_title: title.map(|t| crate::text::clip(crate::text::first_line(&t), 60)),
                kind: r.get(4)?,
                text: r.get(5)?,
                received_at: r.get(6)?,
                state: r.get(7)?,
                note: r.get(8)?,
                device: r.get(9)?,
                atts: vec![],
            })
        })
        .map_err(e)?;
    let mut out: Vec<ReplyRow> = rows.flatten().collect();
    for row in &mut out {
        row.atts = crate::attach::for_reply(conn, &row.reply_id);
    }
    Ok(out)
}

#[derive(Serialize)]
pub struct OfferView {
    uri: String,
    svg: String,
    expires_in: u64,
}

#[derive(Serialize)]
pub struct PendingView {
    name: String,
    sas: String,
    waited_sec: u64,
}

#[derive(Serialize)]
pub struct RelayStatus {
    enabled: bool,
    env: String,
    host: String,
    status: relay::Status,
    devices: Vec<relay::Device>,
    offer: Option<OfferView>,
    pending: Option<PendingView>,
    push: bool,
    paused: bool,
    confirm: bool,
    bg_resume: bool,
    pending_confirm: i64,
    replies: Vec<ReplyRow>,
    mcp_add_command: String,
    start_command: String,
}

type Relay<'a> = State<'a, std::sync::Arc<relay::Shared>>;

#[tauri::command]
pub fn relay_status(state: State<AppState>, shared: Relay) -> R<RelayStatus> {
    let conn = state.conn.lock().map_err(e)?;
    relay_status_on(&conn, &shared)
}

pub fn relay_status_on(conn: &Connection, shared: &relay::Shared) -> R<RelayStatus> {
    let conn = conn;
    let flag = |k: &str| db::get_meta(conn, k).as_deref() == Some("1");
    let env = relay::env_of(conn);
    let (mcp_add_command, start_command) = crate::deliver::setup_commands();
    let offer = {
        let o = shared.offer.lock().map_err(e)?;
        o.as_ref()
            .filter(|o| !o.used && o.expires > std::time::Instant::now())
            .map(|o| OfferView {
                uri: o.uri.clone(),
                svg: relay::qr_svg(&o.uri).unwrap_or_default(),
                expires_in: o.expires.saturating_duration_since(std::time::Instant::now()).as_secs(),
            })
    };
    let pending = shared
        .pending
        .lock()
        .map_err(e)?
        .as_ref()
        .filter(|p| p.decision.is_none())
        .map(|p| PendingView { name: p.name.clone(), sas: p.sas.clone(), waited_sec: p.requested.elapsed().as_secs() });
    Ok(RelayStatus {
        enabled: relay::enabled(&conn),
        host: relay::host(&env).to_string(),
        env,
        status: shared.status.lock().map_err(e)?.clone(),
        devices: relay::devices(&conn),
        offer,
        pending,
        push: relay::push_enabled(&conn),
        paused: flag("conoti.paused"),
        confirm: flag("conoti.confirm"),
        bg_resume: flag("conoti.bg_resume"),
        pending_confirm: conn.query_row("SELECT COUNT(*) FROM conoti_reply WHERE state = 'confirm'", [], |r| r.get(0)).unwrap_or(0),
        replies: reply_rows(conn, "r.device IS NOT NULL", &[])?,
        mcp_add_command,
        start_command,
    })
}

/// 폰 연결 설정. enabled·env 는 중계 연결을 다시 맺는다.
#[tauri::command]
pub fn relay_set(state: State<AppState>, shared: Relay, key: String, value: String) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    let (meta_key, ok) = match key.as_str() {
        "enabled" => ("relay.enabled", value == "0" || value == "1"),
        "env" => ("relay.env", value == "prod" || value == "dev"),
        "push" => ("relay.push", value == "0" || value == "1"),
        "paused" => ("conoti.paused", value == "0" || value == "1"),
        "confirm" => ("conoti.confirm", value == "0" || value == "1"),
        "bg_resume" => ("conoti.bg_resume", value == "0" || value == "1"),
        _ => return Err(format!("알 수 없는 설정: {key}")),
    };
    if !ok {
        return Err(format!("허용 범위를 벗어난 값: {key}={value}"));
    }
    db::set_meta(&conn, meta_key, &value).map_err(e)?;
    if key == "enabled" || key == "env" {
        *shared.offer.lock().map_err(e)? = None;
        shared.kick.store(true, Ordering::SeqCst);
    }
    shared.bump_changed();
    Ok(())
}

/// 페어링 QR 을 만든다(5분 · 한 번만). 폰 연결이 꺼져 있으면 켠다.
#[tauri::command]
pub async fn relay_new_offer(app: tauri::AppHandle) -> R<OfferView> {
    use tauri::Manager;
    tauri::async_runtime::spawn_blocking(move || {
        let shared = app.state::<std::sync::Arc<relay::Shared>>();
        let state = app.state::<AppState>();
        let env = {
            let conn = state.conn.lock().map_err(e)?;
            if !relay::enabled(&conn) {
                db::set_meta(&conn, "relay.enabled", "1").map_err(e)?;
                shared.kick.store(true, Ordering::SeqCst);
            }
            relay::env_of(&conn)
        };
        let id = shared.identity()?; // 처음이면 새로 만든다
        let uri = relay::new_offer(&shared, &env, &id);
        Ok(OfferView { svg: relay::qr_svg(&uri)?, uri, expires_in: relay::OFFER_TTL.as_secs() })
    })
    .await
    .map_err(e)?
}

#[tauri::command]
pub fn relay_cancel_offer(shared: Relay) -> R<()> {
    *shared.offer.lock().map_err(e)? = None;
    Ok(())
}

/// 새 기기 연결 요청에 답한다.
#[tauri::command]
pub fn relay_decide_pair(shared: Relay, approve: bool, can_reply: bool, sas: String) -> R<()> {
    decide_pair_on(&shared, approve, can_reply, &sas)
}

/// 허용·거절은 **화면에 보여 준 그 요청**(확인 코드)에만 적용한다. 창이 떠 있는 사이 요청이 바뀌었으면
/// 사용자가 코드를 본 적 없는 기기를 허용하게 되므로 거부한다(2026-09-24 보안 점검).
pub fn decide_pair_on(shared: &relay::Shared, approve: bool, can_reply: bool, sas: &str) -> R<()> {
    let mut p = shared.pending.lock().map_err(e)?;
    match p.as_mut() {
        Some(pd) if pd.decision.is_none() && pd.sas == sas => {
            pd.decision = Some((approve, can_reply));
            Ok(())
        }
        Some(pd) if pd.decision.is_none() => Err("연결 요청이 바뀌었습니다 — 새 확인 코드를 보고 다시 결정하세요".into()),
        _ => Err("기다리는 연결 요청이 없습니다".into()),
    }
}

#[tauri::command]
pub async fn relay_remove_device(app: tauri::AppHandle, pid: String) -> R<()> {
    use tauri::Manager;
    tauri::async_runtime::spawn_blocking(move || {
        let shared = app.state::<std::sync::Arc<relay::Shared>>();
        let conn = db::open(&paths::db_path()).map_err(e)?;
        relay::remove_device(&conn, &shared, &pid)
    })
    .await
    .map_err(e)?
}

/// 이 기기가 폰에서 세션을 관리(보관함 보기·보관·고정·기록에서 지우기)할 수 있는지
#[tauri::command]
pub fn relay_set_device_manage(state: State<AppState>, shared: Relay, pid: String, can_manage: bool) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    let n = conn
        .execute("UPDATE relay_device SET can_manage = ?2 WHERE pid = ?1", params![pid, can_manage as i64])
        .map_err(e)?;
    if n == 0 {
        return Err("그런 기기가 없습니다".into());
    }
    shared.bump_changed();
    Ok(())
}

/// 이 기기가 폰에서 예약 전송을 만들고·고치고·취소하고·처리할 수 있는지(기본 끔)
#[tauri::command]
pub fn relay_set_device_schedule(state: State<AppState>, shared: Relay, pid: String, can_schedule: bool) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    let n = conn
        .execute("UPDATE relay_device SET can_schedule = ?2 WHERE pid = ?1", params![pid, can_schedule as i64])
        .map_err(e)?;
    if n == 0 {
        return Err("그런 기기가 없습니다".into());
    }
    shared.bump_changed();
    Ok(())
}

#[tauri::command]
pub fn relay_set_device_reply(state: State<AppState>, shared: Relay, pid: String, can_reply: bool) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    let n = conn
        .execute("UPDATE relay_device SET can_reply = ?2 WHERE pid = ?1", params![pid, can_reply as i64])
        .map_err(e)?;
    if n == 0 {
        return Err("그런 기기가 없습니다".into());
    }
    shared.bump_changed();
    Ok(())
}

/// 세션별 폰 답: 0 받음(기본) · 1 막음
#[tauri::command]
pub fn conoti_set_session_mode(state: State<AppState>, shared: Relay, session_id: String, mode: i64) -> R<()> {
    if !(0..=1).contains(&mode) {
        return Err("mode 는 0·1".into());
    }
    let conn = state.conn.lock().map_err(e)?;
    conn.execute(
        "INSERT INTO conoti_session (session_id, mode, enabled_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(session_id) DO UPDATE SET mode = excluded.mode",
        params![session_id, mode, time::now_iso()],
    )
    .map_err(e)?;
    shared.bump_changed();
    Ok(())
}

/// 데스크톱 확인 모드에서 폰 답을 전달하거나 거절한다.
#[tauri::command]
pub fn conoti_decide(state: State<AppState>, shared: Relay, reply_id: String, approve: bool) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    crate::conoti::decide(&conn, &reply_id, approve)?;
    shared.bump_changed();
    Ok(())
}

#[tauri::command]
pub fn conoti_pending(state: State<AppState>, session_id: String) -> R<Vec<ReplyRow>> {
    let conn = state.conn.lock().map_err(e)?;
    reply_rows(&conn, "r.state = 'confirm' AND r.session_id = ?1", &[&session_id])
}

// ── 입력창: 세션에 말 보내기 · 새 작업 ─────────────────────────────────────────

/// 입력창에서 보낸 말. 기록만 하고 실제 전달은 폰 답과 같은 전달 스레드가 한다(채널 → 이어서 실행, 일하는 중이면 대기).
#[tauri::command]
pub fn send_message(
    state: State<AppState>,
    session_id: String,
    text: String,
    atts: Option<Vec<String>>,
    quote_turn: Option<i64>,
    quote_part: Option<String>,
) -> R<Value> {
    let conn = state.conn.lock().map_err(e)?;
    let quote = quote_turn.zip(quote_part.as_deref());
    let r = crate::conoti::accept_desktop(&conn, &session_id, &text, &atts.unwrap_or_default(), quote)?;
    state.kick.store(true, Ordering::SeqCst);
    Ok(r)
}

/// 아직 요청으로 잡히지 않은 보낸 말(데스크톱·폰)
#[tauri::command]
pub fn session_outbox(state: State<AppState>, session_id: String) -> R<Vec<Value>> {
    let conn = state.conn.lock().map_err(e)?;
    Ok(crate::conoti::outbox_for(&conn, &session_id))
}

#[tauri::command]
pub fn cancel_message(state: State<AppState>, reply_id: String) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    crate::conoti::cancel_desktop(&conn, &reply_id)
}

/// 새 작업 폴더 후보: 최근 세션의 작업 폴더(지금 있는 것만)
#[tauri::command]
pub fn recent_dirs(state: State<AppState>) -> R<Vec<String>> {
    let conn = state.conn.lock().map_err(e)?;
    let mut st = conn
        .prepare(
            "SELECT project_dir FROM session WHERE project_dir IS NOT NULL AND project_dir <> '' AND hidden = 0
              GROUP BY project_dir ORDER BY MAX(COALESCE(last_at, first_at, '')) DESC LIMIT 40",
        )
        .map_err(e)?;
    let dirs: Vec<String> = st.query_map([], |r| r.get::<_, String>(0)).map_err(e)?.flatten().collect();
    Ok(dirs.into_iter().filter(|d| !d.chars().any(char::is_control) && std::path::Path::new(d).is_dir()).take(15).collect())
}

#[tauri::command]
pub async fn pick_folder(app: tauri::AppHandle) -> R<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let picked = app.dialog().file().blocking_pick_folder();
    Ok(picked.and_then(|p| p.into_path().ok()).map(|p| p.to_string_lossy().into_owned()))
}

#[derive(Serialize)]
pub struct Started {
    short_id: String,
    session_id: Option<String>,
}

/// 새 작업: 고른 폴더에서 백그라운드 세션을 띄운다.
/// Claude Code(`agent = "claude"`, 기본) = 사용자의 기본 권한 설정 그대로 ·
/// Codex(`"codex"`) = `codex exec` — 설정에 샌드박스가 없으면 폴더 안 쓰기 허용(workspace-write), 승인은 묻지 않는다(never).
#[tauri::command]
pub async fn start_task(app: tauri::AppHandle, cwd: String, text: String, atts: Option<Vec<String>>, agent: Option<String>) -> R<Started> {
    use tauri::Manager;
    let atts = atts.unwrap_or_default();
    let codex = match agent.as_deref() {
        None | Some("claude") => false,
        Some("codex") => true,
        Some(_) => return Err("알 수 없는 에이전트".into()),
    };
    crate::conoti::check_body(&text, !atts.is_empty())?;
    let dir = std::path::PathBuf::from(cwd.trim());
    if !dir.is_absolute() || !dir.is_dir() {
        return Err("폴더가 없습니다".into());
    }
    let block = {
        let state = app.state::<AppState>();
        let conn = state.conn.lock().map_err(e)?;
        crate::attach::check_desktop_ids(&conn, &atts)?;
        crate::attach::block(&conn, &atts)
    };
    let prompt = crate::attach::append_block(&crate::conoti::wrap_desk(&text), block);
    let name = crate::deliver::task_name(if text.trim().is_empty() { "이미지 보기" } else { &text });
    if codex {
        let images = crate::attach::block_paths(&prompt);
        let dir2 = dir.clone();
        let sid = tauri::async_runtime::spawn_blocking(move || crate::codex::start(&dir2, &prompt, &images)).await.map_err(e)??;
        crate::codex::remember(&sid);
        let state = app.state::<AppState>();
        {
            let conn = state.conn.lock().map_err(e)?;
            // 기록 파일을 읽기 전에도 목록에 이름과 함께 보이게(Codex exec 는 이름을 받지 않는다)
            conn.execute(
                "INSERT INTO session (id, project_dir, title, agent, first_at, last_at) VALUES (?1, ?2, ?3, 'codex', ?4, ?4)
                 ON CONFLICT(id) DO UPDATE SET title = COALESCE(session.title, excluded.title), agent = 'codex'",
                params![sid, dir.to_string_lossy(), name, crate::time::now_iso()],
            )
            .map_err(e)?;
            crate::conoti::record_started(&conn, Some(&sid), &text, "새 Codex 작업을 백그라운드로 시작", &atts)?;
        }
        state.kick.store(true, Ordering::SeqCst);
        return Ok(Started { short_id: sid.chars().take(8).collect(), session_id: Some(sid) });
    }
    let (short, sid) = tauri::async_runtime::spawn_blocking(move || crate::deliver::start_session(&dir, &name, &prompt))
        .await
        .map_err(e)??;
    let state = app.state::<AppState>();
    {
        let conn = state.conn.lock().map_err(e)?;
        crate::conoti::record_started(&conn, sid.as_deref(), &text, "새 작업을 백그라운드로 시작", &atts)?;
    }
    state.kick.store(true, Ordering::SeqCst);
    Ok(Started { short_id: short, session_id: sid })
}

// ── 이미지 첨부 ──────────────────────────────────────────────────────────────

/// 입력창에 붙인 이미지 한 장(원본 바이트를 그대로 받는다 — base64 로 부풀리지 않게). 헤더 `x-name` = 파일 이름(URI 인코딩)
/// 풀기·줄이기·형식 바꾸기(최대 수 초)는 DB 잠금 밖에서 — 그동안 화면·수집이 멈추지 않게.
#[tauri::command]
pub async fn attachment_put(app: tauri::AppHandle, request: tauri::ipc::Request<'_>) -> R<crate::attach::Meta> {
    use tauri::Manager;
    let tauri::ipc::InvokeBody::Raw(bytes) = request.body() else {
        return Err("이미지 바이트가 아닙니다".into());
    };
    let bytes = bytes.clone();
    let name = request
        .headers()
        .get("x-name")
        .and_then(|v| v.to_str().ok())
        .map(percent_decode);
    let prepared = tauri::async_runtime::spawn_blocking(move || crate::attach::prepare(bytes, name.as_deref(), "desktop"))
        .await
        .map_err(e)??;
    let state = app.state::<AppState>();
    let conn = state.conn.lock().map_err(e)?;
    crate::attach::commit(&conn, prepared)
}

/// `encodeURIComponent` 로 보낸 파일 이름 되돌리기(깨진 조각은 글자 그대로)
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |x: u8| (x as char).to_digit(16);
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 화면에 그릴 바이트 — `thumb` · `view` · `orig`
#[tauri::command]
pub fn attachment_get(state: State<AppState>, id: String, size: String) -> R<tauri::ipc::Response> {
    let conn = state.conn.lock().map_err(e)?;
    let (bytes, _mime) = crate::attach::read(&conn, &id, &size)?;
    Ok(tauri::ipc::Response::new(bytes))
}

#[tauri::command]
pub fn attachment_meta(state: State<AppState>, ids: Vec<String>) -> R<Vec<crate::attach::Meta>> {
    let conn = state.conn.lock().map_err(e)?;
    Ok(crate::attach::metas(&conn, &ids.into_iter().take(200).collect::<Vec<_>>()))
}

/// Finder(탐색기)에서 파일 위치 열기
#[tauri::command]
pub fn attachment_reveal(state: State<AppState>, id: String) -> R<()> {
    let path = {
        let conn = state.conn.lock().map_err(e)?;
        crate::attach::path_of(&conn, &id).ok_or("이미지 파일이 없습니다")?
    };
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("/usr/bin/open").arg("-R").arg(&path).spawn();
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let r = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(&path)).spawn();
    r.map(|_| ()).map_err(e)
}

// ── 기록 보관함 ──────────────────────────────────────────────────────────────

#[tauri::command]
pub fn archive_turns(state: State<AppState>, query: String, before: Option<String>, limit: Option<i64>, tags: Option<crate::tags::TagFilter>) -> R<crate::archive::Page<crate::archive::TurnHit>> {
    let conn = state.conn.lock().map_err(e)?;
    crate::archive::search_turns_tagged(&conn, &query, before.as_deref(), limit.unwrap_or(40), tags.as_ref())
}

#[tauri::command]
pub fn archive_messages(
    state: State<AppState>,
    query: String,
    device: String,
    images_only: bool,
    before: Option<String>,
    limit: Option<i64>,
) -> R<crate::archive::Page<crate::archive::Message>> {
    if !matches!(device.as_str(), "all" | "desktop" | "phone") {
        return Err("device".into());
    }
    let conn = state.conn.lock().map_err(e)?;
    crate::archive::messages(&conn, &query, &device, images_only, before.as_deref(), limit.unwrap_or(60))
}

#[tauri::command]
pub fn archive_images(state: State<AppState>, query: String, before: Option<String>, limit: Option<i64>) -> R<crate::archive::Page<crate::archive::Image>> {
    let conn = state.conn.lock().map_err(e)?;
    crate::archive::images(&conn, &query, before.as_deref(), limit.unwrap_or(120))
}

#[tauri::command]
pub fn archive_stats(state: State<AppState>) -> R<crate::archive::Stats> {
    let conn = state.conn.lock().map_err(e)?;
    Ok(crate::archive::stats(&conn))
}

#[tauri::command]
pub fn archive_delete_messages(state: State<AppState>, rids: Vec<String>) -> R<usize> {
    let conn = state.conn.lock().map_err(e)?;
    let n = crate::archive::delete_messages(&conn, &rids)?;
    state.kick.store(true, Ordering::SeqCst);
    Ok(n)
}

#[tauri::command]
pub fn archive_delete_images(state: State<AppState>, ids: Vec<String>) -> R<usize> {
    let conn = state.conn.lock().map_err(e)?;
    crate::archive::delete_images(&conn, &ids)
}

#[tauri::command]
pub fn archive_sessions(
    state: State<AppState>,
    query: String,
    scope: String,
    short: bool,
    idle_days: Option<i64>,
    offset: Option<i64>,
) -> R<crate::archive::Page<crate::archive::SessionRow>> {
    if !matches!(scope.as_str(), "all" | "visible" | "archived") {
        return Err("scope".into());
    }
    let conn = state.conn.lock().map_err(e)?;
    crate::archive::sessions(&conn, &query, &scope, short, idle_days, offset.unwrap_or(0), 200)
}

#[tauri::command]
pub fn archive_set_archived(app: tauri::AppHandle, state: State<AppState>, ids: Vec<String>, archived: bool) -> R<usize> {
    let n = {
        let conn = state.conn.lock().map_err(e)?;
        crate::archive::set_archived(&conn, &ids, archived)?
    };
    crate::sessions_changed(&app, ids);
    Ok(n)
}

/// 정리 제안대로 한 번에 보관 — `kind`: short(요청 1개 이하) | idle(30일 넘게 조용함). 보관한 세션 수.
#[tauri::command]
pub fn archive_tidy(app: tauri::AppHandle, state: State<AppState>, kind: String) -> R<usize> {
    let (ids, n) = {
        let conn = state.conn.lock().map_err(e)?;
        let ids = match kind.as_str() {
            "short" => crate::archive::tidy_candidates(&conn, true, None)?,
            "idle" => crate::archive::tidy_candidates(&conn, false, Some(crate::archive::TIDY_IDLE_DAYS))?,
            _ => return Err("kind".into()),
        };
        let n = crate::archive::set_archived(&conn, &ids, true)?;
        (ids, n)
    };
    crate::sessions_changed(&app, ids);
    Ok(n)
}

#[tauri::command]
pub fn archive_delete_sessions(app: tauri::AppHandle, state: State<AppState>, ids: Vec<String>) -> R<crate::archive::Deleted> {
    let out = {
        let conn = state.conn.lock().map_err(e)?;
        crate::archive::delete_sessions(&conn, &ids)?
    };
    crate::sessions_changed(&app, out.deleted.clone());
    Ok(out)
}

#[cfg(test)]
mod pair_decision_tests {
    #[test]
    fn decision_applies_only_to_the_shown_request() {
        let shared = crate::relay::Shared::new();
        assert!(super::decide_pair_on(&shared, true, true, "123 456").is_err(), "요청 없음");
        *shared.pending.lock().unwrap() = Some(crate::relay::Pending {
            name: "폰".into(),
            sas: "111 222".into(),
            ch: 1,
            requested: std::time::Instant::now(),
            decision: None,
        });
        // 창에 떠 있던 코드와 다르면(그사이 요청이 바뀜) 거부
        assert!(super::decide_pair_on(&shared, true, true, "123 456").is_err());
        assert!(shared.pending.lock().unwrap().as_ref().unwrap().decision.is_none());
        super::decide_pair_on(&shared, true, false, "111 222").unwrap();
        assert_eq!(shared.pending.lock().unwrap().as_ref().unwrap().decision, Some((true, false)));
    }
}

#[cfg(test)]
mod relay_status_tests {
    /// 설정 화면의 폰 연결 칸이 부르는 조회를 실제 DB 복사본으로: AI_INBOX_SIM_DB=<복사본> cargo test relay_status_on_real_db -- --ignored --nocapture
    #[test]
    #[ignore]
    fn relay_status_on_real_db() {
        let path = std::env::var("AI_INBOX_SIM_DB").expect("AI_INBOX_SIM_DB");
        let conn = crate::db::open(std::path::Path::new(&path)).unwrap();
        crate::db::migrate(&conn).unwrap();
        let shared = crate::relay::Shared::new();
        let st = super::relay_status_on(&conn, &shared).expect("relay_status 실패");
        println!("{}", serde_json::to_string(&st).unwrap().chars().take(400).collect::<String>());
    }
}

// ── 새 버전 ──────────────────────────────────────────────────────────────────

// ── /clear 로 끝난 대화 ──────────────────────────────────────────────────────

/// 끝난 대화의 처리를 고른다 — keep(이력으로 보관) · purge(삭제 예약) · ask(보류). 바뀐 세션 수
#[tauri::command]
pub fn clear_decide(app: tauri::AppHandle, state: State<AppState>, ids: Vec<String>, decision: String) -> R<usize> {
    if ids.len() > 500 {
        return Err("한 번에 500개까지만 고를 수 있습니다".into());
    }
    let n = {
        let conn = state.conn.lock().map_err(e)?;
        crate::lifecycle::decide(&conn, &ids, &decision)?
    };
    crate::sessions_changed(&app, ids);
    Ok(n)
}

/// /clear 됐지만 아직 정하지 않은(결정 대기) 세션들 — 결정 창용
#[derive(Serialize)]
pub struct ClearRow {
    id: String,
    name: String,
    project_name: Option<String>,
    turns: i64,
    last_at: Option<String>,
    ended: crate::lifecycle::Ended,
}

/// `only_undecided`: true 면 아직 안내를 확인하지 않은 것만, false 면 삭제 예약·미정 전체(보관 제외)
#[tauri::command]
pub fn clear_list(state: State<AppState>, only_undecided: bool) -> R<Vec<ClearRow>> {
    let conn = state.conn.lock().map_err(e)?;
    let mut st = conn
        .prepare(
            "SELECT s.id, s.live_name, s.title, s.agent_name, s.project_dir,
                    (SELECT prompt_text FROM turn t WHERE t.session_id = s.id AND t.hidden = 0 ORDER BY seq LIMIT 1),
                    (SELECT COUNT(*) FROM turn t WHERE t.session_id = s.id AND t.hidden = 0),
                    (SELECT MAX(COALESCE(ended_at, last_activity_at, prompt_at)) FROM turn t WHERE t.session_id = s.id AND t.hidden = 0),
                    s.cleared_at, s.clear_state, s.purge_at, s.clear_asked
               FROM session s
              WHERE s.cleared_at IS NOT NULL AND (?1 = 0 OR s.clear_asked = 0) AND (?1 = 1 OR COALESCE(s.clear_state,'') <> 'keep')
                AND EXISTS (SELECT 1 FROM turn t WHERE t.session_id = s.id AND t.hidden = 0)
              ORDER BY s.cleared_at DESC LIMIT 300",
        )
        .map_err(e)?;
    let rows = st
        .query_map(params![only_undecided as i64], |r| {
            let dir: Option<String> = r.get(4)?;
            let (name, _) = display_name(r.get(1)?, r.get(2)?, r.get(3)?, r.get(5)?, &dir);
            Ok(ClearRow {
                id: r.get(0)?,
                name,
                project_name: project_name(&dir),
                turns: r.get(6)?,
                last_at: r.get(7)?,
                ended: crate::lifecycle::ended_of(r.get(8)?, r.get(9)?, r.get(10)?, r.get(11)?).unwrap_or_default(),
            })
        })
        .map_err(e)?;
    Ok(rows.flatten().collect())
}

/// 안내를 확인만 하고 기본 처리를 그대로 두기 — 결정 대기에서 뺀다
#[tauri::command]
pub fn clear_ack(app: tauri::AppHandle, state: State<AppState>, ids: Vec<String>) -> R<()> {
    {
        let conn = state.conn.lock().map_err(e)?;
        for id in &ids {
            conn.execute("UPDATE session SET clear_asked = 1 WHERE id = ?1 AND cleared_at IS NOT NULL", params![id]).map_err(e)?;
        }
    }
    crate::sessions_changed(&app, ids);
    Ok(())
}

#[tauri::command]
pub fn clear_overview(state: State<AppState>) -> R<crate::lifecycle::Overview> {
    let conn = state.conn.lock().map_err(e)?;
    Ok(crate::lifecycle::overview(&conn))
}

// ── 대화 이력 검색(채팅 모드) ──────────────────────────────────────────────────

#[tauri::command]
pub fn history_status(state: State<AppState>) -> R<crate::history::Status> {
    let conn = state.conn.lock().map_err(e)?;
    Ok(crate::history::status(&conn))
}

/// 설정 한 칸: enabled(0/1) · consent(0/1 — 대화 발췌가 고른 구독 서비스로 간다는 안내에 동의) · provider(auto|claude|codex) · model_claude · model_codex
#[tauri::command]
pub fn history_set(state: State<AppState>, key: String, value: String) -> R<()> {
    let conn = state.conn.lock().map_err(e)?;
    crate::history::set(&conn, &key, &value)
}

/// 설치·로그인된 구독 CLI(Claude Code · Codex)를 새로 감지한다 — CLI 를 몇 번 부르므로 1~2초 걸린다(앱 잠금을 잡지 않는다)
#[tauri::command]
pub async fn history_detect() -> R<crate::llm::Detection> {
    tauri::async_runtime::spawn_blocking(crate::llm::detect).await.map_err(e)
}

/// 이전 버전이 저장한 API 키 파일을 지운다(더는 쓰지 않는다)
#[tauri::command]
pub fn history_forget_key() -> R<()> {
    crate::history::forget_legacy_key()
}

#[tauri::command]
pub async fn history_ask(messages: Vec<crate::history::ChatMsg>, local_only: Option<bool>, tags: Option<crate::tags::TagFilter>) -> R<crate::history::Answer> {
    // 검색은 자기 DB 연결로, 네트워크를 기다리는 동안 앱 잠금을 잡지 않는다
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db::open(&paths::db_path()).map_err(e)?;
        crate::history::ask(&conn, &messages, local_only.unwrap_or(false), tags.as_ref())
    })
    .await
    .map_err(e)?
}

#[tauri::command]
pub fn update_state(app: tauri::AppHandle) -> crate::update::UpdateState {
    crate::update::state(&app)
}

#[tauri::command]
pub fn update_ack(app: tauri::AppHandle) {
    crate::update::ack(&app)
}

#[tauri::command]
pub fn update_set_check(on: bool) {
    crate::update::set_check(on)
}

#[tauri::command]
pub fn update_skip(version: String) {
    crate::update::skip(&version)
}

#[tauri::command]
pub async fn update_check_now(app: tauri::AppHandle) -> R<Option<crate::update::Available>> {
    crate::update::check(&app).await
}

#[tauri::command]
pub async fn update_install(app: tauri::AppHandle) -> R<()> {
    crate::update::install(&app).await
}

/// 디스크에 깔린 새 버전을 적용하려고 다시 시작
#[tauri::command]
pub fn app_restart(app: tauri::AppHandle) {
    app.restart()
}

#[cfg(test)]
mod perf_probe {
    use super::*;

    /// 매 틱·화면 새로고침마다 도는 쿼리의 비용 — 실제 DB 의 **복사본**으로만(수동 실행):
    /// `sqlite3 inbox.db ".backup /tmp/x.db"` 뒤 `AI_INBOX_DB_COPY=/tmp/x.db cargo test --release perf_hot_queries -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn perf_hot_queries() {
        let path = std::env::var("AI_INBOX_DB_COPY").expect("AI_INBOX_DB_COPY");
        assert!(!path.ends_with("/inbox.db") || !path.contains("com.yeojeonghun"), "실제 inbox.db 말고 복사본을 넘겨라");
        let c = Connection::open(path).unwrap();
        crate::db::migrate(&c).unwrap();
        let time = |name: &str, f: &mut dyn FnMut() -> usize| {
            let n = f();
            let t = std::time::Instant::now();
            for _ in 0..20 {
                f();
            }
            println!("{name:<28} {:>7.2}ms  ({n})", t.elapsed().as_secs_f64() * 1000.0 / 20.0);
        };
        time("counts_of", &mut || {
            counts_of(&c);
            1
        });
        time("list_sessions(all)", &mut || list_sessions_on(&c, "all", "").unwrap().len());
        let big: String = c.query_row("SELECT session_id FROM turn GROUP BY 1 ORDER BY COUNT(*) DESC LIMIT 1", [], |r| r.get(0)).unwrap();
        time("get_chat(가장 긴 세션) 바이트", &mut || {
            let p = get_chat_on(&c, &big, None, None).unwrap();
            serde_json::to_vec(&p).unwrap().len()
        });
    }
}
