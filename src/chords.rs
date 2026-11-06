//! Mode chords: user-configurable button combos (see [`crate::config`]),
//! debounced. A chord fires when ALL its buttons are held — extra pressed
//! buttons are ignored (exact matching would be unusable: the capacitive touch
//! bits are live whenever a finger rests on a pad). When chords overlap, the
//! one with more buttons wins.

use std::time::{Duration, Instant};

use steampuck::{Buttons, BUTTON_NAMES};

use crate::config::{self, ChordCfg, Feedback};
use crate::link::Sweep;

/// A chord must stay released this long before it can fire again. The face/paddle
/// switches bounce a few edges per tap; the read loop runs per report (~270Hz), so
/// without this a single tap fires 2-4 times. Well above contact-bounce (~10ms),
/// below any deliberate re-tap.
const CHORD_DEBOUNCE: Duration = Duration::from_millis(80);

/// Built-in chime palette, used when a chord's `chime` isn't configured.
/// Tuned on-device: rising = entering Xbox mode, falling = leaving to Steam.
const CHIME_XBOX: Sweep = Sweep { start_hz: 200, end_hz: 800, ms: 250, gain_db: 0 };
const CHIME_STEAM: Sweep = Sweep { start_hz: 800, end_hz: 200, ms: 250, gain_db: 0 };
const CHIME_BLIP: Sweep = Sweep { start_hz: 400, end_hz: 600, ms: 80, gain_db: 0 };

/// A system command fired by a button chord.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Chord {
    StartXbox,
    SteamMode,
    ToggleLizard,
}

/// One resolved chord: the button mask that must be held, the action it fires,
/// and the haptic confirmation to play.
#[derive(Clone, Copy)]
pub struct ChordSpec {
    mask: u32,
    pub action: Chord,
    pub feedback: Feedback,
    pub chime: Sweep,
}

/// Debounced chord detector: fires on a genuine rising edge, at most once per
/// press. Switches bounce a few edges per tap; we fire on the first, latch, and
/// re-arm only after every chord has stayed released for `CHORD_DEBOUNCE`.
pub struct Chords {
    specs: [ChordSpec; 3], // sorted most-buttons-first: the most specific wins
    armed: bool,
    absent_since: Option<Instant>,
}

impl Chords {
    /// Resolve the configured chords. Config problems warn and fall back to
    /// that chord's default — a bad chord must not take the daemon down.
    pub fn from_config(cfg: &config::ChordsCfg) -> Chords {
        let mut specs = [
            Self::spec(Chord::StartXbox, &cfg.xbox, config::DEFAULT_XBOX_BUTTONS, CHIME_XBOX),
            Self::spec(Chord::SteamMode, &cfg.steam, config::DEFAULT_STEAM_BUTTONS, CHIME_STEAM),
            Self::spec(Chord::ToggleLizard, &cfg.mouse, config::DEFAULT_MOUSE_BUTTONS, CHIME_BLIP),
        ];
        specs.sort_by_key(|s| std::cmp::Reverse(s.mask.count_ones()));
        for i in 0..specs.len() {
            for j in i + 1..specs.len() {
                if specs[i].mask == specs[j].mask {
                    eprintln!(
                        "config: chords {:?} and {:?} use the same buttons — {:?} wins",
                        specs[i].action, specs[j].action, specs[i].action
                    );
                }
            }
        }
        Chords { specs, armed: true, absent_since: None }
    }

    fn spec(action: Chord, cfg: &ChordCfg, defaults: &[&str], default_chime: Sweep) -> ChordSpec {
        let mask = Self::resolve(&cfg.buttons).unwrap_or_else(|bad| {
            eprintln!("config: {action:?} chord: unknown button {bad:?} — using default chord");
            Self::resolve(defaults).expect("built-in defaults resolve")
        });
        if mask.count_ones() < 2 {
            eprintln!("config: {action:?} chord has one button — will fire during normal play");
        }
        ChordSpec {
            mask,
            action,
            feedback: cfg.feedback,
            chime: cfg.chime.map(|c| c.sweep()).unwrap_or(default_chime),
        }
    }

