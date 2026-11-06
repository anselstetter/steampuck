//! Config file: `$XDG_CONFIG_HOME/steampuck/config.toml` (usually
//! `~/.config/steampuck/config.toml`). The single source of configuration —
//! there are no env-var knobs.
//!
//! Missing file = built-in defaults, silently. Malformed file = loud stderr
//! warning + defaults: an input daemon that refuses to start over a typo
//! leaves you without a controller after a reboot. Unknown keys are treated
//! as malformed (`deny_unknown_fields`) so typos are loud, not silent.
//!
//! See `packaging/config.example.toml` for a full annotated example.

use std::path::PathBuf;

use serde::Deserialize;

use crate::link::Sweep;

/// Default chords, shared with the fallback path for unresolvable button names.
pub const DEFAULT_XBOX_BUTTONS: &[&str] = &["L4", "L5", "R4", "R5", "X"];
pub const DEFAULT_STEAM_BUTTONS: &[&str] = &["L4", "L5", "R4", "R5", "A"];
pub const DEFAULT_MOUSE_BUTTONS: &[&str] = &["L4", "L5", "R4", "R5", "B"];

#[derive(Deserialize, Default, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub chords: ChordsCfg,
    #[serde(default)]
    pub feedback: FeedbackCfg,
    #[serde(default)]
    pub motion: MotionCfg,
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChordsCfg {
    #[serde(default = "default_xbox")]
    pub xbox: ChordCfg,
    #[serde(default = "default_steam")]
    pub steam: ChordCfg,
    #[serde(default = "default_mouse")]
    pub mouse: ChordCfg,
}

impl Default for ChordsCfg {
    /// Defaults ARE the empty config — the per-field serde default fns are the
    /// single definition; a hand-written mirror here would be a second copy.
    fn default() -> ChordsCfg {
        toml::from_str("").expect("empty config parses")
    }
}

/// One chord: the buttons that must all be held, and what the controller does
/// to confirm the chord fired.
#[derive(Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChordCfg {
    /// Button names per `steampuck::BUTTON_NAMES` (case-insensitive).
    pub buttons: Vec<String>,
    #[serde(default)]
    pub feedback: Feedback,
    /// Chime parameters when `feedback = "chime"`; omitted = the built-in
    /// palette (xbox rising, steam falling, mouse blip).
    #[serde(default)]
    pub chime: Option<ChimeCfg>,
}

/// Haptic confirmation when a chord fires.
#[derive(Deserialize, Clone, Copy, PartialEq, Debug, Default)]
#[serde(rename_all = "lowercase")]
pub enum Feedback {
    #[default]
    Chime,
    Rumble,
    None,
}

/// A `0x84` log-sweep chime, config-shaped. Mirrors [`Sweep`].
#[derive(Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ChimeCfg {
    pub start_hz: u16,
    pub end_hz: u16,
    pub ms: u16,
    /// 0 = firmware default volume; positive = louder, negative = quieter (dB).
    #[serde(default)]
    pub gain_db: i8,
}

impl ChimeCfg {
    pub fn sweep(&self) -> Sweep {
        Sweep { start_hz: self.start_hz, end_hz: self.end_hz, ms: self.ms, gain_db: self.gain_db }
    }
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeedbackCfg {
    /// Rumble-tick magnitude for `feedback = "rumble"`, percent 0-100.
    #[serde(default = "default_rumble_strength")]
    pub rumble_strength: u32,
}

impl Default for FeedbackCfg {
    fn default() -> FeedbackCfg {
        toml::from_str("").expect("empty config parses")
    }
}

impl FeedbackCfg {
    /// The tick magnitude as the raw u16 the rumble report takes.
    pub fn rumble_magnitude(&self) -> u16 {
        (self.rumble_strength.min(100) * 0xffff / 100) as u16
    }
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MotionCfg {
    /// Gyro scale in LSB per deg/s. Default 32768/2000 = 16.384 — SDL pins the
    /// gyro's full scale at ±2000 deg/s over the s16 range.
    #[serde(default = "default_gyro_scale")]
    pub gyro_scale: f32,
}

impl Default for MotionCfg {
    fn default() -> MotionCfg {
        toml::from_str("").expect("empty config parses")
    }
}

impl MotionCfg {
    /// The gyro scale, guarded against nonsense values.
    pub fn scale(&self) -> f32 {
        if self.gyro_scale > 0.0 {
            self.gyro_scale
        } else {
            eprintln!("config: gyro_scale must be > 0 — using {}", default_gyro_scale());
            default_gyro_scale()
        }
    }
}

fn strings(names: &[&str]) -> Vec<String> {
    names.iter().map(|s| s.to_string()).collect()
}

fn default_chord(buttons: &[&str]) -> ChordCfg {
    ChordCfg { buttons: strings(buttons), feedback: Feedback::Chime, chime: None }
}
fn default_xbox() -> ChordCfg {
    default_chord(DEFAULT_XBOX_BUTTONS)
}
fn default_steam() -> ChordCfg {
    default_chord(DEFAULT_STEAM_BUTTONS)
}
fn default_mouse() -> ChordCfg {
    default_chord(DEFAULT_MOUSE_BUTTONS)
}
fn default_rumble_strength() -> u32 {
    50
}
fn default_gyro_scale() -> f32 {
    16.384
}

/// `$XDG_CONFIG_HOME/steampuck/config.toml`, falling back to `~/.config`.
fn path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute()) // XDG spec: ignore relative paths
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("steampuck/config.toml")
}

