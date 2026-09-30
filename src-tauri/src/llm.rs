//! 모델 호출 — 사용자가 이 컴퓨터에 설치·로그인해 둔 **구독 CLI**(Claude Code `claude -p` · Codex `codex exec`)로 부른다.
//!
//! 이 앱은 API 키를 받지 않는다. 대화 이력 검색(`history.rs`)과 태그 제안(`tags.rs`)이 프롬프트를 만들어 여기로 넘기면,
//! 사용자가 고른(또는 자동 감지한) CLI 를 **임시 빈 작업 폴더**에서, **도구 없이**, **세션을 저장하지 않고** 한 번 돌려 답만 받는다.
//!
//! 지키는 것:
//! - 프롬프트는 표준입력으로만 넘긴다(인자에 대화 내용을 넣지 않는다 — `ps` 로 보인다). 인자에는 모델 이름·고정 옵션·고정 지침만 있다.
//! - 내부 호출 표식: 환경변수 `AI_INBOX_INTERNAL=1` + 작업 폴더 `llm-scratch/`. 훅(`hook`·`wake`·`channel`)은 표식을 보면 바로 끝나고,
//!   수집기는 이 폴더에서 돈 세션(등록부·기록)을 사용자 세션으로 만들지 않는다 — 가짜 세션·태그가 이력 검색 대상이 되는 되먹임을 끊는다.
//! - 격리: Claude 는 `--safe-mode`(훅·MCP·CLAUDE.md·스킬·플러그인 전부 끔 — 구독 로그인은 유지, 실측) + `--tools ""` + `--no-session-persistence`.
//!   Codex 는 `--ephemeral --ignore-user-config --ignore-rules -s read-only`. 전권 우회 옵션은 어디에도 없다.
//! - 오류 문구에 CLI 의 출력(stderr 는 요청 내용을 되풀이한다 — 실측)을 옮기지 않는다. 분류한 고정 문구만.
//! - 하루 호출 상한 · 제한 시간 · 한 번에 하나씩.
//!
//! 정본 설명은 docs/CLEAR.md 의 "모델 호출".

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::Local;
use rusqlite::Connection;
use serde::Serialize;

use crate::{db, paths};

type R<T> = Result<T, String>;

/// 앱이 띄운 모델 호출임을 알리는 환경변수 — 훅 서브커맨드가 보면 아무것도 하지 않고 끝난다
pub const INTERNAL_ENV: &str = "AI_INBOX_INTERNAL";
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-haiku-4-5-20251001";
pub const DEFAULT_CODEX_MODEL: &str = "gpt-6-luna";
/// 하루 호출 상한 — 구독 사용량이 조용히 새지 않게
pub const DAILY_CAP: i64 = 100;
/// 답 글자 상한(넘으면 자른다)
const MAX_OUT: usize = 256 * 1024;

/// 내부 호출인가(훅 서브커맨드용)
pub fn is_internal_env() -> bool {
    std::env::var_os(INTERNAL_ENV).is_some_and(|v| v == "1")
}

// ── 감지 ─────────────────────────────────────────────────────────────────────

#[derive(Serialize, Clone, Default, Debug, PartialEq)]
pub struct CliInfo {
    pub installed: bool,
    pub version: Option<String>,
    /// Some(true) 로그인 · Some(false) 로그인 안 됨 · None 알 수 없음(옛 버전 등 — 시도는 해 본다)
    pub logged_in: Option<bool>,
    /// 로그인 방식(구독인지 API 키인지) — 표시용 짧은 말
    pub login_kind: Option<String>,
    /// Claude 만: `--safe-mode` 를 지원하는 버전인가
    pub safe_mode: bool,
}

impl CliInfo {
    pub fn usable(&self) -> bool {
        self.installed && self.logged_in != Some(false)
    }
}

#[derive(Serialize, Clone, Default, Debug)]
pub struct Detection {
    pub claude: CliInfo,
    pub codex: CliInfo,
    pub at_ms: i64,
}

fn cache() -> &'static Mutex<Option<(Detection, Instant)>> {
    static C: OnceLock<Mutex<Option<(Detection, Instant)>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(None))
}

