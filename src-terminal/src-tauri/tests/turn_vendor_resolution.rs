const TAURI_MANIFEST: &str = include_str!("../Cargo.toml");
const TAURI_LOCK: &str = include_str!("../Cargo.lock");

#[test]
fn standalone_terminal_uses_the_corrected_vendored_rtc_turn() {
    assert!(
        TAURI_MANIFEST.contains("rtc-turn = { path = \"../../vendor/rtc-turn-0.21.1\" }"),
        "the standalone Tauri workspace must patch rtc-turn to the corrected vendor crate"
    );

    let rtc_turn = TAURI_LOCK
        .split("[[package]]")
        .find(|package| package.lines().any(|line| line.trim() == "name = \"rtc-turn\""))
        .expect("Terminal Cargo.lock must contain rtc-turn");

    assert!(
        rtc_turn.lines().any(|line| line.trim() == "version = \"0.21.1\""),
        "the vendored crate version must remain 0.21.1"
    );
    assert!(
        !rtc_turn.lines().any(|line| line.trim().starts_with("source = ")),
        "Terminal must not resolve the unpatched crates.io rtc-turn"
    );
}
