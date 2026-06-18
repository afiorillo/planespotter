//! Position sources: where aircraft observations come from.

mod network;

pub use network::NetworkSource;

use crate::model::Aircraft;
use anyhow::Result;
use async_trait::async_trait;

/// A source of live aircraft positions, queryable by a point + radius.
///
/// Implementors over-report (everything within the circle); the engine filters to the exact
/// region with [`Region::contains`](crate::geo::Region::contains). A future RTL-SDR/dump1090
/// source implements this same trait.
#[async_trait]
pub trait PositionSource: Send + Sync {
    /// Return all aircraft with a known position within `radius_nm` of `(lat, lon)`.
    async fn poll(&self, lat: f64, lon: f64, radius_nm: f64) -> Result<Vec<Aircraft>>;
}
