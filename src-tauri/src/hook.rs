//! `ai-inbox hook` — Claude Code 훅이 부르는 서브커맨드.
//!
//! stdin 의 훅 JSON 에서 필요한 칸만 골라 스풀 폴더에 파일 하나로 떨어뜨리고 끝난다.
//! 앱이 떠 있든 아니든 상관없다 — 앱은 다음 폴링 때(또는 켜질 때) 읽어 간다.
//! 어떤 오류가 나도 조용히 0 으로 끝낸다. 훅이 Claude Code 를 막거나 화면에 글을 찍으면 안 된다.

use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::paths;

/// 훅 JSON 에서 가져가는 칸. 프롬프트 본문은 가져가지 않는다(대화 기록에 이미 있다).
const KEEP: &[&str] = &[
    "hook_event_name",
    "session_id",
    "transcript_path",
    "cwd",
    "message",           // Notification
    "notification_type", // Notification (버전에 따라 있음)
    "title",             // Notification
    "source",            // SessionStart: startup | resume | clear | compact
    "reason",            // SessionEnd
    "agent_id",          // SubagentStop
    "agent_type",        // SubagentStop
    "permission_mode",
];

pub fn run() {
    let _ = write_event();
}

fn write_event() -> std::io::Result<()> {
    let mut raw = Vec::new();
    std::io::stdin().take(8 * 1024 * 1024).read_to_end(&mut raw)?;
    let parsed: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
    let Value::Object(map) = parsed else {
        return Ok(());
    };

    let mut out = serde_json::Map::new();
    for key in KEEP {
        if let Some(v) = map.get(*key) {
            // 문자열은 2,000자까지만 — 훅 입력이 비정상적으로 커도 스풀이 불어나지 않게
            let v = match v {
                Value::String(s) if s.chars().count() > 2000 => Value::String(s.chars().take(2000).collect()),
                Value::String(_) | Value::Bool(_) | Value::Number(_) | Value::Null => v.clone(),
                _ => continue,
            };
            out.insert((*key).to_string(), v);
        }
    }
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    out.insert("received_at_ms".into(), json!(now_ms as u64));

    let dir = paths::spool_dir();
    if let Some(parent) = dir.parent() {
        paths::ensure_private_dir(parent);
    }
    paths::ensure_private_dir(&dir);
    let stem = format!("{:013}-{}", now_ms, std::process::id());
    let tmp = dir.join(format!("{stem}.tmp"));
    let dst = dir.join(format!("{stem}.json"));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    std::io::Write::write_all(&mut f, &serde_json::to_vec(&Value::Object(out))?)?;
    drop(f);
    std::fs::rename(&tmp, &dst)?;
    Ok(())
}
