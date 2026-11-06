//! Steam Controller Puck (USB 28de:1304) protocol codec — pure, no OS deps.
//!
//! The Puck is a wireless receiver; it forwards the controller's input report
//! verbatim to the host as a 46-byte HID report (report id `0x45`; the plain-USB
//! id `0x42` carries the identical layout). This module decodes that
//! report into a [`ControllerState`]. Layout verified against real capture,
//! the openpuck project (github.com/safijari/openpuck), and SDL's
//! `SDL_hidapi_steam_triton.c` (`TritonMTUNoQuat_t`, parsed for both ids).
//!
//! Byte offsets below are into the raw HID report *including* the leading
//! report-id byte — i.e. exactly what `hidapi`'s `read` hands you.

#![forbid(unsafe_code)]

/// Main controller input report id (SDL: `ID_TRITON_CONTROLLER_STATE_BLE`) —
/// what all three transports (Puck, wired USB-C, Bluetooth) stream.
pub const REPORT_ID_INPUT: u8 = 0x45;
/// Plain-USB input id (SDL: `ID_TRITON_CONTROLLER_STATE`); identical layout —
/// SDL parses both ids with the same struct.
pub const REPORT_ID_INPUT_USB: u8 = 0x42;
/// Length of an input report: 1 report-id byte + 45 payload bytes.
pub const INPUT_REPORT_LEN: usize = 46;
/// Wireless connect-state report (edge-triggered): payload byte 0 is
/// 2 = controller connected, 1 = disconnected (powered off / out of range).
pub const REPORT_ID_WIRELESS_STATUS: u8 = 0x79;
/// Alternate id for the same wireless-status payload (SDL handles both).
pub const REPORT_ID_WIRELESS_STATUS_X: u8 = 0x46;

/// Button bitfield: little-endian `u32` at report bytes 2..6. Bits 0..=29 used.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub struct Buttons(pub u32);

impl Buttons {
    // report byte 2 (bits 0-7)
    // VIEW/MENU corrected by on-device measurement — the openpuck-derived
    // layout had them crossed. X/Y verified by chord press: bit 2 IS the
    // printed X. (Web gamepad testers may still show X/Y "swapped" for the
    // virtual pad: Linux's BTN_NORTH is historically the X button, and some
    // browser mappers place it at the top slot. A real X360 pad does the same.)
    pub const A: u32 = 1 << 0;
    pub const B: u32 = 1 << 1;
    pub const X: u32 = 1 << 2;
    pub const Y: u32 = 1 << 3;
    pub const QUICK_ACCESS: u32 = 1 << 4; // "..." QAM button
    pub const R3: u32 = 1 << 5;
    pub const MENU: u32 = 1 << 6; // ≡
    pub const R4: u32 = 1 << 7; // back paddle
    // report byte 3 (bits 8-15)
    pub const R5: u32 = 1 << 8; // back paddle
    pub const RB: u32 = 1 << 9;
    pub const DPAD_DOWN: u32 = 1 << 10;
    pub const DPAD_RIGHT: u32 = 1 << 11;
    pub const DPAD_LEFT: u32 = 1 << 12;
    pub const DPAD_UP: u32 = 1 << 13;
    pub const VIEW: u32 = 1 << 14; // ⧉
    pub const L3: u32 = 1 << 15;
    // report byte 4 (bits 16-23)
    pub const STEAM: u32 = 1 << 16;
    pub const L4: u32 = 1 << 17; // back paddle
    pub const L5: u32 = 1 << 18; // back paddle
    pub const LB: u32 = 1 << 19;
    pub const RSTICK_TOUCH: u32 = 1 << 20;
    pub const RPAD_TOUCH: u32 = 1 << 21;
    pub const RPAD_CLICK: u32 = 1 << 22;
    pub const RTRIGGER_CLICK: u32 = 1 << 23;
    // report byte 5 (bits 24-29)
    pub const LSTICK_TOUCH: u32 = 1 << 24;
    pub const LPAD_TOUCH: u32 = 1 << 25;
    pub const LPAD_CLICK: u32 = 1 << 26;
    pub const LTRIGGER_CLICK: u32 = 1 << 27;
    pub const RGRIP: u32 = 1 << 28;
    pub const LGRIP: u32 = 1 << 29;

