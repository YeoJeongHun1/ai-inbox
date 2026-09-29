//! 내장 SQLite. 파일 하나(inbox.db)에 WAL 로 쌓는다.
//!
//! 정본은 Claude Code 대화 기록(~/.claude/projects/**/*.jsonl)과 Codex 대화 기록(~/.codex/sessions/**/rollout-*.jsonl)이고 이 DB 는 그걸
//! "요청 1건 = turn 1행" 으로 정리한 파생 저장소다. 단, 읽음·별표·고정 같은
//! **사용자 상태는 여기에만 있다** — 재수집해도 지워지지 않게 upsert 에서 건드리지 않는다.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

pub const SCHEMA_VERSION: i64 = 10;

/// 요청 1건 = 1행. v2: 요청 ID 는 세션 안에서만 유일하다(다른 세션의 같은 ID 가 덮어쓰지 않게).
const TURN_COLUMNS: &str = r#"
    id                 INTEGER PRIMARY KEY,
    session_id         TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    prompt_uuid        TEXT NOT NULL,
    seq                INTEGER NOT NULL,
    origin             TEXT,        -- human | peer
    prompt_source      TEXT,        -- typed | queued | suggestion_accepted | system
    peer_name          TEXT,
    prompt_at          TEXT NOT NULL,
    prompt_text        TEXT,
    slash_command      TEXT,
    understanding      TEXT,        -- 첫 도구 호출 전의 어시스턴트 말
    plan_json          TEXT,        -- TodoWrite/TaskCreate 항목
    summary            TEXT,        -- Claude Code 가 남긴 away_summary
    response_text      TEXT,        -- 마지막 어시스턴트 말 = 결과 보고
    first_reply_at     TEXT,
    last_activity_at   TEXT,
    stopped_at         TEXT,
    ended_at           TEXT,
    duration_ms        INTEGER,     -- 요청 → 끝 (벽시계)
    active_ms          INTEGER,     -- turn_duration 합 (모델이 실제로 일한 시간)
    ttfr_ms            INTEGER,
    status             TEXT NOT NULL DEFAULT 'running',
    needs_input        INTEGER NOT NULL DEFAULT 0,
    hidden             INTEGER NOT NULL DEFAULT 0,   -- /clear 같은 로컬 명령(API 호출 0)
    error_count        INTEGER NOT NULL DEFAULT 0,
    pending_bg         INTEGER NOT NULL DEFAULT 0,
    model              TEXT,
    effort             TEXT,
    api_calls          INTEGER NOT NULL DEFAULT 0,
    input_tokens       INTEGER NOT NULL DEFAULT 0,
    output_tokens      INTEGER NOT NULL DEFAULT 0,
    thinking_tokens    INTEGER NOT NULL DEFAULT 0,
    cache_create_5m    INTEGER NOT NULL DEFAULT 0,
    cache_create_1h    INTEGER NOT NULL DEFAULT 0,
    cache_read         INTEGER NOT NULL DEFAULT 0,
    web_search         INTEGER NOT NULL DEFAULT 0,
    web_fetch          INTEGER NOT NULL DEFAULT 0,
    context_tokens     INTEGER NOT NULL DEFAULT 0,
    tool_calls         INTEGER NOT NULL DEFAULT 0,
    files_changed      INTEGER NOT NULL DEFAULT 0,
    subagent_count     INTEGER NOT NULL DEFAULT 0,
    task_notifications INTEGER NOT NULL DEFAULT 0,
    cwd                TEXT,
    git_branch         TEXT,
    -- 사용자 상태 (재수집이 덮지 않는다)
    read_at            TEXT,
    starred            INTEGER NOT NULL DEFAULT 0,
    notified           INTEGER NOT NULL DEFAULT 0,
    updated_at         TEXT,
    UNIQUE (session_id, prompt_uuid)
"#;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT
);

