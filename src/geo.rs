//! Region geometry: circle / polygon / bounding-box shapes, point containment, and the
//! bounding circle used to build position-source queries.

use anyhow::{Context, Result, bail};

/// Earth mean radius in nautical miles (used for haversine distance).
const EARTH_RADIUS_NM: f64 = 3440.065;

/// A watched geographic region. Coordinates are `(lat, lon)` in degrees throughout.
#[derive(Debug, Clone, PartialEq)]
pub enum Region {
    Circle {
        lat: f64,
        lon: f64,
        radius_nm: f64,
    },
    /// A polygon with an exterior ring and zero or more holes. A point is contained when it
    /// lies inside the exterior ring and outside every hole.
    Polygon {
        exterior: Vec<(f64, f64)>,
        holes: Vec<Vec<(f64, f64)>>,
    },
    BBox {
        min_lat: f64,
        min_lon: f64,
        max_lat: f64,
        max_lon: f64,
    },
}

impl Region {
    /// Whether the given point lies within this region.
    pub fn contains(&self, lat: f64, lon: f64) -> bool {
        match self {
            Region::Circle {
                lat: clat,
                lon: clon,
                radius_nm,
            } => haversine_nm(*clat, *clon, lat, lon) <= *radius_nm,
            Region::Polygon { exterior, holes } => {
                point_in_ring(exterior, lat, lon)
                    && !holes.iter().any(|h| point_in_ring(h, lat, lon))
            }
            Region::BBox {
                min_lat,
                min_lon,
                max_lat,
                max_lon,
            } => lat >= *min_lat && lat <= *max_lat && lon >= *min_lon && lon <= *max_lon,
        }
    }

    /// A `(lat, lon, radius_nm)` circle guaranteed to enclose the entire region. Used to build
    /// the position-source point/radius query; results are then filtered with [`contains`].
    pub fn bounding_circle(&self) -> (f64, f64, f64) {
        match self {
            Region::Circle {
                lat,
                lon,
                radius_nm,
            } => (*lat, *lon, *radius_nm),
            Region::Polygon { exterior, .. } => bounding_circle_of(exterior),
            Region::BBox {
                min_lat,
                min_lon,
                max_lat,
                max_lon,
            } => bounding_circle_of(&[
                (*min_lat, *min_lon),
                (*min_lat, *max_lon),
                (*max_lat, *max_lon),
                (*max_lat, *min_lon),
            ]),
        }
    }

    /// Parse one or more `Region::Polygon`s from an inline GeoJSON string.
    ///
    /// Accepts a bare `Polygon`/`MultiPolygon` geometry, a `Feature`, or a `FeatureCollection`
    /// (as exported by geojson.io). Every `Polygon` becomes one region and every `MultiPolygon`
    /// expands to several. GeoJSON coordinates are `[lon, lat]`; we convert to our `(lat, lon)`.
    /// The first ring of each polygon is the exterior, the rest are holes. Only explicitly-given
    /// content is accepted (no file paths).
    pub fn from_geojson(content: &str) -> Result<Vec<Region>> {
        use geojson::{GeoJson, Geometry};
        use geojson::Value as GeoValue;

        // Collect every geometry regardless of whether the top level is a geometry, a feature,
        // or a feature collection.
        let geojson: GeoJson = content.parse().context("parsing inline geojson")?;
        let geometries: Vec<Geometry> = match geojson {
            GeoJson::Geometry(g) => vec![g],
            GeoJson::Feature(f) => f.geometry.into_iter().collect(),
            GeoJson::FeatureCollection(fc) => {
                fc.features.into_iter().filter_map(|f| f.geometry).collect()
            }
        };

        let to_ring = |ring: &Vec<Vec<f64>>| -> Result<Vec<(f64, f64)>> {
            ring.iter()
                .map(|pos| match pos.as_slice() {
                    // GeoJSON position is [lon, lat, (elevation)].
                    [lon, lat, ..] => Ok((*lat, *lon)),
                    _ => bail!("geojson position must have at least [lon, lat]"),
                })
                .collect()
        };

        let polygon_to_region = |rings: &Vec<Vec<Vec<f64>>>| -> Result<Region> {
            let mut iter = rings.iter();
            let exterior = match iter.next() {
                Some(r) => to_ring(r)?,
                None => bail!("geojson polygon has no rings"),
            };
            let holes = iter.map(to_ring).collect::<Result<Vec<_>>>()?;
            Ok(Region::Polygon { exterior, holes })
        };

        let mut regions = Vec::new();
        for geometry in geometries {
            match geometry.value {
                GeoValue::Polygon(rings) => regions.push(polygon_to_region(&rings)?),
                GeoValue::MultiPolygon(polys) => {
                    for p in &polys {
                        regions.push(polygon_to_region(p)?);
                    }
                }
                other => bail!(
                    "geojson geometry must be Polygon or MultiPolygon, got {}",
                    other.type_name()
                ),
            }
        }
        if regions.is_empty() {
            bail!("geojson contained no Polygon/MultiPolygon geometry");
        }
        Ok(regions)
    }
}

