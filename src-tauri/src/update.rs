//! 새 버전 — 세 가지를 알리고, 언제 바꿀지는 사용자가 고른다.
//!   1. 업데이트된 뒤 첫 실행: "바뀐 점"(앱에 들어 있는 CHANGELOG.md 의 그 버전 절)
//!   2. GitHub 릴리스에 새 버전: 알림만 한다. 사용자가 "업데이트"를 누르면 받아서(서명 확인 — 공개키는 앱에 고정) 설치하고 다시 시작.
//!      설정에서 끌 수 있다. 이것만 네트워크를 쓴다(github.com 의 공개 릴리스 정보).
//!   3. 디스크의 앱이 지금 도는 것보다 새것(다른 방법으로 설치됨): "다시 시작하면 적용"
//! 폰 연결 서버가 옛 버전을 받지 않으면(`bye: upgrade`) 폰 연결만 멈추고 업데이트를 권한다(relay/client.rs).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use semver::Version;
use serde::Serialize;
use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_updater::UpdaterExt;

use crate::{db, paths, time};

const CHANGELOG: &str = include_str!("../../CHANGELOG.md");
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

/// CHANGELOG.md 의 `## <버전>` 절
pub fn notes_for(version: &str) -> Option<String> {
    let head = format!("## {version}");
    let start = CHANGELOG.lines().position(|l| l.trim() == head)?;
    let body: Vec<&str> = CHANGELOG.lines().skip(start + 1).take_while(|l| !l.starts_with("## ")).collect();
    let s = body.join("\n").trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// a 가 b 보다 새 버전인가(형식이 틀리면 아니다)
pub fn newer(a: &str, b: &str) -> bool {
    match (Version::parse(a.trim().trim_start_matches('v')), Version::parse(b.trim().trim_start_matches('v'))) {
        (Ok(a), Ok(b)) => a > b,
        _ => false,
    }
}

#[derive(Serialize, Clone)]
pub struct Available {
    pub version: String,
    pub notes: Option<String>,
    pub date: Option<String>,
}

#[derive(Default)]
pub struct Shared {
    available: Mutex<Option<Available>>,
    /// 디스크에 설치된 더 새 버전
    installed: Mutex<Option<String>>,
}

#[derive(Serialize)]
pub struct UpdateState {
    current: String,
    /// 업데이트된 뒤 처음 열었으면 그 버전의 바뀐 점
    whats_new: Option<String>,
    check: bool,
    skipped: Option<String>,
    available: Option<Available>,
    installed: Option<String>,
    last_check: Option<String>,
    last_error: Option<String>,
}

fn meta(key: &str) -> Option<String> {
    db::open(&paths::db_path()).ok().and_then(|c| db::get_meta(&c, key))
}

fn set_meta(key: &str, value: &str) {
    if let Ok(c) = db::open(&paths::db_path()) {
        let _ = db::set_meta(&c, key, value);
    }
}

fn check_enabled() -> bool {
    meta("update.check").as_deref() != Some("0")
}

pub fn state(app: &AppHandle) -> UpdateState {
    let current = app.package_info().version.to_string();
    // 이 기능 이전(0.1.x)부터 쓰던 사람은 기록이 없다 — 데이터가 앱을 켜기 전부터 있었으면(설치 10분 넘음) 0.1.0 에서 올라온 것으로 본다
    let seen = meta("update.seen").or_else(|| {
        let old = meta("installed_at")
            .and_then(|t| time::parse(&t))
            .is_some_and(|t| chrono::Utc::now() - t > chrono::Duration::minutes(10));
        old.then(|| "0.1.0".to_string())
    });
    let whats_new = match &seen {
        // 처음 설치 — 바뀐 점을 보여 줄 게 없다
        None => {
            set_meta("update.seen", &current);
            None
        }
        Some(s) if newer(&current, s) => notes_for(&current),
        _ => None,
    };
    let sh = app.state::<Shared>();
    let skipped = meta("update.skipped");
    let available = sh.available.lock().ok().and_then(|a| a.clone()).filter(|a| skipped.as_deref() != Some(a.version.as_str()));
    let installed = sh.installed.lock().ok().and_then(|a| a.clone());
    UpdateState {
        current,
        whats_new,
        check: check_enabled(),
        skipped,
        available,
        installed,
        last_check: meta("update.last_check"),
        last_error: meta("update.last_error"),
    }
}

/// "바뀐 점"을 봤다
pub fn ack(app: &AppHandle) {
    set_meta("update.seen", &app.package_info().version.to_string());
}

pub fn set_check(on: bool) {
    set_meta("update.check", if on { "1" } else { "0" });
}

pub fn skip(version: &str) {
    if Version::parse(version.trim_start_matches('v')).is_ok() {
        set_meta("update.skipped", version);
    }
}

/// GitHub 릴리스에 새 버전이 있나 — 있으면 기억하고 화면에 알린다
pub async fn check(app: &AppHandle) -> Result<Option<Available>, String> {
    let result = async {
        let updater = app.updater().map_err(|e| e.to_string())?;
        updater.check().await.map_err(|e| e.to_string())
    }
    .await;
    set_meta("update.last_check", &time::now_iso());
    let found = match result {
        Ok(u) => {
            set_meta("update.last_error", "");
            u.map(|u| Available { version: u.version.clone(), notes: u.body.clone(), date: u.date.map(|d| d.to_string()) })
        }
        Err(e) => {
            let e = if e.contains("valid release JSON") {
                "받을 수 있는 공개 릴리스가 아직 없습니다".to_string()
            } else {
                crate::text::clip(&e, 200)
            };
            set_meta("update.last_error", &e);
            return Err(e);
        }
    };
    let sh = app.state::<Shared>();
    if let Ok(mut a) = sh.available.lock() {
        *a = found.clone();
    }
    if let Some(av) = &found {
        if meta("update.skipped").as_deref() != Some(av.version.as_str()) {
            let _ = app.emit("update-available", av);
        }
    }
    Ok(found)
}

/// 받아서 설치하고 다시 시작한다(서명이 공개키와 맞지 않으면 설치하지 않는다)
pub async fn install(app: &AppHandle) -> Result<(), String> {
    let updater = app.updater().map_err(|e| e.to_string())?;
    let update = updater.check().await.map_err(|e| e.to_string())?.ok_or("새 버전이 없습니다")?;
    let a = app.clone();
    let mut done: u64 = 0;
    update
        .download_and_install(
            move |chunk, total| {
                done += chunk as u64;
                let _ = a.emit("update-progress", json!({"done": done, "total": total}));
            },
            || {
                // Windows: 대기 훅이 이 실행 파일을 쥐고 있으면 설치기가 강제로 끝낸다 — 설치 전에 스스로 끝나게(wake::release_all)
                if cfg!(windows) {
                    crate::wake::release_all(Duration::from_secs(3));
                }
            },
        )
        .await
        .map_err(|e| {
            crate::wake::resume();
            e.to_string()
        })?;
    app.restart();
}

/// 디스크에 설치된 앱의 버전(macOS: 번들의 Info.plist). 다른 방법으로 새 버전을 깔았으면 여기가 앞선다
fn disk_version() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let exe = std::env::current_exe().ok()?;
    let plist = exe.parent()?.parent()?.join("Info.plist");
    let s = std::fs::read_to_string(plist).ok()?;
    let re = regex::Regex::new(r"<key>CFBundleShortVersionString</key>\s*<string>([^<]+)</string>").ok()?;
    re.captures(&s).map(|c| c[1].trim().to_string())
}

