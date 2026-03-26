use std::sync::Arc;

use winit::{
    application::ApplicationHandler,
    dpi::PhysicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowAttributes, WindowId},
};

use satellite_viewer::app::AppState;
use satellite_viewer::executor::Executor;
use satellite_viewer::renderer::{
    context::GpuContext,
    egui_pass::EguiPass,
    map_pipeline::MapPipeline,
    tile_cache::{TileCache, TileId},
    viewport::MapViewport,
};

fn main() {
    #[cfg(not(target_os = "ios"))]
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let event_loop = EventLoop::new().expect("create event loop");
    let mut handler = AppHandler::default();
    event_loop.run_app(&mut handler).expect("run event loop");
}

// ── Application handler ───────────────────────────────────────────────────────

#[derive(Default)]
struct AppHandler {
    state: Option<RunningState>,
}

impl ApplicationHandler for AppHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return; // already initialised (e.g. iOS resume after suspend)
        }
        let attrs = WindowAttributes::default()
            .with_title("Satellite Viewer")
            .with_inner_size(winit::dpi::LogicalSize::new(1280u32, 800u32))
            .with_min_inner_size(winit::dpi::LogicalSize::new(640u32, 480u32));

        let window =
            Arc::new(event_loop.create_window(attrs).expect("create window"));

        let state = RunningState::new(Arc::clone(&window));
        self.state = Some(state);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
        let Some(state) = &mut self.state else { return };

        // Let egui see the event first; if it consumes it, don't pass to map.
        let egui_consumed = state.egui.handle_event(&state.window, &event);

        match &event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                state.resize(*size);
            }

            WindowEvent::RedrawRequested => {
                state.render();
            }

            // ── Map interaction (only when egui didn't consume the event) ─────
            WindowEvent::CursorMoved { position, .. } if !egui_consumed => {
                state.cursor = (position.x, position.y);
                if state.drag_active {
                    let dx = state.cursor.0 - state.drag_start.0;
                    let dy = state.cursor.1 - state.drag_start.1;
                    state.drag_start = state.cursor;
                    state.viewport.pan_pixels(-dx, -dy);
                }
            }

            WindowEvent::MouseInput { state: btn_state, button: MouseButton::Left, .. }
                if !egui_consumed =>
            {
                state.drag_active = *btn_state == ElementState::Pressed;
                state.drag_start  = state.cursor;
            }

            WindowEvent::MouseWheel { delta, .. } if !egui_consumed => {
                let scroll = match delta {
                    MouseScrollDelta::LineDelta(_, y)  => *y as f64 * 0.3,
                    MouseScrollDelta::PixelDelta(p)    => p.y * 0.005,
                };
                let (cx, cy) = state.cursor;
                state.viewport.zoom_at(scroll, cx, cy);
            }

            _ => {}
        }

        state.window.request_redraw();
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(s) = &self.state {
            s.window.request_redraw();
        }
    }
}

// ── Running state ─────────────────────────────────────────────────────────────

struct RunningState {
    window:      Arc<Window>,
    gpu:         GpuContext,
    pipeline:    MapPipeline,
    tile_cache:  TileCache,
    egui:        EguiPass,
    viewport:    MapViewport,
    app:         AppState,

    // Pointer state for drag-to-pan.
    cursor:      (f64, f64),
    drag_active: bool,
    drag_start:  (f64, f64),
}

