//! 요청 하나를 마크다운 문서로 — 화면의 `src/markdown.ts` 와 같은 문서를 폰에도 보내기 위한 Rust 판.
//! 입력은 `api::TurnDetail` 을 JSON 으로 바꾼 값(필드 이름이 화면 쪽 타입과 같다).
//! 🚨 화면 쪽 문서 구성을 바꾸면 여기도 같이 바꾼다(테스트가 절 제목 순서를 본다).
//! 구성 = 턴 카드 6칸: 질문 · 이해 · 결과 · 응답 필요 · 과정 · 비용 (+ 이어진 요청). 규칙은 `src/turncard.ts` 와 같다.

use std::sync::OnceLock;

use chrono::{DateTime, Local, Timelike};
use serde_json::Value;

use crate::conoti::REPLY_HEADER;

fn s<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn n(v: &Value, k: &str) -> i64 {
    v.get(k).and_then(Value::as_i64).unwrap_or(0)
}

fn on(v: &Value, k: &str) -> Option<i64> {
    v.get(k).and_then(Value::as_i64)
}

#[cfg(test)]
thread_local! {
    /// 호환 시험용 — 이 스레드에서만 화면 시각의 UTC 오프셋(초)을 고정한다(러너 시간대와 무관하게 같은 문서가 나오도록)
    pub static TEST_UTC_OFFSET: std::cell::Cell<Option<i32>> = const { std::cell::Cell::new(None) };
}

#[cfg(test)]
fn local(iso: &str) -> Option<DateTime<chrono::FixedOffset>> {
    let d = DateTime::parse_from_rfc3339(iso).ok()?;
    match TEST_UTC_OFFSET.with(|o| o.get()) {
        Some(secs) => Some(d.with_timezone(&chrono::FixedOffset::east_opt(secs)?)),
        None => Some(d.with_timezone(&Local).fixed_offset()),
    }
}

#[cfg(not(test))]
fn local(iso: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(iso).ok().map(|d| d.with_timezone(&Local))
}

pub fn clock(iso: Option<&str>) -> String {
    iso.and_then(local).map(|t| format!("{:02}:{:02}", t.hour(), t.minute())).unwrap_or_default()
}

pub fn full_time(iso: Option<&str>) -> String {
    iso.and_then(local).map(|t| t.format("%Y-%m-%d %H:%M:%S").to_string()).unwrap_or_else(|| "—".into())
}

/// 3분 12초 · 1시간 4분 · 42초
pub fn duration(ms: Option<i64>) -> String {
    let Some(ms) = ms else { return "—".into() };
    let s = (ms as f64 / 1000.0).round() as i64;
    if s < 60 {
        return format!("{s}초");
    }
    let m = s / 60;
    if m < 60 {
        return if s % 60 != 0 { format!("{m}분 {}초", s % 60) } else { format!("{m}분") };
    }
    let h = m / 60;
    if m % 60 != 0 { format!("{h}시간 {}분", m % 60) } else { format!("{h}시간") }
}

pub fn tokens(x: i64) -> String {
    if x == 0 {
        return "0".into();
    }
    if x < 1000 {
        return x.to_string();
    }
    if x < 1_000_000 {
        return format!("{:.*}k", if x < 10_000 { 1 } else { 0 }, x as f64 / 1000.0);
    }
    format!("{:.*}M", if x < 10_000_000 { 2 } else { 1 }, x as f64 / 1_000_000.0)
}

/// claude-opus-5-5[1m] → Opus 5.5 · 1M · gpt-6-astra → GPT-6 Astra (src/format.ts 의 modelName 과 같은 규칙)
pub fn model_name(m: Option<&str>) -> String {
    let Some(m) = m.filter(|m| !m.is_empty()) else { return "—".into() };
    if let Some(rest) = m.strip_prefix("gpt-").or_else(|| m.strip_prefix("GPT-")) {
        let mut parts = rest.split('-').filter(|p| !p.is_empty());
        let ver = parts.next().unwrap_or("");
        let words: Vec<String> =
            parts.map(|w| w.chars().take(1).flat_map(char::to_uppercase).chain(w.chars().skip(1)).collect()).collect();
        return format!("GPT-{ver}{}", if words.is_empty() { String::new() } else { format!(" {}", words.join(" ")) });
    }
    let one_m = m.to_lowercase().contains("[1m]");
    let base = m.replace("[1m]", "").replace("[1M]", "");
    let base = base.strip_prefix("claude-").unwrap_or(&base).to_string();
    let base = match base.rsplit_once('-') {
        Some((head, tail)) if tail.len() == 8 && tail.chars().all(|c| c.is_ascii_digit()) => head.to_string(),
        _ => base,
    };
    let mut parts = base.split('-');
    let fam = parts.next().unwrap_or("");
    let family: String = fam.chars().take(1).flat_map(char::to_uppercase).chain(fam.chars().skip(1)).collect();
    let ver = parts.collect::<Vec<_>>().join(".");
    format!("{family}{}{}", if ver.is_empty() { String::new() } else { format!(" {ver}") }, if one_m { " · 1M" } else { "" })
}

