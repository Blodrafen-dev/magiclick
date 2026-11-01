mod clipboard;
mod compositor;
mod config;
mod core;
mod doctor;
mod drag;
mod inject;
mod runtime;

use anyhow::Result;
use clap::{Parser, Subcommand};
use clipboard::Clipboard;
use config::{ArmOnCopy, Config, ConfirmAction};
use core::{Event, Outcome, Router, Selection};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(
    name = "magiclick",
    version,
    about = "Auto-copy a selection, confirm to paste."
)]
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

/// Where `magiclick support` points. One source of truth: the bar widget's
/// Support row and the README both lead here, so moving the tip jar is a
/// one-line change rather than a hunt.
///
/// One constant and no network code on purpose. This daemon reads every key and
/// pointer event on the machine and injects keystrokes into whatever window is
/// focused; outbound HTTP in that process would be a permanent line in its
/// threat model for a tip jar. So it prints the URL, and the only request ever
/// made is the one your own browser makes, when you ask for it with `--open`.
///
/// Ko-fi is the destination because it is the page that can hold everything —
/// the tip button, memberships later, and a link list — with no approval step
/// and no fee on one-time tips. Buy Me a Coffee is listed next to it in
/// `.github/FUNDING.yml`. GitHub Sponsors is there too, but
/// `/sponsors/Blodrafen-dev` still redirects to the profile until that listing
/// is approved, so this does not point at it.
const SUPPORT_URL: &str = "https://ko-fi.com/magiclick";

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

/// Did PRIMARY move off the value it held when the gesture began?
///
/// Only this counts as a new primary selection. Holding still is not news — see
/// `clipboard_is_news` for what that costs when it is mistaken for it.
fn primary_moved(seen: u64, baseline: Option<u64>) -> bool {
    Some(seen) != baseline
}

/// Does CLIPBOARD hold something neither we nor the gesture put there?
///
/// Excluding `owned` is deliberate, and the opposite of what this path used to
/// do. The clipboard is where our own writes land, so it was argued that an
/// unchanged read was likelier a drag that selected nothing than a deliberate
/// repeat. That holds while PRIMARY is the stream that moves. It stops holding
/// in a terminal that does its own selection: PRIMARY freezes at whatever it had
/// while the app publishes the real selection to CLIPBOARD, and then the one
/// stream that knows what the user just highlighted is the one being ignored.
fn clipboard_is_news(seen: u64, clip_before: Option<u64>, owned: Option<u64>) -> bool {
    Some(seen) != clip_before && Some(seen) != owned
}

/// What both streams held when the button went down.
///
/// This is the baseline for "did this gesture select something?". It is
/// deliberately not the last text the daemon copied: a keyboard selection, a
/// clipboard manager, or a Ctrl+C between two gestures changes a stream without
/// the daemon witnessing a drag, and comparing against the last copy then made
/// the next drag that selected nothing adopt that older text — the stale paste.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct AtPress {
    primary: Option<u64>,
    clipboard: Option<u64>,
    /// False when the snapshot could not be taken in time. The caller falls back
    /// to the last copy and says so in the log, rather than pretending.
    sampled: bool,
}

/// Read both streams off the event loop, once per button press.
///
/// Two transfers per press is affordable. Blocking the router for up to 800ms on
/// a wedged clipboard owner is not — that is indistinguishable from the daemon
/// hanging, and it would happen on every click on the desktop.
fn sample_at_press() -> mpsc::Receiver<(Option<u64>, Option<u64>)> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let clip = clipboard::default_backend();
        let primary = clip.primary().ok().flatten().map(|t| hash(&t));
        let clipboard = clip.clipboard().ok().flatten().map(|t| hash(&t));
        let _ = tx.send((primary, clipboard));
    });
    rx
}

/// The gesture's baseline, or the last copy's if the snapshot did not arrive.
///
/// Bounded wait: a drag lasts at least a few frames, so the answer is normally
/// already there by the time the button comes up. When it is not, degrading to
/// the old baseline beats stalling the router — and `sampled: false` is carried
/// into the log so a degrading machine is visible rather than silent.
fn settle_baseline(
    pending: Option<mpsc::Receiver<(Option<u64>, Option<u64>)>>,
    legacy: AtPress,
) -> AtPress {
    match pending.and_then(|rx| rx.recv_timeout(Duration::from_millis(120)).ok()) {
        Some((primary, clipboard)) => AtPress {
            primary,
            clipboard,
            sampled: true,
        },
        None => legacy,
    }
}

/// Whether the clipboard still needs our write.
///
/// `owned_hash` is bookkeeping, not evidence. Between two selections of the same
/// text something else can take the clipboard — the chat client you copied from,
/// a clipboard manager restoring an entry — and trusting the hash then leaves
/// that foreign value in place while the arm claims to own it. The safety guard
/// reads the clipboard, sees a stranger, and disarms: the "armed, then instantly
/// disarmed" bug. So confirm against the live clipboard, and only after the hash
/// already matches, so an ordinary first selection still costs no transfer.
fn must_write(owned: Option<u64>, h: u64, clip: &dyn Clipboard) -> bool {
    if owned != Some(h) {
        return true;
    }
    !matches!(clip.clipboard(), Ok(Some(ref c)) if hash(c) == h)
}

