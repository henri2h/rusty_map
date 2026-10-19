//! Fast EV chargers from QualiCharge's map (map.qualicharge.beta.gouv.fr, run by the French
//! energy ministry): `/api/irve/points/` lists every station connected to QualiCharge with a live
//! availability summary (cached ~5 min on their side), `/api/irve/stations/<id>/` gives one
//! station's charge points with their live state and current price (not cached).
//!
//! Undocumented: it is their map's own backend. Requests identify the app ([`USER_AGENT`]), the
//! list is only refetched when it changed (`If-None-Match`), and any failure just leaves the
//! layer as it was.

use serde_json::Value;

use map_component::util::tiles::{FetchError, background_runtime, client};

const API: &str = "https://map.qualicharge.beta.gouv.fr/api/irve";

/// Credited wherever this data is shown.
pub const SOURCE_URL: &str = "https://map.qualicharge.beta.gouv.fr/";

const USER_AGENT: &str = concat!("rusty_map/", env!("CARGO_PKG_VERSION"), " (map@carnot.cc)");

/// Stations below this maximum power (kW) aren't fast chargers and aren't shown.
pub const FAST_KW: f64 = 50.0;

/// One station from the list, with its live availability summary.
#[derive(Clone, PartialEq, Debug)]
pub struct Charger {
    /// `id_station_itinerance`, e.g. `FRELCP12953945`.
    pub id: String,
    pub name: String,
    pub operator: Option<String>,
    pub lat: f64,
    pub lon: f64,
    pub points: u32,
    pub max_kw: f64,
    /// Plug names present at the station (`"CCS"`, `"CHAdeMO"`, `"Type 2"`, …).
    pub plugs: Vec<&'static str>,
    pub access: Option<String>,
    pub card_payment: bool,
    pub price: Option<String>,
    pub availability: Availability,
}

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Availability {
    /// Charge points reporting a live status at all.
    pub reporting: u32,
    pub in_service: u32,
    pub free: u32,
    pub occupied: u32,
    /// Latest status, Unix milliseconds.
    pub latest_ms: Option<i64>,
}

