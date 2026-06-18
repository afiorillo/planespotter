//! Core data types shared across the engine, sources, enrichers, and frontends.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single aircraft as reported by a [`PositionSource`](crate::source::PositionSource).
///
/// Only fields that are reliably present in ADS-B feeds are required; the rest are optional
/// because ground traffic and partial messages often omit them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Aircraft {
    /// ICAO 24-bit address as a lowercase hex string, e.g. `"4ca7b5"`. Stable identity.
    pub hex: String,
    /// Trimmed callsign / flight id, e.g. `"BAW123"`. `None` if the aircraft isn't transmitting one.
    pub callsign: Option<String>,
    pub lat: f64,
    pub lon: f64,
    /// Barometric altitude in feet. `None` when on the ground or not reported.
    pub altitude_ft: Option<f64>,
    /// Ground speed in knots.
    pub ground_speed_kt: Option<f64>,
    /// True track over ground in degrees.
    pub track_deg: Option<f64>,
    /// Aircraft type designator, e.g. `"A320"`.
    pub type_code: Option<String>,
    /// Registration / tail number, e.g. `"G-EUYB"`.
    pub registration: Option<String>,
    /// Mode A squawk code.
    pub squawk: Option<String>,
}

/// An airport, as resolved by an enricher.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Airport {
    pub icao: Option<String>,
    pub iata: Option<String>,
    pub name: Option<String>,
}

/// Route + schedule information for a flight, assembled by one or more enrichers.
///
/// Any field may be absent depending on which enrichers are configured and what they could
/// resolve (e.g. route from adsbdb but no schedule because no AeroDataBox key).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FlightInfo {
    pub origin: Option<Airport>,
    pub destination: Option<Airport>,
    pub scheduled_departure: Option<DateTime<Utc>>,
    pub scheduled_arrival: Option<DateTime<Utc>>,
    pub estimated_arrival: Option<DateTime<Utc>>,
    pub actual_departure: Option<DateTime<Utc>>,
    /// Arrival delay in minutes (positive = late). Derived from schedule vs. estimated/actual.
    pub delay_minutes: Option<i64>,
}

impl FlightInfo {
    /// True when this carries no useful information at all.
    pub fn is_empty(&self) -> bool {
        self.origin.is_none()
            && self.destination.is_none()
            && self.scheduled_departure.is_none()
            && self.scheduled_arrival.is_none()
            && self.estimated_arrival.is_none()
            && self.actual_departure.is_none()
            && self.delay_minutes.is_none()
    }

    /// Merge another `FlightInfo` into this one, filling only fields we don't already have.
    /// Used by [`CompositeEnricher`](crate::enrich::CompositeEnricher) to combine providers.
    pub fn merge(&mut self, other: FlightInfo) {
        self.origin = self.origin.take().or(other.origin);
        self.destination = self.destination.take().or(other.destination);
        self.scheduled_departure = self.scheduled_departure.or(other.scheduled_departure);
        self.scheduled_arrival = self.scheduled_arrival.or(other.scheduled_arrival);
        self.estimated_arrival = self.estimated_arrival.or(other.estimated_arrival);
        self.actual_departure = self.actual_departure.or(other.actual_departure);
        self.delay_minutes = self.delay_minutes.or(other.delay_minutes);
    }
}

/// What happened to an aircraft relative to a watched region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventKind {
    /// Aircraft just appeared inside the region.
    Entered,
    /// Aircraft remains inside the region (position refreshed).
    Updated,
    /// Aircraft left the region (or stopped being reported).
    Left,
}

/// A spotting event broadcast by the engine to every subscribed frontend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpottingEvent {
    pub kind: EventKind,
    /// Name of the region this event pertains to.
    pub region: String,
    pub aircraft: Aircraft,
    /// Enrichment, if it was resolved (only populated on `Entered`).
    pub flight_info: Option<FlightInfo>,
    pub observed_at: DateTime<Utc>,
}
