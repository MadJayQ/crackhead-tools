/// Web-Mercator viewport math.
///
/// Coordinate systems used here:
///   • **Geo**      — WGS-84 (lon, lat) in decimal degrees.
///   • **Mercator** — floating-point pixel position at the current zoom.
///     At zoom `z` the world is `(2^z × TILE_PX)` px wide and tall.
///   • **Screen**   — pixel offset from the top-left corner of the window.
///   • **NDC**      — normalised device coordinates fed to the vertex shader.
///     (-1, -1) = bottom-left, (1, 1) = top-right.  wgpu clips in NDC space.

use std::f64::consts::PI;

pub const TILE_PX: f64 = 256.0;

/// Viewport state — the only thing the renderer needs to know about the map.
#[derive(Clone, Debug)]
pub struct MapViewport {
    /// Geographic centre of the window.
    pub center_lon: f64,
    pub center_lat: f64,
    pub zoom: f64,
    /// Window dimensions in physical pixels.
    pub screen_w: u32,
    pub screen_h: u32,
}

impl MapViewport {
    pub fn new(lon: f64, lat: f64, zoom: f64, w: u32, h: u32) -> Self {
        Self {
            center_lon: lon,
            center_lat: lat,
            zoom,
            screen_w: w,
            screen_h: h,
        }
    }

    // ── Coordinate conversions ─────────────────────────────────────────────────

    /// Mercator pixel position of a (lon, lat) point at this zoom level.
    pub fn geo_to_mercator(&self, lon: f64, lat: f64) -> (f64, f64) {
        let scale = 2f64.powf(self.zoom) * TILE_PX;
        let mx = (lon + 180.0) / 360.0 * scale;
        let lat_r = lat.to_radians();
        let my = (1.0 - (lat_r.tan() + 1.0 / lat_r.cos()).ln() / PI) / 2.0 * scale;
        (mx, my)
    }

    /// Screen pixel position of a (lon, lat) point.
    pub fn geo_to_screen(&self, lon: f64, lat: f64) -> (f64, f64) {
        let (mx, my) = self.geo_to_mercator(lon, lat);
        let (cx, cy) = self.geo_to_mercator(self.center_lon, self.center_lat);
        let sx = mx - cx + self.screen_w as f64 / 2.0;
        let sy = my - cy + self.screen_h as f64 / 2.0;
        (sx, sy)
    }

    /// NDC position of a (lon, lat) point, suitable for vertex buffers.
    pub fn geo_to_ndc(&self, lon: f64, lat: f64) -> [f32; 2] {
        let (sx, sy) = self.geo_to_screen(lon, lat);
        let nx = (2.0 * sx / self.screen_w as f64 - 1.0) as f32;
        let ny = (1.0 - 2.0 * sy / self.screen_h as f64) as f32;
        [nx, ny]
    }

    // ── Tile enumeration ───────────────────────────────────────────────────────

    /// Integer zoom level for tile requests.
    pub fn tile_zoom(&self) -> u8 {
        self.zoom.floor().clamp(0.0, 19.0) as u8
    }

    /// All (x, y) tile indices visible in the current viewport.
    pub fn visible_tiles(&self) -> Vec<(u32, u32)> {
        let z = self.tile_zoom();
        let n = 2u32.pow(z as u32);
        let tile_size = TILE_PX;           // tile pixels at zoom z

        // Tile that contains the screen centre.
        let (cx, cy) = self.geo_to_mercator(self.center_lon, self.center_lat);
        let cx_tile = (cx / tile_size) as i64;
        let cy_tile = (cy / tile_size) as i64;

        // How many extra tiles we need on each side.
        let extra_x = (self.screen_w as f64 / (2.0 * tile_size)).ceil() as i64 + 1;
        let extra_y = (self.screen_h as f64 / (2.0 * tile_size)).ceil() as i64 + 1;

        let mut tiles = Vec::new();
        for dy in -extra_y..=extra_y {
            for dx in -extra_x..=extra_x {
                let tx = cx_tile + dx;
                let ty = cy_tile + dy;
                if tx < 0 || ty < 0 || tx >= n as i64 || ty >= n as i64 {
                    continue;
                }
                tiles.push((tx as u32, ty as u32));
            }
        }
        tiles
    }

