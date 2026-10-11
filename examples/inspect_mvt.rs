//! Debug tool: dumps a `.pbf`/`.mvt` tile's `poi`/`pois` layer feature ids + properties.
//! Usage: `cargo run --example inspect_mvt -- <path-to-tile>`

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: inspect_mvt <tile-file>");
    let data = std::fs::read(path).unwrap();
    let reader = mvt_reader::Reader::new(data).unwrap();
    for (i, name) in reader.get_layer_names().unwrap().iter().enumerate() {
        if name != "poi" && name != "pois" {
            continue;
        }
        println!("=== layer {name} ===");
        let features = reader.get_features(i).unwrap();
        for f in features.iter() {
            println!("id={:?} props={:?}", f.id, f.properties);
        }
    }
}
