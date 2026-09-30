mod api;
mod archive;
mod attach;
pub mod channel;
mod codex;
mod conoti;
mod db;
mod deliver;
mod doc;
pub mod hook;
pub mod install;
mod history;
mod llm;
mod ingest;
mod lifecycle;
mod paths;
mod relay;
mod sched;
mod tags;

pub use paths::set_data_dir_override;
mod text;
mod time;
mod update;
pub mod wake;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rusqlite::Connection;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, RunEvent, WindowEvent};
use tauri_plugin_notification::NotificationExt;

pub struct AppState {
    pub conn: Mutex<Connection>,
    pub rescan: Arc<AtomicBool>,
    /// 입력창에서 보냈다 — 전달 스레드가 기다리지 말고 바로 돈다
    pub kick: Arc<AtomicBool>,
}

const TICK: Duration = Duration::from_millis(1500);

#[derive(Clone, serde::Serialize)]
struct ChangedPayload {
    sessions: Vec<String>,
    counts: api::Counts,
    working: bool,
}

/// 화면·폰 밖에서(기록 창 등) 세션을 보관·삭제한 뒤 — 목록·배지·폰에 알린다
pub(crate) fn sessions_changed(app: &AppHandle, sessions: Vec<String>) {
    app.state::<Arc<relay::Shared>>().bump_changed();
    let counts = {
        let state = app.state::<AppState>();
        let c = state.conn.lock().map(|c| api::counts_of(&c)).unwrap_or_default();
        c
    };
    apply_badges(app, &counts);
    let _ = app.emit("inbox-changed", ChangedPayload { sessions, counts, working: false });
}

fn show_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

/// 트레이 제목(macOS 메뉴 막대 숫자)·툴팁·Dock 배지를 안 읽음 수로 맞춘다.
pub fn refresh_badges(app: &AppHandle) {
    let counts = {
        let state = app.state::<AppState>();
        let Ok(conn) = state.conn.lock() else { return };
        api::counts_of(&conn)
    };
    apply_badges(app, &counts);
    let _ = app.emit("inbox-counts", counts);
}

fn apply_badges(app: &AppHandle, c: &api::Counts) {
    let attention = c.attention;
    let unread = c.unread;
    if let Some(tray) = app.tray_by_id("main") {
        let title = if attention > 0 {
            format!("❓{attention}")
        } else if unread > 0 {
            format!("{unread}")
        } else {
            String::new()
        };
        #[cfg(target_os = "macos")]
        let _ = tray.set_title(if title.is_empty() { None } else { Some(title.as_str()) });
        let _ = tray.set_tooltip(Some(format!(
            "AI Inbox — 안 읽음 {unread} · 확인 필요 {attention} · 진행 중 {}",
            c.active
        )));
    }
    #[cfg(target_os = "macos")]
    if let Some(w) = app.get_webview_window("main") {
        let n = unread + attention;
        let _ = w.set_badge_count(if n > 0 { Some(n) } else { None });
    }
}

