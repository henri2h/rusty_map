use std::time;

use freya::engine::prelude::{Color, TextBlob};
use rustc_hash::FxHashMap;

/// Cross-frame label fade state: newly placed labels fade in, labels that lose their spot fade
/// out in place (as "ghosts", anchored to world coordinates so they track pan/zoom). Owned by
/// the component, mutated once per frame by [`super::draw_labels`].
#[derive(Default)]
pub struct LabelAnimator {
    /// identity → current opacity + last placement snapshot (for drawing the fade-out ghost).
    pub(super) entries: FxHashMap<u64, AnimEntry>,
    pub(super) last_frame: Option<time::Instant>,
    /// Per-label text measurements, see [`TextMetrics`].
    pub(super) metrics: FxHashMap<(u64, bool), TextMetrics>,
}

/// A label's text shaped once at a reference size: glyph-stripped text plus its bounds per pixel
/// of font size. Lets the placement loop size, fit-check and collide every candidate without
/// shaping it — only labels that actually get drawn build a `TextBlob`. Shaping every candidate
/// every frame (thousands) measured ~40% of main-thread time. Scaled linearly by font size, which
/// hinting makes approximate to within a pixel — fine for fit/collision; the drawn blob is exact.
pub(super) struct TextMetrics {
    /// `None` when every glyph is supported (the label's own text is drawn as is).
    pub(super) stripped: Option<String>,
    pub(super) width_per_px: f32,
    pub(super) height_per_px: f32,
    /// The last blob drawn for this label and the font size (bits) it was shaped at — reused
    /// while the size holds (any pan, or zoom within a size step) instead of reshaping every
    /// frame.
    pub(super) blob: Option<(u32, TextBlob)>,
}

pub(super) struct AnimEntry {
    pub(super) opacity: f32,
    pub(super) placed_this_frame: bool,
    pub(super) ghost: Ghost,
}

/// Everything needed to redraw a label/icon after it lost placement, while it fades out.
pub(super) struct Ghost {
    pub(super) world_x: f64,
    pub(super) world_y: f64,
    pub(super) angle: f32,
    pub(super) kind: GhostKind,
}

pub(super) enum GhostKind {
    Text {
        text: String,
        size: f32,
        bold: bool,
        text_color: Color,
        halo_color: Color,
        halo_width: f32,
    },
    Icon {
        sprite: String,
    },
}

impl LabelAnimator {
    /// True while any fade is mid-flight — the caller should request another frame.
    pub fn is_animating(&self) -> bool {
        self.entries
            .values()
            .any(|e| e.opacity < 1.0 || !e.placed_this_frame)
    }
}

/// Stable identity for an icon across frames: sprite name + quantized world position (icons
/// have no style layer or unique text to key on).
pub(super) fn icon_identity(sprite: &str, world_x: f64, world_y: f64) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    sprite.hash(&mut hasher);
    ((world_x * 1e7).round() as i64).hash(&mut hasher);
    ((world_y * 1e7).round() as i64).hash(&mut hasher);
    hasher.finish()
}

/// FxHash-style string hash (cheap, non-cryptographic) for the dedup key.
pub(super) fn fxhash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = rustc_hash::FxHasher::default();
    s.hash(&mut hasher);
    hasher.finish()
}