    /// Geographic bounds of a tile (z, x, y).
    pub fn tile_bounds(z: u8, x: u32, y: u32) -> (f64, f64, f64, f64) {
        let n = 2u32.pow(z as u32) as f64;
        let min_lon = x as f64 / n * 360.0 - 180.0;
        let max_lon = (x + 1) as f64 / n * 360.0 - 180.0;
        let max_lat = Self::tile_lat(y as f64, n);
        let min_lat = Self::tile_lat(y as f64 + 1.0, n);
        (min_lon, min_lat, max_lon, max_lat)
    }

    fn tile_lat(y: f64, n: f64) -> f64 {
        (PI * (1.0 - 2.0 * y / n)).sinh().atan().to_degrees()
    }

    /// NDC quad corners for a tile: [TL, TR, BL, BR] as [[f32;2]; 4].
    pub fn tile_ndc_quad(&self, z: u8, x: u32, y: u32) -> [[f32; 2]; 4] {
        let (min_lon, min_lat, max_lon, max_lat) = Self::tile_bounds(z, x, y);
        [
            self.geo_to_ndc(min_lon, max_lat), // TL
            self.geo_to_ndc(max_lon, max_lat), // TR
            self.geo_to_ndc(min_lon, min_lat), // BL
            self.geo_to_ndc(max_lon, min_lat), // BR
        ]
    }

    /// NDC quad corners for an arbitrary geographic bounding box.
    pub fn bounds_ndc_quad(
        &self,
        min_lon: f64,
        min_lat: f64,
        max_lon: f64,
        max_lat: f64,
    ) -> [[f32; 2]; 4] {
        [
            self.geo_to_ndc(min_lon, max_lat), // TL
            self.geo_to_ndc(max_lon, max_lat), // TR
            self.geo_to_ndc(min_lon, min_lat), // BL
            self.geo_to_ndc(max_lon, min_lat), // BR
        ]
    }

    // ── Drag / zoom helpers ────────────────────────────────────────────────────

    /// Pan by `(dx, dy)` screen pixels.
    pub fn pan_pixels(&mut self, dx: f64, dy: f64) {
        let scale = 2f64.powf(self.zoom) * TILE_PX;
        let dlon = dx / scale * 360.0;
        let dlat_mercator = dy / scale;

        let (_, my) = self.geo_to_mercator(self.center_lon, self.center_lat);
        let new_my = my + dlat_mercator * scale;
        let lat_r = ((1.0 - new_my * 2.0 / scale) * PI).sinh().atan();
        self.center_lon = (self.center_lon - dlon).clamp(-180.0, 180.0);
        self.center_lat = lat_r.to_degrees().clamp(-85.0, 85.0);
    }

    /// Zoom in/out by `delta` steps centred on the screen-pixel position `(px, py)`.
    pub fn zoom_at(&mut self, delta: f64, px: f64, py: f64) {
        // Convert cursor position to geo before zoom change.
        let sx = px - self.screen_w as f64 / 2.0;
        let sy = py - self.screen_h as f64 / 2.0;
        let scale = 2f64.powf(self.zoom) * TILE_PX;
        let (cx, cy) = self.geo_to_mercator(self.center_lon, self.center_lat);
        let cursor_mx = cx + sx;
        let cursor_my = cy + sy;
        let cursor_lon = cursor_mx / scale * 360.0 - 180.0;

        self.zoom = (self.zoom + delta).clamp(1.0, 19.0);

        // After zoom, shift centre so the cursor geo-point stays under the cursor.
        let new_scale = 2f64.powf(self.zoom) * TILE_PX;
        let new_cx = cursor_mx * (new_scale / scale) - sx;
        let new_cy = cursor_my * (new_scale / scale) - sy;
        let new_lon = new_cx / new_scale * 360.0 - 180.0;
        self.center_lon = (self.center_lon + (cursor_lon - new_lon)).clamp(-180.0, 180.0);
        let lat_r = ((1.0 - new_cy * 2.0 / new_scale) * PI).sinh().atan();
        self.center_lat = lat_r.to_degrees().clamp(-85.0, 85.0);
    }

    /// Returns a `crate::ui::map_view::MapViewport` snapshot for data fetchers.
    pub fn as_data_viewport(&self) -> crate::ui::map_view::MapViewport {
        let deg_span = 360.0 / 2f64.powi(self.zoom as i32);
        crate::ui::map_view::MapViewport {
            center_lon: self.center_lon,
            center_lat: self.center_lat,
            zoom: self.zoom,
            bounds: crate::data::GeoBounds::new(
                self.center_lon - deg_span / 2.0,
                self.center_lat - deg_span / 4.0,
                self.center_lon + deg_span / 2.0,
                self.center_lat + deg_span / 4.0,
            ),
        }
    }
}