/// 마지막 감지 결과(없으면 None) — 화면을 그릴 때 기다리지 않게
pub fn last_detection() -> Option<Detection> {
    cache().lock().ok().and_then(|g| g.as_ref().map(|(d, _)| d.clone()))
}

/// 감지 결과. `max_age` 안의 것이 있으면 그대로, 아니면 새로(로그인 확인은 CLI 를 부르므로 1~2초 걸린다)
pub fn detect_cached(max_age: Duration) -> Detection {
    if let Ok(g) = cache().lock() {
        if let Some((d, at)) = g.as_ref() {
            if at.elapsed() < max_age {
                return d.clone();
            }
        }
    }
    detect()
}

pub fn detect() -> Detection {
    let d = Detection { claude: detect_claude(), codex: detect_codex(), at_ms: chrono::Utc::now().timestamp_millis() };
    if let Ok(mut g) = cache().lock() {
        *g = Some((d.clone(), Instant::now()));
    }
    d
}

fn capture_simple(mut c: Command, limit: Duration) -> Option<Captured> {
    c.stdin(Stdio::null());
    exec_cli(c, &[], limit).ok()
}

fn detect_claude() -> CliInfo {
    let mut info = CliInfo::default();
    let Ok(mut c) = crate::deliver::claude_cmd() else { return info };
    info.installed = true;
    c.arg("--version");
    info.version = capture_simple(c, Duration::from_secs(8)).and_then(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().next().map(str::to_string)).filter(|v| v.len() < 40);
    if let Ok(mut h) = crate::deliver::claude_cmd() {
        h.arg("--help");
        info.safe_mode = capture_simple(h, Duration::from_secs(8)).is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains("--safe-mode"));
    }
    if let Ok(mut a) = crate::deliver::claude_cmd() {
        a.args(["auth", "status", "--json"]);
        if let Some(o) = capture_simple(a, Duration::from_secs(10)) {
            (info.logged_in, info.login_kind) = parse_claude_auth(&o.stdout);
        }
    }
    info
}

/// `claude auth status --json` → (로그인 여부, 방식). 이메일·조직은 읽지도 않는다
pub fn parse_claude_auth(stdout: &[u8]) -> (Option<bool>, Option<String>) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(stdout) else { return (None, None) };
    let logged = v.get("loggedIn").and_then(|x| x.as_bool());
    let kind = v.get("authMethod").and_then(|x| x.as_str()).map(|m| match m {
        "claude.ai" => "Claude 구독".to_string(),
        other => other.chars().take(24).collect::<String>(),
    });
    (logged, if logged == Some(true) { kind } else { None })
}

fn detect_codex() -> CliInfo {
    let mut info = CliInfo::default();
    let Ok(mut c) = crate::codex::codex_cmd_for(true) else { return info };
    info.installed = true;
    info.version = crate::codex::version();
    c.args(["login", "status"]);
    if let Some(o) = capture_simple(c, Duration::from_secs(10)) {
        let mut text = String::from_utf8_lossy(&o.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&o.stderr));
        (info.logged_in, info.login_kind) = parse_codex_login(&text, o.code == Some(0));
    }
    info
}

/// `codex login status` 의 문구 → (로그인 여부, 방식). "Logged in using ChatGPT" / "Logged in using an API key" / "Not logged in"
pub fn parse_codex_login(text: &str, ok: bool) -> (Option<bool>, Option<String>) {
    let low = text.to_lowercase();
    if low.contains("not logged in") {
        return (Some(false), None);
    }
    if ok && low.contains("logged in") {
        let kind = if low.contains("chatgpt") {
            "ChatGPT 구독"
        } else if low.contains("api key") {
            "API 키(구독 아님)"
        } else {
            "로그인됨"
        };
        return (Some(true), Some(kind.to_string()));
    }
    (None, None)
}

// ── 설정 ─────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Claude,
    Codex,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Claude => "claude",
            Provider::Codex => "codex",
        }
    }
    pub fn default_model(self) -> &'static str {
        match self {
            Provider::Claude => DEFAULT_CLAUDE_MODEL,
            Provider::Codex => DEFAULT_CODEX_MODEL,
        }
    }
}

pub fn valid_model(m: &str) -> bool {
    !m.is_empty() && m.len() <= 64 && m.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_' || c == '[' || c == ']')
        && !m.starts_with('-')
}

