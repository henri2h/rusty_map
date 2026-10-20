//! Fast EV chargers (see [`api`]): a map layer behind a toggle, coloured by live availability,
//! and the live state of a tapped station's charge points in the drawer ([`ChargerLive`]).

pub mod api;
mod live;

use std::{rc::Rc, time::Duration};

use freya::prelude::*;
use map_component::{MapController, MapMarker, MarkerSet, util::tiles::background_delay};

pub use live::ChargerLive;

use api::{Charger, StationState};

use crate::components::place::PlaceController;

/// QualiCharge refreshes its list about every 5 minutes; asking more often only returns the
/// same file.
const REFRESH_EVERY: Duration = Duration::from_secs(150);

pub const FREE: Color = Color::from_rgb(24, 128, 56);
pub const BUSY: Color = Color::from_rgb(230, 124, 0);
pub const DOWN: Color = Color::from_rgb(197, 34, 31);
pub const UNKNOWN: Color = Color::from_rgb(120, 124, 130);

pub fn state_color(state: StationState) -> Color {
    match state {
        StationState::Free => FREE,
        StationState::Busy => BUSY,
        StationState::OutOfService => DOWN,
        StationState::Unknown => UNKNOWN,
    }
}

/// Charger layer state, provided as context by `HomePage`.
#[derive(Clone, Copy, PartialEq)]
pub struct Chargers {
    /// The layer is shown (the map toggle).
    pub enabled: State<bool>,
    /// The latest list; kept while the layer is off, so turning it back on is instant.
    pub list: State<Rc<Vec<Charger>>>,
    etag: State<Option<String>>,
}

impl Chargers {
    /// A hook — call it directly from a component's `render()` body.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            enabled: use_state(|| false),
            list: use_state(|| Rc::new(Vec::new())),
            etag: use_state(|| None),
        }
    }

    /// The station behind a map marker id (its index in [`Self::list`]).
    pub fn by_marker(&self, id: u64) -> Option<Charger> {
        self.list.peek().get(id as usize).cloned()
    }
}

/// While mounted (the toggle is on): keeps [`Chargers::list`] fresh and the map's markers in
/// sync with it and with the selected station. Renders nothing.
#[derive(PartialEq)]
pub struct ChargerLayer {
    pub chargers: Chargers,
    pub map: MapController,
    pub places: PlaceController,
}

impl Component for ChargerLayer {
    fn render(&self) -> impl IntoElement {
        use_hook({
            let chargers = self.chargers;
            let platform = self.map.platform.clone();
            move || {
                spawn(async move {
                    let (mut list, mut etag) = (chargers.list, chargers.etag);
                    loop {
                        let known = etag.peek().clone();
                        match api::fetch_chargers(known).await {
                            Ok(Some((fresh, tag))) => {
                                log::info!("[chargers] {} fast chargers", fresh.len());
                                list.set(Rc::new(fresh));
                                etag.set(tag);
                                platform.send(UserEvent::RequestRedraw);
                            }
                            Ok(None) => {}
                            Err(err) => log::warn!("[chargers] list failed: {err:?}"),
                        }
                        background_delay(REFRESH_EVERY).await;
                    }
                })
            }
        });

        // Markers follow the list and the selection.
        use_side_effect({
            let list = self.chargers.list;
            let selected = self.places.selected_place;
            let mut map = self.map.clone();
            move || {
                let list = list.read().clone();
                let selected_id = selected
                    .read()
                    .as_ref()
                    .and_then(|place| place.charger.clone());
                let markers = list
                    .iter()
                    .enumerate()
                    .map(|(index, charger)| {
                        MapMarker::from_lonlat(
                            index as u64,
                            charger.lon,
                            charger.lat,
                            state_color(charger.availability.state()),
                        )
                    })
                    .collect();
                let selected = selected_id
                    .and_then(|id| list.iter().position(|charger| charger.id == id))
                    .map(|index| index as u64);
                map.set_markers(Some(MarkerSet {
                    markers,
                    selected,
                    ..Default::default()
                }));
            }
        });

        use_drop({
            let mut map = self.map.clone();
            move || map.set_markers(None)
        });

        rect()
    }
}
