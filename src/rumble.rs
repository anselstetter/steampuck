//! Controller-side rumble latch, refreshed by the main loop.

use std::time::{Duration, Instant};

use crate::link::Link;

/// A rumble request decoded from the game's force-feedback traffic:
/// magnitudes plus an optional stop deadline (`None` = play until stopped).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct RumbleCmd {
    pub strong: u16,
    pub weak: u16,
    pub until: Option<Instant>,
}

impl RumbleCmd {
    /// Silence, no deadline.
    pub const ZERO: RumbleCmd = RumbleCmd { strong: 0, weak: 0, until: None };

    /// True if both motors are idle.
    pub fn is_silent(&self) -> bool {
        self.strong == 0 && self.weak == 0
    }
}

/// The controller-side rumble latch. The hardware auto-stops haptics after
/// ~50ms, so an active rumble must be refreshed every 40ms; `until` retires a
/// finite effect with an explicit stop (and ends the chord tick cleanly — the
/// auto-stop would let the low-freq motor thump 2-4 times).
pub struct Rumble {
    cur: RumbleCmd,
    last_sent: Instant,
}

impl Rumble {
    pub fn new() -> Rumble {
        Rumble { cur: RumbleCmd::ZERO, last_sent: Instant::now() }
    }

    /// Latch a command and send it immediately.
    pub fn set(&mut self, link: &Link, cmd: RumbleCmd) {
        self.cur = cmd;
        link.rumble(cmd.strong, cmd.weak);
        self.last_sent = Instant::now();
    }

    /// Zero the latch without sending (mode switch / controller gone).
    pub fn clear(&mut self) {
        self.cur = RumbleCmd::ZERO;
    }

    /// Keep an active rumble alive past the ~50ms hardware timeout and retire it
    /// when its deadline passes. openpuck scales the SC2's gentle motor by 200%
    /// here; add a scale if magnitudes still feel weak.
    pub fn tick(&mut self, link: &Link) {
        if self.cur.is_silent() {
            return;
        }
        if self.cur.until.is_some_and(|t| Instant::now() >= t) {
            self.clear();
            link.rumble(0, 0);
        } else if self.last_sent.elapsed() >= Duration::from_millis(40) {
            link.rumble(self.cur.strong, self.cur.weak);
            self.last_sent = Instant::now();
        }
    }
}
