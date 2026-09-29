//! Codex 대화 기록(rollout JSONL) 해석 — `ingest.rs` 의 요청 누산기(`TurnAcc`)에 Claude Code 와 같은 모양으로 쌓는다.
//!
//! 요청 1건 = Codex 턴 1개(`task_started` … `task_complete` | `turn_aborted`). 두 형식을 읽는다(2026-09 실측, 0.146~0.156):
//!   - 0.150 이후: event_msg `item_completed` 의 item — UserMessage · AgentMessage · CommandExecution · FileChange ·
//!     McpToolCall · Extension · ImageView · SubAgentActivity · ContextCompaction
//!   - 그 전: event_msg `user_message` · `agent_message` · `patch_apply_end` · `context_compacted` 와
//!     response_item `function_call`·`custom_tool_call`(도구)
//!   새 형식도 response_item 에 도구 호출을 남긴다 — 같은 동작을 두 번 세지 않게 **턴마다 한쪽만** 센다
//!   (그 턴의 사용자 말이 item 으로 왔거나 파일에서 item 을 본 적이 있으면 item 쪽).
//! 한 턴에 사용자 말이 여럿일 수 있다(대기열 말이 합쳐지거나 일하는 도중 보낸 말). 모델이 아무것도 하기 전이면 앞 말에 합치고,
//! 그 뒤면 Claude 쪽 "작업 중에 보낸 말"과 같이 따로 한 쌍으로 둔다(답 = 그 뒤 첫 에이전트 말).
//! 건너뛰는 것:
//!   - 하위 에이전트 스레드(session_meta.source.subagent) — 파일째. 부모 턴의 하위 에이전트(SubAgentActivity)로만 센다
//!   - 가져온 대화: Claude Code 대화를 Codex 로 가져오면 그 사본 끝에 agent_message "<EXTERNAL SESSION IMPORTED>" 가 붙는다 —
//!     그 앞은 이미 Claude 쪽에서 모은 대화라 버린다(그 뒤 Codex 에서 이어 간 턴만 받는다)
//! 토큰: token_count 가 같은 값을 여러 번 적는다 — 스레드 누계(total_tokens)가 늘 때만 그 호출의 사용량을 더한다.
//! 입력 토큰은 OpenAI 방식(캐시 포함)이라 캐시 읽기를 빼서 Claude 칸(input · cache_read)에 맞춘다.

use serde_json::Value;

use super::{FileState, Line, PlanItem, SessionPatch, Sub, TurnAcc, Usage};
use crate::{text, time};

pub(super) const IMPORT_MARKER: &str = "<EXTERNAL SESSION IMPORTED>";

/// 파일 하나를 읽는 동안의 Codex 쪽 상태
#[derive(Default)]
pub(super) struct CxState {
    /// 하위 에이전트 스레드 — 통째로 건너뛴다
    pub skip: bool,
    /// 이 파일에서 item_completed 형식을 본 적이 있다
    items_seen: bool,
    /// 지금 턴의 도구는 item 쪽으로 센다
    turn_items: bool,
    /// 지금 턴에 사용자 말이 들어왔다
    turn_prompt: bool,
    /// 지금 턴에 모델이 무언가 했다(글·도구)
    turn_output: bool,
    model: Option<String>,
    effort: Option<String>,
    cwd: Option<String>,
    branch: Option<String>,
    /// 스레드 토큰 누계 — 늘 때만 센다
    last_total: i64,
    /// 지금 턴에 모델 쪽 기록(도구 호출·응답·항목)이 하나라도 있었나 — 그 전의 token_count 는 앞 턴 누계의 반복이다
    turn_activity: bool,
    /// 포크한 스레드: 이 줄 번호(`ordinal`) 앞은 부모 기록의 사본이라 버린다
    copied_until: Option<i64>,
}

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn n(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0).clamp(0, 10_000_000_000)
}

