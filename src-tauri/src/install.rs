//! ~/.claude/settings.json 에 이 앱의 훅을 넣고 뺀다.
//!
//! - 다른 훅·설정은 건드리지 않는다. 키 순서를 지킨다(preserve_order).
//! - 우리 훅 = 명령어가 정확히 `"<경로>/ai-inbox(.exe)" hook` 또는 `… wake` 인 항목만. 비슷한 이름의 남의 훅은 우리 것이 아니다.
//! - `wake` 는 실행 중인 세션에 말을 넣는 대기 훅(`asyncRewake`, `wake.rs`) — SessionStart · Stop · ConfigChange 에 건다.
//!   PreToolUse 에도 건다 — 폰 말로 시작한 요청처럼 대기자가 없는 채 일하는 동안에도 다음 말을 받게(대기자가 있으면 바로 끝난다).
//! - 첨부 이미지 폴더 **읽기** 허용 규칙 하나(`permissions.allow` 의 `Read(//<데이터 폴더>/attachments/**)`)도 함께 넣고 뺀다 —
//!   세션이 붙여 넣은 이미지를 권한 창 없이 열게(폰에서 보낸 이미지는 PC 앞에 사람이 없을 때 온다). 읽기만, 그 폴더만.
//! - 처음 바꾸기 전 상태를 settings.json.ai-inbox-backup 에 **한 번만** 남긴다(이후 덮어쓰지 않는다).
//! - 임시 파일(600) → rename. 원래 파일 권한을 따르고, 파일이 없었으면 600 으로 만든다.
//! - 읽은 뒤 쓰기 직전에 파일이 바뀌었으면 쓰지 않는다(Claude Code 가 동시에 고치는 경우).

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

use crate::paths;

pub const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "Notification",
    "Stop",
    "SubagentStop",
    "SessionEnd",
];

/// 실행 중인 세션에 말을 넣는 대기 훅이 걸리는 이벤트(ConfigChange = 앱이 설정 파일 수정 시각을 갱신해 다시 잇는다)
pub const WAKE_EVENTS: &[&str] = &["SessionStart", "Stop", "ConfigChange", "PreToolUse"];
/// 이것만 걸려 있으면 쉬는 세션에 넣을 수 있다 — PreToolUse 는 작업 중 전달용이라 없어도 예전처럼 동작한다(0.6.1 이하 설치)
const WAKE_CORE: &[&str] = &["SessionStart", "Stop", "ConfigChange"];

const BIN_NAMES: &[&str] = &["ai-inbox", "ai-inbox.exe"];

#[derive(Serialize)]
pub struct HookStatus {
    pub settings_path: String,
    pub installed_events: Vec<String>,
    pub missing_events: Vec<String>,
    /// 실행 중인 세션에 말을 넣는 대기 훅이 빠진 이벤트(옛 버전에서 설치했으면 여기가 찬다 — 다시 설치하면 된다)
    pub wake_missing: Vec<String>,
    /// 설치된 훅이 가리키는 실행 파일이 지금 이 앱과 다르면 그 명령어
    pub stale_command: Option<String>,
    /// 훅은 있는데 첨부 이미지 읽기 허용이 빠졌다(0.3.0 이전에 설치)
    pub read_missing: bool,
    pub command: String,
}

fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ai-inbox"))
}

fn exe_string() -> String {
    let exe = exe_path().to_string_lossy().into_owned();
    // Windows 는 슬래시로 적는다 — Git Bash·cmd·PowerShell 모두 받는다. macOS/Linux 의 \ 는 경로 문자라 그대로 둔다.
    if cfg!(windows) { exe.replace('\\', "/") } else { exe }
}

pub fn hook_command() -> String {
    format!("\"{}\" hook", exe_string())
}

pub fn wake_command() -> String {
    format!("\"{}\" wake", exe_string())
}

/// 첨부 이미지 폴더 읽기 허용 규칙. Claude Code 규칙에서 절대 경로는 `//` 로 시작한다(실측: 공백 있는 경로도 된다).
/// 윈도우는 경로 규칙 표기가 달라 넣지 않는다(권한 창이 뜬다).
pub fn read_rule() -> Option<String> {
    if cfg!(windows) {
        return None;
    }
    let dir = crate::attach::dir().to_string_lossy().into_owned();
    (dir.starts_with('/') && !dir.contains([')', '*', '\n'])).then(|| format!("Read(/{dir}/**)"))
}