/// Whether a clipboard takeover is close enough to our own write to be part of
/// the same gesture.
///
/// A terminal UI that copies the selection it just saw land (OSC 52) writes the
/// clipboard a few hundred milliseconds after the drag, on top of ours. That is
/// not somebody else copying — it is the app we selected in publishing its own
/// idea of the same text — so the arm should survive it. A takeover later than
/// this is a real one, and a second one for the same arm means the clipboard is
/// genuinely in use elsewhere: fail closed there rather than ping-pong.
fn same_gesture(since_owned: Duration, reasserted: bool) -> bool {
    !reasserted && since_owned < Duration::from_millis(1500)
}

/// Letters and digits only, lowercased, so a re-serialisation of the same words
/// compares equal: the same phrase with different spacing, padding or punctuation
/// is the same selection, not a new one.
fn folded(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Is a clipboard write the app's version of the selection we already carry?
///
/// A terminal UI that copies the selection it saw land rarely writes the same
/// bytes: it copies its idea of the region — the whole line, trailing spaces,
/// a different newline. Judging that by the clock alone was wrong in the
/// expensive direction: within the gesture, *any* write looked like ours, so a
/// genuinely new selection was thrown away and the previous arm pasted in its
/// place. So compare the text. Containment either way means "the same words at
/// a different extent", which is ours to keep.
///
/// Containment is only evidence when there is enough text to be evidence: a
/// two-letter arm is inside half the lines on screen, and treating those as ours
/// is how a stale arm gets pasted over something new.
fn looks_like_same_selection(foreign: &str, armed: &str) -> bool {
    let f = folded(foreign);
    let a = folded(armed);
    if f.is_empty() || a.is_empty() {
        return false;
    }
    if f == a {
        return true;
    }
    let (short, long) = if a.len() <= f.len() {
        (&a, &f)
    } else {
        (&f, &a)
    };
    short.chars().count() >= 8 && long.contains(short.as_str())
}

/// Put the armed text back on the clipboard, and wait — briefly — for it to land.
///
/// `ctrl+v` pastes the clipboard, not our copy of the text, so at the moment of
/// the paste the two have to be the same bytes. A terminal UI that copies the
/// selection it just saw re-publishes its own serialisation of it, and that can
/// land after our write; pasting it would look right and be wrong in the ways
/// that matter, like a missing line break in a command. So assert ours and
/// confirm, bounded, because `wl-copy` forks and the child takes ownership a
/// moment later.
fn reassert_for_paste(clip: &dyn Clipboard, sel: &core::Selection) -> bool {
    let ours = || clip.clipboard().ok().flatten().map(|t| hash(&t)) == Some(sel.hash);
    if ours() {
        return true;
    }
    if clip.set_clipboard(&sel.text).is_err() {
        return false;
    }
    let deadline = Instant::now() + Duration::from_millis(300);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
        if ours() {
            return true;
        }
    }
    false
}

/// Did the gesture that just ended leave a selection behind?
///
/// The pointer reader classifies a release as a click or a drag by how far the
/// pointer travelled, and that threshold is a guess about the mouse — not a fact
/// about the user. Apps select on any movement at all, so a quick, short drag
/// selects a word and still looks like a click here. Treating it as a click
/// pastes over the selection that was just made, which is the one thing this
/// program must never do.
///
/// The app is the authority. New non-empty text on either stream means the
/// gesture was selecting, whatever our threshold said. Both streams matter:
/// terminals and GTK publish to PRIMARY, Chromium and Electron only to CLIPBOARD.
fn release_left_a_selection(
    clip: &dyn Clipboard,
    primary_base: Option<u64>,
    clip_base: Option<u64>,
    owned: Option<u64>,
) -> bool {
    let fresh = |t: &str, base: Option<u64>| {
        let h = hash(t);
        Some(h) != base && Some(h) != owned
    };
    if let Ok(Some(t)) = clip.primary()
        && !t.is_empty()
        && fresh(&t, primary_base)
    {
        return true;
    }
    if let Ok(Some(t)) = clip.clipboard()
        && !t.is_empty()
        && fresh(&t, clip_base)
    {
        return true;
    }
    false
}