    /// Button names (case-insensitive, per `BUTTON_NAMES`) -> combined mask.
    /// Err carries the first unknown name; an empty list is an error too.
    fn resolve<S: AsRef<str>>(names: &[S]) -> Result<u32, String> {
        let mut mask = 0u32;
        for n in names {
            let n = n.as_ref();
            match BUTTON_NAMES.iter().find(|(name, _)| name.eq_ignore_ascii_case(n)) {
                Some(&(_, bit)) => mask |= bit,
                None => return Err(n.to_string()),
            }
        }
        if mask == 0 { Err("(no buttons)".into()) } else { Ok(mask) }
    }

    /// Human-readable button list for `action`'s chord (startup banner).
    pub fn label(&self, action: Chord) -> String {
        let mask = self.spec_for(action).mask;
        BUTTON_NAMES
            .iter()
            .filter(|&&(_, bit)| mask & bit != 0)
            .map(|&(name, _)| name)
            .collect::<Vec<_>>()
            .join("+")
    }

    /// The spec for `action` (the reconnect announcement reuses StartXbox's).
    pub fn spec_for(&self, action: Chord) -> ChordSpec {
        *self.specs.iter().find(|s| s.action == action).expect("all actions have a spec")
    }

    /// The chord engaged *this frame*, by level, not edge — [`Chords::step`]
    /// debounces. Superset match; most-buttons-first order settles overlaps.
    fn chord_of(&self, cur: Buttons) -> Option<ChordSpec> {
        self.specs.iter().find(|s| cur.0 & s.mask == s.mask).copied()
    }