/// 우리가 넣은 읽기 규칙(데이터 폴더가 바뀌었어도 알아본다)
fn is_our_rule(rule: &str) -> bool {
    read_rule().as_deref() == Some(rule)
        || (rule.starts_with("Read(//") && rule.ends_with(&format!("/{}/attachments/**)", paths::IDENTIFIER)))
}

/// 우리 읽기 규칙을 뺀다. 우리가 비운 allow·permissions 만 지운다.
fn strip_rule(settings: &mut Value) -> bool {
    let Some(perms) = settings.get_mut("permissions").and_then(Value::as_object_mut) else { return false };
    let Some(allow) = perms.get_mut("allow").and_then(Value::as_array_mut) else { return false };
    let before = allow.len();
    allow.retain(|r| !r.as_str().map(is_our_rule).unwrap_or(false));
    if allow.len() == before {
        return false;
    }
    if allow.is_empty() {
        perms.remove("allow");
        if perms.is_empty() {
            if let Some(root) = settings.as_object_mut() {
                root.remove("permissions");
            }
        }
    }
    true
}

fn add_rule(settings: &mut Value) -> Result<(), String> {
    let Some(rule) = read_rule() else { return Ok(()) };
    let root = settings.as_object_mut().ok_or("settings.json 최상위가 객체가 아닙니다")?;
    let perms = root.entry("permissions").or_insert_with(|| json!({}));
    let perms = perms.as_object_mut().ok_or("settings.json 의 permissions 가 객체가 아닙니다")?;
    let allow = perms.entry("allow").or_insert_with(|| json!([]));
    let allow = allow.as_array_mut().ok_or("settings.json 의 permissions.allow 가 배열이 아닙니다")?;
    if !allow.iter().any(|r| r.as_str() == Some(rule.as_str())) {
        allow.push(json!(rule));
    }
    Ok(())
}

fn has_rule(settings: &Value) -> bool {
    let Some(rule) = read_rule() else { return true };
    settings
        .get("permissions")
        .and_then(|p| p.get("allow"))
        .and_then(Value::as_array)
        .is_some_and(|a| a.iter().any(|r| r.as_str() == Some(rule.as_str())))
}

/// 훅을 이미 설치한 사용자가 새 버전으로 올라오면 읽기 규칙만 더한다(훅은 건드리지 않는다). 더했으면 true.
pub fn ensure_read_rule() -> Result<bool, String> {
    let st = status()?;
    if !st.read_missing {
        return Ok(false);
    }
    let path = paths::settings_path();
    let loaded = read_settings(&path)?;
    let mut settings = loaded.value.clone();
    strip_rule(&mut settings);
    add_rule(&mut settings)?;
    write_settings(&path, &loaded, &settings)?;
    Ok(true)
}

/// 훅 명령어는 셸이 실행한다. 큰따옴표 안에서도 해석되는 문자가 있거나,
/// 곧 사라질 임시 위치(DMG 마운트·Gatekeeper 격리 복사본)면 설치하지 않는다.
fn check_exe_path() -> Result<(), String> {
    let exe = exe_string();
    if let Some(bad) = exe.chars().find(|c| c.is_control() || "\"$`%!“”„‟".contains(*c) || (!cfg!(windows) && *c == '\\')) {
        return Err(format!(
            "앱 경로에 셸이 해석하는 문자 '{bad}' 가 있어 훅을 설치하지 않습니다. 앱을 특수문자 없는 폴더로 옮긴 뒤 다시 시도하세요: {exe}"
        ));
    }
    if cfg!(target_os = "macos") && (exe.starts_with("/Volumes/") || exe.contains("/AppTranslocation/")) {
        return Err(
            "앱이 디스크 이미지나 임시 격리 위치에서 실행 중입니다. '응용 프로그램' 폴더로 옮겨 다시 연 뒤 훅을 설치하세요."
                .into(),
        );
    }
    Ok(())
}

/// `"…/ai-inbox" hook|wake` 형태이고 파일 이름이 정확히 ai-inbox(.exe) 일 때만 우리 훅. 반환: 하위 명령
fn our_kind(command: &str) -> Option<&'static str> {
    let c = command.trim();
    let rest = c.strip_prefix('"')?;
    let (path, tail) = rest.split_once('"')?;
    let kind = match tail.trim() {
        "hook" => "hook",
        "wake" => "wake",
        _ => return None,
    };
    let name = path.rsplit(['/', '\\']).next().unwrap_or("");
    BIN_NAMES.contains(&name).then_some(kind)
}

