//! Raw input. Reads /dev/input directly, which is why this works the same under
//! X11 and Wayland: the compositor never has to cooperate.
//!
//! Requires membership in the `input` group. `magiclick doctor` checks that.

use anyhow::Result;
use evdev::{Device, EventType, KeyCode, RelativeAxisCode};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Anything the rest of the program cares about coming out of the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// Left button pressed. The drag has not been classified yet.
    Press,
    /// Left button released after meaningful travel.
    Drag,
    /// Left button released without travel. Carries how far the pointer moved,
    /// so a "click" that was really a short drag can be told apart later.
    Click(u32),
    /// The configured confirm key.
    Confirm,
    /// Right button: "never mind". The cheapest way to throw an arm away.
    Cancel,
    /// Some other key, i.e. the user is doing something else now.
    KeyCode,
}

/// Keys that never mean "the user moved on" — holding shift mid-drag is normal.
fn is_modifier(code: u16) -> bool {
    matches!(
        KeyCode::new(code),
        KeyCode::KEY_LEFTSHIFT
            | KeyCode::KEY_RIGHTSHIFT
            | KeyCode::KEY_LEFTCTRL
            | KeyCode::KEY_RIGHTCTRL
            | KeyCode::KEY_LEFTALT
            | KeyCode::KEY_RIGHTALT
            | KeyCode::KEY_LEFTMETA
            | KeyCode::KEY_RIGHTMETA
            | KeyCode::KEY_CAPSLOCK
            | KeyCode::KEY_NUMLOCK
    )
}

/// Attach a reader for every device we can use, and keep doing it.
///
/// Devices are not permanent: unplugging a mouse, docking, or resuming from
/// suspend destroys `/dev/input/eventN` and hands out a new number. A reader
/// that dies must be replaced, or the daemon goes quietly deaf.
pub fn spawn_readers(confirm_code: u16, threshold: i32, tx: Sender<Input>) -> Result<usize> {
    let reg: Arc<Mutex<Registry>> = Arc::new(Mutex::new(Registry::default()));
    let stop = Arc::new(AtomicBool::new(false));

    let first = scan(&reg, &stop, confirm_code, threshold, &tx);

    let (reg2, stop2, tx2) = (reg.clone(), stop.clone(), tx.clone());
    std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_secs(3));
            if stop2.load(Ordering::Relaxed) {
                break;
            }
            let n = scan(&reg2, &stop2, confirm_code, threshold, &tx2);
            if n > 0 {
                eprintln!("magiclick: re-attached {n} input device(s)");
            }
        }
    });

    Ok(first)
}

/// Which devices we hold, and which have died in a way that re-opening the
/// same node will not fix. Keyed by the node's inode: a device that really
/// came back has a new one.
#[derive(Default)]
struct Registry {
    live: HashMap<PathBuf, u64>,
    dead: HashMap<PathBuf, u64>,
}

fn inode(path: &PathBuf) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).map(|m| m.ino()).unwrap_or(0)
}

fn scan(
    reg: &Arc<Mutex<Registry>>,
    stop: &Arc<AtomicBool>,
    confirm_code: u16,
    threshold: i32,
    tx: &Sender<Input>,
) -> usize {
    let mut spawned = 0;

    for (path, dev) in evdev::enumerate() {
        let ino = inode(&path);
        {
            let Ok(r) = reg.lock() else { continue };
            if r.live.contains_key(&path) {
                continue;
            }
            // Same node, still dead: re-reading it just burns CPU and log.
            if r.dead.get(&path) == Some(&ino) {
                continue;
            }
        }
        let name = dev.name().unwrap_or("").to_string();
        // Never read our own injected events back.
        if name.contains("magiclick") {
            continue;
        }

        let Some(keys) = dev.supported_keys() else {
            continue;
        };
        let is_pointer = keys.contains(KeyCode::BTN_LEFT);
        let is_keyboard = keys.contains(KeyCode::KEY_A);
        if !is_pointer && !is_keyboard {
            continue;
        }

        if let Ok(mut r) = reg.lock() {
            r.live.insert(path.clone(), ino);
            r.dead.remove(&path);
        }
        spawned += 1;

        let (tx, reg, stop) = (tx.clone(), reg.clone(), stop.clone());
        let p = path.clone();
        std::thread::spawn(move || {
            if is_pointer {
                pointer_loop(dev, p.clone(), threshold, tx, stop.clone());
            } else {
                keyboard_loop(dev, p.clone(), confirm_code, tx, stop.clone());
            }
            // Gone (unplugged, suspended, or the daemon is shutting down):
            // free the path and remember the inode so we do not thrash on a
            // device that is merely broken.
            if let Ok(mut r) = reg.lock() {
                if let Some(ino) = r.live.remove(&p) {
                    r.dead.insert(p, ino);
                }
            }
        });
    }

    spawned
}