/// 설정한 공급자: auto(기본) | claude | codex
pub fn provider_setting(conn: &Connection) -> String {
    db::get_meta(conn, "setting.llm_provider").filter(|p| matches!(p.as_str(), "auto" | "claude" | "codex")).unwrap_or_else(|| "auto".into())
}

pub fn model_of(conn: &Connection, p: Provider) -> String {
    db::get_meta(conn, &format!("setting.llm_model_{}", p.name())).filter(|m| valid_model(m)).unwrap_or_else(|| p.default_model().to_string())
}

/// 지금 쓸 공급자. auto 는 로그인된 것 중 Claude 먼저
pub fn resolve(conn: &Connection, det: &Detection) -> Option<Provider> {
    match provider_setting(conn).as_str() {
        "claude" => det.claude.usable().then_some(Provider::Claude),
        "codex" => det.codex.usable().then_some(Provider::Codex),
        _ => {
            if det.claude.usable() && det.claude.logged_in == Some(true) {
                Some(Provider::Claude)
            } else if det.codex.usable() && det.codex.logged_in == Some(true) {
                Some(Provider::Codex)
            } else if det.claude.usable() {
                Some(Provider::Claude)
            } else if det.codex.usable() {
                Some(Provider::Codex)
            } else {
                None
            }
        }
    }
}

/// 설정 한 칸: provider(auto|claude|codex) · model_claude · model_codex
pub fn set(conn: &Connection, key: &str, value: &str) -> R<()> {
    let v = value.trim();
    match key {
        "provider" => {
            if !matches!(v, "auto" | "claude" | "codex") {
                return Err("auto · claude · codex 중 하나".into());
            }
            db::set_meta(conn, "setting.llm_provider", v).map_err(|e| e.to_string())
        }
        "model_claude" | "model_codex" => {
            if !v.is_empty() && !valid_model(v) {
                return Err("모델 이름 형식이 올바르지 않습니다".into());
            }
            // 빈 값 = 기본값으로 되돌림
            db::set_meta(conn, &format!("setting.llm_{key}"), v).map_err(|e| e.to_string())
        }
        _ => Err(format!("알 수 없는 설정: {key}")),
    }
}

// ── 호출 횟수 ────────────────────────────────────────────────────────────────

fn today_key() -> String {
    format!("history.calls.{}", Local::now().format("%Y-%m-%d"))
}

pub fn calls_today(conn: &Connection) -> i64 {
    db::get_meta(conn, &today_key()).and_then(|v| v.parse().ok()).unwrap_or(0)
}

// ── 호출 ─────────────────────────────────────────────────────────────────────

pub struct Completion {
    pub text: String,
    pub model: String,
}

fn gate() -> &'static Mutex<()> {
    static G: OnceLock<Mutex<()>> = OnceLock::new();
    G.get_or_init(|| Mutex::new(()))
}

/// 하루 상한을 세면서 모델을 한 번 부른다. `system` 은 고정 지침(사용자 글 금지), `prompt` 는 표준입력으로 간다.
pub fn complete(conn: &Connection, system: &str, prompt: &str, timeout: Duration) -> R<Completion> {
    if calls_today(conn) >= DAILY_CAP {
        return Err(format!("오늘 호출 한도({DAILY_CAP}회)에 도달했습니다"));
    }
    let det = detect_cached(Duration::from_secs(300));
    let Some(provider) = resolve(conn, &det) else {
        return Err("사용할 수 있는 구독 CLI 가 없습니다 — Claude Code 또는 Codex CLI 를 설치하고 로그인하세요".into());
    };
    let model = model_of(conn, provider);
    let _g = gate().lock().map_err(|_| "내부 오류".to_string())?;
    // 세는 것은 부르기 전에(오류로 끝나도 구독 사용량은 썼을 수 있다)
    let n = calls_today(conn) + 1;
    let _ = db::set_meta(conn, &today_key(), &n.to_string());
    let text = match provider {
        Provider::Claude => run_claude(crate::deliver::claude_cmd()?, &model, system, prompt, det.claude.safe_mode, timeout)?,
        Provider::Codex => run_codex(crate::codex::codex_cmd_for(true)?, &model, system, prompt, timeout)?,
    };
    Ok(Completion { text, model })
}

