//! Raw input. Reads /dev/input directly, which is why this works the same under
//! X11 and Wayland: the compositor never has to cooperate.
//!
//! Requires membership in the `input` group. `noclickcopy doctor` checks that.

use anyhow::Result;
use evdev::{Device, EventType, KeyCode, RelativeAxisCode};
use std::sync::mpsc::Sender;

/// Anything the rest of the program cares about coming out of the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// Left button pressed. The drag has not been classified yet.
    Press,
    /// Left button released after meaningful travel.
    Drag,
    /// Left button released without travel.
    Click,
    /// The configured confirm key.
    Confirm,
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

pub fn spawn_readers(confirm_code: u16, threshold: i32, tx: Sender<Input>) -> Result<usize> {
    let mut spawned = 0;

    for (path, dev) in evdev::enumerate() {
        let name = dev.name().unwrap_or("").to_string();
        // Never read our own injected events back.
        if name.contains("noclickcopy") {
            continue;
        }

        let Some(keys) = dev.supported_keys() else {
            continue;
        };
        let is_pointer = keys.contains(KeyCode::BTN_LEFT);
        let is_keyboard = keys.contains(KeyCode::KEY_A);

        if is_pointer {
            let (tx, path) = (tx.clone(), path.clone());
            std::thread::spawn(move || pointer_loop(dev, path, threshold, tx));
            spawned += 1;
        } else if is_keyboard {
            let (tx, path) = (tx.clone(), path.clone());
            std::thread::spawn(move || keyboard_loop(dev, path, confirm_code, tx));
            spawned += 1;
        }
    }

    Ok(spawned)
}

/// (pointers, keyboards) that we would be able to read. Used by doctor.
pub fn count_devices() -> (usize, usize) {
    let (mut p, mut k) = (0, 0);
    for (_, dev) in evdev::enumerate() {
        if dev.name().unwrap_or("").contains("noclickcopy") {
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

fn pointer_loop(mut dev: Device, path: std::path::PathBuf, threshold: i32, tx: Sender<Input>) {
    let mut pressed = false;
    let mut travel = 0i32;

    loop {
        let events = match dev.fetch_events() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("noclickcopy: {} closed: {e}", path.display());
                return;
            }
        };

        for ev in events {
            match ev.event_type() {
                EventType::KEY => {
                    if ev.code() == KeyCode::BTN_LEFT.code() {
                        match ev.value() {
                            1 => {
                                pressed = true;
                                travel = 0;
                                if tx.send(Input::Press).is_err() {
                                    return;
                                }
                            }
                            0 if pressed => {
                                pressed = false;
                                let msg = if travel >= threshold {
                                    Input::Drag
                                } else {
                                    Input::Click
                                };
                                if tx.send(msg).is_err() {
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

fn keyboard_loop(mut dev: Device, path: std::path::PathBuf, confirm_code: u16, tx: Sender<Input>) {
    loop {
        let events = match dev.fetch_events() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("noclickcopy: {} closed: {e}", path.display());
                return;
            }
        };

        for ev in events {
            if ev.event_type() != EventType::KEY || ev.value() != 1 {
                continue;
            }
            let code = ev.code();
            let msg = if code == confirm_code {
                Input::Confirm
            } else if is_modifier(code) {
                continue;
            } else {
                Input::KeyCode
            };
            if tx.send(msg).is_err() {
                return;
            }
        }
    }
}
