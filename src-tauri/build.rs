fn main() {
    // 빌드 시각(이 PC 의 시각) — 같은 버전 번호로 여러 번 빌드하는 내부 테스트를 구별하려고 화면에 보인다.
    // 소스·화면 묶음이 바뀔 때만 이 스크립트가 다시 돈다(그때 시각이 새로 찍힌다).
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../dist");
    println!("cargo:rerun-if-changed=../CHANGELOG.md");
    println!("cargo:rerun-if-changed=tauri.conf.json");
    println!("cargo:rerun-if-changed=Cargo.toml");
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    println!("cargo:rustc-env=AI_INBOX_BUILD_AT={now}");
    tauri_build::build()
}