/// Claude Code 인자 — 도구 없음 · 세션 저장 없음 · 모든 사용자 설정·훅·MCP 끔. 대화 내용은 여기 없다
pub fn claude_args(model: &str, system: &str, safe_mode: bool) -> Vec<String> {
    let mut a: Vec<String> = vec!["-p".into()];
    if safe_mode {
        a.push("--safe-mode".into());
    } else {
        // 옛 버전 — 가능한 만큼 끈다(훅은 내부 호출 표식을 보고 스스로 끝난다)
        a.extend(["--strict-mcp-config".into(), "--disable-slash-commands".into()]);
    }
    a.extend(["--tools".into(), String::new(), "--no-session-persistence".into(), "--output-format".into(), "text".into()]);
    a.extend(["--model".into(), model.into(), "--system-prompt".into(), system.into()]);
    a
}

/// Codex 인자 — 읽기 전용 샌드박스 · 세션 저장 없음 · 사용자 설정·규칙 무시 · 마지막 글만 파일로
pub fn codex_args(model: &str, scratch: &Path, out: &Path) -> Vec<String> {
    let s = |p: &Path| p.to_string_lossy().to_string();
    let mut a: Vec<String> = ["exec", "--ephemeral", "--ignore-user-config", "--ignore-rules", "--skip-git-repo-check", "-s", "read-only", "--color", "never"].map(String::from).to_vec();
    a.extend(["-C".into(), s(scratch), "-m".into(), model.into(), "-c".into(), "model_reasoning_effort=\"low\"".into(), "-o".into(), s(out), "-".into()]);
    a
}

fn scratch() -> R<PathBuf> {
    let dir = paths::llm_scratch_dir();
    if let Some(parent) = dir.parent() {
        paths::ensure_private_dir(parent);
    }
    paths::ensure_private_dir(&dir);
    Ok(dir)
}

pub(crate) fn run_claude(cmd: Command, model: &str, system: &str, prompt: &str, safe_mode: bool, timeout: Duration) -> R<String> {
    run_claude_in(&scratch()?, cmd, model, system, prompt, safe_mode, timeout)
}

fn run_claude_in(dir: &Path, mut cmd: Command, model: &str, system: &str, prompt: &str, safe_mode: bool, timeout: Duration) -> R<String> {
    if !valid_model(model) {
        return Err("모델 이름 형식이 올바르지 않습니다".into());
    }
    cmd.args(claude_args(model, system, safe_mode)).current_dir(dir).env(INTERNAL_ENV, "1");
    let out = exec_cli(cmd, prompt.as_bytes(), timeout).map_err(exec_msg)?;
    finish(out, None)
}

pub(crate) fn run_codex(cmd: Command, model: &str, system: &str, prompt: &str, timeout: Duration) -> R<String> {
    run_codex_in(&scratch()?, cmd, model, system, prompt, timeout)
}

fn run_codex_in(dir: &Path, mut cmd: Command, model: &str, system: &str, prompt: &str, timeout: Duration) -> R<String> {
    if !valid_model(model) {
        return Err("모델 이름 형식이 올바르지 않습니다".into());
    }
    let out_file = dir.join(format!("out-{}-{}.txt", std::process::id(), chrono::Utc::now().timestamp_millis()));
    cmd.args(codex_args(model, dir, &out_file)).current_dir(dir).env(INTERNAL_ENV, "1");
    // Codex 에는 지침 칸이 따로 없다 — 한 덩어리로 표준입력에
    let body = format!("{system}\n\n---\n\n{prompt}");
    let res = exec_cli(cmd, body.as_bytes(), timeout).map_err(exec_msg);
    let text = std::fs::read_to_string(&out_file).ok();
    let _ = std::fs::remove_file(&out_file);
    finish(res?, text)
}

