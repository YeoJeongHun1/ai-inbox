//! 세션에 말을 넣는 방법을 고른다 — 데스크톱 입력창과 폰 답이 같은 길을 쓴다.
//!   1. 세션이 AI Inbox 채널과 함께 실행 중 → 채널로
//!   2. 실행 중인 세션(터미널·백그라운드)에 대기 훅(`ai-inbox wake`, asyncRewake)이 붙어 있음 → 세션이 쉬는 중이면 바로,
//!      일하는 중이면 끝날 때까지 기다렸다가 넣는다. 대기 훅이 아직 없으면 설정 파일 수정 시각을 갱신해 다시 잇는다(ConfigChange)
//!   3. 대기 훅 없는 백그라운드 세션이 쉬는 중 → 멈추고 `claude --bg --resume <세션> "<말>"`
//!   4. 세션이 꺼져 있음 → `claude --bg --resume <세션> "<말>"` (폰 답은 사용자가 설정에서 허락했을 때만)
//!   5. 대기 훅이 설치되지 않은 터미널 세션 → 넣지 않는다(훅을 다시 설치하면 된다)
//!
//! Codex 세션(`codex.rs`)은 길이 둘뿐이다: 열려 있고 쉬는 중이면 대기열(`codex queue`, 열린 TUI 가 곧바로 가져간다) ·
//! 일하는 중이면 끝난 뒤 · 꺼져 있으면 `codex exec resume`(꺼진 세션 이어서 실행 설정을 따른다).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::{channel, conoti, db, paths};

pub const TERMINAL_TEXT: &str =
    "실행 중인 세션에 말을 넣으려면 훅을 업데이트해야 합니다 — 설정 → Claude Code 훅 → 훅 설치 (세션을 다시 시작할 필요는 없습니다)";
pub const OFFLINE_TEXT: &str = "세션이 꺼져 있음 — 데스크톱 설정에서 '꺼진 세션 이어서 실행'을 켜면 폰 답으로 이어집니다";

/// 전달 결과: 넣었다 · 기다린다(다음에 다시) · 못 넣는다
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Done(String),
    /// 지금은 못 넣는다 — 잠시 뒤 다시(연결을 잇는 중 · 세션을 멈추는 중 …)
    Wait(String),
    /// 세션이 앞 작업을 하는 중 — 끝나면 넣는다. 폰 말의 10분 시계는 이 동안 멈춘다(`conoti_reply.wait_from`)
    Busy(String),
    Fail(String),
}

/// 지금 이 세션에 말을 넣는 길
#[derive(Debug, PartialEq)]
pub enum Route {
    Channel,
    /// 대기 훅이 붙어 있고 세션이 쉬거나 일하는 중 — 바로 넣는다(일하는 중이면 도구 사이에 읽힌다)
    Live,
    /// 세션이 일하는 중인데 대기 훅이 없다 · 권한 승인 대기(그 사유) — 끝나면 넣는다
    Busy(Option<String>),
    /// 대기 훅 없는 백그라운드 세션이 쉬는 중 — 멈추고 이어서 실행
    Idle,
    /// 쉬는 세션인데 대기 훅이 아직 없다 — 다시 잇는 중
    Unarmed,
    /// 꺼져 있음 — 이어서 실행
    Ended,
    /// 대기 훅이 설치되지 않은 터미널 세션 — 못 넣는다
    Terminal,
}

pub struct Live {
    /// `claude --bg` 로 띄운 세션(등록부 kind = "bg", jobId 있음)
    pub background: bool,
    /// 입력을 받을 수 있는가 기준으로 맞춘 값(`input_status`)
    pub status: Option<String>,
    /// 등록부 원래 값이 `shell` — 입력은 기다리지만 백그라운드 셸이 돈다(멈추면 그 셸이 죽는다)
    pub shell: bool,
    pub waiting_for: Option<String>,
}

/// 등록부의 `status` 를 "입력을 받을 수 있는가" 기준으로 맞춘다.
/// Claude Code 의 값은 `busy · shell · idle · waiting` — `shell` 은 **입력을 기다리지만 백그라운드 셸이 도는 중**이다
/// (실측 2026-09-24: 끝나지 않는 백그라운드 명령 하나 때문에 세션이 몇 시간씩 `shell` 로 남아 보낸 말이 전달되지 않았다).
pub fn input_status(raw: Option<String>) -> Option<String> {
    match raw.as_deref() {
        Some("shell") => Some("idle".into()),
        _ => raw,
    }
}

/// Claude Code 세션 등록부(~/.claude/sessions/<pid>.json)에서 이 세션의 살아 있는 프로세스
pub fn live_entry(session_id: &str) -> Option<Live> {
    let rd = std::fs::read_dir(paths::registry_dir()).ok()?;
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else { continue };
        if bytes.len() > 64 * 1024 {
            continue;
        }
        let Ok(v) = serde_json::from_slice::<Value>(&bytes) else { continue };
        if v.get("sessionId").and_then(Value::as_str) != Some(session_id) {
            continue;
        }
        let Some(pid) = v.get("pid").and_then(Value::as_i64) else { continue };
        if !pid_alive(pid) {
            continue;
        }
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let background = matches!(s("kind").as_deref(), Some("bg" | "background")) || s("jobId").is_some();
        let raw = s("status");
        let shell = raw.as_deref() == Some("shell");
        return Some(Live { background, status: input_status(raw), shell, waiting_for: s("waitingFor") });
    }
    None
}

