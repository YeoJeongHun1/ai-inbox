//! OpenAI Codex CLI 연동 — 세션(스레드)이 지금 열려 있는지 보고, 말을 넣고, 새 작업을 띄운다.
//! 대화 기록을 turn 으로 모으는 쪽은 `ingest_codex.rs` 다.
//!
//! 모두 2026-09-28 codex-cli 0.156 으로 실측한 동작이다.
//!   - **열려 있음**: 스레드를 연 프로세스(TUI·데스크톱 앱·exec)가 `thread-writer-locks/<스레드>.lock` 에 쓰기 잠금을 건다.
//!     파일은 닫은 뒤에도 남으므로 "있는가"가 아니라 "잠겼는가"를 본다. macOS 는 `F_GETLK` 로 **잡지 않고** 본다
//!     (flock 잠금이 `F_WRLCK`·pid -1 로 보인다). 잠금을 잠깐이라도 잡으면 그 순간 스레드를 열려던 Codex 가 실패할 수 있다.
//!   - **열린 세션이 쉬는 중** → `codex queue --thread <id> --message <말>` — 대기열(SQLite)에 쓰면 열린 TUI 가 곧바로 가져가
//!     새 턴으로 처리한다(쉬는 중에 넣어도, 열 때 쌓여 있어도). 일하는 중이면 Claude 쪽과 같이 끝날 때까지 기다렸다 넣는다.
//!   - **꺼진 세션** → `codex exec resume <id> <말>` 을 백그라운드로. 같은 기록 파일에 이어 쓰고, 대기열에 남은 말도 같이 처리한다.
//!     샌드박스는 그 스레드가 쓰던 값을 이어받되 **전권(danger-full-access)·알 수 없음이면 workspace-write 로 낮춘다**
//!     (`-c sandbox_mode=…` — resume 은 --sandbox 를 받지 않는다). 승인 정책은 `never`(물을 사람이 없다). 권한을 넓히는 옵션은 주지 않는다.
//!   - **새 작업** → `codex exec --json <말>` — 첫 줄 `thread.started` 의 `thread_id` 가 세션 ID.
//!   - Git 저장소도 신뢰한 폴더도 아니면 exec 가 거절한다 — 사용자가 고른 폴더·이미 그 폴더에서 돌던 세션이므로 `--skip-git-repo-check` 를 준다.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::paths;

/// 세션 표의 `agent` 값
pub const AGENT: &str = "codex";

// ── 실행 파일 ─────────────────────────────────────────────────────────────────

static FOUND: Mutex<Option<(Option<PathBuf>, Instant)>> = Mutex::new(None);

/// Finder 로 띄운 앱은 PATH 가 짧다 — 흔한 설치 위치를 먼저 보고, 없으면 로그인 셸에 묻는다(1분 캐시).
pub fn find_codex() -> Option<PathBuf> {
    if let Ok(g) = FOUND.lock() {
        if let Some((p, at)) = g.as_ref() {
            if at.elapsed() < Duration::from_secs(60) {
                return p.clone();
            }
        }
    }
    let found = locate();
    if let Ok(mut g) = FOUND.lock() {
        *g = Some((found.clone(), Instant::now()));
    }
    found
}

