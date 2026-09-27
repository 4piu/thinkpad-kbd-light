//! One-time Windows Location lookup, called only from the foreground settings window.

use windows::Devices::Geolocation::{GeolocationAccessStatus, Geolocator};
use windows::Foundation::TimeSpan;
use windows::Win32::System::WinRT::{RoInitialize, RoUninitialize, RO_INIT_SINGLETHREADED};

use crate::settings::Location;

pub fn from_windows_settings() -> Result<Location, String> {
    // The tray's Win32 UI thread is not otherwise initialized for WinRT.
    let _runtime = WinRtApartment::new()?;
    let status = Geolocator::RequestAccessAsync()
        .and_then(|operation| operation.get())
        .map_err(|e| format!("Windows Location permission could not be checked: {e}"))?;
    if status != GeolocationAccessStatus::Allowed {
        return Err("Windows Location access is off or denied. Enable it in Settings > Privacy & security > Location, then save again.".into());
    }
    let locator = Geolocator::new().map_err(|e| format!("Windows Location is unavailable: {e}"))?;
    let position = locator
        .GetGeopositionAsyncWithAgeAndTimeout(
            TimeSpan {
                Duration: 30 * 60 * 10_000_000,
            },
            TimeSpan {
                Duration: 10 * 10_000_000,
            },
        )
        .and_then(|operation| operation.get())
        .map_err(|e| format!("Windows could not determine your location: {e}"))?;
    let coordinate = position
        .Coordinate()
        .and_then(|c| c.Point())
        .and_then(|p| p.Position())
        .map_err(|e| format!("Windows returned an unusable location: {e}"))?;
    let location = Location {
        latitude: coordinate.Latitude,
        longitude: coordinate.Longitude,
    };
    if !location.valid() {
        return Err("Windows returned coordinates outside the expected range.".into());
    }
    Ok(location)
}

struct WinRtApartment;

impl WinRtApartment {
    fn new() -> Result<Self, String> {
        // SAFETY: balanced by Drop on this same UI thread.
        unsafe { RoInitialize(RO_INIT_SINGLETHREADED) }
            .map_err(|e| format!("Windows Location runtime could not start: {e}"))?;
        Ok(Self)
    }
}

impl Drop for WinRtApartment {
    fn drop(&mut self) {
        // SAFETY: this guard is created and dropped on the same UI thread.
        unsafe { RoUninitialize() };
    }
}
