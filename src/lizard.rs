//! Mouse-suppression heartbeat: holds "lizard mode" off against the firmware
//! watchdog.

use std::time::{Duration, Instant};

use crate::link::Link;

/// How often to resend lizard-OFF while suppressing the mouse. The controller's
/// watchdog re-enables the phantom mouse within a few seconds (measured ~8s over
/// BLE), so the old 3s cadence lost the race and the mouse crept back; 250ms holds
/// it down solidly (validated on-device) and is negligible traffic.
const LIZARD_REASSERT: Duration = Duration::from_millis(250);

/// Holds the phantom mouse/keyboard ("lizard mode") off against the controller's
/// watchdog, which re-enables it within a few seconds (and faster over BLE than
/// SDL's 3s cadence): while suppressing, OFF is resent every `LIZARD_REASSERT`.
/// Turning the mouse back ON = stop resending (the watchdog restores it) plus
/// one explicit ON so it doesn't lag ~8s.
pub struct Lizard {
    suppress: bool,
    last_sent: Option<Instant>,
}

impl Lizard {
    pub fn new() -> Lizard {
        Lizard { suppress: true, last_sent: None }
    }

    /// Suppress the mouse, resending OFF on the very next tick.
    pub fn suppress_now(&mut self) {
        self.suppress = true;
        self.reassert();
    }

    /// Resend OFF on the next tick without changing intent (fresh link after
    /// replug — the new controller session starts with lizard on).
    pub fn reassert(&mut self) {
        self.last_sent = None;
    }

    /// Hand the mouse back immediately (Steam mode: native state, Steam owns it).
    pub fn release(&mut self, link: &Link) {
        self.suppress = false;
        link.lizard(true);
    }

    /// Flip mouse suppression; returns true if the mouse is now on.
    pub fn toggle(&mut self, link: &Link) -> bool {
        self.suppress = !self.suppress;
        if self.suppress {
            self.reassert();
        } else {
            link.lizard(true); // the watchdog would restore it, but lags ~8s
        }
        !self.suppress
    }

    /// The heartbeat: while suppressing with a live stream, resend OFF every
    /// `LIZARD_REASSERT`. Parked automatically when nothing streams.
    pub fn tick(&mut self, link: &Link) {
        if !self.suppress || !link.is_streaming() {
            return;
        }
        if self.last_sent.is_none_or(|t| t.elapsed() >= LIZARD_REASSERT) {
            link.lizard(false);
            self.last_sent = Some(Instant::now());
        }
    }
}
