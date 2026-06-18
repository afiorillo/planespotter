//! Network position source for the airplanes.live / adsb.fi REST API.
//!
//! Both expose `GET {base}/point/{lat}/{lon}/{radius}` returning `{ "ac": [ ... ] }` in the
//! readsb/tar1090 schema, with no API key required (~1 req/s).

use super::PositionSource;
use crate::model::Aircraft;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

/// API maximum search radius in nautical miles.
const MAX_RADIUS_NM: f64 = 250.0;

pub struct NetworkSource {
    client: reqwest::Client,
    base_url: String,
}

impl NetworkSource {
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("planespotter/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building HTTP client")?;
        Ok(NetworkSource {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        })
    }
}

/// Raw API response. Fields we don't use are ignored.
#[derive(Debug, Deserialize)]
struct PointResponse {
    #[serde(default)]
    ac: Vec<RawAircraft>,
}

/// A single aircraft record. `flight`, `alt_baro`, etc. are frequently absent.
#[derive(Debug, Deserialize)]
struct RawAircraft {
    hex: Option<String>,
    flight: Option<String>,
    lat: Option<f64>,
    lon: Option<f64>,
    /// Altitude is a number in feet, or the string `"ground"`.
    #[serde(default)]
    alt_baro: serde_json::Value,
    gs: Option<f64>,
    track: Option<f64>,
    t: Option<String>,
    r: Option<String>,
    squawk: Option<String>,
}

impl RawAircraft {
    /// Convert to our model, returning `None` for records without identity or position.
    fn into_aircraft(self) -> Option<Aircraft> {
        let hex = self.hex?;
        let (lat, lon) = (self.lat?, self.lon?);
        Some(Aircraft {
            hex: hex.to_lowercase(),
            callsign: self.flight.map(|f| f.trim().to_string()).filter(|f| !f.is_empty()),
            lat,
            lon,
            altitude_ft: self.alt_baro.as_f64(),
            ground_speed_kt: self.gs,
            track_deg: self.track,
            type_code: self.t,
            registration: self.r,
            squawk: self.squawk,
        })
    }
}

#[async_trait]
impl PositionSource for NetworkSource {
    async fn poll(&self, lat: f64, lon: f64, radius_nm: f64) -> Result<Vec<Aircraft>> {
        let radius = radius_nm.clamp(1.0, MAX_RADIUS_NM).ceil() as u64;
        let url = format!("{}/point/{lat}/{lon}/{radius}", self.base_url);
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("requesting {url}"))?
            .error_for_status()
            .with_context(|| format!("error status from {url}"))?;
        let body: PointResponse = resp
            .json()
            .await
            .with_context(|| format!("decoding response from {url}"))?;
        Ok(body.ac.into_iter().filter_map(RawAircraft::into_aircraft).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skips_records_without_position() {
        let json = r#"{ "ac": [
            { "hex": "ABC123", "flight": "BAW123 ", "lat": 51.4, "lon": -0.4, "alt_baro": 3000, "t": "A320", "r": "G-EUYB" },
            { "hex": "DEF456", "flight": "NOPOS" },
            { "flight": "NOHEX", "lat": 1.0, "lon": 2.0 }
        ] }"#;
        let resp: PointResponse = serde_json::from_str(json).unwrap();
        let aircraft: Vec<Aircraft> = resp.ac.into_iter().filter_map(RawAircraft::into_aircraft).collect();
        assert_eq!(aircraft.len(), 1);
        let a = &aircraft[0];
        assert_eq!(a.hex, "abc123");
        assert_eq!(a.callsign.as_deref(), Some("BAW123")); // trimmed
        assert_eq!(a.altitude_ft, Some(3000.0));
        assert_eq!(a.type_code.as_deref(), Some("A320"));
    }

    #[test]
    fn handles_ground_altitude_string() {
        let json = r#"{ "ac": [ { "hex": "abc", "lat": 1.0, "lon": 2.0, "alt_baro": "ground" } ] }"#;
        let resp: PointResponse = serde_json::from_str(json).unwrap();
        let a = resp.ac.into_iter().filter_map(RawAircraft::into_aircraft).next().unwrap();
        assert_eq!(a.altitude_ft, None); // "ground" -> not a number
    }
}