/// A named region plus an optional altitude band, as the engine actually watches it.
///
/// The altitude band keeps the watch focused on (e.g.) aircraft on approach and excludes
/// high overflights. An aircraft is admitted when it's inside the geometry AND — if it reports
/// an altitude — that altitude is within `[min_alt_ft, max_alt_ft]`. Aircraft with no reported
/// altitude are not excluded on altitude grounds (we can't confirm a violation).
#[derive(Debug, Clone)]
pub struct WatchedRegion {
    pub name: String,
    pub region: Region,
    pub min_alt_ft: Option<f64>,
    pub max_alt_ft: Option<f64>,
}

impl WatchedRegion {
    /// Whether this region admits an aircraft at the given position and (optional) altitude.
    pub fn admits(&self, lat: f64, lon: f64, altitude_ft: Option<f64>) -> bool {
        if !self.region.contains(lat, lon) {
            return false;
        }
        if let (Some(min), Some(alt)) = (self.min_alt_ft, altitude_ft) {
            if alt < min {
                return false;
            }
        }
        if let (Some(max), Some(alt)) = (self.max_alt_ft, altitude_ft) {
            if alt > max {
                return false;
            }
        }
        true
    }

    /// The query circle enclosing this region's geometry.
    pub fn bounding_circle(&self) -> (f64, f64, f64) {
        self.region.bounding_circle()
    }
}

/// Great-circle distance in nautical miles between two `(lat, lon)` points.
fn haversine_nm(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_RADIUS_NM * a.sqrt().asin()
}

