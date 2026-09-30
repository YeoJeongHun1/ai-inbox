//! 내장 SQLite. 파일 하나(inbox.db)에 WAL 로 쌓는다.
//!
//! 정본은 Claude Code 대화 기록(~/.claude/projects/**/*.jsonl)과 Codex 대화 기록(~/.codex/sessions/**/rollout-*.jsonl)이고 이 DB 는 그걸
//! "요청 1건 = turn 1행" 으로 정리한 파생 저장소다. 단, 읽음·별표·고정 같은
//! **사용자 상태는 여기에만 있다** — 재수집해도 지워지지 않게 upsert 에서 건드리지 않는다.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

pub const SCHEMA_VERSION: i64 = 14;

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
    agent           TEXT,       -- 어느 도구의 세션인가(v9): NULL = Claude Code · 'codex' = OpenAI Codex
    cleared_at      TEXT,       -- /clear 로 끝난 시각(v11, SessionEnd reason=clear) — NULL = 끝나지 않음
    cleared_to      TEXT,       -- /clear 뒤 이어서 시작된 새 세션(SessionStart source=clear)
    clear_state     TEXT,       -- 끝난 대화의 처리(v11): 'purge' 삭제 예약 · 'keep' 이력으로 보관 · 'ask' 아직 안 정함(자동 삭제 없음)
    purge_at        TEXT,       -- 삭제 예정 시각(clear_state = 'purge')
    clear_asked     INTEGER NOT NULL DEFAULT 0,  -- 사용자가 확인·결정했나 — 0 이면 "결정해 주세요" 안내를 띄운다
    kept_at         TEXT
);
-- (session_clear_state 인덱스는 열 추가 마이그레이션 뒤에 만든다 — 옛 DB 에서 이 묶음이 먼저 돌기 때문)

-- 삭제 예약 세션을 지운 기록(v11). 값(대화 내용·이름)은 남기지 않고 건수만
CREATE TABLE IF NOT EXISTS purge_log (
    id       INTEGER PRIMARY KEY,
    at       TEXT NOT NULL,
    sessions INTEGER NOT NULL,
    turns    INTEGER NOT NULL,
    reason   TEXT NOT NULL
);

