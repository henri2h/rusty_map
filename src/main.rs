pub mod components;
pub mod pages;
pub mod route;

use freya::prelude::*;
use freya_router::prelude::{Router, RouterConfig};

use crate::route::Route;

fn main() {
    env_logger::init();

    launch(
        LaunchConfig::new()
            // The map keeps many large tile bitmaps on screen at once (over-zoom re-bakes up to
            // 4096², line overlays up to 2048²); past the default 256 MiB Skia evicts and
            // re-uploads them every frame — measured ~47% of main-thread time.
            .with_gpu_resource_cache_limit(GpuResourceCacheLimit::from_mb(1024))
            .with_window(WindowConfig::new(app)),
    );
}

fn app() -> impl IntoElement {
    Router::<Route>::new(|| RouterConfig::default().with_initial_path(Route::HomePage))
}
