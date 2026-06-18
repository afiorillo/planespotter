//! Schedule + delay enrichment via AeroDataBox (RapidAPI / api.market). Requires an API key.
//!
//! `GET https://{host}/flights/callsign/{callsign}` with `x-rapidapi-key` / `x-rapidapi-host`
//! headers returns an array of flight movements. Each has `departure` and `arrival` objects with
//! an `airport` and several time objects (`scheduledTime`, `revisedTime`, `predictedTime`,
//! `runwayTime`, `actualTime`), each carrying a `utc` timestamp string like `"2024-01-01 10:00Z"`.

use super::Enricher;
use crate::model::{Airport, FlightInfo};
use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDateTime, Utc};
use serde::Deserialize;

pub struct AeroDataBoxStatus {
    client: reqwest::Client,
    host: String,
    api_key: String,
}

impl AeroDataBoxStatus {
    pub fn new(api_key: String, host: String) -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("planespotter/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building HTTP client")?;
        Ok(AeroDataBoxStatus {
            client,
            host,
            api_key,
        })
    }
}

#[derive(Debug, Deserialize)]
struct Flight {
    departure: Option<Movement>,
    arrival: Option<Movement>,
}

#[derive(Debug, Deserialize)]
struct Movement {
    airport: Option<RawAirport>,
    #[serde(rename = "scheduledTime")]
    scheduled_time: Option<TimeObj>,
    #[serde(rename = "revisedTime")]
    revised_time: Option<TimeObj>,
    #[serde(rename = "predictedTime")]
    predicted_time: Option<TimeObj>,
    #[serde(rename = "runwayTime")]
    runway_time: Option<TimeObj>,
    #[serde(rename = "actualTime")]
    actual_time: Option<TimeObj>,
}

impl Movement {
    fn scheduled(&self) -> Option<DateTime<Utc>> {
        self.scheduled_time.as_ref().and_then(TimeObj::parse)
    }

    /// Best available "real" time, most authoritative first.
    fn estimated(&self) -> Option<DateTime<Utc>> {
        [
            &self.actual_time,
            &self.runway_time,
            &self.revised_time,
            &self.predicted_time,
        ]
        .into_iter()
        .flatten()
        .find_map(TimeObj::parse)
    }
}

#[derive(Debug, Deserialize)]
struct TimeObj {
    utc: Option<String>,
}

impl TimeObj {
    fn parse(&self) -> Option<DateTime<Utc>> {
        let raw = self.utc.as_deref()?.trim();
        // Try RFC3339 first, then AeroDataBox's "YYYY-MM-DD HH:MM[:SS]Z" form.
        if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
            return Some(dt.with_timezone(&Utc));
        }
        for fmt in ["%Y-%m-%d %H:%M:%SZ", "%Y-%m-%d %H:%MZ", "%Y-%m-%d %H:%M"] {
            if let Ok(naive) = NaiveDateTime::parse_from_str(raw, fmt) {
                return Some(DateTime::from_naive_utc_and_offset(naive, Utc));
            }
        }
        None
    }
}

#[derive(Debug, Deserialize)]
struct RawAirport {
    icao: Option<String>,
    iata: Option<String>,
    name: Option<String>,
}

impl From<RawAirport> for Airport {
    fn from(a: RawAirport) -> Self {
        Airport {
            icao: a.icao,
            iata: a.iata,
            name: a.name,
        }
    }
}

/// Build a `FlightInfo` from a single flight movement.
fn to_flight_info(flight: Flight) -> FlightInfo {
    let mut info = FlightInfo::default();
    if let Some(dep) = flight.departure {
        info.scheduled_departure = dep.scheduled();
        info.actual_departure = dep.estimated();
        info.origin = dep.airport.map(Into::into);
    }
    if let Some(arr) = flight.arrival {
        let sched = arr.scheduled();
        let est = arr.estimated();
        info.scheduled_arrival = sched;
        info.estimated_arrival = est;
        if let (Some(s), Some(e)) = (sched, est) {
            info.delay_minutes = Some((e - s).num_minutes());
        }
        info.destination = arr.airport.map(Into::into);
    }
    info
}

#[async_trait]
impl Enricher for AeroDataBoxStatus {
    async fn enrich(&self, callsign: &str) -> Result<Option<FlightInfo>> {
        let url = format!("https://{}/flights/callsign/{}", self.host, callsign.trim());
        let resp = self
            .client
            .get(&url)
            .header("x-rapidapi-key", &self.api_key)
            .header("x-rapidapi-host", &self.host)
            .send()
            .await
            .with_context(|| format!("requesting {url}"))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = resp
            .error_for_status()
            .with_context(|| format!("error status from {url}"))?;
        let flights: Vec<Flight> = resp
            .json()
            .await
            .with_context(|| format!("decoding response from {url}"))?;

        // Most relevant movement is the last (latest scheduled); take the first available.
        let info = flights.into_iter().next().map(to_flight_info);
        Ok(info.filter(|i| !i.is_empty()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_delay_from_revised_arrival() {
        let json = r#"[{
            "departure": {
                "airport": { "icao": "EGLL", "iata": "LHR", "name": "Heathrow" },
                "scheduledTime": { "utc": "2024-01-01 10:00Z" }
            },
            "arrival": {
                "airport": { "icao": "KJFK", "iata": "JFK", "name": "JFK" },
                "scheduledTime": { "utc": "2024-01-01 18:00Z" },
                "revisedTime": { "utc": "2024-01-01 18:14Z" }
            }
        }]"#;
        let flights: Vec<Flight> = serde_json::from_str(json).unwrap();
        let info = to_flight_info(flights.into_iter().next().unwrap());
        assert_eq!(info.origin.unwrap().iata.as_deref(), Some("LHR"));
        assert_eq!(info.destination.unwrap().icao.as_deref(), Some("KJFK"));
        assert_eq!(info.delay_minutes, Some(14));
    }

    #[test]
    fn parses_rfc3339_time() {
        let t = TimeObj {
            utc: Some("2024-01-01T18:00:00Z".into()),
        };
        assert!(t.parse().is_some());
    }
}