pub fn spawn(app: AppHandle) {
    std::thread::Builder::new()
        .name("update".into())
        .spawn(move || {
            let current = app.package_info().version.to_string();
            let mut next_check = Instant::now() + Duration::from_secs(30);
            let mut told_installed: Option<String> = None;
            loop {
                if let Some(v) = disk_version().filter(|v| newer(v, &current)) {
                    if told_installed.as_deref() != Some(v.as_str()) {
                        if let Ok(mut i) = app.state::<Shared>().installed.lock() {
                            *i = Some(v.clone());
                        }
                        let _ = app.emit("update-installed", &v);
                        told_installed = Some(v);
                    }
                }
                if Instant::now() >= next_check {
                    next_check = Instant::now() + CHECK_EVERY;
                    if check_enabled() {
                        let _ = tauri::async_runtime::block_on(check(&app));
                    }
                }
                std::thread::sleep(Duration::from_secs(60));
            }
        })
        .expect("update thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_notes() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("v1.0.0", "0.9.9"));
        assert!(!newer("0.2.0", "0.2.0"));
        assert!(!newer("x", "0.1.0"));
        let n = notes_for("0.2.0").expect("CHANGELOG 에 0.2.0 절");
        assert!(n.contains("실행 중인 세션"));
        assert!(!n.contains("## 0.1.0"));
        assert_eq!(notes_for("9.9.9"), None);
    }
}
