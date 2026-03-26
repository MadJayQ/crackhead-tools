use std::sync::mpsc;

use crate::data::{
    nexrad::{NexradClient, NexradEvent},
    sentinel::{SentinelClient, SentinelEvent},
};
use crate::ui::{
    map_view::MapView,
    playback::PlaybackPanel,
    sidebar::Sidebar,
};

/// Top-level events flowing from background tasks back to the UI thread.
pub enum AppEvent {
    Nexrad(NexradEvent),
    Sentinel(SentinelEvent),
}

/// Persistent application state, owned by the egui/eframe render loop.
pub struct SatelliteViewerApp {
    // ── Background runtime ────────────────────────────────────────────────────
    runtime: tokio::runtime::Runtime,
    /// Kept alive so background tasks can send events even after being spawned.
    _event_tx: mpsc::SyncSender<AppEvent>,
    event_rx: mpsc::Receiver<AppEvent>,

    // ── Data clients ──────────────────────────────────────────────────────────
    nexrad_client: NexradClient,
    sentinel_client: SentinelClient,

    // ── UI panels ─────────────────────────────────────────────────────────────
    map_view: MapView,
    playback_panel: PlaybackPanel,
    sidebar: Sidebar,

    // ── Window state ──────────────────────────────────────────────────────────
    show_sidebar: bool,
}

impl SatelliteViewerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Install the image loader so egui_extras can decode PNG/JPEG bytes
        // into egui textures.
        egui_extras::install_image_loaders(&cc.egui_ctx);

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build Tokio runtime");

        // A bounded channel avoids unbounded memory growth when the network
        // is fast but the UI is slow.
        let (event_tx, event_rx) = mpsc::sync_channel(64);

        let nexrad_client = NexradClient::new(event_tx.clone(), cc.egui_ctx.clone());
        let sentinel_client = SentinelClient::new(event_tx.clone(), cc.egui_ctx.clone());

        let map_view = MapView::new(&cc.egui_ctx);

        Self {
            runtime,
            _event_tx: event_tx,
            event_rx,
            nexrad_client,
            sentinel_client,
            map_view,
            playback_panel: PlaybackPanel::default(),
            sidebar: Sidebar::default(),
            show_sidebar: true,
        }
    }

    /// Drain the event channel and apply incoming data to the relevant panels.
    fn process_events(&mut self, ctx: &egui::Context) {
        while let Ok(event) = self.event_rx.try_recv() {
            match event {
                AppEvent::Nexrad(e) => {
                    self.map_view.radar_layer.handle_event(e, ctx);
                    self.playback_panel
                        .sync_from_radar(&self.map_view.radar_layer);
                }
                AppEvent::Sentinel(e) => {
                    self.map_view.sentinel_layer.handle_event(e, ctx);
                }
            }
        }
    }

    /// Advance the radar playback clock and request new frames when needed.
    fn tick_playback(&mut self, ctx: &egui::Context) {
        let Some(next_frame) = self.playback_panel.tick() else {
            return;
        };
        self.map_view.radar_layer.set_frame(next_frame);
        ctx.request_repaint();
    }

    /// Kick off background fetches when the viewport has moved far enough that
    /// we need new data.
    fn maybe_fetch_new_data(&mut self) {
        let viewport = self.map_view.viewport();

        // Sentinel — fetch the best available scene for the current viewport.
        if self.sentinel_client.needs_refresh(&viewport) {
            let client = self.sentinel_client.clone();
            let vp = viewport.clone();
            self.runtime.spawn(async move {
                if let Err(e) = client.fetch_scene(vp).await {
                    log::warn!("Sentinel fetch failed: {e}");
                }
            });
        }

        // NEXRAD — keep the ring-buffer of composite frames up to date.
        if self.nexrad_client.needs_refresh() {
            let client = self.nexrad_client.clone();
            let range = self.playback_panel.time_range();
            self.runtime.spawn(async move {
                if let Err(e) = client.fetch_frames(range).await {
                    log::warn!("NEXRAD fetch failed: {e}");
                }
            });
        }
    }
}

impl eframe::App for SatelliteViewerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 1. Pull results from background tasks.
        self.process_events(ctx);

        // 2. Advance playback and fetch missing data.
        self.tick_playback(ctx);
        self.maybe_fetch_new_data();

        // 3. Top menu bar.
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
                    ui.checkbox(
                        &mut self.map_view.sentinel_layer.visible,
                        "Sentinel-2 overlay",
                    );
                    ui.checkbox(
                        &mut self.map_view.radar_layer.visible,
                        "NEXRAD radar overlay",
                    );
                });
                ui.menu_button("Help", |ui| {
                    if ui.button("About").clicked() {
                        // TODO: open about window
                    }
                });

                // Right-aligned status indicator.
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.nexrad_client.is_fetching() {
                        ui.spinner();
                        ui.label("Fetching radar…");
                    } else if self.sentinel_client.is_fetching() {
                        ui.spinner();
                        ui.label("Fetching satellite…");
                    }
                });
            });
        });

        // 4. Radar playback bar — docked to the bottom.
        egui::TopBottomPanel::bottom("playback_bar")
            .resizable(false)
            .min_height(80.0)
            .show(ctx, |ui| {
                self.playback_panel.show(ui, &mut self.map_view.radar_layer);
            });

        // 5. Settings sidebar — docked to the right.
        if self.show_sidebar {
            egui::SidePanel::right("sidebar")
                .resizable(true)
                .default_width(280.0)
                .show(ctx, |ui| {
                    self.sidebar.show(
                        ui,
                        &mut self.map_view,
                        &mut self.playback_panel,
                        &self.nexrad_client,
                        &self.sentinel_client,
                    );
                });
        }

        // 6. Central map view — fills whatever space remains.
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ctx, |ui| {
                self.map_view.show(ui);
            });
    }
}
