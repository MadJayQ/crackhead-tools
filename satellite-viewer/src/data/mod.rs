pub mod nexrad;
pub mod sentinel;

/// Axis-aligned geographic bounding box (WGS-84 decimal degrees).
#[derive(Debug, Clone, PartialEq)]
pub struct GeoBounds {
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
}

impl GeoBounds {
    pub fn new(min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64) -> Self {
        Self { min_lon, min_lat, max_lon, max_lat }
    }

    pub fn center_lon(&self) -> f64 {
        (self.min_lon + self.max_lon) / 2.0
    }

    pub fn center_lat(&self) -> f64 {
        (self.min_lat + self.max_lat) / 2.0
    }

    /// Returns true if `other` overlaps this bounding box.
    pub fn intersects(&self, other: &GeoBounds) -> bool {
        self.min_lon < other.max_lon
            && self.max_lon > other.min_lon
            && self.min_lat < other.max_lat
            && self.max_lat > other.min_lat
    }

    /// Expand the bounds so they include `other`.
    pub fn union(&self, other: &GeoBounds) -> GeoBounds {
        GeoBounds {
            min_lon: self.min_lon.min(other.min_lon),
            min_lat: self.min_lat.min(other.min_lat),
            max_lon: self.max_lon.max(other.max_lon),
            max_lat: self.max_lat.max(other.max_lat),
        }
    }
}

/// A decoded, RGBA image together with the geographic area it covers.
#[derive(Clone)]
pub struct GeoImage {
    pub bounds: GeoBounds,
    /// Raw RGBA8 pixels, row-major from top-left.
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}
