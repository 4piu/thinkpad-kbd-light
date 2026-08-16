//! Opens the settings window on its own, with default settings and no hardware.
//!
//! `cargo run --example settings_preview`
//!
//! Useful for working on the layout without the tray app in the way — it caught the
//! client-versus-window sizing bug that clipped the button row.
use std::time::{Duration, Instant};

fn main() {
    tkbl::platform::set_dpi_aware();
    let settings = tkbl::Settings::default();
    tkbl::settings_window::open(&settings).expect("open");

    let start = Instant::now();
    while tkbl::settings_window::is_open() && start.elapsed() < Duration::from_secs(25) {
        tkbl::platform::pump_messages();
        if let Some(outcome) = tkbl::settings_window::take_outcome() {
            println!("outcome: {outcome:?}");
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    println!("closed");
}
