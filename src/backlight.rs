//! Tier 2 of the HAL: the keyboard backlight, expressed in levels.
//!
//! The hardware has exactly three states. There is no smooth fade and no percentage —
//! "dim" is literally [`Level::Low`]. See `notes/02-design-spec.md` §2.

use std::io;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::acpi::{AcpiGateway, IOCTL_MLCG, IOCTL_MLCS};

/// Backlight level. The X1C6 panel supports exactly these three.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    Off = 0,
    Low = 1,
    High = 2,
}

impl Level {
    pub fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Level::Off),
            1 => Some(Level::Low),
            2 => Some(Level::High),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Low => "low",
            Level::High => "high",
        }
    }
}

/// Bits 0-1 of the `MLCG` result hold the current level.
const MLCG_LEVEL_MASK: u32 = 0x3;
/// Bit 9 of the `MLCG` result means "keyboard backlight supported".
/// Matches how Linux's `thinkpad_acpi` reads the same value.
const MLCG_SUPPORTED_BIT: u32 = 1 << 9;

/// Minimum spacing between two *actual* writes.
///
/// Every write traps into SMM, which briefly stalls all cores. Redundant writes are
/// dropped entirely (see [`Backlight::set_level`]); this is the floor for the rest, so a
/// caller bug cannot turn into an SMI storm. Rate limiting lives here, in the HAL, rather
/// than relying on every caller to behave.
const MIN_WRITE_INTERVAL: Duration = Duration::from_millis(20);

/// What the backlight can do. Implemented by the real driver and by a fake, so the state
/// machine and UI can be developed and unit-tested without touching hardware.
pub trait BacklightController: Send + Sync {
    fn is_supported(&self) -> bool;
    fn max_level(&self) -> Level {
        Level::High
    }
    fn get_level(&self) -> io::Result<Level>;
    /// Set the level. Implementations must be idempotent and must skip redundant writes.
    fn set_level(&self, level: Level) -> io::Result<()>;
}

struct WriteState {
    last_written: Option<Level>,
    last_write_at: Option<Instant>,
}

/// The real controller, talking to `\\.\IBMPMDRV`.
pub struct Backlight {
    gateway: AcpiGateway,
    supported: bool,
    state: Mutex<WriteState>,
}

impl Backlight {
    /// Open the driver and probe for support.
    pub fn open() -> io::Result<Self> {
        let gateway = AcpiGateway::open()?;
        let raw = gateway.evaluate(IOCTL_MLCG, 0)?;
        Ok(Self {
            gateway,
            supported: raw & MLCG_SUPPORTED_BIT != 0,
            state: Mutex::new(WriteState {
                last_written: None,
                last_write_at: None,
            }),
        })
    }

    /// Raw `MLCG` result, for diagnostics.
    pub fn raw_status(&self) -> io::Result<u32> {
        self.gateway.evaluate(IOCTL_MLCG, 0)
    }

    /// Forget what we last wrote, so the next `set_level` always reaches the hardware.
    ///
    /// Call this after resume/unlock: the firmware forgets the level across sleep, so our
    /// cached value is stale and the write must not be optimised away. This is exactly
    /// the bug the application exists to fix.
    pub fn invalidate_cache(&self) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.last_written = None;
    }
}

impl BacklightController for Backlight {
    fn is_supported(&self) -> bool {
        self.supported
    }

    fn get_level(&self) -> io::Result<Level> {
        let raw = self.gateway.evaluate(IOCTL_MLCG, 0)?;
        Level::from_u8((raw & MLCG_LEVEL_MASK) as u8).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unexpected level in MLCG result 0x{raw:08X}"),
            )
        })
    }

    fn set_level(&self, level: Level) -> io::Result<()> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());

        // Write-on-change: never spend an SMI trap saying what is already true.
        if st.last_written == Some(level) {
            return Ok(());
        }

        if let Some(prev) = st.last_write_at {
            let since = prev.elapsed();
            if since < MIN_WRITE_INTERVAL {
                std::thread::sleep(MIN_WRITE_INTERVAL - since);
            }
        }

        // MLCS returns 0 on success. The 0x00010000 bit in the AML argument selects which
        // ACPI notification the firmware raises; Lenovo does not appear to use this
        // interface at all, and the bare level works, so we pass the level unadorned.
        self.gateway.evaluate(IOCTL_MLCS, level as u32)?;

        st.last_written = Some(level);
        st.last_write_at = Some(Instant::now());
        Ok(())
    }
}

/// In-memory stand-in so the state machine and UI can be built and tested off-hardware.
pub struct FakeBacklight {
    level: Mutex<Level>,
    supported: bool,
}

impl FakeBacklight {
    pub fn new(initial: Level) -> Self {
        Self {
            level: Mutex::new(initial),
            supported: true,
        }
    }

    pub fn unsupported() -> Self {
        Self {
            level: Mutex::new(Level::Off),
            supported: false,
        }
    }
}

impl BacklightController for FakeBacklight {
    fn is_supported(&self) -> bool {
        self.supported
    }

    fn get_level(&self) -> io::Result<Level> {
        Ok(*self.level.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn set_level(&self, level: Level) -> io::Result<()> {
        *self.level.lock().unwrap_or_else(|e| e.into_inner()) = level;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_roundtrip() {
        for v in 0u8..=2 {
            assert_eq!(Level::from_u8(v).unwrap() as u8, v);
        }
        assert!(Level::from_u8(3).is_none());
    }

    #[test]
    fn levels_are_ordered() {
        assert!(Level::Off < Level::Low && Level::Low < Level::High);
    }

    #[test]
    fn fake_roundtrips() {
        let b = FakeBacklight::new(Level::Off);
        b.set_level(Level::High).unwrap();
        assert_eq!(b.get_level().unwrap(), Level::High);
    }

    #[test]
    fn mlcg_decoding_matches_observed_hardware() {
        // Value actually observed on the X1C6: level 2, supported.
        let raw: u32 = 0x0005_0202;
        assert_eq!(raw & MLCG_LEVEL_MASK, 2);
        assert_ne!(raw & MLCG_SUPPORTED_BIT, 0);
    }
}
