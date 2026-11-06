# steampuck

## Disclaimer

This project was made with heavy use of a LLM.

## What

Use Valve's Steam Controller as a standard Xbox 360 pad on Linux — without
Steam running.

steampuck is a small userspace daemon. It reads the controller's HID reports,
mirrors them into a virtual Xbox 360 pad through uinput, and keeps the
controller's built-in mouse/keyboard emulation ("lizard mode") switched off so
it doesn't fight your game. Games, emulators, and anything else that speaks
XInput/SDL see an ordinary gamepad.

- **All three transports**: wireless Puck receiver (`28de:1304`), wired USB-C
  (`28de:1302`), and Bluetooth (`28de:1303`). They stream the same report, so
  they behave identically.
- **Rumble**: force feedback from the game is forwarded to the controller.
- **Gyro**: a DSU (cemuhook) motion server on `127.0.0.1:26760` feeds
  gyro and accelerometer data to Cemu, Dolphin, Citra, yuzu, RPCS3, and PPSSPP.
- **Mode chords**: button combos hand the controller back to Steam Input or
  toggle the mouse, each confirmed by a haptic chime.
- **Hotplug**: starts before the controller is connected and survives
  power-off, unplug, and replug. Held inputs are released on disconnect, so
  nothing sticks.
- **No root**: runs as your user.

Linux only. One controller at a time.

## Requirements

- Rust 1.88 or newer
- A C compiler, `pkg-config`, and the libudev headers (`libudev-dev` on
  Debian/Ubuntu, `systemd-devel` on Fedora). On Nix, `nix develop` provides them.
- Read/write access to the controller's hidraw nodes and to `/dev/uinput`.
  Steam's udev rules grant both: install your distro's `steam-devices` package,
  or set `hardware.steam-hardware.enable = true` on NixOS.

## Build and run

```sh
cargo build --release
./target/release/steampuck
```

On startup it prints the HID interfaces it opened, the virtual pad's
`/dev/input` nodes, and the active chords. Stop it with Ctrl-C.

### Run on login

`packaging/steampuck.service` is a systemd user unit. Its `ExecStart` is a
hard-coded path to the release binary, so point it at yours first — or run
`cargo install --path . --root ~/.local` and use `%h/.local/bin/steampuck`.

```sh
mkdir -p ~/.config/systemd/user
cp packaging/steampuck.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now steampuck.service
```

Logs: `journalctl --user -u steampuck -f`

## Modes

steampuck starts in Xbox mode. Three chords switch modes at runtime; hold all
the listed buttons at once.

| Chord (default)        | Action                                                              |
| ---------------------- | ------------------------------------------------------------------- |
| L4 + L5 + R4 + R5 + X  | **Xbox mode**: virtual pad and motion server on, mouse suppressed    |
| L4 + L5 + R4 + R5 + A  | **Steam mode**: remove the virtual pad and release the controller to Steam Input |
| L4 + L5 + R4 + R5 + B  | **Toggle the mouse** (lizard mode) on or off, in either mode         |

In Steam mode steampuck only watches for the chord back to Xbox mode. The
chord buttons still reach the game while you hold them.

## Button mapping

| Steam Controller      | Virtual pad                                   |
| --------------------- | --------------------------------------------- |
| A / B / X / Y         | A / B / X / Y                                 |
| LB / RB               | LB / RB                                       |
| Triggers              | LT / RT (analog)                              |
| Sticks, L3 / R3       | Sticks, stick clicks                          |
| D-pad                 | D-pad (hat)                                   |
| View / Menu / Steam   | Back / Start / Guide                          |
| R4, R5, L4, L5, QAM   | Extra buttons `BTN_TRIGGER_HAPPY1`–`5`        |

The back paddles and the QAM button have no Xbox 360 equivalent. They appear as
extra buttons that raw-joystick binding can see and SDL's Xbox mapping ignores.
Trackpads, grips, and the capacitive touch sensors are not mapped to the pad,
but all of them can be used in chords.

To remap, edit `BUTTON_MAP` in `src/pad.rs`.

## Configuration

steampuck writes an annotated default config to
`~/.config/steampuck/config.toml` (or `$XDG_CONFIG_HOME/steampuck/config.toml`)
on first run. The same file lives in the repo at
`packaging/config.example.toml`. Every key is optional.

```toml
[chords.xbox]
buttons  = ["L4", "L5", "R4", "R5", "X"]
feedback = "chime"            # "chime", "rumble", or "none"

[chords.xbox.chime]           # optional; overrides the built-in chime
start_hz = 200
end_hz   = 800
ms       = 250
gain_db  = 0

[chords.steam]
buttons  = ["L4", "L5", "R4", "R5", "A"]

[chords.mouse]
buttons  = ["L4", "L5", "R4", "R5", "B"]

[feedback]
rumble_strength = 50          # percent, for feedback = "rumble"

[motion]
gyro_scale = 16.384           # LSB per deg/s
```

Button names are case-insensitive:

```
A B X Y  LB RB  L3 R3  L4 L5 R4 R5  VIEW MENU STEAM QAM
DPAD_UP DPAD_DOWN DPAD_LEFT DPAD_RIGHT  LGRIP RGRIP
LPAD_TOUCH LPAD_CLICK RPAD_TOUCH RPAD_CLICK
LSTICK_TOUCH RSTICK_TOUCH  LTRIGGER_CLICK RTRIGGER_CLICK
```

A chord fires when all of its buttons are held; extra pressed buttons are
ignored. If two chords overlap, the one with more buttons wins.

A malformed config, including an unknown key, is reported on stderr and
ignored: the daemon starts with the defaults rather than leaving you without a
controller. Restart to apply changes:

```sh
systemctl --user restart steampuck
```

## Motion in emulators

Point the emulator's DSU / cemuhook client at `127.0.0.1`, port `26760`,
slot 0. The server reports motion only; buttons and sticks come from the
virtual pad, so nothing is doubled. If port 26760 is taken, steampuck logs it
and runs without motion.

`tools/dsu_probe.py` is a small DSU client that checks the handshake and
prints the streamed accel/gyro values:

```sh
python3 tools/dsu_probe.py
```

## How it works

The controller streams a 46-byte input report (id `0x45`, about 270 Hz) on
every transport. `src/lib.rs` is a pure codec for it, with no OS dependencies
and no unsafe code, tested against frames captured from the device. The
daemon is built around that codec, one module per subsystem:

| Module      | Role                                                                    |
| ----------- | ----------------------------------------------------------------------- |
| `link.rs`   | HID side: opens the interfaces, reads reports, sends everything bound for the controller |
| `pad.rs`    | The uinput Xbox 360 pad: state diffing and force feedback               |
| `lizard.rs` | Resends "lizard off" every 250 ms; the firmware re-enables the mouse after a few seconds |
| `rumble.rs` | Refreshes an active rumble every 40 ms; the hardware stops it after about 50 ms |
| `chords.rs` | Debounced chord detection                                               |
| `dsu.rs`    | The DSU motion server                                                   |
| `config.rs` | Config file loading and defaults                                        |
| `app.rs`    | Owns the subsystems and runs the main loop                              |

The report layout is cross-checked against SDL's `SDL_hidapi_steam_triton.c`
and the [openpuck](https://github.com/safijari/openpuck) project.

## Tests

```sh
cargo test
```
