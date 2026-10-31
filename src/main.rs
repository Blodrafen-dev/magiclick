mod clipboard;
mod compositor;
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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(name = "magiclick", version, about = "Auto-copy a selection, confirm to paste.")]
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
    /// Set the arm window to N x `arm_timeout_ms`, or step it to the next
    /// multiple when no value is given. The bar's right-click menu passes 1-4.
    ArmTimeout {
        /// Multiple of `arm_timeout_ms`: 1, 2, 3 or 4. Omit to cycle.
        scale: Option<u8>,
    },
    /// Print mouse button and key events as they arrive (find your confirm key).
    Watch,
    /// Why didn't it paste? The last few decisions, in plain language.
    Log {
        /// How many decisions to show.
        #[arg(short = 'n', long, default_value_t = 15)]
        n: usize,
    },
    /// Print the project's support link. Opens a browser only with --open.
    Support {
        /// Open the link in your browser.
        #[arg(long)]
        open: bool,
    },
}

/// Where `magiclick support` points.
///
/// One constant and no network code on purpose. This daemon reads every key and
/// pointer event on the machine and injects keystrokes into whatever window is
/// focused; outbound HTTP in that process would be a permanent line in its
/// threat model for a tip jar. So it prints the URL, and the only request ever
/// made is the one your own browser makes, when you ask for it with `--open`.
const SUPPORT_URL: &str = "https://www.buymeacoffee.com/<handle>";

/// "3m ago" reads faster than a wall clock when you are asking why a thing you
/// just did did not happen.
fn ago(secs: u64) -> String {
    match secs {
        0..=9 => "just now".to_string(),
        10..=59 => format!("{secs}s ago"),
        60..=3599 => format!("{}m ago", secs / 60),
        _ => format!("{}h ago", secs / 3600),
    }
}

fn print_log(n: usize) {
    let entries = runtime::recent(n.max(1));
    if entries.is_empty() {
        println!("No decisions recorded this session.");
        println!("Start the daemon, use it, and ask again.");
        return;
    }
    let now = std::time::SystemTime::now();
    for (at, text) in entries {
        let age = now.duration_since(at).unwrap_or_default().as_secs();
        println!("{:>8}  {text}", ago(age));
    }
}

fn hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// "30s" for the configured default, "2x (60s)" once lengthened. Shown in
/// the bar tooltip, where the decision is actually being made.
fn arm_window(base_ms: u64, scale: u8) -> String {
    let total = base_ms.saturating_mul(scale as u64) / 1000;
    if scale <= 1 {
        format!("{total}s")
    } else {
        format!("{scale}x ({total}s)")
    }
}

