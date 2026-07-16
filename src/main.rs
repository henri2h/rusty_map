pub mod components;
pub mod pages;
pub mod route;
pub mod utils;

use freya::prelude::*;
use freya_router::prelude::{Router, RouterConfig};

use crate::route::Route;

fn main() {
    env_logger::init();

    launch(LaunchConfig::new().with_window(WindowConfig::new(app)));
}

fn app() -> impl IntoElement {
    Router::<Route>::new(|| RouterConfig::default().with_initial_path(Route::HomePage))
}