fn notify(app: &AppHandle, f: &ingest::Finished) {
    let state = app.state::<AppState>();
    let (enabled, min_sec, name, duration_ms) = {
        let Ok(conn) = state.conn.lock() else { return };
        let enabled = db::setting_i64(&conn, "notify", 1) != 0;
        let min_sec = db::setting_i64(&conn, "notify_min_sec", 0);
        let (name, dur): (String, i64) = conn
            .query_row(
                "SELECT COALESCE(s.live_name, s.title, s.agent_name,
                                 substr(t.prompt_text, 1, 24), '세션'), COALESCE(t.duration_ms, 0)
                   FROM turn t JOIN session s ON s.id = t.session_id WHERE t.id = ?1",
                [f.turn_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or(("세션".into(), 0));
        (enabled, min_sec, name, dur)
    };
    if !enabled || (f.status != "waiting" && duration_ms < min_sec * 1000) {
        return;
    }
    // 앱을 보고 있으면 굳이 OS 알림을 띄우지 않는다
    let focused = app
        .get_webview_window("main")
        .and_then(|w| Some(w.is_focused().ok()? && w.is_visible().ok()?))
        .unwrap_or(false);
    if focused {
        return;
    }
    let icon = match (f.status.as_str(), f.needs_input) {
        ("waiting", _) => "⏸",
        (_, true) => "❓",
        ("interrupted", _) | ("stopped", _) => "⏹",
        _ => "✅",
    };
    let label = match (f.status.as_str(), f.needs_input) {
        ("waiting", _) => "승인 대기",
        (_, true) => "답이 필요해요",
        ("interrupted", _) => "중단됨",
        ("stopped", _) => "멈춤",
        _ => "완료",
    };
    // 잠금 화면에 내용이 보이는 게 싫으면 설정에서 끌 수 있다
    let body = {
        let Ok(conn) = state.conn.lock() else { return };
        if db::setting_i64(&conn, "notify_body", 1) != 0 {
            f.text.clone()
        } else {
            "내용은 AI Inbox 에서 확인하세요".to_string()
        }
    };
    let _ = app
        .notification()
        .builder()
        .title(format!("{icon} {name} · {label}"))
        .body(body)
        .show();
}

/// /clear 로 끝난 세션을 알린다 — 화면에 결정 안내(이력으로 보관 / 삭제 예약). 이름·내용은 실어 보내지 않고 개수만 알리고, 화면이 목록에서 읽는다.
fn announce_cleared(app: &AppHandle, ids: &[String]) {
    let state = app.state::<AppState>();
    let (default, shown) = {
        let Ok(conn) = state.conn.lock() else { return };
        let shown = ids
            .iter()
            .filter(|id| {
                conn.query_row(
                    "SELECT COUNT(*) FROM turn WHERE session_id = ?1 AND hidden = 0",
                    [id.as_str()],
                    |r| r.get::<_, i64>(0),
                )
                .map(|n| n > 0)
                .unwrap_or(false)
            })
            .count();
        (lifecycle::default_state(&conn), shown)
    };
    if shown == 0 {
        return;
    }
    let _ = app.emit("clear-detected", serde_json::json!({ "count": shown, "default": default }));
}

fn spawn_ingest(app: AppHandle, rescan: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("ingest".into())
        .spawn(move || {
            let open = || {
                let c = db::open(&paths::db_path()).expect("db open");
                db::migrate(&c).expect("db migrate");
                c
            };
            let mut ing = ingest::Ingestor::new(open());
            let mut was_working = false;
            loop {
                if rescan.swap(false, Ordering::SeqCst) {
                    let c = open();
                    let _ = c.execute("DELETE FROM source_file", []);
                    ing = ingest::Ingestor::new(c);
                }
                let started = Instant::now();
                let rep = ing.tick();
                if !rep.changed.is_empty() {
                    app.state::<Arc<relay::Shared>>().bump_changed();
                }
                if !rep.finished.is_empty() {
                    app.state::<Arc<relay::Shared>>().bump_finished();
                }
                // 화면에 알릴 것은 바뀐 세션이 있거나 첫 백필이 진행·끝났을 때만.
                // 파일을 읽기만 하고 바뀐 게 없을 때(작업 중인 세션의 기록이 조금씩 늘 때 흔하다)까지 알리면
                // 화면이 1.5초마다 목록·대화·문서를 통째로 다시 받았다.
                let backfill_edge = rep.working || was_working;
                was_working = rep.working;
                if !rep.changed.is_empty() || backfill_edge {
                    let counts = {
                        let state = app.state::<AppState>();
                        let c = state.conn.lock().map(|c| api::counts_of(&c)).unwrap_or_default();
                        c
                    };
                    apply_badges(&app, &counts);
                    let _ = app.emit(
                        "inbox-changed",
                        ChangedPayload { sessions: rep.changed.iter().cloned().collect(), counts, working: rep.working },
                    );
                }
                if rep.tags_changed {
                    let _ = app.emit("tags-changed", ());
                }
                if !rep.cleared.is_empty() {
                    announce_cleared(&app, &rep.cleared);
                }
                for f in &rep.finished {
                    notify(&app, f);
                }
                let spent = started.elapsed();
                if spent < TICK {
                    std::thread::sleep(TICK - spent);
                }
            }
        })
        .expect("ingest thread");
}

/// 전달 스레드: 입력창·폰에서 보낸 말을 세션에 넣고, 그 결과가 끝나면 처리됨으로 표시한다.
fn spawn_phone(app: AppHandle, kick: Arc<AtomicBool>) {
    std::thread::Builder::new()
        .name("phone".into())
        .spawn(move || {
            let conn = match db::open(&paths::db_path()) {
                Ok(c) => c,
                Err(_) => return,
            };
            conoti::cleanup_legacy(&conn);
            let mut pipe = conoti::Pipeline::new(conn);
            let mut last_confirm = 0usize;
            loop {
                // 예약 전송: 시각이 된 예약을 대기열에 넣는다(같은 틱에 아래 파이프라인이 전달)
                let srep = pipe.tick_schedule(chrono::Utc::now(), &sched::LiveProbe);
                if !srep.alerts.is_empty() {
                    for a in &srep.alerts {
                        let _ = app.notification().builder().title(format!("⏰ {}", a.title())).body(format!("{} — {}", a.session_name, a.note)).show();
                    }
                    // 폰에는 내용 없는 알림만(푸시) — 연결돼 있으면 changed 로 화면이 새로 불러온다
                    app.state::<Arc<relay::Shared>>().bump_sched_alert(srep.alerts.iter().map(|a| a.push_kind()).collect());
                }
                if !srep.alerts.is_empty() || !srep.changed_sessions.is_empty() {
                    let _ = app.emit("sched-changed", ());
                }
                let deliverer = deliver::LocalDeliver { bg_resume: deliver::bg_resume_enabled() };
                let mut rep = pipe.tick(&deliverer);
                rep.changed_sessions.extend(srep.changed_sessions);
                if !rep.changed_sessions.is_empty() || rep.needs_confirm != last_confirm {
                    if rep.needs_confirm > last_confirm {
                        let _ = app
                            .notification()
                            .builder()
                            .title("📱 폰에서 답이 왔습니다")
                            .body("데스크톱에서 확인한 뒤 세션에 전달됩니다")
                            .show();
                    }
                    last_confirm = rep.needs_confirm;
                    app.state::<Arc<relay::Shared>>().bump_changed();
                    emit_changed(&app, rep.changed_sessions);
                }
                for _ in 0..10 {
                    if kick.swap(false, Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        })
        .expect("phone thread");
}

fn emit_changed(app: &AppHandle, sessions: Vec<String>) {
    let counts = {
        let state = app.state::<AppState>();
        let c = state.conn.lock().map(|c| api::counts_of(&c)).unwrap_or_default();
        c
    };
    apply_badges(app, &counts);
    let _ = app.emit("inbox-changed", ChangedPayload { sessions, counts, working: false });
}

/// 폰 연결 스레드(켜야만 네트워크를 쓴다)
fn spawn_relay(app: AppHandle) {
    let shared = app.state::<Arc<relay::Shared>>().inner().clone();
    let a1 = app.clone();
    let a2 = app.clone();
    let hooks = relay::client::Hooks {
        on_pair_request: Box::new(move |name: &str, sas: &str| {
            let _ = a1.emit("relay-pair", serde_json::json!({"name": name, "sas": sas}));
            let _ = a1.notification().builder().title("📱 새 기기 연결 요청").body(format!("{name} — AI Inbox 에서 허용하거나 거절하세요")).show();
            show_main(&a1);
        }),
        on_local_change: Box::new(move |sessions: Vec<String>| emit_changed(&a2, sessions)),
    };
    relay::client::spawn(shared, paths::db_path(), hooks, app.package_info().version.to_string());
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "AI Inbox 열기", true, None::<&str>)?;
    let read_all = MenuItem::with_id(app, "read_all", "모두 읽음으로 표시", true, None::<&str>)?;
    let phone = MenuItem::with_id(app, "phone", "폰 연결 (QR)…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "종료", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &phone, &read_all, &PredefinedMenuItem::separator(app)?, &quit])?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/tray.png"))?;
    TrayIconBuilder::with_id("main")
        .icon(icon)
        .icon_as_template(true)
        .tooltip("AI Inbox")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, ev| match ev.id().as_ref() {
            "open" => show_main(app),
            "phone" => {
                show_main(app);
                let _ = app.emit("open-phone", ());
            }
            "read_all" => {
                let state = app.state::<AppState>();
                if let Ok(conn) = state.conn.lock() {
                    let _ = api::mark_all_read_on(&conn, None);
                }
                app.state::<Arc<relay::Shared>>().bump_changed();
                refresh_badges(app);
                let _ = app.emit("inbox-changed", ChangedPayload { sessions: vec![], counts: api::Counts::default(), working: false });
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, ev| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = ev {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_app_url;

    #[test]
    fn app_origin_only() {
        let ok = |u: &str| is_app_url(&tauri::Url::parse(u).unwrap());
        assert!(ok("tauri://localhost/index.html"));
        assert!(ok("http://tauri.localhost/"));
        assert!(!ok("http://tauri.localhost:8080/"));
        assert!(!ok("https://example.com/"));
        assert!(!ok("file:///etc/passwd"));
        assert!(!ok("tauri://evil/"));
        assert!(!ok("http://127.0.0.1:1420/"));
    }
}

/// `ai-inbox ingest-once` — 창 없이 수집만 끝까지 돌리고 요약을 찍는다(검증용).
pub fn ingest_once() {
    let conn = db::open(&paths::db_path()).expect("db");
    db::migrate(&conn).expect("migrate");
    let mut ing = ingest::Ingestor::new(conn);
    let t = Instant::now();
    let mut ticks = 0;
    loop {
        ticks += 1;
        let rep = ing.tick();
        if (!rep.working && rep.processed == 0) || ticks > 200 {
            break;
        }
    }
    let conn = db::open(&paths::db_path()).expect("db");
    let q = |sql: &str| conn.query_row(sql, [], |r| r.get::<_, i64>(0)).unwrap_or(-1);
    println!(
        "ticks={ticks} elapsed={:.2}s sessions={} turns={} visible={} steps={} unread={} running={}",
        t.elapsed().as_secs_f64(),
        q("SELECT COUNT(*) FROM session"),
        q("SELECT COUNT(*) FROM turn"),
        q("SELECT COUNT(*) FROM turn WHERE hidden = 0"),
        q("SELECT COUNT(*) FROM turn_step"),
        q("SELECT COUNT(*) FROM turn WHERE hidden = 0 AND read_at IS NULL AND status IN ('done','interrupted','stopped')"),
        q("SELECT COUNT(*) FROM turn WHERE hidden = 0 AND status IN ('running','background','waiting')"),
    );
}

/// 앱 자신의 페이지만 띄운다. 대화 기록 속 링크 등으로 창이 바깥 주소로 넘어가지 않게.
/// origin 을 포트까지 정확히 비교한다: macOS/Linux `tauri://localhost`, Windows `http(s)://tauri.localhost`,
/// 개발 빌드만 Vite 서버 `http://localhost:1420`.
fn is_app_url(url: &tauri::Url) -> bool {
    let host = url.host_str();
    let port = url.port();
    match url.scheme() {
        "tauri" => host == Some("localhost") && port.is_none(),
        "http" | "https" if host == Some("tauri.localhost") => port.is_none(),
        "http" if cfg!(debug_assertions) => host == Some("localhost") && port == Some(1420),
        _ => false,
    }
}

fn build_main_window(app: &AppHandle) -> tauri::Result<()> {
    let cfg = app
        .config()
        .app
        .windows
        .iter()
        .find(|w| w.label == "main")
        .cloned()
        .expect("tauri.conf.json 에 main 창 설정이 없습니다");
    tauri::WebviewWindowBuilder::from_config(app, &cfg)?
        .on_navigation(|url| is_app_url(url))
        .build()?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let conn = db::open(&paths::db_path()).expect("DB 를 열 수 없습니다");
    db::migrate(&conn).expect("DB 스키마 적용 실패");
    let rescan = Arc::new(AtomicBool::new(false));
    let kick = Arc::new(AtomicBool::new(false));

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main(app)))
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, None))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .manage(update::Shared::default())
        .manage(AppState { conn: Mutex::new(conn), rescan: rescan.clone(), kick: kick.clone() })
        .manage(relay::Shared::new())
        .invoke_handler(tauri::generate_handler![
            api::list_sessions,
            api::get_chat,
            api::get_turn,
            api::mark_read,
            api::mark_unread,
            api::mark_session_read,
            api::restore_unread,
            api::session_outline,
            api::set_starred,
            api::set_pinned,
            api::set_hidden,
            api::get_counts,
            api::hook_status,
            api::install_hooks,
            api::uninstall_hooks,
            api::app_info,
            api::about,
            api::set_setting,
            api::rescan,
            api::save_markdown,
            api::reveal_data_dir,
            api::relay_status,
            api::relay_set,
            api::relay_new_offer,
            api::relay_cancel_offer,
            api::relay_decide_pair,
            api::relay_remove_device,
            api::relay_set_device_reply,
            api::relay_set_device_manage,
            api::relay_set_device_schedule,
            api::conoti_set_session_mode,
            api::conoti_decide,
            api::conoti_pending,
            api::send_message,
            api::session_outbox,
            api::cancel_message,
            api::recent_dirs,
            api::pick_folder,
            api::start_task,
            api::codex_status,
            api::attachment_put,
            api::attachment_get,
            api::attachment_meta,
            api::attachment_reveal,
            api::archive_turns,
            api::archive_messages,
            api::archive_images,
            api::archive_stats,
            api::archive_delete_messages,
            api::archive_delete_images,
            api::archive_sessions,
            api::archive_set_archived,
            api::archive_tidy,
            api::archive_delete_sessions,
            api::clear_decide,
            api::clear_list,
            api::clear_ack,
            api::clear_overview,
            api::history_status,
            api::history_set,
            api::history_detect,
            sched::sched_add,
            sched::sched_list,
            sched::sched_counts,
            sched::sched_update,
            sched::sched_cancel,
            sched::sched_act,
            sched::sched_settings,
            sched::sched_set_setting,
            sched::sched_allow_set,
            sched::sched_perm_info,
            sched::sched_warn_off,
            sched::sched_window_save,
            sched::sched_window_delete,
            sched::sched_rule_set,
            api::history_forget_key,
            api::history_ask,
            api::get_chat_tagged,
            tags::tag_overview,
            tags::tag_session,
            tags::tag_create,
            tags::tag_update,
            tags::tag_merge,
            tags::tag_delete,
            tags::tag_rule_add,
            tags::tag_rule_remove,
            tags::tag_reset_defaults,
            tags::turn_tag_set,
            tags::tag_suggest_folders,
            tags::tag_suggest_for_turn,
            tags::tag_context,
            tags::tag_ai_status,
            tags::tag_ai_set,
            tags::tag_ai_suggest,
            tags::tag_ai_pending,
            tags::tag_ai_decide,
            tags::tag_ai_decide_all,
            api::update_state,
            api::update_ack,
            api::update_set_check,
            api::update_skip,
            api::update_check_now,
            api::update_install,
            api::app_restart,
        ])
        .setup(move |app| {
            // 업데이트 직전에 멈춰 둔 대기 훅을 다시 받는다(wake::release_all)
            wake::resume();
            build_main_window(app.handle())?;
            build_tray(app.handle())?;
            spawn_ingest(app.handle().clone(), rescan.clone());
            spawn_phone(app.handle().clone(), kick.clone());
            // 훅을 이미 설치한 사용자가 0.3.0 으로 올라오면 **한 번만** 첨부 이미지 폴더 읽기 허용을 더한다(바뀐 점에 적어 둠).
            // 사용자가 그 뒤 규칙을 지웠으면 다시 넣지 않는다 — 그다음은 설정의 "다시 설치" 로만.
            std::thread::spawn(|| {
                let Ok(conn) = db::open(&paths::db_path()) else { return };
                if db::get_meta(&conn, "install.read_rule_once").is_some() {
                    return;
                }
                if install::ensure_read_rule().is_ok() {
                    let _ = db::set_meta(&conn, "install.read_rule_once", &time::now_iso());
                }
            });
            spawn_relay(app.handle().clone());
            update::spawn(app.handle().clone());
            refresh_badges(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| {
            // 창을 닫아도 트레이에 남아 계속 모은다
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|app, event| {
        #[cfg(target_os = "macos")]
        if let RunEvent::Reopen { .. } = event {
            show_main(app);
        }
        let _ = (app, &event);
    });
}
