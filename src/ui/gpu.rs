//! Vulkan presentation for the live layer surface. No GPU libraries are linked.
use anyhow::{Context, Result, bail};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use std::ptr::NonNull;
use wayland_client::{Connection, Proxy, protocol::wl_surface::WlSurface};

pub(super) struct Gpu {
    // Drop the swapchain before its Wayland objects and connection.
    surface: wgpu::Surface<'static>,
    renderer: egui_wgpu::Renderer,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    needs_configure: bool,
    _window: WlSurface,
    _connection: Connection,
}

impl Gpu {
    pub fn new(connection: &Connection, window: &WlSurface) -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let display = NonNull::new(connection.backend().display_ptr().cast())
            .context("Wayland display pointer")?;
        let window_ptr =
            NonNull::new(window.id().as_ptr().cast()).context("Wayland surface pointer")?;
        // SAFETY: these are live libwayland objects. Owned clones below keep both
        // valid until after the wgpu surface has been dropped.
        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                    display,
                ))),
                raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(window_ptr)),
            })?
        };
        let adapter = futures_lite::future::block_on(instance.request_adapter(
            &wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::LowPower,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            },
        ))?;
        let capabilities = surface.get_capabilities(&adapter);
        let alpha_mode = capabilities
            .alpha_modes
            .iter()
            .copied()
            .find(|mode| *mode == wgpu::CompositeAlphaMode::PreMultiplied)
            .context("GPU surface needs premultiplied transparency")?;
        let format = capabilities
            .formats
            .iter()
            .copied()
            .find(|format| *format == wgpu::TextureFormat::Bgra8Unorm)
            .or_else(|| {
                capabilities
                    .formats
                    .iter()
                    .copied()
                    .find(|format| !format.is_srgb())
            })
            .context("GPU surface needs an unorm color format")?;
        let (device, queue) =
            futures_lite::future::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("bharta"),
                ..Default::default()
            }))?;
        let mut config = surface
            .get_default_config(&adapter, 1, 1)
            .context("GPU surface configuration")?;
        config.format = format;
        config.alpha_mode = alpha_mode;
        config.present_mode = wgpu::PresentMode::Fifo;
        config.desired_maximum_frame_latency = 2;
        let renderer = egui_wgpu::Renderer::new(
            &device,
            format,
            egui_wgpu::RendererOptions {
                dithering: false,
                ..Default::default()
            },
        );
        eprintln!("bharta: Vulkan renderer ({})", adapter.get_info().name);
        Ok(Self {
            surface,
            renderer,
            device,
            queue,
            config,
            needs_configure: true,
            _window: window.clone(),
            _connection: connection.clone(),
        })
    }

    /// False means presentation was temporarily unavailable; retry next frame.
    pub fn draw(
        &mut self,
        size: [u32; 2],
        jobs: &[egui::ClippedPrimitive],
        textures: &egui::TexturesDelta,
        pixels_per_point: f32,
        before_present: impl FnOnce(),
    ) -> Result<bool> {
        for (id, delta) in &textures.set {
            self.renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        let result = self.paint(size, jobs, pixels_per_point, before_present);
        for id in &textures.free {
            self.renderer.free_texture(id);
        }
        result
    }

    fn paint(
        &mut self,
        size: [u32; 2],
        jobs: &[egui::ClippedPrimitive],
        pixels_per_point: f32,
        before_present: impl FnOnce(),
    ) -> Result<bool> {
        if self.needs_configure || self.config.width != size[0] || self.config.height != size[1] {
            self.config.width = size[0];
            self.config.height = size[1];
            self.surface.configure(&self.device, &self.config);
            self.needs_configure = false;
        }
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                self.needs_configure = true;
                frame
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                return Ok(false);
            }
            wgpu::CurrentSurfaceTexture::Validation => bail!("GPU presentation validation failed"),
        };
        let view = frame.texture.create_view(&Default::default());
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: size,
            pixels_per_point,
        };
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let callbacks =
            self.renderer
                .update_buffers(&self.device, &self.queue, &mut encoder, jobs, &screen);
        {
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("bharta UI"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            self.renderer.render(&mut pass, jobs, &screen);
        }
        self.queue
            .submit(callbacks.into_iter().chain([encoder.finish()]));
        before_present();
        frame.present();
        Ok(true)
    }
}
