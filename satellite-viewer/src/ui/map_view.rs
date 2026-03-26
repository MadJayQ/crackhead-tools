/// `MapViewport` — a geographic viewport snapshot shared with data-fetch clients.
///
/// The actual map rendering is handled by `src/renderer/viewport.rs` (Mercator
/// math + NDC transforms) and `src/renderer/tile_cache.rs` (OSM tile fetching).
/// This module just provides the `MapViewport` type that the data modules use
/// to decide whether to re-fetch.

use crate::data::GeoBounds;

/// Snapshot of the map viewport for data-fetching decisions.
#[derive(Clone, Debug)]
pub struct MapViewport {
    pub center_lon: f64,
    pub center_lat: f64,
    pub zoom: f64,
    pub bounds: GeoBounds,
}

impl MapViewport {
    pub fn is_close_to(&self, other: &MapViewport) -> bool {
        const THRESHOLD: f64 = 0.5;
        (self.center_lon - other.center_lon).abs() < THRESHOLD
            && (self.center_lat - other.center_lat).abs() < THRESHOLD
            && (self.zoom - other.zoom).abs() < 0.5
    }

    pub fn zoom_level(&self) -> u8 {
        self.zoom.round().clamp(1.0, 18.0) as u8
    }
}
