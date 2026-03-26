/// Central map panel.
///
/// Architecture:
///   • `walkers::HttpTiles` fetches OpenStreetMap base-map tiles.
///   • Two custom `walkers::Plugin` implementations sit on top:
///       - `SentinelPlugin`  — Sentinel-2 true-colour tiles
///       - `RadarPlugin`     — NEXRAD composite frames (single full-CONUS image)
///   • Both plugins own the data they need for one frame and are recreated each update.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use egui::ColorImage;

use crate::data::{
    nexrad::{NexradEvent, RadarFrameStore},
    sentinel::SentinelEvent,
    GeoBounds, GeoImage,
};

// ── MapViewport ───────────────────────────────────────────────────────────────

/// A snapshot of the current map viewport (center + zoom).
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

// ── Texture helpers ───────────────────────────────────────────────────────────

fn geo_image_to_texture(
    ctx: &egui::Context,
    name: &str,
    img: &GeoImage,
) -> egui::TextureHandle {
    let color_image = ColorImage::from_rgba_unmultiplied(
        [img.width as usize, img.height as usize],
        &img.pixels,
    );
    ctx.load_texture(name, color_image, egui::TextureOptions::LINEAR)
}

/// Convert a geographic (lon, lat) pair to a screen `Pos2` via the projector.
/// `Projector::project` returns `Vec2` (offset from screen origin), so we
/// need to convert it.
fn project_lon_lat(projector: &walkers::Projector, lon: f64, lat: f64) -> egui::Pos2 {
    let v = projector.project(walkers::lon_lat(lon, lat));
    egui::pos2(v.x, v.y)
}

// ── Sentinel layer ────────────────────────────────────────────────────────────

pub struct SentinelLayer {
    pub visible: bool,
    pub opacity: f32,
    pub scene_id: Option<String>,
    pub cloud_cover: Option<f64>,
    pub scene_datetime: Option<String>,
    pub cog_url: Option<String>,
    tiles: HashMap<(u8, u32, u32), egui::TextureHandle>,
    tile_bounds: HashMap<(u8, u32, u32), GeoBounds>,
}

impl SentinelLayer {
    pub fn new() -> Self {
        Self {
            visible: true,
            opacity: 0.8,
            scene_id: None,
            cloud_cover: None,
            scene_datetime: None,
            cog_url: None,
            tiles: HashMap::new(),
            tile_bounds: HashMap::new(),
        }
    }

    pub fn handle_event(&mut self, event: SentinelEvent, ctx: &egui::Context) {
        match event {
            SentinelEvent::SceneFound {
                scene_id,
                bounds: _,
                cloud_cover,
                datetime,
                cog_url,
            } => {
                self.scene_id = Some(scene_id);
                self.cloud_cover = Some(cloud_cover);
                self.scene_datetime = Some(datetime);
                self.cog_url = Some(cog_url);
                self.tiles.clear();
                self.tile_bounds.clear();
            }
            SentinelEvent::TileReady { z, x, y, image } => {
                let name = format!("sentinel_{z}_{x}_{y}");
                let bounds = image.bounds.clone();
                let tex = geo_image_to_texture(ctx, &name, &image);
                self.tiles.insert((z, x, y), tex);
                self.tile_bounds.insert((z, x, y), bounds);
            }
            SentinelEvent::Error(e) => {
                log::warn!("Sentinel error: {e}");
            }
        }
    }

    /// Produce a `Plugin` instance for this frame. Clones only the references needed.
    pub fn plugin(&self) -> SentinelPlugin {
        SentinelPlugin {
            tiles: self.tiles.iter().map(|(k, v)| (*k, v.id())).collect(),
            tile_bounds: self.tile_bounds.clone(),
            visible: self.visible,
            opacity: self.opacity,
        }
    }
}

pub struct SentinelPlugin {
    tiles: HashMap<(u8, u32, u32), egui::TextureId>,
    tile_bounds: HashMap<(u8, u32, u32), GeoBounds>,
    visible: bool,
    opacity: f32,
}

impl walkers::Plugin for SentinelPlugin {
    fn run(
        self: Box<Self>,
        ui: &mut egui::Ui,
        response: &egui::Response,
        projector: &walkers::Projector,
        _map_memory: &walkers::MapMemory,
    ) {
        if !self.visible || self.tiles.is_empty() {
            return;
        }

        let painter = ui.painter();
        let tint = egui::Color32::from_rgba_unmultiplied(
            255,
            255,
            255,
            (self.opacity * 255.0) as u8,
        );

        for ((z, x, y), tex_id) in &self.tiles {
            let Some(bounds) = self.tile_bounds.get(&(*z, *x, *y)) else {
                continue;
            };
            let tl = project_lon_lat(projector, bounds.min_lon, bounds.max_lat);
            let br = project_lon_lat(projector, bounds.max_lon, bounds.min_lat);
            let rect = egui::Rect::from_two_pos(tl, br);

            if !response.rect.intersects(rect) {
                continue;
            }

            painter.image(
                *tex_id,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                tint,
            );
        }
    }
}

// ── Radar layer ───────────────────────────────────────────────────────────────

pub struct RadarLayer {
    pub visible: bool,
    pub opacity: f32,
    pub store: RadarFrameStore,
    textures: HashMap<usize, egui::TextureHandle>,
}

