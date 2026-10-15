//! Decodes a tapped POI's own `osm_type`/`osm_id` straight out of the MVT feature id the tile
//! *already downloaded for rendering* carries — no extra network round trip. Current
//! Planetiler/OpenMapTiles-schema sources (including the live `openmaptiles.data.gouv.fr` style
//! this app actually loads — see `map_style_loader::STYLE_URL`) pack the OSM element id into a
//! feature's protocol-level id via Planetiler's own public `vectorTileFeatureId` scheme
//! (`id = osm_id * 10 + type_code`, where `type_code` is 1 for a node, 2 for a way, 3 for a
//! relation) — confirmed by decoding a real tile and cross-checking the recovered id against the
//! live OSM API. `shortbread_v1` (the offline fallback style's source) sets no feature id at all,
//! so [`decode_feature_id`] returning `None` there just falls back to reverse-geocoding — see
//! `map_input`'s POI-tap handler.

/// Undoes Planetiler's `vectorTileFeatureId(10, osm_id, type)` packing (see this module's doc
/// comment): the low decimal digit is the type code, the rest is the OSM id. Returns the
/// single-letter form (`N`/`W`/`R`) Nominatim's `/details` `osmtype` param expects — the same
/// shape a Photon search result's `osm_type` is already in (see `SearchResult::osm_type`'s doc
/// comment). `None` when `id` doesn't decode to a known type digit — either the source doesn't
/// use this packing at all, or the feature isn't an OSM-backed node/way/relation.
pub fn decode_feature_id(id: u64) -> Option<(String, u64)> {
    let osm_type = match id % 10 {
        1 => "N",
        2 => "W",
        3 => "R",
        _ => return None,
    };
    Some((osm_type.to_string(), id / 10))
}

/// The OSM way id behind a line feature (`transportation` layer). The data.gouv.fr tiles pack
/// ways as `osm_id * 10` plus type digit 2 — or 0 for lines Planetiler merged from several ways,
/// whose id is still one of them (checked against the live OSM API for metro, RER and tram
/// segments in Paris).
pub fn decode_line_way_id(id: u64) -> Option<u64> {
    matches!(id % 10, 0 | 2)
        .then_some(id / 10)
        .filter(|&way| way > 0)
}