fn locate() -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        // 개발 빌드의 시험용만(paths.rs 의 데이터 폴더 바꾸기와 같은 규칙)
        if let Ok(p) = std::env::var("AI_INBOX_CODEX_BIN") {
            let p = PathBuf::from(p);
            if p.is_absolute() && p.is_file() {
                return Some(p);
            }
        }
    }
    let home = dirs::home_dir().unwrap_or_default();
    let cands: Vec<PathBuf> = if cfg!(windows) {
        // 네이티브 exe 를 먼저 — npm 래퍼(.cmd)로는 여러 줄 인자를 넘길 수 없다(codex_cmd)
        let mut v = vec![home.join(".local/bin/codex.exe"), home.join("AppData/Local/Programs/codex/codex.exe")];
        if let Some(exe) = find_exe_under(&home.join("AppData/Roaming/npm/node_modules/@openai/codex"), 0) {
            v.push(exe);
        }
        v.push(home.join("AppData/Roaming/npm/codex.cmd"));
        v
    } else {
        vec![
            PathBuf::from("/opt/homebrew/bin/codex"),
            PathBuf::from("/usr/local/bin/codex"),
            home.join(".local/bin/codex"),
            home.join(".npm-global/bin/codex"),
            home.join(".bun/bin/codex"),
            home.join(".volta/bin/codex"),
        ]
    };
    if let Some(p) = cands.into_iter().find(|p| p.is_file()) {
        return Some(p);
    }
    #[cfg(unix)]
    {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut c = Command::new(shell);
        c.args(["-lc", "command -v codex"]).stdin(Stdio::null());
        let out = crate::deliver::output_within(c, Duration::from_secs(8))?;
        let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
        if p.is_absolute() && p.is_file() {
            return Some(p);
        }
    }
    None
}

/// npm 패키지 안의 네이티브 codex 실행 파일 (깊이 6까지). 같은 폴더에 `codex-code-mode-host.exe`·`codex-command-runner.exe`
/// 같은 보조 exe 가 함께 들어 있어 「codex 로 시작하는 첫 파일」로 고르면 디렉터리 순서에 따라 엉뚱한 것을 부른다 — 이름으로 고른다
fn find_exe_under(dir: &Path, depth: usize) -> Option<PathBuf> {
    let mut best: Option<(u8, PathBuf)> = None;
    collect_exe(dir, depth, &mut best);
    best.map(|(_, p)| p)
}

fn collect_exe(dir: &Path, depth: usize, best: &mut Option<(u8, PathBuf)>) {
    if depth > 6 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_exe(&p, depth + 1, best);
        } else if let Some(r) = p.file_name().and_then(|n| n.to_str()).and_then(exe_rank) {
            if best.as_ref().is_none_or(|(b, _)| r < *b) {
                *best = Some((r, p));
            }
        }
    }
}

/// 0 = `codex.exe`(지금 npm 패키지), 1 = `codex-<대상 삼중항>.exe`(예전 npm 패키지, 예 `codex-x86_64-pc-windows-msvc.exe`).
/// 그 밖의 `codex-*.exe`(보조 실행 파일)는 후보가 아니다
fn exe_rank(name: &str) -> Option<u8> {
    let n = name.to_ascii_lowercase();
    if n == "codex.exe" {
        return Some(0);
    }
    let triple = n.strip_prefix("codex-")?.strip_suffix(".exe")?;
    triple.ends_with("-pc-windows-msvc").then_some(1)
}

/// `codex --version` → "codex-cli 0.156.0" 의 버전 부분
pub fn version() -> Option<String> {
    let bin = find_codex()?;
    let mut c = Command::new(bin);
    c.arg("--version").stdin(Stdio::null());
    let out = crate::deliver::output_within(c, Duration::from_secs(8))?;
    let s = String::from_utf8_lossy(&out.stdout);
    let v = s.split_whitespace().last()?.trim();
    (!v.is_empty() && v.len() < 40).then(|| v.to_string())
}

/// 이 앱이 Claude Code 나 Codex 안에서 띄워졌으면 그 세션의 환경변수가 따라온다 — 새 Codex 가 자기를 자식으로 착각하거나
/// 그 세션의 통로(메시징 토큰 등)를 물려받지 않게 `CLAUDE*`·`CODEX_*` 를 모두 지운다. 사용자 설정·인증에 쓰는 것만 남긴다
/// (`CODEX_SQLITE_HOME` 을 지우면 대기열이 열린 TUI 와 다른 DB 에 들어간다).
const KEEP_CODEX_ENV: &[&str] = &["CODEX_HOME", "CODEX_SQLITE_HOME", "CODEX_CA_CERTIFICATE", "CODEX_API_KEY", "CODEX_ACCESS_TOKEN"];

