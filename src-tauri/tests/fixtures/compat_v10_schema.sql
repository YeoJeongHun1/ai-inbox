CREATE TABLE turn (
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
);
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT
);
CREATE TABLE source_file (
    path       TEXT PRIMARY KEY,
    session_id TEXT,
    size       INTEGER NOT NULL DEFAULT 0,
    mtime_ms   INTEGER NOT NULL DEFAULT 0,
    offset     INTEGER NOT NULL DEFAULT 0,   -- 다음 시작 위치 = 아직 열린 마지막 요청의 첫 줄
    skipped    INTEGER NOT NULL DEFAULT 0    -- 백필 기간 밖이라 그 뒤로 늘어난 부분만 따라가는 파일
);
CREATE TABLE session (
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
CREATE INDEX turn_session_seq ON turn (session_id, seq);
CREATE INDEX turn_unread ON turn (session_id) WHERE read_at IS NULL;
CREATE INDEX turn_prompt_at ON turn (prompt_at DESC);
CREATE TABLE turn_tool (
    turn_id   INTEGER NOT NULL REFERENCES turn(id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    calls     INTEGER NOT NULL,
    PRIMARY KEY (turn_id, tool_name)
);
CREATE TABLE turn_file (
    turn_id INTEGER NOT NULL REFERENCES turn(id) ON DELETE CASCADE,
    path    TEXT NOT NULL,
    edits   INTEGER NOT NULL,
    PRIMARY KEY (turn_id, path)
);
CREATE TABLE turn_subagent (
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
CREATE TABLE turn_step (
    turn_id INTEGER NOT NULL REFERENCES turn(id) ON DELETE CASCADE,
    seq     INTEGER NOT NULL,
    at      TEXT,
    kind    TEXT NOT NULL,   -- text | tool | task | compact | error | interrupt | continue | summary
    name    TEXT,
    text    TEXT,
    PRIMARY KEY (turn_id, seq)
);
CREATE TABLE turn_deleted (
    session_id  TEXT NOT NULL,
    prompt_uuid TEXT NOT NULL,
    deleted_at  TEXT NOT NULL,
    PRIMARY KEY (session_id, prompt_uuid)
);
CREATE INDEX turn_deleted_uuid ON turn_deleted (prompt_uuid);
CREATE TABLE hook_event (
    id         INTEGER PRIMARY KEY,
    session_id TEXT,
    event      TEXT NOT NULL,
    at         TEXT NOT NULL,
    detail     TEXT
);
CREATE INDEX hook_event_session ON hook_event (session_id, at);
CREATE TABLE conoti_session (
    session_id TEXT PRIMARY KEY,
    mode       INTEGER NOT NULL DEFAULT 0,
    enabled_at TEXT
);
CREATE TABLE relay_device (
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
CREATE TABLE conoti_reply (
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
CREATE INDEX conoti_reply_state ON conoti_reply (state);
CREATE INDEX conoti_reply_received ON conoti_reply (received_at);
CREATE TABLE attachment (
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
CREATE INDEX attachment_created ON attachment (created_at);
CREATE TABLE reply_attachment (
    reply_id TEXT NOT NULL,
    att_id   TEXT NOT NULL,
    ord      INTEGER NOT NULL,
    PRIMARY KEY (reply_id, ord)
);
CREATE INDEX reply_attachment_att ON reply_attachment (att_id);
CREATE TABLE att_upload (
    device TEXT NOT NULL,
    rid    TEXT NOT NULL,
    ord    INTEGER NOT NULL,
    att_id TEXT NOT NULL,
    at     TEXT NOT NULL,
    PRIMARY KEY (device, rid, ord)
);
