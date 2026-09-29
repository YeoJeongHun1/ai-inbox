//! 시각은 전부 UTC ISO-8601 문자열(밀리초, 끝에 Z)로 저장한다 — 대화 기록과 같은 형식이라
//! 문자열 비교가 곧 시각 비교가 된다.

use chrono::{DateTime, SecondsFormat, TimeZone, Utc};

pub fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn iso_from_ms(ms: i64) -> String {
    Utc.timestamp_millis_opt(ms)
        .single()
        .unwrap_or_else(Utc::now)
        .to_rfc3339_opts(SecondsFormat::Millis, true)
}

pub fn parse(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts).ok().map(|d| d.with_timezone(&Utc))
}

/// 대화 기록의 timestamp 를 우리 형식으로 맞춘다(형식이 다르면 파싱해서 다시 쓴다).
pub fn normalize(ts: &str) -> Option<String> {
    parse(ts).map(|d| d.to_rfc3339_opts(SecondsFormat::Millis, true))
}

pub fn diff_ms(from: &str, to: &str) -> Option<i64> {
    Some((parse(to)? - parse(from)?).num_milliseconds().max(0))
}

pub fn age_ms(ts: &str) -> Option<i64> {
    parse(ts).map(|d| (Utc::now() - d).num_milliseconds())
}
