//! `tkbl` — command-line front end for the keyboard backlight HAL.
//!
//! This is milestone 1 from `notes/02-design-spec.md` §9: prove the hardware layer works
//! on its own, before any UI exists. Argument parsing is done by hand to keep the binary
//! small and the dependency set at exactly one crate.

use std::process::ExitCode;

use tkbl::{Backlight, BacklightController, Level};

const USAGE: &str = "\
tkbl - ThinkPad keyboard backlight control

USAGE:
    tkbl <COMMAND>

COMMANDS:
    get                 Print the current level
    set <LEVEL>         Set the level: off|low|high, or 0|1|2
    info                Show driver status and decode the raw MLCG bitfield
    test                Cycle off -> low -> high, then restore the original level
    raw <IOCTL> <ARG>   Evaluate an arbitrary ACPI method (requires --force)

EXAMPLES:
    tkbl get
    tkbl set low
    tkbl info
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        print!("{USAGE}");
        return ExitCode::from(2);
    }

    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn open() -> Result<Backlight, String> {
    Backlight::open().map_err(|e| {
        format!(
            "cannot open the Lenovo PM driver (\\\\.\\IBMPMDRV): {e}\n\
             hint: this needs a ThinkPad with ibmpmdrv.sys installed; it is not a permissions problem"
        )
    })
}

fn run(args: &[String]) -> Result<(), String> {
    match args[0].as_str() {
        "get" => {
            let b = open()?;
            let lvl = b.get_level().map_err(|e| e.to_string())?;
            println!("{}", lvl.as_str());
            Ok(())
        }

        "set" => {
            let arg = args
                .get(1)
                .ok_or("set requires a level: off|low|high (or 0|1|2)")?;
            let level = parse_level(arg)?;
            let b = open()?;
            if !b.is_supported() {
                return Err("this machine reports no keyboard backlight support".into());
            }
            b.set_level(level).map_err(|e| e.to_string())?;
            let readback = b.get_level().map_err(|e| e.to_string())?;
            if readback != level {
                return Err(format!(
                    "wrote {} but hardware reports {}",
                    level.as_str(),
                    readback.as_str()
                ));
            }
            println!("{}", level.as_str());
            Ok(())
        }

        "info" => {
            let b = open()?;
            let raw = b.raw_status().map_err(|e| e.to_string())?;
            let lvl = b.get_level().map_err(|e| e.to_string())?;
            println!("device      : \\\\.\\IBMPMDRV (opened, no elevation)");
            println!("supported   : {}", b.is_supported());
            println!("level       : {} ({})", lvl.as_str(), lvl as u8);
            println!("max level   : {}", b.max_level() as u8);
            println!("raw MLCG    : 0x{raw:08X}");
            println!("  bits 0-1  : {} (current level)", raw & 0x3);
            println!(
                "  bit 9     : {} (supported)",
                if raw & (1 << 9) != 0 { 1 } else { 0 }
            );
            Ok(())
        }

        "test" => {
            let b = open()?;
            if !b.is_supported() {
                return Err("this machine reports no keyboard backlight support".into());
            }
            let original = b.get_level().map_err(|e| e.to_string())?;
            println!("original level: {}", original.as_str());

            for level in [Level::Off, Level::Low, Level::High] {
                b.set_level(level).map_err(|e| e.to_string())?;
                std::thread::sleep(std::time::Duration::from_millis(900));
                let got = b.get_level().map_err(|e| e.to_string())?;
                println!(
                    "  set {:<4} -> readback {:<4} [{}]",
                    level.as_str(),
                    got.as_str(),
                    if got == level { "MATCH" } else { "MISMATCH" }
                );
            }

            b.set_level(original).map_err(|e| e.to_string())?;
            println!("restored to {}", original.as_str());
            Ok(())
        }

        "raw" => {
            // Deliberately guarded. This evaluates arbitrary ACPI methods through a
            // power-management driver; a wrong code can wedge the EC. See
            // notes/03-ioctl-trace-plan.md for why we do not guess these.
            if !args.iter().any(|a| a == "--force") {
                return Err(
                    "raw evaluates arbitrary ACPI methods and can wedge the embedded \
                     controller.\nPass --force if you know the IOCTL is safe."
                        .into(),
                );
            }
            let ioctl = parse_u32(args.get(1).ok_or("raw requires <IOCTL> <ARG>")?)?;
            let arg = parse_u32(args.get(2).ok_or("raw requires <IOCTL> <ARG>")?)?;
            let gw = tkbl::AcpiGateway::open().map_err(|e| e.to_string())?;
            let out = gw.evaluate(ioctl, arg).map_err(|e| e.to_string())?;
            println!("0x{out:08X}");
            Ok(())
        }

        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            Ok(())
        }

        other => Err(format!("unknown command '{other}'\n\n{USAGE}")),
    }
}

fn parse_level(s: &str) -> Result<Level, String> {
    match s.to_ascii_lowercase().as_str() {
        "off" | "0" => Ok(Level::Off),
        "low" | "dim" | "1" => Ok(Level::Low),
        "high" | "max" | "2" => Ok(Level::High),
        other => Err(format!(
            "invalid level '{other}' (use off|low|high or 0|1|2)"
        )),
    }
}

fn parse_u32(s: &str) -> Result<u32, String> {
    let t = s.trim();
    let r = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X"));
    match r {
        Some(hex) => u32::from_str_radix(hex, 16).map_err(|e| format!("bad hex '{s}': {e}")),
        None => t
            .parse::<u32>()
            .map_err(|e| format!("bad number '{s}': {e}")),
    }
}
