//! `tkbl-tray` — the background application.
//!
//! No console: it lives in the notification area. Closing is only possible from the tray
//! menu, which is also where the backlight gets restored on the way out.

#![windows_subsystem = "windows"]

use std::path::PathBuf;
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

use tkbl::daemon::{Daemon, TICK};
use tkbl::settings_window::{self, Outcome};
use tkbl::tray::{
    Tray, TrayEvent, ID_AUTO, ID_DIM, ID_EXIT, ID_OFF, ID_ON, ID_SETTINGS, ID_STARTUP,
};
use tkbl::{autostart, platform, Level, PowerSource, Settings};

/// How often to notice that settings.json changed on disk.
const SETTINGS_POLL: Duration = Duration::from_secs(5);

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn fatal(message: &str) -> ! {
    // SAFETY: both strings are NUL-terminated and outlive the call.
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(message).as_ptr(),
            wide("ThinkPad Keyboard Backlight").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
    std::process::exit(1)
}

/// Refuse to start twice: two instances would fight over the backlight.
fn claim_single_instance() -> bool {
    // SAFETY: name is NUL-terminated; the handle is intentionally leaked for process life.
    unsafe {
        let handle = CreateMutexW(
            std::ptr::null(),
            1,
            wide("Local\\ThinkpadKbdLight.SingleInstance").as_ptr(),
        );
        if handle.is_null() {
            return true; // cannot tell; better to run than to refuse
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(handle);
            return false;
        }
    }
    true
}

fn settings_mtime(path: &PathBuf) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok()?.modified().ok()
}

fn tooltip(daemon: &Daemon) -> String {
    let level = daemon.level().map(|l| l.as_str()).unwrap_or("unknown");
    let source = match daemon.engine().power_source() {
        PowerSource::Ac => "AC",
        PowerSource::Battery => "battery",
    };
    if daemon.is_paused() {
        format!("Keyboard backlight: {level} ({source}) - paused")
    } else {
        format!(
            "Keyboard backlight: {level} ({source}) - {:?}",
            daemon.phase()
        )
    }
}

fn main() {
    if !claim_single_instance() {
        return; // already running; nothing to say
    }
    // Before any window exists, or the settings window renders blurry on a HiDPI panel.
    platform::set_dpi_aware();

    let path = match Settings::default_path() {
        Ok(p) => p,
        Err(e) => fatal(&format!("Cannot locate the settings folder:\n{e}")),
    };
    let settings = match Settings::load_or_default(&path) {
        Ok(s) => s,
        Err(e) => fatal(&format!(
            "Could not read settings:\n{}\n\n{e}\n\nFix or delete the file, then start again.",
            path.display()
        )),
    };

    let mut daemon = match Daemon::new(settings.clone()) {
        Ok(d) => d,
        Err(e) => fatal(&format!(
            "Cannot control the keyboard backlight:\n{e}\n\n\
             This needs a ThinkPad with the Lenovo PM driver (ibmpmdrv.sys) installed."
        )),
    };

    let mut tray = match Tray::new() {
        Ok(t) => t,
        Err(e) => fatal(&format!("Cannot create the tray icon:\n{e}")),
    };

    let mut settings = settings;
    let mut last_settings_poll = Instant::now();
    let mut last_mtime = settings_mtime(&path);
    let mut startup_on = autostart::is_enabled();
    let mut first = true;

    loop {
        platform::pump_messages();
        let now = Instant::now();

        let report = daemon.step(now);

        let mut quit = false;
        for event in tray.take_events() {
            match event {
                TrayEvent::Toggle => {
                    // Simple: dark -> the profile's active level, lit -> off.
                    let active = settings
                        .profile(daemon.engine().power_source())
                        .active_level;
                    let next = match daemon.level() {
                        Some(Level::Off) | None => active,
                        Some(_) => Level::Off,
                    };
                    daemon.set_level_manual(next);
                }
                TrayEvent::MenuCommand(id) => match id {
                    ID_ON => daemon.set_level_manual(Level::High),
                    ID_DIM => daemon.set_level_manual(Level::Low),
                    ID_OFF => daemon.set_level_manual(Level::Off),
                    ID_AUTO => {
                        if daemon.is_paused() {
                            daemon.resume_auto();
                        } else {
                            daemon.set_level_manual(daemon.level().unwrap_or(Level::High));
                        }
                    }
                    ID_SETTINGS => {
                        let _ = settings_window::open(&settings);
                    }
                    ID_STARTUP => {
                        startup_on = !startup_on;
                        if autostart::set(startup_on).is_err() {
                            startup_on = autostart::is_enabled();
                        }
                    }
                    ID_EXIT => quit = true,
                    _ => {}
                },
            }
        }

        if quit {
            break;
        }

        if let Some(Outcome::Saved(fresh)) = settings_window::take_outcome() {
            // The window does not own this checkbox; the tray menu does.
            let mut fresh = *fresh;
            fresh.start_with_windows = startup_on;
            if fresh.save(&path).is_ok() {
                last_mtime = settings_mtime(&path);
            }
            settings = fresh.clone();
            daemon.replace_settings(fresh);
            // New timeouts should take effect now, not after the next override.
            daemon.resume_auto();
        }

        // Pick up hand edits to settings.json without needing a restart.
        if now.duration_since(last_settings_poll) >= SETTINGS_POLL {
            last_settings_poll = now;
            let mtime = settings_mtime(&path);
            if mtime != last_mtime {
                last_mtime = mtime;
                if let Ok(fresh) = Settings::load_or_default(&path) {
                    settings = fresh.clone();
                    daemon.replace_settings(fresh);
                }
            }
        }

        tray.set_menu_state(
            daemon.level(),
            daemon.is_paused(),
            daemon.engine().power_source() == PowerSource::Ac,
            startup_on,
        );
        if report.any() || first {
            tray.update(daemon.level(), &tooltip(&daemon), first);
            first = false;
        }

        std::thread::sleep(TICK);
    }

    daemon.shutdown();
}
