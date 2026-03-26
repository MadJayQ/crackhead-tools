// Re-export modules so both the desktop binary and the iOS staticlib can
// reach the same types.

pub mod app;
pub mod data;
pub mod ui;

// ── iOS entry point ────────────────────────────────────────────────────────────
// Your Swift/ObjC AppDelegate calls this after the UIWindow is ready.
// Compile with: cargo build --target aarch64-apple-ios --release
//
// Xcode setup:
//   1. Add the .a file produced in target/aarch64-apple-ios/release/ to
//      your Xcode project's "Link Binary With Libraries" build phase.
//   2. In your AppDelegate: extern "C" { fn satellite_viewer_main(); }
//      and call it from applicationDidFinishLaunching.
#[cfg(target_os = "ios")]
#[no_mangle]
pub extern "C" fn satellite_viewer_main() {
    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };
    eframe::run_native(
        "Satellite Viewer",
        native_options,
        Box::new(|cc| Ok(Box::new(app::SatelliteViewerApp::new(cc)))),
    )
    .expect("failed to start eframe on iOS");
}