/// 종료 코드·답을 검사한다. `file_text` 가 있으면 그것이 답(Codex `-o`)
fn finish(out: Captured, file_text: Option<String>) -> R<String> {
    let mut answer = match file_text {
        Some(t) => t,
        None => String::from_utf8_lossy(&out.stdout).to_string(),
    };
    if out.code != Some(0) {
        let mut hay = String::from_utf8_lossy(&out.stdout).to_lowercase();
        hay.push('\n');
        hay.push_str(&String::from_utf8_lossy(&out.stderr).to_lowercase());
        return Err(classify(&hay, out.code));
    }
    answer = answer.trim().to_string();
    if answer.is_empty() {
        return Err("모델이 빈 답을 보냈습니다".into());
    }
    if answer.len() > MAX_OUT {
        answer = crate::text::clip(&answer, MAX_OUT);
    }
    Ok(answer)
}

/// 실패 원인을 고정 문구로 — CLI 가 낸 글(요청 내용이 섞일 수 있다)은 옮기지 않는다
pub fn classify(hay_lower: &str, code: Option<i32>) -> String {
    let has = |ws: &[&str]| ws.iter().any(|w| hay_lower.contains(w));
    if has(&["not logged in", "please run /login", "unauthorized", "401", "authentication", "log in", "login required"]) {
        "로그인이 필요합니다 — 터미널에서 다시 로그인하세요(claude · codex login)".into()
    } else if has(&["rate limit", "usage limit", "429", "quota", "limit reached", "too many requests"]) {
        "구독 사용 한도에 걸렸습니다 — 잠시 뒤에 다시 시도하세요".into()
    } else if hay_lower.contains("model") && has(&["not found", "unknown", "invalid", "does not exist", "not supported", "unavailable", "not available"]) {
        "모델을 쓸 수 없습니다 — 설정에서 모델 이름을 바꾸세요".into()
    } else {
        format!("모델 호출이 실패했습니다(종료 코드 {})", code.map(|c| c.to_string()).unwrap_or_else(|| "-".into()))
    }
}

// ── 프로세스 실행 ────────────────────────────────────────────────────────────

pub struct Captured {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

pub enum ExecErr {
    Spawn,
    Timeout,
}

fn exec_msg(e: ExecErr) -> String {
    match e {
        ExecErr::Spawn => "모델 CLI 를 실행하지 못했습니다".into(),
        ExecErr::Timeout => "모델이 제한 시간 안에 답하지 않았습니다".into(),
    }
}

/// 출력을 상한까지 모으는 읽기 스레드. 자식이 남긴 손자 프로세스가 파이프를 쥐고 있어도 `collect` 가 오래 기다리지 않게 공유 버퍼를 쓴다
struct Reader {
    buf: std::sync::Arc<Mutex<Vec<u8>>>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn read_capped(mut r: impl Read + Send + 'static, cap: usize) -> Reader {
    let buf = std::sync::Arc::new(Mutex::new(Vec::new()));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (b2, d2) = (buf.clone(), done.clone());
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match r.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut b) = b2.lock() {
                        if b.len() < cap {
                            let take = n.min(cap - b.len());
                            b.extend_from_slice(&chunk[..take]);
                        }
                    }
                    // 넘치는 분은 버리되 계속 읽어 자식이 막히지 않게 한다
                }
            }
        }
        d2.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    Reader { buf, done }
}

impl Reader {
    /// 읽기가 끝나기를 잠깐(최대 `wait`) 기다린 뒤 모인 만큼 가져온다
    fn collect(self, wait: Duration) -> Vec<u8> {
        let until = Instant::now() + wait;
        while !self.done.load(std::sync::atomic::Ordering::SeqCst) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        self.buf.lock().map(|b| b.clone()).unwrap_or_default()
    }
}

