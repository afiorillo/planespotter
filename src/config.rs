//! TOML configuration: poll interval, position source, enrichers, and watched regions.

use crate::geo::{Region, WatchedRegion};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Seconds between poll cycles.
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    #[serde(default)]
    pub source: SourceConfig,
    #[serde(default)]
    pub enrich: EnrichConfig,
    #[serde(default)]
    pub regions: Vec<RegionConfig>,
}

fn default_poll_interval() -> u64 {
    5
}

#[derive(Debug, Clone, Deserialize)]
pub struct SourceConfig {
    /// Which position source to use. `"network"` today; `"rtlsdr"` later.
    #[serde(default = "default_source_kind")]
    pub kind: String,
    /// Base URL for the network source (airplanes.live or the API-compatible adsb.fi).
    #[serde(default = "default_base_url")]
    pub base_url: String,
}

fn default_source_kind() -> String {
    "network".to_string()
}

fn default_base_url() -> String {
    "https://api.airplanes.live/v2".to_string()
}

impl Default for SourceConfig {
    fn default() -> Self {
        SourceConfig {
            kind: default_source_kind(),
            base_url: default_base_url(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct EnrichConfig {
    /// Enable the free adsbdb route lookup (origin/destination).
    #[serde(default)]
    pub adsbdb: bool,
    /// AeroDataBox API key for schedule/delay. Enrichment is skipped when absent.
    #[serde(default)]
    pub aerodatabox_key: Option<String>,
    /// RapidAPI host for AeroDataBox (overridable in case the user is on api.market etc.).
    #[serde(default = "default_aerodatabox_host")]
    pub aerodatabox_host: String,
}

fn default_aerodatabox_host() -> String {
    "aerodatabox.p.rapidapi.com".to_string()
}

/// One `[[regions]]` entry: a name plus exactly one shape specification, plus optional
/// altitude limits.
#[derive(Debug, Clone, Deserialize)]
pub struct RegionConfig {
    pub name: String,
    pub circle: Option<CircleConfig>,
    /// Inline polygon as `[[lat, lon], ...]`.
    pub polygon: Option<Vec<[f64; 2]>>,
    pub bbox: Option<BBoxConfig>,
    /// Inline GeoJSON `Polygon`/`MultiPolygon` geometry (content only, no file paths).
    pub geojson: Option<String>,
    /// Only admit aircraft at or above this barometric altitude (feet).
    pub min_alt_ft: Option<f64>,
    /// Only admit aircraft at or below this barometric altitude (feet) — excludes high overflights.
    pub max_alt_ft: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CircleConfig {
    pub lat: f64,
    pub lon: f64,
    pub radius_nm: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BBoxConfig {
    pub min_lat: f64,
    pub min_lon: f64,
    pub max_lat: f64,
    pub max_lon: f64,
}

impl RegionConfig {
    /// Expand this config entry into one or more watched regions (GeoJSON `MultiPolygon` yields
    /// several). The altitude band applies to every region produced. Errors if zero or more than
    /// one shape is specified.
    pub fn into_regions(self) -> Result<Vec<WatchedRegion>> {
        let specified = self.circle.is_some() as u8
            + self.polygon.is_some() as u8
            + self.bbox.is_some() as u8
            + self.geojson.is_some() as u8;
        if specified != 1 {
            bail!(
                "region {:?} must specify exactly one of: circle, polygon, bbox, geojson (found {})",
                self.name,
                specified
            );
        }
        let (name, min_alt_ft, max_alt_ft) = (self.name.clone(), self.min_alt_ft, self.max_alt_ft);
        let wrap = |region: Region| WatchedRegion {
            name: name.clone(),
            region,
            min_alt_ft,
            max_alt_ft,
        };

        if let Some(c) = self.circle {
            return Ok(vec![wrap(Region::Circle {
                lat: c.lat,
                lon: c.lon,
                radius_nm: c.radius_nm,
            })]);
        }
        if let Some(p) = self.polygon {
            let exterior = p.into_iter().map(|[lat, lon]| (lat, lon)).collect();
            return Ok(vec![wrap(Region::Polygon {
                exterior,
                holes: vec![],
            })]);
        }
        if let Some(b) = self.bbox {
            return Ok(vec![wrap(Region::BBox {
                min_lat: b.min_lat,
                min_lon: b.min_lon,
                max_lat: b.max_lat,
                max_lon: b.max_lon,
            })]);
        }
        // geojson
        let content = self.geojson.unwrap();
        let regions = Region::from_geojson(&content)
            .with_context(|| format!("parsing geojson for region {:?}", self.name))?;
        Ok(regions.into_iter().map(wrap).collect())
    }
}

impl Config {
    /// Load and parse a config file.
    pub fn load(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let config: Config = toml::from_str(&text)
            .with_context(|| format!("parsing config {}", path.display()))?;
        Ok(config)
    }

    /// Flatten all region configs into resolved watched regions.
    pub fn resolve_regions(&self) -> Result<Vec<WatchedRegion>> {
        let mut out = Vec::new();
        for rc in &self.regions {
            out.extend(rc.clone().into_regions()?);
        }
        if out.is_empty() {
            bail!("no regions configured");
        }
        Ok(out)
    }

    /// Override secrets from the environment (e.g. the AeroDataBox key from a gitignored `.env`).
    pub fn apply_env_secrets(&mut self) {
        if let Ok(key) = std::env::var("AERODATABOX_API_KEY") {
            if !key.trim().is_empty() {
                self.enrich.aerodatabox_key = Some(key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_full_config() {
        let toml_src = r#"
poll_interval_secs = 7

[source]
kind = "network"
base_url = "https://api.airplanes.live/v2"

[enrich]
adsbdb = true
aerodatabox_key = "abc"

[[regions]]
name = "circle region"
[regions.circle]
lat = 51.0
lon = -0.4
radius_nm = 6
"#;
        let cfg: Config = toml::from_str(toml_src).unwrap();
        assert_eq!(cfg.poll_interval_secs, 7);
        assert!(cfg.enrich.adsbdb);
        assert_eq!(cfg.enrich.aerodatabox_key.as_deref(), Some("abc"));
        let regions = cfg.resolve_regions().unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].name, "circle region");
    }

    #[test]
    fn parses_altitude_limits() {
        let toml_src = r#"
[[regions]]
name = "approach"
max_alt_ft = 4000
[regions.circle]
lat = 40.64
lon = -73.78
radius_nm = 6
"#;
        let cfg: Config = toml::from_str(toml_src).unwrap();
        let regions = cfg.resolve_regions().unwrap();
        assert_eq!(regions[0].max_alt_ft, Some(4000.0));
        // A high overflight is excluded; a low approach inside the circle is admitted.
        assert!(!regions[0].admits(40.64, -73.78, Some(35000.0)));
        assert!(regions[0].admits(40.64, -73.78, Some(2500.0)));
        // Unknown altitude is not excluded on altitude grounds.
        assert!(regions[0].admits(40.64, -73.78, None));
    }

    #[test]
    fn rejects_region_with_multiple_shapes() {
        let rc = RegionConfig {
            name: "bad".into(),
            circle: Some(CircleConfig {
                lat: 0.0,
                lon: 0.0,
                radius_nm: 1.0,
            }),
            polygon: Some(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]]),
            bbox: None,
            geojson: None,
            min_alt_ft: None,
            max_alt_ft: None,
        };
        assert!(rc.into_regions().is_err());
    }

    #[test]
    fn geojson_region_expands() {
        let rc = RegionConfig {
            name: "gj".into(),
            circle: None,
            polygon: None,
            bbox: None,
            geojson: Some(
                r#"{ "type": "Polygon", "coordinates": [[[0,0],[1,0],[1,1],[0,1],[0,0]]] }"#
                    .into(),
            ),
            min_alt_ft: None,
            max_alt_ft: None,
        };
        let regions = rc.into_regions().unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].name, "gj");
    }
}
