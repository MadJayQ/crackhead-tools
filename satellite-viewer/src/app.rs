/// Application state — holds all data layers and drives the egui UI.
///
/// This struct is no longer tied to `eframe`.  It is updated from
/// `RunningState::render()` each frame.

use std::collections::HashMap;
use std::sync::{mpsc, Arc};
use crate::executor::Executor;

use crate::data::{
    nexrad::{NexradClient, NexradEvent, RadarFrameStore},
    sentinel::{SentinelClient, SentinelEvent},
    GeoBounds, GeoImage,
};
use crate::renderer::{context::GpuContext, map_pipeline::MapPipeline, viewport::MapViewport};
use crate::ui::{playback::PlaybackPanel, sidebar::Sidebar};

// ── AppEvent ──────────────────────────────────────────────────────────────────

pub enum AppEvent {
    Nexrad(NexradEvent),
    Sentinel(SentinelEvent),
}

// ── GPU image layers ──────────────────────────────────────────────────────────

/// Holds decoded pixel data + lazily-uploaded wgpu bind groups for the
/// Sentinel-2 tile overlay.
pub struct SentinelGpuLayer {
    pub visible: bool,
    pub opacity: f32,
    pub scene_id:       Option<String>,
    pub cloud_cover:    Option<f64>,
    pub scene_datetime: Option<String>,
    /// Key: (z, x, y).  Value: (geographic bounds, texture bind group).
    pub tiles: HashMap<(u8, u32, u32), (GeoBounds, wgpu::BindGroup)>,
    /// Decoded images waiting to be uploaded on the GPU thread.
    pending: Vec<((u8, u32, u32), GeoImage)>,
}

impl SentinelGpuLayer {
    fn new() -> Self {
        Self {
            visible:        true,
            opacity:        0.8,
            scene_id:       None,
            cloud_cover:    None,
            scene_datetime: None,
            tiles:          HashMap::new(),
            pending:        Vec::new(),
        }
    }

    fn enqueue(&mut self, key: (u8, u32, u32), image: GeoImage) {
        self.pending.push((key, image));
    }

    /// Upload any pending images as wgpu textures (call from the GPU thread).
    pub fn flush(&mut self, gpu: &GpuContext, pipeline: &MapPipeline) {
        for (key, image) in self.pending.drain(..) {
            let (z, x, y) = key;
            let label = format!("sentinel_{z}_{x}_{y}");
            let texture = pipeline.upload_texture(
                gpu, &label, &image.pixels, image.width, image.height,
            );
            let view = texture.create_view(&Default::default());
            let bg = pipeline.texture_bind_group(gpu, &view);
            self.tiles.insert(key, (image.bounds, bg));
        }
    }
}

/// Holds NEXRAD composite frames + lazily-uploaded wgpu bind groups.
pub struct RadarGpuLayer {
    pub visible: bool,
    pub opacity: f32,
    pub store:   RadarFrameStore,
    /// GPU textures, keyed by frame index.
    textures: HashMap<usize, (GeoBounds, wgpu::BindGroup)>,
    /// Decoded images waiting for GPU upload.
    pending: Vec<(usize, GeoImage)>,
}

impl RadarGpuLayer {
    fn new() -> Self {
        Self {
            visible:  true,
            opacity:  0.6,
            store:    RadarFrameStore::default(),
            textures: HashMap::new(),
            pending:  Vec::new(),
        }
    }

    fn handle_event(&mut self, event: NexradEvent) {
        match event {
            NexradEvent::FrameReady { timestamp, image } => {
                self.store.insert(timestamp, image.clone());
                // Enqueue the texture upload for the frame we just inserted.
                let idx = self.store.len() - 1;
                self.pending.push((idx, image));
            }
            NexradEvent::FrameError { timestamp, error } => {
                log::debug!("NEXRAD {timestamp}: {error}");
            }
            NexradEvent::FetchComplete => {
                log::info!("NEXRAD fetch done — {} frames", self.store.len());
            }
        }
    }

    /// Upload pending frames as wgpu textures (GPU thread).
    pub fn flush(&mut self, gpu: &GpuContext, pipeline: &MapPipeline) {
        for (idx, image) in self.pending.drain(..) {
            let Some(frame) = self.store.frames.get(idx) else { continue };
            let label = format!("nexrad_{}", frame.timestamp.timestamp());
            let texture = pipeline.upload_texture(
                gpu, &label, &image.pixels, image.width, image.height,
            );
            let view = texture.create_view(&Default::default());
            let bg = pipeline.texture_bind_group(gpu, &view);
            self.textures.insert(idx, (image.bounds, bg));
        }
    }

    /// Returns `(bounds, bind_group)` for the current frame, if available.
    pub fn current_frame_gpu(&self) -> Option<(&GeoBounds, &wgpu::BindGroup)> {
        self.textures
            .get(&self.store.current_index)
            .map(|(b, bg)| (b, bg))
    }

    pub fn frames(&self) -> &[crate::data::nexrad::RadarFrame] {
        &self.store.frames
    }
}

// ── AppState ──────────────────────────────────────────────────────────────────

pub struct AppState {
    #[allow(dead_code)]
    executor:         Arc<Executor>,  // kept alive to drive data-fetch workers
    _event_tx:        mpsc::SyncSender<AppEvent>,
    event_rx:         mpsc::Receiver<AppEvent>,
    nexrad_client:    NexradClient,
    sentinel_client:  SentinelClient,

