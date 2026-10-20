//! Where the app's own tiles (`tools/tiles`, built from the France extract) have data: mainland
//! France and Corsica. Elsewhere they hold nothing but sea, so the map draws those tiles from
//! the style's own whole-planet source instead (see `StyleSpec::tile_url_for`).

use super::tiles::TileKey;

/// Below this zoom one tile spans several countries: the planet source draws them all, where
/// France's own tiles would leave its neighbours blank.
pub const MIN_OWN_ZOOM: u32 = 6;

/// Mainland France, as `(lon, lat)`, generously outside its borders and coast (≈10–20 km): a
/// tile only needs to touch it to be drawn from the France tiles, which hold every feature of
/// the extract, so a little too much is harmless and too little would cut border towns off.
const MAINLAND: &[(f64, f64)] = &[
    (1.45, 51.15),
    (2.55, 51.20),
    (3.35, 50.90),
    (4.20, 50.45),
    (4.85, 50.25),
    (5.90, 49.65),
    (6.55, 49.55),
    (8.35, 49.05),
    (7.70, 47.50),
    (7.05, 47.50),
    (6.10, 46.65),
    (6.05, 46.15),
    (7.15, 45.95),
    (7.15, 45.05),
    (7.80, 43.75),
    (6.70, 42.85),
    (4.50, 43.10),
    (3.25, 42.30),
    (1.45, 42.35),
    (-0.80, 42.70),
    (-1.95, 43.25),
    (-1.70, 44.60),
    (-2.50, 47.00),
    (-4.95, 47.70),
    (-5.25, 48.50),
    (-3.50, 49.00),
    (-1.95, 48.85),
    (-1.95, 49.80),
    (0.20, 49.80),
    (1.45, 50.25),
];

const CORSICA: &[(f64, f64)] = &[(8.40, 43.10), (9.70, 43.10), (9.70, 41.25), (8.40, 41.25)];

/// `true` when tile `key` touches mainland France or Corsica at [`MIN_OWN_ZOOM`] or deeper.
pub fn in_own_coverage((z, x, y): TileKey) -> bool {
    if z < MIN_OWN_ZOOM {
        return false;
    }
    let ((min_lon, min_lat), (max_lon, max_lat)) = tile_bounds(z, x, y);
    [MAINLAND, CORSICA].into_iter().any(|polygon| {
        // A polygon corner inside the tile, or a sample of the tile inside the polygon. Tiles
        // at `MIN_OWN_ZOOM` and deeper are small next to these polygons' edges, so a 3×3 grid
        // finds every overlap that matters.
        polygon.iter().any(|&(lon, lat)| {
            (min_lon..=max_lon).contains(&lon) && (min_lat..=max_lat).contains(&lat)
        }) || (0..3).any(|i| {
            (0..3).any(|j| {
                let lon = min_lon + (max_lon - min_lon) * i as f64 / 2.0;
                let lat = min_lat + (max_lat - min_lat) * j as f64 / 2.0;
                contains(polygon, lon, lat)
            })
        })
    })
}

/// `((min_lon, min_lat), (max_lon, max_lat))` of XYZ tile `x`/`y` at zoom `z`.
fn tile_bounds(z: u32, x: u32, y: u32) -> ((f64, f64), (f64, f64)) {
    let n = 2f64.powi(z as i32);
    let lon = |x: f64| x / n * 360.0 - 180.0;
    let lat = |y: f64| {
        (std::f64::consts::PI * (1.0 - 2.0 * y / n))
            .sinh()
            .atan()
            .to_degrees()
    };
    let (x, y) = (f64::from(x), f64::from(y));
    ((lon(x), lat(y + 1.0)), (lon(x + 1.0), lat(y)))
}

/// Even-odd point-in-polygon test.
fn contains(polygon: &[(f64, f64)], lon: f64, lat: f64) -> bool {
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let ((xi, yi), (xj, yj)) = (polygon[i], polygon[j]);
        if (yi > lat) != (yj > lat) && lon < (xj - xi) * (lat - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The z14 tile holding `(lon, lat)`.
    fn tile(lon: f64, lat: f64) -> TileKey {
        let n = f64::from(1u32 << 14);
        let x = ((lon + 180.0) / 360.0 * n) as u32;
        let lat = lat.to_radians();
        let y = ((1.0 - lat.tan().asinh() / std::f64::consts::PI) / 2.0 * n) as u32;
        (14, x, y)
    }

    #[test]
    fn france_is_covered_and_its_neighbours_are_not() {
        for (name, lon, lat) in [
            ("Paris", 2.35, 48.86),
            ("Aix-en-Provence", 5.45, 43.53),
            ("Brest", -4.49, 48.39),
            ("Strasbourg", 7.75, 48.58),
            ("Ajaccio", 8.74, 41.93),
            ("Menton", 7.50, 43.78),
            ("Hendaye", -1.78, 43.36),
        ] {
            assert!(in_own_coverage(tile(lon, lat)), "{name}");
        }
        for (name, lon, lat) in [
            ("Madrid", -3.70, 40.42),
            ("Barcelona", 2.17, 41.39),
            ("Turin", 7.69, 45.07),
            ("Brussels", 4.35, 50.85),
            ("Zürich", 8.54, 47.37),
            ("London", -0.13, 51.51),
            ("Boise", -116.2, 43.6),
        ] {
            assert!(!in_own_coverage(tile(lon, lat)), "{name}");
        }
        assert!(!in_own_coverage((5, 16, 11)), "low zoom");
    }
}
