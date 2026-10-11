use std::sync::OnceLock;

use freya::engine::prelude::{Font, FontEdging, FontHinting, FontMgr, FontStyle, Typeface};

/// Text rendered crisp: full hinting snaps stems to the pixel grid, subpixel positioning off so
/// glyphs land on whole pixels (combined with the integer-snapped draw origin for unrotated
/// labels below) — fractional positions were rendering every glyph through anti-aliasing smear.
pub(super) fn make_font(typeface: Typeface, size: f32) -> Font {
    let mut font = Font::from_typeface(typeface, size);
    font.set_edging(FontEdging::AntiAlias);
    font.set_hinting(FontHinting::Full);
    font.set_subpixel(false);
    font
}

/// Drops characters the font has no glyph for (emoji, unsupported scripts, …) instead of letting
/// Skia draw its missing-glyph tofu box — "Noto Sans" alone carries no emoji coverage, so any
/// emoji in an OSM `name` field would otherwise render as a stray rectangle.
pub(super) fn strip_unsupported_glyphs<'a>(
    text: &'a str,
    font: &Font,
) -> std::borrow::Cow<'a, str> {
    let has_glyph = |c: char| font.unichar_to_glyph(c as i32) != 0;
    if text.chars().all(has_glyph) {
        return std::borrow::Cow::Borrowed(text);
    }
    std::borrow::Cow::Owned(text.chars().filter(|&c| has_glyph(c)).collect())
}

pub(super) fn typefaces() -> &'static (Typeface, Typeface) {
    static TYPEFACES: OnceLock<(Typeface, Typeface)> = OnceLock::new();
    TYPEFACES.get_or_init(|| {
        let mgr = FontMgr::new();
        let pick = |style: FontStyle| {
            mgr.match_family_style("Noto Sans", style)
                .or_else(|| mgr.legacy_make_typeface(None, style))
                .expect("system font manager always yields some typeface")
        };
        (pick(FontStyle::normal()), pick(FontStyle::bold()))
    })
}
