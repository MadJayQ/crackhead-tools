/// egui render pass — integrates `egui-wgpu` and `egui-winit`.
///
/// Responsibilities:
///   • Translates winit `WindowEvent`s into egui input via `egui_winit::State`.
///   • Runs the egui frame (`Context::run`) with the provided UI closure.
///   • Tessellates the output and renders it into a `wgpu::RenderPass` that
///     draws *on top* of the map (LoadOp::Load, not Clear).
///   • Manages egui texture uploads/frees on the `egui_wgpu::Renderer`.

use std::sync::Arc;

use winit::{event::WindowEvent, window::Window};

use super::context::GpuContext;

pub struct EguiPass {
    pub ctx:      egui::Context,
    state:        egui_winit::State,
    renderer:     egui_wgpu::Renderer,
}

impl EguiPass {
    pub fn new(gpu: &GpuContext, window: &Arc<Window>) -> Self {
        let ctx = egui::Context::default();

        // Install image loaders so egui_extras can decode PNG/JPEG in the UI.
        egui_extras::install_image_loaders(&ctx);

        let state = egui_winit::State::new(
            ctx.clone(),
            egui::ViewportId::ROOT,
            window.as_ref(),
            Some(window.scale_factor() as f32),
            None,
            None,
        );

        let renderer = egui_wgpu::Renderer::new(
            &gpu.device,
            gpu.surface_format,
            egui_wgpu::RendererOptions::default(),
        );

        Self { ctx, state, renderer }
    }

    // ── Input ─────────────────────────────────────────────────────────────────

    /// Feed a winit window event to egui.  Returns `true` if egui consumed it
    /// (i.e. the map should ignore it — e.g. clicking on a button).
    pub fn handle_event(&mut self, window: &Window, event: &WindowEvent) -> bool {
        self.state.on_window_event(window, event).consumed
    }

    // ── Render ────────────────────────────────────────────────────────────────

    /// Run `build_ui`, then render the resulting primitives.
    ///
    /// Uses `LoadOp::Load` so the map drawn earlier is preserved.
    pub fn render(
        &mut self,
        gpu: &GpuContext,
        encoder: &mut wgpu::CommandEncoder,
        surface_view: &wgpu::TextureView,
        window: &Window,
        build_ui: impl FnMut(&egui::Context),
    ) {
        // ── Build frame ───────────────────────────────────────────────────────
        let raw_input = self.state.take_egui_input(window);
        let full_output = self.ctx.run(raw_input, build_ui);

        self.state
            .handle_platform_output(window, full_output.platform_output);

        // ── Upload textures ───────────────────────────────────────────────────
        for (tex_id, delta) in &full_output.textures_delta.set {
            self.renderer
                .update_texture(&gpu.device, &gpu.queue, *tex_id, delta);
        }

        // ── Tessellate ────────────────────────────────────────────────────────
        let ppp = full_output.pixels_per_point;
        let clipped = self.ctx.tessellate(full_output.shapes, ppp);

        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels:  [gpu.surface_config.width, gpu.surface_config.height],
            pixels_per_point: ppp,
        };

        self.renderer
            .update_buffers(&gpu.device, &gpu.queue, encoder, &clipped, &screen);

        // ── Render pass (draws on top of map) ─────────────────────────────────
        // `egui_wgpu::Renderer::render` requires `RenderPass<'static>`.
        // We use `forget_lifetime()` here which is safe because the render
        // pass is dropped (via `drop(rp)`) before the encoder is submitted.
        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view:           surface_view,
                    resolve_target: None,
                    depth_slice:    None,
                    ops: wgpu::Operations {
                        load:  wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                occlusion_query_set:      None,
                timestamp_writes:         None,
            }).forget_lifetime();
            self.renderer.render(&mut rp, &clipped, &screen);
            drop(rp);
        }

        // ── Free unused textures ──────────────────────────────────────────────
        for tex_id in &full_output.textures_delta.free {
            self.renderer.free_texture(tex_id);
        }
    }
}
