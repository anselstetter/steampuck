//! Minimal DSU ("cemuhook" UDP motion protocol, v1001) server.
//!
//! Streams the controller's gyro + accelerometer to emulators that speak DSU —
//! Cemu, Dolphin, Citra, yuzu, RPCS3, PPSSPP — which read motion from a UDP
//! server on `127.0.0.1:26760`. Buttons/sticks come from the evdev pad, so we
//! report a single slot 0 with *motion only* (neutral buttons/sticks) to avoid
//! double-input. Packet layout is verified against v1993/cemuhook-protocol.
//!
//! Encoding helpers (`frame`/`encode_pad`/`crc32`) are pure and unit-tested; the
//! `Server` wraps a non-blocking `UdpSocket` and is driven from the main loop:
//! `poll()` every iteration (answer requests), `send()` per fresh controller state.

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

const PROTOCOL_VERSION: u16 = 1001;
const MSG_VERSION: u32 = 0x100000;
const MSG_INFO: u32 = 0x100001;
const MSG_DATA: u32 = 0x100002;
/// Stop streaming to a client that hasn't asked for data in this long.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(5);
/// Stable fake MAC so clients recognise the device across launches.
const MAC: [u8; 6] = [0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e];

/// One motion sample, already converted to DSU units (accel in g, gyro in deg/s).
pub struct Motion {
    pub ts_us: u64,
    pub accel: [f32; 3],
    pub gyro: [f32; 3],
}

pub struct Server {
    sock: UdpSocket,
    id: u32,
    clients: HashMap<SocketAddr, Instant>, // subscribers -> last data request
    packet_no: u32,
}

impl Server {
    /// Bind the DSU port on localhost. `id` identifies this server run to clients.
    pub fn bind(port: u16, id: u32) -> io::Result<Server> {
        let sock = UdpSocket::bind(("127.0.0.1", port))?;
        sock.set_nonblocking(true)?;
        Ok(Server {
            sock,
            id,
            clients: HashMap::new(),
            packet_no: 0,
        })
    }