-- 요청 태그(v12) — 한 세션 안에서 여러 주제를 다룰 때 주제별로 나눠 보려는 표식. 사용자 데이터라 이 표들은 재수집이 건드리지 않는다.
CREATE TABLE IF NOT EXISTS tag (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL COLLATE NOCASE UNIQUE,
    color      TEXT NOT NULL DEFAULT '',      -- '#rrggbb' · 빈 값 = 이름으로 정한 자동 색
    minor      INTEGER NOT NULL DEFAULT 0,    -- 1 = 작은 태그(종류 표식) — 사이드바·대표 태그에서 뒤로
    created_at TEXT NOT NULL
);
-- 자동 태깅 규칙: kind = path(요청이 건드린 경로·작업 폴더에 들어 있는 조각) | keyword(요청·응답 글 속 낱말)
CREATE TABLE IF NOT EXISTS tag_rule (
    id      INTEGER PRIMARY KEY,
    tag_id  INTEGER NOT NULL REFERENCES tag(id) ON DELETE CASCADE,
    kind    TEXT NOT NULL,
    pattern TEXT NOT NULL,
    source  TEXT NOT NULL DEFAULT 'user',     -- user | default | suggested
    UNIQUE (tag_id, kind, pattern)
);
-- 요청 ↔ 태그. state: auto(규칙) · manual(사용자가 붙임) · ai(모델 제안 — 사용자가 받아들이기 전) · off(사용자가 뗌 — 자동으로 다시 붙이지 않는다)
CREATE TABLE IF NOT EXISTS turn_tag (
    turn_id INTEGER NOT NULL REFERENCES turn(id) ON DELETE CASCADE,
    tag_id  INTEGER NOT NULL REFERENCES tag(id) ON DELETE CASCADE,
    state   TEXT NOT NULL,
    score   REAL NOT NULL DEFAULT 0,
    at      TEXT NOT NULL,
    src     TEXT,                              -- hook = 요청 시점 훅이 정함(0.9.1) · NULL = 0.9.0 의 뒤늦은 규칙 계산(v13 이 지운다)
    PRIMARY KEY (turn_id, tag_id)
);
CREATE INDEX IF NOT EXISTS turn_tag_tag ON turn_tag (tag_id, turn_id) WHERE state IN ('auto', 'manual');
-- 자동 태깅을 마친 요청: 규칙 판(meta tags.rev)이나 요청 내용(turn.updated_at)이 바뀌면 다시 본다
CREATE TABLE IF NOT EXISTS turn_tagged (
    turn_id      INTEGER PRIMARY KEY REFERENCES turn(id) ON DELETE CASCADE,
    rev          INTEGER NOT NULL,
    turn_updated TEXT,
    at           TEXT NOT NULL,
    ai_at        TEXT                          -- 모델에 제안을 물어본 시각(같은 요청을 두 번 묻지 않는다)
);
-- 요청 시점 훅 기록: 요청을 보낼 때 정한 태그(원문은 없다). 대화 기록에서 같은 요청을 읽으면 turn_id 로 이어진다
CREATE TABLE IF NOT EXISTS turn_hint (
    id         INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL,
    key        TEXT NOT NULL,                  -- 프롬프트 지문(정규화한 앞 500자의 SHA-256 앞 8바이트)
    at_ms      INTEGER NOT NULL,
    src        TEXT NOT NULL,                  -- hashtag | rule | project | kind | inherit | none
    tag_ids    TEXT NOT NULL DEFAULT '',
    exact      INTEGER NOT NULL DEFAULT 0,     -- 1 = 앱이 직접 기록(입력창·폰) — 지문이 같을 때만 잇는다
    turn_id    INTEGER                         -- 이어진 요청(아직 없으면 NULL)
);
CREATE INDEX IF NOT EXISTS turn_hint_session ON turn_hint (session_id, at_ms);
CREATE INDEX IF NOT EXISTS turn_hint_turn ON turn_hint (turn_id) WHERE turn_id IS NOT NULL;
-- 요청이 읽거나 고친 경로(작업 폴더·Read·Edit·Grep·Bash 속 절대경로) — 태깅 신호. 이 PC 안에만 있다
CREATE TABLE IF NOT EXISTS turn_touch (
    turn_id INTEGER PRIMARY KEY REFERENCES turn(id) ON DELETE CASCADE,
    paths   TEXT NOT NULL
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
    can_schedule INTEGER NOT NULL DEFAULT 0, -- 폰에서 예약 전송을 만들기·고치기·취소·처리(v14) — 기본 끔, PC 설정에서 기기마다 켠다
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
    quote        TEXT,           -- 답장 대상(v10): 'prompt' | 'response' — turn_id 의 요청·결과에 단 답. NULL = 그냥 이어서
    sched        TEXT            -- 예약 전송에서 온 줄(v14): 예약 회차 키(schedule_run). NULL = 예약이 아님 — 꺼진 세션을 이어서 실행하지 않는다
);
CREATE INDEX IF NOT EXISTS conoti_reply_state ON conoti_reply (state);
CREATE INDEX IF NOT EXISTS conoti_reply_received ON conoti_reply (received_at);