/// Is what a drag left behind something we should carry?
///
/// Identical text usually means the drag selected nothing — an image, a
/// scrollbar, a slider. But browsers keep the previous primary selection alive
/// across clicks, so it also means "the same phrase, dragged again", which is
/// indistinguishable from the outside. If that text is one we have already
/// carried, the second reading is the likely one; if it belongs to some other
/// app and we have never touched it, keep failing closed.
///
/// `settled` guards the opposite mistake: an app can publish its new selection
/// a few milliseconds after the button comes up, so the first read of an
/// unchanged value may be the *previous* selection still standing. Believing a
/// repeat only after a short grace period gives a slow app the chance to report
/// something new first, and means the repeat is only taken when it is genuinely
/// still there.
fn is_new_selection(seen: u64, baseline: Option<u64>, owned: Option<u64>, settled: bool) -> bool {
    Some(seen) != baseline || (settled && owned == Some(seen))
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

fn support(open: bool) -> Result<()> {
    // A placeholder is better than a guessed handle: `/magiclick` may well
    // belong to someone else, and a tip jar that quietly pays a stranger is
    // worse than one that is not wired up yet.
    if SUPPORT_URL.contains('<') {
        anyhow::bail!("support link not configured yet — set SUPPORT_URL in src/main.rs");
    }
    println!("{SUPPORT_URL}");
    if open {
        std::process::Command::new("xdg-open")
            .arg(SUPPORT_URL)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
    } else {
        eprintln!("pass --open to open it in your browser");
    }
    Ok(())
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
        Cmd::ArmTimeout { scale } => {
            let base = Config::load().map(|c| c.arm_timeout_ms).unwrap_or(30_000);
            let next = match scale {
                // A named window wins: stepping from the current one would
                // land somewhere else, which is not what the menu asked for.
                Some(n) if (1..=4).contains(&n) => n,
                Some(n) => anyhow::bail!("arm window scale must be 1-4, got {n}"),
                None => {
                    let current = runtime::arm_scale();
                    if current >= 4 { 1 } else { current + 1 }
                }
            };
            runtime::set_arm_scale(next);
            println!("{}", arm_window(base, next));
        }
        Cmd::Watch => watch()?,
        Cmd::Log { n } => print_log(n),
        Cmd::Support { open } => support(open)?,
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

// Every diagnostic goes two places: the journal, for anyone debugging the
// daemon, and the ring file `magiclick log` reads. Same sentence both ways, so
// there is no "the log says one thing and the journal another".
macro_rules! say {
    ($($arg:tt)*) => {{
        let line = format!($($arg)*);
        eprintln!("magiclick: {line}");
        runtime::note(&line);
    }};
}

fn run() -> Result<()> {
    let cfg = Config::load()?;
    let confirm_code = confirm_key_code(&cfg.confirm_key)?;

    // A clean stop for the bar: on SIGTERM/SIGINT we finish the current event,
    // remove the state file, and exit. Without this, `systemctl stop` leaves the
    // last state on disk and the widget keeps claiming the daemon is working.
    install_stop_handlers();

    let (tx, rx) = mpsc::channel::<drag::Input>();
    let n = drag::spawn_readers(confirm_code, cfg.drag_threshold, tx)?;
    if n == 0 {
        anyhow::bail!("no input devices readable — run `magiclick doctor`");
    }

    let clip = clipboard::default_backend();
    // The configured value is the floor. The bar's right-click steps it up in
    // whole multiples, for the flow where you select, wander off, and come back.
    let base_timeout_ms = cfg.arm_timeout_ms;
    let mut scale = runtime::arm_scale();
    let mut timeout_ms = base_timeout_ms.saturating_mul(scale as u64);
    let mut router = Router::new(Duration::from_millis(timeout_ms));
    let mut injector = if cfg.confirm_action == ConfirmAction::Paste {
        Some(inject::Injector::new()?)
    } else {
        None
    };

    say!(
        "auto-copy: {n} input device(s), confirm on {} ({:?}), arm window {}s",
        cfg.confirm_key,
        cfg.confirm_action,
        timeout_ms / 1000
    );
    runtime::set_timeout(&arm_window(base_timeout_ms, scale));
    runtime::set_base(base_timeout_ms);

    let mut last_clipboard_check = Instant::now();
    let mut owned_hash: Option<u64> = None;
    let mut enabled = runtime::control().map(|c| c != "off").unwrap_or(true);
    let mut published = String::new();
    let mut last_control_check = Instant::now();
    let mut last_arm: Option<Instant> = None;
    // What the selections held after the last copy. Read once here, then only
    // updated by our own copies: polling them while idle meant asking whichever
    // app owns the clipboard for a transfer every second, forever, for a value
    // nobody was looking at.
    let mut primary_before: Option<u64> = clip.primary().ok().flatten().map(|t| hash(&t));
    let mut clip_before: Option<u64> = clip.clipboard().ok().flatten().map(|t| hash(&t));

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
        if STOP.load(Ordering::Relaxed) {
            say!("stopping on signal");
            break;
        }
        if last_control_check.elapsed() > Duration::from_millis(250) {
            last_control_check = Instant::now();
            let now_enabled = runtime::control().map(|c| c != "off").unwrap_or(true);
            if now_enabled != enabled {
                enabled = now_enabled;
                if !enabled {
                    router.handle(core::Event::Interference("disabled"));
                    say!("auto-copy turned off");
                } else {
                    // An arm made before the toggle must not survive it, or the
                    // first click after switching back on fires a paste nobody
                    // asked for.
                    let _ = router.handle(core::Event::Interference("re-enabled"));
                    say!("auto-copy turned on");
                }
                publish!();
            }
            // The widget's right-click lands here: same file, same poll.
            let now_scale = runtime::arm_scale();
            if now_scale != scale {
                scale = now_scale;
                timeout_ms = base_timeout_ms.saturating_mul(scale as u64);
                router.set_timeout(Duration::from_millis(timeout_ms));
                let label = arm_window(base_timeout_ms, scale);
                runtime::set_timeout(&label);
                say!("arm window now {label}");
            }
        }

        let event = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(input) => match input {
                drag::Input::Press if enabled => {
                    // Nothing to do. The "what did we have before the drag"
                    // baseline is whatever the last drag left behind, so taking
                    // it here would just read back our own last copy — and it
                    // costs a clipboard transfer on every click on the desktop.
                    continue;
                }
                drag::Input::Drag if enabled => {
                    // The new selection is published some time after the button
                    // comes up. Ask the primary selection first — that is what a
                    // drag in a browser or a terminal actually sets — and only
                    // look at CLIPBOARD if primary never moved.
                    //
                    // Short and sparse on purpose: every read makes the focused
                    // app serve a clipboard transfer on its own event loop, and
                    // Chromium does it on its UI thread. Reading both streams
                    // sixteen times a second is what made the page stutter in
                    // the middle of a selection.
                    let started = Instant::now();
                    let deadline = started + Duration::from_millis(400);
                    let mut picked: Option<(String, &'static str)> = None;
                    let mut last_p: Option<(usize, u64)> = None;
                    let mut last_c: Option<(usize, u64)> = None;

                    while Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(40));
                        let Ok(Some(t)) = clip.primary() else {
                            continue;
                        };
                        if t.is_empty() {
                            continue;
                        }
                        let h = hash(&t);
                        last_p = Some((t.len(), h));
                        let settled = started.elapsed() >= Duration::from_millis(120);
                        if is_new_selection(h, primary_before, owned_hash, settled) {
                            picked = Some((t, "primary"));
                            break;
                        }
                    }

                    if picked.is_none() {
                        // One look only, for apps that never publish primary.
                        if let Ok(Some(t)) = clip.clipboard() {
                            if !t.is_empty() {
                                let h = hash(&t);
                                last_c = Some((t.len(), h));
                                // No `owned` here on purpose: the clipboard is
                                // where our own writes land, so an unchanged
                                // read is far more likely to be a drag that
                                // selected nothing than a deliberate repeat.
                                if is_new_selection(h, clip_before, None, true) {
                                    picked = Some((t, "clipboard"));
                                }
                            }
                        }
                    }
                    match picked {
                        None => {
                            // Nothing new: rebase anyway. While armed the live
                            // selection *is* the armed text, and a frozen
                            // baseline made every later drag look "unchanged",
                            // so the arm could never be replaced — only fired.
                            if let Some((_, h)) = last_p {
                                primary_before = Some(h);
                            }
                            if let Some((_, h)) = last_c {
                                clip_before = Some(h);
                            }
                            say!(
                                "a selection in {} did not change ({} already selected, {}) — nothing copied",
                                source_app().unwrap_or_else(|| "?".into()),
                                match last_p {
                                    Some((n, _)) => format!("{n} bytes"),
                                    None => "nothing".into(),
                                },
                                match last_c {
                                    Some((n, _)) => format!("{n} bytes on the clipboard"),
                                    None => "clipboard empty".into(),
                                }
                            );
                            continue;
                        }
                        Some((text, from)) => {
                            // `wl-copy -n` cannot remove a newline the selection
                            // already carries, so a line selection pasted a blank
                            // line after itself. Strip exactly one line ending.
                            let text = core::trim_selection(&text).to_string();
                            if text.is_empty() {
                                continue;
                            }
                            if text.len() > cfg.max_bytes {
                                say!(
                                    "selection too big ({} bytes, limit {}) — ignored",
                                    text.len(),
                                    cfg.max_bytes
                                );
                                continue;
                            }
                            let h = hash(&text);
                            // Re-selecting text we already carry is a real
                            // drag — the router decides whether it is news (it
                            // is, once nothing is armed). Only the redundant
                            // clipboard write is skipped: we already own it.
                            let already_ours = owned_hash == Some(h);
                            if cfg.copy_on_drag && !already_ours {
                                if let Err(e) = clip.set_clipboard(&text) {
                                    say!("clipboard write failed: {e}");
                                }
                            }
                            owned_hash = Some(h);
                            // Rebase the baselines to the live state: our own
                            // clipboard write must never look like a new drag,
                            // and the source we took the text from now holds it.
                            if from == "primary" {
                                primary_before = Some(h);
                            }
                            if from == "clipboard" || cfg.copy_on_drag {
                                clip_before = Some(h);
                            }
                            let app = source_app();
                            say!(
                                "copied {} bytes from {from} in {}",
                                text.len(),
                                app.as_deref().unwrap_or("?")
                            );
                            Event::DragCopied(Selection {
                                hash: h,
                                len: text.len(),
                                source_app: app,
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
                                say!(
                                    "a click landed {:.1}s after the arming, past the {}s window — nothing pasted",
                                    age.as_secs_f64(),
                                    timeout_ms as f64 / 1000.0
                                );
                            }
                        }
                    }
                    // Bar, desktop, layer surface: never a text field, and always
                    // a navigation gesture — "which desktop / which window".
                    // Stay armed and say nothing: pasting here is what erased a
                    // prompt, and disarming here would kill the very flow the
                    // user is performing (select -> switch desktop -> click).
                    if router.is_armed()
                        && compositor::click_target() == compositor::Target::NoWindow
                    {
                        say!("click landed on the bar or desktop — kept armed, nothing pasted");
                        continue;
                    }
                    // The click must actually focus what was clicked before any
                    // key goes out, or the chord lands in the window that was
                    // focused a moment ago (browser click, terminal paste).
                    if router.is_armed()
                        && !compositor::wait_for_focus(Duration::from_millis(400))
                    {
                        say!("the clicked window never took focus — kept armed, click inside it");
                        continue;
                    }
                    Event::Click
                }
                drag::Input::Confirm if enabled => Event::Confirm,
                drag::Input::Cancel if enabled => Event::Interference("right-clicked"),
                drag::Input::KeyCode if enabled => Event::Interference("typed"),
                _ => continue,
            },
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let mut ev = Event::Tick(Instant::now());
                if router.is_armed() {
                    // While armed, watch for something else taking the clipboard.
                    // Once a second is enough for a safety net, and after the
                    // first read it is our own `wl-copy` serving the data, so
                    // this stays cheap.
                    if last_clipboard_check.elapsed() > Duration::from_millis(1000) {
                        last_clipboard_check = Instant::now();
                        if let Ok(Some(current)) = clip.clipboard() {
                            if Some(hash(&current)) != owned_hash {
                                ev = Event::ClipboardMoved;
                            }
                        }
                    }
                }
                ev
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
                    say!(
                        "armed {} bytes from {} — click where it goes",
                        sel.len,
                        sel.source_app.as_deref().unwrap_or("?")
                    );
                }
            }
            Outcome::Disarmed(reason) => {
                last_arm = None;
                say!("disarmed — {reason}")
            }
            Outcome::Fire => {
                last_arm = None;
                if let (Some(inj), Some(_sel)) = (injector.as_mut(), held.as_ref()) {
                    let chord = if cfg.paste_chord.eq_ignore_ascii_case("auto") {
                        compositor::paste_chord().to_string()
                    } else {
                        cfg.paste_chord.clone()
                    };
                    if let Err(e) = inj.chord(&chord) {
                        say!("paste failed: {e}");
                    } else {
                        say!("pasted via {chord}");
                    }
                } else {
                    say!(
                        "confirm seen ({} bytes) — observe mode, nothing typed",
                        held.as_ref().map(|s| s.len).unwrap_or(0)
                    );
                }
            }
        }
        publish!();
    }

    runtime::clear_state();
    Ok(())
}

