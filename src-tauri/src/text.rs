//! 대화 기록 텍스트 다듬기: 프롬프트 잡음 제거, 비밀값 가리기, 도구 호출 한 줄 요약.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("regex"))
}

/// 문자 수 기준으로 자른다(한글이 바이트 경계에서 깨지지 않게).
pub fn clip(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    out.push('…');
    out
}

pub fn first_line(s: &str) -> &str {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
}

/// 토큰·키로 보이는 문자열을 가린다. 앱 안에서만 보지만 MD 로 내보내 공유하거나 알림으로 뜰 수 있어서다.
/// 완벽한 탐지는 아니다(README 의 보안 절 참고) — 흔한 형식을 잡는다. **저장하는 모든 텍스트 칸이 여기를 지난다.**
pub fn redact(s: &str) -> String {
    static PEM: OnceLock<Regex> = OnceLock::new();
    static URL_CRED: OnceLock<Regex> = OnceLock::new();
    static KV: OnceLock<Regex> = OnceLock::new();
    static AUTH: OnceLock<Regex> = OnceLock::new();
    static TOKEN: OnceLock<Regex> = OnceLock::new();
    // 개인키 블록 (PEM·OpenSSH·PGP). END 가 없으면 끝까지
    let pem = re(&PEM, r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY[A-Z ]*-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY[A-Z ]*-----|\z)");
    // scheme://user:password@host
    let url_cred = re(&URL_CRED, r"(?i)\b([a-z][a-z0-9+.\-]*://[^\s/:@]+):([^\s/@]+)@");
    // key=value · key: value · "key": "value" · accessToken=… (camelCase 포함)
    let kv = re(
        &KV,
        r#"(?i)\b([a-z0-9_.\-]*(?:api[_-]?key|apikey|secret|token|password|passwd|pwd|passphrase|credential|access[_-]?key|private[_-]?key|auth)s?)(["']?\s*[:=]\s*["']?)([^\s"',;{}()<>\[\]]{8,})"#,
    );
    let auth = re(&AUTH, r"(?i)\b(bearer|basic|token)\s+[A-Za-z0-9._~+/\-]{8,}=*");
    let token = re(
        &TOKEN,
        r"(sk-(?:ant-|proj-)?[A-Za-z0-9_\-]{20,}|(?:sk|rk|pk)_(?:live|test)_[A-Za-z0-9]{16,}|whsec_[A-Za-z0-9]{20,}|ak_live_[A-Za-z0-9_\-]{8,}|gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}|glpat-[A-Za-z0-9_\-]{20,}|hf_[A-Za-z0-9]{30,}|xox[abpr]-[A-Za-z0-9\-]{10,}|xapp-[A-Za-z0-9\-]{10,}|hooks\.slack\.com/services/[A-Za-z0-9/]{20,}|discord(?:app)?\.com/api/webhooks/[0-9]+/[A-Za-z0-9_\-]{20,}|AKIA[0-9A-Z]{16}|ASIA[0-9A-Z]{16}|AIza[0-9A-Za-z_\-]{30,}|GOCSPX-[A-Za-z0-9_\-]{20,}|ya29\.[A-Za-z0-9_\-]{20,}|npm_[A-Za-z0-9]{30,}|eyJ[A-Za-z0-9_\-]{10,}\.eyJ[A-Za-z0-9_\-]{10,}\.[A-Za-z0-9_\-]{10,})",
    );
    if !(pem.is_match(s) || url_cred.is_match(s) || kv.is_match(s) || auth.is_match(s) || token.is_match(s)) {
        return s.to_string();
    }
    let out = pem.replace_all(s, "[개인키 가림]");
    let out = url_cred.replace_all(&out, "$1:[가림]@");
    let out = token.replace_all(&out, |c: &regex::Captures| {
        let head: String = c[0].chars().take(6).collect();
        format!("{head}…[가림]")
    });
    let out = kv.replace_all(&out, |c: &regex::Captures| {
        // 이미 가린 값이면 그대로
        if c[3].contains("[가림]") { c[0].to_string() } else { format!("{}{}[가림]", &c[1], &c[2]) }
    });
    let out = auth.replace_all(&out, "$1 [가림]");
    out.into_owned()
}

/// 가린 뒤 자른다(자르고 가리면 토큰 앞부분이 남을 수 있다).
pub fn safe(s: &str, max_chars: usize) -> String {
    clip(&redact(s), max_chars)
}

