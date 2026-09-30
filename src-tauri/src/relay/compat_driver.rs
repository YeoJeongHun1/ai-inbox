//! 구 폰 시뮬레이터 — 운영 코노티 앱(1.5.0 · 1.6.0)의 `ai_relay` 클라이언트가 PC 에 보내는 rpc 를
//! `Runner::call`(진짜 요청 처리 경로) 로 그대로 재현한다. 요청 모양은 폰 소스(`relay_controllers.dart`)에서 옮겼다:
//!
//! | 폰 호출 | 파라미터 |
//! |---|---|
//! | hello | `{name, app, ticket?, acct}` |
//! | sessions | `{filter}` (`all`·`unread`·`attention`·`active`·`archived`) |
//! | chat | `{sid, before?, limit}` |
//! | turn · read | `{id}` · `{ids}` 또는 `{sid}` |
//! | reply | `{sid, turn_id?, text, rid, atts?, quote?}` (`quote` 는 1.6.0 부터) |
//! | att · att_get | `{rid, i, data}` · `{id, size}` |
//! | manage · tidy · unpair | `{op, sids}` · `{kind}` · `{}` |
//!
//! 이 파일은 0.8.0 트리(기준선 스냅샷을 만드는 쪽)와 0.10.0 트리에 **글자 그대로 같은 것**이 들어간다 —
//! 같은 입력으로 두 버전의 응답을 비교하려면 입력이 한 글자도 다르면 안 된다.
//! 값은 전부 지어낸 것이다(개인 경로·계정 없음).

use super::*;
use crate::relay::crypto::generate_keypair;

pub const PID_MANAGE: &str = "0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a";
pub const PID_LITE: &str = "0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b0b";

fn png_b64() -> String {
    use base64::Engine;
    let img = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(31, 7, image::Rgb([9, 8, 7])));
    let mut b = std::io::Cursor::new(Vec::new());
    img.write_to(&mut b, image::ImageFormat::Png).unwrap();
    base64::engine::general_purpose::STANDARD.encode(b.into_inner())
}

/// 고정 시각의 지어낸 세션·요청·기기. 요청은 상태가 골고루 섞이게(끝남·진행 중·입력 필요·중단·폰 답·다른 세션이 보냄).
pub fn seed(conn: &Connection) {
    let sess = |id: &str, name: Option<&str>, title: Option<&str>, dir: &str, pinned: i64, agent: Option<&str>, last: &str| {
        conn.execute(
            "INSERT INTO session (id, project_dir, git_branch, model, cost_usd, first_at, last_at, live_name, title, pinned, agent)
             VALUES (?1, ?2, 'main', 'claude-fixture-1', 1.25, '2026-08-05T00:00:00.000Z', ?3, ?4, ?5, ?6, ?7)",
            params![id, dir, last, name, title, pinned, agent],
        )
        .unwrap();
    };
    sess("sess-alpha-0001", Some("api-refactor"), None, "/Users/x/proj", 1, None, "2026-08-10T03:00:00.000Z");
    sess("sess-beta-00002", None, Some("docs 정리"), "/Users/x/docs", 0, None, "2026-08-10T02:00:00.000Z");
    sess("sess-gamma-0003", Some("codex-run"), None, "/Users/x/lab", 0, Some("codex"), "2026-08-10T01:00:00.000Z");
    sess("sess-delta-0004", Some("old-idle"), None, "/Users/x/old", 0, None, "2026-08-01T00:00:00.000Z");
    let phone_prompt = format!("{}\n요청: 문서 정리\n\n2번으로 해 줘", crate::conoti::REPLY_HEADER);
    #[allow(clippy::type_complexity)]
    let turns: [(&str, i64, &str, Option<&str>, &str, Option<&str>, &str, i64, Option<&str>); 9] = [
        // (세션, seq, 요청 글, 출처, 상태, 결과, 시각, 입력 필요, peer)
        ("sess-alpha-0001", 1, "리팩터링 시작해 줘", None, "done", Some("끝났어요.\n\n1. 계속\n2. 멈춤\n\n어느 쪽이 좋을까요?"), "2026-08-10T00:10:00.000Z", 0, None),
        ("sess-alpha-0001", 2, "테스트도 돌려 줘", None, "running", None, "2026-08-10T03:00:00.000Z", 0, None),
        ("sess-beta-00002", 1, "문서 목차 만들어 줘", None, "done", Some("목차를 만들었어요."), "2026-08-10T01:00:00.000Z", 1, None),
        ("sess-beta-00002", 2, phone_prompt.as_str(), None, "done", Some("2번으로 정리했어요."), "2026-08-10T02:00:00.000Z", 0, None),
        ("sess-beta-00002", 3, "다른 세션이 보낸 말", Some("peer"), "interrupted", None, "2026-08-10T02:10:00.000Z", 0, Some("worker-2")),
        ("sess-gamma-0003", 1, "코덱스로 돌려 줘", None, "done", Some("완료"), "2026-08-10T01:00:00.000Z", 0, None),
        ("sess-delta-0004", 1, "오래된 요청", None, "done", Some("오래전 결과"), "2026-08-01T00:00:00.000Z", 0, None),
        ("sess-delta-0004", 2, "채널로 온 요청", Some("channel"), "stopped", None, "2026-08-01T00:10:00.000Z", 0, None),
        ("sess-alpha-0001", 3, "작업 중에 보낸 말", None, "background", None, "2026-08-10T03:05:00.000Z", 0, None),
    ];
    for (sid, seq, prompt, origin, status, resp, at, needs, peer) in turns {
        conn.execute(
            "INSERT INTO turn (session_id, prompt_uuid, seq, origin, peer_name, prompt_at, prompt_text, response_text, summary, status, needs_input, model, duration_ms, tool_calls, files_changed, subagent_count, output_tokens, ended_at, read_at, starred)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, '요약 한 줄', ?9, ?10, 'claude-fixture-1', 4200, 3, 1, 0, 120, ?11, ?12, ?13)",
            params![
                sid,
                format!("u-{sid}-{seq}"),
                seq,
                origin,
                peer,
                at,
                prompt,
                resp,
                status,
                needs,
                if matches!(status, "running" | "background") { None } else { Some(at) },
                // 첫 세션의 1번·베타의 3번만 읽음 — 나머지 끝난 요청은 안 읽음
                if (sid == "sess-alpha-0001" && seq == 1) || (sid == "sess-beta-00002" && seq == 3) { Some(at) } else { None },
                (seq == 1 && sid == "sess-alpha-0001") as i64,
            ],
        )
        .unwrap();
    }
    for (pid, name, manage) in [(PID_MANAGE, "iPhone 지어냄", 1), (PID_LITE, "Pixel 지어냄", 0)] {
        conn.execute(
            "INSERT INTO relay_device (pid, name, phone_pub, can_reply, can_manage, created_at) VALUES (?1, ?2, 'ab', 1, ?3, '2026-08-05T00:00:00.000Z')",
            params![pid, name, manage],
        )
        .unwrap();
    }
    db::set_meta(conn, "conoti.bg_resume", "1").unwrap();
}

