//! Embed a minimal `Info.plist` into this crate's **test** binaries.
//!
//! macOS only shows a TCC prompt when the responsible executable carries the
//! matching usage-description string. A `cargo test` binary is a bare Mach-O
//! with no bundle and therefore no strings, so the on-device dictation tests
//! would be refused — or the process killed outright — instead of prompting.
//!
//! `-sectcreate __TEXT __info_plist` is the supported way to give a plain
//! executable an `Info.plist`. `rustc-link-arg` reaches this crate's own
//! binaries, examples and test harnesses and nothing that depends on it —
//! the library target is never linked, so the app is unaffected and gets
//! the same two strings from `src-tauri/Info.plist`. This only makes `cargo
//! test -p neo-voice -- --ignored` usable on a real machine.

use std::path::PathBuf;

const INFO_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key>
  <string>com.starkbot.neo.voice-tests</string>
  <key>CFBundleName</key>
  <string>neo-voice tests</string>
  <key>NSMicrophoneUsageDescription</key>
  <string>starkbot-neo listens for your spoken requests while Listen is on. Only detected speech is sent for transcription.</string>
  <key>NSSpeechRecognitionUsageDescription</key>
  <string>starkbot-neo turns what you say into text on this Mac. On-device dictation keeps the audio on your machine.</string>
</dict>
</plist>
"#;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    let path = PathBuf::from(out_dir).join("tests-Info.plist");
    if std::fs::write(&path, INFO_PLIST).is_err() {
        return;
    }
    println!(
        "cargo:rustc-link-arg=-Wl,-sectcreate,__TEXT,__info_plist,{}",
        path.display()
    );
}
