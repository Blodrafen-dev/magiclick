//! Runtime state shared with the bar widget through two tiny files.
//!
//! A file instead of a socket on purpose: the widget can read it with a plain
//! `FileView`, there is no protocol to version, and a stale file is harmless.

use std::path::PathBuf;

pub fn dir() -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir());
    base.join("magiclick")
}

pub fn state_path() -> PathBuf {
    dir().join("state")
}

pub fn control_path() -> PathBuf {
    dir().join("control")
}

fn ensure_dir() {
    let _ = std::fs::create_dir_all(dir());
}

/// "off" | "idle" | "armed"
pub fn set_state(s: &str) {
    ensure_dir();
    let _ = std::fs::write(state_path(), s);
}

pub fn state() -> String {
    std::fs::read_to_string(state_path())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "off".into())
}

/// "on" | "off". Written by `magiclick toggle`, read by the daemon.
pub fn set_control(v: &str) {
    ensure_dir();
    let _ = std::fs::write(control_path(), v);
}

pub fn control() -> Option<String> {
    std::fs::read_to_string(control_path())
        .ok()
        .map(|s| s.trim().to_string())
}
