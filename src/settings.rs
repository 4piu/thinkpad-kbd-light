//! Persisted configuration: one profile per power source.
//!
//! See `notes/02-design-spec.md` §4 and §5.

use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::backlight::Level;

/// Which profile applies right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PowerSource {
    Ac,
    Battery,
}

/// Behaviour for one power source.
///
/// Some combinations are meaningless rather than invalid, and are resolved by the
/// `effective_*` accessors rather than rejected:
///
/// * `active_level == Off` — the backlight is off already, so neither timer does anything.
/// * `active_level == Low` — there is no room to dim before turning off, so the dim timer
///   is inert and only the off timer applies.
///
/// Keeping this logic here, rather than in the UI, means the state machine cannot act on a
/// combination the user could not see.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub enabled: bool,
    pub active_level: Level,
    /// Seconds of idle before dimming. `None` = never dim.
    pub dim_after_secs: Option<u32>,
    /// Seconds of idle before turning off. `None` = never turn off.
    pub off_after_secs: Option<u32>,
}

impl Profile {
    /// How long to wait before dimming, or `None` if dimming cannot apply.
    pub fn effective_dim_after(&self) -> Option<Duration> {
        if !self.enabled || self.active_level <= Level::Low {
            return None;
        }
        self.dim_after_secs.map(|s| Duration::from_secs(s as u64))
    }

    /// How long to wait before turning off, or `None` if that cannot apply.
    pub fn effective_off_after(&self) -> Option<Duration> {
        if !self.enabled || self.active_level == Level::Off {
            return None;
        }
        self.off_after_secs.map(|s| Duration::from_secs(s as u64))
    }

    /// The level the dim step drops to: always one below the active level.
    ///
    /// There is no separate setting for this. With three levels, the only choice that is
    /// not already expressible through the two timers is "the level below active" — any
    /// other target simply turns the dim timer into a second off timer. See the note in
    /// `notes/02-design-spec.md` §4.
    pub fn effective_dim_level(&self) -> Level {
        match self.active_level {
            Level::High => Level::Low,
            _ => Level::Off,
        }
    }

    /// Human-readable problems, for the settings UI to surface.
    pub fn validate(&self) -> Result<(), String> {
        if let (Some(dim), Some(off)) = (self.dim_after_secs, self.off_after_secs) {
            if dim >= off {
                return Err(format!(
                    "dim timeout ({dim}s) must be shorter than the off timeout ({off}s)"
                ));
            }
        }
        Ok(())
    }
}

/// The whole configuration file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    /// Written but not yet read. Kept only because it is the one thing that genuinely
    /// cannot be added retroactively: without it, a future version has no way to tell an
    /// old file from a new one. No compatibility is promised until there is a release —
    /// if the schema changes, delete the file.
    pub version: u32,
    pub ac: Profile,
    pub battery: Profile,
    /// Force the backlight off while the session is locked. Nobody is typing.
    pub turn_off_when_locked: bool,
    /// On exit, restore the profile's active level rather than leaving it dark.
    pub restore_on_exit: bool,
    pub start_with_windows: bool,
}

pub const CURRENT_VERSION: u32 = 1;

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            ac: Profile {
                enabled: true,
                active_level: Level::High,
                dim_after_secs: Some(30),
                off_after_secs: Some(300),
            },
            // On battery: start low, no intermediate step, go dark quickly.
            battery: Profile {
                enabled: true,
                active_level: Level::Low,
                dim_after_secs: None,
                off_after_secs: Some(60),
            },
            turn_off_when_locked: true,
            restore_on_exit: true,
            start_with_windows: false,
        }
    }
}

impl Settings {
    pub fn profile(&self, source: PowerSource) -> &Profile {
        match source {
            PowerSource::Ac => &self.ac,
            PowerSource::Battery => &self.battery,
        }
    }

    pub fn profile_mut(&mut self, source: PowerSource) -> &mut Profile {
        match source {
            PowerSource::Ac => &mut self.ac,
            PowerSource::Battery => &mut self.battery,
        }
    }