    /// True if any bit in `mask` is set.
    #[inline]
    pub fn has(self, mask: u32) -> bool {
        self.0 & mask != 0
    }
}

/// Config-facing names for every button bit, kept next to the consts above so
/// they can't drift. One name per bit; "QAM" is the friendly alias for
/// `QUICK_ACCESS` (the "..." button).
pub const BUTTON_NAMES: &[(&str, u32)] = &[
    ("A", Buttons::A),
    ("B", Buttons::B),
    ("X", Buttons::X),
    ("Y", Buttons::Y),
    ("QAM", Buttons::QUICK_ACCESS),
    ("R3", Buttons::R3),
    ("MENU", Buttons::MENU),
    ("R4", Buttons::R4),
    ("R5", Buttons::R5),
    ("RB", Buttons::RB),
    ("DPAD_DOWN", Buttons::DPAD_DOWN),
    ("DPAD_RIGHT", Buttons::DPAD_RIGHT),
    ("DPAD_LEFT", Buttons::DPAD_LEFT),
    ("DPAD_UP", Buttons::DPAD_UP),
    ("VIEW", Buttons::VIEW),
    ("L3", Buttons::L3),
    ("STEAM", Buttons::STEAM),
    ("L4", Buttons::L4),
    ("L5", Buttons::L5),
    ("LB", Buttons::LB),
    ("RSTICK_TOUCH", Buttons::RSTICK_TOUCH),
    ("RPAD_TOUCH", Buttons::RPAD_TOUCH),
    ("RPAD_CLICK", Buttons::RPAD_CLICK),
    ("RTRIGGER_CLICK", Buttons::RTRIGGER_CLICK),
    ("LSTICK_TOUCH", Buttons::LSTICK_TOUCH),
    ("LPAD_TOUCH", Buttons::LPAD_TOUCH),
    ("LPAD_CLICK", Buttons::LPAD_CLICK),
    ("LTRIGGER_CLICK", Buttons::LTRIGGER_CLICK),
    ("RGRIP", Buttons::RGRIP),
    ("LGRIP", Buttons::LGRIP),
];

impl core::fmt::Debug for Buttons {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Buttons({:#010x})", self.0)
    }
}

/// A trackpad. `x`/`y` are signed, centered at 0; `pressure` unsigned. All zero
/// when the pad is not being touched.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Pad {
    pub x: i16,
    pub y: i16,
    pub pressure: u16,
}

/// Inertial measurement unit, raw sensor units. Full scale per SDL's Triton
/// driver: accel ±2 g (16384 LSB/g), gyro ±2000 deg/s (16.384 LSB per deg/s).
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct Imu {
    /// Free-running microsecond counter (use deltas as dt for gyro integration).
    pub timestamp_us: u32,
    pub accel: [i16; 3], // x, y, z
    pub gyro: [i16; 3],  // x, y, z
}

/// Fully decoded controller state from one input report.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub struct ControllerState {
    /// Per-report sequence counter; a changed value means a fresh report.
    pub seq: u8,
    pub buttons: Buttons,
    /// Analog triggers, 0 ..= ~32767.
    pub left_trigger: u16,
    pub right_trigger: u16,
    /// Analog sticks, `(x, y)`, center ~0, ±32767. `x` right-positive, `y` up-positive.
    pub left_stick: (i16, i16),
    pub right_stick: (i16, i16),
    pub left_pad: Pad,
    pub right_pad: Pad,
    pub imu: Imu,
}