fn is_ours(command: &str) -> bool {
    our_kind(command).is_some()
}

struct Loaded {
    value: Value,
    /// 읽은 원문 (없으면 None) — 쓰기 직전 비교용
    raw: Option<String>,
}

fn read_settings(path: &Path) -> Result<Loaded, String> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(Loaded { value: json!({}), raw: Some(text) }),
        Ok(text) => {
            let value: Value = serde_json::from_str(&text)
                .map_err(|e| format!("settings.json 을 읽지 못했습니다(JSON 오류): {e}"))?;
            if !value.is_object() {
                return Err("settings.json 최상위가 객체가 아닙니다".into());
            }
            Ok(Loaded { value, raw: Some(text) })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Loaded { value: json!({}), raw: None }),
        Err(e) => Err(format!("settings.json 을 열지 못했습니다: {e}")),
    }
}

fn write_settings(path: &Path, loaded: &Loaded, value: &Value) -> Result<(), String> {
    // 심볼릭 링크면 실제 파일을 바꾸되(링크 보존), 백업은 링크 옆(~/.claude)에 둔다.
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target.parent().ok_or("settings.json 위치를 알 수 없습니다")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;

    if let Some(raw) = &loaded.raw {
        let backup = path.with_file_name("settings.json.ai-inbox-backup");
        if !backup.exists() {
            write_private(&backup, raw.as_bytes(), None)?;
        }
    }

    let mut text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    text.push('\n');
    let tmp = dir.join(format!("settings.json.ai-inbox-{}.tmp", std::process::id()));
    let perms = std::fs::metadata(&target).ok().map(|m| m.permissions());
    write_private(&tmp, text.as_bytes(), perms)?;

    // 읽은 뒤 누가 바꿨으면 덮지 않는다
    let now = std::fs::read_to_string(&target).ok();
    if now != loaded.raw {
        let _ = std::fs::remove_file(&tmp);
        return Err("settings.json 이 방금 다른 곳에서 바뀌었습니다. 잠시 뒤 다시 시도하세요.".into());
    }
    std::fs::rename(&tmp, &target).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        e.to_string()
    })
}

/// 새 파일을 600 으로 만들어 쓴다(원래 권한이 있으면 그걸로 바꾼다).
fn write_private(path: &Path, bytes: &[u8], perms: Option<std::fs::Permissions>) -> Result<(), String> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    f.sync_all().ok();
    drop(f);
    if let Some(p) = perms {
        let _ = std::fs::set_permissions(path, p);
    }
    Ok(())
}

/// 이벤트 배열에서 우리 항목(명령어)들을 찾는다. `kind` = hook | wake
fn our_commands(groups: &Value, kind: &str) -> Vec<String> {
    let mut found = Vec::new();
    for group in groups.as_array().into_iter().flatten() {
        for hook in group.get("hooks").and_then(Value::as_array).into_iter().flatten() {
            if let Some(cmd) = hook.get("command").and_then(Value::as_str) {
                if our_kind(cmd) == Some(kind) {
                    found.push(cmd.to_string());
                }
            }
        }
    }
    found
}

pub fn status() -> Result<HookStatus, String> {
    let path = paths::settings_path();
    let settings = read_settings(&path)?.value;
    let command = hook_command();
    let mut installed = Vec::new();
    let mut missing = Vec::new();
    let mut stale = None;
    for ev in EVENTS {
        let cmds = settings.get("hooks").and_then(|h| h.get(*ev)).map(|g| our_commands(g, "hook")).unwrap_or_default();
        if cmds.is_empty() {
            missing.push(ev.to_string());
        } else {
            installed.push(ev.to_string());
            if let Some(other) = cmds.iter().find(|c| **c != command) {
                stale = Some(other.clone());
            }
        }
    }
    let wake = wake_command();
    let wake_missing = WAKE_EVENTS
        .iter()
        .filter(|ev| {
            let cmds = settings.get("hooks").and_then(|h| h.get(**ev)).map(|g| our_commands(g, "wake")).unwrap_or_default();
            !cmds.iter().any(|c| *c == wake)
        })
        .map(|ev| ev.to_string())
        .collect();
    let read_missing = !installed.is_empty() && !has_rule(&settings);
    Ok(HookStatus {
        settings_path: path.to_string_lossy().into_owned(),
        read_missing,
        installed_events: installed,
        missing_events: missing,
        wake_missing,
        stale_command: stale,
        command,
    })
}

