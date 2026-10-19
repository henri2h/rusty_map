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