fn inherited(key: &str) -> bool {
    key.starts_with("CLAUDE") || (key.starts_with("CODEX_") && !KEEP_CODEX_ENV.contains(&key))
}

fn codex_cmd() -> Result<Command, String> {
    codex_cmd_for(false)
}

/// `stdin_prompt` — 말을 인자가 아닌 표준입력으로 넘기는 호출(모델 조회)은 npm 래퍼(.cmd)로도 된다(줄바꿈이 인자에 없다)
pub(crate) fn codex_cmd_for(stdin_prompt: bool) -> Result<Command, String> {
    let bin = find_codex().ok_or("codex 실행 파일을 찾지 못함 — Codex CLI 를 설치했는지 확인하세요")?;
    if cfg!(windows) && !stdin_prompt && bin.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat")) {
        // Rust 는 .cmd/.bat 에 줄바꿈이 든 인자를 넘기지 않는다(InvalidInput) — 넣는 말은 항상 여러 줄이다
        return Err("Windows 에서는 npm 래퍼(codex.cmd)로 여러 줄 말을 넘길 수 없습니다 — codex.exe 가 있는 설치가 필요합니다".into());
    }
    let mut c = Command::new(&bin);
    crate::deliver::no_console(&mut c);
    for (k, _) in std::env::vars_os() {
        if k.to_str().is_some_and(inherited) {
            c.env_remove(k);
        }
    }
    // Finder 로 띄운 앱은 PATH 가 짧다 — npm 설치(#!/usr/bin/env node)가 node 를 찾게 흔한 위치와 codex 폴더를 앞에 붙인다
    #[cfg(unix)]
    {
        let mut path = std::env::var("PATH").unwrap_or_default();
        for extra in ["/usr/local/bin", "/opt/homebrew/bin"] {
            if !path.split(':').any(|p| p == extra) {
                path = format!("{extra}:{path}");
            }
        }
        if let Some(dir) = bin.parent().and_then(|d| d.to_str()) {
            path = format!("{dir}:{path}");
        }
        c.env("PATH", path);
    }
    c.stdin(Stdio::null());
    Ok(c)
}

// ── 이 세션이 Codex 것인가 ─────────────────────────────────────────────────────

/// 세션 → Codex 여부. 수집기가 Codex 세션을 저장할 때 알려 주고(`remember`), 모르는 세션은 DB 를 한 번 본다.
/// Claude 세션(아니오)은 10초만 기억한다 — 막 시작한 Codex 세션이 아직 DB 에 없을 수 있어서.
fn agents() -> &'static Mutex<HashMap<String, (bool, Instant)>> {
    static A: OnceLock<Mutex<HashMap<String, (bool, Instant)>>> = OnceLock::new();
    A.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn remember(session_id: &str) {
    if let Ok(mut m) = agents().lock() {
        m.insert(session_id.to_string(), (true, Instant::now()));
    }
}

pub fn is_codex(session_id: &str) -> bool {
    if let Ok(m) = agents().lock() {
        if let Some((yes, at)) = m.get(session_id) {
            if *yes || at.elapsed() < Duration::from_secs(10) {
                return *yes;
            }
        }
    }
    let db = paths::db_path();
    // DB 가 없으면(첫 실행) 만들지 않는다 · 시험은 데이터 폴더를 바꿔 둔 경우에만 DB 를 본다(실제 DB 를 열지 않게)
    let usable = db.exists() && (!cfg!(test) || paths::has_data_dir_override());
    let yes = (usable.then(|| crate::db::open(&db).ok()).flatten())
        .and_then(|c| {
            c.query_row("SELECT agent FROM session WHERE id = ?1", [session_id], |r| r.get::<_, Option<String>>(0))
                .ok()
                .flatten()
        })
        .as_deref()
        == Some(AGENT);
    if let Ok(mut m) = agents().lock() {
        if m.len() > 5000 {
            m.retain(|_, (y, _)| *y);
        }
        m.insert(session_id.to_string(), (yes, Instant::now()));
    }
    yes
}

