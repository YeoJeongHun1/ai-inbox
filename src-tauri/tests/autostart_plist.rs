//! 설정의 "로그인할 때 자동 시작" 토글(tauri-plugin-autostart, `MacosLauncher::LaunchAgent`)이 쓰는 라이브러리가
//! 실제로 `~/Library/LaunchAgents/<앱 이름>.plist`(RunAtLoad) 를 만들고 지우는지 — **가짜 HOME** 에서 확인한다(진짜 로그인 항목은 건드리지 않는다).
//! 플러그인의 설정과 같은 값: 앱 이름 = 패키지 이름(`ai-inbox`), 실행 파일 경로 = 현재 실행 파일, LaunchAgent 방식.

#![cfg(target_os = "macos")]

use auto_launch::AutoLaunchBuilder;

#[test]
fn enabling_autostart_writes_a_launch_agent_plist_with_run_at_load_and_disabling_removes_it() {
    let home = std::env::temp_dir().join(format!("aiinbox-fakehome-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("Library")).unwrap(); // 진짜 홈에는 ~/Library 가 있다(라이브러리는 그 밑 LaunchAgents 만 만든다)
    std::env::set_var("HOME", &home);

    let exe = env!("CARGO_BIN_EXE_ai-inbox"); // 플러그인은 current_exe() 를 쓴다 — 여기서는 방금 빌드한 실행 파일
    let al = AutoLaunchBuilder::new().set_app_name("ai-inbox").set_app_path(exe).set_use_launch_agent(true).build().unwrap();
    assert!(!al.is_enabled().unwrap());
    al.enable().unwrap();
    assert!(al.is_enabled().unwrap());

    let plist = home.join("Library/LaunchAgents/ai-inbox.plist");
    let body = std::fs::read_to_string(&plist).expect("plist 가 만들어져야 한다");
    assert!(body.contains("RunAtLoad") && body.contains("<true/>"), "{body}");
    assert!(body.contains(exe), "{body}");
    assert!(body.contains("<key>Label</key>") && body.contains("ai-inbox"), "{body}");

    al.disable().unwrap();
    assert!(!al.is_enabled().unwrap() && !plist.exists());
    let _ = std::fs::remove_dir_all(&home);
}
