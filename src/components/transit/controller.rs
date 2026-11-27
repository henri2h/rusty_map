//! App-level public-transport state: which transit view the drawer shows (a tapped track's
//! lines, or one trip's stops) and the journey planner's endpoints. Provided as context by
//! `HomePage`, so drawer sections deep in the tree (a departure row) can open views directly.

use std::sync::atomic::{AtomicU64, Ordering};

use freya::prelude::*;
use map_component::{IconGlyph, MapController, MapItem, MapLayer, TappedLine};

use crate::components::search::util::search;

use super::{
    api::Departure,
    osm_lines::{RouteInfo, TrackInfo, routes_from_tile},
    when::When,
};

/// The planner's start and destination dots on the map (see [`TransitController::show_endpoints`]).
const ENDPOINTS_LAYER: u64 = 0x7472_6970; // "trip"
/// Colors of the planner's start and destination, as on its fields.
pub(crate) const START_COLOR: Color = Color::from_rgb(24, 128, 56);
pub(crate) const END_COLOR: Color = Color::from_rgb(217, 48, 37);

/// One end of a planned journey.
#[derive(Clone, PartialEq)]
pub struct Endpoint {
    pub name: String,
    pub lat: f64,
    pub lon: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Field {
    From,
    To,
}

#[derive(Clone, PartialEq)]
pub enum TransitView {
    /// A rail/metro/tram track tapped on the map.
    Track {
        id: u64,
        way_id: Option<u64>,
        /// Where the tap landed and how far from it the line may be.
        lat: f64,
        lon: f64,
        radius_m: f64,
        /// `transportation` subclass from the tile (`subway`, `tram`, `rail`), shown while the
        /// OSM lookup runs or when it finds nothing.
        kind: String,
        /// The lines using the track, straight from rusty_map's own tiles (`transit_lines`):
        /// no lookup needed. `None` with other tile sources.
        from_tile: Option<TrackInfo>,
    },
    /// One line: its whole route on the map, its stops with that line's next departures.
    Line {
        id: u64,
        route: RouteInfo,
        /// The tapped track it was opened from: finds the line's relation when the tile didn't
        /// carry its id.
        way_id: Option<u64>,
    },
    /// One vehicle's run, opened from a departure row at stop `from_stop`.
    Trip {
        id: u64,
        departure: Departure,
        from_stop: String,
        /// Where `from_stop` is, to save this line and direction there as a favorite.
        stop_position: Option<(f64, f64)>,
    },
}

impl TransitView {
    pub fn id(&self) -> u64 {
        match self {
            TransitView::Track { id, .. }
            | TransitView::Trip { id, .. }
            | TransitView::Line { id, .. } => *id,
        }
    }
}

static NEXT_VIEW_ID: AtomicU64 = AtomicU64::new(1);

fn next_view_id() -> u64 {
    NEXT_VIEW_ID.fetch_add(1, Ordering::Relaxed)
}

/// How the Directions sheet gets there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TravelMode {
    #[default]
    Transit,
    Walk,
}

#[derive(Clone, PartialEq)]
pub struct TransitController {
    pub view: State<Option<TransitView>>,
    pub planner_open: State<bool>,
    pub from: State<Option<Endpoint>>,
    pub to: State<Option<Endpoint>>,
    /// The field a search or map pick fills.
    pub active_field: State<Field>,
    /// Waiting for a map tap to set the active field (crosshair cursor, banner over the map).
    /// Map taps only choose endpoints in this mode, so browsing the map while reading results
    /// doesn't overwrite them.
    pub picking: State<bool>,
    /// When the planned journey leaves or arrives.
    pub when: State<When>,
    /// The selected itinerary's step-by-step view is open, in place of the planner's form and
    /// list (see `planner::Itineraries`).
    pub trip_open: State<bool>,
    pub mode: State<TravelMode>,
    /// The map, to draw and frame trips/journeys on it.
    pub map: MapController,
}

impl TransitController {
    /// A hook — call it directly from a component's `render()` body.
    pub fn new(map: MapController) -> Self {
        Self {
            view: use_state(|| None),
            planner_open: use_state(|| false),
            from: use_state(|| None),
            to: use_state(|| None),
            active_field: use_state(|| Field::From),
            picking: use_state(|| false),
            when: use_state(When::default),
            trip_open: use_state(|| false),
            mode: use_state(TravelMode::default),
            map,
        }
    }

    pub fn show_track(&mut self, line: &TappedLine) {
        let text = |key: &str| match line.properties.get(key) {
            Some(mvt_reader::feature::Value::String(value)) => Some(value.clone()),
            _ => None,
        };
        // `transit_lines` names its `mode` after the route (`train`); tracks use `rail`.
        let kind = text("subclass")
            .or_else(|| text("mode").map(|mode| if mode == "train" { "rail".into() } else { mode }))
            .unwrap_or_default();
        let routes = routes_from_tile(&line.properties);
        let from_tile = (!routes.is_empty()).then(|| TrackInfo {
            name: None,
            railway: Some(kind.clone()),
            operator: None,
            geometry: line.path.clone(),
            routes,
        });
        self.map.set_route_overlay(None);
        self.view.set(Some(TransitView::Track {
            id: next_view_id(),
            way_id: line.osm_way_id,
            lat: line.lat,
            lon: line.lon,
            radius_m: line.hit_radius_m,
            kind,
            from_tile,
        }));
    }

    pub fn show_line(&mut self, route: RouteInfo, way_id: Option<u64>) {
        self.map.set_route_overlay(None);
        self.view.set(Some(TransitView::Line {
            id: next_view_id(),
            route,
            way_id,
        }));
    }