pub fn route(session_id: &str) -> Route {
    if crate::codex::is_codex(session_id) {
        return codex_route(session_id);
    }
    if channel::channel_alive(session_id) {
        return Route::Channel;
    }
    let Some(l) = live_entry(session_id) else { return Route::Ended };
    let idle = l.status.as_deref() == Some("idle");
    // 일하는 중이어도 대기자가 있으면 넣는다 — 터미널에서 작업 중에 친 말처럼 다음 도구 사이에 읽힌다. 권한 승인 대기는 기다린다.
    let waiter = crate::wake::takes_now(l.status.as_deref()) && l.waiting_for.is_none() && crate::wake::waiter_alive(session_id);
    decide(idle, l.shell, l.background, waiter, crate::install::wake_ready(), l.waiting_for)
}

/// Codex 세션: 열려 있나(잠금 파일) · 일하는 중인가(수집기가 적은 live_status) · 이 앱이 띄운 exec 가 아직 도는가
fn codex_route(session_id: &str) -> Route {
    let live = crate::codex::thread_live(session_id);
    let busy = crate::codex::exec_running(session_id) || codex_busy(session_id);
    decide_codex(live, busy)
}

fn decide_codex(live: Option<bool>, busy: bool) -> Route {
    match live {
        _ if busy => Route::Busy(None),
        Some(true) => Route::Live,
        Some(false) => Route::Ended,
        // 열려 있는지 알 수 없는 플랫폼 — 대기열에만 넣는다(열어 둔 Codex 가 가져가고, 닫혀 있으면 다음에 열 때)
        None => Route::Live,
    }
}

fn codex_busy(session_id: &str) -> bool {
    let path = paths::db_path();
    // 시험은 데이터 폴더를 바꿔 둔 경우에만 DB 를 본다(실제 DB 를 열지 않게)
    if !path.exists() || (cfg!(test) && !paths::has_data_dir_override()) {
        return false;
    }
    db::open(&path)
        .ok()
        .and_then(|c| {
            c.query_row("SELECT live_status FROM session WHERE id = ?1", [session_id], |r| r.get::<_, Option<String>>(0)).ok().flatten()
        })
        .as_deref()
        == Some("busy")
}

/// 전달 경로 판정(등록부·파일을 읽지 않는 순수 함수 — 시험용으로 뗐다)
fn decide(idle: bool, shell: bool, background: bool, waiter: bool, hooks: bool, waiting_for: Option<String>) -> Route {
    if waiter {
        return Route::Live;
    }
    match (idle, background) {
        (false, true) => Route::Busy(waiting_for),
        (false, false) if hooks => Route::Busy(waiting_for),
        // 대기 훅 없는 백그라운드 세션이 백그라운드 셸을 돌리는 중이면 기다린다 — 멈추고 이어서 실행하면 그 셸(개발 서버 등)이 죽는다
        (true, true) if shell => Route::Busy(None),
        (true, true) => Route::Idle,
        (true, false) if hooks => Route::Unarmed,
        _ => Route::Terminal,
    }
}

fn busy_note(_session_id: &str, waiting_for: &Option<String>) -> String {
    match waiting_for {
        Some(_) => "세션이 권한 승인을 기다립니다 — 승인한 뒤 하던 일이 끝나면 보냅니다".into(),
        None => "앞 작업이 끝나면 보냅니다".into(),
    }
}

pub struct LocalDeliver {
    pub bg_resume: bool,
}