/// (pointers, keyboards) that we would be able to read. Used by doctor.
pub fn count_devices() -> (usize, usize) {
    let (mut p, mut k) = (0, 0);
    for (_, dev) in evdev::enumerate() {
        if dev.name().unwrap_or("").contains("magiclick") {
            continue;
        }
        let Some(keys) = dev.supported_keys() else {
            continue;
        };
        if keys.contains(KeyCode::BTN_LEFT) {
            p += 1;
        } else if keys.contains(KeyCode::KEY_A) {
            k += 1;
        }
    }
    (p, k)
}

fn pointer_loop(
    mut dev: Device,
    path: PathBuf,
    threshold: i32,
    tx: Sender<Input>,
    stop: Arc<AtomicBool>,
) {
    let mut pressed = false;
    let mut travel = 0i32;

    loop {
        let events = match dev.fetch_events() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("magiclick: {} closed: {e}", path.display());
                return;
            }
        };

        for ev in events {
            match ev.event_type() {
                EventType::KEY => {
                    // Right button is the universal "no": it cancels an armed
                    // selection without touching anything else. Cheaper and more
                    // obvious than waiting for the timeout.
                    if ev.code() == KeyCode::BTN_RIGHT.code() {
                        if ev.value() == 1 && tx.send(Input::Cancel).is_err() {
                            stop.store(true, Ordering::Relaxed);
                            return;
                        }
                        continue;
                    }
                    if ev.code() == KeyCode::BTN_LEFT.code() {
                        match ev.value() {
                            1 => {
                                pressed = true;
                                travel = 0;
                                if tx.send(Input::Press).is_err() {
                                    stop.store(true, Ordering::Relaxed);
                                    return;
                                }
                            }
                            0 if pressed => {
                                pressed = false;
                                let msg = if travel >= threshold {
                                    Input::Drag
                                } else {
                                    Input::Click(travel.max(0) as u32)
                                };
                                if tx.send(msg).is_err() {
                                    stop.store(true, Ordering::Relaxed);
                                    return;
                                }
                            }
                            _ => {}
                        }
                    }
                }
                EventType::RELATIVE if pressed => {
                    let code = RelativeAxisCode(ev.code());
                    if code == RelativeAxisCode::REL_X || code == RelativeAxisCode::REL_Y {
                        travel += ev.value().abs();
                    }
                }
                _ => {}
            }
        }
    }
}

fn keyboard_loop(
    mut dev: Device,
    path: PathBuf,
    confirm_code: u16,
    tx: Sender<Input>,
    stop: Arc<AtomicBool>,
) {
    // Modifiers held down mean "a shortcut", not "typing". Switching desktop
    // with Super+4 is navigation — the whole point of browsing for a target —
    // and must not cancel the arm.
    let mut held_mod = false;

    loop {
        let events = match dev.fetch_events() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("magiclick: {} closed: {e}", path.display());
                return;
            }
        };

        for ev in events {
            if ev.event_type() != EventType::KEY {
                continue;
            }
            let code = ev.code();
            let value = ev.value();

            if is_modifier(code) {
                match code {
                    // Shift alone is still typing ("Hello"), so only the
                    // command modifiers suppress the typing signal.
                    29 | 97 | 56 | 100 | 125 | 126 => held_mod = value != 0,
                    _ => {}
                }
                continue;
            }

            if value != 1 {
                continue;
            }
            if held_mod {
                continue;
            }

            let msg = if code == confirm_code {
                Input::Confirm
            } else {
                Input::KeyCode
            };
            if tx.send(msg).is_err() {
                stop.store(true, Ordering::Relaxed);
                return;
            }
        }
    }
}