// ── 세션이 열려 있나 ──────────────────────────────────────────────────────────

fn lock_path(thread_id: &str) -> PathBuf {
    paths::codex_locks_dir().join(format!("{thread_id}.lock"))
}

/// 이 스레드를 지금 어떤 Codex 프로세스가 열고 있나. 알 수 없는 플랫폼이면 None.
pub fn thread_live(thread_id: &str) -> Option<bool> {
    if !crate::channel::valid_session_id(thread_id) {
        return Some(false);
    }
    if !paths::codex_locks_dir().is_dir() {
        // 잠금 폴더가 없는 옛 Codex — 열려 있는지 알 수 없다(꺼진 줄 알고 두 번째 작성자로 붙지 않게)
        return None;
    }
    locked(&lock_path(thread_id))
}

/// 지금 열려 있는 스레드 전부(잠금 파일 폴더를 한 번 훑는다)
pub fn live_threads() -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(rd) = std::fs::read_dir(paths::codex_locks_dir()) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("lock") {
            continue;
        }
        let Some(id) = p.file_stem().and_then(|s| s.to_str()) else { continue };
        if !crate::channel::valid_session_id(id) {
            continue; // .coordination.lock 등
        }
        if locked(&p) == Some(true) {
            out.insert(id.to_string());
        }
    }
    out
}

/// macOS: `F_GETLK` — 잠금을 잡지 않고 누가 잡고 있는지만 묻는다(flock 잠금도 여기 보인다).
#[cfg(target_os = "macos")]
fn locked(path: &Path) -> Option<bool> {
    use std::os::unix::io::AsRawFd;
    let Ok(f) = std::fs::File::open(path) else { return Some(false) };
    // SAFETY: flock 구조체를 0 으로 채운 뒤 전체 범위의 쓰기 잠금을 묻는다. fd 는 f 가 살아 있는 동안 유효하다.
    let mut fl: libc::flock = unsafe { std::mem::zeroed() };
    fl.l_type = libc::F_WRLCK as libc::c_short;
    fl.l_whence = libc::SEEK_SET as libc::c_short;
    fl.l_start = 0;
    fl.l_len = 0;
    let r = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETLK, &mut fl) };
    if r != 0 {
        return None;
    }
    Some(fl.l_type != libc::F_UNLCK as libc::c_short)
}

/// Linux: flock 과 fcntl 잠금이 따로라 F_GETLK 로는 안 보인다 — 공유 잠금을 시도해 보고 곧바로 푼다.
#[cfg(all(unix, not(target_os = "macos")))]
fn locked(path: &Path) -> Option<bool> {
    use std::os::unix::io::AsRawFd;
    let Ok(f) = std::fs::File::open(path) else { return Some(false) };
    // SAFETY: 이 파일 설명자에 대한 비차단 공유 잠금 시도와 해제뿐이다.
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
    if r == 0 {
        unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_UN) };
        return Some(false);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::EWOULDBLOCK) => Some(true),
        _ => None,
    }
}

/// Windows: 잠금 상태를 보지 않는다 — 열려 있는지 모르면 대기열에만 넣는다(`deliver`).
#[cfg(windows)]
fn locked(_path: &Path) -> Option<bool> {
    None
}

// ── 이 앱이 띄운 exec 프로세스 ────────────────────────────────────────────────

/// 세션 → 이 앱이 띄운 `codex exec` 의 pid. 끝나면 거두는 스레드가 지운다.
fn running() -> &'static Mutex<HashMap<String, u32>> {
    static R: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 이 앱이 이어서 실행한 exec 가 아직 그 세션을 붙잡고 있나(끝나기 전에 다음 말을 대기열에 넣지 않게)
