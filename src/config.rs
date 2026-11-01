use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

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
    /// Whether a click on its own confirms an arm, or only `confirm_key` does.
    ///
    /// A click is a good confirm when it lands on a text field, and a bad one
    /// when it lands on chrome that cannot be told apart from a field: a tab
    /// strip, a "+" button, a toolbar. Linux offers no reliable veto for that
    /// without an accessibility tree, so turning this off makes the confirm
    /// deliberate instead of inferred.
    pub confirm_on_click: bool,
    /// Chord injected in "paste" mode.
    pub paste_chord: String,
    /// Copy drag selections to the clipboard automatically.
    pub copy_on_drag: bool,
    /// Arm from a click that caused a copy: copy glyphs, "Copy link address".
    pub arm_on_copy: ArmOnCopy,
    /// How long after a click a copy still counts as caused by that click.
    pub copy_click_window_ms: u64,
    /// Two clicks closer together than this are one multi-click. An armed
    /// confirm is also held this long, so a double-click selects instead of
    /// pasting over the word it highlights.
    pub multi_click_ms: u64,
    /// Refuse to carry selections larger than this. Guards pastejacking-ish mistakes.
    pub max_bytes: usize,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ConfirmAction {
    Observe,
    Paste,
}

/// Whether an arm can come from something other than a drag.
///
/// "click" is the copy-glyph case: a click that changes the clipboard we did not
/// write is a copy someone asked for, so it arms the same way a drag does.
#[derive(Debug, Deserialize, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ArmOnCopy {
    Off,
    Click,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            arm_timeout_ms: 30_000,
            drag_threshold: 40,
            confirm_key: "KEY_F13".into(),
            confirm_action: ConfirmAction::Observe,
            confirm_on_click: true,
            paste_chord: "auto".into(),
            copy_on_drag: true,
            arm_on_copy: ArmOnCopy::Click,
            copy_click_window_ms: 600,
            multi_click_ms: 300,
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
        resolve(&base)
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

/// New location first, then the pre-rename one.
///
/// A config you tuned is worth more than a tidy path: renaming the binary must
/// not quietly reset `confirm_action` back to `observe`, or drop the key your
/// thumb button is bound to. The new path wins once it exists, so a deliberate
/// `config.toml` under `magiclick/` retires the old one.
fn resolve(base: &Path) -> PathBuf {
    let current = base.join("magiclick").join("config.toml");
    if current.exists() {
        return current;
    }
    let legacy = base.join("noclickcopy").join("config.toml");
    if legacy.exists() {
        return legacy;
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    /// Isolated base dir; these never touch the real `~/.config`.
    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("magiclick-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn falls_back_to_the_pre_rename_path() {
        let base = scratch("legacy");
        write(&base.join("noclickcopy").join("config.toml"));
        assert_eq!(resolve(&base), base.join("noclickcopy").join("config.toml"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_new_path_wins_when_both_exist() {
        let base = scratch("both");
        write(&base.join("noclickcopy").join("config.toml"));
        write(&base.join("magiclick").join("config.toml"));
        assert_eq!(resolve(&base), base.join("magiclick").join("config.toml"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_click_confirms_unless_it_is_turned_off() {
        let on: Config = toml::from_str("").unwrap();
        assert!(on.confirm_on_click, "click-to-paste is the default");
        let off: Config = toml::from_str("confirm_on_click = false").unwrap();
        assert!(!off.confirm_on_click);
    }

    #[test]
    fn neither_path_reports_the_new_one() {
        let base = scratch("none");
        assert_eq!(resolve(&base), base.join("magiclick").join("config.toml"));
        let _ = std::fs::remove_dir_all(&base);
    }
}
