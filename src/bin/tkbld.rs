//! `tkbld` — the backlight daemon.
//!
//! Milestone 3, part 1 from `notes/02-design-spec.md` §9: the behaviour, without the tray
//! UI. Runs in a console so it can be watched while testing; the tray front end comes next.

use std::time::{Duration, Instant};

use tkbl::backlight::BacklightController;
use tkbl::platform::{self, EventPump, PlatformEvent};
use tkbl::{Backlight, Engine, Settings};

/// State-machine cadence. `GetLastInputInfo` is a cheap syscall, so this can be brisk.
const TICK: Duration = Duration::from_millis(250);

/// How often to read the hardware level to notice an Fn+Space override.
///
/// Deliberately slower than `TICK`. Reading the level is *not* free: `MLCG` traps into
/// SMM just as a write does, and an SMI briefly stalls every core. Polling at `TICK`
/// would mean four traps a second, forever, for a change the user makes a few times a
/// day. One second still notices the override well within the firmware's own ~1s fade.
const LEVEL_POLL: Duration = Duration::from_secs(1);

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let path = Settings::default_path().map_err(|e| e.to_string())?;
    let settings = Settings::load_or_default(&path).map_err(|e| {
        format!(
            "could not read {}: {e}\nfix or delete the file to continue",
            path.display()
        )
    })?;

    let backlight = Backlight::open()
        .map_err(|e| format!("cannot open the Lenovo PM driver (\\\\.\\IBMPMDRV): {e}"))?;
    if !backlight.is_supported() {
        return Err("this machine reports no keyboard backlight support".into());
    }

    let pump = EventPump::new().map_err(|e| format!("cannot create event window: {e}"))?;

    let source = platform::power_source();
    let mut engine = Engine::new(settings, source, Instant::now());
    println!("tkbld running. settings: {}", path.display());
    println!("power source: {source:?}");

    // While the display is off we stop driving entirely: the user is not there, and a
    // write would only wake the EC for nothing.
    let mut display_off = false;
    let mut last_level_poll = Instant::now();
    let mut last_phase = None;

    loop {
        let now = Instant::now();

        for event in pump.drain() {
            match event {
                PlatformEvent::Resume => {
                    // The firmware forgets the level across sleep, so this must assert
                    // rather than reconcile: drop both caches and re-apply unconditionally.
                    display_off = false;
                    engine.on_resume(now);
                    backlight.invalidate_cache();
                    println!("[event] resume / display on -> re-applying");
                }
                PlatformEvent::DisplayOff => {
                    display_off = true;
                    println!("[event] display off");
                }
                PlatformEvent::PowerSourceChanged(src) => {
                    engine.set_power_source(src);
                    println!("[event] power source -> {src:?}");
                }
                PlatformEvent::SessionLock => {
                    engine.set_locked(true, now);
                    println!("[event] session locked");
                }
                PlatformEvent::SessionUnlock => {
                    engine.set_locked(false, now);
                    engine.on_resume(now);
                    backlight.invalidate_cache();
                    println!("[event] session unlocked -> re-applying");
                }
            }
        }

        if display_off {
            std::thread::sleep(TICK);
            continue;
        }

        // The OS is the authority on idle time, so sync the engine to it every tick
        // rather than tracking input ourselves.
        let idle = platform::idle_duration();
        engine.on_input(now.checked_sub(idle).unwrap_or(now));

        // Notice Fn+Space, which is handled in firmware and cannot be intercepted.
        if now.duration_since(last_level_poll) >= LEVEL_POLL {
            last_level_poll = now;
            if let Ok(observed) = backlight.get_level() {
                let was_suspended = engine.is_suspended();
                engine.on_observed_level(observed);
                if engine.is_suspended() && !was_suspended {
                    println!(
                        "[override] level changed externally to {} - backing off",
                        observed.as_str()
                    );
                }
            }
        }

        if let Some(desired) = engine.tick(now) {
            match backlight.set_level(desired) {
                Ok(()) => engine.note_applied(desired),
                Err(e) => eprintln!("[warn] could not set level: {e}"),
            }
        }

        let phase = engine.phase();
        if last_phase != Some(phase) {
            println!("[phase] {phase:?}");
            last_phase = Some(phase);
        }

        std::thread::sleep(TICK);
    }
}