pub fn exec_running(thread_id: &str) -> bool {
    let pid = running().lock().ok().and_then(|m| m.get(thread_id).copied());
    pid.is_some_and(|p| crate::ingest::pid_alive(p as i64))
}

/// 넣을 글은 머리말로 시작해야 한다 — 인자로 넘길 때 `-` 로 시작해 옵션으로 읽히는 일이 없게
fn safe_prompt(text: &str) -> Result<(), String> {
    if text.starts_with(crate::conoti::REPLY_HEADER) || text.starts_with(crate::conoti::INBOX_HEADER) || text.starts_with(crate::conoti::SCHED_HEADER) {
        Ok(())
    } else {
        Err("머리말 없는 글은 넣지 않습니다".into())
    }
}

fn image_args(images: &[PathBuf]) -> Vec<String> {
    images
        .iter()
        .filter(|p| p.is_file())
        .map(|p| format!("--image={}", p.to_string_lossy()))
        .collect()
}

/// 열린 세션의 대기열에 넣는다. Codex 가 쉬는 중이면 곧바로 새 턴으로 처리한다.
pub fn queue(thread_id: &str, text: &str, images: &[PathBuf]) -> Result<(), String> {
    if !crate::channel::valid_session_id(thread_id) {
        return Err("세션 ID 형식 오류".into());
    }
    safe_prompt(text)?;
    let mut c = codex_cmd()?;
    c.arg("queue").arg(format!("--thread={thread_id}")).arg(format!("--message={text}")).args(image_args(images));
    let out = c
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("codex 실행 실패: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(launch_error(&out.stderr, &out.stdout, out.status.code()))
}

fn launch_error(stderr: &[u8], stdout: &[u8], code: Option<i32>) -> String {
    let all = format!("{}\n{}", crate::text::cli_text(stderr), crate::text::cli_text(stdout));
    if all.contains("Not inside a trusted directory") {
        return "Codex 가 이 폴더를 신뢰하지 않습니다 — 터미널에서 그 폴더로 가서 codex 를 한 번 열고 신뢰를 허용하세요".into();
    }
    if all.contains("usage limit") {
        return "Codex 사용 한도에 걸렸습니다 — 한도가 풀린 뒤 다시 보내세요".into();
    }
    if all.to_lowercase().contains("not logged in") || all.contains("codex login") {
        return "Codex 에 로그인되어 있지 않습니다 — 터미널에서 codex login".into();
    }
    // 설정 경고(무시된 설정 등)는 사유가 아니다
    let line = all
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.contains("ignoring") && !l.starts_with("Reading additional input"))
        .unwrap_or("");
    format!("codex 가 종료 코드 {code:?} 로 끝남 {}", crate::text::clip(line, 160))
}