-- 대화 기록 파일마다 어디까지 읽었나
CREATE TABLE IF NOT EXISTS source_file (
    path       TEXT PRIMARY KEY,
    session_id TEXT,
    size       INTEGER NOT NULL DEFAULT 0,
    mtime_ms   INTEGER NOT NULL DEFAULT 0,
    offset     INTEGER NOT NULL DEFAULT 0,   -- 다음 시작 위치 = 아직 열린 마지막 요청의 첫 줄
    skipped    INTEGER NOT NULL DEFAULT 0    -- 백필 기간 밖이라 그 뒤로 늘어난 부분만 따라가는 파일
);

CREATE TABLE IF NOT EXISTS session (
    id              TEXT PRIMARY KEY,
    transcript_path TEXT,
    project_dir     TEXT,
    title           TEXT,       -- /rename 으로 지은 이름(custom-title)
    agent_name      TEXT,
    live_name       TEXT,       -- 살아 있는 세션 등록부의 이름(nameSource=user)
    git_branch      TEXT,
    cc_version      TEXT,
    model           TEXT,
    first_at        TEXT,
    last_at         TEXT,
    cost_usd        REAL,
    lines_added     INTEGER,
    lines_removed   INTEGER,
    live_status     TEXT,       -- busy | idle | NULL(프로세스 없음)
    live_pid        INTEGER,
    notify_at       TEXT,       -- 마지막 Notification 훅
    notify_msg      TEXT,
    ended_at        TEXT,       -- SessionEnd 훅
    end_reason      TEXT,
    pinned          INTEGER NOT NULL DEFAULT 0,
    hidden          INTEGER NOT NULL DEFAULT 0,  -- 보관(목록·폰에서 뺌)
    archived_at     TEXT,       -- 보관한 시각(v6) — 이 뒤에 온 요청·결과가 있으면 목록으로 되돌린다
    agent           TEXT        -- 어느 도구의 세션인가(v9): NULL = Claude Code · 'codex' = OpenAI Codex
);

-- turn 표는 TURN_COLUMNS 로 만든다 (마이그레이션에서 재사용)
CREATE INDEX IF NOT EXISTS turn_session_seq ON turn (session_id, seq);
CREATE INDEX IF NOT EXISTS turn_unread ON turn (session_id) WHERE read_at IS NULL;
CREATE INDEX IF NOT EXISTS turn_prompt_at ON turn (prompt_at DESC);