impl RadarLayer {
    pub fn new() -> Self {
        Self {
            visible: true,
            opacity: 0.6,
            store: RadarFrameStore::default(),
            textures: HashMap::new(),
        }
    }

    pub fn handle_event(&mut self, event: NexradEvent, ctx: &egui::Context) {
        match event {
            NexradEvent::FrameReady { timestamp, image } => {
                self.store.insert(timestamp, image);
                let idx = self.store.current_index;
                self.ensure_texture(idx, ctx);
            }
            NexradEvent::FrameError { timestamp, error } => {
                log::debug!("NEXRAD missing frame {timestamp}: {error}");
            }
            NexradEvent::FetchComplete => {
                log::info!("NEXRAD fetch complete — {} frames loaded", self.store.len());
            }
        }
    }

    pub fn set_frame(&mut self, index: usize) {
        if index < self.store.len() {
            self.store.current_index = index;
        }
    }

    fn ensure_texture(&mut self, index: usize, ctx: &egui::Context) {
        if self.textures.contains_key(&index) {
            return;
        }
        if let Some(frame) = self.store.frames.get(index) {
            let name = format!("nexrad_{}", frame.timestamp.timestamp());
            let tex = geo_image_to_texture(ctx, &name, &frame.image);
            self.textures.insert(index, tex);
        }
    }

    pub fn current_texture_id(&self) -> Option<egui::TextureId> {
        self.textures.get(&self.store.current_index).map(|t| t.id())
    }

    pub fn current_timestamp(&self) -> Option<DateTime<Utc>> {
        self.store.current().map(|f| f.timestamp)
    }

    pub fn plugin(&self) -> RadarPlugin {
        RadarPlugin {
            texture_id: self.current_texture_id(),
            visible: self.visible,
            opacity: self.opacity,
        }
    }

    pub fn frames(&self) -> &[crate::data::nexrad::RadarFrame] {
        &self.store.frames
    }
}

pub struct RadarPlugin {
    texture_id: Option<egui::TextureId>,
    visible: bool,
    opacity: f32,
}

impl walkers::Plugin for RadarPlugin {
    fn run(
        self: Box<Self>,
        ui: &mut egui::Ui,
        _response: &egui::Response,
        projector: &walkers::Projector,
        _map_memory: &walkers::MapMemory,
    ) {
        use crate::data::nexrad::NEXRAD_CONUS_BOUNDS;

        if !self.visible {
            return;
        }
        let Some(tex_id) = self.texture_id else {
            return;
        };

        let tl = project_lon_lat(
            projector,
            NEXRAD_CONUS_BOUNDS.min_lon,
            NEXRAD_CONUS_BOUNDS.max_lat,
        );
        let br = project_lon_lat(
            projector,
            NEXRAD_CONUS_BOUNDS.max_lon,
            NEXRAD_CONUS_BOUNDS.min_lat,
        );
        let rect = egui::Rect::from_two_pos(tl, br);

        let tint = egui::Color32::from_rgba_unmultiplied(
            255,
            255,
            255,
            (self.opacity * 255.0) as u8,
        );

        ui.painter().image(
            tex_id,
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            tint,
        );
    }
}

// ── MapView ───────────────────────────────────────────────────────────────────

/// The initial map center: geographic center of CONUS (Kansas).
const INITIAL_LON: f64 = -98.58;
const INITIAL_LAT: f64 = 38.53;
const INITIAL_ZOOM: f64 = 4.0;

pub struct MapView {
    tiles: walkers::HttpTiles,
    map_memory: walkers::MapMemory,
    /// Stored so we can pass a consistent `my_position` to `Map::new`.
    my_position: walkers::Position,
    pub sentinel_layer: SentinelLayer,
    pub radar_layer: RadarLayer,
}

impl MapView {
    pub fn new(ctx: &egui::Context) -> Self {
        let tiles =
            walkers::HttpTiles::new(walkers::sources::OpenStreetMap, ctx.clone());

        let mut map_memory = walkers::MapMemory::default();
        map_memory.set_zoom(INITIAL_ZOOM).ok();

        let my_position = walkers::lon_lat(INITIAL_LON, INITIAL_LAT);
        map_memory.center_at(my_position);

        Self {
            tiles,
            map_memory,
            my_position,
            sentinel_layer: SentinelLayer::new(),
            radar_layer: RadarLayer::new(),
        }
    }

    pub fn viewport(&self) -> MapViewport {
        let zoom = self.map_memory.zoom();
        // The visible position is either the user-dragged center or `my_position`.
        let center = self
            .map_memory
            .detached()
            .unwrap_or(self.my_position);
        let lon = center.x();
        let lat = center.y();

        // Approximate degree span based on zoom.
        let deg_span = 360.0 / 2f64.powi(zoom as i32);

        MapViewport {
            center_lon: lon,
            center_lat: lat,
            zoom,
            bounds: GeoBounds::new(
                lon - deg_span / 2.0,
                lat - deg_span / 4.0,
                lon + deg_span / 2.0,
                lat + deg_span / 4.0,
            ),
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui) {
        let sentinel_plugin = self.sentinel_layer.plugin();
        let radar_plugin = self.radar_layer.plugin();

        ui.add(
            walkers::Map::new(
                Some(&mut self.tiles),
                &mut self.map_memory,
                self.my_position,
            )
            .with_plugin(sentinel_plugin)
            .with_plugin(radar_plugin),
        );
    }
}
