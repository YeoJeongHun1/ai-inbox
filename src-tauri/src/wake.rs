//! `ai-inbox wake` — **실행 중인** Claude Code 세션에 말을 넣는 대기 훅.
//!
//! Claude Code 의 `asyncRewake` 훅(https://code.claude.com/docs/en/hooks)으로 설치된다: 훅이 백그라운드에서 돌다가
//! 종료 코드 2 로 끝나면 Claude 가 깨어나 훅의 stderr 를 받는다(쉬고 있던 세션도 깨어난다 — 2026-09-24 실측).
//! SessionStart · Stop(요청이 끝날 때마다) · ConfigChange(설정 파일이 바뀔 때 — 앱이 수정 시각만 갱신해 다시 잇는다) 에 걸린다.
//!
//! 한 세션에 대기자는 하나: `waiters/<세션>.json` 에 자기 pid 를 적고, 더 새 대기자가 이어받으면 조용히 끝난다.
//! 세션이 **쉬는 중(idle)이거나 일하는 중(busy)** 이면 outbox/<세션>/ 의 말 하나를 가져가 넘긴다 — 일하는 중이면 Claude Code 가
//! 다음 도구 사이에 끼워 읽는다(터미널에서 작업 중에 친 말처럼, 2026-09-28 사용자 요청). 권한 승인·질문 대기(waiting)면 기다린다.
//! 말을 넘긴 대기자는 끝나므로 PreToolUse 에도 걸어 둔다 — 대기자가 살아 있으면 새로 이어받지 않고 바로 끝난다.
//!
//! 🚨 `claude -p`·SDK 실행에서는 바로 끝난다: 그쪽은 비동기 훅이 끝나길 기다리느라 프로세스가 끝나지 않는다(실측).
//! outbox 에는 앱이 검사를 통과시킨 말만 들어간다(데이터 폴더 700). 네트워크는 쓰지 않는다.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::{channel, paths};

/// Claude 에게 보이는 첫 줄 — 훅 "오류" 틀로 들어가므로 무엇인지 먼저 밝힌다.
/// 터미널에는 요약 한 줄만 그려지므로 받은 말을 답 첫머리에 인용으로 옮겨 적게 한다 — 앱은 수집 때 뗀다(`text::strip_echo`).
pub const NOTE: &str = "사용자가 AI Inbox(데스크톱 앱·폰)에서 이 세션에 보낸 다음 지시입니다 — 훅 오류가 아닙니다. \
이 말은 사용자의 터미널 화면에 보이지 않으니, 답을 쓸 때 맨 먼저 아래 머리말부터 사용자가 보낸 말의 마지막 줄까지를 \
고치거나 줄이지 말고 인용으로 옮겨 적으세요(이 알림을 감싼 태그는 빼고) — 첫 줄은 `> 📥 ` 뒤에 머리말, 나머지 모든 줄(빈 줄 포함) 앞에 `> `. \
그다음 평소 사용자 요청처럼 이어서 작업하고 결과를 보고하세요 — 하던 작업 도중에 받았다면 터미널에서 작업 중에 친 말처럼 하던 일에 반영하고, \
마지막 보고에 이 말에 대한 답을 따로 적으세요(사용자는 이 말의 답으로 그 마지막 보고를 봅니다).";

/// 지금 말을 넘겨도 되는 상태인가 — 쉬는 중이거나 일하는 중(도구 사이에 읽힌다). 권한 승인·질문 대기(waiting)는 아니다.
pub fn takes_now(status: Option<&str>) -> bool {
    matches!(status, Some("idle" | "busy"))
}

const HEARTBEAT: Duration = Duration::from_secs(3);
const POLL: Duration = Duration::from_millis(500);

