//! Standalone (no dependency on the `rusty_map` lib target, which doesn't exist) re-check of
//! `poi_lookup`'s matching logic against the real McDonald's-on-Champs-Élysées case from the bug
//! report, run by hand against OpenFreeMap — not part of the app or its test suite.

use mvt_reader::feature::Value;

const LOOKUP_ZOOM: u32 = 14;

fn lonlat_to_tile(lon: f64, lat: f64, zoom: u32) -> (u32, u32) {
    let n = 2f64.powi(zoom as i32);
    let x = ((lon + 180.0) / 360.0 * n).floor().clamp(0.0, n - 1.0) as u32;
    let lat_rad = lat.to_radians();
    let y = ((1.0 - (lat_rad.tan() + 1.0 / lat_rad.cos()).ln() / std::f64::consts::PI) / 2.0 * n)
        .floor()
        .clamp(0.0, n - 1.0) as u32;
    (x, y)
}

fn tile_local_to_lonlat(
    tile_x: u32,
    tile_y: u32,
    zoom: u32,
    local_x: f64,
    local_y: f64,
) -> (f64, f64) {
    let n = 2f64.powi(zoom as i32);
    let world_x = (tile_x as f64 + local_x) / n;
    let world_y = (tile_y as f64 + local_y) / n;
    let lon = world_x * 360.0 - 180.0;
    let lat = (std::f64::consts::PI * (1.0 - 2.0 * world_y))
        .sinh()
        .atan()
        .to_degrees();
    (lon, lat)
}

fn value_to_str(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn decode_feature_id(id: u64) -> Option<(&'static str, u64)> {
    let osm_type = match id % 10 {
        1 => "node",
        2 => "way",
        3 => "relation",
        _ => return None,
    };
    Some((osm_type, id / 10))
}

#[tokio::main]
async fn main() {
    let lon = 2.2924_f64;
    let lat = 48.8732_f64;
    let name = "McDonald's";

    let http = reqwest::Client::new();

    let tilejson_text = http
        .get("https://tiles.openfreemap.org/planet")
        .header(reqwest::header::USER_AGENT, "rusty_map-debug/1.0")
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let tilejson: serde_json::Value = serde_json::from_str(&tilejson_text).unwrap();
    let template = tilejson["tiles"][0].as_str().unwrap();
    println!("tile template = {template}");

    let (tile_x, tile_y) = lonlat_to_tile(lon, lat, LOOKUP_ZOOM);
    println!("tile = {tile_x}/{tile_y}/{LOOKUP_ZOOM}");

    let url = template
        .replace("{z}", &LOOKUP_ZOOM.to_string())
        .replace("{x}", &tile_x.to_string())
        .replace("{y}", &tile_y.to_string());

    let data = http
        .get(&url)
        .header(reqwest::header::USER_AGENT, "rusty_map-debug/1.0")
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap()
        .to_vec();

    let reader = mvt_reader::Reader::new(data).unwrap();
    let layers = reader.get_layer_metadata().unwrap();
    let (layer_index, layer) = layers
        .iter()
        .enumerate()
        .find(|(_, l)| l.name == "poi")
        .unwrap();
    let extent = layer.extent.max(1) as f64;
    let features = reader.get_features(layer_index).unwrap();

    let best = features
        .iter()
        .filter_map(|f| {
            let (osm_type, osm_id) = decode_feature_id(f.id?)?;
            let props = f.properties.as_ref()?;
            let fname = value_to_str(props.get("name")?)?;
            if fname.to_lowercase().contains("mcdonald") {
                println!("raw candidate: {fname} type={osm_type} id={osm_id}");
            }
            if !fname.eq_ignore_ascii_case(name) {
                return None;
            }
            let (px, py) = match &f.geometry {
                geo_types::Geometry::Point(p) => (p.x(), p.y()),
                geo_types::Geometry::MultiPoint(mp) => {
                    let p = mp.0.first()?;
                    (p.x(), p.y())
                }
                _ => return None,
            };
            let (flon, flat) = tile_local_to_lonlat(
                tile_x,
                tile_y,
                LOOKUP_ZOOM,
                px as f64 / extent,
                py as f64 / extent,
            );
            let dist_sq = (flon - lon).powi(2) + (flat - lat).powi(2);
            println!(
                "candidate type={osm_type} id={osm_id} at ({flon:.6},{flat:.6}) dist_sq={dist_sq:.10}"
            );
            Some((dist_sq, osm_type, osm_id))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0));

    println!("BEST: {best:?}");
}
