//! Diagnostic only: prints device classes and activity counts, never input contents.
use std::time::{Duration, Instant};

fn main() -> std::io::Result<()> {
    for (name, source) in tkbl::input::devices()? {
        println!("{source:?}: {name}");
    }
    let _pump = tkbl::platform::EventPump::new()?;
    let seconds = std::env::args()
        .nth(1)
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(60);
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut last_print = Instant::now();
    while Instant::now() < end {
        tkbl::platform::pump_messages();
        if last_print.elapsed() >= Duration::from_secs(1) {
            last_print = Instant::now();
            let a = tkbl::input::activity();
            println!(
                "activity: internal={} external={} unknown={}",
                a.internal_events, a.external_events, a.unknown_events
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