/// Set when SIGTERM/SIGINT arrives, so the event loop can exit at a safe point
/// instead of being killed mid-write.
static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop_signal(_sig: i32) {
    STOP.store(true, Ordering::SeqCst);
}

unsafe extern "C" {
    /// `signal(2)`. Declared here rather than pulling in a signal crate for two
    /// lines: the handler only stores a bool, so it is async-signal-safe.
    fn signal(signum: i32, handler: usize) -> usize;
}

fn install_stop_handlers() {
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    let h = on_stop_signal as *const () as usize;
    // SAFETY: the handler only writes an atomic; SIG_ERR is ignored deliberately
    // (a failed handler means the old behaviour: killed without cleanup).
    unsafe {
        signal(SIGTERM, h);
        signal(SIGINT, h);
    }
}

/// Debug aid: press a button, read its name. Held until Ctrl-C.
fn watch() -> Result<()> {
    use std::sync::mpsc::channel;
    let (tx, rx) = channel::<String>();
    let mut seen = 0;
    for (path, mut dev) in evdev::enumerate() {
        let name = dev.name().unwrap_or("?").to_string();
        if name.contains("magiclick") {
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
        anyhow::bail!("no devices readable — run `magiclick doctor`");
    }
    eprintln!("magiclick: watching {} device(s). Press the button, then Ctrl-C.", seen);
    for line in rx {
        println!("{line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_changed_selection_is_always_taken() {
        assert!(is_new_selection(7, Some(6), None, false));
        assert!(is_new_selection(7, Some(6), Some(1), true));
    }

    #[test]
    fn the_first_ever_drag_is_taken() {
        assert!(is_new_selection(7, None, None, false));
    }

    #[test]
    fn unchanged_text_we_have_never_carried_is_refused() {
        // Some other app's selection sitting there while we drag an image.
        assert!(!is_new_selection(7, Some(7), None, true));
        assert!(!is_new_selection(7, Some(7), Some(9), true));
    }

    #[test]
    fn dragging_the_same_text_we_carried_again_is_taken() {
        // Chromium: select a phrase, paste it, drag the same phrase again.
        // Primary never changed, so only memory of what we carried can tell
        // this apart from a drag that selected nothing.
        assert!(is_new_selection(7, Some(7), Some(7), true));
    }

    #[test]
    fn a_repeat_is_not_believed_before_the_app_has_had_time_to_publish() {
        // First read after release: the old selection may still be standing
        // while the app is about to publish the new one. Waiting is free.
        assert!(!is_new_selection(7, Some(7), Some(7), false));
    }
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