/// 우리 항목을 모두 빼고(옛 경로 포함) 지금 실행 파일로 다시 넣는다.
pub fn install() -> Result<HookStatus, String> {
    check_exe_path()?;
    let path = paths::settings_path();
    let loaded = read_settings(&path)?;
    let mut settings = loaded.value.clone();
    strip_ours(&mut settings)?;
    strip_rule(&mut settings);
    add_rule(&mut settings)?;
    let command = hook_command();
    let root = settings.as_object_mut().ok_or("settings.json 최상위가 객체가 아닙니다")?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or("settings.json 의 hooks 가 객체가 아닙니다")?;
    for ev in EVENTS {
        let arr = hooks.entry(ev.to_string()).or_insert_with(|| json!([]));
        let arr = arr
            .as_array_mut()
            .ok_or_else(|| format!("settings.json 의 hooks.{ev} 가 배열이 아닙니다 — 직접 확인해 주세요"))?;
        arr.push(json!({
            "hooks": [{ "type": "command", "command": command, "timeout": 5 }]
        }));
    }
    let wake = wake_command();
    for ev in WAKE_EVENTS {
        let arr = hooks.entry(ev.to_string()).or_insert_with(|| json!([]));
        let arr = arr
            .as_array_mut()
            .ok_or_else(|| format!("settings.json 의 hooks.{ev} 가 배열이 아닙니다 — 직접 확인해 주세요"))?;
        arr.push(json!({
            "hooks": [{ "type": "command", "command": wake, "asyncRewake": true }]
        }));
    }
    write_settings(&path, &loaded, &settings)?;
    status()
}

/// 실행 중인 세션들이 설정을 다시 읽게(ConfigChange → 대기 훅이 다시 뜬다) settings.json 의 **수정 시각만** 갱신한다.
/// 내용은 바꾸지 않는다. 20초에 한 번까지.
pub fn rearm() -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = std::time::SystemTime::now();
    let secs = now.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let last = LAST.load(Ordering::SeqCst);
    if secs.saturating_sub(last) < 20 || LAST.compare_exchange(last, secs, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return false;
    }
    let path = paths::settings_path();
    let target = std::fs::canonicalize(&path).unwrap_or(path);
    std::fs::OpenOptions::new().write(true).open(&target).and_then(|f| f.set_modified(now)).is_ok()
}

/// 대기 훅(wake)이 지금 실행 파일로 쉬는 세션용 이벤트에 걸려 있나
pub fn wake_ready() -> bool {
    status().map(|s| s.wake_missing.iter().all(|e| !WAKE_CORE.contains(&e.as_str()))).unwrap_or(false)
}

pub fn uninstall() -> Result<HookStatus, String> {
    let path = paths::settings_path();
    let loaded = read_settings(&path)?;
    let mut settings = loaded.value.clone();
    let hooks = strip_ours(&mut settings)?;
    if strip_rule(&mut settings) || hooks {
        write_settings(&path, &loaded, &settings)?;
    }
    status()
}

