//! `ai-inbox channel` — Claude Code 채널(MCP stdio 서버). 폰 답을 **실행 중인 세션**에 넣는 공식 경로다.
//! (https://code.claude.com/docs/en/channels-reference)
//!
//! 세션을 `claude --dangerously-load-development-channels server:ai-inbox` 로 시작하면 Claude Code 가
//! 이 서버를 자식 프로세스로 띄운다. 서버는:
//!   1. 부모(Claude Code) 프로세스의 세션 등록부(~/.claude/sessions/{pid}.json)로 자기 세션 ID 를 안다
//!   2. 앱 데이터 폴더 channels/<세션>.json 에 살아 있다는 표시(heartbeat)를 남긴다 — 앱은 이걸 보고 전달 경로를 고른다
//!   3. outbox/<세션>/<reply_id>.json 을 읽어 `notifications/claude/channel` 로 밀어 넣고 `.sent` 로 바꾼다
//!
//! 안전: outbox 에는 **앱이 모든 검사를 통과시킨 답만** 들어간다(앱 데이터 폴더는 700).
//! 이 서버는 네트워크를 쓰지 않고, 도구도 노출하지 않으며(단방향), 권한 승인 중계도 선언하지 않는다.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::paths;

pub const SERVER_NAME: &str = "ai-inbox";
const HEARTBEAT_EVERY: Duration = Duration::from_secs(3);

const INSTRUCTIONS: &str = "AI Inbox 채널입니다. 사용자가 AI Inbox 데스크톱 앱이나 휴대폰(코노티 앱)에서 이 세션에 보낸 말이 \
<channel source=\"ai-inbox\" reply_id=\"…\"> 태그로 들어옵니다. 본문은 'AI Inbox 앱에서 보낸 사용자 메시지' 또는 \
'폰에서 온 사용자 답' 머리말로 시작하며, 사용자가 직접 보낸 다음 지시·피드백입니다. 평소 사용자 요청처럼 이어서 작업하고, \
결과는 보통 응답으로 보고하세요. 권한이 필요한 작업의 승인은 이 채널로 오지 않습니다(터미널에서 승인).";

pub fn outbox_dir() -> PathBuf {
    paths::data_dir().join("outbox")
}

