//! [`App`] owns every subsystem: `new()` is the setup (config load included),
//! `tick()` one main-loop iteration, `apply_chord()` the conductor for mode
//! transitions (which inherently cut across every subsystem).
//!
//! All user configuration comes from the config file (see [`crate::config`]);
//! there are no env-var knobs.

use std::error::Error;
use std::thread::sleep;
use std::time::{Duration, Instant};

use steampuck::ControllerState;

use crate::chords::{Chord, ChordSpec, Chords};
use crate::config::{self, Feedback};
use crate::dsu;
use crate::link::{Link, LinkEvent};
use crate::lizard::Lizard;
use crate::pad::Pad;
use crate::rumble::{Rumble, RumbleCmd};

/// How long the confirmation tick drives the motor. The hardware's ~50ms haptic
/// auto-stop lets a low-freq pulse thump 2-4 times; a short explicit stop makes it
/// one clean tick. Tuned on-device; the stop lands up to one loop wake (~4ms) late.
const CHORD_TICK_MS: u64 = 12;

/// Runtime output mode, switched by button chords. The [`Pad`] lives inside
/// `Xbox`, so "pad exists" and "in Xbox mode" are the same fact by construction:
/// entering Steam mode drops the pad (removing the uinput device from the
/// system) and there is no second flag to keep in sync.
enum Mode {
    /// Emulate the Xbox pad + DSU motion and suppress the mouse (default).
    Xbox(Pad),
    /// Release the controller so Steam Input drives it as a Steam Controller: no
    /// virtual pad, no lizard/haptic output, no DSU. We keep reading only to watch
    /// for the chord back to Xbox mode.
    Steam,
}

/// Convert a state's raw IMU into DSU motion units and the DSU/DS4 body frame:
/// accel in g (raw / 16384), gyro in deg/s (raw / `gyro_scale`, env
/// `STEAMPUCK_GYRO_SCALE`). The controller's frame has Z up when lying flat;
/// DSU wants X right, Y up, Z toward the player — the same `[x, z, -y]` shuffle
/// SDL's Triton driver applies to both sensors. Sanity check: at rest, accel ≈
/// (0, +1, 0). If a single motion axis reads mirrored on-device, flip it here.
/// (Free adapter between codec frame and DSU protocol — belongs to neither.)
fn motion_of(s: &ControllerState, gyro_scale: f32) -> dsu::Motion {
    let g = |v: i16| v as f32 / 16384.0;
    let d = |v: i16| v as f32 / gyro_scale;
    let [ax, ay, az] = s.imu.accel;
    let [gx, gy, gz] = s.imu.gyro;
    dsu::Motion {
        ts_us: s.imu.timestamp_us as u64,
        accel: [g(ax), g(az), -g(ay)],
        gyro: [d(gx), d(gz), -d(gy)],
    }
}

pub struct App {
    link: Link,
    mode: Mode,
    chords: Chords,
    lizard: Lizard,
    rumble: Rumble,
    dsu: Option<dsu::Server>,
    /// Gyro scale in LSB per deg/s (config `[motion] gyro_scale`).
    gyro_scale: f32,
    /// Chord-tick magnitude (config `[feedback] rumble_strength`, 0 = off).
    chord_rumble: u16,
}

impl App {
    pub fn new() -> Result<App, Box<dyn Error>> {
        let cfg = config::load();
        let chords = Chords::from_config(&cfg.chords);

        // ponytail: single controller assumed — all slots fold into one pad.
        let link = Link::open()?;

        // Start in Xbox mode: emulate the pad. Chords switch modes at runtime.
        let mut mode = Mode::Xbox(Pad::new()?);

        eprint!("virtual pad up as:");
        if let Mode::Xbox(p) = &mut mode {
            for node in p.dev_nodes()? {
                eprint!(" {}", node.display());
            }
        }
        eprintln!();
        eprintln!(
            "chords: {} = Xbox mode · {} = Steam mode · {} = toggle mouse",
            chords.label(Chord::StartXbox),
            chords.label(Chord::SteamMode),
            chords.label(Chord::ToggleLizard),
        );
        eprintln!(
            "{} — Ctrl-C to stop.",
            if link.has_devices() {
                "Xbox mode active"
            } else {
                "waiting for the controller (Puck or wired)"
            }
        );

        let gyro_scale = cfg.motion.scale();
        // DSU motion server: streams gyro/accel to emulators (Cemu, Dolphin,
        // Citra, yuzu, RPCS3). Non-fatal if the port is taken — the pad still
        // works without it.
        let dsu = match dsu::Server::bind(26760, std::process::id()) {
            Ok(s) => {
                eprintln!(
                    "DSU motion server on 127.0.0.1:26760 (gyro scale {gyro_scale} LSB/deg/s)"
                );
                Some(s)
            }
            Err(e) => {
                eprintln!("DSU motion server disabled ({e})");
                None
            }
        };

        Ok(App {
            link,
            mode,
            chords,
            lizard: Lizard::new(),
            rumble: Rumble::new(),
            dsu,
            gyro_scale,
            chord_rumble: cfg.feedback.rumble_magnitude(),
        })
    }