impl RunningState {
    fn new(window: Arc<Window>) -> Self {
        let gpu = pollster::block_on(GpuContext::new(Arc::clone(&window)))
            .expect("init wgpu");

        let pipeline = MapPipeline::new(&gpu);
        let egui     = EguiPass::new(&gpu, &window);

        // Repaint callback: tell winit to render a new frame.
        let window_weak = Arc::downgrade(&window);
        let repaint: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            if let Some(w) = window_weak.upgrade() {
                w.request_redraw();
            }
        });

        // Hand-rolled thread pool — replaces Tokio.
        // 8 workers: enough for concurrent tile downloads.
        let executor = Executor::new(8);

        let tile_cache = TileCache::new(Arc::clone(&executor), Arc::clone(&repaint));

        let (w, h) = gpu.size();
        let viewport = MapViewport::new(-98.58, 38.53, 4.0, w, h);

        let app = AppState::new(egui.ctx.clone(), Arc::clone(&executor));

        Self {
            window,
            gpu,
            pipeline,
            tile_cache,
            egui,
            viewport,
            app,
            cursor:      (0.0, 0.0),
            drag_active: false,
            drag_start:  (0.0, 0.0),
        }
    }

    fn resize(&mut self, size: PhysicalSize<u32>) {
        self.gpu.reconfigure(size);
        self.viewport.screen_w = size.width;
        self.viewport.screen_h = size.height;
    }

    fn render(&mut self) {
        // ── Drain incoming data events ────────────────────────────────────────
        self.app.process_events(&self.gpu, &self.pipeline);

        // ── Flush tile uploads ────────────────────────────────────────────────
        self.tile_cache.flush(&self.gpu, &self.pipeline);

        // ── Request visible OSM tiles ─────────────────────────────────────────
        let z = self.viewport.tile_zoom();
        for (x, y) in self.viewport.visible_tiles() {
            self.tile_cache.request(TileId { z, x, y });
        }

        // ── Acquire swap-chain frame ──────────────────────────────────────────
        let frame = match self.gpu.surface.get_current_texture() {
            Ok(f) => f,
            Err(wgpu::SurfaceError::Outdated | wgpu::SurfaceError::Lost) => {
                let size = self.window.inner_size();
                self.gpu.reconfigure(size);
                return;
            }
            Err(e) => {
                log::error!("surface error: {e}");
                return;
            }
        };

        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.gpu.device.create_command_encoder(
            &wgpu::CommandEncoderDescriptor { label: Some("frame encoder") },
        );

        // ── Map render pass ───────────────────────────────────────────────────
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("map pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view:           &view,
                    resolve_target: None,
                    depth_slice:    None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.15, g: 0.15, b: 0.15, a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });

            // 1 — OSM base-map tiles
            let opaque_params = self.pipeline.params_bind_group(&self.gpu, 1.0);
            for (x, y) in self.viewport.visible_tiles() {
                let id = TileId { z, x, y };
                let Some(tex_bg) = self.tile_cache.get(&id) else { continue };
                let corners = self.viewport.tile_ndc_quad(z, x, y);
                self.pipeline.draw_quad(
                    &self.gpu, &mut rp, corners, tex_bg, &opaque_params,
                );
            }

            // 2 — Sentinel-2 overlay tiles
            if self.app.sentinel_layer.visible {
                let sentinel_params = self.pipeline.params_bind_group(
                    &self.gpu,
                    self.app.sentinel_layer.opacity,
                );
                for (key, (bounds, tex_bg)) in &self.app.sentinel_layer.tiles {
                    let corners = self.viewport.bounds_ndc_quad(
                        bounds.min_lon, bounds.min_lat,
                        bounds.max_lon, bounds.max_lat,
                    );
                    self.pipeline
                        .draw_quad(&self.gpu, &mut rp, corners, tex_bg, &sentinel_params);
                    let _ = key; // suppress unused warning
                }
            }

            // 3 — NEXRAD radar overlay
            if self.app.radar_layer.visible {
                if let Some((bounds, tex_bg)) = self.app.radar_layer.current_frame_gpu() {
                    let radar_params = self.pipeline.params_bind_group(
                        &self.gpu,
                        self.app.radar_layer.opacity,
                    );
                    let corners = self.viewport.bounds_ndc_quad(
                        bounds.min_lon, bounds.min_lat,
                        bounds.max_lon, bounds.max_lat,
                    );
                    self.pipeline
                        .draw_quad(&self.gpu, &mut rp, corners, tex_bg, &radar_params);
                }
            }
        }

        // 4 — egui UI (menu, playback bar, sidebar) drawn on top of the map.
        let _vp = self.viewport.as_data_viewport();
        self.egui.render(
            &self.gpu,
            &mut encoder,
            &view,
            &self.window,
            |ctx| self.app.show_ui(ctx, &mut self.viewport),
        );

        // ── Kick off data fetches for the current viewport ────────────────────
        self.app.maybe_fetch(&self.viewport);

        self.gpu.queue.submit([encoder.finish()]);
        frame.present();
    }
}