pub fn status_label(status: &str, needs_input: bool, pending_bg: i64) -> String {
    match status {
        "running" => "작업 중".into(),
        "background" => {
            if pending_bg > 1 { format!("백그라운드 작업 {pending_bg}개 대기") } else { "백그라운드 작업 대기".into() }
        }
        "waiting" => "승인·답변 대기".into(),
        "interrupted" => "중단됨".into(),
        "stopped" => "멈춤".into(),
        _ => if needs_input { "답이 필요해요".into() } else { "완료".into() },
    }
}

/// mcp__computer-use__screenshot → computer-use:screenshot
pub fn tool_name(t: Option<&str>) -> String {
    let Some(t) = t else { return "도구".into() };
    if let Some(rest) = t.strip_prefix("mcp__") {
        if let Some((a, b)) = rest.split_once("__") {
            return format!("{a}:{b}");
        }
    }
    t.to_string()
}

pub fn tilde_path(p: &str) -> String {
    let re_mac = regex::Regex::new(r"^/Users/[^/]+").unwrap();
    let re_linux = regex::Regex::new(r"^/home/[^/]+").unwrap();
    let re_win = regex::Regex::new(r"^[A-Za-z]:\\Users\\[^\\]+").unwrap();
    let out = re_mac.replace(p, "~");
    let out = re_linux.replace(&out, "~");
    re_win.replace(&out, "~").into_owned()
}

fn short_path(p: &str, cwd: Option<&str>) -> String {
    if let Some(c) = cwd {
        if let Some(rest) = p.strip_prefix(c) {
            let rest = rest.trim_start_matches(['/', '\\']);
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    tilde_path(p)
}

fn esc(x: &str) -> String {
    x.replace('|', "\\|").replace('\n', " ")
}

fn first_line(x: Option<&str>, max: usize) -> String {
    let line = x.unwrap_or("").lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max).collect::<String>())
    } else {
        line.to_string()
    }
}

/// 폰에서 온 답이면 (원래 요청 제목, 본문)
pub fn phone_reply(prompt: Option<&str>) -> Option<(Option<String>, String)> {
    let rest = prompt?.strip_prefix(REPLY_HEADER)?;
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    if let Some(after) = rest.strip_prefix("요청: ") {
        if let Some((title, body)) = after.split_once("\n\n") {
            return Some((Some(title.to_string()), body.to_string()));
        }
        // 이미지만 보낸 답: 경로 목록을 떼면 본문 없이 제목 줄만 남는다
        if !after.contains('\n') {
            return Some((Some(after.trim().to_string()), String::new()));
        }
    }
    Some((None, rest.trim().to_string()))
}

/// 답이 필요한 물음(추정): 응답 끝 8줄 중 물음표·"~주세요"로 끝나는 줄(목록 기호는 뗀다)
pub fn questions_of(response: Option<&str>) -> Vec<String> {
    let Some(r) = response else { return vec![] };
    let lines: Vec<&str> = r.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(8)..];
    let q = re(&Q_END, r"[?？]\s*\**$");
    let ask = re(&ASK_END, r"(주세요|알려 주세요|말씀해 주세요)\.?$");
    let bullet = re(&BULLET, r"^[-*]\s+");
    tail.iter().filter(|l| q.is_match(l) || ask.is_match(l)).map(|l| bullet.replace(l, "").into_owned()).collect()
}

// ── 턴 카드 규칙 — src/turncard.ts 와 같다(공용 시험값 tests/fixtures/turn_card_vectors.json) ──

static Q_END: OnceLock<regex::Regex> = OnceLock::new();
static ASK_END: OnceLock<regex::Regex> = OnceLock::new();
static BULLET: OnceLock<regex::Regex> = OnceLock::new();
static STATE_END: OnceLock<regex::Regex> = OnceLock::new();
static ONSET_END: OnceLock<regex::Regex> = OnceLock::new();
static ONSET_EN: OnceLock<regex::Regex> = OnceLock::new();
static NEWLINES: OnceLock<regex::Regex> = OnceLock::new();
static SOFT_NL: OnceLock<regex::Regex> = OnceLock::new();