    /// Answer all pending requests. Non-blocking; call every loop iteration.
    pub fn poll(&mut self) {
        let mut buf = [0u8; 128];
        loop {
            match self.sock.recv_from(&mut buf) {
                Ok((n, from)) => self.handle(&buf[..n], from),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
        }
    }

    fn handle(&mut self, pkt: &[u8], from: SocketAddr) {
        // Header: DSUC magic @0, ver @4, len @6, crc @8, client id @12, event @16.
        // We are lenient on incoming CRC/version for interop.
        if pkt.len() < 20 || &pkt[0..4] != b"DSUC" {
            return;
        }
        let event = u32::from_le_bytes([pkt[16], pkt[17], pkt[18], pkt[19]]);
        match event {
            MSG_VERSION => {
                let out = frame(self.id, MSG_VERSION, &PROTOCOL_VERSION.to_le_bytes());
                let _ = self.sock.send_to(&out, from);
            }
            MSG_INFO => {
                // payload: count i32 @20, then that many slot bytes @24.
                if pkt.len() < 24 {
                    return;
                }
                let count = i32::from_le_bytes([pkt[20], pkt[21], pkt[22], pkt[23]]).clamp(0, 4);
                for i in 0..count as usize {
                    let Some(&slot) = pkt.get(24 + i) else { break };
                    let mut payload = shared_begin(slot).to_vec();
                    payload.push(0); // trailing zero byte
                    let out = frame(self.id, MSG_INFO, &payload);
                    let _ = self.sock.send_to(&out, from);
                }
            }
            MSG_DATA => {
                // Subscribe this client; `send` streams pad data to it until timeout.
                self.clients.insert(from, Instant::now());
            }
            _ => {}
        }
    }

    /// Broadcast one pad-data packet to every active subscriber. Call once per
    /// fresh controller state. No-op when nobody is listening.
    pub fn send(&mut self, m: &Motion) {
        self.clients.retain(|_, t| t.elapsed() < CLIENT_TIMEOUT);
        if self.clients.is_empty() {
            return;
        }
        self.packet_no = self.packet_no.wrapping_add(1);
        let out = frame(self.id, MSG_DATA, &encode_pad(self.packet_no, m));
        for a in self.clients.keys() {
            let _ = self.sock.send_to(&out, a);
        }
    }
}

/// The 11-byte "shared beginning" both info and data responses start with.
/// Only slot 0 is reported connected (full gyro, wireless).
fn shared_begin(slot: u8) -> [u8; 11] {
    let mut p = [0u8; 11];
    p[0] = slot;
    if slot == 0 {
        p[1] = 2; // slot state: connected
        p[2] = 2; // device model: full gyro
        p[3] = 2; // connection: bluetooth (wireless receiver)
        p[4..10].copy_from_slice(&MAC);
        p[10] = 0x05; // battery: full
    }
    p
}

/// Encode the 80-byte pad-data payload: neutral buttons/sticks/touch + motion.
fn encode_pad(packet_no: u32, m: &Motion) -> Vec<u8> {
    let mut p = Vec::with_capacity(80);
    p.extend_from_slice(&shared_begin(0)); // 11
    p.push(1); // controller connected
    p.extend_from_slice(&packet_no.to_le_bytes()); // 4
    p.extend_from_slice(&[0, 0, 0, 0]); // buttons1, buttons2, home, touch (none)
    p.extend_from_slice(&[128, 128, 128, 128]); // LX, LY, RX, RY (neutral)
    p.extend_from_slice(&[0u8; 12]); // analog dpad/buttons
    p.extend_from_slice(&[0u8; 12]); // two touch points (inactive)
    p.extend_from_slice(&m.ts_us.to_le_bytes()); // 8
    for v in m.accel {
        p.extend_from_slice(&v.to_le_bytes());
    }
    for v in m.gyro {
        p.extend_from_slice(&v.to_le_bytes());
    }
    debug_assert_eq!(p.len(), 80);
    p
}

/// Wrap `payload` (which follows the event-type field) in the 20-byte DSU header,
/// filling in the length and CRC32 (computed over the whole packet, CRC zeroed).
fn frame(id: u32, event: u32, payload: &[u8]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(20 + payload.len());
    pkt.extend_from_slice(b"DSUS");
    pkt.extend_from_slice(&PROTOCOL_VERSION.to_le_bytes());
    pkt.extend_from_slice(&((4 + payload.len()) as u16).to_le_bytes()); // len sans 16B header
    pkt.extend_from_slice(&[0, 0, 0, 0]); // CRC placeholder
    pkt.extend_from_slice(&id.to_le_bytes());
    pkt.extend_from_slice(&event.to_le_bytes());
    pkt.extend_from_slice(payload);
    let crc = crc32(&pkt);
    pkt[8..12].copy_from_slice(&crc.to_le_bytes());
    pkt
}

/// Standard CRC-32 (IEEE, reflected, poly 0xEDB88320) — the one DSU expects.
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_standard_check_value() {
        // The canonical CRC-32 test vector.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn data_packet_layout_and_crc() {
        let m = Motion {
            ts_us: 0xdead_beef,
            accel: [1.0, 2.0, 3.0],
            gyro: [4.0, 5.0, 6.0],
        };
        let payload = encode_pad(7, &m);
        assert_eq!(payload.len(), 80);

        let pkt = frame(0xAABB_CCDD, MSG_DATA, &payload);
        assert_eq!(pkt.len(), 100, "spec: pad-data packet is 100 bytes total");
        assert_eq!(&pkt[0..4], b"DSUS");
        assert_eq!(u16::from_le_bytes([pkt[4], pkt[5]]), 1001);
        assert_eq!(u16::from_le_bytes([pkt[6], pkt[7]]), 84); // 4 (event) + 80 payload
        assert_eq!(
            u32::from_le_bytes([pkt[16], pkt[17], pkt[18], pkt[19]]),
            MSG_DATA
        );

        // Motion lands at payload 48/56/68 -> packet 68/76/88.
        assert_eq!(u64::from_le_bytes(pkt[68..76].try_into().unwrap()), 0xdead_beef);
        assert_eq!(f32::from_le_bytes(pkt[76..80].try_into().unwrap()), 1.0); // accel X
        assert_eq!(f32::from_le_bytes(pkt[84..88].try_into().unwrap()), 3.0); // accel Z
        assert_eq!(f32::from_le_bytes(pkt[88..92].try_into().unwrap()), 4.0); // gyro pitch
        assert_eq!(f32::from_le_bytes(pkt[96..100].try_into().unwrap()), 6.0); // gyro roll

        // CRC verifies when the field is re-zeroed.
        let stored = u32::from_le_bytes([pkt[8], pkt[9], pkt[10], pkt[11]]);
        let mut zeroed = pkt.clone();
        zeroed[8..12].copy_from_slice(&[0; 4]);
        assert_eq!(crc32(&zeroed), stored);
        assert_ne!(stored, 0);
    }

    #[test]
    fn info_response_is_32_bytes_total() {
        let mut payload = shared_begin(0).to_vec();
        payload.push(0);
        let pkt = frame(1, MSG_INFO, &payload);
        assert_eq!(pkt.len(), 32);
        assert_eq!(pkt[20], 0); // slot 0
        assert_eq!(pkt[21], 2); // connected
        assert_eq!(pkt[22], 2); // full gyro
    }
}
