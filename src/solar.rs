//! Local solar-position calculation (NOAA's solar equations). No network service.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::settings::Location;

/// True between apparent sunset and sunrise. None means time/location is unusable.
pub fn sun_is_down(now: SystemTime, location: Location) -> Option<bool> {
    if !location.valid() {
        return None;
    }
    let seconds = now.duration_since(UNIX_EPOCH).ok()?.as_secs_f64();
    let jd = seconds / 86_400.0 + 2_440_587.5;
    let t = (jd - 2_451_545.0) / 36_525.0;
    let radians = f64::to_radians;

    let l0 = (280.46646 + t * (36_000.76983 + 0.0003032 * t)).rem_euclid(360.0);
    let m = radians(357.52911 + t * (35_999.05029 - 0.0001537 * t));
    let e = 0.016708634 - t * (0.000042037 + 0.0000001267 * t);
    let center = m.sin() * (1.914602 - t * (0.004817 + 0.000014 * t))
        + (2.0 * m).sin() * (0.019993 - 0.000101 * t)
        + (3.0 * m).sin() * 0.000289;
    let omega = radians(125.04 - 1934.136 * t);
    let apparent = radians(l0 + center - 0.00569 - 0.00478 * omega.sin());
    let obliquity = 23.0
        + (26.0 + (21.448 - t * (46.8150 + t * (0.00059 - t * 0.001813))) / 60.0) / 60.0
        + 0.00256 * omega.cos();
    let obliquity = radians(obliquity);
    let declination = (obliquity.sin() * apparent.sin()).asin();
    let y = (obliquity / 2.0).tan().powi(2);
    let l0 = radians(l0);
    let equation_minutes = 4.0
        * (y * (2.0 * l0).sin() - 2.0 * e * m.sin() + 4.0 * e * y * m.sin() * (2.0 * l0).cos()
            - 0.5 * y * y * (4.0 * l0).sin()
            - 1.25 * e * e * (2.0 * m).sin())
        .to_degrees();
    let utc_minutes = (seconds.rem_euclid(86_400.0)) / 60.0;
    let solar_minutes =
        (utc_minutes + equation_minutes + 4.0 * location.longitude).rem_euclid(1440.0);
    let hour_angle = radians(solar_minutes / 4.0 - 180.0);
    let latitude = radians(location.latitude);
    let altitude = (latitude.sin() * declination.sin()
        + latitude.cos() * declination.cos() * hour_angle.cos())
    .asin()
    .to_degrees();
    Some(altitude < -0.833)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn zurich_day_and_night_at_equinox() {
        let zurich = Location {
            latitude: 47.3769,
            longitude: 8.5417,
        };
        // 2026-03-20 12:00 and 00:00 UTC, respectively.
        let noon = UNIX_EPOCH + Duration::from_secs(1_773_921_600);
        let midnight = noon - Duration::from_secs(43_200);
        assert_eq!(sun_is_down(noon, zurich), Some(false));
        assert_eq!(sun_is_down(midnight, zurich), Some(true));
    }

    #[test]
    fn rejects_invalid_location() {
        assert_eq!(
            sun_is_down(
                SystemTime::now(),
                Location {
                    latitude: f64::NAN,
                    longitude: 0.0
                }
            ),
            None
        );
    }
}
