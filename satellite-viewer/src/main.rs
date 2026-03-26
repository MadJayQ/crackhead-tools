// Desktop entry point.
// On iOS the library entry point `satellite_viewer_main` (in lib.rs) is used
// instead; the AppDelegate calls it after the UIWindow is ready.

fn main() -> anyhow::Result<()> {
    #[cfg(not(target_os = "ios"))]
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Satellite Viewer")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([800.0, 600.0]),
        // Use the wgpu renderer so the same code path runs on Windows (DX12/Vulkan)
        // and iOS (Metal) — no conditional compilation needed.
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    eframe::run_native(
        "Satellite Viewer",
        native_options,
        Box::new(|cc| Ok(Box::new(satellite_viewer::app::SatelliteViewerApp::new(cc)))),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}
