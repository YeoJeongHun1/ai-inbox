//! `ai-inbox hook` — Claude Code 훅이 부르는 서브커맨드.
//!
//! stdin 의 훅 JSON 에서 필요한 칸만 골라 스풀 폴더에 파일 하나로 떨어뜨리고 끝난다.
//! 앱이 떠 있든 아니든 상관없다 — 앱은 다음 폴링 때(또는 켜질 때) 읽어 간다.
//! 어떤 오류가 나도(패닉 포함) 조용히 0 으로 끝낸다. 훅이 Claude Code 를 막거나 화면에 글을 찍으면 안 된다.
//!
//! UserPromptSubmit 은 요청 시점 태깅(`tags.rs`)의 재료도 함께 남긴다: 프롬프트 지문·`#태그`·git 최상위 폴더 이름·규칙 매칭용 글(앞 3,000자).
//! 데이터베이스는 건드리지 않는다(앱이 꺼져 있어도, 락 경합·마이그레이션 전 스키마와 상관없이 수 ms 안에 끝나게). 앱이 스풀을 읽는 즉시 태그를 정해 기록한다.

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
    // 앱이 띄운 모델 호출(llm.rs)의 훅 — 사용자 세션이 아니다. 아무것도 남기지 않는다
    if crate::llm::is_internal_env() {
        return;
    }
    // 어떤 경우에도 화면에 글을 찍거나 0 이 아닌 코드로 끝나 사용자의 요청을 막지 않는다
    std::panic::set_hook(Box::new(|_| {}));
    let _ = std::panic::catch_unwind(|| write_event());
}

/// 훅 JSON(stdin) → 스풀 파일에 쓸 JSON. 객체가 아니면 None.
pub fn spool_entry(input: &Value, now_ms: u64) -> Option<Value> {
    let Value::Object(map) = input else { return None };
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
    if map.get("hook_event_name").and_then(Value::as_str) == Some("UserPromptSubmit") {
        add_tag_material(map, &mut out);
    }
    out.insert("received_at_ms".into(), json!(now_ms));
    Some(Value::Object(out))
}

fn write_event() -> std::io::Result<()> {
    let mut raw = Vec::new();
    std::io::stdin().take(8 * 1024 * 1024).read_to_end(&mut raw)?;
    let parsed: Value = serde_json::from_slice(&raw).unwrap_or(Value::Null);
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0) as u64;
    let Some(out) = spool_entry(&parsed, now_ms) else { return Ok(()) };

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
    std::io::Write::write_all(&mut f, &serde_json::to_vec(&out)?)?;
    drop(f);
    std::fs::rename(&tmp, &dst)?;
    Ok(())
}

/// 요청 시점 태깅 재료 — 프롬프트가 없거나 문자열이 아니면 아무것도 넣지 않는다(옛 앱과도 호환: 모르는 칸은 무시된다)
fn add_tag_material(map: &serde_json::Map<String, Value>, out: &mut serde_json::Map<String, Value>) {
    let Some(prompt) = map.get("prompt").and_then(Value::as_str) else { return };
    out.insert("prompt_key".into(), json!(crate::tags::prompt_key(&typed(prompt))));
    out.insert("prompt_head".into(), json!(prompt.chars().take(3000).collect::<String>()));
    let tags = crate::tags::parse_hashtags(prompt);
    if !tags.is_empty() {
        out.insert("hashtags".into(), json!(tags));
    }
    if let Some((root, name)) = map.get("cwd").and_then(Value::as_str).and_then(crate::tags::project_of_cwd) {
        out.insert("proj_root".into(), json!(root));
        out.insert("proj_name".into(), json!(name));
    }
}

/// 지문은 사람이 친 부분만으로(답장 줄·첨부 목록 제외) — 대화 기록 쪽과 같은 규칙
fn typed(prompt: &str) -> String {
    crate::tags::typed_prompt(prompt)
}
