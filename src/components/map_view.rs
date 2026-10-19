//! Where the map was (centre and zoom) when the app was last used, restored at the next start.
//! Kept in `<data dir>/rusty_map/map_view.json`, saved a couple of seconds after the view stops
//! changing.

use std::{path::PathBuf, time::Duration};

use freya::prelude::*;
use map_component::{
    MapController,
    util::{
        tiles::background_delay,
        viewport::{center_to_lonlat, lonlat_to_center},
    },
};
use serde::{Deserialize, Serialize};

/// How often the view is checked for changes to save.
const SAVE_EVERY: Duration = Duration::from_secs(2);

/// `center` is the map's anchor point (see `Viewport`), as longitude/latitude.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
struct MapView {
    lon: f64,
    lat: f64,
    zoom: f32,
}

fn store_path() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("rusty_map").join("map_view.json"))
}

fn load() -> Option<MapView> {
    let text = std::fs::read_to_string(store_path()?).ok()?;
    let view: MapView = serde_json::from_str(&text).ok()?;
    (view.lon.is_finite() && view.lat.is_finite() && view.zoom.is_finite()).then_some(view)
}

fn save(view: MapView) {
    let Some(path) = store_path() else {
        return;
    };
    let result = (|| {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string(&view).map_err(std::io::Error::other)?;
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, json)?;
        std::fs::rename(&temporary, &path)
    })();
    if let Err(err) = result {
        log::warn!("[map_view] couldn't save {}: {err}", path.display());
    }
}

fn current(map: &MapController) -> MapView {
    let (lon, lat) = center_to_lonlat(*map.center.peek());
    MapView {
        lon,
        lat,
        zoom: *map.zoom.peek(),
    }
}

/// A hook: puts `map` back where it was last time, then keeps saving its view while the calling
/// component lives. Polls instead of subscribing, so panning doesn't re-render the caller.
pub fn use_remembered_view(map: &MapController) {
    use_hook({
        let map = map.clone();
        move || {
            let mut saved = load();
            if let Some(view) = saved {
                let (mut center, mut zoom) = (map.center, map.zoom);
                center.set(lonlat_to_center(view.lon, view.lat));
                zoom.set(view.zoom);
            }
            spawn(async move {
                loop {
                    background_delay(SAVE_EVERY).await;
                    let view = current(&map);
                    if saved != Some(view) {
                        save(view);
                        saved = Some(view);
                    }
                }
            });
        }
    });
}
