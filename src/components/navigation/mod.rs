//! Walking navigation: a route from Transitous (`route`), turned into instructions
//! (`maneuvers`), followed fix by fix (`session`), guided on screen (`ui`).
//!
//! [`NavigationController`] holds the walk being guided. `HomePage` provides it; the Directions
//! sheet's Walk tab starts it (`ui::WalkRoute`), and `ui::NavigationOverlay` follows the
//! location readings while it runs: progress, re-routing, the map, the banner.

pub mod elevation;
pub(crate) mod geo;
pub mod maneuvers;
pub mod profile_view;
pub mod route;
pub mod session;
pub mod streets;
pub mod ui;

use std::{rc::Rc, time::Duration};

use freya::prelude::*;
use map_component::{
    Insets, MapController, OverlayPath, RouteOverlay, util::viewport::lonlat_to_world,
};
use tokio::sync::mpsc;
use torin::geometry::CursorPoint;

use crate::{
    components::transit::Endpoint,
    location::{self, Fix},
    mobile::{self, Haptic, haptic, keep_screen_on},
};

use self::{
    geo::point_at,
    route::{WalkRoute, fetch_walk_route},
    session::{Progress, Status, Update},
};

/// At most one re-route this often: the router is a shared community service.
const REROUTE_EVERY: Duration = Duration::from_secs(30);
const ROUTE_COLOR: Color = Color::from_rgb(26, 115, 232);
/// Zoom following starts at (closer if the map already is).
const FOLLOW_ZOOM: f32 = 17.0;
/// Screen kept clear of the banner and bottom bar when centring on the walker.
const BANNER_HEIGHT: f32 = 140.0;
const BOTTOM_BAR_HEIGHT: f32 = 96.0;

/// The walk being guided.
#[derive(Clone, PartialEq)]
pub struct Session {
    pub route: Rc<WalkRoute>,
    pub destination: Endpoint,
    pub progress: Progress,
    /// After the latest fix; `None` until the first one.
    pub update: Option<Update>,
    pub rerouting: bool,
    pub arrived: bool,
    /// When the latest re-route was asked for (seconds since the Unix epoch).
    pub rerouted_at: Option<f64>,
    /// The map keeps centring on the walker (until they pan it away).
    pub following: bool,
    /// Where the latest centring left the map: a different centre means the user panned.
    pub followed_to: Option<CursorPoint>,
}

#[derive(Clone)]
pub struct NavigationController {
    pub session: State<Option<Session>>,
    pub map: MapController,
    reroute: mpsc::UnboundedSender<(f64, f64)>,
}

impl PartialEq for NavigationController {
    fn eq(&self, other: &Self) -> bool {
        self.session == other.session
            && self.map == other.map
            && self.reroute.same_channel(&other.reroute)
    }
}

impl NavigationController {
    /// A hook — call it directly from a component's `render()` body (`HomePage`).
    pub fn new(map: MapController) -> Self {
        let session = use_state(|| None::<Session>);
        let reroute = use_hook({
            let map = map.clone();
            move || {
                let (sender, receiver) = mpsc::unbounded_channel();
                // In this hook's scope: the task lives as long as the page.
                spawn(reroute_task(receiver, session, map));
                sender
            }
        });
        Self {
            session,
            map,
            reroute,
        }
    }

    pub fn is_active(&self) -> bool {
        self.session.read().is_some()
    }

    /// Starts guiding `route` to `destination`: location on, screen kept awake.
    pub fn start(&mut self, route: WalkRoute, destination: Endpoint) {
        location::set_tracking(true);
        location::simulate_walk(route.points.clone());
        keep_screen_on(true);
        haptic(Haptic::Toggle);
        draw_remaining(&mut self.map, &route, 0.0);
        self.session.set(Some(Session {
            route: Rc::new(route),
            destination,
            progress: Progress::default(),
            update: None,
            rerouting: false,
            arrived: false,
            rerouted_at: None,
            following: true,
            followed_to: None,
        }));
    }

    /// Ends the walk. Location stays on: the dot is still useful, and the location control
    /// turns it off.
    pub fn stop(&mut self) {
        location::stop_simulation();
        keep_screen_on(false);
        self.map.set_route_overlay(None);
        self.session.set(None);
    }