impl conoti::Deliver for LocalDeliver {
    fn deliver(&self, t: &conoti::Target, reply_id: &str, text: &str, from_desktop: bool) -> Outcome {
        let may_launch = from_desktop || self.bg_resume;
        if crate::codex::is_codex(&t.session_id) {
            return deliver_codex(t, text, may_launch);
        }
        match route(&t.session_id) {
            Route::Channel => match channel::hand_over(&t.session_id, reply_id, text, Duration::from_secs(6)) {
                Ok(()) => Outcome::Done("채널로 전달".into()),
                Err(e) => Outcome::Fail(e),
            },
            Route::Live => match channel::hand_over(&t.session_id, reply_id, text, Duration::from_secs(8)) {
                Ok(()) => Outcome::Done("실행 중인 세션에 전달".into()),
                Err(_) => {
                    crate::install::rearm();
                    Outcome::Wait("세션에 넣는 중".into())
                }
            },
            Route::Unarmed => {
                crate::install::rearm();
                Outcome::Wait("세션 연결을 잇는 중 — 터미널에서 한 번 말해도 바로 이어집니다".into())
            }
            Route::Terminal => Outcome::Fail(TERMINAL_TEXT.into()),
            Route::Busy(w) => Outcome::Busy(busy_note(&t.session_id, &w)),
            Route::Idle if !may_launch => Outcome::Fail(OFFLINE_TEXT.into()),
            Route::Idle => {
                // 쉬는 백그라운드 세션: 멈춘 뒤(대화는 남는다) 같은 ID 로 이어서 실행.
                // 백그라운드 관리자가 "실행 중"으로 아는 동안 이어서 실행하면 복사본 세션이 생긴다(실측) — 완전히 멈출 때까지 본다
                stop_session(&t.session_id);
                let until = Instant::now() + Duration::from_secs(15);
                while (live_entry(&t.session_id).is_some() || daemon_active(&t.session_id)) && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(500));
                }
                if live_entry(&t.session_id).is_some() || daemon_active(&t.session_id) {
                    return Outcome::Wait("쉬고 있는 백그라운드 세션을 멈추는 중".into());
                }
                match resume_in_background(t, text) {
                    Ok(()) => Outcome::Done("백그라운드 세션에서 이어서 실행".into()),
                    Err(e) => Outcome::Fail(e),
                }
            }
            Route::Ended if !may_launch => Outcome::Fail(OFFLINE_TEXT.into()),
            Route::Ended if daemon_active(&t.session_id) => Outcome::Wait("백그라운드 세션이 아직 멈추는 중".into()),
            Route::Ended => match resume_in_background(t, text) {
                Ok(()) => Outcome::Done("꺼진 세션을 백그라운드로 이어서 실행".into()),
                Err(e) => Outcome::Fail(e),
            },
        }
    }
}

/// Codex 세션에 넣기. 이미지는 글 끝의 경로 목록과 함께 `--image` 로도 넘긴다(Codex 가 바로 본다).
fn deliver_codex(t: &conoti::Target, text: &str, may_launch: bool) -> Outcome {
    let images = crate::attach::block_paths(text);
    match codex_route(&t.session_id) {
        Route::Busy(_) => Outcome::Busy("앞 작업이 끝나면 보냅니다".into()),
        Route::Live => match crate::codex::queue(&t.session_id, text, &images) {
            Ok(()) if crate::codex::thread_live(&t.session_id).is_none() => {
                Outcome::Done("Codex 대기열에 넣음 — 세션이 열려 있으면 바로, 아니면 다음에 열 때 처리".into())
            }
            Ok(()) => Outcome::Done("실행 중인 Codex 세션에 전달".into()),
            Err(e) => Outcome::Fail(e),
        },
        Route::Ended if !may_launch => Outcome::Fail(OFFLINE_TEXT.into()),
        Route::Ended => {
            let Some(cwd) = t.cwd.as_deref().map(Path::new).filter(|d| d.is_dir()) else {
                return Outcome::Fail("세션 작업 폴더가 없음".into());
            };
            match crate::codex::resume(&t.session_id, cwd, text, &images) {
                Ok(()) => Outcome::Done("꺼진 Codex 세션을 이어서 실행".into()),
                Err(e) => Outcome::Fail(e),
            }
        }
        _ => Outcome::Fail("Codex 세션에 넣을 길이 없음".into()),
    }
}

fn pid_alive(pid: i64) -> bool {
    crate::ingest::pid_alive(pid)
}

/// Finder 로 띄운 앱은 PATH 가 짧다 — 흔한 설치 위치를 먼저 보고, 없으면 로그인 셸에 묻는다.
pub fn find_claude() -> Option<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut cands: Vec<PathBuf> = vec![
        home.join(".local/bin/claude"),
        home.join(".claude/local/claude"),
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
    ];
    if cfg!(windows) {
        cands = vec![
            home.join(".local/bin/claude.exe"),
            home.join("AppData/Roaming/npm/claude.cmd"),
            home.join("AppData/Local/Programs/claude/claude.exe"),
        ];
    }
    if let Some(p) = cands.into_iter().find(|p| p.is_file()) {
        return Some(p);
    }
    #[cfg(unix)]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let out = Command::new(shell).args(["-lc", "command -v claude"]).output().ok()?;
        let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        if p.is_absolute() && p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Claude Code 안에서 띄운 앱이면 그 세션의 환경변수가 따라온다 — 새 세션이 자기를 자식으로 착각하지 않게 지운다.
const INHERITED_ENV: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_PID",
    "CLAUDE_CODE_MESSAGING_SOCKET",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_EXECPATH",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_EFFORT",
];

fn claude_cmd() -> Result<Command, String> {
    let claude = find_claude().ok_or("claude 실행 파일을 찾지 못함")?;
    let mut c = Command::new(claude);
    for k in INHERITED_ENV {
        c.env_remove(k);
    }
    c.stdin(Stdio::null());
    Ok(c)
}