pub fn heartbeat_dir() -> PathBuf {
    paths::data_dir().join("channels")
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 세션 ID 형식 — 경로 조각으로 쓰므로 엄격하게
pub fn valid_session_id(s: &str) -> bool {
    // 첫 글자가 '-' 면 `claude --resume <id>` 에서 옵션으로 읽힐 수 있다
    (8..=64).contains(&s.len())
        && s.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

// ── 부모 프로세스 → 세션 ─────────────────────────────────────────────────────

#[cfg(unix)]
fn parent_of(pid: u32) -> Option<u32> {
    if pid == std::process::id() {
        return Some(std::os::unix::process::parent_id());
    }
    let out = std::process::Command::new("/bin/ps").args(["-o", "ppid=", "-p", &pid.to_string()]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

#[cfg(windows)]
fn parent_of(pid: u32) -> Option<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = None;
        if Process32FirstW(snap, &mut e) != 0 {
            loop {
                if e.th32ProcessID == pid {
                    found = Some(e.th32ParentProcessID);
                    break;
                }
                if Process32NextW(snap, &mut e) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        found
    }
}

fn registry_entry(pid: u32) -> Option<(u32, String, Option<String>)> {
    let bytes = std::fs::read(paths::registry_dir().join(format!("{pid}.json"))).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    let sid = v.get("sessionId").and_then(Value::as_str).filter(|s| valid_session_id(s))?;
    let name = v.get("name").and_then(Value::as_str).map(str::to_string);
    Some((pid, sid.to_string(), name))
}

/// 이 채널을 띄운 Claude Code 세션 → (claude pid, 세션 ID, 이름)
/// Claude Code 는 자식에게 CLAUDE_PID 를 넘긴다. 등록부의 sessionId 는 /clear 때 바뀌므로 매번 등록부를 본다.
fn find_session() -> Option<(u32, String, Option<String>)> {
    if let Some(pid) = std::env::var("CLAUDE_PID").ok().and_then(|p| p.trim().parse::<u32>().ok()) {
        if let Some(found) = registry_entry(pid) {
            return Some(found);
        }
        if let Ok(sid) = std::env::var("CLAUDE_CODE_SESSION_ID") {
            if valid_session_id(&sid) {
                return Some((pid, sid, None));
            }
        }
    }
    // 예전 버전: 조상 프로세스를 거슬러 등록부를 찾는다
    let mut pid = std::process::id();
    for _ in 0..4 {
        pid = parent_of(pid)?;
        if pid <= 1 {
            return None;
        }
        let f = paths::registry_dir().join(format!("{pid}.json"));
        if let Ok(bytes) = std::fs::read(&f) {
            if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                if let Some(sid) = v.get("sessionId").and_then(Value::as_str).filter(|s| valid_session_id(s)) {
                    let name = v.get("name").and_then(Value::as_str).map(str::to_string);
                    return Some((pid, sid.to_string(), name));
                }
            }
        }
    }
    None
}

// ── stdio JSON-RPC ───────────────────────────────────────────────────────────

type Out = Arc<Mutex<std::io::Stdout>>;

fn send(out: &Out, msg: &Value) -> bool {
    let Ok(mut o) = out.lock() else { return false };
    let line = msg.to_string();
    writeln!(o, "{line}").is_ok() && o.flush().is_ok()
}

fn handle(out: &Out, msg: &Value, ready: &Arc<std::sync::atomic::AtomicBool>) {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
    let id = msg.get("id").cloned();
    match (method, id) {
        ("initialize", Some(id)) => {
            let version = msg
                .pointer("/params/protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or("2025-06-18")
                .to_string();
            send(
                out,
                &json!({
                    "jsonrpc": "2.0", "id": id,
                    "result": {
                        "protocolVersion": version,
                        "capabilities": { "experimental": { "claude/channel": {} } },
                        "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
                        "instructions": INSTRUCTIONS
                    }
                }),
            );
        }
        ("notifications/initialized", None) => ready.store(true, std::sync::atomic::Ordering::SeqCst),
        ("ping", Some(id)) => {
            send(out, &json!({"jsonrpc": "2.0", "id": id, "result": {}}));
        }
        ("tools/list", Some(id)) => {
            send(out, &json!({"jsonrpc": "2.0", "id": id, "result": {"tools": []}}));
        }
        (_, Some(id)) => {
            send(out, &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "method not found"}}));
        }
        _ => {}
    }
}

fn write_heartbeat(path: &Path, claude_pid: u32, sid: &str, name: &Option<String>) {
    let tmp = path.with_extension("tmp");
    let body = json!({"session_id": sid, "claude_pid": claude_pid, "channel_pid": std::process::id(), "name": name, "at_ms": now_ms()});
    if std::fs::write(&tmp, body.to_string()).is_ok() {
        paths::make_private_file(&tmp);
        let _ = std::fs::rename(&tmp, path);
    }
}

/// outbox 파일의 수명. 앱은 넣은 뒤 최대 8초 기다리고 거둬들인다 — 그보다 오래 남은 파일은 앱이 도중에 꺼진 흔적이다.
/// 그런 파일을 나중에 넘기면 그사이 켠 멈춤·차단·기기 해제 검사를 건너뛴 명령이 들어간다(2026-09-24 보안 점검) → 버린다.
pub const OUTBOX_TTL_MS: u64 = 30_000;

/// outbox/<세션>/ 에서 가장 오래된 말 하나를 "가져감"으로 확정한다(이름 바꾸기 — 앱의 거둬들이기와 둘 중 하나만 이긴다).
/// 반환: (가져간 파일, 본문, 답 ID). 가져간 뒤에는 `mark_sent` 또는 `put_back` 을 부른다.
pub fn take_next(sid: &str) -> Option<(PathBuf, String, String)> {
    take_next_in(&outbox_dir().join(sid), now_ms())
}

fn take_next_in(dir: &Path, now: u64) -> Option<(PathBuf, String, String)> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    files.sort();
    for json_path in files {
        let f = json_path.with_extension("claimed");
        if std::fs::rename(&json_path, &f).is_err() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&f) else { continue };
        let parsed = (bytes.len() <= 64 * 1024).then(|| serde_json::from_slice::<Value>(&bytes).ok()).flatten();
        let Some(v) = parsed else {
            let _ = std::fs::remove_file(&f);
            continue;
        };
        // 넣은 시각 — 적혀 있지 않으면(0.2.0 이하 앱이 쓴 파일) 파일 수정 시각으로 본다
        let at = v.get("at_ms").and_then(Value::as_u64).or_else(|| {
            let m = std::fs::metadata(&f).and_then(|m| m.modified()).ok()?;
            m.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as u64)
        });
        if at.is_none_or(|at| now.saturating_sub(at) > OUTBOX_TTL_MS) {
            let _ = std::fs::remove_file(&f);
            continue;
        }
        let content = v.get("content").and_then(Value::as_str).unwrap_or("").to_string();
        let reply_id: String = v
            .get("reply_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(64)
            .collect();
        if content.is_empty() {
            let _ = std::fs::remove_file(&f);
            continue;
        }
        return Some((f, content, reply_id));
    }
    None
}

