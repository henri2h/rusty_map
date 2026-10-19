//! Flat-earth geometry for walking distances: an equirectangular projection around the points
//! involved, accurate to well under a percent over the few kilometres of a walk. Points are
//! `(lon, lat)` in degrees, as in route geometries.

/// Metres per degree of latitude (and of longitude at the equator).
const METRES_PER_DEGREE: f64 = 111_320.0;

/// `b` relative to `a` in metres: (east, north).
pub fn offset_m(a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let mean_lat = ((a.1 + b.1) / 2.0).to_radians();
    (
        (b.0 - a.0) * METRES_PER_DEGREE * mean_lat.cos(),
        (b.1 - a.1) * METRES_PER_DEGREE,
    )
}

pub fn distance_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (east, north) = offset_m(a, b);
    east.hypot(north)
}

/// Compass bearing from `a` to `b`, degrees clockwise from north in `[0, 360)`.
pub fn bearing(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (east, north) = offset_m(a, b);
    east.atan2(north).to_degrees().rem_euclid(360.0)
}

/// `to - from` as a turn in `(-180, 180]`: positive turns right (clockwise).
pub fn turn_angle(from: f64, to: f64) -> f64 {
    let turn = (to - from).rem_euclid(360.0);
    if turn > 180.0 { turn - 360.0 } else { turn }
}

/// Distance along `points` at each point, from the first.
pub fn cumulative(points: &[(f64, f64)]) -> Vec<f64> {
    let mut along = Vec::with_capacity(points.len());
    let mut total = 0.0;
    for (index, point) in points.iter().enumerate() {
        if index > 0 {
            total += distance_m(points[index - 1], *point);
        }
        along.push(total);
    }
    along
}

/// The point `at` metres along `points` (clamped to its ends), with `along` from
/// [`cumulative`].
pub fn point_at(points: &[(f64, f64)], along: &[f64], at: f64) -> (f64, f64) {
    let Some(last) = points.last() else {
        return (0.0, 0.0);
    };
    if at <= 0.0 {
        return points[0];
    }
    // First point at or past `at`.
    let index = along.partition_point(|distance| *distance < at);
    if index >= points.len() {
        return *last;
    }
    if index == 0 {
        return points[0];
    }
    let (a, b) = (points[index - 1], points[index]);
    let span = along[index] - along[index - 1];
    let t = if span > 0.0 {
        (at - along[index - 1]) / span
    } else {
        0.0
    };
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

/// The closest point to `p` on segment `a`–`b`: its distance in metres, and how far along the
/// segment it is (0 at `a`, 1 at `b`).
pub fn project_on_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> (f64, f64) {
    let (bx, by) = offset_m(a, b);
    let (px, py) = offset_m(a, p);
    let length_sq = bx * bx + by * by;
    let t = if length_sq > 0.0 {
        ((px * bx + py * by) / length_sq).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (dx, dy) = (px - bx * t, py - by * t);
    (dx.hypot(dy), t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearings_and_turns() {
        let origin = (2.35, 48.85);
        assert!((bearing(origin, (2.35, 48.86)) - 0.0).abs() < 0.5);
        assert!((bearing(origin, (2.36, 48.85)) - 90.0).abs() < 0.5);
        assert_eq!(turn_angle(350.0, 10.0), 20.0);
        assert_eq!(turn_angle(10.0, 350.0), -20.0);
        assert_eq!(turn_angle(0.0, 180.0), 180.0);
    }

    #[test]
    fn distances_along_a_line() {
        let points = [(2.35, 48.85), (2.35, 48.851), (2.35, 48.852)];
        let along = cumulative(&points);
        assert!((along[2] - 222.6).abs() < 1.0);
        let mid = point_at(&points, &along, along[2] / 2.0);
        assert!((mid.1 - 48.851).abs() < 1e-6);
        let (distance, t) = project_on_segment((2.3501, 48.8505), points[0], points[1]);
        assert!((t - 0.5).abs() < 0.01);
        assert!((distance - 7.3).abs() < 0.5);
    }
}