/// 폰이 하는 일을 순서대로. 반환 = 호출 이름 → PC 가 돌려준 봉투(`{id, ok, r|err,msg}`). 폰이 한 번도 안 부르는 것은 넣지 않는다.
pub fn run_script(conn: &Connection) -> Value {
    let kp = generate_keypair().unwrap();
    let id = Identity { key: kp.private, public: kp.public, secret: [9u8; 32], psks: HashMap::new() };
    let shared = Shared::new();
    shared.set_identity_for_test(id.clone());
    let hooks = Hooks { on_pair_request: Box::new(|_, _| {}), on_local_change: Box::new(|_| {}) };
    let mut r = Runner {
        shared: &shared,
        conn,
        hooks: &hooks,
        host: "dev.conoti.app",
        env: "dev".into(),
        name: "PC",
        version: "t",
        id,
        chans: HashMap::new(),
        closing: Vec::new(),
        sent: VecDeque::new(),
    };
    let mut out = serde_json::Map::new();
    let mut n = 0;
    let mut call = |r: &mut Runner, name: &str, pid: &str, m: &str, p: Value| {
        n += 1;
        let (resp, _close) = r.call(pid, &json!({"id": n, "m": m, "p": p}));
        out.insert(name.to_string(), resp);
    };
    let (m, l) = (PID_MANAGE, PID_LITE);
    let first_turn = |sid: &str, seq: i64| -> i64 { conn.query_row("SELECT id FROM turn WHERE session_id = ?1 AND seq = ?2", params![sid, seq], |r| r.get(0)).unwrap() };
    let (a1, a2, b2) = (first_turn("sess-alpha-0001", 1), first_turn("sess-alpha-0001", 2), first_turn("sess-beta-00002", 2));

    call(&mut r, "hello", m, "hello", json!({"name": "iPhone 지어냄", "app": "1.6.0", "ticket": "pt1.fixture", "acct": "0123456789abcdef"}));
    call(&mut r, "hello_lite", l, "hello", json!({"name": "Pixel 지어냄", "app": "1.5.0", "acct": "fedcba9876543210"}));
    for f in ["all", "unread", "attention", "active"] {
        call(&mut r, &format!("sessions_{f}"), m, "sessions", json!({"filter": f}));
    }
    call(&mut r, "sessions_lite", l, "sessions", json!({"filter": "all"}));
    call(&mut r, "sessions_archived_denied", l, "sessions", json!({"filter": "archived"}));
    call(&mut r, "chat_alpha", m, "chat", json!({"sid": "sess-alpha-0001", "limit": 30}));
    call(&mut r, "chat_alpha_page", m, "chat", json!({"sid": "sess-alpha-0001", "before": 3, "limit": 1}));
    call(&mut r, "chat_beta", m, "chat", json!({"sid": "sess-beta-00002", "limit": 30}));
    call(&mut r, "chat_gamma_codex", m, "chat", json!({"sid": "sess-gamma-0003", "limit": 30}));
    call(&mut r, "chat_missing", m, "chat", json!({"sid": "sess-nope-0009", "limit": 30}));
    call(&mut r, "turn_done", m, "turn", json!({"id": a1}));
    call(&mut r, "turn_running", m, "turn", json!({"id": a2}));
    call(&mut r, "turn_phone_reply", m, "turn", json!({"id": b2}));
    call(&mut r, "turn_missing", m, "turn", json!({"id": 99999}));
    call(&mut r, "read_ids", m, "read", json!({"ids": [a1]}));
    call(&mut r, "read_sid", m, "read", json!({"sid": "sess-beta-00002"}));
    call(&mut r, "sessions_after_read", m, "sessions", json!({"filter": "unread"}));
    // 답 보내기 — 1.5.0 형태(quote 없음), 1.6.0 답장(quote), 같은 rid 재전송, 잘못된 rid, 없는 세션
    call(&mut r, "reply_plain", m, "reply", json!({"sid": "sess-alpha-0001", "turn_id": a1, "text": "계속해 줘", "rid": "rid-fixture-0001"}));
    call(&mut r, "reply_quote", m, "reply", json!({"sid": "sess-alpha-0001", "turn_id": a1, "text": "이 결과 좋아요", "rid": "rid-fixture-0002", "quote": "response"}));
    call(&mut r, "reply_again", m, "reply", json!({"sid": "sess-alpha-0001", "turn_id": a1, "text": "계속해 줘", "rid": "rid-fixture-0001"}));
    call(&mut r, "reply_bad_rid", m, "reply", json!({"sid": "sess-alpha-0001", "text": "x", "rid": "no"}));
    call(&mut r, "reply_no_session", m, "reply", json!({"sid": "sess-nope-0009", "text": "x", "rid": "rid-fixture-0003"}));
    // 사진 — 올리기 → 답에 붙이기 → 받기(thumb·view)
    call(&mut r, "att_up", m, "att", json!({"rid": "rid-fixture-0004", "i": 0, "data": png_b64()}));
    let att_id = out["att_up"]["r"]["id"].as_str().unwrap_or("").to_string();
    let mut call = |r: &mut Runner, name: &str, pid: &str, m: &str, p: Value| {
        n += 1;
        let (resp, _close) = r.call(pid, &json!({"id": n, "m": m, "p": p}));
        out.insert(name.to_string(), resp);
    };
    call(&mut r, "reply_with_att", m, "reply", json!({"sid": "sess-beta-00002", "text": "사진 봐 줘", "rid": "rid-fixture-0004", "atts": [att_id.clone()]}));
    call(&mut r, "att_get_thumb", m, "att_get", json!({"id": att_id.clone(), "size": "thumb"}));
    call(&mut r, "att_get_view", m, "att_get", json!({"id": att_id.clone(), "size": "view"}));
    call(&mut r, "att_get_orig_denied", m, "att_get", json!({"id": att_id, "size": "orig"}));
    call(&mut r, "chat_alpha_after_reply", m, "chat", json!({"sid": "sess-alpha-0001", "limit": 30}));
    call(&mut r, "chat_beta_after_reply", m, "chat", json!({"sid": "sess-beta-00002", "limit": 30}));
    // 세션 관리(1.5.0 부터)
    call(&mut r, "manage_denied", l, "manage", json!({"op": "archive", "sids": ["sess-delta-0004"]}));
    call(&mut r, "tidy_denied", l, "tidy", json!({"kind": "short"}));
    call(&mut r, "sessions_tidy_counts", m, "sessions", json!({"filter": "all"}));
    call(&mut r, "manage_archive", m, "manage", json!({"op": "archive", "sids": ["sess-delta-0004"]}));
    call(&mut r, "sessions_archived", m, "sessions", json!({"filter": "archived"}));
    call(&mut r, "chat_archived", m, "chat", json!({"sid": "sess-delta-0004", "limit": 30}));
    call(&mut r, "manage_unarchive", m, "manage", json!({"op": "unarchive", "sids": ["sess-delta-0004"]}));
    call(&mut r, "manage_pin", m, "manage", json!({"op": "pin", "sids": ["sess-beta-00002"]}));
    call(&mut r, "manage_unpin", m, "manage", json!({"op": "unpin", "sids": ["sess-beta-00002"]}));
    call(&mut r, "manage_delete_busy", m, "manage", json!({"op": "delete", "sids": ["sess-alpha-0001"]}));
    call(&mut r, "tidy_idle", m, "tidy", json!({"kind": "idle"}));
    call(&mut r, "manage_delete", m, "manage", json!({"op": "delete", "sids": ["sess-gamma-0003"]}));
    call(&mut r, "sessions_final", m, "sessions", json!({"filter": "all"}));
    call(&mut r, "unknown_method", m, "no_such_method", json!({}));
    let (resp, close) = r.call(PID_LITE, &json!({"id": 900, "m": "unpair", "p": {}}));
    out.insert("unpair".into(), resp);
    out.insert("unpair_closes".into(), json!(close));
    let (resp, close) = r.call("ffffffffffffffffffffffffffffffff", &json!({"id": 901, "m": "sessions", "p": {}}));
    out.insert("unpaired_device".into(), resp);
    out.insert("unpaired_device_closes".into(), json!(close));
    Value::Object(out)
}