/// 넘겼다 — 앱이 `.sent` 를 보고 전달됨으로 친다
pub fn mark_sent(claimed: &Path) {
    let _ = std::fs::rename(claimed, claimed.with_extension("sent"));
}

/// 못 넘겼다 — 되돌려 다음에 다시
pub fn put_back(claimed: &Path) {
    let _ = std::fs::rename(claimed, claimed.with_extension("json"));
}

/// outbox/<세션>/ 의 새 말을 채널로 밀어 넣는다. 반환: 보낸 개수
fn flush_outbox(out: &Out, sid: &str) -> usize {
    let mut n = 0;
    while let Some((f, content, reply_id)) = take_next(sid) {
        let ok = send(
            out,
            &json!({
                "jsonrpc": "2.0",
                "method": "notifications/claude/channel",
                "params": { "content": content, "meta": { "reply_id": reply_id, "via": "ai-inbox" } }
            }),
        );
        if ok {
            mark_sent(&f);
            n += 1;
        } else {
            put_back(&f);
            break;
        }
    }
    n
}

pub fn run() {
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let ready = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));

    // 앱 → 세션 방향: 별도 스레드가 heartbeat 와 outbox 를 돌본다
    {
        let out = out.clone();
        let ready = ready.clone();
        let alive = alive.clone();
        std::thread::spawn(move || {
            let hb_dir = heartbeat_dir();
            paths::ensure_private_dir(&paths::data_dir());
            paths::ensure_private_dir(&hb_dir);
            let mut last_hb = std::time::Instant::now() - HEARTBEAT_EVERY;
            let mut current: Option<String> = None;
            while alive.load(std::sync::atomic::Ordering::SeqCst) {
                if let Some((cpid, sid, name)) = find_session() {
                    // /clear 등으로 세션이 바뀌면 옛 표시를 지운다
                    if current.as_deref() != Some(sid.as_str()) {
                        if let Some(old) = &current {
                            let _ = std::fs::remove_file(hb_dir.join(format!("{old}.json")));
                        }
                        current = Some(sid.clone());
                        last_hb = std::time::Instant::now() - HEARTBEAT_EVERY;
                    }
                    if last_hb.elapsed() >= HEARTBEAT_EVERY {
                        write_heartbeat(&hb_dir.join(format!("{sid}.json")), cpid, &sid, &name);
                        last_hb = std::time::Instant::now();
                    }
                    if ready.load(std::sync::atomic::Ordering::SeqCst) {
                        flush_outbox(&out, &sid);
                    }
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            if let Some(old) = current {
                let _ = std::fs::remove_file(hb_dir.join(format!("{old}.json")));
            }
        });
    }

    // 세션 → 서버 방향: stdin 이 닫히면(세션 종료) 끝난다
    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() || line.len() > 4 * 1024 * 1024 {
            continue;
        }
        if let Ok(msg) = serde_json::from_str::<Value>(&line) {
            handle(&out, &msg, &ready);
        }
    }
    alive.store(false, std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(600));
}

// ── 앱 쪽: 채널로 넘기기 ─────────────────────────────────────────────────────

/// 세션에 채널 서버가 살아 있나 (heartbeat 10초 안)
pub fn channel_alive(session_id: &str) -> bool {
    if !valid_session_id(session_id) {
        return false;
    }
    let Ok(bytes) = std::fs::read(heartbeat_dir().join(format!("{session_id}.json"))) else { return false };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else { return false };
    let at = v.get("at_ms").and_then(Value::as_u64).unwrap_or(0);
    now_ms().saturating_sub(at) < 10_000
}