/// user/assistant content(문자열 또는 블록 배열)에서 보이는 텍스트만.
pub fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => {
            let mut parts = Vec::new();
            for b in blocks {
                match b.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = b.get("text").and_then(Value::as_str) {
                            if !t.is_empty() {
                                parts.push(t.to_string());
                            }
                        }
                    }
                    Some("image") => parts.push("[이미지]".to_string()),
                    _ => {}
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

pub struct CleanPrompt {
    pub text: String,
    pub slash: Option<String>,
}

/// 사람이 실제로 친 말만 남긴다. `/스킬 인자` 는 (slash, 인자) 로 나눈다.
pub fn clean_prompt(raw: &str) -> CleanPrompt {
    static NOISE: OnceLock<Regex> = OnceLock::new();
    static NAME: OnceLock<Regex> = OnceLock::new();
    static ARGS: OnceLock<Regex> = OnceLock::new();
    let noise = re(
        &NOISE,
        r"(?s)<(system-reminder|local-command-caveat|local-command-stdout|local-command-stderr|command-message)>.*?</(system-reminder|local-command-caveat|local-command-stdout|local-command-stderr|command-message)>\s*",
    );
    let name = re(&NAME, r"<command-name>\s*(/?[^<\s]+)\s*</command-name>");
    let args = re(&ARGS, r"(?s)<command-args>(.*?)</command-args>");

    let slash = name.captures(raw).map(|c| {
        let n = c[1].to_string();
        if n.starts_with('/') { n } else { format!("/{n}") }
    });
    let mut text = noise.replace_all(raw, "").into_owned();
    if slash.is_some() {
        let arg = args.captures(&text).map(|c| c[1].trim().to_string()).unwrap_or_default();
        text = name.replace_all(&text, "").into_owned();
        text = args.replace_all(&text, "").into_owned();
        let rest = text.trim();
        text = if rest.is_empty() { arg } else if arg.is_empty() { rest.to_string() } else { format!("{arg}\n{rest}") };
    }
    CleanPrompt { text: text.trim().to_string(), slash }
}

/// 대기 훅(`ai-inbox wake`, asyncRewake)이 깨운 줄이면 넘긴 말(머리말부터). Claude Code 는 이걸
/// `Stop hook blocking error from command "…": <훅 stderr>` 로 감싼 task-notification 으로 남긴다(2026-09-24 실측).
pub fn rewake_message(raw: &str) -> Option<String> {
    if !raw.contains("hook blocking error from command") {
        return None;
    }
    let start = [crate::conoti::INBOX_HEADER, crate::conoti::SCHED_HEADER, crate::conoti::REPLY_HEADER].iter().filter_map(|h| raw.find(h)).min()?;
    let rest = &raw[start..];
    let end = rest.find("</system-reminder>").unwrap_or(rest.len());
    let msg = rest[..end].trim_end();
    (!msg.is_empty()).then(|| msg.to_string())
}

/// 대기 훅으로 넣은 말은 터미널에 요약 한 줄("Stop hook feedback")만 보이고 본문은 모델에게만 간다 —
/// 그래서 Claude 가 답 첫머리에 받은 말을 이 표시로 시작하는 인용(`> `)으로 옮겨 적는다(`wake::NOTE`).
pub const ECHO_MARK: &str = "📥";

/// 답 첫머리의 받은 말 인용을 뗀다 — 앱·폰은 요청을 따로 보여 주니 답에 두 번 나오지 않게.
/// 첫 줄이 `>` 로 시작하고 표시가 있을 때만, 이어지는 `>` 줄 묶음까지.
pub fn strip_echo(t: &str) -> &str {
    let s = t.trim_start();
    let first = s.lines().next().unwrap_or("");
    if !(first.starts_with('>') && first.contains(ECHO_MARK)) {
        return t;
    }
    let mut rest = s;
    while !rest.is_empty() {
        let (line, tail) = rest.split_once('\n').unwrap_or((rest, ""));
        if !line.trim_start().starts_with('>') {
            break;
        }
        rest = tail;
    }
    rest.trim_start()
}

/// 메신저식 답장 — 보낸 말 본문 맨 앞의 한 줄 `답장: #12 결과 「발췌」` + 빈 줄. 한 세션에서 여러 작업이 도니
/// 어느 요청·결과를 두고 하는 말인지 세션(Claude)과 화면(폰·데스크톱)이 같이 알게 한다(0.7.0).
pub const QUOTE_PREFIX: &str = "답장: #";

#[derive(Debug, Clone, PartialEq)]
pub struct Quote {
    pub seq: i64,
    /// "prompt" | "response"
    pub part: &'static str,
    pub text: String,
}

/// 답장 대상 한 줄 — 요청이면 160자, 결과면 240자까지 한 줄로
pub fn quote_line(seq: i64, part: &str, src: &str) -> String {
    let (label, max) = if part == "response" { ("결과", 240) } else { ("요청", 160) };
    let flat = src
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .replace("**", "")
        .replace('`', "")
        .replace(['「', '」'], "\"");
    format!("{QUOTE_PREFIX}{seq} {label} 「{}」", clip(&flat, max))
}

/// 본문 맨 앞의 답장 줄을 풀어 (답장 대상, 나머지 본문)
pub fn split_quote(body: &str) -> (Option<Quote>, &str) {
    static Q: OnceLock<Regex> = OnceLock::new();
    let q = re(&Q, r"^답장: #(\d+) (요청|결과) 「(.*)」$");
    let s = body.trim_start_matches(['\n', '\r']);
    let (first, rest) = s.split_once('\n').unwrap_or((s, ""));
    let Some(m) = q.captures(first.trim_end()) else { return (None, body) };
    let Ok(seq) = m[1].parse::<i64>() else { return (None, body) };
    let part = if &m[2] == "결과" { "response" } else { "prompt" };
    (Some(Quote { seq, part, text: m[3].to_string() }), rest.trim_start_matches(['\n', '\r']))
}

/// `<channel source="…" …>본문</channel>` → 본문 (채널로 들어온 메시지)
pub fn strip_channel_tag(raw: &str) -> String {
    static C: OnceLock<Regex> = OnceLock::new();
    let c = re(&C, r"(?s)<channel\b[^>]*>(.*?)</channel>");
    let parts: Vec<String> = c.captures_iter(raw).map(|m| m[1].trim().to_string()).collect();
    if parts.is_empty() { raw.trim().to_string() } else { parts.join("\n\n") }
}

/// 다른 세션이 보낸 메시지 → (보낸 세션 이름, 본문)
pub fn parse_peer(raw: &str) -> (Option<String>, String) {
    static P: OnceLock<Regex> = OnceLock::new();
    let p = re(&P, r#"(?s)<cross-session-message[^>]*?from-name="([^"]*)"[^>]*>(.*?)</cross-session-message>"#);
    if let Some(c) = p.captures(raw) {
        return (Some(c[1].to_string()), c[2].trim().to_string());
    }
    (None, raw.trim().to_string())
}

/// <task-notification> → (tool-use-id, status, summary)
pub fn parse_task_notification(raw: &str) -> (Option<String>, Option<String>, String) {
    fn tag(raw: &str, name: &str) -> Option<String> {
        let open = format!("<{name}>");
        let close = format!("</{name}>");
        let start = raw.find(&open)? + open.len();
        let end = raw[start..].find(&close)? + start;
        Some(raw[start..end].trim().to_string())
    }
    let summary = tag(raw, "summary").unwrap_or_else(|| clip(first_line(raw), 200));
    (tag(raw, "tool-use-id"), tag(raw, "status"), summary)
}

/// 도구 호출 한 줄 요약 (작업 과정에 보이는 문장)
pub fn tool_summary(name: &str, input: &Value, cwd: Option<&str>) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).map(str::to_string);
    let short_path = |p: String| -> String {
        if let Some(c) = cwd {
            if let Some(rest) = p.strip_prefix(c) {
                let rest = rest.trim_start_matches(['/', '\\']);
                if !rest.is_empty() {
                    return rest.to_string();
                }
            }
        }
        p
    };
    let out = match name {
        "Bash" | "PowerShell" => s("description").or_else(|| s("command").map(|c| first_line(&c).to_string())),
        "Read" | "Edit" | "Write" | "NotebookEdit" | "MultiEdit" => {
            s("file_path").or_else(|| s("notebook_path")).map(short_path)
        }
        "Glob" | "Grep" => s("pattern").map(|p| match s("path") {
            Some(path) => format!("{p}  ({})", short_path(path)),
            None => p,
        }),
        "Agent" | "Task" => {
            let ty = s("subagent_type").unwrap_or_else(|| "general-purpose".into());
            Some(format!("[{ty}] {}", s("description").unwrap_or_default()))
        }
        "WebSearch" => s("query"),
        "WebFetch" => s("url"),
        "Skill" => s("skill").map(|k| match s("args") {
            Some(a) if !a.is_empty() => format!("/{k} {a}"),
            _ => format!("/{k}"),
        }),
        "TodoWrite" => input.get("todos").and_then(Value::as_array).map(|t| format!("할 일 {}개", t.len())),
        "TaskCreate" => s("subject"),
        "AskUserQuestion" => input
            .get("questions")
            .and_then(Value::as_array)
            .and_then(|q| q.first())
            .and_then(|q| q.get("question"))
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    };
    let out = out.or_else(|| {
        // 알려지지 않은 도구: 처음 나오는 문자열 값
        input.as_object().and_then(|m| m.values().find_map(|v| v.as_str().map(str::to_string)))
    });
    safe(&out.unwrap_or_default().replace('\n', " "), 240)
}

