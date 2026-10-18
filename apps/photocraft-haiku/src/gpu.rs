//! Hardware rendering into a reusable offscreen target. Haiku has no wgpu surface handle;
//! the native host presents the completed bitmap. Document compositing and egui triangles
//! both use the real wgpu device. Readback cost is reported separately from GPU submission.
use egui_wgpu::{RenderState, RendererOptions, ScreenDescriptor, WgpuConfiguration, WgpuSetup};
use photocraft_ui_egui::gpu_canvas;
use std::time::{Duration, Instant};

pub struct Gpu {
    pub state: RenderState,
    target: Option<Target>,
    pixels: Vec<u32>,
}
struct Target {
    size: [u32; 2],
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback: wgpu::Buffer,
    stride: u32,
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
        let state = pollster::block_on(RenderState::create(&config, &instance, None, RendererOptions::default())).map_err(|e| e.to_string())?;
        log::info!(target: "photocraft_haiku", "air/OS GPU: {:?}", state.adapter.get_info());
        Ok(Self { state, target: None, pixels: Vec::new() })
    }
    pub fn max_texture_side(&self) -> usize {
        self.state.device.limits().max_texture_dimension_2d as usize
    }
    fn resize(&mut self, size: [u32; 2]) -> Result<(), String> {
        if self.target.as_ref().is_some_and(|t| t.size == size) {
            return Ok(());
        }
        let [w, h] = size;
        let limit = self.state.device.limits().max_texture_dimension_2d;
        if w == 0 || h == 0 || w > limit || h > limit {
            return Err(format!("Window {w}x{h} exceeds GPU limit {limit}"));
        }
        let stride = w.checked_mul(4).and_then(|s| s.checked_add(255)).map(|s| s / 256 * 256).ok_or("Readback stride overflow")?;
        let bytes = u64::from(stride) * u64::from(h);
        // Bound allocations even when the window server sends unreasonable dimensions.
        if bytes > self.state.device.limits().max_buffer_size.min(512 << 20) {
            return Err("Window readback exceeds memory budget".into());
        }
        let texture = self.state.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("PhotoCraft native frame"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.state.target_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let readback = self.state.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("PhotoCraft native readback"),
            size: bytes,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        self.target = Some(Target { size, texture, view, readback, stride });
        self.pixels.resize(w as usize * h as usize, 0);
        Ok(())
    }
    pub fn render(
        &mut self,
        size: [u32; 2],
        primitives: &[egui::ClippedPrimitive],
        delta: &egui::TexturesDelta,
        ppp: f32,
        clear: egui::Color32,
    ) -> Result<(), String> {
        self.resize(size)?;
        let target = self.target.as_ref().ok_or("Missing frame target")?;
        let state = &self.state;
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
                        view: &target.view,
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
        encoder.copy_texture_to_buffer(
            target.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &target.readback,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(target.stride), rows_per_image: Some(size[1]) },
            },
            wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
        );
        commands.push(encoder.finish());
        let submission = state.queue.submit(commands);
        for id in &delta.free {
            renderer.free_texture(id);
        }
        drop(renderer);
        let submitted = start.elapsed();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        target.readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        state.device.poll(wgpu::PollType::Wait { submission_index: Some(submission), timeout: Some(Duration::from_secs(5)) }).map_err(|e| e.to_string())?;
        rx.recv_timeout(Duration::from_secs(1)).map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
        {
            let mapped = target.readback.slice(..).get_mapped_range().map_err(|e| e.to_string())?;
            for (src, dst) in mapped.chunks_exact(target.stride as usize).zip(self.pixels.chunks_exact_mut(size[0] as usize)) {
                for (pixel, out) in src.as_chunks::<4>().0.iter().zip(dst) {
                    *out = u32::from_le_bytes([pixel[2], pixel[1], pixel[0], pixel[3]]);
                }
            }
        }
        target.readback.unmap();
        log::debug!(target: "photocraft_haiku",
            "GPU frame {}x{}: submit {:.2} ms, readback {:.2} ms",
            size[0],
            size[1],
            submitted.as_secs_f64() * 1e3,
            (start.elapsed() - submitted).as_secs_f64() * 1e3
        );
        Ok(())
    }
    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }
    pub fn image(&self) -> egui::ColorImage {
        let size = self.target.as_ref().map_or([0, 0], |t| t.size.map(|n| n as usize));
        let rgba: Vec<u8> = self
            .pixels
            .iter()
            .flat_map(|p| {
                let [b, g, r, a] = p.to_le_bytes();
                [r, g, b, a]
            })
            .collect();
        egui::ColorImage::from_rgba_premultiplied(size, &rgba)
    }
}
