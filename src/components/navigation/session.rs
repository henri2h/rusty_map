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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::navigation::{
        geo::cumulative,
        maneuvers::{RawStep, build},
    };

    const M: f64 = 1.0 / 111_320.0;

    /// A point given in metres (east, north) from a Paris origin.
    fn at(x: f64, y: f64) -> (f64, f64) {
        let lat0 = 48.85f64;
        (2.35 + x * M / lat0.to_radians().cos(), lat0 + y * M)
    }

    fn route(steps: &[(&str, &[(f64, f64)])]) -> WalkRoute {
        let steps: Vec<RawStep> = steps
            .iter()
            .map(|(street, points)| RawStep {
                direction: "CONTINUE".into(),
                street: street.to_string(),
                points: points.iter().map(|(x, y)| at(*x, *y)).collect(),
            })
            .collect();
        let (points, maneuvers) = build(&steps);
        let along = cumulative(&points);
        let distance_m = *along.last().unwrap();
        WalkRoute {
            points,
            along,
            distance_m,
            duration_s: distance_m / 1.25,
            maneuvers,
            source: crate::components::navigation::route::RouteSource::Transitous,
        }
    }

    /// North 100 m on Rue A, left, west 100 m on Rue B.
    fn l_route() -> WalkRoute {
        route(&[
            ("Rue A", &[(0.0, 0.0), (0.0, 100.0)]),
            ("Rue B", &[(0.0, 100.0), (-100.0, 100.0)]),
        ])
    }

    #[test]
    fn follows_and_advances() {
        let route = l_route();
        let mut progress = Progress::default();
        let update = progress.update(&route, at(3.0, 40.0), 0.0);
        assert_eq!(update.status, Status::OnRoute);
        assert_eq!(update.maneuver, 1, "the left turn is next");
        assert!((update.to_next_m - 60.0).abs() < 1.0);
        assert!((update.remaining_m - 160.0).abs() < 1.0);
        assert!((update.remaining_s - 128.0).abs() < 1.0);

        let update = progress.update(&route, at(0.0, 95.0), 1.0);
        assert!(update.advanced, "within the margin of the turn");
        assert_eq!(update.maneuver, 2, "arrival is next");
    }

    #[test]
    fn off_route_after_two_far_fixes_then_back() {
        let route = l_route();
        let mut progress = Progress::default();
        progress.update(&route, at(0.0, 20.0), 0.0);
        assert_eq!(
            progress.update(&route, at(60.0, 30.0), 1.0).status,
            Status::OnRoute
        );
        assert_eq!(
            progress.update(&route, at(60.0, 35.0), 2.0).status,
            Status::OffRoute
        );
        assert_eq!(
            progress.update(&route, at(1.0, 40.0), 3.0).status,
            Status::OnRoute
        );
    }

    #[test]
    fn arrives_near_the_end() {
        let route = l_route();
        let mut progress = Progress::default();
        progress.update(&route, at(-50.0, 100.0), 0.0);
        assert_eq!(
            progress.update(&route, at(-90.0, 101.0), 1.0).status,
            Status::Arrived
        );
    }

    #[test]
    fn out_and_back_does_not_jump_ahead() {
        // North 150 m then straight back south along the same line.
        let route = route(&[
            ("Rue A", &[(0.0, 0.0), (0.0, 150.0)]),
            ("Rue A", &[(0.0, 150.0), (1.0, 0.0)]),
        ]);
        let mut progress = Progress::default();
        progress.update(&route, at(0.0, 10.0), 0.0);
        let update = progress.update(&route, at(0.5, 30.0), 1.0);
        // On the way out (about 30 m along), not the way back (about 270 m).
        assert!(update.along_m < 100.0, "{}", update.along_m);
    }
}