pub fn waiter_dir() -> PathBuf {
    paths::data_dir().join("waiters")
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// 등록부(~/.claude/sessions/<pid>.json)의 (세션 ID, kind, entrypoint, status)
fn registry(pid: i64) -> Option<(String, String, String, Option<String>)> {
    let bytes = std::fs::read(paths::registry_dir().join(format!("{pid}.json"))).ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Some((s("sessionId")?, s("kind").unwrap_or_default(), s("entrypoint").unwrap_or_default(), crate::deliver::input_status(s("status"))))
}

/// 대기해도 되는 세션인가 — 사람이 보는 대화형 세션과 `claude --bg` 세션만
pub fn eligible(env_entry: Option<&str>, kind: &str, entry: &str) -> bool {
    let sdk = |e: &str| e.starts_with("sdk");
    !env_entry.is_some_and(sdk) && !sdk(entry) && matches!(kind, "interactive" | "bg" | "background")
}

fn write_marker(file: &Path, me: u32, claude_pid: i64) {
    let tmp = file.with_extension(format!("{me}.tmp"));
    let body = json!({"pid": me, "claude_pid": claude_pid, "at_ms": now_ms()});
    if std::fs::write(&tmp, body.to_string()).is_ok() {
        paths::make_private_file(&tmp);
        let _ = std::fs::rename(&tmp, file);
    }
}

fn marker_pid(file: &Path) -> Option<u32> {
    let v: Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    v.get("pid").and_then(Value::as_u64).map(|p| p as u32)
}

fn remove_if_mine(file: &Path, me: u32) {
    if marker_pid(file) == Some(me) {
        let _ = std::fs::remove_file(file);
    }
}

/// 앱 쪽: 이 세션에 대기자가 살아 있나(심장 박동 10초 안 + 프로세스 생존)
pub fn waiter_alive(session_id: &str) -> bool {
    if !channel::valid_session_id(session_id) {
        return false;
    }
    let Ok(bytes) = std::fs::read(waiter_dir().join(format!("{session_id}.json"))) else { return false };
    let Ok(v) = serde_json::from_slice::<Value>(&bytes) else { return false };
    let fresh = now_ms().saturating_sub(v.get("at_ms").and_then(Value::as_u64).unwrap_or(0)) < 10_000;
    let alive = v.get("pid").and_then(Value::as_i64).map(crate::ingest::pid_alive).unwrap_or(false);
    fresh && alive
}

/// 훅 진입점. 반환값이 종료 코드: 0 = 조용히 끝(아무것도 안 보임) · 2 = Claude 를 깨운다(stderr 가 보인다)
pub fn run() -> i32 {
    let mut raw = Vec::new();
    if std::io::stdin().take(1024 * 1024).read_to_end(&mut raw).is_err() {
        return 0;
    }
    let Ok(input) = serde_json::from_slice::<Value>(&raw) else { return 0 };
    let Some(sid) = input.get("session_id").and_then(Value::as_str).filter(|s| channel::valid_session_id(s)).map(str::to_string) else {
        return 0;
    };
    let Some(claude_pid) = std::env::var("CLAUDE_PID").ok().and_then(|p| p.trim().parse::<i64>().ok()) else { return 0 };
    let env_entry = std::env::var("CLAUDE_CODE_ENTRYPOINT").ok();
    match registry(claude_pid) {
        Some((_, kind, entry, _)) if eligible(env_entry.as_deref(), &kind, &entry) => {}
        _ => return 0,
    }

    // 도구를 부를 때마다 불린다 — 대기자가 이미 있으면 이어받지 않는다(프로세스를 갈아치우지 않게)
    if input.get("hook_event_name").and_then(Value::as_str) == Some("PreToolUse") && waiter_alive(&sid) {
        return 0;
    }

    let dir = waiter_dir();
    paths::ensure_private_dir(&paths::data_dir());
    paths::ensure_private_dir(&dir);
    let file = dir.join(format!("{sid}.json"));
    let me = std::process::id();
    write_marker(&file, me, claude_pid);
    let mut beat = Instant::now();
    loop {
        std::thread::sleep(POLL);
        // 더 새 대기자가 이어받았다
        if marker_pid(&file) != Some(me) {
            return 0;
        }
        // 세션이 끝났거나 /clear 로 다른 세션이 됐다
        let Some((reg_sid, _, _, status)) = registry(claude_pid).filter(|_| crate::ingest::pid_alive(claude_pid)) else {
            remove_if_mine(&file, me);
            return 0;
        };
        if reg_sid != sid {
            remove_if_mine(&file, me);
            return 0;
        }
        if beat.elapsed() >= HEARTBEAT {
            write_marker(&file, me, claude_pid);
            beat = Instant::now();
        }
        if !takes_now(status.as_deref()) {
            continue;
        }
        if let Some((claimed, content, _rid)) = channel::take_next(&sid) {
            remove_if_mine(&file, me);
            channel::mark_sent(&claimed);
            eprint!("{NOTE}\n\n{content}");
            return 2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_asks_for_the_echo_the_app_strips() {
        assert!(NOTE.contains(&format!("`> {} `", crate::text::ECHO_MARK)));
    }

    #[test]
    fn hands_over_while_idle_or_working_but_not_while_asking() {
        assert!(takes_now(Some("idle")));
        assert!(takes_now(Some("busy")));
        assert!(!takes_now(Some("waiting")));
        assert!(!takes_now(None));
    }

    #[test]
    fn only_interactive_or_background_sessions() {
        assert!(eligible(Some("cli"), "interactive", "cli"));
        assert!(eligible(None, "bg", "cli"));
        assert!(eligible(Some("claude-vscode"), "interactive", "claude-vscode"));
        // claude -p · SDK — 비동기 훅을 기다리느라 끝나지 않는다
        assert!(!eligible(Some("sdk-cli"), "interactive", "sdk-cli"));
        assert!(!eligible(Some("cli"), "interactive", "sdk-ts"));
        assert!(!eligible(Some("cli"), "", "cli"));
    }
}
