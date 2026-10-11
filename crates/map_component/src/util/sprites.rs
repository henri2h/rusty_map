//! POI icon sprites: fetches the style's sprite sheet (disk-cached like tiles/styles), and
//! converts its SDF-encoded glyphs into a crisp, pre-tinted RGBA atlas once at load — the paint
//! loop then just crops and blits per icon.

use bytes::Bytes;
use freya::engine::prelude::{ColorType, Data, Image, ImageInfo, Rect, raster_from_data};
use rustc_hash::FxHashMap;

use super::tiles::{FetchError, fetch_bytes_cached, fetch_text_cached};

/// Icon tint applied at load time (the sprites are SDF alpha masks with no color of their own).
const TINT: (u8, u8, u8) = (66, 62, 82);

pub struct SpriteSheet {
    pub image: Image,
    /// sprite name (e.g. `icon-bakery`) → source rect in `image`.
    pub index: FxHashMap<String, Rect>,
}

impl SpriteSheet {
    pub fn get(&self, name: &str) -> Option<Rect> {
        self.index.get(name).copied()
    }

    /// Resolves a raw POI kind (e.g. `bakery`) to whatever sprite name this sheet actually uses
    /// — different styles' sprite sheets name icons differently: shortbread's basics sheet uses
    /// `icon-bakery`, OpenMapTiles/osm-bright's uses `bakery_11` (Maki naming, size-suffixed).
    /// Returns the matched sheet name (for stable identity/redraw) plus its source rect.
    pub fn resolve(&self, kind: &str) -> Option<(&str, Rect)> {
        [
            format!("icon-{kind}"),
            format!("{kind}_11"),
            format!("{kind}-11"),
            kind.to_string(),
        ]
        .into_iter()
        .find_map(|name| self.index.get_key_value(&name))
        .map(|(name, rect)| (name.as_str(), *rect))
    }
}

/// Fetches and builds the sprite sheet for `base_url` (the style JSON's `sprite` field;
/// `.json`/`.png` suffixes select index and atlas). Runs its decode/convert work inline — call
/// from a background task, not the UI thread.
pub async fn load_sprites(base_url: &str) -> Result<SpriteSheet, FetchError> {
    let index_json = fetch_text_cached(format!("{base_url}.json")).await?;
    let png = fetch_bytes_cached(format!("{base_url}.png")).await?;

    let index = parse_index(&index_json).ok_or(FetchError::InvalidData)?;
    let image = build_atlas(&png).ok_or(FetchError::InvalidData)?;

    Ok(SpriteSheet { image, index })
}

fn parse_index(json: &str) -> Option<FxHashMap<String, Rect>> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let map = v.as_object()?;

    let mut index = FxHashMap::default();
    for (name, entry) in map {
        let x = entry["x"].as_f64()? as f32;
        let y = entry["y"].as_f64()? as f32;
        let w = entry["width"].as_f64()? as f32;
        let h = entry["height"].as_f64()? as f32;
        index.insert(name.clone(), Rect::from_xywh(x, y, w, h));
    }
    Some(index)
}

/// Decodes the sprite PNG and converts its SDF alpha channel into a normal crisp alpha mask,
/// tinted [`TINT`], premultiplied. SDF encodes signed distance in alpha (0.5 = glyph edge);
/// blitting it raw shows gray fuzzy squares.
fn build_atlas(png: &Bytes) -> Option<Image> {
    let encoded = Image::from_encoded(Data::new_copy(png))?;
    let (width, height) = (encoded.width(), encoded.height());

    // Rasterize into a known-layout surface so pixels can be read back predictably.
    let mut surface = freya::engine::prelude::raster_n32_premul((width, height))?;
    surface.canvas().draw_image(&encoded, (0, 0), None);
    let snapshot = surface.image_snapshot();
    let pixmap = snapshot.peek_pixels()?;
    let src = pixmap.bytes()?;

    let (tint_r, tint_g, tint_b) = TINT;
    // The output uses the same native n32 layout as the surface we read from; only the channel
    // *order* differs per platform (BGRA vs RGBA), alpha is last in both.
    let n32_is_bgra = ColorType::N32 == ColorType::BGRA8888;
    let mut out = vec![0u8; (width * height * 4) as usize];
    for (dst, px) in out.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        let sdf = px[3] as f32 / 255.0;
        // Smoothstep across the SDF edge (0.5) — crisp silhouette with ~1px anti-aliasing.
        let t = ((sdf - 0.47) / 0.09).clamp(0.0, 1.0);
        let coverage = t * t * (3.0 - 2.0 * t);
        let alpha = (coverage * 255.0).round() as u8;
        let premul = |c: u8| ((c as f32) * coverage).round() as u8;
        if n32_is_bgra {
            dst[0] = premul(tint_b);
            dst[1] = premul(tint_g);
            dst[2] = premul(tint_r);
        } else {
            dst[0] = premul(tint_r);
            dst[1] = premul(tint_g);
            dst[2] = premul(tint_b);
        }
        dst[3] = alpha;
    }

    let info = ImageInfo::new_n32_premul((width, height), None);
    raster_from_data(&info, Data::new_copy(&out), (width * 4) as usize)
}