    pub fn show_trip(
        &mut self,
        departure: Departure,
        from_stop: String,
        stop_position: Option<(f64, f64)>,
    ) {
        self.map.set_route_overlay(None);
        self.view.set(Some(TransitView::Trip {
            id: next_view_id(),
            departure,
            from_stop,
            stop_position,
        }));
    }

    /// Back to whatever was under the view (a selected place, or the bare map).
    pub fn close_view(&mut self) {
        if self.view.peek().is_some() {
            self.view.set(None);
            self.map.set_route_overlay(None);
        }
    }

    /// Opens the planner, heading to `to` when given (a place's "Directions" button).
    pub fn open_planner(&mut self, to: Option<Endpoint>) {
        self.close_view();
        self.to.set(to);
        self.planner_open.set(true);
        self.endpoints_changed();
        // The start is still unknown: invite picking it right away.
        self.start_picking(Field::From);
    }

    /// Enters map-picking mode for `field`.
    pub fn start_picking(&mut self, field: Field) {
        self.active_field.set(field);
        self.picking.set(true);
        self.map.cursor.set(Some(CursorIcon::Crosshair));
    }

    pub fn stop_picking(&mut self) {
        if *self.picking.peek() {
            self.picking.set(false);
            self.map.cursor.set(None);
        }
    }

    pub fn close_planner(&mut self) {
        self.stop_picking();
        self.planner_open.set(false);
        self.from.set(None);
        self.to.set(None);
        self.endpoints_changed();
        self.map.set_route_overlay(None);
    }

    /// Fills the active field (a map tap or picked place while planning), then moves on to the
    /// other field if it's still empty — still picking, if a map pick filled this one — or else
    /// leaves picking mode.
    pub fn set_endpoint(&mut self, endpoint: Endpoint) {
        let field = *self.active_field.peek();
        match field {
            Field::From => self.from.set(Some(endpoint)),
            Field::To => self.to.set(Some(endpoint)),
        }
        self.endpoints_changed();
        let other_empty = match field {
            Field::From => self.to.peek().is_none(),
            Field::To => self.from.peek().is_none(),
        };
        if other_empty {
            self.active_field.set(match field {
                Field::From => Field::To,
                Field::To => Field::From,
            });
        } else {
            self.stop_picking();
        }
    }

    pub fn swap_endpoints(&mut self) {
        let from = self.from.peek().clone();
        let to = self.to.peek().clone();
        self.from.set(to);
        self.to.set(from);
        self.endpoints_changed();
    }

    /// One step back through what transit shows: a view, else an itinerary's steps (to the
    /// list), else the planner. `false` when none was open, for the caller to go back further.
    pub fn back(&mut self) -> bool {
        if self.view.peek().is_some() {
            self.close_view();
        } else if *self.trip_open.peek() {
            self.close_trip();
        } else if *self.planner_open.peek() {
            self.close_planner();
        } else {
            return false;
        }
        true
    }

    /// Back from the selected itinerary's steps to the planner and its list.
    pub fn close_trip(&mut self) {
        if *self.trip_open.peek() {
            self.trip_open.set(false);
        }
    }

    /// Fills the active field with a point tapped on the map (see [`Self::set_endpoint`]), named
    /// by its coordinates until its street address comes in.
    pub fn set_map_point(&mut self, lat: f64, lon: f64) {
        let placeholder = format!("Point on map ({lat:.4}, {lon:.4})");
        self.set_endpoint(Endpoint {
            name: placeholder.clone(),
            lat,
            lon,
        });
        let (mut from, mut to) = (self.from, self.to);
        // In the window's scope: the tap's own element doesn't outlive a re-render.
        spawn_in_window(async move {
            let Ok(Some(address)) = search::reverse_address(lat, lon).await else {
                return;
            };
            // Renamed only if still that point: it may have been replaced meanwhile. Set
            // directly, not through `set_endpoint`: the same place, nothing else to update.
            for endpoint in [&mut from, &mut to] {
                let unchanged = endpoint
                    .peek()
                    .as_ref()
                    .is_some_and(|endpoint| endpoint.name == placeholder);
                if unchanged {
                    endpoint.set(Some(Endpoint {
                        name: address.clone(),
                        lat,
                        lon,
                    }));
                }
            }
        });
    }

    /// After the start or destination changed: what was shown for the old journey goes (its
    /// steps), and the new ends are marked on the map.
    fn endpoints_changed(&mut self) {
        self.close_trip();
        self.show_endpoints();
    }

    /// Marks the planner's start and destination on the map, so a point picked on it shows
    /// where it landed (and the route's ends stand out).
    fn show_endpoints(&mut self) {
        let dot = |endpoint: &Option<Endpoint>, id, color| {
            endpoint.as_ref().map(|endpoint| {
                MapItem::icon_lonlat(
                    id,
                    endpoint.lon,
                    endpoint.lat,
                    IconGlyph::Dot,
                    0.0,
                    color,
                    18.0,
                )
            })
        };
        let items: Vec<MapItem> = [
            dot(&self.from.peek(), 0, START_COLOR),
            dot(&self.to.peek(), 1, END_COLOR),
        ]
        .into_iter()
        .flatten()
        .collect();
        if items.is_empty() {
            self.map.remove_layer(ENDPOINTS_LAYER);
        } else {
            self.map.set_layer(MapLayer::new(ENDPOINTS_LAYER, items));
        }
    }
}
