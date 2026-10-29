use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    /// How long an arm survives before it is dropped. Rule 5.
    pub arm_timeout_ms: u64,
    /// Movement (in device units) beyond which a button press counts as a drag.
    pub drag_threshold: i32,
    /// Key that confirms a paste. Bind your thumb button / foot pedal to this.
    pub confirm_key: String,
    /// What a confirm does: "observe" (log only, zero risk) or "paste".
    pub confirm_action: ConfirmAction,
    /// Chord injected in "paste" mode.
    pub paste_chord: String,
    /// Copy drag selections to the clipboard automatically.
    pub copy_on_drag: bool,
    /// Refuse to carry selections larger than this. Guards pastejacking-ish mistakes.
    pub max_bytes: usize,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ConfirmAction {
    Observe,
    Paste,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            arm_timeout_ms: 10_000,
            drag_threshold: 40,
            confirm_key: "KEY_F13".into(),
            confirm_action: ConfirmAction::Observe,
            paste_chord: "super+v".into(),
            copy_on_drag: true,
            max_bytes: 64 * 1024,
        }
    }
}

impl Config {
    pub fn path() -> PathBuf {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
            });
        base.join("noclickcopy").join("config.toml")
    }

    pub fn load() -> Result<Self> {
        let path = Self::path();
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))
    }
}