impl Availability {
    pub fn state(&self) -> StationState {
        if self.reporting == 0 {
            StationState::Unknown
        } else if self.free > 0 {
            StationState::Free
        } else if self.in_service > 0 && self.occupied > 0 {
            StationState::Busy
        } else if self.in_service == 0 {
            StationState::OutOfService
        } else {
            StationState::Unknown
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StationState {
    /// At least one charge point free.
    Free,
    /// In service, every reporting point occupied.
    Busy,
    OutOfService,
    Unknown,
}

/// The list (fast chargers only) and its ETag; `Ok(None)` when unchanged since `etag`.
pub async fn fetch_chargers(
    etag: Option<String>,
) -> Result<Option<(Vec<Charger>, Option<String>)>, FetchError> {
    on_background(async move {
        let mut request = client()
            .get(format!("{API}/points/"))
            .header(reqwest::header::USER_AGENT, USER_AGENT);
        if let Some(etag) = &etag {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }
        let response = request.send().await.map_err(FetchError::Network)?;
        if response.status() == reqwest::StatusCode::NOT_MODIFIED {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(FetchError::Http(response.status().as_u16()));
        }
        let etag = response
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let text = response.text().await.map_err(FetchError::Network)?;
        let json: Value = serde_json::from_str(&text).map_err(|_| FetchError::InvalidData)?;
        let chargers = json["stations"]
            .as_array()
            .ok_or(FetchError::InvalidData)?
            .iter()
            .filter_map(to_charger)
            .filter(|charger| charger.max_kw >= FAST_KW)
            .collect();
        Ok(Some((chargers, etag)))
    })
    .await
}

fn to_charger(json: &Value) -> Option<Charger> {
    let summary = &json["summary"];
    let dynamic = &json["dynamic_summary"];
    let count = |value: &Value| value.as_u64().unwrap_or(0) as u32;
    Some(Charger {
        id: json["id_station_itinerance"].as_str()?.to_string(),
        name: text(&json["nom_station"]).unwrap_or_else(|| "Charging station".to_string()),
        operator: text(&json["nom_operateur"]).or_else(|| text(&json["nom_amenageur"])),
        lat: json["lat"].as_f64()?,
        lon: json["lng"].as_f64()?,
        points: count(&json["pdc_count"]),
        max_kw: summary["max_power"].as_f64().unwrap_or(0.0),
        plugs: PLUGS
            .iter()
            .filter(|(key, _)| summary[format!("has_{key}")].as_bool() == Some(true))
            .map(|(_, name)| *name)
            .collect(),
        access: text(&json["condition_acces"]),
        card_payment: json["paiement_cb"].as_bool() == Some(true),
        price: text(&summary["pricing_headline"]),
        availability: Availability {
            reporting: count(&dynamic["pdcs_with_dynamic_count"]),
            in_service: count(&dynamic["en_service_count"]),
            free: count(&dynamic["available_count"]),
            occupied: count(&dynamic["occupied_count"]),
            latest_ms: dynamic["latest_status_timestamp"].as_i64(),
        },
    })
}

/// IRVE schema plug flags (`prise_type_*`) and their usual names.
const PLUGS: [(&str, &str); 5] = [
    ("prise_type_combo_ccs", "CCS"),
    ("prise_type_chademo", "CHAdeMO"),
    ("prise_type_2", "Type 2"),
    ("prise_type_ef", "Domestic (E/F)"),
    ("prise_type_autre", "Other"),
];

fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// One station's details and charge points, live.
#[derive(Clone, PartialEq, Debug)]
pub struct StationDetail {
    pub address: Option<String>,
    pub hours: Option<String>,
    pub phone: Option<String>,
    pub points: Vec<ChargePoint>,
}

#[derive(Clone, PartialEq, Debug)]
pub struct ChargePoint {
    pub id: String,
    pub kw: Option<f64>,
    pub plugs: Vec<&'static str>,
    pub state: PointState,
    /// When the state was reported (`horodatage`), as given.
    pub reported_at: Option<String>,
    /// Current price, e.g. `"0.64 €/kWh"` (VAT included).
    pub price: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PointState {
    Free,
    Occupied,
    Reserved,
    OutOfService,
    Unknown,
}

/// `Ok(None)`: QualiCharge doesn't know this station.
pub async fn fetch_station(id: String) -> Result<Option<StationDetail>, FetchError> {
    on_background(async move {
        let response = client()
            .get(format!("{API}/stations/{id}/"))
            .header(reqwest::header::USER_AGENT, USER_AGENT)
            .send()
            .await
            .map_err(FetchError::Network)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(FetchError::Http(response.status().as_u16()));
        }
        let text = response.text().await.map_err(FetchError::Network)?;
        let json: Value = serde_json::from_str(&text).map_err(|_| FetchError::InvalidData)?;
        Ok(Some(to_station(&json)))
    })
    .await
}

fn to_station(json: &Value) -> StationDetail {
    let points = json["pdcs"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|pdc| {
            let dynamic = &pdc["dynamic"];
            ChargePoint {
                id: text(&pdc["id_pdc_itinerance"]).unwrap_or_default(),
                kw: pdc["puissance_nominale"].as_f64(),
                plugs: PLUGS
                    .iter()
                    .filter(|(key, _)| pdc[*key].as_bool() == Some(true))
                    .map(|(_, name)| *name)
                    .collect(),
                state: point_state(dynamic),
                reported_at: text(&dynamic["horodatage"]),
                price: pdc["applicable_tariff"]["raw"]
                    .as_str()
                    .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                    .and_then(|tariff| tariff_text(&tariff)),
            }
        })
        .collect();
    StationDetail {
        address: text(&json["adresse_station"]),
        hours: text(&json["horaires"]),
        phone: text(&json["telephone_operateur"])
            .map(|phone| phone.trim_start_matches("tel:").replace('-', " ")),
        points,
    }
}

fn point_state(dynamic: &Value) -> PointState {
    match (
        dynamic["etat_pdc"].as_str(),
        dynamic["occupation_pdc"].as_str(),
    ) {
        (Some("hors_service"), _) => PointState::OutOfService,
        (_, Some("libre")) => PointState::Free,
        (_, Some("occupe")) => PointState::Occupied,
        (_, Some("reserve")) => PointState::Reserved,
        _ => PointState::Unknown,
    }
}

/// An OCPI tariff (what QualiCharge stores) as one line: its energy, time and session prices,
/// VAT included. `None` when it has no price component this understands.
fn tariff_text(tariff: &Value) -> Option<String> {
    let currency = match tariff["currency"].as_str() {
        Some("EUR") | None => "€".to_string(),
        Some(other) => format!(" {other}"),
    };
    let mut parts = Vec::new();
    let components = tariff["elements"]
        .as_array()
        .into_iter()
        .flatten()
        .take(1)
        .flat_map(|element| element["price_components"].as_array().into_iter().flatten());
    for component in components {
        let Some(price) = component["price"].as_f64() else {
            continue;
        };
        let with_vat = price * (1.0 + component["vat"].as_f64().unwrap_or(0.0) / 100.0);
        match component["type"].as_str() {
            Some("ENERGY") => parts.push(format!("{with_vat:.2}{currency}/kWh")),
            // OCPI prices time per hour.
            Some("TIME") => parts.push(format!("{:.2}{currency}/min", with_vat / 60.0)),
            Some("FLAT") if with_vat > 0.0 => {
                parts.push(format!("{with_vat:.2}{currency} per session"))
            }
            _ => {}
        }
    }
    (!parts.is_empty()).then(|| parts.join(" + "))
}

/// Runs `future` on the background runtime (reqwest needs Tokio; freya's executor isn't).
async fn on_background<T: Send + 'static>(
    future: impl std::future::Future<Output = Result<T, FetchError>> + Send + 'static,
) -> Result<T, FetchError> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    background_runtime().spawn(async move {
        let _ = tx.send(future.await);
    });
    rx.await.unwrap_or(Err(FetchError::TaskFailed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_list_entries() {
        let json: Value = serde_json::from_str(
            r#"{"id_station_itinerance":"FRDREP270644195","lat":47.37,"lng":-1.93,
                "nom_station":"FRANCE LITERIE","nom_operateur":"DREAM ENERGY","pdc_count":6,
                "paiement_cb":true,"summary":{"max_power":320,"has_prise_type_combo_ccs":true,
                "has_prise_type_2":false,"pricing_headline":null},
                "dynamic_summary":{"pdcs_with_dynamic_count":6,"en_service_count":6,
                "available_count":2,"occupied_count":4,"latest_status_timestamp":1792430649776}}"#,
        )
        .unwrap();
        let charger = to_charger(&json).unwrap();
        assert_eq!(charger.plugs, vec!["CCS"]);
        assert_eq!(charger.max_kw, 320.0);
        assert_eq!(charger.availability.state(), StationState::Free);
        assert!(charger.card_payment);
    }

    #[test]
    fn station_states() {
        let busy = Availability {
            reporting: 4,
            in_service: 4,
            free: 0,
            occupied: 4,
            latest_ms: None,
        };
        assert_eq!(busy.state(), StationState::Busy);
        let down = Availability {
            reporting: 2,
            ..Default::default()
        };
        assert_eq!(down.state(), StationState::OutOfService);
        assert_eq!(Availability::default().state(), StationState::Unknown);
    }

    #[test]
    fn prices_ocpi_tariffs() {
        let tariff: Value = serde_json::from_str(
            r#"{"currency":"EUR","elements":[{"price_components":[
                {"type":"ENERGY","price":0.53333,"vat":20},{"type":"TIME","price":6,"vat":20}]}]}"#,
        )
        .unwrap();
        assert_eq!(tariff_text(&tariff).unwrap(), "0.64€/kWh + 0.12€/min");
    }

    #[test]
    fn reads_point_states() {
        let state = |json: &str| point_state(&serde_json::from_str(json).unwrap());
        assert_eq!(
            state(r#"{"etat_pdc":"en_service","occupation_pdc":"libre"}"#),
            PointState::Free
        );
        assert_eq!(
            state(r#"{"etat_pdc":"hors_service","occupation_pdc":"libre"}"#),
            PointState::OutOfService
        );
        assert_eq!(state("null"), PointState::Unknown);
    }
}
