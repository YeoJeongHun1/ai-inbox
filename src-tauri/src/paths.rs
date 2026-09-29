//! 앱이 읽고 쓰는 경로. 훅 서브커맨드는 Tauri 를 띄우기 전에 돌기 때문에
//! Tauri 의 app_data_dir 과 같은 규칙(= OS 데이터 폴더 / identifier)으로 직접 계산한다.

use std::path::PathBuf;

pub const IDENTIFIER: &str = "com.yeojeonghun.ai-inbox";

static OVERRIDE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// 진단 명령(`ingest-once`)이 실제 데이터 폴더 대신 쓸 폴더. 훅·앱 본체는 이 값을 받지 않는다.
pub fn set_data_dir_override(dir: PathBuf) {
    let _ = OVERRIDE.set(dir);
}

/// 데이터 폴더를 바꿔 둔 실행인가(시험·진단)
pub fn has_data_dir_override() -> bool {
    OVERRIDE.get().is_some()
}

/// macOS: ~/Library/Application Support/<id>   Windows: %LOCALAPPDATA%\<id> (로밍 프로필로 복사되지 않게)
pub fn data_dir() -> PathBuf {
    if let Some(dir) = OVERRIDE.get() {
        return dir.clone();
    }
    // 개발 빌드에서만 환경변수로 바꿀 수 있다
    if cfg!(debug_assertions) {
        if let Ok(dir) = std::env::var("AI_INBOX_DATA_DIR") {
            if !dir.trim().is_empty() {
                return PathBuf::from(dir);
            }
        }
    }
    dirs::data_local_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".ai-inbox"))
        .join(IDENTIFIER)
}

/// 훅이 이벤트 한 건을 파일 하나로 떨어뜨리는 곳. 앱이 읽고 지운다.
pub fn spool_dir() -> PathBuf {
    data_dir().join("spool")
}

pub fn db_path() -> PathBuf {
    data_dir().join("inbox.db")
}

/// Claude Code 설정 폴더 — CLAUDE_CONFIG_DIR 이 있으면 그걸 따른다.
pub fn claude_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CLAUDE_CONFIG_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    dirs::home_dir().unwrap_or_default().join(".claude")
}

pub fn projects_dir() -> PathBuf {
    claude_dir().join("projects")
}

/// 살아 있는 세션 등록부: {pid}.json 에 sessionId·name·status(busy/idle)
pub fn registry_dir() -> PathBuf {
    claude_dir().join("sessions")
}

pub fn settings_path() -> PathBuf {
    claude_dir().join("settings.json")
}

/// Codex 설정 폴더 — CODEX_HOME 이 있으면 그걸 따른다.
pub fn codex_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("CODEX_HOME") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    dirs::home_dir().unwrap_or_default().join(".codex")
}

/// Codex 대화 기록: sessions/YYYY/MM/DD/rollout-<시각>-<스레드 ID>.jsonl
pub fn codex_sessions_dir() -> PathBuf {
    codex_dir().join("sessions")
}

/// 스레드를 여는 프로세스(TUI·데스크톱 앱·exec)가 쓰기 잠금을 잡는 파일: <스레드 ID>.lock
pub fn codex_locks_dir() -> PathBuf {
    codex_dir().join("thread-writer-locks")
}

/// 스레드 이름(`/rename`·자동 제목) 기록 — 줄마다 {id, thread_name, updated_at}
pub fn codex_index_path() -> PathBuf {
    codex_dir().join("session_index.jsonl")
}

/// 폴더를 만들고 본인만 들어갈 수 있게(700). Windows 는 사용자 프로필 ACL 을 따른다.
pub fn ensure_private_dir(dir: &std::path::Path) {
    let _ = std::fs::create_dir_all(dir);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
}

/// 파일을 본인만 읽고 쓰게(600).
pub fn make_private_file(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}