/// 명령을 돌리되 제한 시간을 넘기면 끊는다
pub(crate) fn output_within(mut c: Command, limit: Duration) -> Option<std::process::Output> {
    let mut child = c.stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let until = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Claude Code 의 백그라운드 관리자(`claude agents --json`)가 이 세션을 아직 실행 중으로 아는가.
/// 알 수 없으면(옛 버전·시간 초과) false — 등록부 확인만으로 간다.
fn daemon_active(session_id: &str) -> bool {
    let Ok(mut c) = claude_cmd() else { return false };
    c.args(["agents", "--json"]);
    let Some(out) = output_within(c, Duration::from_secs(8)) else { return false };
    let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) else { return false };
    v.as_array()
        .map(|a| a.iter().any(|x| x.get("sessionId").and_then(Value::as_str) == Some(session_id) && x.get("pid").is_some()))
        .unwrap_or(false)
}

/// `claude --bg` 가 알려 주는 짧은 ID(세션 ID 앞 8자리)
pub fn short_of(session_id: &str) -> String {
    session_id.chars().take(8).collect()
}

fn valid_short(s: &str) -> bool {
    (6..=16).contains(&s.len()) && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// "backgrounded · <짧은 id>" 에서 짧은 ID
fn parse_short(stdout: &str) -> Option<String> {
    stdout.split_whitespace().skip_while(|w| *w != "·").nth(1).filter(|w| valid_short(w)).map(str::to_string)
}

fn launch_error(out: &std::process::Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr);
    let all = format!("{err}\n{}", String::from_utf8_lossy(&out.stdout));
    if all.contains("not trusted") {
        return "이 폴더는 아직 Claude Code 가 신뢰하지 않은 폴더입니다 — 터미널에서 그 폴더로 가서 claude 를 한 번 열고 신뢰를 허용하세요".into();
    }
    let line = all.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    format!("claude 가 종료 코드 {:?} 로 끝남 {}", out.status.code(), crate::text::clip(line, 160))
}

/// 넣을 글은 머리말로 시작해야 한다 — 인자로 넘길 때 `-` 로 시작해 옵션으로 읽히는 일이 없게
fn safe_prompt(text: &str) -> Result<(), String> {
    if text.starts_with(conoti::REPLY_HEADER) || text.starts_with(conoti::INBOX_HEADER) {
        Ok(())
    } else {
        Err("머리말 없는 글은 넣지 않습니다".into())
    }
}

fn resume_in_background(t: &conoti::Target, text: &str) -> Result<(), String> {
    if !channel::valid_session_id(&t.session_id) {
        return Err("세션 ID 형식 오류".into());
    }
    safe_prompt(text)?;
    let cwd = t.cwd.clone().filter(|d| Path::new(d).is_dir()).ok_or("세션 작업 폴더가 없음")?;
    // 셸을 거치지 않고 인자로 직접 넘긴다(본문이 셸에 해석되지 않는다).
    // 권한 모드는 지정하지 않는다 = 사용자의 기본 설정. 승인이 필요하면 세션이 멈추고 `claude attach` 로 연다.
    let out = claude_cmd()?
        .current_dir(cwd)
        .args(["--bg", "--resume", &t.session_id, text])
        .output()
        .map_err(|e| format!("claude 실행 실패: {e}"))?;
    if !out.status.success() {
        return Err(launch_error(&out));
    }
    // 요청이 끝나면 이 백그라운드 세션을 멈춘다(쉬는 세션이 프로세스를 붙잡고 있지 않게)
    let short = parse_short(&String::from_utf8_lossy(&out.stdout)).unwrap_or_else(|| short_of(&t.session_id));
    if short != short_of(&t.session_id) {
        // 원래 세션이 아직 실행 중이라 복사본이 생겼다 — 복사본은 멈추고 다음에 다시 (말은 복사본에서 이미 시작됐을 수 있다)
        stop_background(&short);
        return Err("세션이 아직 실행 중이라 복사본이 생겨 멈췄습니다 — 잠시 뒤 다시 보내세요".into());
    }
    if let Ok(conn) = db::open(&paths::db_path()) {
        let _ = db::set_meta(&conn, &format!("conoti.bg.{}", t.session_id), &short);
    }
    Ok(())
}

/// 세션 이름: 할 일의 첫 줄(머리말이 이름이 되지 않게 직접 붙인다)
pub fn task_name(body: &str) -> String {
    let line = crate::text::first_line(body.trim());
    let clean: String = line.chars().filter(|c| !c.is_control()).collect::<String>().trim_start_matches(['-', ' ']).to_string();
    let name = crate::text::clip(clean.trim(), 40);
    if name.is_empty() { "AI Inbox 작업".into() } else { name }
}