pub(super) fn feed(st: &mut FileState, line_start: u64, raw: &[u8], closed: &mut Vec<TurnAcc>, patch: &mut SessionPatch) {
    if st.cx.as_ref().is_none_or(|c| c.skip) {
        return;
    }
    let Ok(v) = serde_json::from_slice::<Value>(raw) else { return };
    let typ = s(&v, "type").unwrap_or("");
    if let (Some(until), Some(ord)) = (st.cx.as_ref().and_then(|c| c.copied_until), v.get("ordinal").and_then(Value::as_i64)) {
        if ord < until && typ != "session_meta" {
            return; // 포크 때 부모에서 복사해 온 줄
        }
    }
    let at = s(&v, "timestamp").and_then(time::normalize);
    let null = Value::Null;
    let p = v.get("payload").unwrap_or(&null);

    match typ {
        "session_meta" => {
            let cx = st.cx.as_mut().expect("cx");
            let sub = p.get("source").and_then(|x| x.get("subagent")).is_some() || s(p, "thread_source") == Some("subagent");
            if sub {
                cx.skip = true;
                st.open = None;
                st.side.clear();
                closed.clear();
                return;
            }
            if let Some(c) = s(p, "cwd").filter(|c| !c.is_empty()) {
                cx.cwd = Some(c.to_string());
                if patch.cwd.is_none() {
                    patch.cwd = Some(c.to_string());
                }
            }
            if let Some(b) = p.get("git").and_then(|g| s(g, "branch")).filter(|b| !b.is_empty()) {
                cx.branch = Some(b.to_string());
                patch.branch = Some(b.to_string());
            }
            if let Some(ver) = s(p, "cli_version").filter(|x| !x.is_empty()) {
                patch.version = Some(ver.to_string());
            }
            if s(p, "forked_from_id").is_some() {
                // 포크: 부모 기록을 새 시각으로 복사해 담는다 — 그 끝 줄 번호(…history_start_ordinal) 앞은 버린다
                cx.copied_until = p
                    .as_object()
                    .and_then(|m| m.iter().find(|(k, _)| k.ends_with("history_start_ordinal")).and_then(|(_, v)| v.as_i64()));
            }
            stamp(patch, &at);
            return;
        }
        "turn_context" => {
            let cx = st.cx.as_mut().expect("cx");
            if let Some(m) = s(p, "model").filter(|m| !m.is_empty()) {
                cx.model = Some(m.to_string());
                patch.model = Some(m.to_string());
            }
            if let Some(e) = s(p, "effort").or_else(|| s(p, "reasoning_effort")).filter(|e| !e.is_empty()) {
                cx.effort = Some(e.to_string());
            }
            if let Some(c) = s(p, "cwd").filter(|c| !c.is_empty()) {
                cx.cwd = Some(c.to_string());
                // session_meta 를 이번에 못 읽었어도(요청 없던 스레드에 턴이 붙은 경우) 작업 폴더는 채운다
                if patch.cwd.is_none() {
                    patch.cwd = Some(c.to_string());
                }
            }
            let (model, effort, cwd) = (cx.model.clone(), cx.effort.clone(), cx.cwd.clone());
            if let Some(acc) = st.open.as_mut() {
                if let Some(m) = model {
                    acc.models.entry(m).or_default();
                }
                if let Some(e) = effort {
                    acc.efforts.entry(e).or_default();
                }
                if acc.cwd.is_none() {
                    acc.cwd = cwd;
                }
            }
            return;
        }
        "event_msg" | "response_item" => stamp(patch, &at),
        _ => return,
    }

    if typ == "response_item" {
        on_response_item(st, p, &at);
        return;
    }
    match s(p, "type").unwrap_or("") {
        "task_started" => {
            let cx = st.cx.as_mut().expect("cx");
            cx.turn_items = cx.items_seen;
            cx.turn_prompt = false;
            cx.turn_output = false;
            cx.turn_activity = false;
            let uuid = s(p, "turn_id").filter(|t| !t.is_empty()).map(str::to_string).unwrap_or_else(|| format!("pos-{line_start}"));
            let acc = new_acc(st, uuid, at.clone().unwrap_or_else(time::now_iso));
            begin(st, line_start, acc, closed);
        }
        "user_message" => {
            let msg = s(p, "message").unwrap_or("");
            let imgs = p.get("local_images").and_then(Value::as_array).map(Vec::len).unwrap_or(0)
                + p.get("images").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
            on_user(st, line_start, msg, imgs, None, &at, closed);
        }
        "agent_message" => {
            let msg = s(p, "message").unwrap_or("");
            on_agent(st, msg, &at, closed);
        }
        "task_complete" => {
            if let Some(acc) = st.open.as_mut() {
                acc.stopped_at = at.clone().or_else(|| Some(time::now_iso()));
                acc.active_ms = acc.active_ms.saturating_add(n(p, "duration_ms").min(7 * 86_400_000));
                if let Some(m) = s(p, "last_agent_message").map(str::trim).filter(|m| !m.is_empty() && *m != IMPORT_MARKER) {
                    if acc.response.is_none() {
                        acc.response = Some(text::clip(&text::redact(m), 120_000));
                    }
                }
                if let Some(err) = p.get("error").filter(|e| !e.is_null()) {
                    acc.errors += 1;
                    let msg = s(err, "message").unwrap_or("Codex 오류");
                    acc.step(&at, "error", None, text::safe(msg, 400));
                }
                acc.dirty = true;
            }
            super::close_sides(st, closed);
        }
        "turn_aborted" => {
            if let Some(acc) = st.open.as_mut() {
                acc.interrupted_at = at.clone().or_else(|| Some(time::now_iso()));
                let label = match s(p, "reason") {
                    None | Some("interrupted") => "사용자가 중단함".to_string(),
                    Some(r) => format!("중단됨 ({})", text::clip(r, 60)),
                };
                acc.step(&at, "interrupt", None, label);
                acc.dirty = true;
            }
            super::close_sides(st, closed);
        }
        "token_count" => on_tokens(st, line_start, p),
        "context_compacted" => {
            if let Some(acc) = st.open.as_mut() {
                acc.step(&at, "compact", None, "대화가 길어 앞부분을 압축함".into());
            }
        }
        "patch_apply_end" => {
            // 옛 형식: 바뀐 파일(도구 수는 response_item 의 apply_patch 가 센다)
            if let (Some(acc), Some(ch)) = (st.open.as_mut(), p.get("changes").and_then(Value::as_object)) {
                for path in ch.keys() {
                    *acc.files.entry(path.clone()).or_default() += 1;
                }
                acc.touch(&at);
            }
        }
        "item_completed" => {
            let Some(item) = p.get("item") else { return };
            {
                let cx = st.cx.as_mut().expect("cx");
                cx.items_seen = true;
                cx.turn_items = true;
                if s(item, "type") != Some("UserMessage") {
                    cx.turn_activity = true;
                }
            }
            on_item(st, line_start, item, &at, closed);
        }
        _ => {}
    }
}

fn stamp(patch: &mut SessionPatch, at: &Option<String>) {
    if let Some(a) = at {
        if patch.first_at.as_deref().is_none_or(|f| a.as_str() < f) {
            patch.first_at = Some(a.clone());
        }
        if patch.last_at.as_deref().is_none_or(|l| a.as_str() > l) {
            patch.last_at = Some(a.clone());
        }
    }
}

