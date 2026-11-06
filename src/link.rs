//! The HID side: candidate interfaces across the three transports, the read
//! pass that turns raw reports into semantic events, and every controller-bound
//! send (rumble, lizard mode, chimes).

use std::time::{Duration, Instant};

use hidapi::{HidApi, HidDevice};

use steampuck::{
    parse, ControllerState, REPORT_ID_WIRELESS_STATUS, REPORT_ID_WIRELESS_STATUS_X,
};

const VID: u16 = 0x28de;
/// The Steam Controller on each of its three transports — all stream the identical
/// 0x45 report (HID-over-GATT for BT), so the daemon handles them identically.
const PID_PUCK: u16 = 0x1304; // wireless "Puck" receiver
const PID_WIRED: u16 = 0x1302; // wired directly over USB-C
const PID_BT: u16 = 0x1303; // paired directly over Bluetooth (HOGP)

/// A haptic chime: one `0x84` log-sweep OUTPUT report — the firmware glides
/// `start_hz` → `end_hz` over `ms` and stops itself. Fire-and-forget (nothing to
/// resend or stop), so it's safe anywhere in the loop. Verified on-device.
#[derive(Clone, Copy)]
pub struct Sweep {
    pub start_hz: u16,
    pub end_hz: u16,
    pub ms: u16,
    /// 0 = firmware default volume; positive = louder, negative = quieter (dB).
    pub gain_db: i8,
}

/// What one [`Link::read_pass`] produced.
pub enum LinkEvent {
    /// A decoded 0x45 controller state. `fresh` = the stream just (re)appeared
    /// (startup, plug-in, reconnect) — there was no streaming interface before.
    State(ControllerState, bool),
    /// Wireless status: controller powered on.
    Connected,
    /// Wireless status: controller powered off. The receiver's interfaces stay
    /// alive — reads return 0, never Err — so this is the only disconnect signal
    /// while the receiver itself stays up.
    Disconnected,
    /// A read failed: the receiver/cable itself went away; the interfaces were
    /// dropped and the 1s rescan will reopen them.
    Lost,
}

/// The HID side: every candidate interface across the three transports, plus
/// which one the controller actually streams on. All controller-bound output
/// (rumble, lizard, chimes) goes through here and targets the streaming
/// interface — the ONLY one that accepts it; the receiver's other slot/BT
/// interfaces reject feature report 0x01 with EPIPE. No stream = sends no-op.
pub struct Link {
    api: HidApi,
    devices: Vec<HidDevice>,
    active: Option<usize>, // index of the interface the controller streams on
    last_scan: Instant,    // last hotplug re-scan for the receiver
    buf: [u8; 64],
}

impl Link {
    /// Open all present interfaces. Empty (not an error) with nothing plugged:
    /// the main loop waits for the receiver and survives unplug/replug.
    pub fn open() -> Result<Link, hidapi::HidError> {
        let api = HidApi::new()?;
        let devices = Self::scan(&api);
        Ok(Link {
            api,
            devices,
            active: None,
            last_scan: Instant::now(),
            buf: [0u8; 64],
        })
    }

    /// Open every deduplicated HID interface (VID/PID match), each non-blocking.
    /// Empty if the receiver isn't present. The connected controller streams the
    /// 0x45 report on exactly one of these; `parse` filters out the rest.
    fn scan(api: &HidApi) -> Vec<HidDevice> {
        let mut devices = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for info in api.device_list() {
            if info.vendor_id() != VID
                || !matches!(info.product_id(), PID_PUCK | PID_WIRED | PID_BT)
            {
                continue;
            }
            // hidapi may list an interface once per top-level HID collection; dedup by
            // hidraw path so each node is opened only once.
            if !seen.insert(info.path().to_owned()) {
                continue;
            }
            if let Ok(dev) = info.open_device(api) {
                let _ = dev.set_blocking_mode(false);
                eprintln!(
                    "  interface [{}] = {:04x}:{:04x} {}",
                    devices.len(),
                    info.vendor_id(),
                    info.product_id(),
                    info.path().to_string_lossy()
                );
                devices.push(dev);
            }
        }
        devices
    }

    /// True while a controller is streaming 0x45 on some interface.
    pub fn is_streaming(&self) -> bool {
        self.active.is_some()
    }

    /// True if no interface is currently open (receiver absent).
    pub fn has_devices(&self) -> bool {
        !self.devices.is_empty()
    }

    /// The streaming interface, if any.
    fn dev(&self) -> Option<&HidDevice> {
        self.active.map(|i| &self.devices[i])
    }

    /// Send the haptic OUTPUT report `0x80` (`MsgHapticRumble`, 10 bytes incl.
    /// report id). Byte layout AND values match SDL's Triton driver: the
    /// low/high-frequency magnitudes ride in the left/right `speed` fields while
    /// `type` and `intensity` stay 0. The controller's ~50 ms haptic safety
    /// timeout cuts the motor unless refreshed, so [`crate::rumble::Rumble`]
    /// resends every ~40 ms; a zero magnitude stops it immediately.
    pub fn rumble(&self, strong: u16, weak: u16) {
        let Some(dev) = self.dev() else { return };
        let report = [
            0x80,                // ID_OUT_REPORT_HAPTIC_RUMBLE
            0x00,                // type
            0x00,                // intensity lo
            0x00,                // intensity hi
            strong as u8,        // left.speed lo  (low-frequency motor)
            (strong >> 8) as u8, // left.speed hi
            0x00,                // left.gain
            weak as u8,          // right.speed lo (high-frequency motor)
            (weak >> 8) as u8,   // right.speed hi
            0x00,                // right.gain
        ];
        if let Err(e) = dev.write(&report) {
            eprintln!("rumble write: {e}");
        }
    }