/// `codex exec --json …` 을 띄우고 첫 `thread.started` 까지 기다린다. 나머지 출력은 거두는 스레드가 흘려보낸다.
fn spawn_exec(mut c: Command, known_thread: Option<&str>) -> Result<String, String> {
    let mut child = c
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("codex 실행 실패: {e}"))?;
    let stdout = child.stdout.take().ok_or("codex 출력 없음")?;
    let stderr = child.stderr.take();
    let (tx, rx) = std::sync::mpsc::channel::<Result<String, String>>();
    // 출력 읽기: thread.started 를 알리고, 그 뒤로는 끝날 때까지 버린다(파이프가 차서 멈추지 않게)
    std::thread::Builder::new()
        .name("codex-exec-out".into())
        .spawn(move || {
            let mut sent = false;
            let mut last_err = String::new();
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sent {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
                match v.get("type").and_then(Value::as_str) {
                    Some("thread.started") => {
                        if let Some(id) = v.get("thread_id").and_then(Value::as_str) {
                            let _ = tx.send(Ok(id.to_string()));
                            sent = true;
                        }
                    }
                    Some("error") | Some("turn.failed") => {
                        last_err = v
                            .get("message")
                            .or_else(|| v.get("error").and_then(|e| e.get("message")))
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                    }
                    _ => {}
                }
            }
            if !sent {
                let _ = tx.send(Err(last_err));
            }
        })
        .map_err(|e| e.to_string())?;
    let err_buf = std::sync::Arc::new(Mutex::new(Vec::<u8>::new()));
    if let Some(mut se) = stderr {
        let buf = err_buf.clone();
        let _ = std::thread::Builder::new().name("codex-exec-err".into()).spawn(move || {
            let mut chunk = [0u8; 4096];
            use std::io::Read;
            while let Ok(n) = se.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                if let Ok(mut b) = buf.lock() {
                    if b.len() < 64 * 1024 {
                        b.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        });
    }
    let got = rx.recv_timeout(Duration::from_secs(30));
    let thread_id = match got {
        Ok(Ok(id)) if crate::channel::valid_session_id(&id) => id,
        Ok(Ok(_)) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("codex 가 알려 준 세션 ID 형식 오류".into());
        }
        Ok(Err(msg)) => {
            let status = child.wait().ok();
            std::thread::sleep(Duration::from_millis(100));
            let err = err_buf.lock().map(|b| b.clone()).unwrap_or_default();
            let mut e = launch_error(&err, msg.as_bytes(), status.and_then(|s| s.code()));
            if e.ends_with("끝남 ") {
                e = "codex 가 세션을 시작하지 못함".into();
            }
            return Err(e);
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err("codex 가 30초 안에 세션을 열지 못함".into());
        }
    };
    if known_thread.is_some_and(|k| k != thread_id) {
        // 이어서 실행했는데 다른 스레드가 열렸다(복사본) — 멈추고 알린다
        let _ = child.kill();
        let _ = child.wait();
        return Err("codex 가 다른 세션을 열어 멈췄습니다".into());
    }
    let pid = child.id();
    if let Ok(mut m) = running().lock() {
        m.insert(thread_id.clone(), pid);
    }
    let tid = thread_id.clone();
    let _ = std::thread::Builder::new().name("codex-exec-wait".into()).spawn(move || {
        let _ = child.wait();
        if let Ok(mut m) = running().lock() {
            if m.get(&tid) == Some(&pid) {
                m.remove(&tid);
            }
        }
    });
    Ok(thread_id)
}

/// 꺼진 세션을 같은 ID 로 이어서 실행한다(백그라운드, 턴이 끝나면 스스로 끝난다).
pub fn resume(thread_id: &str, cwd: &Path, text: &str, images: &[PathBuf]) -> Result<(), String> {
    if !crate::channel::valid_session_id(thread_id) {
        return Err("세션 ID 형식 오류".into());
    }
    safe_prompt(text)?;
    if !cwd.is_dir() {
        return Err("세션 작업 폴더가 없음".into());
    }
    let mut c = codex_cmd()?;
    // 셸을 거치지 않고 인자로 직접 넘긴다. `--` 뒤는 옵션으로 읽히지 않는다
    c.current_dir(cwd).args(["exec", "resume", "--json", "--skip-git-repo-check"]);
    if let Some(cap) = resume_sandbox_cap(thread_sandbox(thread_id).as_deref()) {
        // `exec resume` 에는 --sandbox 가 없다 — 설정 덮어쓰기로 준다(실측 09-28: 전권 스레드가 workspace-write 로 이어짐)
        c.arg("-c").arg(format!("sandbox_mode=\"{cap}\""));
    }
    c.args(image_args(images))
        .arg("--")
        .arg(thread_id)
        .arg(text);
    spawn_exec(c, Some(thread_id)).map(|_| ())
}

