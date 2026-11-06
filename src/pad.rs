//! The virtual Xbox 360 pad: uinput device, state diffing, force feedback.
//!
//! Tunables live at the top: `INVERT_STICK_Y` and `BUTTON_MAP`.

use std::collections::{BTreeSet, HashMap};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use evdev::{
    uinput::VirtualDevice, AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventSummary,
    EventType, FFEffectCode, FFEffectKind, InputEvent, InputId, KeyCode, UInputCode,
    UinputAbsSetup,
};

use steampuck::{Buttons, ControllerState};

use crate::rumble::RumbleCmd;

const STICK_MIN: i32 = -32768;
const STICK_MAX: i32 = 32767;
const TRIGGER_MAX: i32 = 32767;

/// The Puck's stick Y is up-positive; evdev/Xbox convention is up-negative.
/// ponytail: flip this one bool if up/down come out inverted in `jstest`.
const INVERT_STICK_Y: bool = true;

/// Steam Controller button -> Xbox pad key. The first 11 are the standard Xbox
/// 360 set; the 4 paddles + QAM have no X360 equivalent, so they ride along as
/// extra `BTN_TRIGGER_HAPPY` buttons (visible to jstest / raw-joystick binding,
/// ignored by the SDL X360 mapping). Pad clicks, grips and capacitive touches
/// stay unmapped. ponytail: to make a paddle a face button, point it at BTN_SOUTH etc.
const BUTTON_MAP: &[(u32, KeyCode)] = &[
    (Buttons::A, KeyCode::BTN_SOUTH),
    (Buttons::B, KeyCode::BTN_EAST),
    (Buttons::X, KeyCode::BTN_NORTH),
    (Buttons::Y, KeyCode::BTN_WEST),
    (Buttons::LB, KeyCode::BTN_TL),
    (Buttons::RB, KeyCode::BTN_TR),
    (Buttons::VIEW, KeyCode::BTN_SELECT),
    (Buttons::MENU, KeyCode::BTN_START),
    (Buttons::STEAM, KeyCode::BTN_MODE),
    (Buttons::L3, KeyCode::BTN_THUMBL),
    (Buttons::R3, KeyCode::BTN_THUMBR),
    // Extras (no X360 equivalent) as spare buttons — jstest buttons 11..15.
    (Buttons::R4, KeyCode::BTN_TRIGGER_HAPPY1),
    (Buttons::R5, KeyCode::BTN_TRIGGER_HAPPY2),
    (Buttons::L4, KeyCode::BTN_TRIGGER_HAPPY3),
    (Buttons::L5, KeyCode::BTN_TRIGGER_HAPPY4),
    (Buttons::QUICK_ACCESS, KeyCode::BTN_TRIGGER_HAPPY5),
];

/// The method evdev 0.13 forgot: the reader `Device` has `set_nonblocking`,
/// `VirtualDevice` doesn't. Delete this when upstream grows one.
trait VirtualDeviceExt {
    fn set_nonblocking(&self) -> std::io::Result<()>;
}