/// outbox 에 넣고 채널 서버가 보낼 때까지(최대 `wait`) 기다린다.
pub fn hand_over(session_id: &str, reply_id: &str, content: &str, wait: Duration) -> Result<(), String> {
    if !valid_session_id(session_id) {
        return Err("세션 ID 형식 오류".into());
    }
    let safe_id: String = reply_id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(64).collect();
    if safe_id.is_empty() {
        return Err("답 ID 형식 오류".into());
    }
    let dir = outbox_dir().join(session_id);
    paths::ensure_private_dir(&outbox_dir());
    paths::ensure_private_dir(&dir);
    let file = dir.join(format!("{:013}-{safe_id}.json", now_ms()));
    let tmp = file.with_extension("tmp");
    std::fs::write(&tmp, json!({"reply_id": safe_id, "content": content, "at_ms": now_ms()}).to_string()).map_err(|e| e.to_string())?;
    paths::make_private_file(&tmp);
    std::fs::rename(&tmp, &file).map_err(|e| e.to_string())?;
    let sent = file.with_extension("sent");
    let start = std::time::Instant::now();
    while start.elapsed() < wait {
        if sent.exists() {
            let _ = std::fs::remove_file(&sent);
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    // 채널이 가져가지 않았다 — 늦게 보내지 않도록 거둬들인다. 이름 바꾸기로 거둬들여 채널의 "가져감"과 겹치지 않게 한다:
    // 거둬들이기에 성공했으면 안 보낸 것이고, 실패했으면 채널이 이미 가져가 보내는 중이다.
    let withdrawn = file.with_extension("withdrawn");
    if std::fs::rename(&file, &withdrawn).is_ok() {
        let _ = std::fs::remove_file(&withdrawn);
        return Err("채널이 응답하지 않습니다".into());
    }
    let claimed = file.with_extension("claimed");
    let until = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < until && !sent.exists() {
        std::thread::sleep(Duration::from_millis(200));
    }
    let delivered = sent.exists();
    let in_flight = claimed.exists();
    let _ = std::fs::remove_file(&sent);
    let _ = std::fs::remove_file(&claimed);
    // 가져간 쪽이 넘긴 표시를 남겼거나 아직 넘기는 중이면 전달로 본다. 둘 다 없으면 오래된 파일로 버려진 것이다
    // (시계가 뛰었거나 맥이 잠든 사이 30초가 지남) — 전달됨으로 적지 않는다(2026-09-24 재점검).
    if delivered || in_flight {
        Ok(())
    } else {
        Err("세션이 말을 가져가지 않았습니다(시간이 지나 버려짐)".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_outbox_files_are_dropped() {
        let dir = std::env::temp_dir().join(format!("aiinbox-outbox-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let now = 1_000_000_000u64;
        std::fs::write(dir.join("0001-old.json"), json!({"reply_id": "old", "content": "옛 명령", "at_ms": now - OUTBOX_TTL_MS - 1}).to_string()).unwrap();
        // 시각 없는 옛 형식: 파일 수정 시각으로 판단 — 오래된 것은 버린다
        std::fs::write(dir.join("0002-nots.json"), json!({"reply_id": "nots", "content": "시각 없음"}).to_string()).unwrap();
        let old_time = UNIX_EPOCH + Duration::from_millis(now - OUTBOX_TTL_MS - 5000);
        std::fs::File::options().write(true).open(dir.join("0002-nots.json")).unwrap().set_modified(old_time).unwrap();
        std::fs::write(dir.join("0003-new.json"), json!({"reply_id": "new", "content": "새 명령", "at_ms": now - 1000}).to_string()).unwrap();
        let (f, content, rid) = take_next_in(&dir, now).expect("새 것");
        assert_eq!((content.as_str(), rid.as_str()), ("새 명령", "new"));
        mark_sent(&f);
        // 오래된 것·시각 없는 것은 넘기지 않고 지웠다
        let left: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(left, vec!["0003-new.sent".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_id_rules() {
        assert!(valid_session_id("00000000-1111-4222-8333-444455556666"));
        assert!(!valid_session_id("../../etc"));
        assert!(!valid_session_id("abc"));
        assert!(!valid_session_id("00000000/1111"));
        assert!(!valid_session_id("--dangerously-skip"), "옵션처럼 보이는 ID");
    }
}