/// 명령을 돌린다: 표준입력에 `input` 을 쓰고 닫고, 제한 시간을 넘기면 죽인다. 출력은 상한까지만 모은다
pub fn exec_cli(mut cmd: Command, input: &[u8], limit: Duration) -> Result<Captured, ExecErr> {
    crate::deliver::no_console(&mut cmd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // 자기 프로세스 그룹에서 돌려, 시간 초과 때 CLI 가 띄운 하위 프로세스까지 함께 끝낸다
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut cmd, 0);
    let mut child = cmd.spawn().map_err(|_| ExecErr::Spawn)?;
    let stdin = child.stdin.take();
    let data = input.to_vec();
    let writer = std::thread::spawn(move || {
        if let Some(mut s) = stdin {
            let _ = s.write_all(&data); // 자식이 먼저 끝나면 실패해도 된다
        }
    });
    let out = child.stdout.take().map(|o| read_capped(o, 4 * MAX_OUT));
    let err = child.stderr.take().map(|e| read_capped(e, 64 * 1024));
    let until = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(40)),
            _ => {
                #[cfg(unix)]
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    // 손자 프로세스가 파이프를 쥐고 있어도 오래 붙잡히지 않는다
    let stdout = out.map(|r| r.collect(Duration::from_secs(2))).unwrap_or_default();
    let stderr = err.map(|r| r.collect(Duration::from_millis(500))).unwrap_or_default();
    drop(writer);
    match status {
        Some(s) => Ok(Captured { code: s.code(), stdout, stderr }),
        None => Err(ExecErr::Timeout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_auth_parsing_hides_identity() {
        let (l, k) = parse_claude_auth(br#"{"loggedIn":true,"authMethod":"claude.ai","email":"a@b.c","orgName":"x"}"#);
        assert_eq!((l, k.as_deref()), (Some(true), Some("Claude 구독")));
        assert_eq!(parse_claude_auth(br#"{"loggedIn":false}"#), (Some(false), None));
        assert_eq!(parse_claude_auth(b"not json"), (None, None));
    }

    #[test]
    fn codex_login_parsing() {
        assert_eq!(parse_codex_login("Logged in using ChatGPT", true), (Some(true), Some("ChatGPT 구독".into())));
        assert_eq!(parse_codex_login("Logged in using an API key - sk-***", true).1.as_deref(), Some("API 키(구독 아님)"));
        assert_eq!(parse_codex_login("Not logged in", false), (Some(false), None));
        assert_eq!(parse_codex_login("???", false), (None, None));
    }

    #[test]
    fn resolve_prefers_logged_in_and_respects_choice() {
        let c = Connection::open_in_memory().unwrap();
        db::migrate(&c).unwrap();
        let ok = |l| CliInfo { installed: true, logged_in: l, ..Default::default() };
        let none = CliInfo::default();
        let d = |cl: &CliInfo, cx: &CliInfo| Detection { claude: cl.clone(), codex: cx.clone(), at_ms: 0 };
        assert_eq!(resolve(&c, &d(&ok(Some(true)), &ok(Some(true)))), Some(Provider::Claude));
        assert_eq!(resolve(&c, &d(&ok(Some(false)), &ok(Some(true)))), Some(Provider::Codex), "로그인 안 된 쪽은 건너뛴다");
        assert_eq!(resolve(&c, &d(&none, &none)), None, "둘 다 없으면 모델 없이 찾기만");
        set(&c, "provider", "codex").unwrap();
        assert_eq!(resolve(&c, &d(&ok(Some(true)), &ok(Some(true)))), Some(Provider::Codex));
        assert_eq!(resolve(&c, &d(&ok(Some(true)), &ok(Some(false)))), None, "고른 쪽이 로그인 안 됐으면 다른 쪽으로 몰래 넘어가지 않는다");
        assert!(set(&c, "provider", "openai").is_err());
        assert!(set(&c, "model_claude", "haiku; rm -rf").is_err() && set(&c, "model_claude", "--evil").is_err());
        set(&c, "model_codex", "gpt-6-sol").unwrap();
        assert_eq!(model_of(&c, Provider::Codex), "gpt-6-sol");
        set(&c, "model_codex", "").unwrap();
        assert_eq!(model_of(&c, Provider::Codex), DEFAULT_CODEX_MODEL, "빈 값은 기본으로");
    }

    #[test]
    fn args_hold_no_conversation_and_no_bypass() {
        let a = claude_args("claude-haiku-4-5-20251001", "고정 지침", true);
        assert!(a.contains(&"--safe-mode".to_string()) && a.contains(&"--no-session-persistence".to_string()));
        let i = a.iter().position(|x| x == "--tools").unwrap();
        assert_eq!(a[i + 1], "", "도구 전부 끔");
        assert!(!a.iter().any(|x| x.contains("dangerously") || x.contains("bypass") || x == "--permission-mode"));
        let b = codex_args("gpt-6-luna", Path::new("/s"), Path::new("/s/o.txt"));
        for must in ["--ephemeral", "--ignore-user-config", "--ignore-rules", "read-only"] {
            assert!(b.iter().any(|x| x == must), "{must}");
        }
        assert_eq!(b.last().map(String::as_str), Some("-"), "프롬프트는 표준입력");
        assert!(!b.iter().any(|x| x.contains("dangerously") || x.contains("bypass") || x.contains("danger-full")));
    }

    #[test]
    fn classify_never_echoes_cli_text() {
        let m = classify("error: secret-question-text not logged in", Some(1));
        assert!(m.contains("로그인") && !m.contains("secret"));
        assert!(classify("http 429 too many requests", Some(1)).contains("한도"));
        assert!(classify("model foo not found", Some(1)).contains("모델"));
        let g = classify("boom with secret-question-text", Some(3));
        assert!(g.contains("종료 코드 3") && !g.contains("secret"));
    }

    #[test]
    fn scratch_cwd_is_recognised() {
        let d = paths::llm_scratch_dir();
        let s = d.to_string_lossy().to_string();
        assert!(paths::is_internal_cwd(&s));
        assert!(paths::is_internal_cwd(&format!("{s}/sub")));
        assert!(!paths::is_internal_cwd(&format!("{s}-other")));
        assert!(!paths::is_internal_cwd("/w/proj"));
    }

    /// 실제 구독 CLI 한 번씩 왕복(아주 짧은 무해한 프롬프트 — 사용량 최소): `cargo test live_llm -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn live_llm_roundtrip() {
        let d = detect();
        eprintln!("claude={:?}\ncodex={:?}", d.claude, d.codex);
        let dir = std::env::temp_dir().join(format!("aiinbox-live-{}", std::process::id())).join("llm-scratch");
        std::fs::create_dir_all(&dir).unwrap();
        if d.claude.usable() {
            let t = Instant::now();
            let r = run_claude_in(&dir, crate::deliver::claude_cmd().unwrap(), DEFAULT_CLAUDE_MODEL, "짧게 답한다.", "OK 라고만 답해", d.claude.safe_mode, Duration::from_secs(90));
            eprintln!("claude {:?} {} ms", r, t.elapsed().as_millis());
            assert!(r.is_ok());
        }
        if d.codex.usable() {
            let t = Instant::now();
            let r = run_codex_in(&dir, crate::codex::codex_cmd_for(true).unwrap(), DEFAULT_CODEX_MODEL, "짧게 답한다.", "OK 라고만 답해", Duration::from_secs(90));
            eprintln!("codex {:?} {} ms", r, t.elapsed().as_millis());
            assert!(r.is_ok());
        }
        let _ = std::fs::remove_dir_all(dir.parent().unwrap());
    }

    #[cfg(unix)]
    mod fake {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn script(name: &str, body: &str) -> (PathBuf, PathBuf) {
            let dir = std::env::temp_dir().join(format!("aiinbox-llm-{}-{}", std::process::id(), name));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let exe = dir.join("fake-cli");
            std::fs::write(&exe, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
            (exe, dir)
        }

        /// 작업 폴더는 시험 폴더 안의 `llm-scratch` — 실제 데이터 폴더를 건드리지 않는다
        fn with_data<T>(f: impl FnOnce(&Path) -> T) -> T {
            let d = std::env::temp_dir().join(format!("aiinbox-llm-scratch-{}", std::process::id())).join("llm-scratch");
            std::fs::create_dir_all(&d).unwrap();
            f(&d)
        }
        fn claude(sc: &Path, exe: &Path, model: &str, s: &str, p: &str, to: Duration) -> R<String> {
            run_claude_in(sc, Command::new(exe), model, s, p, true, to)
        }

        #[test]
        fn claude_prompt_goes_through_stdin_only_with_marker_env() {
            with_data(|sc| {
                let (exe, dir) = script("claude", &format!("cat > \"{d}/stdin.txt\"\nprintf '%s\\n' \"$@\" > \"{d}/args.txt\"\necho \"$AI_INBOX_INTERNAL:$(pwd)\" > \"{d}/env.txt\"\necho 답입니다", d = "$(dirname \"$0\")"));
                let secret = "SECRET-대화-내용-12345";
                let out = claude(sc, &exe, "claude-haiku-4-5-20251001", "지침", &format!("발췌 {secret}"), Duration::from_secs(10)).unwrap();
                assert_eq!(out, "답입니다");
                let stdin = std::fs::read_to_string(dir.join("stdin.txt")).unwrap();
                assert!(stdin.contains(secret), "본문은 표준입력으로");
                let args = std::fs::read_to_string(dir.join("args.txt")).unwrap();
                assert!(!args.contains(secret), "인자(ps 에 보인다)에는 대화 내용이 없다");
                assert!(args.contains("--safe-mode") && args.contains("--no-session-persistence"));
                let env = std::fs::read_to_string(dir.join("env.txt")).unwrap();
                assert!(env.starts_with("1:") && env.contains("llm-scratch"), "{env}");
                let _ = std::fs::remove_dir_all(&dir);
            });
        }

        #[test]
        fn codex_reads_last_message_file_and_cleans_up() {
            with_data(|sc| {
                let (exe, dir) = script(
                    "codex",
                    "cat > \"$(dirname \"$0\")/stdin.txt\"\nwhile [ $# -gt 0 ]; do if [ \"$1\" = \"-o\" ]; then shift; printf '%s' '마지막 글' > \"$1\"; fi; shift; done\necho '지저분한 로그 SECRET' >&2",
                );
                let out = run_codex_in(sc, Command::new(&exe), "gpt-6-luna", "지침", "발췌", Duration::from_secs(10)).unwrap();
                assert_eq!(out, "마지막 글");
                let stdin = std::fs::read_to_string(dir.join("stdin.txt")).unwrap();
                assert!(stdin.contains("지침") && stdin.contains("발췌"));
                let left: Vec<_> = std::fs::read_dir(sc).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("out-")).collect();
                assert!(left.is_empty(), "임시 출력 파일은 지운다");
                let _ = std::fs::remove_dir_all(&dir);
            });
        }

        #[test]
        fn timeout_kills_the_process_and_errors_are_fixed_phrases() {
            with_data(|sc| {
                let (exe, dir) = script("slow", "sleep 30");
                let t = Instant::now();
                let e = claude(sc, &exe, "haiku", "s", "p", Duration::from_millis(600)).unwrap_err();
                assert!(e.contains("제한 시간") && t.elapsed() < Duration::from_secs(5), "{e}");
                let (exe2, dir2) = script("fail", "cat >/dev/null; echo 'Not logged in · Please run /login SECRET-발췌'; exit 1");
                let e2 = claude(sc, &exe2, "haiku", "s", "p", Duration::from_secs(5)).unwrap_err();
                assert!(e2.contains("로그인") && !e2.contains("SECRET"), "{e2}");
                let (exe3, dir3) = script("empty", "cat >/dev/null; exit 0");
                assert!(claude(sc, &exe3, "haiku", "s", "p", Duration::from_secs(5)).unwrap_err().contains("빈 답"));
                let missing = claude(sc, Path::new("/nonexistent/cli"), "haiku", "s", "p", Duration::from_secs(2)).unwrap_err();
                assert!(missing.contains("실행하지 못"));
                for d in [dir, dir2, dir3] {
                    let _ = std::fs::remove_dir_all(d);
                }
            });
        }

        #[test]
        fn huge_output_is_capped_and_child_does_not_block() {
            with_data(|sc| {
                let (exe, dir) = script("big", "cat >/dev/null; head -c 3000000 /dev/zero | tr '\\0' 'a'");
                let out = claude(sc, &exe, "haiku", "s", "p", Duration::from_secs(20)).unwrap();
                assert!(out.len() <= MAX_OUT + 8);
                let _ = std::fs::remove_dir_all(&dir);
            });
        }

        #[test]
        fn model_name_cannot_smuggle_options() {
            with_data(|sc| {
                let (exe, dir) = script("noop", "cat >/dev/null; echo ok");
                assert!(claude(sc, &exe, "--dangerously-skip-permissions", "s", "p", Duration::from_secs(5)).is_err());
                assert!(run_codex_in(sc, Command::new(&exe), "x y", "s", "p", Duration::from_secs(5)).is_err());
                let _ = std::fs::remove_dir_all(&dir);
            });
        }
    }
}
