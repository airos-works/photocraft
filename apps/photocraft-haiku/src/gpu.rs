//! Hardware rendering of the window. Haiku has no wgpu surface; egui's triangles and the document
//! canvas draw into the presenter's frame texture (`present.rs`), which gets it onto the screen.
use egui_wgpu::{RenderState, Renderer, RendererOptions, ScreenDescriptor, WgpuConfiguration, WgpuSetup};
use photocraft_ui_egui::gpu_canvas;
use std::sync::Arc;
use std::time::Instant;

use crate::present::{self, Presenter, Shown};

pub struct Gpu {
    pub state: RenderState,
    presenter: Presenter,
}
impl Gpu {
    pub fn new() -> Result<Self, String> {
        let mut setup = gpu_canvas::wgpu_setup();
        let WgpuSetup::CreateNew(ref mut create) = setup else { return Err("Expected a new GPU device".into()) };
        create.instance_descriptor.backends = wgpu::Backends::VULKAN;
        create.native_adapter_selector = Some(std::sync::Arc::new(|adapters, _surface| {
            adapters
                .iter()
                .find(|a| matches!(a.get_info().device_type, wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu))
                .cloned()
                .ok_or_else(|| "No hardware Vulkan adapter; using software rendering".to_string())
        }));
        let instance = pollster::block_on(setup.new_instance());
        let config = WgpuConfiguration { wgpu_setup: setup, ..Default::default() };
        let mut state = pollster::block_on(RenderState::create(&config, &instance, None, RendererOptions::default())).map_err(|e| e.to_string())?;
        // Frames are drawn in the screen's byte order: they go to the frame buffer, or into a
        // B_RGB32 bitmap, as they are.
        state.target_format = present::FORMAT;
        state.renderer = Arc::new(egui::mutex::RwLock::new(Renderer::new(&state.device, present::FORMAT, RendererOptions::default())));
        log::info!(target: "photocraft_haiku", "air/OS GPU: {:?}", state.adapter.get_info());
        let presenter = Presenter::new(&state.device, &state.queue, present::Mode::from_env("PHOTOCRAFT_PRESENT"), &crate::host::Native);
        log::info!(target: "photocraft_haiku", "air/OS present: {}", presenter.mode().name());
        Ok(Self { state, presenter })
    }
    pub fn max_texture_side(&self) -> usize {
        self.state.device.limits().max_texture_dimension_2d as usize
    }
    pub fn present_mode(&self) -> present::Mode {
        self.presenter.mode()
    }
    /// Draws the frame and shows it (`scale` device pixels per logical pixel).
    pub fn render(
        &mut self,
        size: [u32; 2],
        primitives: &[egui::ClippedPrimitive],
        delta: &egui::TexturesDelta,
        ppp: f32,
        clear: egui::Color32,
        scale: f32,
    ) -> Result<Shown, String> {
        let state = &self.state;
        let view = self.presenter.target(size)?;
        let start = Instant::now();
        let screen = ScreenDescriptor { size_in_pixels: size, pixels_per_point: ppp };
        let mut encoder = state.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("PhotoCraft native frame") });
        let mut renderer = state.renderer.write();
        for (id, images) in &delta.set {
            for image in images {
                renderer.update_texture(&state.device, &state.queue, *id, image);
            }
        }
        let mut commands = renderer.update_buffers(&state.device, &state.queue, &mut encoder, primitives, &screen);
        {
            let rgba = clear.to_array().map(|c| f64::from(c) / 255.0);
            let mut pass = encoder
                .begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("PhotoCraft native UI"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        depth_slice: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color { r: rgba[0], g: rgba[1], b: rgba[2], a: rgba[3] }),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                })
                .forget_lifetime();
            renderer.render(&mut pass, primitives, &screen);
        }
        commands.push(encoder.finish());
        drop(renderer);
        let encoded = start.elapsed();
        let shown = self.presenter.present(commands, &crate::host::Native, scale)?;
        let mut renderer = self.state.renderer.write();
        for id in &delta.free {
            renderer.free_texture(id);
        }
        log::debug!(target: "photocraft_haiku",
            "GPU frame {}x{}: encode {:.2} ms, present {:.2} ms ({}, {} px in {} rects)",
            size[0],
            size[1],
            encoded.as_secs_f64() * 1e3,
            shown.wait.as_secs_f64() * 1e3,
            if shown.direct { "direct" } else { "readback" },
            shown.pixels,
            shown.rects
        );
        Ok(shown)
    }
    /// The last frame again, where the window's place on the screen changed.
    pub fn present_again(&mut self, scale: f32) -> Result<Shown, String> {
        self.presenter.present(Vec::new(), &crate::host::Native, scale)
    }
    pub fn wants_present(&self) -> bool {
        self.presenter.wants_present()
    }
    pub fn sync_due(&self) -> Option<Instant> {
        self.presenter.sync_due()
    }
    /// Brings app_server's copy of the window up to date with what went straight to the screen.
    pub fn sync(&mut self, scale: f32) -> Result<Shown, String> {
        self.presenter.sync(&crate::host::Native, scale)
    }
    pub fn image(&mut self) -> Result<egui::ColorImage, String> {
        self.presenter.image()
    }
}
impl Drop for Gpu {
    fn drop(&mut self) {
        self.presenter.detach(&crate::host::Native);
    }
}
