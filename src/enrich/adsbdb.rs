//! Route enrichment via adsbdb.com — free, no API key.
//!
//! `GET https://api.adsbdb.com/v0/callsign/{callsign}` →
//! `{ "response": { "flightroute": { "origin": {...}, "destination": {...} } } }`.
//! For an unrecognised callsign the API returns `{ "response": "unknown callsign" }`.

use super::Enricher;
use crate::model::{Airport, FlightInfo};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::Deserialize;

pub struct AdsbdbRoute {
    client: reqwest::Client,
    base_url: String,
}

impl AdsbdbRoute {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("planespotter/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building HTTP client")?;
        Ok(AdsbdbRoute {
            client,
            base_url: "https://api.adsbdb.com/v0".to_string(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct Envelope {
    /// `flightroute` object on success, or a bare string like `"unknown callsign"` otherwise.
    response: ResponseField,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ResponseField {
    Route { flightroute: FlightRoute },
    /// Anything else, e.g. the bare string `"unknown callsign"`.
    Other(#[allow(dead_code)] serde_json::Value),
}

#[derive(Debug, Deserialize)]
struct FlightRoute {
    origin: Option<RawAirport>,
    destination: Option<RawAirport>,
}

#[derive(Debug, Deserialize)]
struct RawAirport {
    icao_code: Option<String>,
    iata_code: Option<String>,
    name: Option<String>,
}

impl From<RawAirport> for Airport {
    fn from(a: RawAirport) -> Self {
        Airport {
            icao: a.icao_code,
            iata: a.iata_code,
            name: a.name,
        }
    }
}

#[async_trait]
impl Enricher for AdsbdbRoute {
    async fn enrich(&self, callsign: &str) -> Result<Option<FlightInfo>> {
        let url = format!("{}/callsign/{}", self.base_url, callsign.trim());
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| format!("requesting {url}"))?;

        // adsbdb returns 404 for unknown callsigns — treat as "no info", not an error.
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = resp
            .error_for_status()
            .with_context(|| format!("error status from {url}"))?;
        let env: Envelope = resp
            .json()
            .await
            .with_context(|| format!("decoding response from {url}"))?;

        match env.response {
            ResponseField::Route { flightroute } => {
                let info = FlightInfo {
                    origin: flightroute.origin.map(Into::into),
                    destination: flightroute.destination.map(Into::into),
                    ..Default::default()
                };
                Ok(Some(info).filter(|i| !i.is_empty()))
            }
            ResponseField::Other(_) => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_route() {
        let json = r#"{ "response": { "flightroute": {
            "callsign": "BAW100",
            "origin": { "icao_code": "EGLL", "iata_code": "LHR", "name": "London Heathrow" },
            "destination": { "icao_code": "KJFK", "iata_code": "JFK", "name": "New York JFK" }
        } } }"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        match env.response {
            ResponseField::Route { flightroute } => {
                let o: Airport = flightroute.origin.unwrap().into();
                assert_eq!(o.icao.as_deref(), Some("EGLL"));
                assert_eq!(flightroute.destination.unwrap().iata_code, Some("JFK".into()));
            }
            _ => panic!("expected route"),
        }
    }

    #[test]
    fn parses_unknown_callsign() {
        let json = r#"{ "response": "unknown callsign" }"#;
        let env: Envelope = serde_json::from_str(json).unwrap();
        assert!(matches!(env.response, ResponseField::Other(_)));
    }
}