/// 새 작업: 고른 폴더에서 `claude --bg --name=<이름> "<말>"`. 반환: (짧은 ID, 등록부에서 찾은 세션 ID)
pub fn start_session(cwd: &Path, name: &str, text: &str) -> Result<(String, Option<String>), String> {
    safe_prompt(text)?;
    if !cwd.is_dir() {
        return Err("폴더가 없습니다".into());
    }
    let out = claude_cmd()?
        .current_dir(cwd)
        .args(["--bg", &format!("--name={name}"), text])
        .output()
        .map_err(|e| format!("claude 실행 실패: {e}"))?;
    if !out.status.success() {
        return Err(launch_error(&out));
    }
    let short = parse_short(&String::from_utf8_lossy(&out.stdout)).ok_or("claude 가 백그라운드 세션 ID 를 알려 주지 않음")?;
    // 등록부에 올라올 때까지 잠깐 기다려 전체 세션 ID 를 찾는다
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(sid) = find_by_short(&short) {
            if let Ok(conn) = db::open(&paths::db_path()) {
                let _ = db::set_meta(&conn, &format!("conoti.bg.{sid}"), &short);
            }
            return Ok((short, Some(sid)));
        }
        if Instant::now() >= until {
            return Ok((short, None));
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn find_by_short(short: &str) -> Option<String> {
    let rd = std::fs::read_dir(paths::registry_dir()).ok()?;
    let prefix = format!("{short}-");
    rd.flatten().find_map(|e| {
        let bytes = std::fs::read(e.path()).ok()?;
        let v: Value = serde_json::from_slice(&bytes).ok()?;
        let sid = v.get("sessionId").and_then(Value::as_str)?;
        (sid.starts_with(&prefix) && channel::valid_session_id(sid)).then(|| sid.to_string())
    })
}

/// 백그라운드 세션을 멈춘다(대화는 남는다 — 다음 말 때 다시 이어서 실행).
pub fn stop_session(session_id: &str) {
    stop_background(&short_of(session_id));
}

pub fn stop_background(short_id: &str) {
    if !valid_short(short_id) {
        return;
    }
    if let Ok(mut c) = claude_cmd() {
        let _ = c.args(["stop", short_id]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

/// 설정 화면에 보여 줄 연결 명령들
pub fn setup_commands() -> (String, String) {
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "ai-inbox".into());
    let add = if cfg!(windows) {
        format!("claude mcp add --scope user {} -- \"{}\" channel", channel::SERVER_NAME, exe.replace('\\', "/"))
    } else {
        format!("claude mcp add --scope user {} -- '{}' channel", channel::SERVER_NAME, exe.replace('\'', "'\\''"))
    };
    let start = format!("claude --dangerously-load-development-channels server:{}", channel::SERVER_NAME);
    (add, start)
}

pub fn bg_resume_enabled() -> bool {
    db::open(&paths::db_path()).map(|c| db::get_meta(&c, "conoti.bg_resume").as_deref() == Some("1")).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes() {
        // 대기 훅이 살아 있으면 셸이 돌아도 바로 넣는다
        assert_eq!(decide(true, true, false, true, true, None), Route::Live);
        assert_eq!(decide(true, true, true, true, true, None), Route::Live);
        // 대기 훅 없는 백그라운드 세션: 셸이 돌면 기다리고(멈추지 않는다), 아니면 멈추고 이어서 실행
        assert_eq!(decide(true, true, true, false, true, None), Route::Busy(None));
        assert_eq!(decide(true, false, true, false, true, None), Route::Idle);
        // 대화형 세션: 대기 훅을 다시 잇는다
        assert_eq!(decide(true, true, false, false, true, None), Route::Unarmed);
        assert_eq!(decide(true, false, false, false, false, None), Route::Terminal);
        assert_eq!(decide(false, false, false, false, true, Some("permission".into())), Route::Busy(Some("permission".into())));
        // 일하는 중: 대기자가 있으면 바로(도구 사이에 읽힌다), 없으면 끝나길 기다린다
        assert_eq!(decide(false, false, false, true, true, None), Route::Live);
        assert_eq!(decide(false, false, true, true, true, None), Route::Live);
        assert_eq!(decide(false, false, false, false, true, None), Route::Busy(None));
        assert_eq!(decide(false, false, false, false, false, None), Route::Terminal);
    }

    #[test]
    fn codex_routes() {
        // 열려 있고 쉬는 중 → 대기열 · 일하는 중 → 기다림 · 꺼짐 → 이어서 실행 · 알 수 없음 → 대기열
        assert_eq!(decide_codex(Some(true), false), Route::Live);
        assert_eq!(decide_codex(Some(true), true), Route::Busy(None));
        assert_eq!(decide_codex(Some(false), false), Route::Ended);
        assert_eq!(decide_codex(Some(false), true), Route::Busy(None), "이 앱이 띄운 exec 가 아직 돌면 기다린다");
        assert_eq!(decide_codex(None, false), Route::Live);
    }

    #[test]
    fn background_shell_counts_as_waiting_for_input() {
        assert_eq!(input_status(Some("shell".into())).as_deref(), Some("idle"));
        assert_eq!(input_status(Some("idle".into())).as_deref(), Some("idle"));
        assert_eq!(input_status(Some("busy".into())).as_deref(), Some("busy"));
        assert_eq!(input_status(Some("waiting".into())).as_deref(), Some("waiting"));
        assert_eq!(input_status(None), None);
    }

    #[test]
    fn short_ids() {
        assert_eq!(parse_short("Starting background service…\nbackgrounded · 11111111\n  claude agents"), Some("11111111".into()));
        assert_eq!(parse_short("backgrounded · ; rm -rf"), None);
        assert_eq!(short_of("11111111-2222-4333-8444-555555555555"), "11111111");
        assert!(!valid_short("--help"));
    }

    #[test]
    fn prompt_must_start_with_header() {
        assert!(safe_prompt("--dangerously-skip-permissions").is_err());
        assert!(safe_prompt(&conoti::wrap_desk("안녕")).is_ok());
        assert!(safe_prompt(&conoti::wrap_reply("t", "text", "x")).is_ok());
    }

    #[test]
    fn task_names() {
        assert_eq!(task_name("  --help 보여 줘\n둘째 줄"), "help 보여 줘");
        assert_eq!(task_name("\n\n"), "AI Inbox 작업");
    }

    #[test]
    fn unknown_session_is_ended() {
        assert_eq!(route("00000000-0000-0000-0000-000000000000"), Route::Ended);
    }

    /// 실제 claude 로 새 작업 → 일하는 중 대기 → 쉬면 멈추고 이어서 실행까지(신뢰한 폴더가 필요하다).
    /// AI_INBOX_LIVE_DIR=<신뢰한 폴더> cargo test live_desktop_send -- --ignored --nocapture
    /// 이미지 붙여 새 작업 → 세션이 Read 로 열어 색을 답하는가. 이어서 이미지 붙인 두 번째 말.
    /// `AI_INBOX_LIVE_DIR=<신뢰한 폴더> cargo test live_image_send -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_image_send() {
        use crate::{attach, conoti, install};
        let dir = PathBuf::from(std::env::var("AI_INBOX_LIVE_DIR").expect("AI_INBOX_LIVE_DIR"));
        let data = std::env::temp_dir().join(format!("aiinbox-live-img-{}", std::process::id()));
        paths::set_data_dir_override(data.clone());
        let conn = db::open(&paths::db_path()).unwrap();
        db::migrate(&conn).unwrap();
        let solid = |rgb: [u8; 3]| {
            let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(80, 50, image::Rgb(rgb)));
            let mut buf = std::io::Cursor::new(Vec::new());
            img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
            buf.into_inner()
        };
        // 이 시험의 데이터 폴더를 읽게 — 설치가 넣는 것과 같은 규칙(프로젝트 설정에 잠시)
        let local = dir.join(".claude/settings.local.json");
        let before = std::fs::read_to_string(&local).ok();
        std::fs::create_dir_all(dir.join(".claude")).unwrap();
        let rule = install::read_rule().expect("규칙");
        std::fs::write(&local, serde_json::json!({"permissions": {"allow": [rule]}}).to_string()).unwrap();

        let red = attach::store(&conn, solid([220, 20, 20]), Some("red.png"), "desktop").unwrap();
        let text = "첨부한 이미지의 주된 색을 한국어 한 단어로만 답해";
        let prompt = attach::append_block(&conoti::wrap_desk(text), attach::block(&conn, std::slice::from_ref(&red.id)));
        println!("{prompt}");
        let (short, sid) = start_session(&dir, &task_name(text), &prompt).expect("새 작업");
        let sid = sid.expect("세션 ID");
        println!("started {short} {sid}");
        conn.execute("INSERT INTO session (id, project_dir) VALUES (?1, ?2)", rusqlite::params![sid, dir.to_string_lossy()]).unwrap();
        let transcript = || {
            std::fs::read_dir(paths::projects_dir())
                .ok()?
                .flatten()
                .map(|d| d.path().join(format!("{sid}.jsonl")))
                .find(|p| p.is_file())
        };
        let answer_after = |marker: &str, want: &[&str], secs: u64| {
            let until = Instant::now() + Duration::from_secs(secs);
            loop {
                if let Some(t) = transcript() {
                    // 그 말이 처음 나온 줄 뒤의 어시스턴트 줄에서 찾는다(파일 끝의 요약 줄에도 같은 글이 있다)
                    let body = std::fs::read_to_string(&t).unwrap_or_default();
                    let lines: Vec<&str> = body.lines().collect();
                    if let Some(at) = lines.iter().position(|l| l.contains(marker)) {
                        if let Some(a) = lines[at + 1..]
                            .iter()
                            .find(|l| l.contains("\"type\":\"assistant\"") && want.iter().any(|w| l.contains(w)))
                        {
                            return a.chars().take(300).collect::<String>();
                        }
                    }
                }
                assert!(Instant::now() < until, "답 없음: {marker}");
                std::thread::sleep(Duration::from_secs(2));
            }
        };
        println!("1st: {}", answer_after(text, &["빨", "적색", "red", "Red"], 150));

        // 두 번째: 파란 이미지만(글 없이) — 쉬는 백그라운드 세션은 멈추고 이어서 실행
        let blue = attach::store(&conn, solid([20, 40, 220]), None, "desktop").unwrap();
        let r = conoti::accept_desktop(&conn, &sid, "", std::slice::from_ref(&blue.id), None).expect("받기");
        let rid = r["rid"].as_str().unwrap().to_string();
        let deliverer = LocalDeliver { bg_resume: false };
        let mut pipe = conoti::Pipeline::new(db::open(&paths::db_path()).unwrap());
        let until = Instant::now() + Duration::from_secs(180);
        loop {
            pipe.tick(&deliverer);
            let (st, note): (String, Option<String>) = conn
                .query_row("SELECT state, note FROM conoti_reply WHERE reply_id = ?1", rusqlite::params![rid], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap();
            println!("  {st} {note:?} route={:?}", route(&sid));
            if st == "delivered" {
                break;
            }
            assert_ne!(st, "rejected", "{note:?}");
            assert!(Instant::now() < until, "시간 초과");
            std::thread::sleep(Duration::from_secs(2));
        }
        let marker = format!("/{}.png", blue.id);
        println!("2nd: {}", answer_after(&marker, &["파", "blue", "Blue", "청"], 150));

        let until = Instant::now() + Duration::from_secs(60);
        while !matches!(route(&sid), Route::Idle) && Instant::now() < until {
            std::thread::sleep(Duration::from_secs(1));
        }
        stop_session(&sid);
        match before {
            Some(b) => std::fs::write(&local, b).unwrap(),
            None => {
                let _ = std::fs::remove_file(&local);
            }
        }
        let _ = std::fs::remove_dir_all(&data);
        println!("ok");
    }

    /// 실제 codex 로 세 경로를 끝까지: 새 작업(codex exec) → 꺼진 세션에 보내기(exec resume) → 열려 있는 세션에 보내기(codex queue).
    /// 열린 세션은 가상 터미널로 띄운 TUI(`AI_INBOX_PTY` = 가상 터미널 스크립트, 인자: 초 · 명령…). 폴더는 Codex 가 신뢰한 곳이어야 한다.
    /// `AI_INBOX_LIVE_DIR=<폴더> AI_INBOX_PTY=<스크립트> cargo test live_codex_send -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_codex_send() {
        use crate::{codex, conoti};
        let dir = PathBuf::from(std::env::var("AI_INBOX_LIVE_DIR").expect("AI_INBOX_LIVE_DIR"));
        let data = std::env::temp_dir().join(format!("aiinbox-live-codex-{}", std::process::id()));
        paths::set_data_dir_override(data.clone());
        let conn = db::open(&paths::db_path()).unwrap();
        db::migrate(&conn).unwrap();
        let rollout = |sid: &str| -> Option<PathBuf> {
            fn walk(d: &Path, sid: &str, depth: usize) -> Option<PathBuf> {
                for e in std::fs::read_dir(d).ok()?.flatten() {
                    let p = e.path();
                    if p.is_dir() && depth < 4 {
                        if let Some(x) = walk(&p, sid, depth + 1) {
                            return Some(x);
                        }
                    } else if p.file_name().is_some_and(|n| n.to_string_lossy().ends_with(&format!("{sid}.jsonl"))) {
                        return Some(p);
                    }
                }
                None
            }
            walk(&paths::codex_sessions_dir(), sid, 0)
        };
        let wait_answer = |sid: &str, word: &str, secs: u64| {
            let until = Instant::now() + Duration::from_secs(secs);
            loop {
                let body = rollout(sid).and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default();
                // 사용자 말 뒤에 그 단어로 끝난 턴(task_complete 의 마지막 에이전트 말)
                let done = body.lines().any(|l| l.contains("\"task_complete\"") && l.contains(&format!("\"last_agent_message\":\"{word}")));
                if done {
                    return;
                }
                assert!(Instant::now() < until, "답 없음: {word}");
                std::thread::sleep(Duration::from_secs(2));
            }
        };
        let deliver_desk = |sid: &str, text: &str, want_note: &str| {
            let r = conoti::accept_desktop(&conn, sid, text, &[], None).expect("받기");
            let rid = r["rid"].as_str().unwrap().to_string();
            let deliverer = LocalDeliver { bg_resume: false };
            let mut pipe = conoti::Pipeline::new(db::open(&paths::db_path()).unwrap());
            let until = Instant::now() + Duration::from_secs(120);
            loop {
                pipe.tick(&deliverer);
                let (st, note): (String, Option<String>) = conn
                    .query_row("SELECT state, note FROM conoti_reply WHERE reply_id = ?1", rusqlite::params![rid], |r| Ok((r.get(0)?, r.get(1)?)))
                    .unwrap();
                println!("  {st} {note:?} route={:?}", route(sid));
                if st == "delivered" {
                    assert!(note.as_deref().unwrap_or("").contains(want_note), "{note:?}");
                    break;
                }
                assert_ne!(st, "rejected", "{note:?}");
                assert!(Instant::now() < until, "시간 초과");
                std::thread::sleep(Duration::from_secs(2));
            }
        };

        // 1. 새 작업
        let sid = codex::start(&dir, &conoti::wrap_desk("다른 말 없이 cxa 라고만 답해"), &[]).expect("새 작업");
        println!("started {sid}");
        conn.execute("INSERT INTO session (id, project_dir, agent) VALUES (?1, ?2, 'codex')", rusqlite::params![sid, dir.to_string_lossy()])
            .unwrap();
        codex::remember(&sid);
        wait_answer(&sid, "cxa", 180);
        let until = Instant::now() + Duration::from_secs(30);
        while (codex::exec_running(&sid) || codex::thread_live(&sid) == Some(true)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(500));
        }
        assert_eq!(route(&sid), Route::Ended, "exec 가 끝나면 꺼진 세션");

        // 2. 꺼진 세션 → exec resume
        deliver_desk(&sid, "다른 말 없이 cxb 라고만 답해", "이어서 실행");
        wait_answer(&sid, "cxb", 180);
        let until = Instant::now() + Duration::from_secs(30);
        while (codex::exec_running(&sid) || codex::thread_live(&sid) == Some(true)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(500));
        }

        // 3. 열려 있는 세션(TUI) → 대기열
        if let Ok(pty) = std::env::var("AI_INBOX_PTY") {
            let mut tui = Command::new("python3")
                .current_dir(&dir)
                .args([pty.as_str(), "90", "codex", "resume", &sid, "--no-alt-screen", "-c", "check_for_update_on_startup=false"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("TUI");
            let until = Instant::now() + Duration::from_secs(40);
            while codex::thread_live(&sid) != Some(true) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(500));
            }
            assert_eq!(codex::thread_live(&sid), Some(true), "TUI 가 세션을 열어야 한다");
            assert_eq!(route(&sid), Route::Live);
            deliver_desk(&sid, "다른 말 없이 cxc 라고만 답해", "실행 중인 Codex");
            wait_answer(&sid, "cxc", 120);
            let _ = tui.kill();
            let _ = tui.wait();
        }
        let _ = std::fs::remove_dir_all(&data);
        println!("ok — {}", rollout(&sid).map(|p| p.display().to_string()).unwrap_or_default());
    }

    #[test]
    #[ignore]
    fn live_desktop_send() {
        use crate::conoti;
        let dir = PathBuf::from(std::env::var("AI_INBOX_LIVE_DIR").expect("AI_INBOX_LIVE_DIR"));
        let data = std::env::temp_dir().join(format!("aiinbox-live-{}", std::process::id()));
        paths::set_data_dir_override(data.clone());
        let conn = db::open(&paths::db_path()).unwrap();
        db::migrate(&conn).unwrap();

        let (short, sid) = start_session(&dir, &task_name("ok1 이라고만 답해"), &conoti::wrap_desk("ok1 이라고만 답해")).expect("새 작업");
        let sid = sid.expect("등록부에서 세션 ID");
        println!("started {short} {sid}");
        conn.execute("INSERT INTO session (id, project_dir) VALUES (?1, ?2)", rusqlite::params![sid, dir.to_string_lossy()]).unwrap();

        // 바로 두 번째 말 — 첫 요청이 끝나기 전이면 기다렸다가, 쉬면 멈추고 이어서 실행
        let r = conoti::accept_desktop(&conn, &sid, "ok2 라고만 답해", &[], None).expect("받기");
        let rid = r["rid"].as_str().unwrap().to_string();
        let deliverer = LocalDeliver { bg_resume: false };
        let mut pipe = conoti::Pipeline::new(db::open(&paths::db_path()).unwrap());
        let until = Instant::now() + Duration::from_secs(180);
        let mut seen_wait = false;
        loop {
            println!("route = {:?}", route(&sid));
            if matches!(route(&sid), Route::Busy(_)) {
                seen_wait = true;
            }
            pipe.tick(&deliverer);
            let (st, note): (String, Option<String>) = conn
                .query_row("SELECT state, note FROM conoti_reply WHERE reply_id = ?1", rusqlite::params![rid], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap();
            println!("  {st} {note:?}");
            if st == "delivered" {
                break;
            }
            assert_ne!(st, "rejected", "{note:?}");
            assert!(Instant::now() < until, "시간 초과");
            std::thread::sleep(Duration::from_secs(2));
        }
        println!("waited while busy: {seen_wait}");
        // 대화 기록에 두 번째 말(머리말 포함)과 그 답이 남을 때까지
        let transcript = std::fs::read_dir(paths::projects_dir())
            .unwrap()
            .flatten()
            .map(|d| d.path().join(format!("{sid}.jsonl")))
            .find(|p| p.is_file())
            .expect("대화 기록");
        let until = Instant::now() + Duration::from_secs(120);
        loop {
            let body = std::fs::read_to_string(&transcript).unwrap_or_default();
            let asked = body.contains("ok2 라고만 답해") && body.contains(conoti::INBOX_HEADER);
            let answered = asked && body.rsplit("ok2 라고만 답해").next().map(|t| t.contains("\"type\":\"assistant\"")).unwrap_or(false);
            if answered {
                break;
            }
            assert!(Instant::now() < until, "두 번째 말의 답이 기록되지 않음");
            std::thread::sleep(Duration::from_secs(2));
        }
        // 끝나면 멈춘다(대화는 남는다)
        let until = Instant::now() + Duration::from_secs(60);
        while !matches!(route(&sid), Route::Idle) && Instant::now() < until {
            std::thread::sleep(Duration::from_secs(1));
        }
        stop_session(&sid);
        let _ = std::fs::remove_dir_all(&data);
        println!("ok — transcript {}", transcript.display());
    }
}