CREATE TABLE IF NOT EXISTS turn_tool (
    turn_id   INTEGER NOT NULL REFERENCES turn(id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    calls     INTEGER NOT NULL,
    PRIMARY KEY (turn_id, tool_name)
);

CREATE TABLE IF NOT EXISTS turn_file (
    turn_id INTEGER NOT NULL REFERENCES turn(id) ON DELETE CASCADE,
    path    TEXT NOT NULL,
    edits   INTEGER NOT NULL,
    PRIMARY KEY (turn_id, path)
);

CREATE TABLE IF NOT EXISTS turn_subagent (
    turn_id     INTEGER NOT NULL REFERENCES turn(id) ON DELETE CASCADE,
    seq         INTEGER NOT NULL,
    agent_type  TEXT,
    description TEXT,
    background  INTEGER NOT NULL DEFAULT 0,
    started_at  TEXT,
    ended_at    TEXT,
    duration_ms INTEGER,
    PRIMARY KEY (turn_id, seq)
);

-- 작업 과정: 중간 보고·도구 호출·백그라운드 완료 알림…  (append-only)
CREATE TABLE IF NOT EXISTS turn_step (
    turn_id INTEGER NOT NULL REFERENCES turn(id) ON DELETE CASCADE,
    seq     INTEGER NOT NULL,
    at      TEXT,
    kind    TEXT NOT NULL,   -- text | tool | task | compact | error | interrupt | continue | summary
    name    TEXT,
    text    TEXT,
    PRIMARY KEY (turn_id, seq)
);

-- 기록에서 지운 요청(v6). 원본을 다시 읽거나 이어서 실행해 생긴 복사본 세션에 같은 요청이 있어도 되살리지 않는다
-- (요청 ID 는 복사본에도 그대로 남는다). 지운 뒤에 새 활동이 붙은 요청과 새 요청은 들어온다.
-- 대화 기록에 ID 가 없어 줄 위치로 만든 ID(pos-…)는 세션 안에서만 같다.
CREATE TABLE IF NOT EXISTS turn_deleted (
    session_id  TEXT NOT NULL,
    prompt_uuid TEXT NOT NULL,
    deleted_at  TEXT NOT NULL,
    PRIMARY KEY (session_id, prompt_uuid)
);
CREATE INDEX IF NOT EXISTS turn_deleted_uuid ON turn_deleted (prompt_uuid);

CREATE TABLE IF NOT EXISTS hook_event (
    id         INTEGER PRIMARY KEY,
    session_id TEXT,
    event      TEXT NOT NULL,
    at         TEXT NOT NULL,
    detail     TEXT
);
CREATE INDEX IF NOT EXISTS hook_event_session ON hook_event (session_id, at);

-- ── 폰 연결(선택 기능) ────────────────────────────────────────────────────
-- 세션별 폰 답: 0 기본(기기 허용을 따름) · 1 이 세션은 폰 답 막음 · 2 받음(v3 의 값, 기본과 같다)
CREATE TABLE IF NOT EXISTS conoti_session (
    session_id TEXT PRIMARY KEY,
    mode       INTEGER NOT NULL DEFAULT 0,
    enabled_at TEXT
);

-- 페어링한 폰(v4). 비밀값(psk)은 여기 없고 데이터 폴더의 relay-identity.json(600)에 있다.
CREATE TABLE IF NOT EXISTS relay_device (
    pid        TEXT PRIMARY KEY,  -- 페어링 번호(16바이트 hex)
    name       TEXT NOT NULL,
    phone_pub  TEXT NOT NULL,     -- 폰 정적 공개키(hex)
    can_reply  INTEGER NOT NULL DEFAULT 1,
    can_manage INTEGER NOT NULL DEFAULT 0,  -- 폰에서 세션 관리(보관함 보기·보관·고정·기록에서 지우기) 허용(v7) — 페어링 때 답 보내기 허용 값을 따른다
    created_at TEXT NOT NULL,
    last_seen  TEXT,
    ticket     TEXT,              -- 푸시 티켓(서버가 봉인한 값 — 알림만 보낼 수 있다)
    acct       TEXT               -- 같은 계정 폰끼리 푸시를 한 번만 보내기 위한 표지
);

-- 폰에서 온 답. 받은 것은 전부 남긴다(전달 기록)
CREATE TABLE IF NOT EXISTS conoti_reply (
    reply_id     TEXT PRIMARY KEY,
    card_id      TEXT,
    request_id   TEXT,
    turn_id      INTEGER,        -- 답이 달린 카드의 요청
    session_id   TEXT,
    kind         TEXT,
    text         TEXT,
    created_at   TEXT,
    received_at  TEXT NOT NULL,
    state        TEXT NOT NULL,  -- confirm(데스크톱 확인 대기) · delivering · delivered · handled · rejected
    note         TEXT,
    delivered_at TEXT,
    result_turn  INTEGER,        -- 이 답으로 시작된 요청
    acked        TEXT,           -- (v3 평문 카드 시절) 서버에 알린 상태
    device       TEXT,           -- 보낸 폰(relay_device.pid)
    wait_from    TEXT,           -- 폰 말의 10분 시계 시작점. 세션이 앞 작업을 하는 동안은 계속 뒤로 밀린다(v8)
    quote        TEXT            -- 답장 대상(v10): 'prompt' | 'response' — turn_id 의 요청·결과에 단 답. NULL = 그냥 이어서
);
CREATE INDEX IF NOT EXISTS conoti_reply_state ON conoti_reply (state);
CREATE INDEX IF NOT EXISTS conoti_reply_received ON conoti_reply (received_at);

-- ── 이미지 첨부(v5) ─────────────────────────────────────────────────────────
-- 파일은 데이터 폴더 attachments/<id 앞 2자>/<id>.<ext>. id = 저장한 바이트의 SHA-256 → 같은 이미지는 한 벌(attach.rs)
CREATE TABLE IF NOT EXISTS attachment (
    id         TEXT PRIMARY KEY,
    mime       TEXT NOT NULL,
    ext        TEXT NOT NULL,
    bytes      INTEGER NOT NULL,
    width      INTEGER NOT NULL,
    height     INTEGER NOT NULL,
    name       TEXT,              -- 붙일 때의 파일 이름(있으면)
    source     TEXT NOT NULL,     -- desktop | phone (처음 올린 곳)
    created_at TEXT NOT NULL,
    touched_at TEXT NOT NULL,     -- 마지막으로 붙이거나 다시 올린 때 — 보내지 않은 이미지 정리 기준
    touched_by TEXT NOT NULL      -- 그때 어디서(폰 1시간 · 데스크톱 7일)
);
CREATE INDEX IF NOT EXISTS attachment_created ON attachment (created_at);

-- 보낸 말(conoti_reply) ↔ 이미지. 같은 이미지가 여러 말에 붙을 수 있다
CREATE TABLE IF NOT EXISTS reply_attachment (
    reply_id TEXT NOT NULL,
    att_id   TEXT NOT NULL,
    ord      INTEGER NOT NULL,
    PRIMARY KEY (reply_id, ord)
);
CREATE INDEX IF NOT EXISTS reply_attachment_att ON reply_attachment (att_id);

-- 폰이 답보다 먼저 올린 이미지(한 장씩). 답의 atts 는 같은 기기·같은 rid 로 올린 것만 쓸 수 있다
CREATE TABLE IF NOT EXISTS att_upload (
    device TEXT NOT NULL,
    rid    TEXT NOT NULL,
    ord    INTEGER NOT NULL,
    att_id TEXT NOT NULL,
    at     TEXT NOT NULL,
    PRIMARY KEY (device, rid, ord)
);
"#;

pub fn open(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(dir) = path.parent() {
        crate::paths::ensure_private_dir(dir);
    }
    let conn = Connection::open(path)?;
    // 대화 내용이 들어 있으므로 본인만 읽게 (WAL·SHM 은 SQLite 가 DB 파일 권한을 따라 만든다)
    crate::paths::make_private_file(path);
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 8000;
         PRAGMA journal_size_limit = 4194304;",
    )?;
    // 옛 버전이 만든 보조 파일(-wal·-shm)도 본인 전용으로
    for suffix in ["-wal", "-shm"] {
        let side = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
        if side.exists() {
            crate::paths::make_private_file(&side);
        }
    }
    Ok(conn)
}

