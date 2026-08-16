//! The idle state machine. Pure logic — no clock, no I/O, no Windows.
//!
//! Time is always passed in, so the whole transition table is unit-testable. See
//! `notes/02-design-spec.md` §3.2.

use std::time::Instant;

use crate::backlight::Level;
use crate::settings::{PowerSource, Settings};

/// What the machine currently thinks it is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Recently used; holding the profile's active level.
    Active,
    /// Idle past the dim timeout.
    Dim,
    /// Idle past the off timeout, or forced off because the session is locked.
    Off,
    /// The user changed the level behind our back (Fn+Space). We stop driving.
    Suspended,
    /// The profile for this power source is switched off.
    Disabled,
}

pub struct Engine {
    settings: Settings,
    source: PowerSource,
    locked: bool,
    suspended: bool,
    last_input: Instant,
    /// The level we last successfully wrote, used to spot external changes.
    last_commanded: Option<Level>,
    phase: Phase,
}

impl Engine {
    pub fn new(settings: Settings, source: PowerSource, now: Instant) -> Self {
        Self {
            settings,
            source,
            locked: false,
            suspended: false,
            last_input: now,
            last_commanded: None,
            phase: Phase::Active,
        }
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn power_source(&self) -> PowerSource {
        self.source
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn is_suspended(&self) -> bool {
        self.suspended
    }

    pub fn replace_settings(&mut self, settings: Settings) {
        self.settings = settings;
    }

    /// Any keyboard, mouse or touchpad activity.
    pub fn on_input(&mut self, now: Instant) {
        self.last_input = now;
    }

    /// AC ↔ battery. Deliberately does *not* clear a manual override: the user's
    /// Fn+Space press is about the light, not about the power source.
    pub fn set_power_source(&mut self, source: PowerSource) {
        self.source = source;
    }

    /// Session lock and unlock. Unlocking counts as activity.
    pub fn set_locked(&mut self, locked: bool, now: Instant) {
        if self.locked && !locked {
            self.last_input = now;
        }
        self.locked = locked;
    }

    /// Wake from sleep, display-on, or unlock.
    ///
    /// Clears a manual override (the user chose "until next resume") and forgets what we
    /// last wrote, so the next `tick` produces a level that the caller will apply
    /// unconditionally. That last part is the whole point of the application: the firmware
    /// forgets the level across sleep, so we must assert it rather than assume it holds.
    pub fn on_resume(&mut self, now: Instant) {
        self.suspended = false;
        self.last_input = now;
        self.last_commanded = None;
    }

    /// Report a level observed from the hardware.
    ///
    /// If it differs from what we last wrote, the user pressed Fn+Space and we back off.
    /// Without this the machine would stomp their keypress on the next tick and the
    /// laptop would feel haunted.
    pub fn on_observed_level(&mut self, observed: Level) {
        if let Some(commanded) = self.last_commanded {
            if observed != commanded {
                self.suspended = true;
            }
        }
    }

    /// Stop driving because the user chose a level by hand.
    ///
    /// Same state as an Fn+Space override, and cleared the same way: on the next resume,
    /// or when the user explicitly hands control back.
    pub fn pause(&mut self) {
        self.suspended = true;
    }

    /// Give up a manual override on the user's say-so (tray menu).
    pub fn resume_control(&mut self) {
        self.suspended = false;
        self.last_commanded = None;
    }

    /// Record that `level` was written successfully.
    pub fn note_applied(&mut self, level: Level) {
        self.last_commanded = Some(level);
    }

    /// The level the backlight should be at now, or `None` if we should not drive it.
    ///
    /// Callers apply the result and then call [`Engine::note_applied`].
    pub fn tick(&mut self, now: Instant) -> Option<Level> {
        let profile = self.settings.profile(self.source);

        if !profile.enabled {
            self.phase = Phase::Disabled;
            return None;
        }
        if self.suspended {
            self.phase = Phase::Suspended;
            return None;
        }
        if self.locked && self.settings.turn_off_when_locked {
            self.phase = Phase::Off;
            return Some(Level::Off);
        }

        // Both timeouts are measured from the last input, not from entry into the previous
        // phase: it is simpler to reason about and matches how people describe it.
        let idle = now.saturating_duration_since(self.last_input);

        if let Some(off_after) = profile.effective_off_after() {
            if idle >= off_after {
                self.phase = Phase::Off;
                return Some(Level::Off);
            }
        }
        if let Some(dim_after) = profile.effective_dim_after() {
            if idle >= dim_after {
                self.phase = Phase::Dim;
                return Some(profile.effective_dim_level());
            }
        }

        self.phase = Phase::Active;
        Some(profile.active_level)
    }

    /// Level to leave behind on exit, so quitting never leaves a dark keyboard.
    pub fn exit_level(&self) -> Option<Level> {
        if self.settings.restore_on_exit {
            Some(self.settings.profile(self.source).active_level)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Profile;
    use std::time::Duration;

    fn engine() -> (Engine, Instant) {
        let now = Instant::now();
        (Engine::new(Settings::default(), PowerSource::Ac, now), now)
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn starts_active_at_the_profile_level() {
        let (mut e, now) = engine();
        assert_eq!(e.tick(now), Some(Level::High));
        assert_eq!(e.phase(), Phase::Active);
    }

    #[test]
    fn dims_then_turns_off() {
        let (mut e, now) = engine(); // ac: dim 30s, off 300s
        assert_eq!(e.tick(now + secs(29)), Some(Level::High));
        assert_eq!(e.tick(now + secs(30)), Some(Level::Low));
        assert_eq!(e.phase(), Phase::Dim);
        assert_eq!(e.tick(now + secs(299)), Some(Level::Low));
        assert_eq!(e.tick(now + secs(300)), Some(Level::Off));
        assert_eq!(e.phase(), Phase::Off);
    }

    #[test]
    fn off_timeout_is_measured_from_last_input_not_from_dimming() {
        let (mut e, now) = engine();
        e.tick(now + secs(40)); // already dim
                                // 300s after input, not 300s after the dim transition
        assert_eq!(e.tick(now + secs(299)), Some(Level::Low));
        assert_eq!(e.tick(now + secs(300)), Some(Level::Off));
    }

    #[test]
    fn input_returns_to_active_and_resets_both_timers() {
        let (mut e, now) = engine();
        assert_eq!(e.tick(now + secs(400)), Some(Level::Off));
        e.on_input(now + secs(400));
        assert_eq!(e.tick(now + secs(400)), Some(Level::High));
        assert_eq!(e.tick(now + secs(429)), Some(Level::High));
        assert_eq!(e.tick(now + secs(430)), Some(Level::Low));
    }

    #[test]
    fn disabled_profile_drives_nothing() {
        let (mut e, now) = engine();
        let s = Settings {
            ac: Profile {
                enabled: false,
                ..Settings::default().ac
            },
            ..Settings::default()
        };
        e.replace_settings(s);
        assert_eq!(e.tick(now), None);
        assert_eq!(e.phase(), Phase::Disabled);
    }

    #[test]
    fn external_change_suspends_and_stops_driving() {
        let (mut e, now) = engine();
        let level = e.tick(now).unwrap();
        e.note_applied(level);

        e.on_observed_level(Level::Off); // user hit Fn+Space
        assert!(e.is_suspended());
        assert_eq!(e.tick(now), None);
        assert_eq!(e.phase(), Phase::Suspended);
    }

    #[test]
    fn observing_our_own_value_does_not_suspend() {
        let (mut e, now) = engine();
        let level = e.tick(now).unwrap();
        e.note_applied(level);
        e.on_observed_level(level);
        assert!(!e.is_suspended());
    }

    #[test]
    fn suspension_survives_input_and_power_change_but_not_resume() {
        let (mut e, now) = engine();
        let level = e.tick(now).unwrap();
        e.note_applied(level);
        e.on_observed_level(Level::Off);

        e.on_input(now + secs(1));
        e.set_power_source(PowerSource::Battery);
        assert!(e.is_suspended(), "override must outlive ordinary events");

        e.on_resume(now + secs(2));
        assert!(!e.is_suspended());
        assert_eq!(e.tick(now + secs(2)), Some(Level::Low)); // battery active level
    }

    #[test]
    fn resume_forgets_what_we_wrote_so_the_next_apply_is_unconditional() {
        let (mut e, now) = engine();
        let level = e.tick(now).unwrap();
        e.note_applied(level);
        e.on_resume(now + secs(1));
        // Nothing is remembered, so a stale cache cannot suppress the re-apply.
        e.on_observed_level(Level::Off);
        assert!(!e.is_suspended());
    }

    #[test]
    fn lock_forces_off_and_unlock_counts_as_input() {
        let (mut e, now) = engine();
        e.set_locked(true, now);
        assert_eq!(e.tick(now), Some(Level::Off));
        assert_eq!(e.phase(), Phase::Off);

        e.set_locked(false, now + secs(500));
        assert_eq!(e.tick(now + secs(500)), Some(Level::High));
    }

    #[test]
    fn lock_is_ignored_when_the_setting_is_off() {
        let (mut e, now) = engine();
        e.replace_settings(Settings {
            turn_off_when_locked: false,
            ..Settings::default()
        });
        e.set_locked(true, now);
        assert_eq!(e.tick(now), Some(Level::High));
    }

    #[test]
    fn battery_profile_skips_the_dim_step() {
        let (mut e, now) = engine();
        e.set_power_source(PowerSource::Battery); // low active, no dim, off at 60s
        assert_eq!(e.tick(now), Some(Level::Low));
        assert_eq!(e.tick(now + secs(59)), Some(Level::Low));
        assert_eq!(e.tick(now + secs(60)), Some(Level::Off));
    }

    #[test]
    fn power_source_switch_changes_the_level_immediately() {
        let (mut e, now) = engine();
        assert_eq!(e.tick(now), Some(Level::High));
        e.set_power_source(PowerSource::Battery);
        assert_eq!(e.tick(now), Some(Level::Low));
    }

    #[test]
    fn timeouts_can_be_disabled_independently() {
        let (mut e, now) = engine();
        e.replace_settings(Settings {
            // dim, but never turn off
            ac: Profile {
                off_after_secs: None,
                ..Settings::default().ac
            },
            ..Settings::default()
        });
        assert_eq!(e.tick(now + secs(30)), Some(Level::Low));
        assert_eq!(e.tick(now + secs(100_000)), Some(Level::Low));
    }

    #[test]
    fn exit_restores_the_active_level() {
        let (e, _) = engine();
        assert_eq!(e.exit_level(), Some(Level::High));

        let s = Settings {
            restore_on_exit: false,
            ..Settings::default()
        };
        let mut e2 = Engine::new(s, PowerSource::Ac, Instant::now());
        assert_eq!(e2.exit_level(), None);
        e2.on_input(Instant::now());
    }

    #[test]
    fn active_low_profile_goes_straight_to_off() {
        let s = Settings {
            ac: Profile {
                enabled: true,
                active_level: Level::Low,
                dim_level: Level::Off,
                dim_after_secs: Some(10),
                off_after_secs: Some(60),
            },
            ..Settings::default()
        };
        let now = Instant::now();
        let mut e = Engine::new(s, PowerSource::Ac, now);
        assert_eq!(e.tick(now), Some(Level::Low));
        assert_eq!(
            e.tick(now + secs(11)),
            Some(Level::Low),
            "dim step is inert"
        );
        assert_eq!(e.tick(now + secs(60)), Some(Level::Off));
    }
}