    /// Centres on the walker again after they panned away.
    pub fn recenter(&mut self) {
        if let Some(session) = self.session.write().as_mut() {
            session.following = true;
            session.followed_to = None;
        }
    }

    /// Takes a location fix (`now`: seconds since the Unix epoch): progress, haptics, arrival,
    /// the remaining route on the map, following the walker, re-routing when off the route.
    pub(crate) fn on_fix(&mut self, fix: Fix, now: f64, insets: mobile::Insets) {
        let off_route = {
            let mut guard = self.session.write();
            let Some(walk) = guard.as_mut().filter(|walk| !walk.arrived) else {
                return;
            };
            let update = walk.progress.update(&walk.route, (fix.lon, fix.lat), now);
            walk.update = Some(update);
            match update.status {
                Status::Arrived => {
                    walk.arrived = true;
                    keep_screen_on(false);
                    location::stop_simulation();
                    haptic(Haptic::Toggle);
                }
                _ if update.advanced => haptic(Haptic::Select),
                _ => {}
            }
            draw_remaining(&mut self.map, &walk.route, update.along_m);

            if walk.following {
                let panned = walk
                    .followed_to
                    .is_some_and(|at| *self.map.center.peek() != at);
                if panned {
                    walk.following = false;
                } else {
                    let zoom = *self.map.zoom.peek();
                    let zoom = if walk.followed_to.is_none() {
                        zoom.max(FOLLOW_ZOOM)
                    } else {
                        zoom
                    };
                    let point = lonlat_to_world(fix.lon, fix.lat);
                    self.map.fit_world_bounds(
                        (point, point),
                        Insets {
                            top: insets.top + BANNER_HEIGHT,
                            bottom: insets.bottom + BOTTOM_BAR_HEIGHT,
                            ..Insets::default()
                        },
                        zoom,
                    );
                    walk.followed_to = Some(*self.map.center.peek());
                }
            }
            update.status == Status::OffRoute
        };
        if off_route {
            self.request_reroute((fix.lat, fix.lon), now);
        }
    }

    /// Asks for a new route from `from` (`(lat, lon)`), unless one was asked for recently.
    fn request_reroute(&mut self, from: (f64, f64), now: f64) {
        let mut session = self.session.write();
        let Some(session) = session.as_mut() else {
            return;
        };
        if session.rerouting
            || session
                .rerouted_at
                .is_some_and(|at| now - at < REROUTE_EVERY.as_secs_f64())
        {
            return;
        }
        session.rerouting = true;
        session.rerouted_at = Some(now);
        let _ = self.reroute.send(from);
    }
}

/// Fetches each re-route asked for, and swaps it in (or gives up quietly, trying again later).
async fn reroute_task(
    mut requests: mpsc::UnboundedReceiver<(f64, f64)>,
    mut session: State<Option<Session>>,
    mut map: MapController,
) {
    while let Some(from) = requests.recv().await {
        let Some(destination) = session.peek().as_ref().map(|s| s.destination.clone()) else {
            continue;
        };
        let result = fetch_walk_route(from, (destination.lat, destination.lon)).await;
        let mut current = session.write();
        // Ended meanwhile, or another walk started.
        let Some(walk) = current
            .as_mut()
            .filter(|walk| walk.destination == destination)
        else {
            continue;
        };
        walk.rerouting = false;
        match result {
            Ok(route) => {
                draw_remaining(&mut map, &route, 0.0);
                walk.route = Rc::new(route);
                walk.progress = Progress::default();
                walk.update = None;
            }
            Err(err) => log::warn!("[navigation] re-route failed: {err:?}"),
        }
    }
}

/// Draws what's left of `route` from `along` metres on.
fn draw_remaining(map: &mut MapController, route: &WalkRoute, along: f64) {
    let start = point_at(&route.points, &route.along, along);
    let first_ahead = route.along.partition_point(|distance| *distance <= along);
    let remaining: Vec<(f64, f64)> = std::iter::once(start)
        .chain(
            route.points[first_ahead.min(route.points.len())..]
                .iter()
                .copied(),
        )
        .collect();
    map.set_route_overlay(Some(RouteOverlay {
        paths: vec![OverlayPath::from_lonlat(
            &remaining,
            ROUTE_COLOR,
            6.0,
            false,
        )],
        stops: Vec::new(),
    }));
}
