mod clipboard;
mod config;
mod core;
mod doctor;
mod drag;
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
        Cmd::Status => {
            let r = doctor::report()?;
            println!(
                "{{\"copy_on_drag\":{},\"paste_router\":{},\"verdict\":\"{}\"}}",
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

    loop {
        let event = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(input) => match input {
                drag::Input::Drag => {
                    // The compositor publishes the new selection slightly after
                    // the button comes up. Give it a beat.
                    std::thread::sleep(Duration::from_millis(70));
                    match clip.primary() {
                        Ok(Some(text)) if !text.is_empty() => {
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
                            Event::DragCopied(Selection {
                                hash: h,
                                len: text.len(),
                                source_app: source_app(),
                                text,
                            })
                        }
                        _ => continue,
                    }
                }
                drag::Input::Click => Event::Click,
                drag::Input::Confirm => Event::Confirm,
                drag::Input::KeyCode => Event::Interference("typed"),
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

        match router.handle(event) {
            Outcome::None => {}
            Outcome::Armed => {
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
                let text = router.armed_selection().map(|s| s.text.clone());
                if let (Some(inj), Some(_)) = (injector.as_mut(), text.as_ref()) {
                    if let Err(e) = inj.chord(&cfg.paste_chord) {
                        eprintln!("noclickcopy: paste failed: {e}");
                    } else {
                        eprintln!("noclickcopy: pasted via {}", cfg.paste_chord);
                    }
                } else {
                    eprintln!("noclickcopy: confirm observed (no paste configured)");
                }
            }
        }
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
