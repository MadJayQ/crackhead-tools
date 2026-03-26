pub mod app;
pub mod executor;
pub mod data;
pub mod renderer;
pub mod ui;

/// iOS entry point.
/// Called by the Swift AppDelegate after `UIApplication` finishes launching.
/// Build: `cargo build --target aarch64-apple-ios --release`
#[cfg(target_os = "ios")]
#[no_mangle]
pub extern "C" fn satellite_viewer_main() {
    use std::sync::Arc;
    use winit::{application::ApplicationHandler, event_loop::EventLoop};

    // Reuse the same ApplicationHandler from main.rs.
    // On iOS, winit drives the run loop via UIKit.
    let event_loop = EventLoop::new().expect("create event loop");
    let mut handler = crate::main_handler::AppHandler::default();
    event_loop.run_app(&mut handler).expect("run app");
}