/// Ray-casting point-in-polygon test on a `(lat, lon)` ring (treated as planar; fine for the
/// small areas these regions cover).
fn point_in_ring(ring: &[(f64, f64)], lat: f64, lon: f64) -> bool {
    if ring.len() < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = ring.len() - 1;
    for i in 0..ring.len() {
        let (lat_i, lon_i) = ring[i];
        let (lat_j, lon_j) = ring[j];
        let intersects = (lon_i > lon) != (lon_j > lon)
            && lat < (lat_j - lat_i) * (lon - lon_i) / (lon_j - lon_i) + lat_i;
        if intersects {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Centroid-based bounding circle for a set of points: centre at the mean, radius to the
/// farthest point. Not minimal, but always enclosing — which is all the query needs.
fn bounding_circle_of(points: &[(f64, f64)]) -> (f64, f64, f64) {
    let n = points.len().max(1) as f64;
    let clat = points.iter().map(|p| p.0).sum::<f64>() / n;
    let clon = points.iter().map(|p| p.1).sum::<f64>() / n;
    let radius = points
        .iter()
        .map(|(lat, lon)| haversine_nm(clat, clon, *lat, *lon))
        .fold(0.0_f64, f64::max);
    (clat, clon, radius)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn circle_contains() {
        let r = Region::Circle {
            lat: 51.4775,
            lon: -0.4614,
            radius_nm: 5.0,
        };
        assert!(r.contains(51.4775, -0.4614)); // centre
        assert!(r.contains(51.50, -0.46)); // ~1.4nm north, inside
        assert!(!r.contains(52.0, -0.46)); // ~31nm north, outside
    }

    #[test]
    fn polygon_contains() {
        // ~unit square around (0,0): lat/lon from -1..1.
        let r = Region::Polygon {
            exterior: vec![(-1.0, -1.0), (-1.0, 1.0), (1.0, 1.0), (1.0, -1.0)],
            holes: vec![],
        };
        assert!(r.contains(0.0, 0.0));
        assert!(!r.contains(2.0, 0.0));
        assert!(!r.contains(0.0, 5.0));
    }

    #[test]
    fn polygon_with_hole() {
        // Outer 4x4 square with an inner 1x1 hole, both centred on the origin.
        let r = Region::Polygon {
            exterior: vec![(-2.0, -2.0), (-2.0, 2.0), (2.0, 2.0), (2.0, -2.0)],
            holes: vec![vec![(-0.5, -0.5), (-0.5, 0.5), (0.5, 0.5), (0.5, -0.5)]],
        };
        assert!(r.contains(1.5, 1.5)); // in outer, outside hole
        assert!(!r.contains(0.0, 0.0)); // inside hole -> excluded
    }

    #[test]
    fn bbox_contains() {
        let r = Region::BBox {
            min_lat: 0.0,
            min_lon: 0.0,
            max_lat: 1.0,
            max_lon: 1.0,
        };
        assert!(r.contains(0.5, 0.5));
        assert!(!r.contains(1.5, 0.5));
    }

    #[test]
    fn bounding_circle_encloses_polygon() {
        let r = Region::Polygon {
            exterior: vec![(0.0, 0.0), (0.0, 0.1), (0.1, 0.1), (0.1, 0.0)],
            holes: vec![],
        };
        let (clat, clon, radius) = r.bounding_circle();
        for (lat, lon) in [(0.0, 0.0), (0.0, 0.1), (0.1, 0.1), (0.1, 0.0)] {
            assert!(haversine_nm(clat, clon, lat, lon) <= radius + 1e-9);
        }
    }

    #[test]
    fn geojson_polygon_roundtrip() {
        // GeoJSON is [lon, lat]; this square spans lon -0.50..-0.40, lat 51.45..51.49.
        let gj = r#"{ "type": "Polygon", "coordinates": [[[-0.50,51.45],[-0.40,51.45],[-0.40,51.49],[-0.50,51.49],[-0.50,51.45]]] }"#;
        let regions = Region::from_geojson(gj).unwrap();
        assert_eq!(regions.len(), 1);
        match &regions[0] {
            Region::Polygon { exterior, holes } => {
                assert!(holes.is_empty());
                // First vertex must be (lat, lon) = (51.45, -0.50).
                assert_eq!(exterior[0], (51.45, -0.50));
            }
            _ => panic!("expected polygon"),
        }
        assert!(regions[0].contains(51.47, -0.45)); // inside
        assert!(!regions[0].contains(51.47, -0.30)); // east, outside
    }

    #[test]
    fn geojson_feature_collection() {
        // geojson.io exports a FeatureCollection; we must unwrap it.
        let gj = r#"{ "type": "FeatureCollection", "features": [
            { "type": "Feature", "properties": {}, "geometry": {
                "type": "Polygon",
                "coordinates": [[[4.85,52.32],[4.90,52.32],[4.90,52.31],[4.85,52.31],[4.85,52.32]]]
            } }
        ] }"#;
        let regions = Region::from_geojson(gj).unwrap();
        assert_eq!(regions.len(), 1);
        assert!(regions[0].contains(52.315, 4.87)); // inside
        assert!(!regions[0].contains(52.40, 4.87)); // north, outside
    }

    #[test]
    fn geojson_multipolygon_expands() {
        let gj = r#"{ "type": "MultiPolygon", "coordinates": [
            [[[0,0],[1,0],[1,1],[0,1],[0,0]]],
            [[[5,5],[6,5],[6,6],[5,6],[5,5]]]
        ] }"#;
        let regions = Region::from_geojson(gj).unwrap();
        assert_eq!(regions.len(), 2);
    }
}
