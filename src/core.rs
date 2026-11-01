//! The whole product, platform-free and unit-testable.
//!
//! Five rules, in order of authority:
//!   1. arm on drag-select (a repeat of the pending arm does not extend it)
//!   2. fire only on an explicit confirm *after* a click has placed the caret
//!   3. one shot, then disarm
//!   4. anything else disarms (fail closed)
//!   5. stale for `timeout`, disarm
//!
//! No platform code lives here on purpose. Every OS adapter feeds these events.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub hash: u64,
    pub len: usize,
    pub source_app: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Armed {
    pub since: Instant,
    pub selection: Selection,
    /// True once the user clicks, which is what places the caret.
    pub caret_placed: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum State {
    Disarmed,
    Armed(Armed),
}

#[derive(Debug, Clone)]
pub enum Event {
    /// A drag finished and the selection changed.
    DragCopied(Selection),
    /// Left click landed. Cheap, frequent, only meaningful while armed.
    Click,
    /// The confirm gesture (thumb button, foot pedal, chord).
    Confirm,
    /// Any input that is not a click or a confirm: typing, scrolling, another drag.
    Interference(&'static str),
    /// Wrapper changed, or something else took the clipboard.
    ClipboardMoved,
    /// Periodic tick, carries now.
    Tick(Instant),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing to do.
    None,
    /// Just armed. Show it.
    Armed,
    /// Paste the held selection now.
    Fire,
    /// Disarmed; the reason is for the log and the bar icon tooltip.
    Disarmed(&'static str),
}

pub struct Router {
    state: State,
    timeout: Duration,
    last_hash: Option<u64>,
}

impl Router {
    pub fn new(timeout: Duration) -> Self {
        Self {
            state: State::Disarmed,
            timeout,
            last_hash: None,
        }
    }

    /// Only the tests need to look inside a router; production code asks
    /// `handle` for an outcome instead.
    #[cfg(test)]
    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn is_armed(&self) -> bool {
        matches!(self.state, State::Armed(_))
    }

    pub fn handle(&mut self, event: Event) -> Outcome {
        match event {
            // Rule 1. Re-arming on a new drag is correct: the newest copy wins.
            Event::DragCopied(sel) => {
                // Same text with an arm still pending: no new information, so
                // leave that arm alone rather than restarting its clock. With
                // nothing pending it is a fresh intent — you copied this,
                // pasted it, and came back for more — and has to arm again.
                // Browsers keep the previous primary selection alive across
                // clicks, so re-selecting the same phrase reports identical
                // text; refusing it made the tool look dead.
                if self.last_hash == Some(sel.hash) && self.is_armed() {
                    return Outcome::None;
                }
                self.last_hash = Some(sel.hash);
                self.state = State::Armed(Armed {
                    since: Instant::now(),
                    selection: sel,
                    caret_placed: false,
                });
                Outcome::Armed
            }

            Event::Click => match &mut self.state {
                State::Armed(a) => {
                    a.caret_placed = true;
                    self.state = State::Disarmed;
                    Outcome::Fire
                }
                State::Disarmed => Outcome::None,
            },

            // Rule 2 and 3.
            Event::Confirm => match &self.state {
                State::Armed(a) if a.caret_placed => {
                    self.state = State::Disarmed;
                    Outcome::Fire
                }
                // Confirm without a click: the caret is still wherever it was.
                // Firing here is how a paste lands mid-word in someone else's
                // document, so it is a no-op by design.
                State::Armed(_) => Outcome::None,
                State::Disarmed => Outcome::None,
            },

            // Rule 4.
            Event::Interference(reason) => {
                if self.is_armed() {
                    self.state = State::Disarmed;
                    Outcome::Disarmed(reason)
                } else {
                    Outcome::None
                }
            }

            Event::ClipboardMoved => {
                if self.is_armed() {
                    self.state = State::Disarmed;
                    Outcome::Disarmed("clipboard changed")
                } else {
                    Outcome::None
                }
            }

            // Rule 5.
            Event::Tick(now) => match &self.state {
                State::Armed(a) if now.saturating_duration_since(a.since) >= self.timeout => {
                    self.state = State::Disarmed;
                    Outcome::Disarmed("timed out")
                }
                _ => Outcome::None,
            },
        }
    }

    /// Change rule 5's window without disturbing an arm already in flight.
    /// Applies at the next tick, so shortening it can disarm immediately.
    pub fn set_timeout(&mut self, timeout: Duration) {
        self.timeout = timeout;
    }

    /// The text to paste, if we are armed. `None` after firing.
    pub fn armed_selection(&self) -> Option<&Selection> {
        match &self.state {
            State::Armed(a) => Some(&a.selection),
            State::Disarmed => None,
        }
    }
}

/// A selection ending in a newline means "this line", not "and a blank line
/// after it". `wl-paste -n` and `wl-copy -n` both leave the bytes alone, so
/// the strip has to happen here, once, before anything is hashed or stored.
pub fn trim_selection(text: &str) -> &str {
    let t = text.strip_suffix('\n').unwrap_or(text);
    t.strip_suffix('\r').unwrap_or(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(n: u64) -> Selection {
        Selection {
            hash: n,
            len: 3,
            source_app: Some("foot".into()),
            text: "abc".into(),
        }
    }

    fn armed(timeout_ms: u64) -> Router {
        let mut r = Router::new(Duration::from_millis(timeout_ms));
        assert_eq!(r.handle(Event::DragCopied(sel(1))), Outcome::Armed);
        r
    }

    #[test]
    fn rule_1_arms_on_drag() {
        assert!(armed(10_000).is_armed());
    }

    #[test]
    fn rule_1_same_text_twice_does_not_rearm() {
        let mut r = armed(10_000);
        assert_eq!(r.handle(Event::DragCopied(sel(1))), Outcome::None);
        assert!(r.is_armed());
    }

    #[test]
    fn rule_1_new_text_rearms() {
        let mut r = armed(10_000);
        assert_eq!(r.handle(Event::DragCopied(sel(2))), Outcome::Armed);
    }

    #[test]
    fn rule_1_same_text_arms_again_once_nothing_is_pending() {
        let mut r = armed(10_000);
        assert_eq!(r.handle(Event::Click), Outcome::Fire);
        // Chromium keeps primary alive, so copying the same phrase twice in a
        // row arrives as the same hash. That is a new intent, not a repeat.
        assert_eq!(r.handle(Event::DragCopied(sel(1))), Outcome::Armed);
    }

    #[test]
    fn rule_1_same_text_arms_again_after_it_expired() {
        let mut r = armed(1_000);
        let start = match r.state() {
            State::Armed(a) => a.since,
            _ => unreachable!(),
        };
        r.handle(Event::Tick(start + Duration::from_millis(1_000)));
        assert!(!r.is_armed());
        assert_eq!(r.handle(Event::DragCopied(sel(1))), Outcome::Armed);
    }

    #[test]
    fn rule_2_click_fires_when_armed() {
        let mut r = armed(10_000);
        // Click-to-paste: the click itself is the confirm.
        assert_eq!(r.handle(Event::Click), Outcome::Fire);
        // One shot: subsequent confirm is a no-op because disarmed.
        assert_eq!(r.handle(Event::Confirm), Outcome::None);
    }

    #[test]
    fn rule_2_confirm_without_click_is_a_noop() {
        let mut r = armed(10_000);
        assert_eq!(r.handle(Event::Confirm), Outcome::None);
        assert!(r.is_armed(), "a stray confirm must not consume the arm");
    }

    #[test]
    fn rule_3_one_shot() {
        let mut r = armed(10_000);
        assert_eq!(r.handle(Event::Click), Outcome::Fire);
        assert!(!r.is_armed());
        assert_eq!(r.handle(Event::Confirm), Outcome::None, "no double paste");
        assert!(r.armed_selection().is_none());
    }

    #[test]
    fn rule_4_typing_disarms() {
        let mut r = armed(10_000);
        assert_eq!(
            r.handle(Event::Interference("typed")),
            Outcome::Disarmed("typed")
        );
        assert!(!r.is_armed());
    }

    #[test]
    fn rule_4_interference_while_disarmed_is_quiet() {
        let mut r = Router::new(Duration::from_secs(10));
        assert_eq!(r.handle(Event::Interference("typed")), Outcome::None);
    }

    #[test]
    fn rule_4_foreign_clipboard_change_disarms() {
        let mut r = armed(10_000);
        assert_eq!(
            r.handle(Event::ClipboardMoved),
            Outcome::Disarmed("clipboard changed")
        );
    }

    #[test]
    fn rule_5_timeout() {
        let mut r = armed(1_000);
        let start = match r.state() {
            State::Armed(a) => a.since,
            _ => unreachable!(),
        };
        assert_eq!(
            r.handle(Event::Tick(start + Duration::from_millis(999))),
            Outcome::None
        );
        assert_eq!(
            r.handle(Event::Tick(start + Duration::from_millis(1_000))),
            Outcome::Disarmed("timed out")
        );
        assert!(!r.is_armed());
    }

    #[test]
    fn longer_timeout_keeps_an_arm_that_would_have_expired() {
        let mut r = armed(1_000);
        let start = match r.state() {
            State::Armed(a) => a.since,
            _ => unreachable!(),
        };
        r.set_timeout(Duration::from_millis(4_000));
        assert_eq!(
            r.handle(Event::Tick(start + Duration::from_millis(3_999))),
            Outcome::None
        );
        assert!(r.is_armed());
    }

    #[test]
    fn shorter_timeout_applies_to_an_arm_already_in_flight() {
        let mut r = armed(10_000);
        let start = match r.state() {
            State::Armed(a) => a.since,
            _ => unreachable!(),
        };
        r.set_timeout(Duration::from_millis(2_000));
        assert_eq!(
            r.handle(Event::Tick(start + Duration::from_millis(2_000))),
            Outcome::Disarmed("timed out")
        );
    }

    #[test]
    fn a_line_selection_loses_one_trailing_line_ending() {
        assert_eq!(trim_selection("echo test\n"), "echo test");
        assert_eq!(trim_selection("echo test\r\n"), "echo test");
        assert_eq!(trim_selection("one\ntwo\n"), "one\ntwo");
        assert_eq!(trim_selection("\n"), "");
        // Interior newlines and ordinary whitespace are content, not terminator.
        assert_eq!(trim_selection("one\ntwo"), "one\ntwo");
        assert_eq!(trim_selection(" padded "), " padded ");
    }

    #[test]
    fn click_while_disarmed_is_quiet() {
        let mut r = Router::new(Duration::from_secs(10));
        assert_eq!(r.handle(Event::Click), Outcome::None);
        assert_eq!(r.handle(Event::Confirm), Outcome::None);
    }
}