-- ── 예약 전송(v14) — 정본 문서는 docs/SCHEDULE.md ────────────────────────────
-- 예약은 대기열(conoti_reply) 밖에 둔다 — 시각이 되는 순간에만 대기열에 한 줄을 넣는다(그 줄의 received_at = 발사 시각이라 3시간 상한이 발사 기준)
CREATE TABLE IF NOT EXISTS schedule (
    id               TEXT PRIMARY KEY,           -- 'sc' + hex16
    rid              TEXT,                       -- 폰이 만든 예약의 멱등 키(같은 rid 로 다시 만들면 처음 것을 돌려준다)
    session_id       TEXT NOT NULL,
    text             TEXT NOT NULL,
    quote            TEXT,                       -- 답장 대상('prompt'|'response') · turn_id 의 요청
    turn_id          INTEGER,
    created_by       TEXT NOT NULL,              -- 'desktop' (폰 예약은 2단계)
    created_at       TEXT NOT NULL,
    updated_at       TEXT NOT NULL,
    rev              INTEGER NOT NULL DEFAULT 1, -- 낙관적 잠금
    kind             TEXT NOT NULL,              -- once | after (cron 은 2단계)
    tz               TEXT NOT NULL,              -- 만든 곳의 IANA 시간대
    cron             TEXT,                       -- 2단계
    until_at         TEXT,                       -- 2단계
    max_runs         INTEGER,                    -- 2단계
    runs             INTEGER NOT NULL DEFAULT 0,
    next_due_at      TEXT,                       -- UTC ISO. NULL = 더 발사할 회차 없음
    on_missed        TEXT NOT NULL,              -- run_once | skip | within (놓친 예약 처리 — 예약마다)
    missed_within_min INTEGER,
    busy_policy      TEXT,                       -- NULL(규칙을 따름) | interrupt | after_work | after_quiet (바쁜 세션 처리 — 예약마다 덮어쓰기)
    state            TEXT NOT NULL               -- active | done | cancelled | paused
);
-- 회차. 상태: pending(발사됨·판단 전) · deferred(바쁜 세션/방해금지 창 뒤로 미룸) · fired(대기열에 넣음) · delivered · handled ·
--            held(못 받아 대기 — 사용자가 보내기/버리기) · missed(놓침) · failed(정책상 막음) · cancelled
CREATE TABLE IF NOT EXISTS schedule_run (
    schedule_id   TEXT NOT NULL,
    occurrence_at TEXT NOT NULL,                 -- 원래 발사 시각(UTC ISO)
    created_at    TEXT NOT NULL,                 -- 발사 처리를 시작한 시각(미룸·3시간 상한의 기준)
    fired_at      TEXT,
    reply_id      TEXT,                          -- conoti_reply 로 넘긴 줄
    state         TEXT NOT NULL,
    reason        TEXT,                          -- held 사유 코드: ended | terminal | perm | busy_limit | stuck | policy | cap | rejected
    note          TEXT,
    notified_at   TEXT,                          -- 폰·화면 알림 1회(조건부 UPDATE 로 한 번만)
    renotified_at TEXT,                          -- 다시 받을 수 있게 됐을 때 알림 1회
    PRIMARY KEY (schedule_id, occurrence_at)
);
CREATE TABLE IF NOT EXISTS schedule_att (
    schedule_id TEXT NOT NULL,
    att_id      TEXT NOT NULL,
    ord         INTEGER NOT NULL,
    PRIMARY KEY (schedule_id, ord)
);
-- 방해금지 창(시간대 규칙). days = 월(1)~일(64) 비트
CREATE TABLE IF NOT EXISTS quiet_window (
    id       INTEGER PRIMARY KEY,
    name     TEXT NOT NULL DEFAULT '',
    days     INTEGER NOT NULL DEFAULT 127,
    start_hm TEXT NOT NULL,
    end_hm   TEXT NOT NULL,
    tz       TEXT NOT NULL,
    enabled  INTEGER NOT NULL DEFAULT 1
);
-- 바쁜 세션 규칙: scope = session(key = 세션 ID) | tag(key = 태그 id) — action = interrupt | after_work | after_quiet
CREATE TABLE IF NOT EXISTS busy_rule (
    id     INTEGER PRIMARY KEY,
    scope  TEXT NOT NULL,
    key    TEXT NOT NULL,
    action TEXT NOT NULL,
    UNIQUE (scope, key)
);
-- 예약을 받을 수 있는 세션 목록(설정 "허용 세션 목록"일 때만 쓴다)
CREATE TABLE IF NOT EXISTS sched_allow (
    session_id TEXT PRIMARY KEY,
    added_at   TEXT NOT NULL
);

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
    if current < 11 && had_turn {
        // v10 → v11: /clear 로 끝난 대화의 처리(삭제 예약·보관). 기존 데이터는 바꾸지 않고 열 추가만 한다 —
        // 그 전에 /clear 된 세션은 '아직 안 정함'(자동 삭제 없음)으로 표시만 한다(되돌릴 수 없는 삭제를 소급하지 않는다).
        // 바꾸기 전에 DB 사본을 한 벌 남긴다(이미 있으면 그대로).
        backup_before(conn, "v10");
        for (col, ddl) in [
            ("cleared_at", "ALTER TABLE session ADD COLUMN cleared_at TEXT;"),
            ("cleared_to", "ALTER TABLE session ADD COLUMN cleared_to TEXT;"),
            ("clear_state", "ALTER TABLE session ADD COLUMN clear_state TEXT;"),
            ("purge_at", "ALTER TABLE session ADD COLUMN purge_at TEXT;"),
            ("clear_asked", "ALTER TABLE session ADD COLUMN clear_asked INTEGER NOT NULL DEFAULT 0;"),
            ("kept_at", "ALTER TABLE session ADD COLUMN kept_at TEXT;"),
        ] {
            let has: bool = conn
                .query_row("SELECT COUNT(*) FROM pragma_table_info('session') WHERE name = ?1", [col], |r| r.get::<_, i64>(0))
                .map(|n| n > 0)?;
            if !has {
                conn.execute_batch(ddl)?;
            }
        }
        conn.execute_batch(
            "UPDATE session SET cleared_at = ended_at, clear_state = 'ask', clear_asked = 1
              WHERE end_reason = 'clear' AND cleared_at IS NULL AND ended_at IS NOT NULL;",
        )?;
    }
    if current < 12 && had_turn {
        // v11 → v12: 요청 태그 — 새 표만 추가한다(위 SCHEMA 가 이미 만들었다). 기존 데이터는 바꾸지 않으니 사본만 남긴다.
        backup_before(conn, "v11");
    }
    if current < 13 && had_turn {
        // v12 → v13: 태그를 요청 시점 훅이 정한다 — 0.9.0 이 뒤늦게 낱말·경로로 붙인 자동 표식(src 없는 auto)만 지운다.
        // 사용자가 붙이고 뗀 것(manual · off)과 모델 제안(ai)은 손대지 않는다. 열이 없을 때(= 처음 이 단계를 밟을 때)만 지워 두 번 돌아도 안전하다.
        // 지우기 전에 사본을 한 벌 남긴다(이미 있으면 그대로).
        let has_src: bool = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('turn_tag') WHERE name = 'src'", [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)?;
        if !has_src {
            backup_before(conn, "v12");
            conn.execute_batch(
                "ALTER TABLE turn_tag ADD COLUMN src TEXT;
                 DELETE FROM turn_tag WHERE state = 'auto' AND src IS NULL;
                 DELETE FROM turn_tagged WHERE ai_at IS NULL;",
            )?;
            let _ = conn.execute("DELETE FROM meta WHERE key = 'tags.rev'", []);
        }
    }
    if current < 14 && had_turn {
        // 폰 예약 허용(기기별, 기본 끔)
        let has_dev: bool = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('relay_device') WHERE name = 'can_schedule'", [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)?;
        if !has_dev {
            backup_before(conn, "v13");
            conn.execute_batch("ALTER TABLE relay_device ADD COLUMN can_schedule INTEGER NOT NULL DEFAULT 0;")?;
        }
        // v13 → v14: 예약 전송 — 새 표는 위 SCHEMA 가 만들었고, 대기열(conoti_reply)에 예약 표식 열 하나만 더한다. 기존 데이터는 바꾸지 않는다.
        // (새 열의 인덱스는 SCHEMA 묶음에 넣지 않았다 — 옛 DB 에서 묶음이 먼저 돈다.) 바꾸기 전에 사본을 한 벌 남긴다(이미 있으면 그대로).
        let has_col: bool = conn
            .query_row("SELECT COUNT(*) FROM pragma_table_info('conoti_reply') WHERE name = 'sched'", [], |r| r.get::<_, i64>(0))
            .map(|n| n > 0)?;
        if !has_col {
            backup_before(conn, "v13");
            conn.execute_batch("ALTER TABLE conoti_reply ADD COLUMN sched TEXT;")?;
        }
    }
    conn.execute_batch("CREATE INDEX IF NOT EXISTS conoti_reply_sched ON conoti_reply (sched) WHERE sched IS NOT NULL;")?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS schedule_due ON schedule (state, next_due_at);")?;
    conn.execute_batch("CREATE UNIQUE INDEX IF NOT EXISTS schedule_rid ON schedule (created_by, rid) WHERE rid IS NOT NULL;")?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS schedule_session ON schedule (session_id, state);")?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS schedule_run_open ON schedule_run (state);")?;
    conn.execute_batch("CREATE INDEX IF NOT EXISTS session_clear_state ON session (clear_state) WHERE clear_state IS NOT NULL;")?;
    if current < SCHEMA_VERSION || get_meta(conn, "schema_version").is_none() {
        set_meta(conn, "schema_version", &SCHEMA_VERSION.to_string())?;
    }
    // 오래된 훅 이벤트는 90일까지만
    let cutoff = (chrono::Utc::now() - chrono::Duration::days(90)).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    conn.execute("DELETE FROM hook_event WHERE at < ?1", params![cutoff])?;
    crate::tags::seed_defaults(conn);
    if get_meta(conn, "installed_at").is_none() {
        set_meta(conn, "installed_at", &crate::time::now_iso())?;
    }
    Ok(())
}

/// 마이그레이션 전 DB 사본(`inbox.db.bak-<이름>`, 본인만 읽기). 파일 DB 일 때만, 이미 있으면 건드리지 않는다. 실패해도 마이그레이션은 계속한다.
fn backup_before(conn: &Connection, tag: &str) {
    let Some(path) = conn.path().map(std::path::PathBuf::from) else { return };
    if path.as_os_str().is_empty() {
        return;
    }
    let dst = std::path::PathBuf::from(format!("{}.bak-{tag}", path.display()));
    if dst.exists() {
        return;
    }
    if conn.execute("VACUUM INTO ?1", params![dst.to_string_lossy()]).is_ok() {
        crate::paths::make_private_file(&dst);
    }
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