#[cfg(test)]
mod migrate_tests {
    #[test]
    fn v7_gives_manage_only_to_devices_that_may_reply() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        super::migrate(&c).unwrap();
        // v6 모양으로 되돌린 뒤 기기 둘(답 허용·답 막음)
        c.execute_batch("ALTER TABLE relay_device DROP COLUMN can_manage;").unwrap();
        super::set_meta(&c, "schema_version", "6").unwrap();
        for (pid, can_reply) in [("aa", 1), ("bb", 0)] {
            c.execute(
                "INSERT INTO relay_device (pid, name, phone_pub, can_reply, created_at) VALUES (?1, '폰', 'ff', ?2, 't')",
                rusqlite::params![pid, can_reply],
            )
            .unwrap();
        }
        super::migrate(&c).unwrap();
        let m = |pid: &str| c.query_row("SELECT can_manage FROM relay_device WHERE pid = ?1", [pid], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!((m("aa"), m("bb")), (1, 0), "답을 막아 둔 기기에 지우기 권한이 생기면 안 된다");
    }
}

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let had_turn: bool = conn
        .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'turn'", [], |r| r.get::<_, i64>(0))
        .map(|n| n > 0)?;
    conn.execute_batch(&format!("CREATE TABLE IF NOT EXISTS turn ({TURN_COLUMNS});"))?;
    conn.execute_batch(SCHEMA)?;
    let current: i64 = get_meta(conn, "schema_version")
        .and_then(|v| v.parse().ok())
        .unwrap_or(if had_turn { 1 } else { SCHEMA_VERSION });
    if current < 2 {
        // v1 → v2: prompt_uuid 단독 UNIQUE → (session_id, prompt_uuid). 읽음·별표 등 사용자 상태는 그대로 옮긴다.
        conn.execute_batch(&format!(
            "PRAGMA foreign_keys = OFF;
             BEGIN;
             CREATE TABLE turn_v2 ({TURN_COLUMNS});
             INSERT INTO turn_v2 SELECT * FROM turn;
             DROP TABLE turn;
             ALTER TABLE turn_v2 RENAME TO turn;
             COMMIT;
             PRAGMA foreign_keys = ON;"
        ))?;
        conn.execute_batch(SCHEMA)?; // 표와 함께 사라진 인덱스를 다시 만든다
    }
    if current < 4 && had_turn {
        // v3 → v4: 평문 카드(서버에 요약을 올리던 방식)를 걷어내고 종단간 중계로 바꿨다
        let has_device: bool = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('conoti_reply') WHERE name = 'device'", [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)?;
        if !has_device {
            conn.execute_batch("ALTER TABLE conoti_reply ADD COLUMN device TEXT;")?;
        }
        conn.execute_batch("DROP TABLE IF EXISTS conoti_card;")?;
    }
    if current < 6 && had_turn {
        // v5 → v6: 보관 시각
        let has_col: bool = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('session') WHERE name = 'archived_at'", [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)?;
        if !has_col {
            conn.execute_batch("ALTER TABLE session ADD COLUMN archived_at TEXT;")?;
        }
    }
    if current < 7 && had_turn {
        // v6 → v7: 기기별 기록 관리 허용
        let has_col: bool = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('relay_device') WHERE name = 'can_manage'", [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)?;
        if !has_col {
            // 이미 연결된 기기는 "답 보내기 허용"을 그대로 따른다 — 답을 막아 둔 기기에 지우기 권한이 생기지 않게
            conn.execute_batch(
                "ALTER TABLE relay_device ADD COLUMN can_manage INTEGER NOT NULL DEFAULT 0;
                 UPDATE relay_device SET can_manage = can_reply;",
            )?;
        }
    }
    if current < 8 && had_turn {
        // v7 → v8: 폰 말의 대기 시계 — 세션이 일하는 동안 기다린 시간은 10분에 넣지 않는다
        let has_col: bool = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('conoti_reply') WHERE name = 'wait_from'", [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)?;
        if !has_col {
            conn.execute_batch("ALTER TABLE conoti_reply ADD COLUMN wait_from TEXT;")?;
        }
    }
    if current < 9 && had_turn {
        // v8 → v9: Codex 세션도 모은다 — 세션마다 어느 도구의 것인지
        let has_col: bool = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('session') WHERE name = 'agent'", [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)?;
        if !has_col {
            conn.execute_batch("ALTER TABLE session ADD COLUMN agent TEXT;")?;
        }
    }
    if current < 10 && had_turn {
        // v9 → v10: 메신저식 답장 — 어느 요청·결과에 단 말인가
        let has_col: bool = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('conoti_reply') WHERE name = 'quote'", [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)?;
        if !has_col {
            conn.execute_batch("ALTER TABLE conoti_reply ADD COLUMN quote TEXT;")?;
        }
    }
    if current < SCHEMA_VERSION {
        set_meta(conn, "schema_version", &SCHEMA_VERSION.to_string())?;
    }
    // 오래된 훅 이벤트는 90일까지만
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(90)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    conn.execute("DELETE FROM hook_event WHERE at < ?1", params![cutoff])?;
    if get_meta(conn, "installed_at").is_none() {
        set_meta(conn, "installed_at", &crate::time::now_iso())?;
    }
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Option<String> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |r| r.get(0))
        .optional()
        .ok()
        .flatten()
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// 앱 설정 (meta 테이블의 setting.* 키)
pub fn setting_i64(conn: &Connection, key: &str, default: i64) -> i64 {
    get_meta(conn, &format!("setting.{key}"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
