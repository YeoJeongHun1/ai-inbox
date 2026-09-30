//! 요청 하나를 마크다운 문서로 — 화면의 `src/markdown.ts` 와 같은 문서를 폰에도 보내기 위한 Rust 판.
//! 입력은 `api::TurnDetail` 을 JSON 으로 바꾼 값(필드 이름이 화면 쪽 타입과 같다).
//! 🚨 화면 쪽 문서 구성을 바꾸면 여기도 같이 바꾼다(테스트가 절 제목 순서를 본다).

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

pub fn num(x: i64) -> String {
    let digits = x.abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    if x < 0 { format!("-{out}") } else { out }
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

fn usd(x: Option<f64>) -> String {
    match x {
        None => "—".into(),
        Some(v) => format!("${:.*}", if v < 10.0 { 2 } else { 1 }, v),
    }
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

/// 답이 필요한 물음: 응답 끝에서 물음표로 끝나는 줄들
pub fn questions_of(response: Option<&str>) -> Vec<String> {
    let Some(r) = response else { return vec![] };
    let lines: Vec<&str> = r.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(8)..];
    let q = regex::Regex::new(r"[?？]\s*\**$").unwrap();
    let ask = regex::Regex::new(r"(주세요|알려 주세요|말씀해 주세요)\.?$").unwrap();
    tail.iter().filter(|l| q.is_match(l) || ask.is_match(l)).map(|l| l.to_string()).collect()
}

fn render_steps(steps: &[&Value]) -> Vec<String> {
    let mut out = Vec::new();
    let mut tools: Vec<&Value> = Vec::new();
    let flush = |tools: &mut Vec<&Value>, out: &mut Vec<String>| {
        if tools.is_empty() {
            return;
        }
        let mut counts: Vec<(String, usize)> = Vec::new();
        for t in tools.iter() {
            let name = tool_name(s(t, "name"));
            match counts.iter_mut().find(|(n, _)| *n == name) {
                Some(c) => c.1 += 1,
                None => counts.push((name, 1)),
            }
        }
        let head = counts.iter().map(|(n, c)| if *c > 1 { format!("{n} ×{c}") } else { n.clone() }).collect::<Vec<_>>().join(" · ");
        out.push(format!("- {} **도구 {}회** — {head}", clock(s(tools[0], "at")), tools.len()));
        for t in tools.iter().take(6) {
            out.push(format!("  - {}", format!("`{}` {}", tool_name(s(t, "name")), s(t, "text").unwrap_or("")).trim()));
        }
        if tools.len() > 6 {
            out.push(format!("  - … 외 {}개", tools.len() - 6));
        }
        tools.clear();
    };
    for st in steps {
        let kind = s(st, "kind").unwrap_or("");
        if kind == "tool" {
            tools.push(st);
            continue;
        }
        flush(&mut tools, &mut out);
        let t = s(st, "text").unwrap_or("").trim().to_string();
        let at = clock(s(st, "at"));
        match kind {
            "text" => {
                let body = if t.chars().count() > 600 { format!("{}…", t.chars().take(600).collect::<String>()) } else { t };
                let flat = regex::Regex::new(r"\n+").unwrap().replace_all(&body, " ").into_owned();
                out.push(format!("- {at} {flat}"));
            }
            "task" => out.push(format!("- {at} 백그라운드 알림 — {t}")),
            "ask" => {
                let who = s(st, "name").map(|n| format!("{n} 이 보냄")).unwrap_or_else(|| "작업 중에 받은 말".into());
                let flat = regex::Regex::new(r"\n+").unwrap().replace_all(&t, " ").into_owned();
                out.push(format!("- {at} **{who}** — {flat}"));
            }
            "summary" => out.push(format!("- {at} 요약 — {t}")),
            "error" => out.push(format!("- {at} 오류 — {t}")),
            "interrupt" => out.push(format!("- {at} 사용자가 중단함")),
            "compact" => out.push(format!("- {at} 대화 압축")),
            "continue" => out.push(format!("- {at} {t}")),
            _ => {}
        }
    }
    flush(&mut tools, &mut out);
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

    l.push("## 요청".into());
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

    if needs_input {
        let qs = questions_of(s(t, "response_text"));
        if !qs.is_empty() {
            l.push("## 답이 필요한 질문".into());
            l.push(String::new());
            let bullet = regex::Regex::new(r"^[-*]\s+").unwrap();
            for q in qs {
                l.push(format!("- {}", bullet.replace(&q, "")));
            }
            l.push(String::new());
        }
    }

    l.push("## 작업 요약".into());
    l.push(String::new());
    if let Some(sum) = s(t, "summary") {
        l.push(sum.to_string());
        l.push(String::new());
    }
    let empty = vec![];
    let tools = d["tools"].as_array().unwrap_or(&empty);
    let files = d["files"].as_array().unwrap_or(&empty);
    let subs = d["subagents"].as_array().unwrap_or(&empty);
    let mut facts = Vec::new();
    if !tools.is_empty() {
        let list = tools
            .iter()
            .map(|x| format!("{} {}", tool_name(x[0].as_str()), x[1].as_i64().unwrap_or(0)))
            .collect::<Vec<_>>()
            .join(" · ");
        facts.push(format!("도구 {}회 — {list}", num(n(t, "tool_calls"))));
    }
    if !files.is_empty() {
        facts.push(format!("바뀐 파일 {}개", files.len()));
    }
    if !subs.is_empty() {
        facts.push(format!("서브에이전트 {}개", subs.len()));
    }
    if n(t, "task_notifications") > 0 {
        facts.push(format!("백그라운드 완료 알림 {}번", n(t, "task_notifications")));
    }
    if n(t, "error_count") > 0 {
        facts.push(format!("오류 {}번", n(t, "error_count")));
    }
    if facts.is_empty() {
        facts.push("도구 호출 없이 답함".into());
    }
    for f in facts {
        l.push(format!("- {f}"));
    }
    l.push(String::new());

    if let Some(u) = s(t, "understanding") {
        if Some(u) != s(t, "response_text") {
            l.push("### 처음 이해한 내용".into());
            l.push(String::new());
            l.push(u.trim().to_string());
            l.push(String::new());
        }
    }

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
        l.push("| 종류 | 할 일 | 방식 | 걸린 시간 |".into());
        l.push("|---|---|---|---|".into());
        for a in subs {
            let took = match on(a, "duration_ms") {
                Some(ms) => duration(Some(ms)),
                None if s(a, "ended_at").is_some() => "—".into(),
                None => "진행 중".into(),
            };
            l.push(format!(
                "| {} | {} | {} | {took} |",
                esc(s(a, "agent_type").unwrap_or("-")),
                esc(s(a, "description").unwrap_or("")),
                if a.get("background").and_then(Value::as_bool).unwrap_or(false) { "백그라운드" } else { "대기" }
            ));
        }
        l.push(String::new());
    }

    l.push("## 응답".into());
    l.push(String::new());
    let resp = s(t, "response_text").map(str::trim).filter(|x| !x.is_empty());
    l.push(match resp {
        Some(r) => r.to_string(),
        None if status == "running" => "_(아직 작업 중)_".into(),
        None if s(t, "prompt_source") == Some("mid-turn") => "_(따로 답한 글이 없습니다 — 앞 요청의 작업 과정·응답에 이어집니다)_".into(),
        None => "_(응답 없음)_".into(),
    });
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

    let resp_trim = s(t, "response_text").map(str::trim);
    let steps: Vec<&Value> = d["steps"]
        .as_array()
        .map(|a| a.iter().filter(|x| !(s(x, "kind") == Some("text") && s(x, "text").map(str::trim) == resp_trim)).collect())
        .unwrap_or_default();
    let rendered = render_steps(&steps);
    if !rendered.is_empty() {
        l.push("## 작업 과정".into());
        l.push(String::new());
        l.extend(rendered);
        l.push(String::new());
    }

    l.push("## 정보".into());
    l.push(String::new());
    l.push("| 항목 | 값 |".into());
    l.push("|---|---|".into());
    let mut row = |k: &str, v: String| l.push(format!("| {k} | {} |", esc(&v)));
    row("상태", label.clone());
    row("요청 시각", full_time(s(t, "prompt_at")));
    row("끝난 시각", full_time(s(t, "ended_at")));
    row("걸린 시간", format!("{} (모델 작업 {})", duration(on(t, "duration_ms")), duration(on(t, "active_ms"))));
    row("첫 반응까지", duration(on(t, "ttfr_ms")));
    row("모델", format!("{}{}", model_name(s(t, "model")), s(t, "effort").map(|e| format!(" · effort {e}")).unwrap_or_default()));
    row("API 호출", format!("{}회", num(n(t, "api_calls"))));
    row("토큰 — 입력", num(n(t, "input_tokens")));
    row("토큰 — 캐시 쓰기 (5분 / 1시간)", format!("{} / {}", num(n(t, "cache_create_5m")), num(n(t, "cache_create_1h"))));
    row("토큰 — 캐시 읽기", num(n(t, "cache_read")));
    let think = n(t, "thinking_tokens");
    row("토큰 — 출력 (생각 포함)", format!("{}{}", num(n(t, "output_tokens")), if think > 0 { format!(" (생각 {})", num(think)) } else { String::new() }));
    row("끝났을 때 문맥 크기", format!("{} 토큰", tokens(n(t, "context_tokens"))));
    if n(t, "web_search") > 0 || n(t, "web_fetch") > 0 {
        row("웹 검색 / 가져오기", format!("{} / {}", n(t, "web_search"), n(t, "web_fetch")));
    }
    row("작업 폴더", s(t, "cwd").map(tilde_path).unwrap_or_else(|| "—".into()));
    if let Some(b) = s(t, "git_branch").filter(|b| *b != "HEAD") {
        row("브랜치", b.to_string());
    }
    row("세션", format!("{} ({})", s(se, "name").unwrap_or(""), s(se, "id").unwrap_or("")));
    if let Some(c) = se.get("cost_usd").and_then(Value::as_f64) {
        row("세션 누적 비용 (API 환산)", usd(Some(c)));
    }
    l.push(String::new());

    if let Some(hooks) = d["hooks"].as_array().filter(|h| !h.is_empty()) {
        l.push("### 훅 이벤트".into());
        l.push(String::new());
        for h in hooks {
            let det = &h["detail"];
            let msg = s(det, "message").or(s(det, "source")).or(s(det, "reason")).unwrap_or("");
            l.push(format!(
                "- {} `{}`{}",
                clock(s(h, "at")),
                s(h, "event").unwrap_or(""),
                if msg.is_empty() { String::new() } else { format!(" — {msg}") }
            ));
        }
        l.push(String::new());
    }

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
        assert_eq!(num(1234567), "1,234,567");
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

    #[test]
    fn document_sections_in_order() {
        let d = json!({
            "turn": {"seq": 3, "prompt_text": "로그인 버그 고쳐 줘\n자세히", "prompt_at": "2026-09-24T01:02:03Z",
                     "status": "done", "needs_input": true, "response_text": "고쳤습니다.\n다음은 어떻게 할까요?",
                     "summary": "버그 수정", "duration_ms": 192000, "tool_calls": 3, "cwd": "/Users/x/p",
                     "model": "claude-opus-5-5", "output_tokens": 5200},
            "session": {"id": "s1", "name": "api-refactor", "project_name": "p", "git_branch": "main", "cost_usd": 1.5},
            "steps": [{"kind": "tool", "name": "Bash", "text": "ls", "at": "2026-09-24T01:02:04Z"},
                      {"kind": "text", "text": "고쳤습니다.\n다음은 어떻게 할까요?", "at": "2026-09-24T01:02:05Z"}],
            "tools": [["Bash", 3]], "files": [["/Users/x/p/src/a.rs", 2]], "subagents": [], "hooks": [],
            "next_id": null
        });
        let md = build(&d);
        let order = ["# 로그인 버그 고쳐 줘", "## 요청", "## 답이 필요한 질문", "## 작업 요약", "### 바뀐 파일", "## 응답", "## 이어진 요청", "## 작업 과정", "## 정보"];
        let mut at = 0;
        for h in order {
            let i = md[at..].find(h).unwrap_or_else(|| panic!("{h} 없음:\n{md}"));
            at += i + h.len();
        }
        assert!(md.contains("- `src/a.rs` (2번 수정)"));
        assert!(md.contains("| 모델 | Opus 5.5 |"));
        assert!(md.contains("**도구 1회** — Bash"));
        // 응답과 같은 text 단계는 작업 과정에서 뺀다
        assert!(!md.split("## 작업 과정").nth(1).unwrap().contains("다음은 어떻게"));
    }
}
