//! The core engine: poll the position source, filter aircraft to each region, enrich newly
//! spotted flights, and broadcast [`SpottingEvent`]s to subscribed frontends.

use crate::config::Config;
use crate::enrich::{AdsbdbRoute, AeroDataBoxStatus, CachingEnricher, CompositeEnricher, Enricher};
use crate::geo::WatchedRegion;
use crate::model::{Aircraft, EventKind, SpottingEvent};
use crate::source::{NetworkSource, PositionSource};
use anyhow::{Result, bail};
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;

/// Cache lifetime for enrichment lookups.
const ENRICH_TTL: Duration = Duration::from_secs(300);

/// Capacity of the broadcast channel buffer.
const EVENT_CHANNEL_CAP: usize = 256;

pub struct Engine {
    source: Arc<dyn PositionSource>,
    enricher: Arc<dyn Enricher>,
    regions: Vec<WatchedRegion>,
    poll_interval: Duration,
    /// Per-region set of aircraft currently inside, with their last-seen state.
    present: HashMap<usize, HashMap<String, Aircraft>>,
    tx: broadcast::Sender<SpottingEvent>,
}

impl Engine {
    /// Build an engine from config, wiring the configured source and enrichers.
    pub fn from_config(config: &Config) -> Result<Self> {
        let source: Arc<dyn PositionSource> = match config.source.kind.as_str() {
            "network" => Arc::new(NetworkSource::new(&config.source.base_url)?),
            other => bail!("unknown source kind {:?} (only \"network\" is implemented)", other),
        };

        let mut enrichers: Vec<Box<dyn Enricher>> = Vec::new();
        if config.enrich.adsbdb {
            enrichers.push(Box::new(AdsbdbRoute::new()?));
        }
        if let Some(key) = &config.enrich.aerodatabox_key {
            enrichers.push(Box::new(AeroDataBoxStatus::new(
                key.clone(),
                config.enrich.aerodatabox_host.clone(),
            )?));
        }
        let enricher: Arc<dyn Enricher> = Arc::new(CachingEnricher::new(
            CompositeEnricher::new(enrichers),
            ENRICH_TTL,
        ));

        let regions = config.resolve_regions()?;
        let (tx, _) = broadcast::channel(EVENT_CHANNEL_CAP);

        Ok(Engine {
            source,
            enricher,
            present: (0..regions.len()).map(|i| (i, HashMap::new())).collect(),
            regions,
            poll_interval: Duration::from_secs(config.poll_interval_secs),
            tx,
        })
    }

    /// Subscribe to spotting events. Call before [`run`](Engine::run).
    pub fn subscribe(&self) -> broadcast::Receiver<SpottingEvent> {
        self.tx.subscribe()
    }

    /// Run a single poll cycle across all regions, emitting events. Exposed for `--once` runs
    /// and testing.
    pub async fn poll_once(&mut self) -> Result<()> {
        for idx in 0..self.regions.len() {
            if let Err(e) = self.poll_region(idx).await {
                let name = &self.regions[idx].name;
                tracing::warn!(region = %name, error = %e, "region poll failed");
            }
        }
        Ok(())
    }

    /// Run forever, polling every `poll_interval`.
    pub async fn run(&mut self) -> Result<()> {
        let mut ticker = tokio::time::interval(self.poll_interval);
        loop {
            ticker.tick().await;
            self.poll_once().await?;
        }
    }

    async fn poll_region(&mut self, idx: usize) -> Result<()> {
        let name = self.regions[idx].name.clone();
        let (lat, lon, radius) = self.regions[idx].bounding_circle();

        let observed = self.source.poll(lat, lon, radius).await?;
        let region = &self.regions[idx];

        // Aircraft genuinely inside the precise region and altitude band, keyed by hex.
        let mut current: HashMap<String, Aircraft> = HashMap::new();
        for ac in observed {
            if region.admits(ac.lat, ac.lon, ac.altitude_ft) {
                current.insert(ac.hex.clone(), ac);
            }
        }

        let previous = self.present.get(&idx).cloned().unwrap_or_default();
        let prev_hexes: HashSet<&String> = previous.keys().collect();

        // Entered / Updated.
        for (hex, ac) in &current {
            if prev_hexes.contains(hex) {
                self.emit(EventKind::Updated, &name, ac.clone(), None);
            } else {
                let flight_info = match &ac.callsign {
                    Some(cs) => self.enricher.enrich(cs).await.unwrap_or_else(|e| {
                        tracing::warn!(callsign = %cs, error = %e, "enrichment failed");
                        None
                    }),
                    None => None,
                };
                self.emit(EventKind::Entered, &name, ac.clone(), flight_info);
            }
        }

        // Left.
        for (hex, ac) in &previous {
            if !current.contains_key(hex) {
                self.emit(EventKind::Left, &name, ac.clone(), None);
            }
        }

        self.present.insert(idx, current);
        Ok(())
    }

    fn emit(&self, kind: EventKind, region: &str, aircraft: Aircraft, flight_info: Option<crate::model::FlightInfo>) {
        let event = SpottingEvent {
            kind,
            region: region.to_string(),
            aircraft,
            flight_info,
            observed_at: Utc::now(),
        };
        // Ignore send errors: a closed channel just means no subscribers right now.
        let _ = self.tx.send(event);
    }
}
