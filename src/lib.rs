//! Planespotter: watch GPS regions and report aircraft inside them with route + delay info.
//!
//! The engine polls a [`PositionSource`](source::PositionSource), filters aircraft to the exact
//! [`Region`](geo::Region) geometry, enriches newly-spotted flights via an
//! [`Enricher`](enrich::Enricher), and broadcasts [`SpottingEvent`](model::SpottingEvent)s that
//! any frontend can subscribe to.

pub mod config;
pub mod engine;
pub mod enrich;
pub mod geo;
pub mod model;
pub mod source;