/// 사용자가 설정에서 샌드박스를 정해 두었나(맨 위든 프로필이든 — 모르면 넓히지 않는다).
/// 없으면 새 작업은 폴더 안 쓰기 허용으로 띄운다 — exec 기본값 read-only 로는 일을 못 한다.
fn config_sets_sandbox() -> bool {
    match std::fs::read_to_string(paths::codex_dir().join("config.toml")) {
        Ok(s) => s.lines().map(str::trim).any(|l| l.starts_with("sandbox_mode") || l.starts_with("profile")),
        // 설정이 있는데 못 읽으면 모르는 것이다 — 넓히지 않는다
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

/// 스레드가 마지막 턴에 쓴 샌드박스(`turn_context.sandbox_policy.type`) — 기록 끝 4MB 에서 찾는다
fn thread_sandbox(thread_id: &str) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let path = rollout_of(thread_id)?;
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(4 * 1024 * 1024);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    text.lines().rev().filter(|l| l.contains("\"turn_context\"")).find_map(|l| {
        let v: Value = serde_json::from_str(l).ok()?;
        v.get("payload")?.get("sandbox_policy")?.get("type")?.as_str().map(str::to_string)
    })
}

fn rollout_of(thread_id: &str) -> Option<PathBuf> {
    fn walk(d: &Path, suffix: &str, depth: usize) -> Option<PathBuf> {
        for e in std::fs::read_dir(d).ok()?.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth < 4 {
                    if let Some(x) = walk(&p, suffix, depth + 1) {
                        return Some(x);
                    }
                }
            } else if p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(suffix)) {
                return Some(p);
            }
        }
        None
    }
    walk(&paths::codex_sessions_dir(), &format!("{thread_id}.jsonl"), 0)
}

/// 이어서 실행의 샌드박스: 스레드가 쓰던 값이 read-only·workspace-write 면 그대로(옵션 없음 → 승계),
/// 전권(danger-full-access)이거나 알 수 없으면 **workspace-write 로 낮춘다** — 폰 답 하나가 샌드박스 없는 실행이 되지 않게.
fn resume_sandbox_cap(thread_sandbox: Option<&str>) -> Option<&'static str> {
    match thread_sandbox {
        Some("read-only") | Some("workspace-write") => None,
        _ => Some("workspace-write"),
    }
}

