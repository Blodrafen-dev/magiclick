//! `magiclick doctor` — install-time verification.
//!
//! Prints what actually works on *this* machine instead of assuming. Every
//! "unsupported" here is a documented limitation, not a crash.

use anyhow::Result;
use std::process::Command;

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Full,
    CopyOnly,
    Unsupported,
}

pub struct Report {
    pub session: String,
    pub clipboard_tools: Vec<&'static str>,
    #[allow(dead_code)]
    pub clipboard_tool_ok: bool,
    pub primary_works: bool,
    pub injector_ok: bool,
    pub input_group: bool,
    pub pointer_devices: usize,
    pub keyboard_devices: usize,
    pub copy_on_drag: bool,
    pub paste_router: bool,
    pub verdict: Verdict,
    pub notes: Vec<String>,
}

/// How many processes match a `pgrep -f` pattern. Used to spot rivals.
fn proc_count(pattern: &str) -> usize {
    Command::new("pgrep")
        .arg("-f")
        .arg("-c")
        .arg(pattern)
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0)
}

fn which(bin: &str) -> bool {    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {bin} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn in_group(group: &str) -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Groups:"))
                .map(|l| l[7..].to_string())
        })
        .map(|ids| {
            ids.split_whitespace().any(|id| {
                let id = id.parse::<u32>().unwrap_or(0);
                std::fs::read_to_string("/etc/group")
                    .map(|g| {
                        g.lines().any(|line| {
                            let f: Vec<&str> = line.split(':').collect();
                            f.len() > 2 && f[0] == group && f[2] == id.to_string()
                        })
                    })
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

pub fn report() -> Result<Report> {
    let session = if env("WAYLAND_DISPLAY").is_some() {
        "wayland".to_string()
    } else if env("DISPLAY").is_some() {
        "x11".to_string()
    } else {
        "tty".to_string()
    };

    let mut tools = Vec::new();
    if which("wl-paste") {
        tools.push("wl-paste");
    }
    if which("wl-copy") {
        tools.push("wl-copy");
    }
    if which("xclip") {
        tools.push("xclip");
    }
    let tool_ok = if session == "wayland" {
        tools.contains(&"wl-paste") && tools.contains(&"wl-copy")
    } else {
        tools.contains(&"xclip")
    };

    let clip = crate::clipboard::default_backend();
    let primary_works = clip.primary().is_ok();
    let injector_ok = crate::inject::available();
    let input_group = in_group("input");

    let (pointer_devices, keyboard_devices) = crate::drag::count_devices();

    let mut notes = Vec::new();
    if !input_group {
        notes.push(String::from("not in the `input` group — drag detection will fail (sudo usermod -aG input $USER)"));
    }
    if !injector_ok {
        notes.push(String::from("/dev/uinput not writable — paste router unavailable, copy-on-drag still works"));
    }
    if !tool_ok {
        notes.push(String::from("clipboard tool missing — install wl-clipboard (Wayland) or xclip (X11)"));
    }
    if pointer_devices == 0 {
        notes.push(String::from("no pointing device found"));
    }
    // Compositor adapter: without one, the daemon cannot tell "clicked a text
    // field" from "clicked the bar", and cannot wait for the click to focus a
    // window before typing into it. Both are silent mis-pastes, so say so.
    match crate::compositor::click_target() {
        crate::compositor::Target::Unknown => notes.push(String::from(
            "no compositor adapter (Hyprland only today) — a click on the bar will paste, and a paste can land in the previously focused window",
        )),
        _ => {}
    }
    // Competing selection watchers. Two things writing the clipboard from the
    // selection is how auto-copy tools double-fire and paste each other's text.
    let mut rivals: Vec<&str> = Vec::new();
    for (pattern, label) in [
        ("cliphist", "cliphist"),
        ("copyq", "CopyQ"),
        ("gpaste-client", "GPaste"),
        ("wl-paste --primary --watch", "a primary->clipboard sync"),
    ] {
        if proc_count(pattern) > 0 {
            rivals.push(label);
        }
    }
    if !rivals.is_empty() {
        notes.push(format!(
            "another selection watcher is running ({}) — two auto-copy paths can race or double-copy",
            rivals.join(", ")
        ));
    }
    let instances = proc_count("magiclick run");
    if instances > 1 {
        notes.push(format!(
            "{instances} magiclick daemons are running — pastes will fire twice; stop all but one"
        ));
    }
    let copy_on_drag = tool_ok && input_group && pointer_devices > 0;
    let paste_router = copy_on_drag && injector_ok;

    let verdict = if paste_router {
        Verdict::Full
    } else if copy_on_drag {
        Verdict::CopyOnly
    } else {
        Verdict::Unsupported
    };

    Ok(Report {
        session,
        clipboard_tools: tools,
        clipboard_tool_ok: tool_ok,
        primary_works,
        injector_ok,
        input_group,
        pointer_devices,
        keyboard_devices,
        copy_on_drag,
        paste_router,
        verdict,
        notes,
    })
}

pub fn print(r: &Report) {
    let yn = |b: bool| if b { "yes" } else { "no" };
    println!("magiclick doctor\n");
    println!("  session            {}", r.session);
    println!(
        "  clipboard tools    {}",
        if r.clipboard_tools.is_empty() {
            "none".to_string()
        } else {
            r.clipboard_tools.join(", ")
        }
    );
    println!("  primary readable   {}", yn(r.primary_works));
    println!("  input group        {}", yn(r.input_group));
    println!("  uinput writable    {}", yn(r.injector_ok));
    println!("  pointer devices    {}", r.pointer_devices);
    println!("  keyboard devices   {}", r.keyboard_devices);
    println!();
    println!("  copy-on-drag       {}", yn(r.copy_on_drag));
    println!("  paste router       {}", yn(r.paste_router));
    println!(
        "  verdict            {}",
        match r.verdict {
            Verdict::Full => "full",
            Verdict::CopyOnly => "copy-only",
            Verdict::Unsupported => "unsupported",
        }
    );
    if !r.notes.is_empty() {
        println!("\n  notes");
        for n in &r.notes {
            println!("    - {n}");
        }
    }
}
