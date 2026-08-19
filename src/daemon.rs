//! The run loop, independent of any front end.
//!
//! `tkbld` drives this from a console; the tray application drives the same thing behind
//! an icon. Keeping it here means the behaviour cannot drift between the two.

use std::io;
use std::time::{Duration, Instant};

use crate::backlight::BacklightController;
use crate::engine::Phase;
use crate::osd::OsdSuppressor;
use crate::platform::{self, EventPump, PlatformEvent};
use crate::{Backlight, Engine, Level, Settings};

/// State-machine cadence. `GetLastInputInfo` is a cheap syscall, so this can be brisk.
pub const TICK: Duration = Duration::from_millis(250);

/// How often to read the hardware level to notice an Fn+Space override.
///
/// Deliberately slower than [`TICK`]. Reading is *not* free: `MLCG` traps into SMM just as
/// a write does, and an SMI briefly stalls every core. Polling at `TICK` would mean four
/// traps a second, forever, to catch something the user does a few times a day. One second
/// still notices it well inside the firmware's own ~1s fade.
pub const LEVEL_POLL: Duration = Duration::from_secs(1);

/// Consecutive driver errors before we call the backlight unavailable and say so.
const DEGRADE_AFTER: u32 = 5;

/// What changed during a [`Daemon::step`], for the front end to reflect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StepReport {
    pub phase_changed: bool,
    pub level_changed: bool,
    pub override_detected: bool,
    pub power_source_changed: bool,
    pub degraded_changed: bool,
}

impl StepReport {
    pub fn any(&self) -> bool {
        self.phase_changed
            || self.level_changed
            || self.override_detected
            || self.power_source_changed
            || self.degraded_changed
    }
}

pub struct Daemon {
    engine: Engine,
    backlight: Backlight,
    pump: EventPump,
    osd: OsdSuppressor,
    /// While the display is off the user is not there, so we stop driving entirely.
    display_off: bool,
    last_level_poll: Instant,
    last_phase: Option<Phase>,
    level: Option<Level>,
    /// Consecutive driver errors; reset by any success.
    failures: u32,
    degraded: bool,
}

impl Daemon {
    pub fn new(settings: Settings) -> io::Result<Self> {
        let backlight = Backlight::open()?;
        if !backlight.is_supported() {
            return Err(io::Error::other(
                "this machine reports no keyboard backlight support",
            ));
        }
        let pump = EventPump::new()?;
        let now = Instant::now();
        Ok(Self {
            engine: Engine::new(settings, platform::power_source(), now),
            backlight,
            pump,
            osd: OsdSuppressor::new(),
            display_off: false,
            last_level_poll: now,
            last_phase: None,
            level: None,
            failures: 0,
            degraded: false,
        })
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn phase(&self) -> Phase {
        self.engine.phase()
    }

    /// Last level we know the hardware to be at.
    pub fn level(&self) -> Option<Level> {
        self.level
    }

    /// True when the driver has failed repeatedly and we have stopped trusting it.
    pub fn is_degraded(&self) -> bool {
        self.degraded
    }

    pub fn replace_settings(&mut self, settings: Settings) {
        self.engine.replace_settings(settings);
    }

    /// Write a level, hiding the OSD popup it causes and tracking driver health.
    fn write_level(&mut self, level: Level, now: Instant) -> bool {
        // Only prepare to suppress when a write will really happen. This is called on
        // every tick, and arming unconditionally left overlapping watcher threads
        // enumerating every window in the session, forever, for writes that
        // write-on-change was about to discard.
        if self.engine.settings().suppress_osd && self.backlight.would_change(level) {
            // Snapshot before the write: anything appearing after it is ours to hide.
            self.osd.arm(now);
        }

        match self.backlight.set_level(level) {
            Ok(()) => {
                self.failures = 0;
                self.degraded = false;
                true
            }
            Err(_) => {
                self.failures += 1;
                self.degraded = self.failures >= DEGRADE_AFTER;
                false
            }
        }
    }

    /// One iteration. Callers pump the message queue first via
    /// [`crate::platform::pump_messages`].
    pub fn step(&mut self, now: Instant) -> StepReport {
        let mut report = StepReport::default();
        let was_degraded = self.degraded;

        for event in self.pump.take_events() {
            match event {
                PlatformEvent::Resume => {
                    // The firmware forgets the level across sleep, so this must assert
                    // rather than reconcile: drop both caches so the re-apply cannot be
                    // optimised away by write-on-change.
                    self.display_off = false;
                    self.engine.on_resume(now);
                    self.backlight.invalidate_cache();
                    // Give a driver that went away another chance.
                    self.failures = 0;
                    self.degraded = false;
                }
                PlatformEvent::DisplayOff => self.display_off = true,
                PlatformEvent::PowerSourceChanged(src) => {
                    self.engine.set_power_source(src);
                    report.power_source_changed = true;
                }
                PlatformEvent::SessionLock => self.engine.set_locked(true, now),
                PlatformEvent::SessionUnlock => {
                    self.engine.set_locked(false, now);
                    self.engine.on_resume(now);
                    self.backlight.invalidate_cache();
                }
            }
        }

        if self.display_off {
            return report;
        }

        // The OS is the authority on idle time; sync to it rather than tracking input.
        let idle = platform::idle_duration();
        self.engine.on_input(now.checked_sub(idle).unwrap_or(now));

        if now.duration_since(self.last_level_poll) >= LEVEL_POLL {
            self.last_level_poll = now;
            match self.backlight.get_level() {
                Ok(observed) => {
                    self.failures = 0;
                    self.degraded = false;
                    if self.level != Some(observed) {
                        self.level = Some(observed);
                        report.level_changed = true;
                    }
                    let was = self.engine.is_suspended();
                    self.engine.on_observed_level(observed);
                    report.override_detected = self.engine.is_suspended() && !was;
                }
                Err(_) => {
                    self.failures += 1;
                    self.degraded = self.failures >= DEGRADE_AFTER;
                }
            }
        }

        if let Some(desired) = self.engine.tick(now) {
            if self.write_level(desired, now) {
                self.engine.note_applied(desired);
                if self.level != Some(desired) {
                    self.level = Some(desired);
                    report.level_changed = true;
                }
            }
        }

        let phase = self.engine.phase();
        report.phase_changed = self.last_phase != Some(phase);
        self.last_phase = Some(phase);
        report.degraded_changed = self.degraded != was_degraded;
        report
    }

    /// Pick a level by hand and stop automatic control until the next resume.
    pub fn set_level_manual(&mut self, level: Level) {
        let now = Instant::now();
        if self.write_level(level, now) {
            self.engine.note_applied(level);
            self.level = Some(level);
        }
        self.engine.pause();
    }

    /// Hand control back to the state machine.
    pub fn resume_auto(&mut self) {
        self.engine.resume_control();
        self.backlight.invalidate_cache();
    }

    pub fn is_paused(&self) -> bool {
        self.engine.is_suspended()
    }

    /// Leave the keyboard lit on the way out, so quitting is never a surprise.
    pub fn shutdown(&mut self) {
        if let Some(level) = self.engine.exit_level() {
            self.backlight.invalidate_cache();
            let now = Instant::now();
            self.write_level(level, now);
        }
    }
}
