//! Places the user saved: a star in the place drawer adds or removes one, the search pill and
//! the journey planner offer them as one-tap shortcuts, and the favorites page lists them with
//! live departures for the ones that are stops.
//!
//! Kept in `<data dir>/rusty_map/favorites.json` (e.g. `~/.local/share/rusty_map/` on Linux),
//! rewritten on every change; in memory only where the platform has no data directory.

use std::path::PathBuf;

use freya::prelude::*;
use serde::{Deserialize, Serialize};

use crate::components::place::SelectedPlace;

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Favorite {
    pub name: String,
    /// What it is ("Station", "Restaurant"), possibly empty.
    #[serde(default)]
    pub kind: String,
    pub lat: f64,
    pub lon: f64,
    /// A public-transport stop: the favorites page shows its live departures.
    #[serde(default)]
    pub stop: bool,
    /// The OSM element (`"node"`, id), to show its full details again when selected.
    #[serde(default)]
    pub osm: Option<(String, u64)>,
}

impl Favorite {
    pub fn from_place(place: &SelectedPlace) -> Option<Self> {
        let (lat, lon) = place.coords?;
        // Unnamed places (a post box) go by their kind, else their coordinates.
        let name = [&place.title, &place.subtitle]
            .into_iter()
            .find(|text| !text.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| crate::components::place::format_coordinates(lat, lon));
        Some(Self {
            name,
            kind: place.subtitle.clone(),
            lat,
            lon,
            stop: place.transit_stop.is_some(),
            osm: place.links.iter().find_map(|(_, _, url)| osm_element(url)),
        })
    }

    /// Same place: same name within ~10 m (a POI's tapped position and its search result's
    /// differ slightly).
    pub fn same_as(&self, name: &str, lat: f64, lon: f64) -> bool {
        self.name == name && (self.lat - lat).abs() < 1e-4 && (self.lon - lon).abs() < 1e-4
    }

    /// Stable per favorite, for diff keys.
    pub fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::hash::DefaultHasher::new();
        (
            &self.name,
            (self.lat * 1e5).round() as i64,
            (self.lon * 1e5).round() as i64,
        )
            .hash(&mut hasher);
        hasher.finish()
    }
}

/// `https://www.openstreetmap.org/node/123` → `("node", 123)`.
fn osm_element(url: &str) -> Option<(String, u64)> {
    let path = url.strip_prefix("https://www.openstreetmap.org/")?;
    let (kind, id) = path.split_once('/')?;
    matches!(kind, "node" | "way" | "relation")
        .then(|| Some((kind.to_string(), id.parse().ok()?)))?
}

fn store_path() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("rusty_map").join("favorites.json"))
}

fn load() -> Vec<Favorite> {
    let Some(path) = store_path() else {
        return Vec::new();
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|err| {
            log::warn!(
                "[favorites] {} unreadable ({err}), starting empty",
                path.display()
            );
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

fn save(list: &[Favorite]) {
    let Some(path) = store_path() else {
        return;
    };
    let result = (|| {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(list).map_err(std::io::Error::other)?;
        // Written aside then renamed, so a crash mid-write can't lose the existing list.
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, json)?;
        std::fs::rename(&temporary, &path)
    })();
    if let Err(err) = result {
        log::warn!("[favorites] couldn't save {}: {err}", path.display());
    }
}

/// The saved places, provided as context by `HomePage`.
#[derive(Clone, Copy, PartialEq)]
pub struct Favorites {
    pub list: State<Vec<Favorite>>,
    /// The favorites page is showing.
    pub page_open: State<bool>,
}

impl Favorites {
    /// A hook — call it directly from a component's `render()` body.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            list: use_state(load),
            page_open: use_state(|| false),
        }
    }

    pub fn contains(&self, name: &str, lat: f64, lon: f64) -> bool {
        self.list
            .read()
            .iter()
            .any(|favorite| favorite.same_as(name, lat, lon))
    }

    /// Saves `favorite`, or removes it when already saved.
    pub fn toggle(&mut self, favorite: Favorite) {
        let mut list = self.list.peek().clone();
        let before = list.len();
        list.retain(|saved| !saved.same_as(&favorite.name, favorite.lat, favorite.lon));
        if list.len() == before {
            list.push(favorite);
        }
        save(&list);
        self.list.set(list);
    }

    pub fn remove(&mut self, favorite: &Favorite) {
        let mut list = self.list.peek().clone();
        list.retain(|saved| saved != favorite);
        save(&list);
        self.list.set(list);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_osm_links() {
        assert_eq!(
            osm_element("https://www.openstreetmap.org/node/12299849571"),
            Some(("node".to_string(), 12299849571))
        );
        assert_eq!(osm_element("https://www.openstreetmap.org/way/x"), None);
        assert_eq!(osm_element("https://example.org/node/1"), None);
    }

    #[test]
    fn matches_nearby_same_name() {
        let favorite = Favorite {
            name: "Gare".into(),
            kind: String::new(),
            lat: 47.96104,
            lon: 3.513,
            stop: true,
            osm: None,
        };
        assert!(favorite.same_as("Gare", 47.96098, 3.51305));
        assert!(!favorite.same_as("Gare", 47.97, 3.513));
        assert!(!favorite.same_as("Poste", 47.96104, 3.513));
    }

    #[test]
    fn old_files_still_load() {
        let list: Vec<Favorite> =
            serde_json::from_str(r#"[{"name": "Home", "lat": 48.8, "lon": 2.3}]"#).unwrap();
        assert_eq!(list[0].kind, "");
        assert!(!list[0].stop);
    }
}
