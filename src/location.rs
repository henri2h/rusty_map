//! The device's position, for the "my location" map control.
//!
//! Android only: `MainActivity` listens to `LocationManager` (the fused provider, else GPS and
//! network) while tracking is on and the activity is in the foreground, and pushes every fix
//! here over JNI as it arrives — no polling. Elsewhere [`AVAILABLE`] is false and the control is
//! hidden.

use std::sync::LazyLock;

use freya::prelude::State;
use tokio::sync::watch;

use crate::mobile::use_watch;

pub const AVAILABLE: bool = cfg!(target_os = "android");

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    /// Meters, 68% confidence.
    pub accuracy_m: Option<f32>,
    /// Degrees clockwise from north, when moving.
    pub bearing: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Reading {
    /// Tracking off, or no fix yet.
    None,
    Fix(Fix),
    /// Permission refused, or location turned off in the system settings.
    Unavailable,
}

static READING: LazyLock<watch::Sender<Reading>> =
    LazyLock::new(|| watch::Sender::new(Reading::None));

/// The latest reading, re-rendering the caller when it changes.
pub fn use_reading() -> State<Reading> {
    use_watch(&READING)
}

/// Starts or stops location updates. Starting asks for the permission first if needed; a refusal
/// arrives as [`Reading::Unavailable`].
pub fn set_tracking(enabled: bool) {
    if !enabled {
        READING.send_replace(Reading::None);
    }
    #[cfg(target_os = "android")]
    if let Err(e) = android::set_tracking(enabled) {
        log::warn!("location: setLocationTracking({enabled}) failed: {e}");
    }
}

/// Desktop has no location source: with `RUSTY_MAP_SIMULATE_WALK` set, starting walking
/// navigation walks fake fixes along its route instead (see [`simulate_walk`]).
pub fn simulating() -> bool {
    !AVAILABLE && std::env::var_os("RUSTY_MAP_SIMULATE_WALK").is_some()
}

/// Bumped to stop a running simulation.
static SIMULATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Walks fake fixes along `points` (`(lon, lat)`) at 1.4 m/s, one a second, a few metres off
/// the line, with one 60 m detour from 40% to 50% of the way (enough to go off the route and
/// re-route). Replaces any simulation already running. Only when [`simulating`].
pub fn simulate_walk(points: Vec<(f64, f64)>) {
    use crate::components::navigation::geo::{bearing, cumulative, point_at};
    use std::sync::atomic::Ordering;

    if !simulating() || points.len() < 2 {
        return;
    }
    let generation = SIMULATION.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        let along = cumulative(&points);
        let length = *along.last().unwrap_or(&0.0);
        let metres_per_degree = 111_320.0;
        let mut walked = 0.0;
        let mut second = 0u32;
        while SIMULATION.load(Ordering::SeqCst) == generation && walked <= length + 10.0 {
            let (lon, lat) = point_at(&points, &along, walked);
            let heading = bearing(
                point_at(&points, &along, walked),
                point_at(&points, &along, walked + 5.0),
            )
            .to_radians();
            // Sideways from the route: GPS-like wobble, plus the detour.
            let detour = if (0.4..0.5).contains(&(walked / length)) {
                60.0
            } else {
                0.0
            };
            let side = 3.0 * (second as f64 * 1.7).sin() + detour;
            let (east, north) = (side * heading.cos(), -side * heading.sin());
            READING.send_replace(Reading::Fix(Fix {
                lat: lat + north / metres_per_degree,
                lon: lon + east / (metres_per_degree * lat.to_radians().cos()),
                accuracy_m: Some(6.0),
                bearing: Some(heading.to_degrees() as f32),
            }));
            std::thread::sleep(std::time::Duration::from_secs(1));
            walked += 1.4;
            second += 1;
        }
    });
}

/// Stops [`simulate_walk`].
pub fn stop_simulation() {
    SIMULATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(target_os = "android")]
mod android {
    use jni::{
        JNIEnv,
        objects::JClass,
        sys::{jdouble, jfloat},
    };

    use super::{Fix, READING, Reading};

    pub fn set_tracking(enabled: bool) -> jni::errors::Result<()> {
        crate::mobile::call_activity(
            "setLocationTracking",
            "(Z)V",
            &[crate::mobile::Arg::Bool(enabled)],
        )
    }

    /// `MainActivity.nativeSetLocation`: a new fix. A negative accuracy or a NaN bearing means
    /// unknown.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_rustymap_app_MainActivity_nativeSetLocation(
        _env: JNIEnv,
        _class: JClass,
        lat: jdouble,
        lon: jdouble,
        accuracy: jfloat,
        bearing: jfloat,
    ) {
        READING.send_replace(Reading::Fix(Fix {
            lat,
            lon,
            accuracy_m: (accuracy >= 0.0).then_some(accuracy),
            bearing: (!bearing.is_nan()).then_some(bearing),
        }));
    }

    /// `MainActivity.nativeLocationUnavailable`: permission refused or location turned off.
    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_rustymap_app_MainActivity_nativeLocationUnavailable(
        _env: JNIEnv,
        _class: JClass,
    ) {
        READING.send_replace(Reading::Unavailable);
    }
}
