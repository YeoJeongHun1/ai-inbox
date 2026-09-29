// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // `ai-inbox hook` — Claude Code 훅이 부른다. Tauri 를 띄우지 않고 바로 끝난다.
    match std::env::args().nth(1).as_deref() {
        Some("hook") => ai_inbox_lib::hook::run(),
        // Claude Code 가 띄우는 채널(MCP stdio) 서버 — 폰 답을 실행 중인 세션에 넣는다
        Some("channel") => ai_inbox_lib::channel::run(),
        // 실행 중인 세션에 말을 넣는 대기 훅(asyncRewake) — 종료 코드 2 가 세션을 깨운다
        Some("wake") => std::process::exit(ai_inbox_lib::wake::run()),
        Some("install-hooks") => {
            let r = ai_inbox_lib::install::install();
            let ok = r.is_ok();
            print_status(r);
            if ok {
                // 쓴 직후의 변경은 새 훅을 부르지 않는다 — 한 번 더(수정 시각만) 알려 실행 중인 세션들이 대기 훅을 띄우게
                std::thread::sleep(std::time::Duration::from_secs(2));
                ai_inbox_lib::install::rearm();
            }
        }
        Some("uninstall-hooks") => print_status(ai_inbox_lib::install::uninstall()),
        Some("hook-status") => print_status(ai_inbox_lib::install::status()),
        Some("ingest-once") => {
            // 진단용: AI_INBOX_DATA_DIR 로 실제 데이터 폴더를 건드리지 않고 돌릴 수 있다
            if let Ok(dir) = std::env::var("AI_INBOX_DATA_DIR") {
                if !dir.trim().is_empty() {
                    ai_inbox_lib::set_data_dir_override(dir.into());
                }
            }
            ai_inbox_lib::ingest_once()
        }
        _ => ai_inbox_lib::run(),
    }
}

fn print_status(r: Result<ai_inbox_lib::install::HookStatus, String>) {
    match r {
        Ok(s) => println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default()),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