fn new_acc(st: &FileState, uuid: String, at: String) -> TurnAcc {
    let cx = st.cx.as_ref().expect("cx");
    let mut acc = TurnAcc::new(uuid, at, &Line::default(), String::new(), None, "human", None);
    acc.codex = true;
    acc.cwd = cx.cwd.clone();
    acc.branch = cx.branch.clone();
    if let Some(m) = &cx.model {
        acc.models.entry(m.clone()).or_default();
    }
    if let Some(e) = &cx.effort {
        acc.efforts.entry(e.clone()).or_default();
    }
    acc
}

/// 새 턴 — 앞 턴을 닫는다(Claude 쪽 start_turn 과 달리 합치지 않는다: 턴 경계가 기록에 분명히 있다)
fn begin(st: &mut FileState, line_start: u64, acc: TurnAcc, closed: &mut Vec<TurnAcc>) {
    if let Some(mut prev) = st.open.take() {
        prev.closed = true;
        prev.dirty = true;
        closed.push(prev);
    }
    super::close_sides(st, closed);
    st.open = Some(acc);
    st.offset = line_start;
}

fn user_text(content: &Value) -> (String, usize) {
    let mut parts = Vec::new();
    let mut imgs = 0;
    if let Some(arr) = content.as_array() {
        for c in arr {
            match s(c, "type") {
                Some("text") | Some("Text") | Some("input_text") => {
                    if let Some(t) = s(c, "text").filter(|t| !t.is_empty()) {
                        parts.push(t.to_string());
                    }
                }
                Some(_) => imgs += 1,
                None => {}
            }
        }
    }
    (parts.join("\n"), imgs)
}

fn on_user(st: &mut FileState, line_start: u64, raw: &str, imgs: usize, id: Option<&str>, at: &Option<String>, closed: &mut Vec<TurnAcc>) {
    let c = text::clean_prompt(raw);
    let mut body = c.text.clone();
    if imgs > 0 && !body.contains("[첨부 이미지") {
        let tag = format!("[이미지 {imgs}장]");
        body = if body.is_empty() { tag } else { format!("{body}\n\n{tag}") };
    }
    if body.is_empty() && c.slash.is_none() {
        return;
    }
    let (body, origin) = super::inbox_or(&body, "human");
    let body = text::redact(&body);
    let when = at.clone().unwrap_or_else(time::now_iso);
    let need_new = match st.open.as_ref() {
        None => true,
        // 끝난 턴 뒤에 턴 시작 줄 없이 온 말(옛 형식·기록 일부) — 새 요청으로
        Some(a) => (a.stopped_at.is_some() || a.interrupted_at.is_some()) && st.cx.as_ref().is_some_and(|c| c.turn_prompt),
    };
    if need_new {
        let uuid = id.map(str::to_string).unwrap_or_else(|| format!("pos-{line_start}"));
        let acc = new_acc(st, uuid, when.clone());
        begin(st, line_start, acc, closed);
        let cx = st.cx.as_mut().expect("cx");
        cx.turn_prompt = false;
        cx.turn_output = false;
    }
    let (has_prompt, has_output) = {
        let cx = st.cx.as_ref().expect("cx");
        (cx.turn_prompt, cx.turn_output)
    };
    let Some(acc) = st.open.as_mut() else { return };
    if !has_prompt {
        acc.prompt_text = body;
        acc.slash = c.slash;
        acc.origin = origin.to_string();
        acc.prompt_at = when;
        acc.dirty = true;
    } else if !has_output {
        // 모델이 한 마디도 하기 전에 이어진 말(대기열에 쌓였던 말 등) — 같은 요청으로 합친다
        let add = body.trim();
        if !add.is_empty() && !acc.prompt_text.contains(add) {
            acc.prompt_text = if acc.prompt_text.is_empty() { add.to_string() } else { format!("{}\n\n{add}", acc.prompt_text.trim_end()) };
        }
        acc.dirty = true;
    } else {
        // 모델이 일하는 도중에 보낸 말 — 따로 한 쌍(답은 그 뒤 첫 에이전트 말)
        acc.step(&Some(when.clone()), "ask", None, text::clip(&body, 2000));
        acc.touch(at);
        let uuid = id.map(str::to_string).unwrap_or_else(|| format!("pos-{line_start}"));
        let mut side = TurnAcc::new(uuid, when, &Line::default(), body, c.slash, origin, None);
        side.codex = true;
        side.source = Some("mid-turn".into());
        side.side = true;
        side.cwd = acc.cwd.clone();
        side.branch = acc.branch.clone();
        st.side.push(side);
    }
    st.cx.as_mut().expect("cx").turn_prompt = true;
}

fn on_agent(st: &mut FileState, raw: &str, at: &Option<String>, closed: &mut Vec<TurnAcc>) {
    let t = raw.trim();
    if t.is_empty() {
        return;
    }
    if t == IMPORT_MARKER {
        // 여기까지는 Claude Code 대화의 사본 — 버린다(이미 Claude 쪽에서 모았다)
        st.open = None;
        st.side.clear();
        closed.clear();
        let cx = st.cx.as_mut().expect("cx");
        cx.turn_prompt = false;
        cx.turn_output = false;
        return;
    }
    let t = text::redact(t);
    if let Some(acc) = st.open.as_mut() {
        if acc.first_reply_at.is_none() {
            acc.first_reply_at = at.clone();
        }
        acc.touch(at);
        if !acc.saw_tool && acc.understanding.is_none() {
            acc.understanding = Some(text::clip(&t, 4000));
        }
        acc.response = Some(text::clip(&t, 120_000));
        acc.step(at, "text", None, text::clip(&t, 12_000));
    }
    st.cx.as_mut().expect("cx").turn_output = true;
    // 작업 중에 보낸 말의 답 — 그 말 뒤 첫 에이전트 말
    if !st.side.is_empty() {
        let mut side = st.side.remove(0);
        if side.first_reply_at.is_none() {
            side.first_reply_at = at.clone();
        }
        side.stopped_at = at.clone();
        side.touch(at);
        side.response = Some(text::clip(&t, 120_000));
        side.step(at, "text", None, text::clip(&t, 12_000));
        side.closed = true;
        side.dirty = true;
        closed.push(side);
    }
}

