//! Enrichers: resolve a callsign into route + schedule + delay information.
//!
//! Raw ADS-B carries none of this, so each enricher calls an external flight-info API. A
//! [`CompositeEnricher`] runs several and merges their results (e.g. route from adsbdb, delay
//! from AeroDataBox), and a [`CachingEnricher`] wraps any enricher with a TTL cache to respect
//! rate limits and free-tier quotas.

mod adsbdb;
mod aerodatabox;

pub use adsbdb::AdsbdbRoute;
pub use aerodatabox::AeroDataBoxStatus;

use crate::model::FlightInfo;
use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Resolves a callsign into [`FlightInfo`]. Returns `Ok(None)` when nothing is known for the
/// callsign (e.g. unrecognised); returns `Err` only on transport/parse failure.
#[async_trait]
pub trait Enricher: Send + Sync {
    async fn enrich(&self, callsign: &str) -> Result<Option<FlightInfo>>;
}

/// Runs several enrichers and merges their output into a single [`FlightInfo`].
///
/// Earlier enrichers take precedence for overlapping fields (see [`FlightInfo::merge`]). An
/// individual enricher erroring is logged and skipped rather than failing the whole lookup.
pub struct CompositeEnricher {
    enrichers: Vec<Box<dyn Enricher>>,
}

impl CompositeEnricher {
    pub fn new(enrichers: Vec<Box<dyn Enricher>>) -> Self {
        CompositeEnricher { enrichers }
    }

    pub fn is_empty(&self) -> bool {
        self.enrichers.is_empty()
    }
}

#[async_trait]
impl Enricher for CompositeEnricher {
    async fn enrich(&self, callsign: &str) -> Result<Option<FlightInfo>> {
        let mut merged: Option<FlightInfo> = None;
        for enricher in &self.enrichers {
            match enricher.enrich(callsign).await {
                Ok(Some(info)) => match &mut merged {
                    Some(acc) => acc.merge(info),
                    None => merged = Some(info),
                },
                Ok(None) => {}
                Err(e) => tracing::warn!(callsign, error = %e, "enricher failed; skipping"),
            }
        }
        Ok(merged.filter(|i| !i.is_empty()))
    }
}

/// Wraps an enricher with a TTL cache keyed by callsign.
pub struct CachingEnricher<E: Enricher> {
    inner: E,
    ttl: Duration,
    cache: Mutex<HashMap<String, (Instant, Option<FlightInfo>)>>,
}

impl<E: Enricher> CachingEnricher<E> {
    pub fn new(inner: E, ttl: Duration) -> Self {
        CachingEnricher {
            inner,
            ttl,
            cache: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl<E: Enricher> Enricher for CachingEnricher<E> {
    async fn enrich(&self, callsign: &str) -> Result<Option<FlightInfo>> {
        {
            let cache = self.cache.lock().await;
            if let Some((at, info)) = cache.get(callsign) {
                if at.elapsed() < self.ttl {
                    return Ok(info.clone());
                }
            }
        }
        let fresh = self.inner.enrich(callsign).await?;
        let mut cache = self.cache.lock().await;
        cache.insert(callsign.to_string(), (Instant::now(), fresh.clone()));
        Ok(fresh)
    }
}
