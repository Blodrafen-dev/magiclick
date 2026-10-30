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
