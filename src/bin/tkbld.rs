//! `tkbld` — the daemon in a console.
//!
//! Same behaviour as the tray application, but it logs what it is doing, which makes it
//! the better thing to run when diagnosing something.

use std::time::Instant;

use tkbl::daemon::{Daemon, TICK};
use tkbl::{platform, Settings};

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

    let mut daemon = Daemon::new(settings).map_err(|e| e.to_string())?;

    println!("tkbld running. settings: {}", path.display());
    println!("power source: {:?}", daemon.engine().power_source());

    loop {
        platform::pump_messages();
        let now = Instant::now();
        let report = daemon.step(now);

        if report.power_source_changed {
            println!(
                "[event] power source -> {:?}",
                daemon.engine().power_source()
            );
        }
        if report.override_detected {
            println!("[override] level changed externally - backing off");
        }
        if report.phase_changed {
            println!(
                "[phase] {:?} ({})",
                daemon.phase(),
                daemon.level().map(|l| l.as_str()).unwrap_or("?")
            );
        }

        std::thread::sleep(TICK);
    }
}
