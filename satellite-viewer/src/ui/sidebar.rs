/// Settings / info sidebar.
///
/// Houses controls that don't need to be permanently visible:
///   • Layer opacity sliders
///   • Sentinel-2 scene metadata
///   • NEXRAD time-range picker
///   • Map provider selector (future)
///   • Attribution / about

use crate::data::{nexrad::NexradClient, sentinel::SentinelClient};
use crate::ui::{map_view::MapView, playback::PlaybackPanel};

#[derive(Default)]
pub struct Sidebar {
    /// Which section of the sidebar is currently open.
    open_section: SidebarSection,
}

#[derive(Default, PartialEq, Clone, Copy)]
enum SidebarSection {
    #[default]
    Layers,
    Radar,
    Satellite,
    About,
}

impl Sidebar {
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        map_view: &mut MapView,
        playback: &mut PlaybackPanel,
        nexrad_client: &NexradClient,
        sentinel_client: &SentinelClient,
    ) {
        ui.add_space(4.0);

        // ── Tab bar ───────────────────────────────────────────────────────────
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.open_section, SidebarSection::Layers, "Layers");
            ui.selectable_value(&mut self.open_section, SidebarSection::Radar, "Radar");
            ui.selectable_value(
                &mut self.open_section,
                SidebarSection::Satellite,
                "Satellite",
            );
            ui.selectable_value(&mut self.open_section, SidebarSection::About, "About");
        });

        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            match self.open_section {
                SidebarSection::Layers => {
                    self.show_layers(ui, map_view);
                }
                SidebarSection::Radar => {
                    self.show_radar(ui, map_view, playback, nexrad_client);
                }
                SidebarSection::Satellite => {
                    self.show_satellite(ui, map_view, sentinel_client);
                }
                SidebarSection::About => {
                    self.show_about(ui);
                }
            }
        });
    }

    // ── Layers tab ────────────────────────────────────────────────────────────

    fn show_layers(&self, ui: &mut egui::Ui, map_view: &mut MapView) {
        egui::CollapsingHeader::new("🛰  Sentinel-2")
            .default_open(true)
            .show(ui, |ui| {
                ui.checkbox(&mut map_view.sentinel_layer.visible, "Visible");
                ui.add(
                    egui::Slider::new(&mut map_view.sentinel_layer.opacity, 0.0..=1.0)
                        .text("Opacity"),
                );
            });

        ui.add_space(4.0);

        egui::CollapsingHeader::new("🌀  NEXRAD radar")
            .default_open(true)
            .show(ui, |ui| {
                ui.checkbox(&mut map_view.radar_layer.visible, "Visible");
                ui.add(
                    egui::Slider::new(&mut map_view.radar_layer.opacity, 0.0..=1.0)
                        .text("Opacity"),
                );
            });
    }

    // ── Radar tab ─────────────────────────────────────────────────────────────

    fn show_radar(
        &self,
        ui: &mut egui::Ui,
        _map_view: &mut MapView,
        playback: &mut PlaybackPanel,
        nexrad_client: &NexradClient,
    ) {
        ui.heading("NEXRAD settings");
        ui.add_space(4.0);

        ui.label("Time range (hours back):");
        ui.add(
            egui::Slider::new(&mut playback.hours_back, 1..=12)
                .text("hours")
                .clamping(egui::SliderClamping::Always),
        );

        ui.add_space(8.0);

        let status = if nexrad_client.is_fetching() {
            "Fetching…"
        } else {
            "Ready"
        };
        ui.label(format!("Status: {status}"));

        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "Data: Iowa Environmental Mesonet NEXRAD composites\n\
                 Product: n0r (base reflectivity, 5-min)\n\
                 Coverage: CONUS",
            )
            .small(),
        );
    }

    // ── Satellite tab ─────────────────────────────────────────────────────────

    fn show_satellite(
        &self,
        ui: &mut egui::Ui,
        map_view: &mut MapView,
        sentinel_client: &SentinelClient,
    ) {
        ui.heading("Sentinel-2 info");
        ui.add_space(4.0);

        let sl = &map_view.sentinel_layer;

        egui::Grid::new("sentinel_meta")
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                ui.label("Scene ID");
                ui.label(sl.scene_id.as_deref().unwrap_or("—"));
                ui.end_row();

                ui.label("Date");
                ui.label(sl.scene_datetime.as_deref().unwrap_or("—"));
                ui.end_row();

                ui.label("Cloud cover");
                ui.label(
                    sl.cloud_cover
                        .map(|c| format!("{c:.1}%"))
                        .unwrap_or_else(|| "—".into()),
                );
                ui.end_row();

                ui.label("Status");
                ui.label(if sentinel_client.is_fetching() {
                    "Fetching…"
                } else {
                    "Idle"
                });
                ui.end_row();
            });

        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "Data: Sentinel-2 L2A via Element84 STAC API\n\
                 Tiles: TiTiler (public instance)\n\
                 Bands: B04/B03/B02 (true colour)",
            )
            .small(),
        );
    }

    // ── About tab ─────────────────────────────────────────────────────────────

    fn show_about(&self, ui: &mut egui::Ui) {
        ui.heading("Satellite Viewer");
        ui.add_space(4.0);
        ui.label("Cross-platform satellite image viewer with live NEXRAD radar overlay.");
        ui.add_space(8.0);

        ui.label(egui::RichText::new("Data sources").strong());
        ui.label("• Sentinel-2 L2A — Copernicus / ESA / AWS");
        ui.label("• NEXRAD composites — Iowa Environmental Mesonet");
        ui.label("• Base map — © OpenStreetMap contributors");
        ui.add_space(8.0);

        ui.label(egui::RichText::new("Built with").strong());
        ui.label("• Rust + egui + eframe (wgpu)");
        ui.label("• walkers (map widget)");
        ui.label("• reqwest + tokio");
        ui.add_space(8.0);

        ui.hyperlink_to(
            "Source code",
            "https://github.com/madjayq/crackhead-tools",
        );
    }
}
