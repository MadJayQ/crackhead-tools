/// `GpuContext` — owns the wgpu `Instance`, `Surface`, `Device`, and `Queue`.
///
/// Created once at startup via `GpuContext::new()` (async, use `pollster::block_on`).
/// After window resize call `GpuContext::reconfigure()`.

use std::sync::Arc;

use anyhow::{Context as _, Result};
use wgpu::TextureFormat;
use winit::window::Window;

pub struct GpuContext {
    pub surface:        wgpu::Surface<'static>,
    pub device:         wgpu::Device,
    pub queue:          wgpu::Queue,
    pub surface_format: TextureFormat,
    pub surface_config: wgpu::SurfaceConfiguration,
}

impl GpuContext {
    pub async fn new(window: Arc<Window>) -> Result<Self> {
        let size = window.inner_size();

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        // SAFETY: the window (Arc) outlives the surface because both are owned
        // by our RunningState which drops them together.
        let surface = instance
            .create_surface(Arc::clone(&window))
            .context("create wgpu surface")?;

        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference:       wgpu::PowerPreference::HighPerformance,
                compatible_surface:     Some(&surface),
                force_fallback_adapter: false,
            })
            .await
            .context("no compatible wgpu adapter")?;

        log::info!(
            "wgpu adapter: {} ({:?})",
            adapter.get_info().name,
            adapter.get_info().backend
        );

        let (device, queue) = adapter
            .request_device(
                &wgpu::DeviceDescriptor {
                    label:             Some("main device"),
                    required_features: wgpu::Features::empty(),
                    required_limits:   wgpu::Limits::default(),
                    memory_hints:      wgpu::MemoryHints::Performance,
                    ..Default::default()
                },
            )
            .await
            .context("request wgpu device")?;

        let caps = surface.get_capabilities(&adapter);
        // Prefer sRGB surface formats for correct colour reproduction.
        let surface_format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);

        let surface_config = wgpu::SurfaceConfiguration {
            usage:        wgpu::TextureUsages::RENDER_ATTACHMENT,
            format:       surface_format,
            width:        size.width.max(1),
            height:       size.height.max(1),
            present_mode: wgpu::PresentMode::AutoVsync,
            alpha_mode:   caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &surface_config);

        Ok(Self {
            surface,
            device,
            queue,
            surface_format,
            surface_config,
        })
    }

    pub fn reconfigure(&mut self, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }
        self.surface_config.width  = new_size.width;
        self.surface_config.height = new_size.height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    pub fn size(&self) -> (u32, u32) {
        (self.surface_config.width, self.surface_config.height)
    }
}
