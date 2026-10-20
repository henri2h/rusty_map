//! The icon for a kind of place outside the map canvas (search results, category chips): the
//! very sprite the map draws for that POI kind when the style's sheet has one (see
//! `SpriteSheet::icon_png`), else a lucide icon picked from the kind or its OSM key.

use bytes::Bytes;
use freya::{icons::lucide, prelude::*};

use map_component::util::sprites::SpriteSheet;

/// `kind`: the OSM tag value (`restaurant`, `bakery`, `city`); `class`: its key (`amenity`,
/// `shop`, `place`), the fallback when nothing matches `kind` itself.
pub fn poi_icon(
    sprites: Option<&SpriteSheet>,
    kind: &str,
    class: Option<&str>,
    size: f32,
    color: Color,
) -> Element {
    if let Some((name, png)) = sprites.and_then(|sheet| sheet.icon_png(kind, class)) {
        return ImageViewer::new((name, png))
            .width(Size::px(size))
            .height(Size::px(size))
            .into_element();
    }
    SvgViewer::new(lucide_icon(kind, class))
        .width(Size::px(size))
        .height(Size::px(size))
        .color(color)
        .into_element()
}

/// The closest lucide icon to an OSM `kind` (tag value), else to its `class` (tag key).
pub fn lucide_icon(kind: &str, class: Option<&str>) -> Bytes {
    match kind {
        "restaurant" | "fast_food" | "food_court" => lucide::utensils(),
        "cafe" => lucide::coffee(),
        "bakery" | "pastry" => lucide::croissant(),
        "bar" | "pub" | "biergarten" => lucide::beer(),
        "supermarket" | "convenience" | "greengrocer" => lucide::shopping_cart(),
        "pharmacy" | "chemist" => lucide::pill(),
        "hospital" | "clinic" => lucide::cross(),
        "doctors" | "dentist" | "healthcare" | "physiotherapist" | "nurse" => lucide::stethoscope(),
        "hairdresser" | "beauty" | "barber" => lucide::scissors(),
        "estate_agent" | "office" | "insurance" | "lawyer" | "accountant" | "company" => {
            lucide::briefcase()
        }
        "post_box" | "post_office" => lucide::mailbox(),
        "books" | "library" => lucide::book_open(),
        "playground" => lucide::toy_brick(),
        "artwork" | "arts_centre" | "gallery" => lucide::palette(),
        "fuel" => lucide::fuel(),
        "charging_station" => lucide::plug_zap(),
        "parking" => lucide::square_parking(),
        "car_rental" | "car_wash" => lucide::car(),
        "hotel" | "hostel" | "motel" | "guest_house" => lucide::bed(),
        "atm" | "bank" | "bureau_de_change" => lucide::banknote(),
        "place_of_worship" => lucide::church(),
        "museum" | "castle" | "monument" | "memorial" | "attraction" | "town_hall" => {
            lucide::landmark()
        }
        "station" | "halt" | "train_station" => lucide::train_front(),
        "tram_stop" => lucide::tram_front(),
        "bus_stop" | "bus_station" => lucide::bus(),
        "peak" | "volcano" | "mountain_range" => lucide::mountain(),
        "water" | "bay" | "beach" | "river" | "lake" => lucide::waves(),
        "park" | "wood" | "forest" | "garden" | "nature_reserve" => lucide::trees(),
        "house" => lucide::house(),
        "city" | "town" | "village" | "hamlet" | "suburb" | "neighbourhood" | "quarter" => {
            lucide::building_2()
        }
        _ => match class {
            Some("shop") => lucide::store(),
            Some("office") => lucide::briefcase(),
            Some("healthcare") => lucide::stethoscope(),
            Some("highway") => lucide::signpost(),
            Some("building") => lucide::building_2(),
            Some("railway") => lucide::train_front(),
            Some("place" | "boundary") => lucide::building_2(),
            _ => lucide::map_pin(),
        },
    }
}