    /// `%LOCALAPPDATA%\ThinkpadKbdLight\settings.json`
    pub fn default_path() -> io::Result<PathBuf> {
        let base = std::env::var("LOCALAPPDATA")
            .map_err(|_| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set"))?;
        Ok(PathBuf::from(base)
            .join("ThinkpadKbdLight")
            .join("settings.json"))
    }

    pub fn load(path: &PathBuf) -> io::Result<Self> {
        let text = fs::read_to_string(path)?;
        // Tolerate a UTF-8 BOM. Notepad and PowerShell's `-Encoding utf8` both write one,
        // and serde_json treats it as a syntax error — so without this, hand-editing the
        // config on Windows can leave the app refusing to start.
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        serde_json::from_str(text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
    }

    /// Load, falling back to defaults if the file is absent.
    ///
    /// A *corrupt* file is still an error: silently replacing a user's configuration
    /// because one character is wrong would be worse than refusing to start.
    pub fn load_or_default(path: &PathBuf) -> io::Result<Self> {
        match Self::load(path) {
            Ok(s) => Ok(s),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e),
        }
    }

    /// Write atomically: a settings file truncated by a power loss on a
    /// battery-powered laptop is not a hypothetical.
    pub fn save(&self, path: &PathBuf) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json.as_bytes())?;
        // On Windows std::fs::rename replaces an existing destination atomically.
        fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_validate() {
        let s = Settings::default();
        assert!(s.ac.validate().is_ok());
        assert!(s.battery.validate().is_ok());
    }

    #[test]
    fn active_low_disables_the_dim_step() {
        let p = Profile {
            enabled: true,
            active_level: Level::Low,
            dim_after_secs: Some(10),
            off_after_secs: Some(60),
        };
        assert_eq!(p.effective_dim_after(), None, "no room to dim below low");
        assert_eq!(p.effective_off_after(), Some(Duration::from_secs(60)));
    }

    #[test]
    fn active_off_disables_both_timers() {
        let p = Profile {
            enabled: true,
            active_level: Level::Off,
            dim_after_secs: Some(10),
            off_after_secs: Some(60),
        };
        assert_eq!(p.effective_dim_after(), None);
        assert_eq!(p.effective_off_after(), None);
    }

    #[test]
    fn dim_target_is_always_one_below_active() {
        let mut p = Settings::default().ac;
        p.active_level = Level::High;
        assert_eq!(p.effective_dim_level(), Level::Low);
        p.active_level = Level::Low;
        assert_eq!(p.effective_dim_level(), Level::Off);
    }

    #[test]
    fn disabled_profile_drives_nothing() {
        let mut p = Settings::default().ac;
        p.enabled = false;
        assert_eq!(p.effective_dim_after(), None);
        assert_eq!(p.effective_off_after(), None);
    }

    #[test]
    fn timers_are_independently_optional() {
        let mut p = Settings::default().ac;
        p.dim_after_secs = None;
        assert_eq!(p.effective_dim_after(), None);
        assert!(p.effective_off_after().is_some());

        p.dim_after_secs = Some(30);
        p.off_after_secs = None;
        assert!(p.effective_dim_after().is_some());
        assert_eq!(p.effective_off_after(), None);
    }

    #[test]
    fn dim_must_precede_off() {
        let mut p = Settings::default().ac;
        p.dim_after_secs = Some(300);
        p.off_after_secs = Some(30);
        assert!(p.validate().is_err());
    }

    #[test]
    fn json_roundtrips() {
        let s = Settings::default();
        let text = serde_json::to_string_pretty(&s).unwrap();
        let back: Settings = serde_json::from_str(&text).unwrap();
        assert_eq!(s, back);
        // Levels are human-editable names, not magic numbers.
        assert!(text.contains("\"high\""));
    }

    #[test]
    fn save_then_load_roundtrips() {
        let mut path = std::env::temp_dir();
        path.push(format!("tkbl-test-{}.json", std::process::id()));
        let mut s = Settings::default();
        s.ac.dim_after_secs = Some(42);
        s.save(&path).unwrap();
        let back = Settings::load(&path).unwrap();
        assert_eq!(s, back);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_utf8_bom_does_not_break_loading() {
        // Notepad and PowerShell's -Encoding utf8 both emit one.
        let path = std::env::temp_dir().join(format!("tkbl-bom-{}.json", std::process::id()));
        let json = serde_json::to_string_pretty(&Settings::default()).unwrap();
        std::fs::write(&path, format!("\u{feff}{json}")).unwrap();
        assert_eq!(Settings::load(&path).unwrap(), Settings::default());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_yields_defaults_but_corrupt_file_errors() {
        let missing = std::env::temp_dir().join("tkbl-does-not-exist-xyz.json");
        let _ = std::fs::remove_file(&missing);
        assert_eq!(
            Settings::load_or_default(&missing).unwrap(),
            Settings::default()
        );

        let bad = std::env::temp_dir().join(format!("tkbl-bad-{}.json", std::process::id()));
        std::fs::write(&bad, b"{ not json").unwrap();
        assert!(Settings::load_or_default(&bad).is_err());
        let _ = std::fs::remove_file(&bad);
    }
}
