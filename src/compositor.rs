//! "Did that click land in a window, or on the bar?"
//!
//! A paste is destructive when it lands in the wrong place. Clicking the bar or
//! an empty desktop is never a text field — and it is exactly the gesture
//! people make while looking for one. So a click outside every window is a
//! cancel, not a confirm.
//!
//! Hyprland answers this with `hyprctl`; everywhere else the answer is
//! "unknown" and the daemon keeps its old, permissive behaviour.

use std::process::Command;

#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    /// The pointer is over a toplevel window.
    Window,
    /// Bar, desktop, layer surface — not a text field anywhere.
    NoWindow,
    /// No compositor adapter: do not second-guess the user.
    Unknown,
}

fn hyprctl(args: &[&str]) -> Option<String> {
    let out = Command::new("hyprctl").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn pair(s: &str) -> Option<(i32, i32)> {
    let (a, b) = s.split_once(',')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn cursor() -> Option<(i32, i32)> {
    pair(hyprctl(&["cursorpos"])?.trim())
}

/// Every mapped toplevel, as (x, y, w, h) in layout coordinates.
fn windows() -> Option<Vec<(i32, i32, i32, i32)>> {
    let text = hyprctl(&["clients"])?;
    let mut out = Vec::new();
    let mut at: Option<(i32, i32)> = None;
    let mut size: Option<(i32, i32)> = None;
    let mut mapped = true;

    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("Window ") {
            at = None;
            size = None;
            mapped = true;
        } else if let Some(v) = t.strip_prefix("at:") {
            at = pair(v);
        } else if let Some(v) = t.strip_prefix("size:") {
            size = pair(v);
        } else if let Some(v) = t.strip_prefix("mapped:") {
            mapped = v.trim() == "1";
        }
        if let (Some(a), Some(s)) = (at, size) {
            if mapped {
                out.push((a.0, a.1, s.0, s.1));
            }
            at = None;
            size = None;
        }
    }
    Some(out)
}

/// Address of the mapped toplevel under the pointer, normalised (no `0x`).
pub fn window_under_cursor() -> Option<String> {
    let (x, y) = cursor()?;
    let text = hyprctl(&["clients"])?;
    let mut addr: Option<String> = None;
    let mut at: Option<(i32, i32)> = None;
    let mut size: Option<(i32, i32)> = None;
    let mut best: Option<String> = None;

    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Window ") {
            addr = rest.split("->").next().map(|a| a.trim().to_string());
            at = None;
            size = None;
        } else if let Some(v) = t.strip_prefix("at:") {
            at = pair(v);
        } else if let Some(v) = t.strip_prefix("size:") {
            size = pair(v);
        } else if let Some(v) = t.strip_prefix("mapped:") {
            let mapped = v.trim() == "1";
            if let (Some(a), Some(at), Some(s)) = (addr.as_ref(), at, size) {
                if mapped && x >= at.0 && x < at.0 + s.0 && y >= at.1 && y < at.1 + s.1 {
                    best = Some(a.clone());
                }
            }
        }
    }
    best
}

/// Address of the focused toplevel, normalised (no `0x`).
pub fn active_window() -> Option<String> {
    let out = hyprctl(&["-j", "activewindow"])?;
    let key = "\"address\":";
    let rest = out.split(key).nth(1)?;
    let raw = rest.trim_start().strip_prefix('"')?;
    let addr = raw.split('"').next()?;
    Some(addr.trim_start_matches("0x").to_string())
}

/// Block until the window under the pointer is the focused one.
///
/// A click focuses a window *and* places the caret; keyboard injection that
/// leaves the daemon before the compositor has applied the focus lands in the
/// window that was focused a millisecond ago — which is how a paste meant for
/// the browser ends up in the terminal beside it. Returns `false` if the
/// compositor never agreed, so the caller can decide what to do.
pub fn wait_for_focus(budget: std::time::Duration) -> bool {
    let started = std::time::Instant::now();
    let Some(target) = window_under_cursor() else {
        // Not over a window (or no Hyprland): nothing to compare against.
        return true;
    };
    loop {
        if active_window().as_deref() == Some(target.as_str()) {
            // Focus is ours; give the client a moment to actually route keys
            // to the widget that was clicked (Chromium needs a few ms).
            std::thread::sleep(std::time::Duration::from_millis(60));
            return true;
        }
        if started.elapsed() > budget {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

pub fn click_target() -> Target {
    let (Some((x, y)), Some(wins)) = (cursor(), windows()) else {
        return Target::Unknown;
    };
    for (wx, wy, ww, wh) in wins {
        if x >= wx && x < wx + ww && y >= wy && y < wy + wh {
            return Target::Window;
        }
    }
    Target::NoWindow
}