/// 새 작업: 고른 폴더에서 `codex exec`. 반환: 세션(스레드) ID
pub fn start(cwd: &Path, text: &str, images: &[PathBuf]) -> Result<String, String> {
    safe_prompt(text)?;
    if !cwd.is_dir() {
        return Err("폴더가 없습니다".into());
    }
    let mut c = codex_cmd()?;
    c.current_dir(cwd).args(["exec", "--json", "--skip-git-repo-check"]);
    if !config_sets_sandbox() {
        c.args(["--sandbox", "workspace-write"]);
    }
    c.args(image_args(images)).arg("--").arg(text);
    spawn_exec(c, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_must_start_with_header() {
        assert!(safe_prompt("--dangerously-bypass-approvals-and-sandbox").is_err());
        assert!(safe_prompt(&crate::conoti::wrap_desk("안녕")).is_ok());
    }

    #[test]
    fn resume_never_runs_without_a_sandbox() {
        assert_eq!(resume_sandbox_cap(Some("danger-full-access")), Some("workspace-write"));
        assert_eq!(resume_sandbox_cap(None), Some("workspace-write"));
        assert_eq!(resume_sandbox_cap(Some("workspace-write")), None, "쓰던 값을 승계");
        assert_eq!(resume_sandbox_cap(Some("read-only")), None, "좁은 값은 넓히지 않는다");
    }

    #[test]
    fn inherited_env_is_dropped_but_config_and_auth_stay() {
        for k in ["CLAUDECODE", "CLAUDE_CODE_MESSAGING_TOKEN", "CLAUDE_PID", "CODEX_THREAD_ID", "CODEX_SANDBOX"] {
            assert!(inherited(k), "{k}");
        }
        for k in ["CODEX_HOME", "CODEX_SQLITE_HOME", "PATH", "HOME"] {
            assert!(!inherited(k), "{k}");
        }
    }

    #[test]
    fn exe_pick_prefers_codex_exe_and_skips_helpers() {
        assert_eq!(exe_rank("codex.exe"), Some(0));
        assert_eq!(exe_rank("Codex.EXE"), Some(0));
        assert_eq!(exe_rank("codex-x86_64-pc-windows-msvc.exe"), Some(1));
        assert_eq!(exe_rank("codex-aarch64-pc-windows-msvc.exe"), Some(1));
        for n in ["codex-code-mode-host.exe", "codex-command-runner.exe", "codex-windows-sandbox-setup.exe", "codex.cmd", "rg.exe"] {
            assert_eq!(exe_rank(n), None, "{n}");
        }

        // 실제 npm 배치: 보조 exe 가 이름순으로 codex.exe 보다 앞선다(`-` < `.`)
        let root = std::env::temp_dir().join(format!("aiinbox-codex-pick-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/bin");
        let res = root.join("node_modules/@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/codex-resources");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&res).unwrap();
        for p in [bin.join("codex-code-mode-host.exe"), res.join("codex-command-runner.exe"), res.join("codex-windows-sandbox-setup.exe")] {
            std::fs::write(p, b"").unwrap();
        }
        assert_eq!(find_exe_under(&root, 0), None, "보조 exe 만 있으면 고르지 않는다");
        std::fs::write(bin.join("codex.exe"), b"").unwrap();
        assert_eq!(find_exe_under(&root, 0), Some(bin.join("codex.exe")));
        let _ = std::fs::remove_dir_all(&root);

        // 예전 패키지: 삼중항 이름만 있을 때
        let old = std::env::temp_dir().join(format!("aiinbox-codex-pick-old-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&old);
        std::fs::create_dir_all(old.join("bin")).unwrap();
        std::fs::write(old.join("bin/codex-x86_64-pc-windows-msvc.exe"), b"").unwrap();
        assert_eq!(find_exe_under(&old, 0), Some(old.join("bin/codex-x86_64-pc-windows-msvc.exe")));
        let _ = std::fs::remove_dir_all(&old);
    }

    #[test]
    fn errors_are_readable() {
        let e = launch_error(b"Not inside a trusted directory and --skip-git-repo-check was not specified.", b"", Some(1));
        assert!(e.contains("신뢰하지 않습니다"), "{e}");
        let e = launch_error(b"Codex is ignoring 1 unrecognized configuration setting\nboom", b"", Some(2));
        assert!(e.ends_with("boom"), "{e}");
    }

    /// 잠금을 잡은 파일은 열린 것으로, 푼 파일은 닫힌 것으로 본다 — 검사가 잠금을 빼앗지 않는다
    #[cfg(unix)]
    #[test]
    fn lock_detection_does_not_take_the_lock() {
        use std::os::unix::io::AsRawFd;
        let dir = std::env::temp_dir().join(format!("aiinbox-codex-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("01a0e539-f547-7823-8107-4786f140daa0.lock");
        std::fs::write(&p, b"").unwrap();
        assert_eq!(locked(&p), Some(false));
        // 다른 프로세스가 잡은 것처럼: 자식 프로세스가 flock 을 잡고 기다린다
        let mut child = Command::new("/usr/bin/python3")
            .args([
                "-c",
                "import fcntl,sys,time;f=open(sys.argv[1],'r');fcntl.flock(f,fcntl.LOCK_EX);print('ok',flush=True);time.sleep(30)",
                p.to_str().unwrap(),
            ])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
        assert_eq!(line.trim(), "ok");
        assert_eq!(locked(&p), Some(true));
        assert_eq!(locked(&p), Some(true), "검사가 잠금을 빼앗으면 두 번째에 달라진다");
        // 이 프로세스가 배타 잠금을 잡을 수 없어야 한다 = 자식이 여전히 잡고 있다
        let f = std::fs::File::open(&p).unwrap();
        let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_ne!(r, 0);
        let _ = child.kill();
        let _ = child.wait();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(locked(&p), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