    /// One main-loop iteration. The loop's clock is the controller itself when
    /// connected (read_pass parks on the streaming interface) and a 1ms poll
    /// when not.
    pub fn tick(&mut self) -> Result<(), Box<dyn Error>> {
        let mut idle = true;

        // Answer DSU clients (version/info/subscribe). Cheap; runs every iteration.
        if let Some(d) = &mut self.dsu {
            d.poll();
        }

        // Force feedback from the game (Xbox mode only — needs the pad).
        if let Mode::Xbox(p) = &mut self.mode
            && let Some(cmd) = p.drain_ff()?
        {
            self.rumble.set(&self.link, cmd);
            idle = false;
        }

        // Hotplug: a fresh link starts with lizard on, so re-assert OFF at once.
        if self.link.rescan_if_empty() {
            self.lizard.reassert();
        }

        // Controller input -> chords, virtual pad, DSU motion.
        for ev in self.link.read_pass() {
            match ev {
                LinkEvent::State(state, fresh) => {
                    if let Some(spec) = self.chords.step(state.buttons, Instant::now()) {
                        self.apply_chord(spec);
                    }
                    if let Mode::Xbox(p) = &mut self.mode {
                        p.emit(&state)?;
                        // Motion moves even when buttons don't.
                        if let Some(d) = &mut self.dsu {
                            d.send(&motion_of(&state, self.gyro_scale));
                        }
                        if fresh {
                            // A fresh stream in Xbox mode = the mode just became
                            // live (startup, plug-in, reconnect): announce with the
                            // Xbox chord's configured feedback. After chord
                            // handling, so a first-frame Steam chord wins.
                            let spec = self.chords.spec_for(Chord::StartXbox);
                            self.feedback(&spec);
                        }
                    }
                    idle = false;
                }
                LinkEvent::Connected => eprintln!("controller connected"),
                LinkEvent::Disconnected | LinkEvent::Lost => {
                    eprintln!("controller disconnected — waiting for reconnect");
                    if let Mode::Xbox(p) = &mut self.mode {
                        p.release(); // nothing sticks on the virtual pad
                    }
                    self.rumble.clear();
                }
            }
        }

        // Hold the phantom mouse off while suppressed — in any mode, so the
        // toggle works from Steam mode too (no-op unless suppressing).
        self.lizard.tick(&self.link);

        // Keep an active rumble alive / retire it at its deadline.
        self.rumble.tick(&self.link);

        if idle && !self.link.is_streaming() {
            // Nothing to park on (no receiver, or no report seen yet).
            sleep(Duration::from_millis(1));
        }
        Ok(())
    }

    /// Apply a fired chord — the conductor. Mode transitions are inherently
    /// cross-cutting (pad, lizard, rumble, chimes), so they live in this one
    /// place instead of being smeared across the subsystems.
    fn apply_chord(&mut self, spec: ChordSpec) {
        // Each arm reports whether it actually changed state — feedback only
        // confirms real transitions, not re-asserting the mode you're already in.
        let changed = match spec.action {
            // Entering Xbox mode suppresses the mouse; re-pressing the chord
            // while already in Xbox is a no-op (use the mouse toggle chord).
            Chord::StartXbox => {
                if matches!(self.mode, Mode::Steam) {
                    match Pad::new() {
                        Ok(p) => {
                            self.lizard.suppress_now();
                            // Fresh pad: FF pool + diff state reset with it, so the
                            // full controller state re-emits on the next report.
                            self.mode = Mode::Xbox(p);
                            self.rumble.clear();
                            eprintln!("→ Xbox mode (pad + DSU, mouse suppressed)");
                            true
                        }
                        Err(e) => {
                            eprintln!("Xbox mode: pad create failed: {e}");
                            false
                        }
                    }
                } else {
                    false
                }
            }
            Chord::SteamMode => {
                if !matches!(self.mode, Mode::Steam) {
                    // Drops the uinput pad (and its FF state) so Steam can take over.
                    self.mode = Mode::Steam;
                    self.rumble.clear();
                    self.lizard.release(&self.link); // back to its native state
                    eprintln!("→ Steam mode (released for Steam Input)");
                    true
                } else {
                    false
                }
            }
            Chord::ToggleLizard => {
                let mouse_on = self.lizard.toggle(&self.link);
                eprintln!("mouse {}", if mouse_on { "on" } else { "off" });
                true // a toggle always changes state
            }
        };
        // Confirm with the configured feedback. Runs AFTER the match — the mode
        // arms clear the rumble latch, which would cancel a tick's stop deadline.
        if changed {
            self.feedback(&spec);
        }
    }

    /// Play a chord's configured haptic confirmation.
    fn feedback(&mut self, spec: &ChordSpec) {
        match spec.feedback {
            // One sweep packet, firmware-timed.
            Feedback::Chime => self.link.chime(spec.chime),
            // Rumble tick: latch magnitude + deadline; Rumble::tick delivers the
            // explicit stop (the ~50ms hardware auto-stop would let the motor
            // thump 2-4 times).
            Feedback::Rumble if self.chord_rumble != 0 => {
                let until = Some(Instant::now() + Duration::from_millis(CHORD_TICK_MS));
                let cmd = RumbleCmd { strong: self.chord_rumble, weak: 0, until };
                self.rumble.set(&self.link, cmd);
            }
            Feedback::Rumble | Feedback::None => {}
        }
    }
}
