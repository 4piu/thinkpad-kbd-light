//! Times the ACPI round-trip, to see what the level poll actually costs.
//!
//!     cargo run --release --example bench_acpi
use std::time::{Duration, Instant};
use tkbl::{acpi::IOCTL_MLCG, AcpiGateway};

fn main() {
    let gw = AcpiGateway::open().expect("open IBMPMDRV");
    // Warm up.
    for _ in 0..3 {
        let _ = gw.evaluate(IOCTL_MLCG, 0);
    }

    let mut times = Vec::new();
    for _ in 0..40 {
        let t = Instant::now();
        let _ = gw.evaluate(IOCTL_MLCG, 0).expect("MLCG");
        times.push(t.elapsed());
        std::thread::sleep(Duration::from_millis(50));
    }
    times.sort();
    let sum: Duration = times.iter().sum();
    println!("MLCG (get) over {} calls:", times.len());
    println!("  min    {:?}", times[0]);
    println!("  median {:?}", times[times.len() / 2]);
    println!("  p90    {:?}", times[times.len() * 9 / 10]);
    println!("  max    {:?}", times[times.len() - 1]);
    println!("  mean   {:?}", sum / times.len() as u32);
    println!();
    println!(
        "at one poll per second that is {:.1}% of a core",
        100.0 * (sum.as_secs_f64() / times.len() as f64)
    );
}
