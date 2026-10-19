pub mod components;
pub mod location;
pub mod mobile;
pub mod pages;
pub mod route;

use freya::prelude::*;
use freya_router::prelude::{Router, RouterConfig};

use crate::route::Route;

/// The map keeps many large tile bitmaps on screen at once (over-zoom re-bakes up to 4096², line
/// overlays up to 2048²); past the default 256 MiB Skia evicts and re-uploads them every frame —
/// measured ~47% of main-thread time.
const GPU_CACHE_MB: usize = 1024;

/// Adds the frame-timing overlay when built with the `perf-overlay` feature.
fn with_perf_overlay(config: LaunchConfig) -> LaunchConfig {
    #[cfg(feature = "perf-overlay")]
    let config = config.with_plugin(
        freya_metrics_plugin::MetricsPlugin::default()
            .with_visible_performance(true)
            .with_visible_stats(true),
    );
    config
}

pub fn app() -> impl IntoElement {
    Router::<Route>::new(|| RouterConfig::default().with_initial_path(Route::HomePage))
}

#[cfg(not(target_os = "android"))]
pub fn run_desktop() {
    env_logger::init();

    launch(with_perf_overlay(
        LaunchConfig::new()
            .with_gpu_resource_cache_limit(GpuResourceCacheLimit::from_mb(GPU_CACHE_MB))
            .with_window(WindowConfig::new(app)),
    ));
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(droid_app: winit::platform::android::activity::AndroidApp) {
    use freya::android::AndroidPlugin;
    use winit::{event_loop::EventLoop, platform::android::EventLoopBuilderExtAndroid};

    android::init_logging();
    let _ = mobile::ANDROID_APP.set(droid_app.clone());

    // `dirs` finds its data/cache directories through `$HOME` and the XDG variables, none of
    // which Android sets: point them at the app's private storage so the tile cache, favorites
    // and the remembered view persist.
    if let Some(files) = droid_app.internal_data_path() {
        let cache = files
            .parent()
            .map_or_else(|| files.join("cache"), |app| app.join("cache"));
        // SAFETY: first thing the process runs, before any other thread exists.
        unsafe {
            std::env::set_var("HOME", &files);
            std::env::set_var("XDG_DATA_HOME", &files);
            std::env::set_var("XDG_CACHE_HOME", &cache);
        }
    }

    let event_loop = EventLoop::<NativeEvent>::with_user_event()
        .with_android_app(droid_app.clone())
        .build()
        .expect("Failed to build event loop");

    launch(with_perf_overlay(
        LaunchConfig::new()
            .with_plugin(AndroidPlugin::new(droid_app))
            .with_gpu_resource_cache_limit(GpuResourceCacheLimit::from_mb(GPU_CACHE_MB))
            .with_window(WindowConfig::new(app))
            .with_event_loop(event_loop),
    ))
}

#[cfg(target_os = "android")]
mod android {
    use std::ffi::{CStr, c_char, c_int};

    /// Log filter (`RUST_LOG` syntax) when the `debug.rusty_map.log` system property is unset.
    const DEFAULT_LOG_FILTER: &str = "warn,rusty_map=info,map_component=info";

    /// Logs go to logcat, tag `rusty_map`. There's no environment to set `RUST_LOG` in, so the
    /// filter comes from a system property, read at launch:
    /// `adb shell setprop debug.rusty_map.log 'warn,map_component=debug'` then restart the app —
    /// `map_component=debug` logs per-frame paint, tile fetch and tile compile timings.
    pub fn init_logging() {
        let filter = system_property(c"debug.rusty_map.log")
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_LOG_FILTER.to_owned());
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Trace)
                .with_tag("rusty_map")
                .with_filter(android_logger::FilterBuilder::new().parse(&filter).build()),
        );
        log::info!("log filter: {filter}");
    }

    unsafe extern "C" {
        /// Bionic; `value` must hold `PROP_VALUE_MAX` (92) bytes.
        fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
    }

    fn system_property(name: &CStr) -> Option<String> {
        let mut value = [0 as c_char; 92];
        let len = unsafe { __system_property_get(name.as_ptr(), value.as_mut_ptr()) };
        (len > 0).then(|| {
            unsafe { CStr::from_ptr(value.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        })
    }
}