/// Did a copy land that this click caused?
///
/// A copy glyph and "Copy link address" both put the text on CLIPBOARD, so that
/// is asked first. PRIMARY is the fallback for apps that only publish there —
/// but a click that collapses a selection *clears* primary, and a cleared
/// primary is not a copy, so empty reads are ignored.
///
/// The baseline was sampled at the moment of the click, so anything that differs
/// from it arrived afterwards. That ordering is the whole guarantee: a copy made
/// before the click cannot arm.
fn copy_since_click(
    clip: &dyn Clipboard,
    clip_base: Option<u64>,
    primary_base: Option<u64>,
    owned: Option<u64>,
) -> Option<(String, &'static str)> {
    if let Ok(Some(t)) = clip.clipboard() {
        let h = hash(&t);
        if Some(h) != clip_base && Some(h) != owned {
            return Some((t, "clipboard"));
        }
    }
    if let Ok(Some(t)) = clip.primary() {
        let h = hash(&t);
        if Some(h) != primary_base && Some(h) != owned {
            return Some((t, "primary"));
        }
    }
    None
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
    // Restore the default SIGPIPE behaviour. Rust ignores it, so `magiclick log
    // | head` panicked on a broken pipe instead of exiting the way every other
    // CLI does — and piping into `head` is how this is actually read.
    restore_sigpipe();

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
    let mut owned_at = Instant::now();
    let mut owned_len: Option<usize> = None;
    // When we last put text on the clipboard, and how much of it. `wl-copy`
    // forks and serves the data from the child, so between the write and the
    // child taking ownership a read still returns whatever was there before.
    // That window is short, and it used to look exactly like a foreign copy —
    // which disarmed every arm made in a terminal moments after it was made.
    // Keep this for the disarm path, but it does NOT reset after each drag
    // (arm lifetime is only reset by disarm), because the arm’s contents
    // are the right ones for paste, and a fresh drag should carry its new
    // selection forward from the new press.
    // One take-back per arm. Reset on every arm and disarm.
    let mut reasserted = false;
    // One adoption of the app's newer selection per gesture, so two writers
    // cannot ping-pong the clipboard between them.
    let mut adopted = false;
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

    // A click with nothing armed opens a short window. If a copy lands inside
    // it, that copy was caused by the click — a copy glyph, or "Copy link
    // address" — and arms exactly like a drag would.
    let mut copy_click: Option<Instant> = None;
    let mut copy_click_base: Option<u64> = None;
    let mut last_copy_poll = Instant::now();
    // What both streams held when the button went down. A click confirms, and
    // only a change since *this press* is a selection: the older baselines
    // predate the press, so anything an app published after our arm would
    // otherwise look like a fresh selection and turn every confirm into a drag —
    // PRIMARY included, which is what a browser's selection had already put
    // there before the click.
    // Started on every press and collected when the gesture ends; see
    // `sample_at_press`. Never read on the event loop.
    let mut press_snapshot: Option<mpsc::Receiver<(Option<u64>, Option<u64>)>> = None;

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
                    // The arm is gone, so forget when it happened: a click ten
                    // seconds from now is not "past the window", and the
                    // diagnostic would otherwise report a stale one.
                    last_arm = None;
                    say!("auto-copy turned off");
                } else {
                    // An arm made before the toggle must not survive it, or the
                    // first click after switching back on fires a paste nobody
                    // asked for.
                    let _ = router.handle(core::Event::Interference("re-enabled"));
                    last_arm = None;
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
            Ok(input) => {
                // What both streams held when this gesture began. A gesture
                // that changed neither selected nothing, whatever text was
                // already sitting there.
                let at_press = settle_baseline(
                    press_snapshot.take(),
                    AtPress {
                        primary: primary_before,
                        clipboard: clip_before,
                        sampled: false,
                    },
                );
                // A click is not always a click. Short drags select text too, and
                // the difference between "I selected this" and "I am confirming"
                // is whether anything new got selected — not how far the mouse
                // moved. Checked here, before the match, because the answer
                // routes the gesture down the drag path instead.
                let short_drag = enabled
                    && router.is_armed()
                    && matches!(input, drag::Input::Click(_))
                    && release_left_a_selection(
                        &*clip,
                        at_press.primary,
                        at_press.clipboard,
                        owned_hash,
                    );
                if let drag::Input::Click(travel) = input
                    && short_drag
                {
                    say!(
                        "click with {travel} units of travel left a new selection — treated as a drag, not a confirm"
                    );
                }
                match input {
                    drag::Input::Press if enabled => {
                        // Sample both streams before this gesture can change them.
                        //
                        // This used to be skipped while nothing was armed, on the
                        // theory that the last copy was a good enough baseline. It
                        // is not: anything that changes PRIMARY or CLIPBOARD outside
                        // a drag the daemon saw — a keyboard selection, a clipboard
                        // manager, a Ctrl+C — then looks like a fresh selection on
                        // the next drag that selected nothing, and that older text
                        // is what gets pasted. Sampling here costs two transfers per
                        // press and removes the whole class.
                        press_snapshot = Some(sample_at_press());
                        continue;
                    }
                    drag::Input::Drag if enabled || short_drag => {
                        // A drag is selecting, not clicking a copy control. Close any
                        // window a previous click opened so the two paths cannot both
                        // claim the same copy.
                        //
                        // A new drag is a new gesture, so it earns a fresh adoption.
                        copy_click = None;
                        adopted = false;
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
                        let primary_deadline = started + Duration::from_millis(250);
                        let clipboard_deadline = started + Duration::from_millis(440);
                        let mut picked: Option<(String, &'static str)> = None;
                        let mut last_p: Option<(usize, u64)> = None;
                        let mut last_p_text: Option<String> = None;
                        let mut last_c: Option<(usize, u64)> = None;

                        // Phase 1 — PRIMARY, and only if it actually moved.
                        //
                        // This loop used to also break when PRIMARY still held text
                        // we already carry, on the theory that re-selecting the same
                        // words is a new intent. That rule is only safe when nothing
                        // else is talking. A terminal that does its own selection
                        // freezes PRIMARY at whatever it held while the app publishes
                        // the real selection to CLIPBOARD, so the rule re-armed the
                        // stale text on every drag and the clipboard was then
                        // overwritten with it: "it keeps pasting old copies".
                        while Instant::now() < primary_deadline {
                            std::thread::sleep(Duration::from_millis(40));
                            let Ok(Some(t)) = clip.primary() else {
                                continue;
                            };
                            if t.is_empty() {
                                continue;
                            }
                            let h = hash(&t);
                            last_p = Some((t.len(), h));
                            if primary_moved(h, at_press.primary) {
                                picked = Some((t, "primary"));
                                break;
                            }
                            last_p_text = Some(t);
                        }

                        // Phase 2 — CLIPBOARD, and before the re-selection rule.
                        //
                        // A stream that moved is better evidence than a stream that
                        // is holding still, so this is asked first. Only reached when
                        // PRIMARY had nothing new to say.
                        if picked.is_none() {
                            while Instant::now() < clipboard_deadline {
                                if let Ok(Some(t)) = clip.clipboard()
                                    && !t.is_empty()
                                {
                                    let h = hash(&t);
                                    last_c = Some((t.len(), h));
                                    if clipboard_is_news(h, at_press.clipboard, owned_hash) {
                                        picked = Some((t, "clipboard"));
                                        break;
                                    }
                                }
                                std::thread::sleep(Duration::from_millis(40));
                            }
                        }

                        // Phase 3 — nothing moved anywhere, so the re-selection rule
                        // is admissible: PRIMARY holding the text we already carry is
                        // the only evidence there is, and nothing contradicted it.
                        if picked.is_none()
                            && let Some(text) = last_p_text
                            && is_new_selection(hash(&text), at_press.primary, owned_hash, true)
                        {
                            picked = Some((text, "primary"));
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
                                    "a selection in {} did not change since the button went down ({} already selected, {}) — nothing copied{}",
                                    source_app().unwrap_or_else(|| "?".into()),
                                    match last_p {
                                        Some((n, _)) => format!("{n} bytes"),
                                        None => "nothing".into(),
                                    },
                                    match last_c {
                                        Some((n, _)) => format!("{n} bytes on the clipboard"),
                                        None => "clipboard empty".into(),
                                    },
                                    if at_press.sampled {
                                        ""
                                    } else {
                                        "; the clipboard did not answer at press, so the last copy is the baseline"
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
                                // is, once nothing is armed). The clipboard write is
                                // skipped only if the clipboard still holds it: the
                                // hash alone goes stale, and skipping then leaves a
                                // foreign value under an arm that claims to own it.
                                if cfg.copy_on_drag
                                    && must_write(owned_hash, h, clip.as_ref())
                                    && let Err(e) = clip.set_clipboard(&text)
                                {
                                    say!("clipboard write failed: {e}");
                                }
                                owned_hash = Some(h);
                                owned_at = Instant::now();
                                owned_len = Some(text.len());
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
                    drag::Input::Click(_) if enabled && !short_drag => {
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
                            // Nothing armed, so this click is free to become an arm:
                            // copy glyphs, "Copy link address" and every other
                            // "this click copied something" case begin here.
                            //
                            // The baseline is sampled now rather than trusted from
                            // the last drag. A Ctrl+C since then would otherwise look
                            // like a copy this click caused.
                            if cfg.arm_on_copy == ArmOnCopy::Click {
                                copy_click = Some(Instant::now());
                                let base = clip.clipboard().ok().flatten().map(|t| hash(&t));
                                copy_click_base = base;
                                clip_before = base;
                                last_copy_poll = Instant::now();
                            }
                            continue;
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
                            say!(
                                "the clicked window never took focus — kept armed, click inside it"
                            );
                            continue;
                        }
                        Event::Click
                    }
                    drag::Input::Confirm if enabled => {
                        copy_click = None;
                        Event::Confirm
                    }
                    drag::Input::Cancel if enabled => {
                        // Right-click is how "Copy link address" starts: the menu
                        // item that does the copying is a click or two away. Disarm
                        // as always, and open the same window for the copy that the
                        // menu is about to make.
                        if cfg.arm_on_copy == ArmOnCopy::Click {
                            copy_click = Some(Instant::now());
                            let base = clip.clipboard().ok().flatten().map(|t| hash(&t));
                            copy_click_base = base;
                            clip_before = base;
                            last_copy_poll = Instant::now();
                        }
                        Event::Interference("right-clicked")
                    }
                    drag::Input::KeyCode if enabled => {
                        // Typing means the click window is over: nothing about a
                        // keystroke suggests a copy glyph was pressed.
                        copy_click = None;
                        Event::Interference("typed")
                    }
                    _ => continue,
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let now = Instant::now();
                let mut ev = Event::Tick(now);

                // The copy window. Polling starts only when a click opened one,
                // so an idle daemon still touches the clipboard exactly as often
                // as it used to: never.
                if let Some(at) = copy_click {
                    if now.saturating_duration_since(at)
                        >= Duration::from_millis(cfg.copy_click_window_ms)
                    {
                        copy_click = None;
                    } else if last_copy_poll.elapsed() >= Duration::from_millis(50) {
                        last_copy_poll = now;
                        if let Some((text, from)) =
                            copy_since_click(&*clip, copy_click_base, primary_before, owned_hash)
                        {
                            copy_click = None;
                            // Same normalisation as a drag: `wl-copy -n` cannot
                            // remove a newline the copy already carried.
                            let text = core::trim_selection(&text).to_string();
                            if !text.is_empty() {
                                if text.len() > cfg.max_bytes {
                                    say!(
                                        "copy too big to carry ({} bytes, limit {}) — ignored",
                                        text.len(),
                                        cfg.max_bytes
                                    );
                                } else {
                                    let h = hash(&text);
                                    // Whatever the site put on the clipboard, we
                                    // re-write it: the paste injects ctrl+v and
                                    // pastes the clipboard, not our copy of the
                                    // text, so the two must be the same bytes.
                                    if owned_hash != Some(h)
                                        && let Err(e) = clip.set_clipboard(&text)
                                    {
                                        say!("clipboard write failed: {e}");
                                    }
                                    owned_hash = Some(h);
                                    owned_at = Instant::now();
                                    owned_len = Some(text.len());
                                    clip_before = Some(h);
                                    if from == "primary" {
                                        primary_before = Some(h);
                                    }
                                    let app = source_app();
                                    say!(
                                        "copied {} bytes from a copy click in {}",
                                        text.len(),
                                        app.as_deref().unwrap_or("?")
                                    );
                                    ev = Event::DragCopied(Selection {
                                        hash: h,
                                        len: text.len(),
                                        source_app: app,
                                        text,
                                    });
                                }
                            }
                        }
                    }
                }

                if router.is_armed() {
                    // While armed, watch for something else taking the clipboard.
                    // Once a second is enough for a safety net, and after the
                    // first read it is our own `wl-copy` serving the data, so
                    // this stays cheap.
                    //
                    // Two guards keep this from firing on our own write:
                    //
                    //  * `wl-copy` forks and the child serves the data, so for a
                    //    few milliseconds after `set_clipboard` a read still
                    //    returns the PREVIOUS contents. That is not a foreign
                    //    copy, and treating it as one disarmed every arm made in
                    //    a terminal within the same tick it was made.
                    //  * a single bad read is not a takeover. A real foreign copy
                    //    is still there 250ms later; a race is not.
                    if last_clipboard_check.elapsed() > Duration::from_millis(1000)
                        && owned_at.elapsed() > Duration::from_millis(300)
                    {
                        last_clipboard_check = Instant::now();
                        if let Ok(Some(current)) = clip.clipboard()
                            && Some(hash(&current)) != owned_hash
                        {
                            // Confirm before acting. Confirming costs 250ms
                            // and removes the entire false-disarm class.
                            std::thread::sleep(Duration::from_millis(250));
                            let again = clip.clipboard().ok().flatten();
                            let still_foreign = matches!(
                                again.as_ref(),
                                Some(t) if Some(hash(t)) != owned_hash
                            );
                            if still_foreign {
                                // Two different things can land here, and
                                // they need opposite answers:
                                //
                                //  * the app we selected in re-publishing the
                                //    selection we already carry, at a
                                //    different extent — ours is the precise
                                //    one, so take the clipboard back once;
                                //  * the app publishing a *new* selection —
                                //    taking the clipboard back then throws
                                //    away the thing the user just selected
                                //    and pastes the previous arm instead,
                                //    which is the stale-paste bug in the
                                //    field. The newer text wins.
                                let foreign = again.unwrap_or_default();
                                let armed = router.armed_selection().cloned();
                                let in_gesture = same_gesture(owned_at.elapsed(), reasserted);
                                let same_text = armed.as_ref().is_some_and(|sel| {
                                    in_gesture && looks_like_same_selection(&foreign, &sel.text)
                                });
                                if same_text {
                                    let sel = armed.expect("same_text implies an arm");
                                    if clip.set_clipboard(&sel.text).is_ok() {
                                        reasserted = true;
                                        say!(
                                            "the app re-published the same selection as {} bytes within the gesture — took the clipboard back, still armed on {} bytes",
                                            foreign.len(),
                                            sel.len
                                        );
                                        owned_at = Instant::now();
                                        owned_hash = Some(sel.hash);
                                        owned_len = Some(sel.len);
                                    } else {
                                        say!(
                                            "clipboard write failed — the arm cannot be pasted now"
                                        );
                                        ev = Event::ClipboardMoved;
                                    }
                                } else if in_gesture
                                    && !adopted
                                    && !foreign.is_empty()
                                    && foreign.len() <= cfg.max_bytes
                                {
                                    let text = core::trim_selection(&foreign).to_string();
                                    if !text.is_empty() {
                                        let h = hash(&text);
                                        if let Err(e) = clip.set_clipboard(&text) {
                                            say!("clipboard write failed: {e}");
                                        }
                                        adopted = true;
                                        owned_hash = Some(h);
                                        owned_at = Instant::now();
                                        owned_len = Some(text.len());
                                        clip_before = Some(h);
                                        let app = source_app();
                                        say!(
                                            "the clipboard changed to different text ({} bytes) within the gesture — carrying that instead of the {} bytes armed",
                                            text.len(),
                                            armed.as_ref().map(|s| s.len).unwrap_or(0)
                                        );
                                        ev = Event::DragCopied(Selection {
                                            hash: h,
                                            len: text.len(),
                                            source_app: app,
                                            text,
                                        });
                                    }
                                } else {
                                    // Our own write may not be visible immediately: wl-copy
                                    // forks, and the child takes ownership a few ms later.
                                    // Skip the disarming test while our write is settling.
                                    if owned_at.elapsed() <= Duration::from_millis(300) {
                                        continue;
                                    }
                                    // A single bad read is not a takeover. A real foreign copy
                                    // still holds after 250ms; a race does not.
                                    std::thread::sleep(Duration::from_millis(250));
                                    let again = clip.clipboard().ok().flatten();
                                    let still_foreign = matches!(again.as_ref(), Some(t) if Some(hash(t)) != owned_hash);
                                    if still_foreign {
                                        say!(
                                            "clipboard now holds {} bytes, the arm holds {} — something else took it",
                                            again.as_ref().map(|t| t.len()).unwrap_or(0),
                                            owned_len.unwrap_or(0)
                                        );
                                        ev = Event::ClipboardMoved;
                                    }
                                }
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
                reasserted = false;
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
                reasserted = false;
                adopted = false;
                say!("disarmed — {reason}")
            }
            Outcome::Fire => {
                last_arm = None;
                if let (Some(inj), Some(sel)) = (injector.as_mut(), held.as_ref()) {
                    if !reassert_for_paste(&*clip, sel) {
                        say!("could not put the arm back on the clipboard — pasting what is there");
                    }
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

/// Put SIGPIPE back to its default, which Rust turns off at startup.
///
/// Without this, `magiclick log | head -5` ends with a panic message about a
/// broken pipe rather than simply stopping, which makes the CLI look broken at
/// exactly the moment someone is reading it.
fn restore_sigpipe() {
    const SIGPIPE: i32 = 13;
    const SIG_DFL: usize = 0;
    // SAFETY: SIG_DFL for SIGPIPE is the documented POSIX default.
    unsafe {
        signal(SIGPIPE, SIG_DFL);
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
        let Some(keys) = dev.supported_keys() else {
            continue;
        };
        let interesting =
            keys.contains(evdev::KeyCode::BTN_LEFT) || keys.contains(evdev::KeyCode::KEY_A);
        if !interesting {
            continue;
        }
        seen += 1;
        let tx = tx.clone();
        std::thread::spawn(move || {
            let _ = &path;
            loop {
                let Ok(events) = dev.fetch_events() else {
                    return;
                };
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
    eprintln!(
        "magiclick: watching {} device(s). Press the button, then Ctrl-C.",
        seen
    );
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

    /// A clipboard whose two streams the test controls.
    struct Fake {
        primary: Option<String>,
        clipboard: std::cell::RefCell<Option<String>>,
        written: std::cell::RefCell<Vec<String>>,
    }

    impl Fake {
        fn new(primary: Option<&str>, clipboard: Option<&str>) -> Self {
            Self {
                primary: primary.map(String::from),
                clipboard: std::cell::RefCell::new(clipboard.map(String::from)),
                written: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl Clipboard for Fake {
        fn primary(&self) -> Result<Option<String>> {
            Ok(self.primary.clone())
        }
        fn clipboard(&self) -> Result<Option<String>> {
            Ok(self.clipboard.borrow().clone())
        }
        fn set_clipboard(&self, text: &str) -> Result<()> {
            self.written.borrow_mut().push(text.to_string());
            *self.clipboard.borrow_mut() = Some(text.to_string());
            Ok(())
        }
    }

    #[test]
    fn a_re_selection_is_written_again_when_something_else_took_the_clipboard() {
        // The reported bug: the same text selected twice, with a foreign
        // clipboard in between. Skipping the write left that stranger in place
        // while the arm claimed it, and the guard disarmed on its own wake-up.
        let h = hash("Cancel");
        let clip = Fake::new(Some("Cancel"), Some("the chat client's 167 bytes"));
        assert!(must_write(Some(h), h, &clip));
    }

    #[test]
    fn a_re_selection_is_not_rewritten_while_the_clipboard_still_holds_it() {
        let h = hash("Cancel");
        let clip = Fake::new(Some("Cancel"), Some("Cancel"));
        assert!(!must_write(Some(h), h, &clip));
        assert!(clip.written.borrow().is_empty());
    }

    #[test]
    fn a_first_selection_is_always_written() {
        let h = hash("Cancel");
        assert!(must_write(
            None,
            h,
            &Fake::new(Some("Cancel"), Some("Cancel"))
        ));
        // An empty clipboard is not a match either.
        assert!(must_write(Some(h), h, &Fake::new(Some("Cancel"), None)));
    }

    #[test]
    fn a_takeover_inside_the_gesture_is_taken_back() {
        // A TUI copies the selection it just saw land, ~300ms after the drag.
        assert!(same_gesture(Duration::from_millis(300), false));
    }

    #[test]
    fn a_takeover_is_not_taken_back_twice() {
        assert!(!same_gesture(Duration::from_millis(100), true));
    }

    #[test]
    fn a_late_takeover_is_someone_else_copying() {
        // Past the gesture, the clipboard is being used by the user elsewhere.
        assert!(!same_gesture(Duration::from_secs(3), false));
    }

    #[test]
    fn a_frozen_primary_yields_to_the_clipboard_the_app_wrote() {
        // The field bug, in one line. A terminal doing its own selection leaves
        // PRIMARY at whatever it held; the app publishes the real selection to
        // CLIPBOARD. The old rule read "PRIMARY still holds the text we carry"
        // as a deliberate re-selection (`is_new_selection` says true because
        // `owned == seen`), re-armed the stale text, and overwrote the app's
        // write with it. Every paste after that was an older copy.
        let stale = "second machine to test the multi-monito";
        let fresh = "  a completely different line the user just highlighted\n";
        let baseline = Some(hash(stale));

        assert!(is_new_selection(
            hash(stale),
            baseline,
            Some(hash(stale)),
            true
        ));
        // ...which is why PRIMARY must be judged by movement alone.
        assert!(!primary_moved(hash(stale), baseline));
        // ...and why the stream that moved gets asked before the rule above.
        assert!(clipboard_is_news(
            hash(fresh),
            Some(hash(stale)),
            Some(hash(stale))
        ));
    }

    #[test]
    fn our_own_clipboard_write_is_not_news() {
        // copy_on_drag writes what it armed, so the next drag sees its own text
        // on the clipboard. That is not a selection, and treating it as one
        // would re-arm the same text forever without a real drag.
        let ours = "what we armed last time";
        assert!(!clipboard_is_news(
            hash(ours),
            Some(hash(ours)),
            Some(hash(ours))
        ));
        // A clipboard that was taken from us between gestures is a different
        // matter: we do not own what is there, so a coincidental match on the
        // baseline is not proof that nothing happened.
        assert!(clipboard_is_news(
            hash("someone else's copy"),
            Some(hash("stale baseline")),
            Some(hash(ours))
        ));
    }

    #[test]
    fn a_moved_primary_still_wins() {
        // PRIMARY moving is authoritative: browsers and GTK apps put the real
        // selection there, and it is the precise region the user dragged.
        assert!(primary_moved(
            hash("the new words"),
            Some(hash("the old words"))
        ));
        // First selection of a session has no baseline at all.
        assert!(primary_moved(hash("anything"), None));
    }

    #[test]
    fn a_flaky_keyboard_copy_differs_from_what_we_carry() {
        // A ghost Ctrl+C shows up here as "the clipboard moved". It is
        // indistinguishable from the app's own write, which is why the log line
        // names the byte counts: see the disarm path.
        assert!(clipboard_is_news(
            hash("a stranger's copy"),
            Some(hash("what we armed")),
            Some(hash("what we armed"))
        ));
    }

    #[test]
    fn a_longer_copy_of_the_arm_is_the_same_selection() {
        // The TUI copied the whole line; we hold the phrase inside it.
        assert!(looks_like_same_selection(
            "  second machine to test the multi-monitor and second-compositor paths\n",
            "second machine to test the multi-monito"
        ));
    }

    #[test]
    fn a_shorter_copy_of_the_arm_is_the_same_selection() {
        assert!(looks_like_same_selection(
            "multi-monitor",
            "second machine to test the multi-monitor"
        ));
    }

    #[test]
    fn different_words_are_a_new_selection() {
        // Not a re-serialisation of ours: the app published something else, and
        // taking the clipboard back would paste the stale arm over it.
        assert!(!looks_like_same_selection(
            "the other thing entirely",
            "second machine to test the multi-monitor"
        ));
    }

    #[test]
    fn spacing_case_and_punctuation_do_not_make_it_new() {
        assert!(looks_like_same_selection(
            "Second  Machine, to test.",
            "second machine to test"
        ));
    }

    #[test]
    fn a_tiny_arm_is_never_matched_by_containment() {
        // "of" is inside half the lines on screen; containment there is not
        // evidence that the app re-published anything of ours.
        assert!(!looks_like_same_selection("of the thing", "of"));
        assert!(looks_like_same_selection("of", "of"));
    }

    #[test]
    fn empty_text_is_never_the_same_selection() {
        assert!(!looks_like_same_selection("", "second machine"));
        assert!(!looks_like_same_selection("second machine", ""));
    }

    #[test]
    fn a_stale_primary_does_not_turn_a_confirm_into_a_drag() {
        // A browser had already put its selection on PRIMARY before the press,
        // so the click did not select it — the arm-time baseline just never saw
        // it, because the arm came from a copy click on CLIPBOARD.
        let own = hash("the copied 15 bytes");
        let stale = Some(hash("a page selection"));
        let clip = Fake::new(Some("a page selection"), Some("the copied 15 bytes"));
        assert!(!release_left_a_selection(&clip, stale, stale, Some(own)));
    }

    #[test]
    fn a_change_after_the_press_is_still_a_selection() {
        let clip = Fake::new(Some("newly selected"), Some("x"));
        let before = Some(hash("what primary held at the press"));
        assert!(release_left_a_selection(&clip, before, None, None));
    }

    #[test]
    fn the_paste_puts_the_arm_back_on_the_clipboard() {
        // The app re-published its own idea of the selection after our write.
        let clip = Fake::new(None, Some("something the app wrote"));
        let sel = core::Selection {
            hash: hash("Cancel"),
            len: 6,
            source_app: None,
            text: "Cancel".to_string(),
        };
        assert!(reassert_for_paste(&clip, &sel));
        assert_eq!(clip.clipboard.borrow().as_deref(), Some("Cancel"));
    }

    #[test]
    fn a_copy_that_lands_after_the_click_is_taken() {
        // Copy glyph: clipboard was "old" when the click landed, "new" after.
        let clip = Fake::new(None, Some("new"));
        let got = copy_since_click(&clip, Some(hash("old")), None, None);
        assert_eq!(got, Some(("new".to_string(), "clipboard")));
        clip.set_clipboard("new").unwrap();
        assert_eq!(*clip.written.borrow(), vec!["new".to_string()]);
    }

    #[test]
    fn a_copy_that_was_already_there_before_the_click_is_refused() {
        // The click did not cause this; it was on the clipboard all along. This
        // is the whole reason the baseline is sampled at click time.
        let clip = Fake::new(None, Some("old"));
        assert_eq!(copy_since_click(&clip, Some(hash("old")), None, None), None);
    }

    #[test]
    fn a_click_that_collapses_a_selection_does_not_arm() {
        // PRIMARY cleared by the click itself: empty is not a copy, and the
        // baseline moved with it.
        let clip = Fake::new(None, Some("old"));
        assert_eq!(copy_since_click(&clip, Some(hash("old")), None, None), None);
        // An unchanged non-empty primary is not news either.
        let clip = Fake::new(Some("words"), Some("old"));
        assert_eq!(
            copy_since_click(&clip, Some(hash("old")), Some(hash("words")), None),
            None
        );
    }

    #[test]
    fn a_copy_we_already_carry_does_not_arm() {
        // Clicking the same copy glyph again: the clipboard moves, but not to
        // anything new, so there is nothing to paste.
        let clip = Fake::new(None, Some("same"));
        assert_eq!(
            copy_since_click(&clip, Some(hash("old")), None, Some(hash("same"))),
            None
        );
    }

    #[test]
    fn a_primary_only_copy_still_arms() {
        let clip = Fake::new(Some("picked"), Some("old"));
        assert_eq!(
            copy_since_click(&clip, Some(hash("old")), Some(hash("before")), None),
            Some(("picked".to_string(), "primary"))
        );
    }

    #[test]
    fn a_short_drag_that_selected_text_is_not_a_confirm() {
        // The whole reason this exists: apps select on any movement, our
        // threshold is coarser than the user's gesture. A release that left new
        // text on PRIMARY was selecting, and pasting over it would destroy the
        // selection the user just made.
        let clip = Fake::new(Some("new words"), Some("old"));
        assert!(release_left_a_selection(
            &clip,
            Some(hash("previous")),
            Some(hash("old")),
            Some(hash("carried"))
        ));
    }

    #[test]
    fn a_chromium_short_drag_counts_even_though_primary_never_moved() {
        // Chromium and Electron publish selections to CLIPBOARD only, so a
        // PRIMARY-only check would let a short drag in a browser paste.
        let clip = Fake::new(None, Some("browser selected this"));
        assert!(release_left_a_selection(
            &clip,
            Some(hash("previous")),
            Some(hash("old clip")),
            None
        ));
    }

    #[test]
    fn a_click_that_selected_nothing_is_still_a_confirm() {
        // Both streams unchanged: the click was clicking, not selecting.
        let clip = Fake::new(Some("previous"), Some("old"));
        assert!(!release_left_a_selection(
            &clip,
            Some(hash("previous")),
            Some(hash("old")),
            None
        ));
        // Nothing selected anywhere: also not a selection.
        let clip = Fake::new(None, None);
        assert!(!release_left_a_selection(&clip, None, None, None));
        // Text we already carry is not a new selection either.
        let clip = Fake::new(Some("carried"), Some("carried"));
        assert!(!release_left_a_selection(
            &clip,
            Some(hash("previous")),
            Some(hash("previous")),
            Some(hash("carried"))
        ));
    }

    #[test]
    fn copy_click_arms_only_when_configured() {
        // Default is on: the feature is the product, and an arm is visible.
        assert_eq!(Config::default().arm_on_copy, ArmOnCopy::Click);
        assert_eq!(Config::default().copy_click_window_ms, 600);
    }

    #[test]
    fn a_stale_primary_that_did_not_move_is_not_this_drags_selection() {
        // The field regression, exactly: PRIMARY already held 13 bytes when the
        // button went down, the drag selected nothing, and the daemon copied
        // that older text because it differed from the last thing it copied.
        let stale = hash("MC-MARKER-XYZ");
        let last_copy = hash("something we carried earlier");
        let at_press = AtPress {
            primary: Some(stale),
            clipboard: Some(last_copy),
            sampled: true,
        };
        assert!(!primary_moved(stale, at_press.primary));
        assert!(!is_new_selection(
            stale,
            at_press.primary,
            Some(last_copy),
            true
        ));
    }

    #[test]
    fn a_selection_published_during_the_gesture_is_still_taken() {
        let at_press = AtPress {
            primary: Some(hash("old")),
            clipboard: None,
            sampled: true,
        };
        assert!(primary_moved(hash("new"), at_press.primary));
        assert!(is_new_selection(hash("new"), at_press.primary, None, true));
    }

    #[test]
    fn a_copy_made_before_the_press_is_not_news() {
        // Ctrl+C, then a drag that selects nothing: the clipboard was already
        // holding this when the button went down, so the gesture did not make it.
        let copied_earlier = hash("ctrl+c text");
        assert!(!clipboard_is_news(
            copied_earlier,
            Some(copied_earlier),
            None
        ));
    }

    #[test]
    fn a_copy_published_during_the_gesture_is_news() {
        let at_press = AtPress {
            primary: None,
            clipboard: Some(hash("ctrl+c text")),
            sampled: true,
        };
        assert!(clipboard_is_news(
            hash("chromium selection"),
            at_press.clipboard,
            None
        ));
        // Our own write is never news, even though it changed the stream.
        let ours = hash("ours");
        assert!(!clipboard_is_news(ours, Some(ours), Some(ours)));
    }

    #[test]
    fn re_selecting_text_we_carry_still_arms() {
        // PRIMARY is frozen at the text we own and nothing else spoke: that is a
        // deliberate re-selection, and the arm is allowed to move on to it.
        let carried = hash("the same phrase");
        assert!(is_new_selection(
            carried,
            Some(carried),
            Some(carried),
            true
        ));
    }

    #[test]
    fn a_missing_snapshot_degrades_to_the_last_copy_and_says_so() {
        let legacy = AtPress {
            primary: Some(1),
            clipboard: Some(2),
            sampled: false,
        };
        assert_eq!(settle_baseline(None, legacy), legacy);
    }

    #[test]
    fn a_delivered_snapshot_is_used() {
        let (tx, rx) = mpsc::channel();
        tx.send((Some(7), None)).unwrap();
        let got = settle_baseline(Some(rx), AtPress::default());
        assert_eq!(
            got,
            AtPress {
                primary: Some(7),
                clipboard: None,
                sampled: true,
            }
        );
    }
}