impl VirtualDeviceExt for VirtualDevice {
    fn set_nonblocking(&self) -> std::io::Result<()> {
        let fd = self.as_raw_fd();
        // SAFETY: plain fcntl on a fd we own — pure FFI, no memory-safety surface.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags == -1 || libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == -1 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

/// Post-transform Xbox axis values for a controller state ([`Pad::axes_of`]).
struct Axes {
    lx: i32,
    ly: i32,
    rx: i32,
    ry: i32,
    lt: i32,
    rt: i32,
    hat_x: i32,
    hat_y: i32,
}

/// The virtual Xbox pad plus everything that lives and dies with it: the diff
/// state for minimal event emission and the force-feedback effect bookkeeping.
/// Dropping the Pad (Steam mode) resets all of that by construction.
pub struct Pad {
    dev: VirtualDevice,
    last: Option<ControllerState>,
    ff_ids: BTreeSet<u16>,                  // free effect-id pool
    effects: HashMap<u16, (u16, u16, u16)>, // id -> (strong, weak, len_ms)
}

impl Pad {
    /// Register the uinput device in its X360 costume — the canonical
    /// VID/PID/name/version quadruple games and SDL mappings match on.
    fn build_device() -> std::io::Result<VirtualDevice> {
        let keys = AttributeSet::from_iter(BUTTON_MAP.iter().map(|&(_, k)| k));
        // fresh AbsInfo per axis: (value, min, max, fuzz, flat, resolution)
        let stick = || AbsInfo::new(0, STICK_MIN, STICK_MAX, 16, 128, 1);
        let trigger = || AbsInfo::new(0, 0, TRIGGER_MAX, 0, 0, 1);
        let hat = || AbsInfo::new(0, -1, 1, 0, 0, 1);

        VirtualDevice::builder()?
            .name("Microsoft X-Box 360 pad")
            .input_id(InputId::new(BusType::BUS_USB, 0x045e, 0x028e, 0x0114))
            .with_keys(&keys)?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_X, stick()))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Y, stick()))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RX, stick()))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RY, stick()))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_Z, trigger()))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_RZ, trigger()))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_HAT0X, hat()))?
            .with_absolute_axis(&UinputAbsSetup::new(AbsoluteAxisCode::ABS_HAT0Y, hat()))?
            .with_ff(&AttributeSet::from_iter([FFEffectCode::FF_RUMBLE]))?
            .with_ff_effects_max(16)
            .build()
    }

    /// Build the pad with its fd non-blocking so FF polling never stalls reads.
    pub fn new() -> std::io::Result<Pad> {
        let dev = Self::build_device()?;
        dev.set_nonblocking()?;
        Ok(Pad {
            dev,
            last: None,
            ff_ids: (0..16).collect(),
            effects: HashMap::new(),
        })
    }

    /// The pad's /dev/input nodes (for the startup banner).
    pub fn dev_nodes(&mut self) -> std::io::Result<Vec<PathBuf>> {
        self.dev.enumerate_dev_nodes_blocking()?.collect()
    }

    fn stick_y(v: i16) -> i32 {
        let y = if INVERT_STICK_Y { -(v as i32) } else { v as i32 };
        y.clamp(STICK_MIN, STICK_MAX)
    }

    fn axes_of(s: &ControllerState) -> Axes {
        let b = s.buttons;
        Axes {
            lx: (s.left_stick.0 as i32).clamp(STICK_MIN, STICK_MAX),
            ly: Self::stick_y(s.left_stick.1),
            rx: (s.right_stick.0 as i32).clamp(STICK_MIN, STICK_MAX),
            ry: Self::stick_y(s.right_stick.1),
            lt: (s.left_trigger as i32).min(TRIGGER_MAX),
            rt: (s.right_trigger as i32).min(TRIGGER_MAX),
            hat_x: b.has(Buttons::DPAD_RIGHT) as i32 - b.has(Buttons::DPAD_LEFT) as i32,
            hat_y: b.has(Buttons::DPAD_DOWN) as i32 - b.has(Buttons::DPAD_UP) as i32,
        }
    }

    /// Events for the controls that changed between `prev` and `cur`.
    fn build_events(prev: Option<&ControllerState>, cur: &ControllerState) -> Vec<InputEvent> {
        let mut ev = Vec::new();

        for &(mask, key) in BUTTON_MAP {
            let now = cur.buttons.has(mask);
            if prev.is_none_or(|p| p.buttons.has(mask) != now) {
                ev.push(InputEvent::new(EventType::KEY.0, key.code(), now as i32));
            }
        }

        let a = Self::axes_of(cur);
        let p = prev.map(Self::axes_of);
        let mut abs = |code: AbsoluteAxisCode, v: i32, old: Option<i32>| {
            if old != Some(v) {
                ev.push(InputEvent::new(EventType::ABSOLUTE.0, code.0, v));
            }
        };
        abs(AbsoluteAxisCode::ABS_X, a.lx, p.as_ref().map(|x| x.lx));
        abs(AbsoluteAxisCode::ABS_Y, a.ly, p.as_ref().map(|x| x.ly));
        abs(AbsoluteAxisCode::ABS_RX, a.rx, p.as_ref().map(|x| x.rx));
        abs(AbsoluteAxisCode::ABS_RY, a.ry, p.as_ref().map(|x| x.ry));
        abs(AbsoluteAxisCode::ABS_Z, a.lt, p.as_ref().map(|x| x.lt));
        abs(AbsoluteAxisCode::ABS_RZ, a.rt, p.as_ref().map(|x| x.rt));
        abs(AbsoluteAxisCode::ABS_HAT0X, a.hat_x, p.as_ref().map(|x| x.hat_x));
        abs(AbsoluteAxisCode::ABS_HAT0Y, a.hat_y, p.as_ref().map(|x| x.hat_y));
        ev
    }

    /// Mirror `state` into the pad, emitting only the controls that changed.
    pub fn emit(&mut self, state: &ControllerState) -> std::io::Result<()> {
        let events = Self::build_events(self.last.as_ref(), state);
        if !events.is_empty() {
            self.dev.emit(&events)?;
        }
        self.last = Some(*state);
        Ok(())
    }

    /// Release every held control so nothing sticks on the virtual pad
    /// (controller powered off / receiver unplugged).
    pub fn release(&mut self) {
        if let Some(last) = self.last.take() {
            let ev = Self::build_events(Some(&last), &ControllerState::default());
            if !ev.is_empty() {
                let _ = self.dev.emit(&ev);
            }
        }
    }

    /// Service the game's force-feedback traffic: uploads/erases maintain the
    /// effect table; a play latches the effect's magnitudes + deadline, a stop
    /// (or unknown id) latches silence. Returns the resulting [`RumbleCmd`]
    /// (last one wins if several land in one drain), `None` if no play/stop.
    pub fn drain_ff(&mut self) -> std::io::Result<Option<RumbleCmd>> {
        // Collect first so the fetch_events borrow of the pad is released.
        let ff_events: Vec<InputEvent> = match self.dev.fetch_events() {
            Ok(it) => it.collect(),
            Err(_) => Vec::new(), // WouldBlock when idle (fd is non-blocking)
        };
        let mut cmd = None;
        for ev in ff_events {
            match ev.destructure() {
                EventSummary::UInput(e, UInputCode::UI_FF_UPLOAD, _) => {
                    let mut up = self.dev.process_ff_upload(e)?;
                    match self.ff_ids.iter().next().copied() {
                        Some(id) => {
                            self.ff_ids.remove(&id);
                            up.set_effect_id(id as i16);
                            let data = up.effect();
                            if let FFEffectKind::Rumble {
                                strong_magnitude,
                                weak_magnitude,
                            } = data.kind
                            {
                                self.effects.insert(
                                    id,
                                    (strong_magnitude, weak_magnitude, data.replay.length),
                                );
                            }
                            up.set_retval(0);
                        }
                        None => up.set_retval(-1),
                    }
                }
                EventSummary::UInput(e, UInputCode::UI_FF_ERASE, _) => {
                    let er = self.dev.process_ff_erase(e)?;
                    let id = er.effect_id() as u16;
                    self.effects.remove(&id);
                    self.ff_ids.insert(id);
                }
                EventSummary::ForceFeedback(_, id, value) => {
                    cmd = Some(match (value != 0).then(|| self.effects.get(&id.0)).flatten() {
                        Some(&(strong, weak, length)) => {
                            // length 0 = play until explicit stop; else auto-stop.
                            let until = (length > 0)
                                .then(|| Instant::now() + Duration::from_millis(length as u64));
                            RumbleCmd { strong, weak, until }
                        }
                        None => RumbleCmd::ZERO,
                    });
                }
                _ => {}
            }
        }
        Ok(cmd)
    }
}
