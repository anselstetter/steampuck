//! steampuck — bridge a Steam Controller to a virtual Xbox 360 pad via uinput, so
//! games see a standard XInput controller with no Steam running. Works over all
//! three transports — the wireless "Puck" receiver (28de:1304), wired USB-C
//! (28de:1302), and direct Bluetooth (28de:1303, HID-over-GATT) — which all
//! present the same 0x45 input report.
//!
//! Reads the controller's HID interface(s) with hidapi, decodes each 0x45
//! report via the `steampuck` codec, and mirrors it into a uinput pad.
//! Runs as your user (hidraw + uinput carry a per-user ACL); no root needed.
//!
//! One module per subsystem, each owning its state and its tunables:
//! [`link`] (the HID side: interfaces + all controller-bound sends),
//! [`pad`] (the uinput pad + everything that dies with it),
//! [`rumble`] (the keep-alive latch), [`lizard`] (the mouse-suppression
//! heartbeat), [`chords`] (debounced mode chords), [`dsu`] (the cemuhook
//! motion server) — and [`app`], which owns them all and runs the loop.

use std::error::Error;

mod app;
mod chords;
mod config;
mod dsu;
mod link;
mod lizard;
mod pad;
mod rumble;

fn main() -> Result<(), Box<dyn Error>> {
    let mut app = app::App::new()?;
    loop {
        app.tick()?;
    }
}
