//! Runtime state shared with the bar widget through two tiny files.
//!
//! A file instead of a socket on purpose: the widget can read it with a plain
//! `FileView`, there is no protocol to version, and a stale file is harmless.

use std::path::{Path, PathBuf};

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

/// Remove the state file on a clean exit.
///
/// The bar reads an absent file as "nothing is running", which is only honest
/// if a daemon that stops removes it. Left in place, a stopped daemon keeps
/// advertising its last state — the widget says "Auto-copy on" while nothing is
/// watching, which is worse than saying nothing. A `kill -9` still leaves a
/// stale file; nothing short of a heartbeat fixes that.
pub fn clear_state() {
    let _ = std::fs::remove_file(state_path());
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

pub fn arm_scale_path() -> PathBuf {
    dir().join("arm_scale")
}

pub fn timeout_path() -> PathBuf {
    dir().join("timeout")
}

pub fn base_path() -> PathBuf {
    dir().join("arm_base")
}

/// How many times the configured `arm_timeout_ms` the current arm window is.
///
/// Session-scoped on purpose: the runtime dir is cleared at logout, so a reboot
/// or a restart returns to the value in your config rather than to a 4x you set
/// once while debugging and then forgot about.
pub fn arm_scale() -> u8 {
    std::fs::read_to_string(arm_scale_path())
        .ok()
        .and_then(|s| s.trim().parse::<u8>().ok())
        .filter(|n| (1..=4).contains(n))
        .unwrap_or(1)
}

pub fn set_arm_scale(n: u8) {
    ensure_dir();
    let _ = std::fs::write(arm_scale_path(), n.clamp(1, 4).to_string());
}

/// Human form of the current window ("30s", "2x (60s)"), for the bar tooltip.
pub fn set_timeout(human: &str) {
    ensure_dir();
    let _ = std::fs::write(timeout_path(), human);
}

/// The configured `arm_timeout_ms` in milliseconds. Published so the bar menu
/// can put real seconds on each multiple instead of assuming the default.
pub fn set_base(ms: u64) {
    ensure_dir();
    let _ = std::fs::write(base_path(), ms.to_string());
}

pub fn recent_path() -> PathBuf {
    dir().join("recent")
}

/// Trim only when the file has clearly outgrown a terminal scrollback, so the
/// common case is a single append and no read at all.
const RECENT_MAX_BYTES: u64 = 8192;
const RECENT_KEEP_LINES: usize = 80;

/// Record one decision in plain language, for `magiclick log`.
///
/// This exists because the useful question after a miss is "what did it think
/// just happened?" — and the answer should not require `journalctl` and a
/// timestamp hunt. The runtime dir clears at logout, which is the right life:
/// yesterday's decisions are not worth keeping in your config directory.
pub fn note(text: &str) {
    ensure_dir();
    let path = recent_path();
    if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > RECENT_MAX_BYTES {
        trim_recent(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // One line per entry: a tab would break the reader, and a newline
        // would look like two decisions.
        let flat = text.replace(['\n', '\t'], " ");
        let _ = writeln!(f, "{at}\t{flat}");
    }
}

fn trim_recent(path: &Path) {
    let body = std::fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = body.lines().filter(|l| !l.is_empty()).collect();
    let keep = &lines[lines.len().saturating_sub(RECENT_KEEP_LINES)..];
    let mut out = keep.join("\n");
    out.push('\n');
    let _ = std::fs::write(path, out);
}

/// Last `n` decisions as (when, what), oldest first.
pub fn recent(n: usize) -> Vec<(std::time::SystemTime, String)> {
    let body = std::fs::read_to_string(recent_path()).unwrap_or_default();
    let mut all: Vec<(std::time::SystemTime, String)> = body
        .lines()
        .filter_map(|line| {
            let (at, text) = line.split_once('\t')?;
            let secs: u64 = at.trim().parse().ok()?;
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
            Some((when, text.to_string()))
        })
        .collect();
    if all.len() > n {
        all.drain(..all.len() - n);
    }
    all
}
