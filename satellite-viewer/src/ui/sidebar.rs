/// Settings sidebar.

use crate::app::{RadarGpuLayer, SentinelGpuLayer};
use crate::data::{nexrad::NexradClient, sentinel::SentinelClient};
use crate::ui::playback::PlaybackPanel;

#[derive(Default)]
pub struct Sidebar {
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
        sentinel_layer: &mut SentinelGpuLayer,
        radar_layer:    &mut RadarGpuLayer,
        playback:       &mut PlaybackPanel,
        nexrad_client:  &NexradClient,
        sentinel_client: &SentinelClient,
    ) {
        ui.add_space(4.0);
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.open_section, SidebarSection::Layers, "Layers");
            ui.selectable_value(&mut self.open_section, SidebarSection::Radar, "Radar");
            ui.selectable_value(&mut self.open_section, SidebarSection::Satellite, "Satellite");
            ui.selectable_value(&mut self.open_section, SidebarSection::About, "About");
        });
        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            match self.open_section {
                SidebarSection::Layers    => self.layers(ui, sentinel_layer, radar_layer),
                SidebarSection::Radar     => self.radar(ui, playback, nexrad_client),
                SidebarSection::Satellite => self.satellite(ui, sentinel_layer, sentinel_client),
                SidebarSection::About     => self.about(ui),
            }
        });
    }

    fn layers(
        &self,
        ui: &mut egui::Ui,
        sentinel: &mut SentinelGpuLayer,
        radar:    &mut RadarGpuLayer,
    ) {
        egui::CollapsingHeader::new("🛰  Sentinel-2").default_open(true).show(ui, |ui| {
            ui.checkbox(&mut sentinel.visible, "Visible");
            ui.add(egui::Slider::new(&mut sentinel.opacity, 0.0..=1.0).text("Opacity"));
        });
        ui.add_space(4.0);
        egui::CollapsingHeader::new("🌀  NEXRAD radar").default_open(true).show(ui, |ui| {
            ui.checkbox(&mut radar.visible, "Visible");
            ui.add(egui::Slider::new(&mut radar.opacity, 0.0..=1.0).text("Opacity"));
        });
    }

    fn radar(&self, ui: &mut egui::Ui, playback: &mut PlaybackPanel, client: &NexradClient) {
        ui.heading("NEXRAD settings");
        ui.add_space(4.0);
        ui.label("Time range (hours back):");
        ui.add(
            egui::Slider::new(&mut playback.hours_back, 1..=12)
                .text("hours")
                .clamping(egui::SliderClamping::Always),
        );
        ui.add_space(8.0);
        ui.label(format!("Status: {}", if client.is_fetching() { "Fetching…" } else { "Ready" }));
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "Data: Iowa Environmental Mesonet NEXRAD composites\n\
                 Product: n0r (base reflectivity, 5-min)\n\
                 Coverage: CONUS",
            ).small(),
        );
    }

    fn satellite(
        &self,
        ui: &mut egui::Ui,
        layer: &SentinelGpuLayer,
        client: &SentinelClient,
    ) {
        ui.heading("Sentinel-2 info");
        ui.add_space(4.0);
        egui::Grid::new("sentinel_meta").num_columns(2).striped(true).show(ui, |ui| {
            ui.label("Scene ID"); ui.label(layer.scene_id.as_deref().unwrap_or("—")); ui.end_row();
            ui.label("Date"); ui.label(layer.scene_datetime.as_deref().unwrap_or("—")); ui.end_row();
            ui.label("Cloud cover");
            ui.label(layer.cloud_cover.map(|c| format!("{c:.1}%")).unwrap_or_else(|| "—".into()));
            ui.end_row();
            ui.label("Status");
            ui.label(if client.is_fetching() { "Fetching…" } else { "Idle" });
            ui.end_row();
        });
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "Data: Sentinel-2 L2A via Element84 STAC API\n\
                 Tiles: TiTiler (public instance)\n\
                 Bands: B04/B03/B02 (true colour)",
            ).small(),
        );
    }

    fn about(&self, ui: &mut egui::Ui) {
        ui.heading("Satellite Viewer");
        ui.add_space(4.0);
        ui.label("Cross-platform satellite + NEXRAD radar viewer.");
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Renderer").strong());
        ui.label("• Homebrew wgpu pipeline (WGSL shaders)");
        ui.label("• winit event loop — no eframe");
        ui.label("• egui UI layer on top");
        ui.add_space(8.0);
        ui.label(egui::RichText::new("Data sources").strong());
        ui.label("• Sentinel-2 L2A — Copernicus / ESA / AWS");
        ui.label("• NEXRAD composites — Iowa Environmental Mesonet");
        ui.label("• Base map — © OpenStreetMap contributors");
        ui.add_space(8.0);
        ui.hyperlink_to("Source code", "https://github.com/madjayq/crackhead-tools");
    }
}
