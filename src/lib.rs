//! Keyboard backlight control for ThinkPads.
//!
//! Two tiers, per `notes/02-design-spec.md` §3.1:
//!
//! * [`acpi::AcpiGateway`] — evaluate an ACPI method through the Lenovo PM driver.
//! * [`backlight::Backlight`] — the keyboard backlight, in levels.
//!
//! The split exists because the driver is a *generic* ACPI evaluator: keeping the
//! primitive separate makes it cheap to support ThinkPads that expose the backlight under
//! different method names, and leaves the door open to other firmware features.
//!
//! Windows only. Requires the Lenovo PM driver (`ibmpmdrv.sys`), which ships with the
//! standard ThinkPad driver set. No elevation required.

#![cfg(windows)]

pub mod acpi;
pub mod backlight;

pub use acpi::AcpiGateway;
pub use backlight::{Backlight, BacklightController, FakeBacklight, Level};