fn on_tokens(st: &mut FileState, line_start: u64, p: &Value) {
    let Some(info) = p.get("info").filter(|i| i.is_object()) else { return };
    let total = info.get("total_token_usage").map(|t| n(t, "total_tokens")).unwrap_or(0);
    let cx = st.cx.as_mut().expect("cx");
    if total > 0 && total == cx.last_total {
        return; // 같은 호출을 다시 적은 줄(사용량 한도 갱신 등)
    }
    let carried = !cx.turn_activity;
    cx.last_total = total;
    if carried {
        // 모델이 이 턴에서 아무것도 하기 전의 token_count = 앞 턴 누계를 다시 적은 것(앱을 다시 켜 턴 중간부터 읽을 때
        // 앞 누계를 몰라 한 번 더 세던 것 — 09-28 검토 재현)
        return;
    }
    let model = cx.model.clone();
    let effort = cx.effort.clone();
    let Some(last) = info.get("last_token_usage") else { return };
    let Some(acc) = st.open.as_mut() else { return };
    let input = n(last, "input_tokens");
    let cached = n(last, "cached_input_tokens").min(input);
    let u = Usage {
        input: input - cached,
        output: n(last, "output_tokens"),
        thinking: n(last, "reasoning_output_tokens"),
        cache_read: cached,
        ..Usage::default()
    };
    if u.input == 0 && u.output == 0 && u.cache_read == 0 {
        return;
    }
    let key = format!("tc{line_start}");
    if !acc.calls.contains_key(&key) {
        acc.call_order.push(key.clone());
    }
    acc.calls.insert(key, u);
    if let Some(m) = model {
        *acc.models.entry(m).or_default() += 1;
    }
    if let Some(e) = effort {
        *acc.efforts.entry(e).or_default() += 1;
    }
    acc.dirty = true;
}

fn tool(acc: &mut TurnAcc, at: &Option<String>, name: &str, summary: String) {
    acc.saw_tool = true;
    *acc.tools.entry(name.to_string()).or_default() += 1;
    acc.step(at, "tool", Some(name.to_string()), text::safe(&summary.replace('\n', " "), 240));
    acc.touch(at);
}

