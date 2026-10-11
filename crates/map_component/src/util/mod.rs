//! Map-rendering engine/support modules — tile fetch/decode/compile, label placement, sprite
//! icons, the viewport/zoom model, tile caching, and POI feature-id decoding. No UI elements
//! here; `map_component`'s own files (`mod.rs`/`map_input.rs`/`map_paint.rs`/
//! `map_style_loader.rs`) are what actually builds/drives the component tree.

pub mod cache;
pub mod labels;
pub mod poi_lookup;
pub mod renderer;
pub mod sprites;
pub mod tiles;
pub mod viewport;