    /// Set "lizard mode" (the phantom mouse/keyboard) on the controller via a
    /// `SET_SETTINGS_VALUES` feature report (id 0x01), SDL's Triton method.
    /// `on=false` writes `LIZARD_MODE_OFF` (suppress the mouse — our default)
    /// and also pins `SETTING_IMU_MODE` to raw accel+gyro, so motion can't
    /// silently drop out on a transport or power state that defaults it off
    /// (SDL enables it the same way); `on=true` writes `LIZARD_MODE_ON` alone
    /// (Steam owns the IMU then). The 0x45 gamepad stream is unaffected either
    /// way. The controller's watchdog re-enables lizard within seconds, so the
    /// "off" case is resent by [`crate::lizard::Lizard`].
    pub fn lizard(&self, on: bool) {
        let Some(dev) = self.dev() else { return };
        let mut report = [0u8; 64];
        report[0] = 0x01; // feature report id (relay to controller)
        report[1] = 0x87; // ID_SET_SETTINGS_VALUES
        report[2] = 0x03; // payload len: 1 ControllerSetting (u8 num + u16 val)
        report[3] = 0x09; // SETTING_LIZARD_MODE
        report[4] = on as u8; // LIZARD_MODE_ON(1) / _OFF(0), u16 LE
        report[5] = 0x00;
        if !on {
            report[2] = 0x06; // 2 ControllerSettings
            report[6] = 0x30; // SETTING_IMU_MODE
            report[7] = 0x18; // SEND_RAW_ACCEL(0x08) | SEND_RAW_GYRO(0x10), u16 LE
            report[8] = 0x00;
        }
        if let Err(e) = dev.send_feature_report(&report) {
            eprintln!("lizard mode: {e}");
        }
    }

    /// Play a [`Sweep`] chime: single `0x84` log-sweep report, both actuators,
    /// firmware-timed.
    pub fn chime(&self, s: Sweep) {
        let Some(dev) = self.dev() else { return };
        let mut r = [0u8; 9];
        r[0] = 0x84; // ID_OUT_REPORT_HAPTIC_LOG_SWEEP
        r[1] = 0x03; // both sides
        r[2] = s.gain_db as u8;
        r[3..5].copy_from_slice(&s.ms.to_le_bytes());
        r[5..7].copy_from_slice(&s.start_hz.to_le_bytes());
        r[7..9].copy_from_slice(&s.end_hz.to_le_bytes());
        if let Err(e) = dev.write(&r) {
            eprintln!("chime: {e}");
        }
    }

    /// Hotplug: (re)discover the receiver whenever we have no devices — at
    /// startup before it's plugged, or after an unplug. Throttled to one scan
    /// per second; returns true when a fresh link appeared.
    pub fn rescan_if_empty(&mut self) -> bool {
        if !self.devices.is_empty() || self.last_scan.elapsed() < Duration::from_secs(1) {
            return false;
        }
        let _ = self.api.refresh_devices();
        self.devices = Self::scan(&self.api);
        self.last_scan = Instant::now();
        if self.devices.is_empty() {
            return false;
        }
        eprintln!("controller up: {} interface(s)", self.devices.len());
        true
    }

    /// One read pass over every interface, translated into semantic events.
    /// The streaming interface gets a 1ms-capped blocking read — the loop parks
    /// there and poll() inside hidapi wakes the instant a report lands (a plain
    /// sleep(1ms) pacing added ~0.5ms average input latency). Idle interfaces
    /// are drained non-blocking; with no stream at all the caller paces the loop.
    pub fn read_pass(&mut self) -> Vec<LinkEvent> {
        let mut events = Vec::new();
        let mut lost = false;
        let mut disconnected = false;
        for (idx, dev) in self.devices.iter().enumerate() {
            let read = if Some(idx) == self.active {
                dev.read_timeout(&mut self.buf, 1)
            } else {
                dev.read(&mut self.buf)
            };
            match read {
                Ok(n) if n > 0 => {
                    if let Some(state) = parse(&self.buf[..n]) {
                        events.push(LinkEvent::State(state, self.active.is_none()));
                        self.active = Some(idx);
                    } else if n >= 2
                        && matches!(
                            self.buf[0],
                            REPORT_ID_WIRELESS_STATUS | REPORT_ID_WIRELESS_STATUS_X
                        )
                    {
                        match self.buf[1] {
                            1 => {
                                disconnected = true;
                                events.push(LinkEvent::Disconnected);
                            }
                            2 => events.push(LinkEvent::Connected),
                            _ => {}
                        }
                    }
                }
                Ok(_) => {}
                Err(_) => {
                    lost = true;
                    break;
                }
            }
        }
        if lost {
            self.devices.clear(); // receiver itself is gone: the 1s rescan reopens it
            events.push(LinkEvent::Lost);
        }
        if lost || disconnected {
            self.active = None; // no stream: also parks the lizard/rumble ticks
        }
        events
    }
}
