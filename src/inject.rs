//! Keystroke injection via /dev/uinput — no ydotool, no daemon, no root.
//! Works under X11 and Wayland alike because it enters below the compositor.

use anyhow::{bail, Context, Result};
use evdev::uinput::VirtualDevice;
use evdev::{AttributeSet, EventType, InputEvent, KeyCode};
use std::time::Duration;

pub struct Injector {
    dev: VirtualDevice,
}

fn modifier(name: &str) -> Option<KeyCode> {
    match name {
        "ctrl" | "control" => Some(KeyCode::KEY_LEFTCTRL),
        "super" | "meta" | "win" => Some(KeyCode::KEY_LEFTMETA),
        "alt" => Some(KeyCode::KEY_LEFTALT),
        "shift" => Some(KeyCode::KEY_LEFTSHIFT),
        _ => None,
    }
}

fn letter(name: &str) -> Option<KeyCode> {
    let mut chars = name.chars();
    let c = chars.next()?;
    if chars.next().is_some() {
        return None;
    }
    let code = match c.to_ascii_lowercase() {
        'v' => KeyCode::KEY_V,
        'c' => KeyCode::KEY_C,
        'x' => KeyCode::KEY_X,
        'a' => KeyCode::KEY_A,
        'z' => KeyCode::KEY_Z,
        _ => return None,
    };
    Some(code)
}

impl Injector {
    pub fn new() -> Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for k in [
            KeyCode::KEY_LEFTCTRL,
            KeyCode::KEY_LEFTMETA,
            KeyCode::KEY_LEFTALT,
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_V,
            KeyCode::KEY_C,
            KeyCode::KEY_X,
            KeyCode::KEY_A,
            KeyCode::KEY_Z,
        ] {
            keys.insert(k);
        }
        let dev = VirtualDevice::builder()
            .context("opening /dev/uinput")?
            .name("noclickcopy-virtual")
            .with_keys(&keys)
            .context("declaring keys")?
            .build()
            .context("creating virtual keyboard")?;
        Ok(Self { dev })
    }

    fn tap(&mut self, key: KeyCode) -> Result<()> {
        let code = key.code();
        self.dev.emit(&[
            InputEvent::new(EventType::KEY.0, code, 1),
            InputEvent::new(EventType::KEY.0, code, 0),
        ])?;
        Ok(())
    }

    /// Fire a chord like "super+v". Modifiers are held across the keypress so
    /// the target sees a real shortcut, not three separate events.
    pub fn chord(&mut self, chord: &str) -> Result<()> {
        let parts: Vec<&str> = chord.split('+').map(str::trim).collect();
        let (last, mods) = parts.split_last().context("empty chord")?;

        let key = letter(last).with_context(|| format!("unsupported key in chord: {last}"))?;
        let held: Vec<KeyCode> = mods
            .iter()
            .map(|m| modifier(m).with_context(|| format!("unknown modifier: {m}")))
            .collect::<Result<_>>()?;

        for m in &held {
            self.dev
                .emit(&[InputEvent::new(EventType::KEY.0, m.code(), 1)])?;
        }
        std::thread::sleep(Duration::from_millis(15));
        self.tap(key)?;
        std::thread::sleep(Duration::from_millis(15));
        for m in held.iter().rev() {
            self.dev
                .emit(&[InputEvent::new(EventType::KEY.0, m.code(), 0)])?;
        }
        Ok(())
    }
}

/// Does the kernel let us create a virtual device at all?
pub fn available() -> bool {
    std::path::Path::new("/dev/uinput").exists()
        && std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/uinput")
            .is_ok()
}

#[allow(dead_code)]
pub fn require() -> Result<()> {
    if !std::path::Path::new("/dev/uinput").exists() {
        bail!("/dev/uinput missing — load the uinput module");
    }
    Ok(())
}