/// The annotated default config, embedded verbatim from the repo; written to
/// the config path on first run so there's a real file to edit. A test pins it
/// to parse exactly to `Config::default()` so file and code can't drift.
const DEFAULT_CONFIG: &str = include_str!("../packaging/config.example.toml");

/// Load the config; on first run, materialize the annotated defaults.
pub fn load() -> Config {
    let path = path();
    match std::fs::read_to_string(&path) {
        Ok(s) => match toml::from_str(&s) {
            Ok(cfg) => {
                eprintln!("config: {}", path.display());
                cfg
            }
            Err(e) => {
                eprintln!("config error in {}:\n{e}-> using built-in defaults", path.display());
                Config::default()
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Failure to write is not fatal — the defaults apply either way.
            let write = path
                .parent()
                .map_or(Ok(()), std::fs::create_dir_all)
                .and_then(|()| std::fs::write(&path, DEFAULT_CONFIG));
            match write {
                Ok(()) => eprintln!("config: wrote defaults to {}", path.display()),
                Err(e) => {
                    eprintln!("config: could not write defaults to {}: {e}", path.display())
                }
            }
            Config::default()
        }
        Err(e) => {
            eprintln!("config: cannot read {}: {e} — using defaults", path.display());
            Config::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_example_parses_to_the_defaults() {
        // The file written on first run must BE the defaults — if this fails,
        // packaging/config.example.toml and the code have drifted apart.
        let cfg: Config = toml::from_str(DEFAULT_CONFIG).expect("example must parse");
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn empty_input_is_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg, Config::default());
        assert_eq!(cfg.chords.xbox.buttons, DEFAULT_XBOX_BUTTONS);
        assert_eq!(cfg.feedback.rumble_strength, 50);
        assert_eq!(cfg.motion.gyro_scale, 16.384);
    }

    #[test]
    fn parses_full_example() {
        let cfg: Config = toml::from_str(
            r#"
            [chords.xbox]
            buttons  = ["L4", "L5", "X"]
            feedback = "chime"
            [chords.xbox.chime]
            start_hz = 100
            end_hz   = 900
            ms       = 300

            [chords.steam]
            buttons  = ["STEAM", "A"]
            feedback = "none"

            [chords.mouse]
            buttons  = ["L4", "L5", "B"]
            feedback = "rumble"

            [feedback]
            rumble_strength = 80

            [motion]
            gyro_scale = 17.0
            "#,
        )
        .unwrap();
        assert_eq!(cfg.chords.xbox.buttons, ["L4", "L5", "X"]);
        let chime = cfg.chords.xbox.chime.unwrap();
        assert_eq!((chime.start_hz, chime.end_hz, chime.ms, chime.gain_db), (100, 900, 300, 0));
        assert_eq!(cfg.chords.steam.feedback, Feedback::None);
        assert_eq!(cfg.chords.mouse.feedback, Feedback::Rumble);
        assert_eq!(cfg.feedback.rumble_strength, 80);
        assert_eq!(cfg.motion.gyro_scale, 17.0);
    }

    #[test]
    fn unknown_keys_are_errors() {
        assert!(toml::from_str::<Config>("[chords.xbox]\nbutton = [\"X\"]").is_err()); // typo'd key
        assert!(toml::from_str::<Config>("[chordz]").is_err()); // typo'd section
    }

    #[test]
    fn rumble_magnitude_scales_and_clamps() {
        assert_eq!(FeedbackCfg { rumble_strength: 0 }.rumble_magnitude(), 0);
        assert_eq!(FeedbackCfg { rumble_strength: 100 }.rumble_magnitude(), 0xffff);
        assert_eq!(FeedbackCfg { rumble_strength: 250 }.rumble_magnitude(), 0xffff);
    }

    #[test]
    fn bad_gyro_scale_falls_back() {
        assert_eq!(MotionCfg { gyro_scale: -1.0 }.scale(), 16.384);
        assert_eq!(MotionCfg { gyro_scale: 20.0 }.scale(), 20.0);
    }
}
