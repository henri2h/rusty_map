//! App-level public-transport state: which transit view the drawer shows (a tapped track's
//! lines, or one trip's stops) and the journey planner's endpoints. Provided as context by
//! `HomePage`, so drawer sections deep in the tree (a departure row) can open views directly.

use std::sync::atomic::{AtomicU64, Ordering};

use freya::prelude::*;
use map_component::{MapController, TappedLine};

use super::{
    api::Departure,
    osm_lines::{TrackInfo, routes_from_tile},
    when::When,
};

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
    /// One vehicle's run, opened from a departure row at stop `from_stop`.
    Trip {
        id: u64,
        departure: Departure,
        from_stop: String,
    },
}

impl TransitView {
    pub fn id(&self) -> u64 {
        match self {
            TransitView::Track { id, .. } | TransitView::Trip { id, .. } => *id,
        }
    }
}

static NEXT_VIEW_ID: AtomicU64 = AtomicU64::new(1);

fn next_view_id() -> u64 {
    NEXT_VIEW_ID.fetch_add(1, Ordering::Relaxed)
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

    pub fn show_trip(&mut self, departure: Departure, from_stop: String) {
        self.map.set_route_overlay(None);
        self.view.set(Some(TransitView::Trip {
            id: next_view_id(),
            departure,
            from_stop,
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
    }
}
