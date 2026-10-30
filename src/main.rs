mod clipboard;
mod config;
mod core;
mod doctor;
mod drag;
mod runtime;
mod inject;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::{ConfirmAction, Config};
use core::{Event, Outcome, Router, Selection};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "noclickcopy", version, about = "Drag to copy, confirm to paste.")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Watch input and route selections (default).
    Run,
    /// Probe this machine and report what will actually work.
    Doctor,
    /// One-shot capability summary, machine readable.
    Status,
    /// Turn watching on or off (what the bar widget calls).
    Enable,
    /// Turn watching off.
    Disable,
    /// Flip it.
    Toggle,
    /// Print mouse button and key events as they arrive (find your confirm key).
    Watch,
}

fn hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// Best effort: which app were we in? Used only for logging and, later, for
/// learning app pairs. Absent is fine.
fn source_app() -> Option<String> {
    let out = std::process::Command::new("timeout")
        .args(["0.3", "hyprctl", "activewindow", "-j"])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    for line in s.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("\"class\":") {
            let v = rest.trim().trim_matches(',').trim().trim_matches('"');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or(Cmd::Run) {
        Cmd::Doctor => {
            let r = doctor::report()?;
            doctor::print(&r);
            std::process::exit(if r.copy_on_drag { 0 } else { 1 });
        }
        Cmd::Enable => {
            runtime::set_control("on");
            println!("on");
        }
        Cmd::Disable => {
            runtime::set_control("off");
            println!("off");
        }
        Cmd::Watch => watch()?,
        Cmd::Toggle => {
            // Base the flip on the control file, not on observed state: the
            // daemon may not be running, and state then never updates.
            let was_on = runtime::control().map(|c| c != "off").unwrap_or(true);
            let next = was_on;
            runtime::set_control(if next { "off" } else { "on" });
            println!("{}", if next { "off" } else { "on" });
        }
        Cmd::Status => {
            let r = doctor::report()?;
            println!(
                "{{\"state\":\"{}\",\"copy_on_drag\":{},\"paste_router\":{},\"verdict\":\"{}\"}}",
                runtime::state(),
                r.copy_on_drag,
                r.paste_router,
                match r.verdict {
                    doctor::Verdict::Full => "full",
                    doctor::Verdict::CopyOnly => "copy-only",
                    doctor::Verdict::Unsupported => "unsupported",
                }
            );
        }
        Cmd::Run => run()?,
    }
    Ok(())
}

fn run() -> Result<()> {
    let cfg = Config::load()?;
    let confirm_code = confirm_key_code(&cfg.confirm_key)?;

    let (tx, rx) = mpsc::channel::<drag::Input>();
    let n = drag::spawn_readers(confirm_code, cfg.drag_threshold, tx)?;
    if n == 0 {
        anyhow::bail!("no input devices readable — run `noclickcopy doctor`");
    }

    let clip = clipboard::default_backend();
    let mut router = Router::new(Duration::from_millis(cfg.arm_timeout_ms));
    let mut injector = if cfg.confirm_action == ConfirmAction::Paste {
        Some(inject::Injector::new()?)
    } else {
        None
    };

    eprintln!(
        "noclickcopy: {} device(s), confirm={} ({:?}), timeout={}ms",
        n, cfg.confirm_key, cfg.confirm_action, cfg.arm_timeout_ms
    );

    let mut last_clipboard_check = Instant::now();
    let mut owned_hash: Option<u64> = None;
    let mut enabled = runtime::control().map(|c| c != "off").unwrap_or(true);
    let mut published = String::new();
    let mut last_control_check = Instant::now();
    let mut last_arm: Option<Instant> = None;
    let mut primary_before: Option<u64> = None;
    let mut clip_before: Option<u64> = None;

    runtime::set_state(if enabled { "idle" } else { "off" });

    // Keep the bar icon honest: publish only on change.
    macro_rules! publish {
        () => {{
            let next = if !enabled {
                "off"
            } else if router.is_armed() {
                "armed"
            } else {
                "idle"
            };
            if next != published {
                published = next.to_string();
                runtime::set_state(next);
            }
        }};
    }

    loop {
        if last_control_check.elapsed() > Duration::from_millis(250) {
            last_control_check = Instant::now();
            let now_enabled = runtime::control().map(|c| c != "off").unwrap_or(true);
            if now_enabled != enabled {
                enabled = now_enabled;
                if !enabled {
                    router.handle(core::Event::Interference("disabled"));
                    eprintln!("noclickcopy: watching disabled");
                } else {
                    eprintln!("noclickcopy: watching enabled");
                }
                publish!();
            }
        }

        let event = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(input) => match input {
                drag::Input::Press if enabled => {
                    // Snapshot both selections *before* the drag, so a stale
                    // read after release can be told apart from a real one.
                    // Some apps (Chromium-based) publish drags to the clipboard
                    // and never touch the primary selection.
                    primary_before = clip.primary().ok().flatten().map(|t| hash(&t));
                    clip_before = clip.clipboard().ok().flatten().map(|t| hash(&t));
                    continue;
                }
                drag::Input::Drag => {
                    // The new selection is published some time after the button
                    // comes up, and *which* selection it lands in depends on the
                    // app. Poll both until one of them actually changes, instead
                    // of guessing at a fixed delay or a single source.
                    let deadline = Instant::now() + Duration::from_millis(800);
                    let mut picked: Option<(String, &'static str)> = None;
                    let mut last_p: Option<usize> = None;
                    let mut last_c: Option<usize> = None;
                    while Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(60));
                        let p = clip.primary().ok().flatten();
                        if let Some(t) = &p {
                            last_p = Some(t.len());
                        }
                        if let Some(t) = &p {
                            if !t.is_empty() && Some(hash(t)) != primary_before {
                                picked = Some((t.clone(), "primary"));
                                break;
                            }
                        }
                        let c = clip.clipboard().ok().flatten();
                        if let Some(t) = &c {
                            last_c = Some(t.len());
                        }
                        if let Some(t) = &c {
                            if !t.is_empty() && Some(hash(t)) != clip_before {
                                picked = Some((t.clone(), "clipboard"));
                                break;
                            }
                        }
                    }
                    match picked {
                        None => {
                            eprintln!(
                                "noclickcopy: drag saw no new selection (window: {}, primary: {}, clipboard: {})",
                                source_app().unwrap_or_else(|| "?".into()),
                                match last_p {
                                    Some(n) => format!("{n}B unchanged"),
                                    None => "empty".into(),
                                },
                                match last_c {
                                    Some(n) => format!("{n}B unchanged"),
                                    None => "empty".into(),
                                }
                            );
                            continue;
                        }
                        Some((text, from)) => {
                            if text.len() > cfg.max_bytes {
                                eprintln!(
                                    "noclickcopy: selection too large ({} bytes), ignored",
                                    text.len()
                                );
                                continue;
                            }
                            let h = hash(&text);
                            if owned_hash == Some(h) {
                                continue;
                            }
                            if cfg.copy_on_drag {
                                if let Err(e) = clip.set_clipboard(&text) {
                                    eprintln!("noclickcopy: clipboard write failed: {e}");
                                }
                            }
                            owned_hash = Some(h);
                            eprintln!("noclickcopy: copied from {from}");
                            Event::DragCopied(Selection {
                                hash: h,
                                len: text.len(),
                                source_app: source_app(),
                                text,
                            })
                        }
                    }
                }
                drag::Input::Click if enabled => {
                    // Diagnosis: a click that lands after the arm expired looks
                    // identical to "nothing happened" from the outside. Say so.
                    if !router.is_armed() {
                        if let Some(t) = last_arm {
                            let age = t.elapsed();
                            if age < Duration::from_secs(60) {
                                eprintln!(
                                    "noclickcopy: click {:.1}s after arm (arm had expired at {}s)",
                                    age.as_secs_f64(),
                                    cfg.arm_timeout_ms as f64 / 1000.0
                                );
                            }
                        }
                    }
                    Event::Click
                }
                drag::Input::Confirm if enabled => Event::Confirm,
                drag::Input::KeyCode if enabled => Event::Interference("typed"),
                _ => continue,
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // While armed, watch for something else taking the clipboard.
                if router.is_armed() && last_clipboard_check.elapsed() > Duration::from_millis(300)
                {
                    last_clipboard_check = Instant::now();
                    if let Ok(Some(current)) = clip.clipboard() {
                        if Some(hash(&current)) != owned_hash {
                            Event::ClipboardMoved
                        } else {
                            Event::Tick(Instant::now())
                        }
                    } else {
                        Event::Tick(Instant::now())
                    }
                } else {
                    Event::Tick(Instant::now())
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        // Fire disarms before it reports, so hold the selection first.
        let held = router.armed_selection().cloned();
        match router.handle(event) {
            Outcome::None => {}
            Outcome::Armed => {
                last_arm = Some(Instant::now());
                if let Some(sel) = router.armed_selection() {
                    eprintln!(
                        "noclickcopy: armed {} bytes from {}",
                        sel.len,
                        sel.source_app.as_deref().unwrap_or("?")
                    );
                }
            }
            Outcome::Disarmed(reason) => eprintln!("noclickcopy: disarmed ({reason})"),
            Outcome::Fire => {
                if let (Some(inj), Some(_sel)) = (injector.as_mut(), held.as_ref()) {
                    if let Err(e) = inj.chord(&cfg.paste_chord) {
                        eprintln!("noclickcopy: paste failed: {e}");
                    } else {
                        eprintln!("noclickcopy: pasted via {}", cfg.paste_chord);
                    }
                } else {
                    eprintln!(
                        "noclickcopy: confirm observed ({} bytes, no paste configured)",
                        held.as_ref().map(|s| s.len).unwrap_or(0)
                    );
                }
            }
        }
        publish!();
    }

    Ok(())
}

/// Debug aid: press a button, read its name. Held until Ctrl-C.
fn watch() -> Result<()> {
    use std::sync::mpsc::channel;
    let (tx, rx) = channel::<String>();
    let mut seen = 0;
    for (path, mut dev) in evdev::enumerate() {
        let name = dev.name().unwrap_or("?").to_string();
        if name.contains("noclickcopy") {
            continue;
        }
        let Some(keys) = dev.supported_keys() else { continue };
        let interesting = keys.contains(evdev::KeyCode::BTN_LEFT)
            || keys.contains(evdev::KeyCode::KEY_A);
        if !interesting {
            continue;
        }
        seen += 1;
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = &path;
            loop {
                let Ok(events) = dev.fetch_events() else { return };
                for ev in events {
                    if ev.event_type() == evdev::EventType::KEY && ev.value() == 1 {
                        let code = ev.code();
                        let name = evdev::KeyCode::new(code);
                        let _ = tx.send(format!("{name:?}"));
                    }
                }
            }
        });
    }
    if seen == 0 {
        anyhow::bail!("no devices readable — run `noclickcopy doctor`");
    }
    eprintln!("noclickcopy: watching {} device(s). Press the button, then Ctrl-C.", seen);
    for line in rx {
        println!("{line}");
    }
    Ok(())
}

fn confirm_key_code(name: &str) -> Result<u16> {
    use evdev::KeyCode;
    let k = match name {
        "KEY_F13" => KeyCode::KEY_F13,
        "KEY_F14" => KeyCode::KEY_F14,
        "KEY_F15" => KeyCode::KEY_F15,
        "KEY_F16" => KeyCode::KEY_F16,
        "KEY_PAUSE" => KeyCode::KEY_PAUSE,
        "KEY_SCROLLLOCK" => KeyCode::KEY_SCROLLLOCK,
        other => anyhow::bail!("unsupported confirm_key {other}; use KEY_F13..KEY_F16"),
    };
    Ok(k.code())
}
