//! 앱이 모델을 부르려고 띄운 CLI(`AI_INBOX_INTERNAL=1`)가 훅을 부르면 아무것도 남기지 않는다(되먹임 방지).

use std::io::Write;
use std::process::{Command, Stdio};

fn run_hook(dir: &std::path::Path, internal: bool) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_ai-inbox"));
    c.arg("hook").env("AI_INBOX_DATA_DIR", dir).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    if internal {
        c.env("AI_INBOX_INTERNAL", "1");
    } else {
        c.env_remove("AI_INBOX_INTERNAL");
    }
    let mut child = c.spawn().unwrap();
    child.stdin.take().unwrap().write_all(br#"{"hook_event_name":"Stop","session_id":"sess-0001","cwd":"/w/x"}"#).unwrap();
    assert!(child.wait().unwrap().success());
}

fn spooled(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir.join("spool")).map(|r| r.flatten().count()).unwrap_or(0)
}

#[test]
fn hook_ignores_internal_model_calls() {
    let dir = std::env::temp_dir().join(format!("aiinbox-marker-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    run_hook(&dir, true);
    assert_eq!(spooled(&dir), 0, "내부 호출의 훅은 스풀에 아무것도 남기지 않는다");
    run_hook(&dir, false);
    assert_eq!(spooled(&dir), 1, "보통 세션의 훅은 그대로 남긴다");
    let _ = std::fs::remove_dir_all(&dir);
}