    pub sentinel_layer: SentinelGpuLayer,
    pub radar_layer:    RadarGpuLayer,

    playback:   PlaybackPanel,
    sidebar:    Sidebar,
    show_sidebar: bool,
}

impl AppState {
    pub fn new(
        egui_ctx: egui::Context,
        executor: Arc<Executor>,
    ) -> Self {
        let (tx, rx) = mpsc::sync_channel::<AppEvent>(64);

        let nexrad_client   = NexradClient::new(tx.clone(), egui_ctx.clone());
        let sentinel_client = SentinelClient::new(tx.clone(), egui_ctx.clone());

        Self {
            executor,
            _event_tx: tx,
            event_rx: rx,
            nexrad_client,
            sentinel_client,
            sentinel_layer: SentinelGpuLayer::new(),
            radar_layer:    RadarGpuLayer::new(),
            playback:       PlaybackPanel::default(),
            sidebar:        Sidebar::default(),
            show_sidebar:   true,
        }
    }

    // ── Events ────────────────────────────────────────────────────────────────

    /// Drain the event channel and upload ready images.
    pub fn process_events(&mut self, gpu: &GpuContext, pipeline: &MapPipeline) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                AppEvent::Nexrad(e) => self.radar_layer.handle_event(e),
                AppEvent::Sentinel(e) => match e {
                    SentinelEvent::SceneFound {
                        scene_id, cloud_cover, datetime, ..
                    } => {
                        self.sentinel_layer.scene_id       = Some(scene_id);
                        self.sentinel_layer.cloud_cover    = Some(cloud_cover);
                        self.sentinel_layer.scene_datetime = Some(datetime);
                        self.sentinel_layer.tiles.clear();
                    }
                    SentinelEvent::TileReady { z, x, y, image } => {
                        self.sentinel_layer.enqueue((z, x, y), image);
                    }
                    SentinelEvent::Error(e) => log::warn!("Sentinel: {e}"),
                },
            }
        }
        self.sentinel_layer.flush(gpu, pipeline);
        self.radar_layer.flush(gpu, pipeline);
    }

    // ── Data fetching ─────────────────────────────────────────────────────────

    pub fn maybe_fetch(&self, viewport: &MapViewport) {
        let data_vp = viewport.as_data_viewport();

        if self.sentinel_client.needs_refresh(&data_vp) {
            self.sentinel_client.fetch_scene(data_vp);
        }

        if self.nexrad_client.needs_refresh() {
            let range = self.playback.time_range();
            self.nexrad_client.fetch_frames(range);
        }
    }

    // ── UI ────────────────────────────────────────────────────────────────────

    /// Draw the egui UI for this frame.
    pub fn show_ui(&mut self, ctx: &egui::Context, viewport: &mut MapViewport) {
        // Top menu bar.
        egui::TopBottomPanel::top("menu_bar").show(ctx, |ui| {
            egui::MenuBar::new().ui(ui, |ui| {
                ui.menu_button("File", |ui| {
                    if ui.button("Quit").clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
                ui.menu_button("View", |ui| {
                    ui.checkbox(&mut self.show_sidebar, "Settings panel");
                    ui.separator();
                    ui.checkbox(&mut self.sentinel_layer.visible, "Sentinel-2 overlay");
                    ui.checkbox(&mut self.radar_layer.visible, "NEXRAD radar overlay");
                });
                // Right-aligned spinner.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.nexrad_client.is_fetching() {
                        ui.spinner(); ui.label("Fetching radar…");
                    } else if self.sentinel_client.is_fetching() {
                        ui.spinner(); ui.label("Fetching satellite…");
                    }
                });
            });
        });

        // Playback bar at the bottom.
        egui::TopBottomPanel::bottom("playback_bar")
            .resizable(false)
            .min_height(80.0)
            .show(ctx, |ui| {
                self.playback.show(ui, &mut self.radar_layer);
            });

        // Settings sidebar.
        if self.show_sidebar {
            egui::SidePanel::right("sidebar")
                .default_width(280.0)
                .show(ctx, |ui| {
                    self.sidebar.show(
                        ui,
                        &mut self.sentinel_layer,
                        &mut self.radar_layer,
                        &mut self.playback,
                        &self.nexrad_client,
                        &self.sentinel_client,
                    );
                });
        }

        // Map coordinate display (bottom-left overlay on the central area).
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                // The map itself is rendered by our wgpu pipeline, not egui.
                // We only overlay the coordinate readout here.
                let rect = ui.available_rect_before_wrap();
                let painter = ui.painter_at(rect);
                let coord_text = format!(
                    "{:.4}°N  {:.4}°E  z{:.1}",
                    viewport.center_lat, viewport.center_lon, viewport.zoom
                );
                painter.text(
                    rect.left_bottom() + egui::vec2(8.0, -8.0),
                    egui::Align2::LEFT_BOTTOM,
                    coord_text,
                    egui::FontId::monospace(12.0),
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 200),
                );
                // Attribution.
                painter.text(
                    rect.right_bottom() + egui::vec2(-8.0, -8.0),
                    egui::Align2::RIGHT_BOTTOM,
                    "© OpenStreetMap contributors",
                    egui::FontId::proportional(11.0),
                    egui::Color32::from_rgba_unmultiplied(255, 255, 255, 180),
                );
            });
    }
}