#[inline]
fn le_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
fn le_i16(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
fn le_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

/// Decode a raw HID input report (including its leading report-id byte).
///
/// Returns `None` unless it is a 46-byte `0x45`/`0x42` input report.
pub fn parse(report: &[u8]) -> Option<ControllerState> {
    if report.len() < INPUT_REPORT_LEN {
        return None;
    }
    match report[0] {
        REPORT_ID_INPUT | REPORT_ID_INPUT_USB => {}
        _ => return None,
    }
    Some(ControllerState {
        seq: report[1],
        buttons: Buttons(le_u32(report, 2)),
        left_trigger: le_u16(report, 6),
        right_trigger: le_u16(report, 8),
        left_stick: (le_i16(report, 10), le_i16(report, 12)),
        right_stick: (le_i16(report, 14), le_i16(report, 16)),
        left_pad: Pad {
            x: le_i16(report, 18),
            y: le_i16(report, 20),
            pressure: le_u16(report, 22),
        },
        right_pad: Pad {
            x: le_i16(report, 24),
            y: le_i16(report, 26),
            pressure: le_u16(report, 28),
        },
        imu: Imu {
            timestamp_us: le_u32(report, 30),
            accel: [le_i16(report, 34), le_i16(report, 36), le_i16(report, 38)],
            gyro: [le_i16(report, 40), le_i16(report, 42), le_i16(report, 44)],
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Real 0x45 frames captured from the device (hidraw4), one per exercised control.
    const IDLE: &str = "454700000000000000001f01d5ff54023b03000000000000000000000000bbe35d009c00a1f45f3f000000000000";
    const A_ONLY: &str = "456d0100000000000000dd0093ff81023b0300000000000000000000000085cecc00860064f4d43f000004000000";
    const LT_FULL: &str = "450d00000008ce7d000029028bfee6010000000000000000000000000000ebc6e7029a0043f407400300ffff0300";
    const LX_PUSHED: &str = "45be00000001000000000f8695303e022303000000000000000000000000173b9d018f007ff4a93f0000fdff0000";
    const LPAD_TOUCH: &str = "453500000002000000001302ee0097022303fe7f0280000000000000000013c87a01690064f49f3f000000000000";

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn idle_frame_is_all_rest() {
        let s = parse(&hex(IDLE)).unwrap();
        assert_eq!(s.seq, 0x47);
        assert_eq!(s.buttons.0, 0);
        assert_eq!(s.left_trigger, 0);
        assert_eq!(s.right_trigger, 0);
        assert_eq!(s.left_stick, (287, -43));
        assert_eq!(s.right_stick, (596, 827));
        assert_eq!(s.left_pad, Pad::default());
        assert_eq!(s.right_pad, Pad::default());
        assert_eq!(s.imu.accel[2], 16223); // ~1 g, controller flat
        assert_eq!(s.imu.gyro[2], 0);
    }

    #[test]
    fn a_button_is_bit0() {
        let s = parse(&hex(A_ONLY)).unwrap();
        assert_eq!(s.buttons.0, Buttons::A);
        assert!(s.buttons.has(Buttons::A));
        assert!(!s.buttons.has(Buttons::B | Buttons::X | Buttons::Y));
        assert_eq!(s.left_trigger, 0);
    }

    #[test]
    fn left_trigger_full_sets_click() {
        let s = parse(&hex(LT_FULL)).unwrap();
        assert_eq!(s.left_trigger, 32206);
        assert_eq!(s.right_trigger, 0);
        assert_eq!(s.buttons.0, Buttons::LTRIGGER_CLICK);
        assert!(s.buttons.has(Buttons::LTRIGGER_CLICK));
    }

    #[test]
    fn left_stick_pushed_left_with_touch() {
        let s = parse(&hex(LX_PUSHED)).unwrap();
        assert_eq!(s.left_stick.0, -31217);
        assert!(s.buttons.has(Buttons::LSTICK_TOUCH));
    }

    #[test]
    fn left_pad_touch_reports_position() {
        let s = parse(&hex(LPAD_TOUCH)).unwrap();
        assert!(s.buttons.has(Buttons::LPAD_TOUCH));
        assert_eq!(s.left_pad.x, 32766);
        assert_eq!(s.left_pad.y, -32766);
    }

    #[test]
    fn accepts_usb_report_id() {
        let mut b = hex(IDLE);
        b[0] = REPORT_ID_INPUT_USB;
        assert!(parse(&b).is_some());
    }

    #[test]
    fn button_names_cover_every_bit_exactly_once() {
        assert_eq!(BUTTON_NAMES.len(), 30);
        let all = BUTTON_NAMES.iter().fold(0u32, |acc, &(_, bit)| {
            assert_eq!(bit.count_ones(), 1, "each entry is a single bit");
            assert_eq!(acc & bit, 0, "bit {bit:#x} listed twice");
            acc | bit
        });
        assert_eq!(all, (1 << 30) - 1); // bits 0..=29, no gaps
    }

    #[test]
    fn rejects_non_input_reports() {
        assert!(parse(&[0x40; INPUT_REPORT_LEN]).is_none()); // lizard mouse report
        assert!(parse(&[]).is_none());
        assert!(parse(&[0x45; 10]).is_none()); // right id, too short
    }
}