/// 우리 훅 항목만 제거. **우리가 비운** 그룹·이벤트만 정리하고, 원래 비어 있던 것은 그대로 둔다.
fn strip_ours(settings: &mut Value) -> Result<bool, String> {
    let Some(hooks) = settings.get_mut("hooks") else { return Ok(false) };
    if hooks.is_null() {
        return Ok(false);
    }
    let hooks = hooks.as_object_mut().ok_or("settings.json 의 hooks 가 객체가 아닙니다")?;
    let mut changed = false;
    let events: Vec<String> = hooks.keys().cloned().collect();
    for ev in events {
        let Some(groups) = hooks.get_mut(&ev).and_then(Value::as_array_mut) else { continue };
        let mut emptied_by_us = vec![false; groups.len()];
        for (i, group) in groups.iter_mut().enumerate() {
            if let Some(list) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = list.len();
                list.retain(|h| !h.get("command").and_then(Value::as_str).map(is_ours).unwrap_or(false));
                if list.len() != before {
                    changed = true;
                    emptied_by_us[i] = list.is_empty();
                }
            }
        }
        let had_groups = !groups.is_empty();
        let mut i = 0;
        groups.retain(|_| {
            let keep = !emptied_by_us[i];
            i += 1;
            keep
        });
        if had_groups && groups.is_empty() {
            hooks.remove(&ev);
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OURS_MAC: &str = "\"/Applications/AI Inbox.app/Contents/MacOS/ai-inbox\" hook";
    const OURS_WIN: &str = "\"C:/Program Files/AI Inbox/ai-inbox.exe\" hook";

    #[test]
    fn strip_keeps_other_hooks() {
        let mut s = json!({
            "hooks": {
                "Stop": [
                    {"hooks": [{"type": "command", "command": "/opt/tools/other-hook.sh"}]},
                    {"hooks": [{"type": "command", "command": OURS_MAC}]}
                ],
                "SessionEnd": [ {"hooks": [{"type": "command", "command": OURS_WIN}]} ],
                "PreToolUse": [
                    {"hooks": [{"type": "command", "command": "\"/opt/tools/my-ai-inbox\" hook"}]},
                    {"hooks": [{"type": "command", "command": "echo \"ai-inbox\" hook"}]},
                    {"matcher": "Bash", "hooks": []}
                ]
            },
            "model": "opus"
        });
        assert!(strip_ours(&mut s).unwrap());
        assert_eq!(s["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert!(s["hooks"].get("SessionEnd").is_none());
        // 이름만 비슷한 남의 훅과, 원래 비어 있던 그룹은 그대로
        assert_eq!(s["hooks"]["PreToolUse"].as_array().unwrap().len(), 3);
        assert_eq!(s["model"], "opus");
    }

    #[test]
    fn ours_is_exact() {
        assert!(is_ours(OURS_MAC));
        assert!(is_ours(OURS_WIN));
        assert_eq!(our_kind("\"/Applications/AI Inbox.app/Contents/MacOS/ai-inbox\" wake"), Some("wake"));
        assert_eq!(our_kind("\"/Applications/AI Inbox.app/Contents/MacOS/ai-inbox\" channel"), None);
        assert!(!is_ours("/opt/tools/other-hook.sh"));
        assert!(!is_ours("\"/opt/tools/my-ai-inbox\" hook"));
        assert!(!is_ours("echo \"ai-inbox\" hook"));
        assert!(!is_ours("\"/x/ai-inbox\" hook; rm -rf ~"));
    }

    #[test]
    fn quoted_path_rules() {
        let bad = |p: &str| p.chars().any(|c| c.is_control() || "\"$`%!“”„‟".contains(c));
        assert!(!bad("C:/Program Files (x86)/AI Inbox/ai-inbox.exe"));
        assert!(!bad("/Users/me/Applications/AI Inbox.app/Contents/MacOS/ai-inbox"));
        assert!(bad("/tmp/$(rm -rf ~)/ai-inbox"));
        assert!(bad("C:/Users/%USERNAME%/ai-inbox.exe"));
        assert!(bad("/tmp/“quoted”/ai-inbox"));
    }

    #[test]
    fn read_rule_is_added_once_and_removed_cleanly() {
        let Some(rule) = read_rule() else { return };
        assert!(rule.starts_with("Read(//") && is_our_rule(&rule));
        assert!(!is_our_rule("Read(//Users/x/other/attachments/**)"));
        assert!(!is_our_rule("Bash(rm:*)"));
        // 남의 규칙은 그대로, 우리 것은 한 번만
        let mut s = json!({"permissions": {"allow": ["Bash(git status:*)"], "deny": ["Read(./.env)"]}});
        add_rule(&mut s).unwrap();
        add_rule(&mut s).unwrap();
        assert_eq!(s["permissions"]["allow"].as_array().unwrap().len(), 2);
        assert!(has_rule(&s));
        assert!(strip_rule(&mut s));
        assert_eq!(s["permissions"]["allow"], json!(["Bash(git status:*)"]));
        assert_eq!(s["permissions"]["deny"], json!(["Read(./.env)"]));
        // 우리가 만든 permissions 는 비면 지운다
        let mut s = json!({"model": "opus"});
        add_rule(&mut s).unwrap();
        assert!(strip_rule(&mut s));
        assert_eq!(s, json!({"model": "opus"}));
        // 원래 비어 있던 allow 는 건드리지 않는다
        let mut s = json!({"permissions": {"allow": []}});
        assert!(!strip_rule(&mut s));
        assert_eq!(s, json!({"permissions": {"allow": []}}));
    }

    #[test]
    fn non_array_event_is_error() {
        let mut s = json!({"hooks": {"Stop": {"oops": true}}});
        assert!(strip_ours(&mut s).is_ok());
        let mut s2 = json!({"hooks": "x"});
        assert!(strip_ours(&mut s2).is_err());
    }
}
