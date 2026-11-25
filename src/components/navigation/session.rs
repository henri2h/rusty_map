//! Following a walk as fixes arrive: where on the route the walker is, the next instruction and
//! how far it is, and whether they've left the route or arrived.

use super::{
    geo::{distance_m, project_on_segment},
    route::WalkRoute,
};

/// Within this of the next instruction, it's done: the one after shows.
const ADVANCE_M: f64 = 10.0;
/// Further than this from the route is off it…
const OFF_ROUTE_M: f64 = 35.0;
/// …for this many fixes in a row, or this long.
const OFF_ROUTE_FIXES: u32 = 2;
const OFF_ROUTE_S: f64 = 15.0;
/// Within this of the end is there.
const ARRIVE_M: f64 = 20.0;
/// Moving further than this along the route since the last fix costs a candidate match: a route
/// passing close to itself (out and back along a street) would otherwise let a fix snap to the
/// wrong pass, metres away along the route but centimetres closer.
const FREE_JUMP_M: f64 = 50.0;
/// Metres of distance from the route charged per metre jumped beyond [`FREE_JUMP_M`].
const JUMP_COST: f64 = 0.2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    OnRoute,
    OffRoute,
    Arrived,
}

/// Where the walker is, after a fix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Update {
    pub status: Status,
    /// Index in the route's maneuvers of the next instruction to follow.
    pub maneuver: usize,
    /// The next instruction changed with this fix.
    pub advanced: bool,
    pub to_next_m: f64,
    pub remaining_m: f64,
    pub remaining_s: f64,
    /// Metres along the route of the matched point.
    pub along_m: f64,
}

/// Matching state carried from fix to fix.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Progress {
    /// Segment (from point `segment` to `segment + 1`) last matched; `None` before the first fix.
    segment: Option<usize>,
    /// Metres along the route last matched.
    along: f64,
    maneuver: usize,
    off_fixes: u32,
    /// When (seconds, any clock) the walker was first seen off the route.
    off_since: Option<f64>,
}

impl Progress {
    /// Takes the fix at `position` (`(lon, lat)`), seen at `now` seconds.
    pub fn update(&mut self, route: &WalkRoute, position: (f64, f64), now: f64) -> Update {
        let (segment, distance, along) = self.match_position(route, position);

        let off = distance > OFF_ROUTE_M;
        if off {
            self.off_fixes += 1;
            self.off_since.get_or_insert(now);
        } else {
            self.off_fixes = 0;
            self.off_since = None;
            self.segment = Some(segment);
            self.along = along;
        }
        let off_route = off
            && (self.off_fixes >= OFF_ROUTE_FIXES
                || self
                    .off_since
                    .is_some_and(|since| now - since >= OFF_ROUTE_S));

        // While off it, the last point matched on the route stands.
        let along = if off { self.along } else { along };
        let previous = self.maneuver;
        // The next instruction: the first ahead of the walker by more than the advance margin.
        self.maneuver = route
            .maneuvers
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, maneuver)| route.along_at(maneuver) > along + ADVANCE_M)
            .map_or(route.maneuvers.len().saturating_sub(1), |(index, _)| index)
            .max(previous);

        let total = *route.along.last().unwrap_or(&0.0);
        let remaining_m = (total - along).max(0.0);
        let at_end = distance_m(position, *route.points.last().unwrap_or(&position)) < ARRIVE_M;
        let status = if at_end || (!off && remaining_m < ARRIVE_M) {
            Status::Arrived
        } else if off_route {
            Status::OffRoute
        } else {
            Status::OnRoute
        };
        let to_next_m = route
            .maneuvers
            .get(self.maneuver)
            .map_or(0.0, |maneuver| (route.along_at(maneuver) - along).max(0.0));

        Update {
            status,
            maneuver: self.maneuver,
            advanced: self.maneuver != previous,
            to_next_m,
            remaining_m,
            remaining_s: remaining_m / route.pace(),
            along_m: along,
        }
    }

    /// The best match of `position` on the route: its segment, distance from it, and metres
    /// along the route. The closest point, except that after the first fix, a jump far ahead
    /// or back along the route costs extra (see [`FREE_JUMP_M`]) — while on it; off it, the
    /// walker may rejoin anywhere.
    fn match_position(&self, route: &WalkRoute, position: (f64, f64)) -> (usize, f64, f64) {
        let tracking = self.segment.is_some() && self.off_fixes == 0;
        let mut best = (0, f64::INFINITY, 0.0);
        let mut best_score = f64::INFINITY;
        for index in 0..route.points.len().saturating_sub(1) {
            let (a, b) = (route.points[index], route.points[index + 1]);
            let (distance, t) = project_on_segment(position, a, b);
            let along = route.along[index] + (route.along[index + 1] - route.along[index]) * t;
            let jump = (along - self.along).abs() - FREE_JUMP_M;
            let score = if tracking && jump > 0.0 {
                distance + jump * JUMP_COST
            } else {
                distance
            };
            if score < best_score {
                best_score = score;
                best = (index, distance, along);
            }
        }
        best
    }
}