fn short_path(p: &str, cwd: Option<&str>) -> String {
    let p = p.strip_prefix("file://").unwrap_or(p);
    if let Some(c) = cwd {
        if let Some(rest) = p.strip_prefix(c) {
            let rest = rest.trim_start_matches(['/', '\\']);
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    p.to_string()
}

/// ["/bin/zsh", "-lc", "<명령>"] → 명령 첫 줄
fn command_line(cmd: &Value) -> String {
    match cmd {
        Value::String(x) => text::first_line(x).to_string(),
        Value::Array(a) => {
            let parts: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            let body = match parts.as_slice() {
                [_, flag, c, ..] if flag.starts_with('-') && flag.contains('c') => c.to_string(),
                _ => parts.join(" "),
            };
            text::first_line(&body).to_string()
        }
        _ => String::new(),
    }
}

fn on_item(st: &mut FileState, line_start: u64, item: &Value, at: &Option<String>, closed: &mut Vec<TurnAcc>) {
    let ty = s(item, "type").unwrap_or("");
    match ty {
        "UserMessage" => {
            let (t, imgs) = user_text(item.get("content").unwrap_or(&Value::Null));
            on_user(st, line_start, &t, imgs, s(item, "id"), at, closed);
            return;
        }
        "AgentMessage" => {
            let (t, _) = user_text(item.get("content").unwrap_or(&Value::Null));
            on_agent(st, &t, at, closed);
            return;
        }
        "Reasoning" | "CollabAgentToolCall" => return,
        _ => {}
    }
    let Some(acc) = st.open.as_mut() else { return };
    let cwd = acc.cwd.clone();
    match ty {
        "CommandExecution" => {
            let c = command_line(item.get("command").unwrap_or(&Value::Null));
            tool(acc, at, "Shell", c);
        }
        "FileChange" => {
            let paths: Vec<String> = item.get("changes").and_then(Value::as_object).map(|m| m.keys().cloned().collect()).unwrap_or_default();
            for path in &paths {
                *acc.files.entry(path.clone()).or_default() += 1;
            }
            let first = paths.first().map(|p| short_path(p, cwd.as_deref())).unwrap_or_default();
            let summary = if paths.len() > 1 { format!("{first} 외 {}개", paths.len() - 1) } else { first };
            tool(acc, at, "Edit", summary);
        }
        "McpToolCall" => {
            let name = format!("mcp__{}__{}", s(item, "server").unwrap_or("?"), s(item, "tool").unwrap_or("?"));
            let args = item.get("arguments").cloned().unwrap_or(Value::Null);
            let summary = s(&args, "title").map(str::to_string).unwrap_or_else(|| text::tool_summary(&name, &args, cwd.as_deref()));
            tool(acc, at, &name, summary);
        }
        "Extension" => match s(item, "kind").unwrap_or("") {
            "clock.sleep" | "" => {}
            "web.search" => {
                let q = item
                    .get("action")
                    .and_then(|a| s(a, "query").map(str::to_string).or_else(|| a.get("queries").and_then(Value::as_array).and_then(|q| q.first()).and_then(Value::as_str).map(str::to_string)).or_else(|| s(a, "url").map(str::to_string)))
                    .or_else(|| s(item, "query").map(str::to_string))
                    .unwrap_or_default();
                tool(acc, at, "WebSearch", q);
            }
            other => tool(acc, at, other, String::new()),
        },
        "ImageView" => {
            let p = short_path(s(item, "path").unwrap_or(""), cwd.as_deref());
            tool(acc, at, "ViewImage", p);
        }
        "SubAgentActivity" => {
            let path = s(item, "agent_path").unwrap_or("");
            let name = path.rsplit('/').next().filter(|x| !x.is_empty()).unwrap_or("agent").to_string();
            let key = s(item, "agent_thread_id").unwrap_or(path).to_string();
            if s(item, "kind") == Some("started") {
                acc.sub_by_tool.insert(key, acc.subs.len());
                acc.subs.push(Sub { agent_type: name.clone(), description: String::new(), background: false, started_at: at.clone(), ended_at: None });
                acc.step(at, "tool", Some("Agent".into()), format!("[{name}]"));
                *acc.tools.entry("Agent".into()).or_default() += 1;
                acc.saw_tool = true;
            } else if let Some(&i) = acc.sub_by_tool.get(&key) {
                acc.subs[i].ended_at = at.clone();
            }
            acc.touch(at);
        }
        "ContextCompaction" => acc.step(at, "compact", None, "대화가 길어 앞부분을 압축함".into()),
        _ => return,
    }
    st.cx.as_mut().expect("cx").turn_output = true;
}

/// 옛 형식의 도구 호출(새 형식 턴에서는 세지 않는다 — item 과 겹친다). 계획(update_plan)은 형식과 무관하게 받는다.
fn on_response_item(st: &mut FileState, p: &Value, at: &Option<String>) {
    let kind = s(p, "type").unwrap_or("");
    if !(kind == "message" && matches!(s(p, "role"), Some("user") | Some("developer") | Some("system"))) {
        if let Some(cx) = st.cx.as_mut() {
            cx.turn_activity = true;
        }
    }
    if !matches!(kind, "function_call" | "custom_tool_call" | "local_shell_call" | "web_search_call") {
        return;
    }
    let name = s(p, "name").unwrap_or(kind);
    let args: Value = match (s(p, "arguments"), s(p, "input")) {
        (Some(a), _) => serde_json::from_str(a).unwrap_or(Value::Null),
        (None, Some(i)) => Value::String(i.to_string()),
        _ => Value::Null,
    };
    let items = st.cx.as_ref().is_some_and(|c| c.turn_items);
    let Some(acc) = st.open.as_mut() else { return };
    if name == "update_plan" {
        if let Some(plan) = args.get("plan").and_then(Value::as_array) {
            acc.plan = plan
                .iter()
                .filter_map(|x| {
                    let t = s(x, "step")?;
                    Some(PlanItem { text: text::safe(t, 500), status: s(x, "status").unwrap_or("pending").to_string() })
                })
                .collect();
            acc.dirty = true;
        }
        return;
    }
    if items {
        return;
    }
    let cwd = acc.cwd.clone();
    match name {
        "wait" | "wait_agent" | "list_agents" | "sleep" | "load_workspace_dependencies" => {}
        "exec" | "exec_command" | "shell" | "local_shell_call" => {
            let c = match &args {
                Value::String(js) => js_cmd(js).unwrap_or_default(),
                v => v.get("cmd").or_else(|| v.get("command")).map(command_line).unwrap_or_default(),
            };
            tool(acc, at, "Shell", c);
        }
        "apply_patch" => {
            let body = match &args {
                Value::String(x) => x.clone(),
                v => s(v, "input").unwrap_or("").to_string(),
            };
            let files: Vec<String> = body
                .lines()
                .filter_map(|l| {
                    ["*** Add File: ", "*** Update File: ", "*** Delete File: "].iter().find_map(|pre| l.strip_prefix(pre)).map(|x| x.trim().to_string())
                })
                .collect();
            let first = files.first().map(|p| short_path(p, cwd.as_deref())).unwrap_or_default();
            tool(acc, at, "Edit", first);
        }
        "view_image" => {
            let path = short_path(s(&args, "path").unwrap_or(""), cwd.as_deref());
            tool(acc, at, "ViewImage", path);
        }
        "web_search_call" => tool(acc, at, "WebSearch", String::new()),
        "spawn_agent" => {
            let t = s(&args, "task_name").unwrap_or("agent").to_string();
            acc.subs.push(Sub { agent_type: t.clone(), description: String::new(), background: false, started_at: at.clone(), ended_at: None });
            tool(acc, at, "Agent", format!("[{t}]"));
        }
        other => {
            let full = match s(p, "namespace") {
                Some(ns) if ns.starts_with("mcp__") => format!("{ns}__{other}"),
                _ => other.to_string(),
            };
            let summary = text::tool_summary(&full, &args, cwd.as_deref());
            tool(acc, at, &full, summary);
        }
    }
    st.cx.as_mut().expect("cx").turn_output = true;
}

/// 코드 모드 도구(`exec`)의 JS 에서 첫 셸 명령: `cmd: "…"` · `"cmd":"…"`
fn js_cmd(js: &str) -> Option<String> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r#""?cmd"?\s*:\s*"((?:[^"\\]|\\.)*)""#).expect("regex"));
    let raw = re.captures(js)?.get(1)?.as_str();
    let unescaped: String = serde_json::from_str(&format!("\"{raw}\"")).unwrap_or_else(|_| raw.to_string());
    Some(text::first_line(&unescaped).to_string())
}

/// rollout-<시각>-<UUID>.jsonl → UUID
pub(crate) fn thread_id_of(path: &std::path::Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    if !stem.starts_with("rollout-") || stem.len() < 36 {
        return None;
    }
    let id = &stem[stem.len() - 36..];
    let ok = id.len() == 36
        && id.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_hexdigit() });
    ok.then(|| id.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(lines: &[&str]) -> (Vec<TurnAcc>, Option<TurnAcc>, SessionPatch, bool) {
        let mut st = super::super::FileState::for_test(true);
        let mut closed = Vec::new();
        let mut patch = SessionPatch::default();
        let mut pos = 0u64;
        for l in lines {
            feed(&mut st, pos, l.as_bytes(), &mut closed, &mut patch);
            pos += l.len() as u64 + 1;
        }
        let skip = st.cx.as_ref().is_some_and(|c| c.skip);
        closed.extend(st.side.drain(..));
        (closed, st.open.take(), patch, skip)
    }

    const META: &str = r#"{"timestamp":"2026-01-01T00:00:06.581Z","type":"session_meta","payload":{"id":"00000000-0000-4000-8000-000000000001","cwd":"/w/proj","originator":"codex-tui","cli_version":"0.156.0","source":"cli","thread_source":"user","git":{"branch":"main"}}}"#;

    #[test]
    fn new_format_turn() {
        let lines = [
            META,
            r#"{"timestamp":"2026-01-01T00:00:06.588Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t1"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:07.428Z","type":"turn_context","payload":{"turn_id":"t1","cwd":"/w/proj","model":"gpt-6-astra","effort":"medium"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:07.500Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>x</environment_context>"}]}}"#,
            r#"{"timestamp":"2026-01-01T00:00:07.670Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"u1","content":[{"type":"text","text":"로그인 버그 고쳐 줘"}]}}}"#,
            r#"{"timestamp":"2026-01-01T00:00:08.000Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"먼저 확인하겠습니다."}],"phase":"commentary"}}}"#,
            r#"{"timestamp":"2026-01-01T00:00:08.100Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"tools.exec_command({cmd:\"cat a.txt\"})"}}"#,
            r#"{"timestamp":"2026-01-01T00:00:08.200Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"CommandExecution","command":["/bin/zsh","-lc","cat a.txt"]}}}"#,
            r#"{"timestamp":"2026-01-01T00:00:09.000Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":1100},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":100,"reasoning_output_tokens":20,"total_tokens":1100}}}}"#,
            r#"{"timestamp":"2026-01-01T00:00:09.001Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":1100},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":100,"reasoning_output_tokens":20,"total_tokens":1100}}}}"#,
            r#"{"timestamp":"2026-01-01T00:00:10.000Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"FileChange","changes":{"/w/proj/src/login.rs":{"type":"update"},"/w/proj/src/auth.rs":{"type":"update"}}}}}"#,
            r#"{"timestamp":"2026-01-01T00:00:11.000Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"고쳤습니다. 배포할까요?"}],"phase":"final_answer"}}}"#,
            r#"{"timestamp":"2026-01-01T00:00:11.500Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"t1","duration_ms":4900,"last_agent_message":"고쳤습니다. 배포할까요?"}}"#,
        ];
        let (closed, open, patch, skip) = run(&lines);
        assert!(!skip);
        assert!(closed.is_empty());
        let a = open.expect("열린 요청");
        assert!(a.codex);
        assert_eq!(a.uuid, "t1");
        assert_eq!(a.prompt_text, "로그인 버그 고쳐 줘");
        assert_eq!(a.prompt_at, "2026-01-01T00:00:07.670Z");
        assert_eq!(a.understanding.as_deref(), Some("먼저 확인하겠습니다."));
        assert_eq!(a.response.as_deref(), Some("고쳤습니다. 배포할까요?"));
        assert_eq!(a.tools.get("Shell"), Some(&1), "response_item 과 item 을 두 번 세면 안 된다");
        assert_eq!(a.tools.get("Edit"), Some(&1));
        assert_eq!(a.files.len(), 2);
        assert_eq!(a.api_calls(), 1, "같은 누계의 token_count 는 한 번");
        let t = a.totals();
        assert_eq!((t.input, t.cache_read, t.output, t.thinking), (200, 800, 100, 20));
        assert_eq!(a.active_ms, 4900);
        assert!(a.stopped_at.is_some());
        assert_eq!(TurnAcc::top(&a.models).as_deref(), Some("gpt-6-astra"));
        assert_eq!(a.branch.as_deref(), Some("main"));
        assert_eq!(patch.cwd.as_deref(), Some("/w/proj"));
        assert_eq!(patch.version.as_deref(), Some("0.156.0"));
        assert_eq!(patch.model.as_deref(), Some("gpt-6-astra"));
    }

    #[test]
    fn old_format_turns_and_abort() {
        let lines = [
            r#"{"timestamp":"2026-08-06T05:00:00.000Z","type":"session_meta","payload":{"id":"x","cwd":"/w","cli_version":"0.146.0","source":"vscode"}}"#,
            r#"{"timestamp":"2026-08-06T05:00:01.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"a"}}"#,
            r#"{"timestamp":"2026-08-06T05:00:01.100Z","type":"event_msg","payload":{"type":"user_message","message":"첫 요청","local_images":[],"images":[]}}"#,
            r#"{"timestamp":"2026-08-06T05:00:02.000Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","input":"const r = await tools.exec_command({\"cmd\":\"ls -la\"})"}}"#,
            r#"{"timestamp":"2026-08-06T05:00:02.500Z","type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch\n*** Add File: /w/a.py\n+x\n*** End Patch"}}"#,
            r#"{"timestamp":"2026-08-06T05:00:02.600Z","type":"event_msg","payload":{"type":"patch_apply_end","changes":{"/w/a.py":{"type":"add"}}}}"#,
            r#"{"timestamp":"2026-08-06T05:00:03.000Z","type":"event_msg","payload":{"type":"agent_message","message":"끝났습니다.","phase":"final_answer"}}"#,
            r#"{"timestamp":"2026-08-06T05:00:03.100Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"a","duration_ms":2100}}"#,
            r#"{"timestamp":"2026-08-06T05:01:00.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"b"}}"#,
            r#"{"timestamp":"2026-08-06T05:01:00.100Z","type":"event_msg","payload":{"type":"user_message","message":"두 번째","local_images":["/tmp/x.png"]}}"#,
            r#"{"timestamp":"2026-08-06T05:01:05.000Z","type":"event_msg","payload":{"type":"turn_aborted","turn_id":"b","reason":"interrupted"}}"#,
        ];
        let (closed, open, _, _) = run(&lines);
        assert_eq!(closed.len(), 1);
        let a = &closed[0];
        assert_eq!(a.prompt_text, "첫 요청");
        assert!(a.closed);
        assert_eq!(a.tools.get("Shell"), Some(&1));
        assert_eq!(a.tools.get("Edit"), Some(&1));
        assert_eq!(a.files.get("/w/a.py"), Some(&1));
        assert_eq!(a.response.as_deref(), Some("끝났습니다."));
        assert!(a.steps.iter().any(|s| s.kind == "tool" && s.text == "ls -la"), "코드 모드 exec 의 명령");
        let b = open.expect("두 번째 요청");
        assert_eq!(b.prompt_text, "두 번째\n\n[이미지 1장]");
        assert!(b.interrupted_last());
    }

    #[test]
    fn queued_messages_merge_or_become_side_turns() {
        let um = |t: &str, id: &str| {
            format!(r#"{{"timestamp":"2026-09-28T00:00:0{id}.000Z","type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"UserMessage","id":"{id}","content":[{{"type":"text","text":"{t}"}}]}}}}}}"#)
        };
        let am = |t: &str, sec: u8| {
            format!(r#"{{"timestamp":"2026-09-28T00:00:{sec:02}.500Z","type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"AgentMessage","content":[{{"type":"Text","text":"{t}"}}]}}}}}}"#)
        };
        let start = r#"{"timestamp":"2026-09-28T00:00:00.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"q"}}"#;
        let (l1, l2, l3, l4) = (um("하나", "1"), um("둘", "2"), am("하나 끝", 3), um("셋", "4"));
        let l5 = am("셋 끝", 5);
        let (closed, open, _, _) = run(&[start, &l1, &l2, &l3, &l4, &l5]);
        let main = open.expect("요청");
        assert_eq!(main.prompt_text, "하나\n\n둘", "모델이 말하기 전에 온 말은 합친다");
        assert_eq!(closed.len(), 1);
        let side = &closed[0];
        assert!(side.side);
        assert_eq!(side.prompt_text, "셋");
        assert_eq!(side.response.as_deref(), Some("셋 끝"));
    }

    #[test]
    fn inbox_header_is_stripped() {
        let body = serde_json::to_string(&crate::conoti::wrap_desk("테스트 돌려 줘")).unwrap();
        let l = format!(r#"{{"timestamp":"2026-09-28T00:00:01.000Z","type":"event_msg","payload":{{"type":"item_completed","item":{{"type":"UserMessage","id":"u","content":[{{"type":"text","text":{body}}}]}}}}}}"#);
        let start = r#"{"timestamp":"2026-09-28T00:00:00.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"q"}}"#;
        let (_, open, _, _) = run(&[start, &l]);
        let a = open.unwrap();
        assert_eq!(a.origin, "inbox");
        assert_eq!(a.prompt_text, "테스트 돌려 줘");
    }

    #[test]
    fn subagent_threads_are_skipped() {
        let lines = [
            r#"{"timestamp":"2026-09-03T11:02:04.000Z","type":"session_meta","payload":{"id":"s","source":{"subagent":{"thread_spawn":{"parent_thread_id":"p"}}},"thread_source":"subagent"}}"#,
            r#"{"timestamp":"2026-09-03T11:02:05.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"x"}}"#,
            r#"{"timestamp":"2026-09-03T11:02:05.100Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"하위 작업"}]}}}"#,
        ];
        let (closed, open, _, skip) = run(&lines);
        assert!(skip);
        assert!(closed.is_empty() && open.is_none());
    }

    #[test]
    fn imported_history_is_dropped() {
        let lines = [
            META,
            r#"{"timestamp":"2026-07-31T05:44:38.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"old1"}}"#,
            r#"{"timestamp":"2026-07-31T05:44:38.100Z","type":"event_msg","payload":{"type":"user_message","message":"Claude 쪽 옛 대화"}}"#,
            r#"{"timestamp":"2026-07-31T05:44:38.200Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"old1"}}"#,
            r#"{"timestamp":"2026-07-31T05:44:38.300Z","type":"event_msg","payload":{"type":"task_started","turn_id":"old2"}}"#,
            r#"{"timestamp":"2026-07-31T05:44:38.400Z","type":"event_msg","payload":{"type":"user_message","message":"옛 대화 2"}}"#,
            r#"{"timestamp":"2026-07-31T05:44:38.500Z","type":"event_msg","payload":{"type":"agent_message","message":"<EXTERNAL SESSION IMPORTED>"}}"#,
            r#"{"timestamp":"2026-07-31T05:44:38.600Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"old2"}}"#,
            r#"{"timestamp":"2026-08-01T01:00:00.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"new1"}}"#,
            r#"{"timestamp":"2026-08-01T01:00:00.100Z","type":"event_msg","payload":{"type":"user_message","message":"Codex 에서 이어 간 말"}}"#,
        ];
        let (closed, open, _, _) = run(&lines);
        assert!(closed.is_empty(), "가져온 대화는 버린다: {:?}", closed.iter().map(|c| c.prompt_text.clone()).collect::<Vec<_>>());
        assert_eq!(open.unwrap().prompt_text, "Codex 에서 이어 간 말");
    }

    #[test]
    fn error_turn_counts_error() {
        let lines = [
            r#"{"timestamp":"2026-09-27T13:00:00.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"e"}}"#,
            r#"{"timestamp":"2026-09-27T13:00:00.100Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"하던 거 계속"}]}}}"#,
            r#"{"timestamp":"2026-09-27T13:00:09.000Z","type":"event_msg","payload":{"type":"task_complete","turn_id":"e","error":{"message":"You've hit your usage limit."}}}"#,
        ];
        let (_, open, _, _) = run(&lines);
        let a = open.unwrap();
        assert_eq!(a.errors, 1);
        assert!(a.steps.iter().any(|s| s.kind == "error" && s.text.contains("usage limit")));
        assert!(a.response.is_none());
    }

    #[test]
    fn carried_token_count_at_turn_start_is_not_counted() {
        // 앱을 다시 켜 턴 시작부터 읽을 때: 턴 첫 token_count 가 앞 턴 누계의 반복이면 세지 않는다
        let lines = [
            r#"{"timestamp":"2026-09-28T00:00:00.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t2"}}"#,
            r#"{"timestamp":"2026-09-28T00:00:00.100Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":1100},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":0,"output_tokens":100,"total_tokens":1100}}}}"#,
            r#"{"timestamp":"2026-09-28T00:00:00.200Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"다음"}]}}}"#,
            r#"{"timestamp":"2026-09-28T00:00:01.000Z","type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","content":[{"type":"Text","text":"네"}]}}}"#,
            r#"{"timestamp":"2026-09-28T00:00:01.100Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"total_tokens":1700},"last_token_usage":{"input_tokens":500,"cached_input_tokens":200,"output_tokens":100,"total_tokens":600}}}}"#,
        ];
        let (_, open, _, _) = run(&lines);
        let a = open.unwrap();
        assert_eq!(a.api_calls(), 1);
        let t = a.totals();
        assert_eq!((t.input, t.cache_read, t.output), (300, 200, 100));
    }

    #[test]
    fn forked_thread_drops_copied_parent_lines() {
        let lines = [
            r#"{"timestamp":"2026-09-28T00:00:00.000Z","ordinal":0,"type":"session_meta","payload":{"id":"f","cwd":"/w","forked_from_id":"p","history_start_ordinal":3}}"#,
            r#"{"timestamp":"2026-09-28T00:00:00.100Z","ordinal":1,"type":"event_msg","payload":{"type":"task_started","turn_id":"parent-turn"}}"#,
            r#"{"timestamp":"2026-09-28T00:00:00.200Z","ordinal":2,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"부모의 말"}]}}}"#,
            r#"{"timestamp":"2026-09-28T00:00:05.000Z","ordinal":3,"type":"event_msg","payload":{"type":"task_started","turn_id":"own"}}"#,
            r#"{"timestamp":"2026-09-28T00:00:05.100Z","ordinal":4,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","content":[{"type":"text","text":"포크 뒤 새 말"}]}}}"#,
        ];
        let (closed, open, _, _) = run(&lines);
        assert!(closed.is_empty(), "부모 사본은 요청이 되지 않는다");
        assert_eq!(open.unwrap().prompt_text, "포크 뒤 새 말");
    }

    #[test]
    fn cwd_comes_from_turn_context_when_meta_was_read_before() {
        let lines = [
            r#"{"timestamp":"2026-09-28T00:00:00.000Z","type":"event_msg","payload":{"type":"task_started","turn_id":"t"}}"#,
            r#"{"timestamp":"2026-09-28T00:00:00.100Z","type":"turn_context","payload":{"cwd":"/w/proj","model":"gpt-6-astra"}}"#,
        ];
        let (_, _, patch, _) = run(&lines);
        assert_eq!(patch.cwd.as_deref(), Some("/w/proj"));
    }

    #[test]
    fn thread_ids_from_file_names() {
        let p = std::path::Path::new("/x/2026/09/28/rollout-2026-09-28T08-36-15-01a0e539-f547-7823-8107-4786f140daa0.jsonl");
        assert_eq!(thread_id_of(p).as_deref(), Some("01a0e539-f547-7823-8107-4786f140daa0"));
        assert_eq!(thread_id_of(std::path::Path::new("/x/other.jsonl")), None);
        assert_eq!(thread_id_of(std::path::Path::new("/x/rollout-2026-09-28T08-36-15-zzzzzzzz-f547-7823-8107-4786f140daa0.jsonl")), None);
    }
}