fn re(cell: &'static OnceLock<regex::Regex>, pat: &str) -> &'static regex::Regex {
    cell.get_or_init(|| regex::Regex::new(pat).expect("regex"))
}

fn flat(x: &str) -> String {
    re(&NEWLINES, r"\n+").replace_all(x, " ").into_owned()
}

/// 문장 나누기 — 줄바꿈과 마침표·물음표·느낌표 뒤 공백
pub fn sentences(x: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev: Option<char> = None;
    for c in x.chars() {
        if c == '\n' || (c.is_whitespace() && prev.is_some_and(|p| ".!?。？！".contains(p))) {
            if !cur.trim().is_empty() {
                out.push(cur.trim().to_string());
            }
            cur.clear();
            prev = None;
            continue;
        }
        cur.push(c);
        prev = Some(c);
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// 화면·문서에 보일 "이해 요약". 응답 되풀이·1~2문장 착수 멘트(하겠습니다·확인합니다·Let me …)는 None.
pub fn understanding_of(understanding: Option<&str>, response: Option<&str>) -> Option<String> {
    let u = understanding.unwrap_or("").trim();
    if u.is_empty() {
        return None;
    }
    let r = response.unwrap_or("").trim();
    if !r.is_empty() && (u == r || r.starts_with(u)) {
        return None;
    }
    let ss = sentences(u);
    if ss.len() <= 2 {
        let last = ss.last().map(String::as_str).unwrap_or("").trim_end_matches(|c: char| c.is_whitespace() || ".!…:~*_`)".contains(c));
        let state = re(
            &STATE_END,
            r"(입니다|있습니다|없습니다|됩니다|같습니다|보입니다|맞습니다|많습니다|적습니다|다릅니다|[했었았였됐]습니다|필요합니다|가능합니다|중요합니다|야 합니다)$",
        );
        let onset = re(&ONSET_END, r"(겠습니다|겠어요|[가-힣]게요|[가-힣]니다)$");
        let en = re(&ONSET_EN, r"(?i)^(i'll|i will|let me|let's|i'm going to|first,? i'll|now i'll)\b");
        if en.is_match(last) || (onset.is_match(last) && !state.is_match(last)) {
            return None;
        }
    }
    Some(u.to_string())
}

/// 응답 필요 (확정?, 이름, 물음들). waiting = 확정(마지막 도구가 AskUserQuestion 이면 질문, 다른 도구면 권한 승인),
/// 끝났는데 needs_input = 추정. 작업 중에 받은 말(ask 단계)은 응답 필요가 아니다.
pub fn attention_of(t: &Value, steps: &[Value]) -> Option<(bool, &'static str, Vec<String>)> {
    let status = s(t, "status").unwrap_or("done");
    if status == "waiting" {
        let tool = steps.iter().rev().find(|x| s(x, "kind") == Some("tool"));
        return Some(match tool {
            Some(x) if s(x, "name") == Some("AskUserQuestion") => {
                (true, "질문에 답 필요", s(x, "text").filter(|q| !q.is_empty()).map(|q| vec![q.to_string()]).unwrap_or_default())
            }
            Some(_) => (true, "권한 승인 대기", vec![]),
            None => (true, "승인·답변 대기", vec![]),
        });
    }
    if status == "done" && t.get("needs_input").and_then(Value::as_bool).unwrap_or(false) {
        return Some((false, "답이 필요해 보임", questions_of(s(t, "response_text"))));
    }
    None
}

/// 과정 요약 한 줄 — "도구 12회(Bash 5, Read 4, Edit 2) · 파일 3 · 서브에이전트 1 · 오류 1"
pub fn process_summary(tool_calls: i64, tools: &[Value], files: usize, subagents: usize, errors: i64) -> String {
    let mut parts = Vec::new();
    if tool_calls > 0 {
        let mut top: Vec<(String, i64)> = tools.iter().map(|x| (tool_name(x[0].as_str()), x[1].as_i64().unwrap_or(0))).collect();
        top.sort_by(|a, b| b.1.cmp(&a.1));
        let top = top.iter().take(3).map(|(n, c)| format!("{n} {c}")).collect::<Vec<_>>().join(", ");
        parts.push(format!("도구 {tool_calls}회{}", if top.is_empty() { String::new() } else { format!("({top})") }));
    }
    if files > 0 {
        parts.push(format!("파일 {files}"));
    }
    if subagents > 0 {
        parts.push(format!("서브에이전트 {subagents}"));
    }
    if errors > 0 {
        parts.push(format!("오류 {errors}"));
    }
    if parts.is_empty() { "도구 없이 답함".into() } else { parts.join(" · ") }
}

/// 서브에이전트 한 줄 — "general-purpose · 설명 · 3분 12초 · 백그라운드"
fn subagent_line(a: &Value) -> String {
    let took = match on(a, "duration_ms") {
        Some(ms) => duration(Some(ms)),
        None if s(a, "ended_at").is_some() => "—".into(),
        None => "진행 중".into(),
    };
    let desc = re(&SOFT_NL, r"\s*\n+\s*").replace_all(s(a, "description").unwrap_or(""), " ").trim().to_string();
    let bg = if a.get("background").and_then(Value::as_bool).unwrap_or(false) { "백그라운드" } else { "" };
    [s(a, "agent_type").unwrap_or("-").to_string(), if desc.is_empty() { "-".into() } else { desc }, took, bg.to_string()]
        .into_iter()
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// 비용 한 줄 조각 — 캐시는 입력과 합치지 않는다. 달러 없음.
pub fn cost_parts(t: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(m) = s(t, "model").filter(|m| !m.is_empty()) {
        let effort = s(t, "effort").filter(|e| !e.is_empty()).map(|e| format!(" · effort {e}")).unwrap_or_default();
        out.push(format!("{}{effort}", model_name(Some(m))));
    }
    let (inp, outp) = (n(t, "input_tokens"), n(t, "output_tokens"));
    if inp != 0 || outp != 0 {
        out.push(format!("입력 {} / 출력 {}", tokens(inp), tokens(outp)));
    }
    let (read, write) = (n(t, "cache_read"), n(t, "cache_create_5m") + n(t, "cache_create_1h"));
    if read != 0 || write != 0 {
        out.push(format!("캐시 읽기 {} · 쓰기 {}", tokens(read), tokens(write)));
    }
    if let Some(ms) = on(t, "active_ms").filter(|ms| *ms > 0) {
        out.push(format!("작업 {}", duration(Some(ms))));
    }
    if n(t, "tool_calls") > 0 {
        out.push(format!("도구 {}회", n(t, "tool_calls")));
    }
    out
}

/// 과정 칸 시간순 목록의 한 항목 — 같은 도구가 이어진 묶음 또는 한 단계
enum Item<'a> {
    Tools { at: Option<&'a str>, name: Option<&'a str>, count: usize, texts: Vec<&'a str> },
    One(&'a Value),
}

/// 과정 칸의 시간순 목록 — 같은 도구가 이어지면 한 줄로 묶고, 응답·이해 칸과 같은 글 단계는 뺀다
fn render_process(steps: &[Value], response: Option<&str>, understanding: Option<&str>) -> Vec<String> {
    let skip: Vec<&str> = [response.map(str::trim), understanding.map(str::trim)].into_iter().flatten().filter(|x| !x.is_empty()).collect();
    let mut items: Vec<Item> = Vec::new();
    for st in steps {
        if s(st, "kind") == Some("tool") {
            let name = s(st, "name");
            let text = s(st, "text").filter(|x| !x.is_empty());
            if let Some(Item::Tools { name: last, count, texts, .. }) = items.last_mut() {
                if *last == name {
                    *count += 1;
                    texts.extend(text);
                    continue;
                }
            }
            items.push(Item::Tools { at: s(st, "at"), name, count: 1, texts: text.into_iter().collect() });
            continue;
        }
        if s(st, "kind") == Some("text") && skip.contains(&s(st, "text").unwrap_or("").trim()) {
            continue;
        }
        items.push(Item::One(st));
    }
    let mut out = Vec::new();
    for it in items {
        match it {
            Item::Tools { at, name, count, texts } => {
                let (at, name) = (clock(at), tool_name(name));
                if count == 1 {
                    out.push(format!("- {at} `{name}` {}", flat(texts.first().copied().unwrap_or(""))).trim_end().to_string());
                    continue;
                }
                out.push(format!("- {at} `{name}` ×{count}"));
                for x in texts.iter().take(6) {
                    out.push(format!("  - {}", flat(x)));
                }
                if texts.len() > 6 {
                    out.push(format!("  - … 외 {}개", texts.len() - 6));
                }
            }
            Item::One(st) => {
                let t = s(st, "text").unwrap_or("").trim().to_string();
                let at = clock(s(st, "at"));
                match s(st, "kind").unwrap_or("") {
                    "text" => {
                        let body = if t.chars().count() > 600 { format!("{}…", t.chars().take(600).collect::<String>()) } else { t };
                        out.push(format!("- {at} {}", flat(&body)));
                    }
                    "task" => out.push(format!("- {at} 백그라운드 알림 — {t}")),
                    "ask" => {
                        let who = s(st, "name").map(|n| format!("{n} 이 보냄")).unwrap_or_else(|| "작업 중에 받은 말".into());
                        out.push(format!("- {at} **{who}** — {}", flat(&t)));
                    }
                    "summary" => out.push(format!("- {at} 요약 — {t}")),
                    "error" => out.push(format!("- {at} **오류** — {t}")),
                    "interrupt" => out.push(format!("- {at} **사용자가 중단함**")),
                    "compact" => out.push(format!("- {at} 대화 압축")),
                    "continue" => out.push(format!("- {at} {t}")),
                    _ => {}
                }
            }
        }
    }
    out
}

pub fn is_finished(status: &str) -> bool {
    matches!(status, "done" | "interrupted" | "stopped")
}

pub fn title_of(d: &Value) -> String {
    let t = &d["turn"];
    let prompt = s(t, "prompt_text");
    let phone = phone_reply(prompt);
    let base = phone.as_ref().map(|p| p.1.as_str()).or(prompt);
    let title = first_line(base, 70);
    if !title.is_empty() {
        return title;
    }
    s(t, "slash_command").map(str::to_string).unwrap_or_else(|| format!("요청 #{}", n(t, "seq")))
}

pub fn build(d: &Value) -> String {
    let t = &d["turn"];
    let se = &d["session"];
    let status = s(t, "status").unwrap_or("done");
    let needs_input = t.get("needs_input").and_then(Value::as_bool).unwrap_or(false);
    let label = status_label(status, needs_input, n(t, "pending_bg"));
    let mut l: Vec<String> = Vec::new();

    l.push(format!("# {}", title_of(d)));
    l.push(String::new());
    let branch = s(se, "git_branch").filter(|b| *b != "HEAD");
    let where_ = [s(se, "name"), s(se, "project_name"), branch].into_iter().flatten().filter(|x| !x.is_empty()).collect::<Vec<_>>().join(" · ");
    l.push(format!("> {where_}  "));
    l.push(format!("> {} · **{label}** · {}", full_time(s(t, "prompt_at")), duration(on(t, "duration_ms"))));
    l.push(String::new());

    // 1. 질문
    l.push("## 질문".into());
    l.push(String::new());
    if s(t, "origin") == Some("peer") {
        l.push(format!("*다른 세션 `{}` 이 보낸 요청*\n", s(t, "peer_name").unwrap_or("?")));
    }
    if s(t, "origin") == Some("inbox") {
        l.push("*AI Inbox 앱에서 보낸 말*\n".into());
    }
    if s(t, "origin") == Some("sched") {
        l.push("*AI Inbox 예약 전송으로 보낸 말*\n".into());
    }
    let phone = phone_reply(s(t, "prompt_text"));
    if let Some((title, _)) = &phone {
        l.push(format!("*폰(코노티)에서 보낸 답{}*\n", title.as_ref().map(|x| format!(" — 원래 요청: {x}")).unwrap_or_default()));
    }
    if s(t, "prompt_source") == Some("mid-turn") {
        l.push("*앞 요청이 진행되는 중에 보낸 말*\n".into());
    }
    if let Some(sc) = s(t, "slash_command") {
        l.push(format!("`{sc}`\n"));
    }
    let body = phone.as_ref().map(|p| p.1.clone()).or_else(|| s(t, "prompt_text").map(str::to_string)).unwrap_or_default();
    // 답장이면 인용으로(`src/markdown.ts` 와 같게)
    let (quote, rest) = crate::text::split_quote(&body);
    if let Some(q) = &quote {
        l.push(format!("> **#{} {}에 답장** — {}\n", q.seq, if q.part == "response" { "결과" } else { "요청" }, q.text));
    }
    let body = rest.to_string();
    let atts = t.get("atts").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
    l.push(if body.trim().is_empty() {
        if atts > 0 { "_(이미지만 보냄)_".into() } else { "_(본문 없음)_".into() }
    } else {
        body.trim().to_string()
    });
    l.push(String::new());
    if atts > 0 {
        l.push(format!("*첨부 이미지 {atts}장*\n"));
    }

    // 2. 이해 — 착수 멘트·응답 되풀이는 뺀다
    let understanding = understanding_of(s(t, "understanding"), s(t, "response_text"));
    if let Some(u) = &understanding {
        l.push("## 이해".into());
        l.push(String::new());
        l.push(u.clone());
        l.push(String::new());
    }

    // 3. 결과
    l.push("## 결과".into());
    l.push(String::new());
    if let Some(sum) = s(t, "summary").filter(|x| !x.is_empty()) {
        l.push(format!("**{}**", sum.trim()));
        l.push(String::new());
    }
    let resp = s(t, "response_text").map(str::trim).filter(|x| !x.is_empty());
    l.push(match resp {
        Some(r) => r.to_string(),
        None if status == "running" => "_(아직 작업 중)_".into(),
        None if s(t, "prompt_source") == Some("mid-turn") => "_(따로 답한 글이 없습니다 — 앞 요청의 작업 과정·응답에 이어집니다)_".into(),
        None => "_(응답 없음)_".into(),
    });
    l.push(String::new());

    // 4. 응답 필요 — 있을 때만. 확정과 추정을 가른다
    let empty = vec![];
    let steps = d["steps"].as_array().unwrap_or(&empty);
    if let Some((sure, label, items)) = attention_of(t, steps) {
        l.push("## 응답 필요".into());
        l.push(String::new());
        l.push(if sure { format!("**{label}**") } else { format!("**{label}** _(추정 — 응답 끝의 물음으로 짐작)_") });
        l.push(String::new());
        for q in &items {
            l.push(format!("- {}", flat(q)));
        }
        if !items.is_empty() {
            l.push(String::new());
        }
    }

    // 5. 과정
    let tools = d["tools"].as_array().unwrap_or(&empty);
    let files = d["files"].as_array().unwrap_or(&empty);
    let subs = d["subagents"].as_array().unwrap_or(&empty);
    l.push("## 과정".into());
    l.push(String::new());
    l.push(format!(
        "{}{}",
        process_summary(n(t, "tool_calls"), tools, files.len(), subs.len(), n(t, "error_count")),
        if status == "interrupted" { " · 중단됨" } else { "" }
    ));
    l.push(String::new());

    let plan: Vec<Value> = s(t, "plan_json").and_then(|j| serde_json::from_str(j).ok()).unwrap_or_default();
    if !plan.is_empty() {
        l.push("### 계획".into());
        l.push(String::new());
        for p in &plan {
            let done = s(p, "status") == Some("completed");
            l.push(format!("- [{}] {}", if done { "x" } else { " " }, s(p, "text").unwrap_or("")));
        }
        l.push(String::new());
    }

    if !files.is_empty() {
        l.push("### 바뀐 파일".into());
        l.push(String::new());
        for f in files {
            let edits = f[1].as_i64().unwrap_or(1);
            l.push(format!(
                "- `{}`{}",
                short_path(f[0].as_str().unwrap_or(""), s(t, "cwd")),
                if edits > 1 { format!(" ({edits}번 수정)") } else { String::new() }
            ));
        }
        l.push(String::new());
    }

    if !subs.is_empty() {
        l.push("### 서브에이전트".into());
        l.push(String::new());
        for a in subs {
            l.push(format!("- {}", subagent_line(a)));
        }
        l.push(String::new());
    }

    let rendered = render_process(steps, s(t, "response_text"), understanding.as_deref());
    if !rendered.is_empty() {
        l.push("### 시간순".into());
        l.push(String::new());
        l.extend(rendered);
        l.push(String::new());
    }

    // 6. 비용 — 달러는 넣지 않는다
    l.push("## 비용".into());
    l.push(String::new());
    let cost = cost_parts(t);
    l.push(if cost.is_empty() { "—".into() } else { cost.join(" · ") });
    l.push(String::new());
    l.push("| 항목 | 값 |".into());
    l.push("|---|---|".into());
    let mut row = |k: &str, v: String| l.push(format!("| {k} | {} |", esc(&v)));
    row("끝난 시각", full_time(s(t, "ended_at")));
    row("작업 폴더", s(t, "cwd").map(tilde_path).unwrap_or_else(|| "—".into()));
    if let Some(b) = s(t, "git_branch").filter(|b| *b != "HEAD") {
        row("브랜치", b.to_string());
    }
    row("세션", format!("{} ({})", s(se, "name").unwrap_or(""), s(se, "id").unwrap_or("")));
    l.push(String::new());

    l.push("## 이어진 요청".into());
    l.push(String::new());
    if d.get("next_id").and_then(Value::as_i64).is_some() {
        let body = s(d, "next_prompt").unwrap_or("").trim().to_string();
        let lines: Vec<&str> = body.lines().filter(|x| !x.trim().is_empty()).collect();
        l.push(format!("*{} · 같은 세션의 다음 요청*", full_time(s(d, "next_at"))));
        l.push(String::new());
        for x in lines.iter().take(8) {
            l.push(format!("> {x}"));
        }
        if lines.len() > 8 {
            l.push("> …".into());
        }
        if lines.is_empty() {
            l.push("> _(본문 없음)_".into());
        }
    } else {
        l.push(if is_finished(status) { "_아직 이어진 요청이 없습니다._".into() } else { "_작업이 끝나면 다음 요청이 여기에 이어집니다._".into() });
    }
    l.push(String::new());

    l.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn helpers_match_screen() {
        assert_eq!(duration(Some(42_000)), "42초");
        assert_eq!(duration(Some(192_000)), "3분 12초");
        assert_eq!(duration(Some(3_840_000)), "1시간 4분");
        assert_eq!(tokens(5200), "5.2k");
        assert_eq!(tokens(52_000), "52k");
        assert_eq!(tokens(1_250_000), "1.25M");
        assert_eq!(model_name(Some("claude-opus-5-5[1m]")), "Opus 5.5 · 1M");
        assert_eq!(model_name(Some("claude-haiku-4-5-20251001")), "Haiku 4.5");
        assert_eq!(model_name(Some("gpt-6-astra")), "GPT-6 Astra");
        assert_eq!(model_name(Some("gpt-5.6-sol")), "GPT-5.6 Sol");
        assert_eq!(model_name(Some("gpt-5")), "GPT-5");
        assert_eq!(tool_name(Some("mcp__computer-use__screenshot")), "computer-use:screenshot");
        assert_eq!(tilde_path("/Users/someone/a/b"), "~/a/b");
        let p = format!("{REPLY_HEADER}\n요청: 원래 일\n\n이렇게 해 줘");
        assert_eq!(phone_reply(Some(&p)), Some((Some("원래 일".into()), "이렇게 해 줘".into())));
        // 이미지만 보낸 답(경로 목록을 뗀 뒤)
        let p = format!("{REPLY_HEADER}\n요청: 원래 일");
        assert_eq!(phone_reply(Some(&p)), Some((Some("원래 일".into()), String::new())));
    }

    fn sections_in_order(md: &str, order: &[&str]) {
        let mut at = 0;
        for h in order {
            let i = md[at..].find(h).unwrap_or_else(|| panic!("{h} 없음(또는 순서 다름):\n{md}"));
            at += i + h.len();
        }
    }

    #[test]
    fn document_sections_in_order() {
        let d = json!({
            "turn": {"seq": 3, "prompt_text": "로그인 버그 고쳐 줘\n자세히", "prompt_at": "2026-09-24T01:02:03Z",
                     "status": "done", "needs_input": true, "response_text": "고쳤습니다.\n다음은 어떻게 할까요?",
                     "understanding": "토큰 만료 검사가 빠져 있습니다.",
                     "summary": "버그 수정", "duration_ms": 192000, "active_ms": 60000, "tool_calls": 3, "error_count": 1,
                     "cwd": "/Users/x/p", "model": "claude-opus-5-5", "input_tokens": 1200, "output_tokens": 5200,
                     "cache_read": 52000, "cache_create_5m": 800},
            "session": {"id": "s1", "name": "api-refactor", "project_name": "p", "git_branch": "main", "cost_usd": 1.5},
            "steps": [{"kind": "text", "text": "토큰 만료 검사가 빠져 있습니다.", "at": "2026-09-24T01:02:03Z"},
                      {"kind": "tool", "name": "Read", "text": "a.rs", "at": "2026-09-24T01:02:04Z"},
                      {"kind": "tool", "name": "Read", "text": "b.rs", "at": "2026-09-24T01:02:04Z"},
                      {"kind": "tool", "name": "Bash", "text": "ls", "at": "2026-09-24T01:02:04Z"},
                      {"kind": "error", "text": "실패", "at": "2026-09-24T01:02:04Z"},
                      {"kind": "text", "text": "고쳤습니다.\n다음은 어떻게 할까요?", "at": "2026-09-24T01:02:05Z"}],
            "tools": [["Bash", 1], ["Read", 2]], "files": [["/Users/x/p/src/a.rs", 2]],
            "subagents": [{"agent_type": "general-purpose", "description": "찾기\n둘째 줄", "background": true, "duration_ms": 42000}],
            "hooks": [{"event": "Stop", "at": "2026-09-24T01:02:05Z", "detail": null}],
            "next_id": null
        });
        let md = build(&d);
        sections_in_order(
            &md,
            &["# 로그인 버그 고쳐 줘", "## 질문", "## 이해", "## 결과", "**버그 수정**", "## 응답 필요", "## 과정", "### 바뀐 파일", "### 서브에이전트", "### 시간순", "## 비용", "## 이어진 요청"],
        );
        assert!(md.contains("**답이 필요해 보임** _(추정"), "{md}");
        assert!(md.contains("- 다음은 어떻게 할까요?"));
        assert!(md.contains("도구 3회(Read 2, Bash 1) · 파일 1 · 서브에이전트 1 · 오류 1"), "{md}");
        assert!(md.contains("- `src/a.rs` (2번 수정)"));
        assert!(md.contains("- general-purpose · 찾기 둘째 줄 · 42초 · 백그라운드"), "{md}");
        assert!(md.contains("`Read` ×2\n  - a.rs\n  - b.rs\n") && md.contains("`Bash` ls\n"), "{md}");
        assert!(md.contains("**오류** — 실패"));
        assert!(md.contains("Opus 5.5 · 입력 1.2k / 출력 5.2k · 캐시 읽기 52k · 쓰기 800 · 작업 1분 · 도구 3회"), "{md}");
        assert!(md.contains("| 세션 | api-refactor (s1) |"));
        // 응답·이해와 같은 글 단계는 과정에서 뺀다(중복 노출 방지)
        let process = md.split("### 시간순").nth(1).unwrap().split("## 비용").next().unwrap();
        assert!(!process.contains("다음은 어떻게") && !process.contains("토큰 만료"), "{process}");
        // 빠진 것: 훅 이벤트 절·14행 정보 표·달러
        assert!(!md.contains("훅 이벤트") && !md.contains("`Stop`"));
        assert!(!md.contains("토큰 — 캐시") && !md.contains("API 호출"));
        assert!(!md.contains('$'));
    }

    #[test]
    fn understanding_filter_matches_screen() {
        let v: Value = serde_json::from_str(include_str!("../tests/fixtures/turn_card_vectors.json")).unwrap();
        for c in v["understanding"].as_array().unwrap() {
            let got = understanding_of(c["u"].as_str(), c["r"].as_str());
            assert_eq!(got.as_deref(), c["out"].as_str(), "{c}");
        }
        // 착수 멘트만 있으면 이해 칸이 없다
        let d = json!({"turn": {"prompt_text": "x", "status": "done", "understanding": "확인해 보겠습니다.", "response_text": "됐습니다."},
                       "session": {}, "steps": [], "tools": [], "files": [], "subagents": []});
        assert!(!build(&d).contains("## 이해"));
    }

    #[test]
    fn attention_sure_vs_guess() {
        let ask = [json!({"kind": "tool", "name": "AskUserQuestion", "text": "어느 쪽?"})];
        let perm = [json!({"kind": "tool", "name": "Bash", "text": "rm"})];
        let waiting = json!({"status": "waiting"});
        assert_eq!(attention_of(&waiting, &ask), Some((true, "질문에 답 필요", vec!["어느 쪽?".to_string()])));
        assert_eq!(attention_of(&waiting, &perm), Some((true, "권한 승인 대기", vec![])));
        assert_eq!(attention_of(&waiting, &[]), Some((true, "승인·답변 대기", vec![])));
        let guess = json!({"status": "done", "needs_input": true, "response_text": "끝.\n- 배포할까요?"});
        assert_eq!(attention_of(&guess, &[]), Some((false, "답이 필요해 보임", vec!["배포할까요?".to_string()])));
        // 작업 중에 받은 말(ask 단계)은 응답 필요가 아니다
        let done = json!({"status": "done", "needs_input": false});
        assert_eq!(attention_of(&done, &[json!({"kind": "ask", "text": "x"})]), None);
        // 응답 필요 칸은 있을 때만
        let d = json!({"turn": {"prompt_text": "x", "status": "done", "response_text": "됐습니다."}, "session": {}, "steps": []});
        assert!(!build(&d).contains("## 응답 필요"));
    }
}