    /// Feed one frame of buttons; returns the chord to *act on*, `Some` only on
    /// the debounced rising edge. `now` is injected for tests.
    pub fn step(&mut self, cur: Buttons, now: Instant) -> Option<ChordSpec> {
        match self.chord_of(cur) {
            Some(spec) => {
                self.absent_since = None;
                if self.armed {
                    self.armed = false;
                    Some(spec)
                } else {
                    None // held or bouncing after the edge we already fired
                }
            }
            None => {
                let since = *self.absent_since.get_or_insert(now);
                if now.duration_since(since) >= CHORD_DEBOUNCE {
                    self.armed = true;
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All four back paddles — the default chords' base.
    const BASE: u32 = Buttons::L4 | Buttons::L5 | Buttons::R4 | Buttons::R5;

    fn b(mask: u32) -> Buttons {
        Buttons(mask)
    }

    fn defaults() -> Chords {
        Chords::from_config(&config::ChordsCfg::default())
    }

    fn action_of(spec: Option<ChordSpec>) -> Option<Chord> {
        spec.map(|s| s.action)
    }

    #[test]
    fn default_chords_map_each_face_with_all_paddles() {
        let ch = defaults();
        assert_eq!(action_of(ch.chord_of(b(BASE | Buttons::X))), Some(Chord::StartXbox));
        assert_eq!(action_of(ch.chord_of(b(BASE | Buttons::A))), Some(Chord::SteamMode));
        assert_eq!(action_of(ch.chord_of(b(BASE | Buttons::B))), Some(Chord::ToggleLizard));
        assert_eq!(action_of(ch.chord_of(b(BASE))), None); // paddles alone are not a chord
    }

    #[test]
    fn needs_every_configured_button() {
        let three = Buttons::L4 | Buttons::L5 | Buttons::R4; // missing R5
        assert_eq!(action_of(defaults().chord_of(b(three | Buttons::X))), None);
    }

    #[test]
    fn extra_buttons_do_not_block_a_chord() {
        // Superset match: touch bits ride along on real hands all the time.
        let held = BASE | Buttons::X | Buttons::LPAD_TOUCH | Buttons::RSTICK_TOUCH;
        assert_eq!(action_of(defaults().chord_of(b(held))), Some(Chord::StartXbox));
    }

    fn chord(buttons: &[&str]) -> ChordCfg {
        ChordCfg {
            buttons: buttons.iter().map(|s| s.to_string()).collect(),
            feedback: Feedback::Chime,
            chime: None,
        }
    }

    #[test]
    fn overlapping_chords_most_specific_wins() {
        let cfg = config::ChordsCfg {
            xbox: chord(&["STEAM", "A"]),
            steam: chord(&["STEAM", "A", "B"]),
            mouse: chord(&["L4", "L5", "B"]),
        };
        let ch = Chords::from_config(&cfg);
        let held = Buttons::STEAM | Buttons::A | Buttons::B; // matches both xbox + steam
        assert_eq!(action_of(ch.chord_of(b(held))), Some(Chord::SteamMode)); // 3 buttons > 2
        assert_eq!(action_of(ch.chord_of(b(Buttons::STEAM | Buttons::A))), Some(Chord::StartXbox));
    }

    #[test]
    fn unknown_button_falls_back_to_default_chord() {
        let cfg = config::ChordsCfg {
            xbox: chord(&["NOT_A_BUTTON"]),
            ..Default::default()
        };
        let ch = Chords::from_config(&cfg);
        assert_eq!(action_of(ch.chord_of(b(BASE | Buttons::X))), Some(Chord::StartXbox));
    }

    #[test]
    fn button_names_are_case_insensitive() {
        assert_eq!(
            Chords::resolve(&["l4", "qam", "Dpad_Up"]).unwrap(),
            Buttons::L4 | Buttons::QUICK_ACCESS | Buttons::DPAD_UP
        );
        assert!(Chords::resolve(&["nope"]).is_err());
        assert!(Chords::resolve::<&str>(&[]).is_err());
    }

    #[test]
    fn label_renders_button_names() {
        let ch = defaults();
        assert_eq!(ch.label(Chord::StartXbox), "X+R4+R5+L4+L5"); // BUTTON_NAMES table order
    }

    // --- debounce latch (Chords::step) ---

    #[test]
    fn fires_once_then_latches_while_held() {
        let mut ch = defaults();
        let t = Instant::now();
        let held = b(BASE | Buttons::B);
        // First frame of the press fires; subsequent held frames do not.
        assert_eq!(action_of(ch.step(held, t)), Some(Chord::ToggleLizard));
        assert_eq!(action_of(ch.step(held, t)), None);
        assert_eq!(action_of(ch.step(held, t)), None);
    }

    #[test]
    fn bounce_between_edges_does_not_refire() {
        let mut ch = defaults();
        let t = Instant::now();
        let held = b(BASE | Buttons::B);
        let gap = b(0); // switch momentarily bounced open
        assert_eq!(action_of(ch.step(held, t)), Some(Chord::ToggleLizard));
        // A brief release (well under CHORD_DEBOUNCE) then re-close must NOT re-fire.
        assert_eq!(action_of(ch.step(gap, t + Duration::from_millis(5))), None);
        assert_eq!(action_of(ch.step(held, t + Duration::from_millis(10))), None);
    }

    #[test]
    fn rearms_after_release_past_debounce() {
        let mut ch = defaults();
        let t = Instant::now();
        let held = b(BASE | Buttons::A);
        assert_eq!(action_of(ch.step(held, t)), Some(Chord::SteamMode));
        // One None frame starts the release timer but does not yet re-arm.
        let t1 = t + Duration::from_millis(1);
        assert_eq!(action_of(ch.step(b(0), t1)), None);
        // A later None frame, past the window, re-arms — so the next press fires again.
        let t2 = t1 + CHORD_DEBOUNCE;
        assert_eq!(action_of(ch.step(b(0), t2)), None);
        assert_eq!(action_of(ch.step(held, t2)), Some(Chord::SteamMode));
    }
}