/// 결과 보고가 사용자에게 무언가를 묻고 끝났나.
pub fn asks_user(response: &str) -> bool {
    let tail: String = {
        let lines: Vec<&str> = response.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        lines[lines.len().saturating_sub(2)..].join(" ")
    };
    let t = tail.trim_end_matches(['*', '_', '`', ')', ' ']);
    if t.ends_with('?') || t.ends_with('？') {
        return true;
    }
    const ASKS: &[&str] = &[
        "말씀해 주세요", "말씀해주세요", "알려 주세요", "알려주세요", "골라 주세요", "골라주세요",
        "정해 주세요", "정해주세요", "확인해 주세요", "확인해주세요", "답해 주세요", "선택해 주세요",
        "결정해 주세요", "승인해 주세요",
    ];
    ASKS.iter().any(|a| t.contains(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slash_with_args() {
        let c = clean_prompt("<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>최근 변경 점검</command-args>");
        assert_eq!(c.slash.as_deref(), Some("/review"));
        assert_eq!(c.text, "최근 변경 점검");
    }

    #[test]
    fn clear_is_empty() {
        let c = clean_prompt("<command-name>/clear</command-name>\n            <command-message>clear</command-message>\n            <command-args></command-args>");
        assert_eq!(c.slash.as_deref(), Some("/clear"));
        assert_eq!(c.text, "");
    }

    #[test]
    fn strips_reminder() {
        let c = clean_prompt("<system-reminder>x\ny</system-reminder>\n안녕");
        assert_eq!(c.text, "안녕");
        assert!(c.slash.is_none());
    }

    #[test]
    fn redacts() {
        let r = redact("key sk-ant-abcdefghijklmnopqrstuvwxyz0123 end");
        assert!(r.contains("[가림]"));
        assert!(!r.contains("abcdefghijklmnop"));
    }

    #[test]
    fn redacts_more() {
        let r = redact("export OPENAI_API_KEY=abcd1234efgh5678 && curl -H 'Authorization: Bearer abcdefghijklmnopqrstuvwxyz'");
        assert!(!r.contains("abcd1234efgh5678"), "{r}");
        assert!(!r.contains("abcdefghijklmnopqrstuvwxyz"), "{r}");
        assert!(r.contains("OPENAI_API_KEY="), "{r}");
        let pem = redact("a\n-----BEGIN RSA PRIVATE KEY-----\nMIIE\n-----END RSA PRIVATE KEY-----\nb");
        assert_eq!(pem, "a\n[개인키 가림]\nb");
        assert_eq!(redact("평범한 문장입니다. token 이라는 단어만 있음"), "평범한 문장입니다. token 이라는 단어만 있음");
    }

    #[test]
    fn redacts_formats() {
        let cases = [
            r#"{"password": "hunter2hunter2", "user": "kim"}"#,
            "accessToken=abcdefgh12345678",
            "DATABASE_URL=postgres://admin:s3cr3tpass@db.example.com:5432/app",
            "Authorization: Basic dXNlcjpwYXNz",
            "-----BEGIN PGP PRIVATE KEY BLOCK-----\nlQOYBF\n",
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA",
            "hf_abcdefghijklmnopqrstuvwxyz0123456789",
            "whsec_abcdefghijklmnopqrstuvwx",
            "GOCSPX-abcdefghijklmnopqrstuv",
            "https://hooks.slack.com/services/T0000/B0000/abcdefghijklmnopqrstuvwx",
        ];
        let secrets = ["hunter2hunter2", "abcdefgh12345678", "s3cr3tpass", "dXNlcjpwYXNz", "lQOYBF", "b3BlbnNzaC1rZXkt",
                       "hijklmnopqrstuvwxyz0123456789", "ijklmnopqrstuvwx", "ghijklmnopqrstuv", "abcdefghijklmnopqrstuvwx"];
        for (c, secret) in cases.iter().zip(secrets) {
            let r = redact(c);
            assert!(!r.contains(secret), "not masked: {c} -> {r}");
        }
        assert!(redact(r#"{"password": "hunter2hunter2"}"#).contains("password"));
        assert!(redact("postgres://admin:s3cr3tpass@db").contains("admin:[가림]@"));
        // 평범한 문장은 그대로
        for plain in ["토큰 사용량을 줄였습니다.", "token 이라는 단어", "Opus 5.5 로 바꿨고 password 정책은 유지"] {
            assert_eq!(redact(plain), plain);
        }
    }

    #[test]
    fn safe_masks_before_clip() {
        let r = safe("key sk-ant-abcdefghijklmnopqrstuvwxyz0123", 12);
        assert!(!r.contains("abcdef"), "{r}");
    }

    #[test]
    fn asks() {
        assert!(asks_user("정리했습니다.\n\n추천안으로 가도 될까요?"));
        assert!(asks_user("바꾸고 싶은 항목만 말씀해 주세요."));
        assert!(!asks_user("배포를 마쳤습니다."));
    }

    #[test]
    fn channel_tag() {
        assert_eq!(strip_channel_tag("<channel source=\"ai-inbox\" reply_id=\"rp_1\">\n폰 답\n</channel>"), "폰 답");
        assert_eq!(strip_channel_tag("그냥 글"), "그냥 글");
    }

    #[test]
    fn peer() {
        let (n, b) = parse_peer("Another Claude session sent a message: <cross-session-message from=\"uds:/tmp/x.sock\" from-name=\"dev-02\" from-mode=\"bypass\">받았어</cross-session-message>");
        assert_eq!(n.as_deref(), Some("dev-02"));
        assert_eq!(b, "받았어");
    }

    #[test]
    fn rewake_lines() {
        let raw = "<task-notification>\n<summary>Stop hook feedback</summary>\n</task-notification>\n<system-reminder>\nStop hook blocking error from command \"ConfigChange:user_settings\": 사용자가 AI Inbox(데스크톱 앱·폰)에서 이 세션에 보낸 다음 지시입니다.\n\nAI Inbox 앱에서 보낸 사용자 메시지\n\n테스트 돌려 줘\n</system-reminder>";
        assert_eq!(rewake_message(raw).as_deref(), Some("AI Inbox 앱에서 보낸 사용자 메시지\n\n테스트 돌려 줘"));
        assert_eq!(rewake_message("<task-notification><summary>CI 끝</summary></task-notification>"), None);
        assert_eq!(rewake_message("Stop hook blocking error from command \"x\": 남의 훅 오류"), None);
    }

    #[test]
    fn reply_quote_round_trip() {
        let l = quote_line(12, "response", "**배포**했습니다.\n\n`dev` 에 「올림」");
        assert_eq!(l, "답장: #12 결과 「배포했습니다. dev 에 \"올림\"」");
        let body = format!("{l}\n\n그럼 운영도 올려 줘");
        let (q, rest) = split_quote(&body);
        assert_eq!(q, Some(Quote { seq: 12, part: "response", text: "배포했습니다. dev 에 \"올림\"".into() }));
        assert_eq!(rest, "그럼 운영도 올려 줘");
        let only = quote_line(3, "prompt", "원래 요청");
        let (q, rest) = split_quote(&only);
        assert_eq!((q.map(|q| (q.seq, q.part)), rest), (Some((3, "prompt")), ""));
        // 답장 줄이 없으면 그대로
        assert_eq!(split_quote("답장: 고마워"), (None, "답장: 고마워"));
        assert_eq!(split_quote("그냥 말").1, "그냥 말");
        // 긴 결과는 240자에서 자른다
        assert_eq!(quote_line(1, "response", &"가".repeat(500)).chars().filter(|c| *c == '가').count(), 240);
    }

    #[test]
    fn echo_quote_is_stripped() {
        let t = "> 📥 폰에서 온 사용자 답 (AI Inbox · 코노티)\n> 요청: 빌드\n>\n> 배포해 줘\n\n배포했습니다.";
        assert_eq!(strip_echo(t), "배포했습니다.");
        // 인용만 있는 글 조각은 빈 글
        assert_eq!(strip_echo("> 📥 AI Inbox 앱에서 보낸 사용자 메시지\n>\n> 테스트"), "");
        // 표시 없는 인용·평범한 글은 그대로
        assert_eq!(strip_echo("> 참고\n본문"), "> 참고\n본문");
        assert_eq!(strip_echo("배포했습니다 📥"), "배포했습니다 📥");
    }
}
