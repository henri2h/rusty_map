use std::{cell::RefCell, rc::Rc, sync::Arc};

use freya::prelude::*;
use torin::geometry::{Area, CursorPoint};

use crate::{
    style::{self, StyleSpec},
    util::viewport::max_zoom_ui,
};

/// Map-rendering state, shared between [`crate::MapComponent`] (which reads/writes it while
/// drawing the raw map and handling pan/zoom/tap) and whatever host UI needs to recenter the map
/// or drop a pin.
#[derive(Clone)]
pub struct MapController {
    pub center: State<CursorPoint>,
    pub zoom: State<f32>,
    pub viewport_size: State<Area>,

    /// Zoom-independent world position (see `Viewport::screen_to_world`) of a dropped pin drawn
    /// each frame by the paint loop; set by the host app (e.g. for a geocoded search result).
    pub marker_world: State<Option<(f64, f64)>>,

    pub style: Rc<RefCell<Arc<StyleSpec>>>,
    pub platform: PlatformWindow,
}

impl PartialEq for MapController {
    fn eq(&self, other: &Self) -> bool {
        self.center == other.center
            && self.zoom == other.zoom
            && self.viewport_size == other.viewport_size
            && self.marker_world == other.marker_world
            && Rc::ptr_eq(&self.style, &other.style)
    }
}

impl MapController {
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let center = use_state(CursorPoint::zero);
        let zoom = use_state(|| {
            (style::default::DEFAULT_MIN_ZOOM + max_zoom_ui(style::default::DEFAULT_MAX_REAL_ZOOM))
                / 2.0
        });
        let viewport_size = use_state(Area::default);
        let marker_world = use_state::<Option<(f64, f64)>>(|| None);
        let style = use_hook(|| Rc::new(RefCell::new(Arc::new(style::default::default_style()))));
        let platform = use_hook(|| Platform::get().current_window());

        Self {
            center,
            zoom,
            viewport_size,
            marker_world,
            style,
            platform,
        }
    }
}
