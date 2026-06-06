use crate::audio::AudioVisualizationSnapshot;
use anyhow::{anyhow, Context, Result};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::shell::{wlr_layer::LayerSurface, WaylandSurface};
use std::ptr::NonNull;
use std::time::Duration;
use wayland_client::{Connection, Proxy};

/// Per-frame inputs the shader needs to render the status overlay. Grouped into
/// a struct so `render`/`write_uniforms` stay under clippy's argument limit and
/// share one source of truth for the field set.
pub(crate) struct FrameParams<'a> {
    pub(crate) processing_elapsed: f32,
    pub(crate) audio: Option<&'a AudioVisualizationSnapshot>,
    pub(crate) fade_alpha: f32,
    pub(crate) fbm_rotation_phase: f32,
    pub(crate) fbm_translation_phase: f32,
    pub(crate) elapsed: Duration,
}

#[derive(Debug)]
pub(crate) struct Renderer {
    pub(crate) surface: wgpu::Surface<'static>,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    pub(crate) config: wgpu::SurfaceConfiguration,
    pub(crate) pipeline: wgpu::RenderPipeline,
    pub(crate) uniform_buffer: wgpu::Buffer,
    pub(crate) uniform_bind_group: wgpu::BindGroup,
}

impl Renderer {
    pub(crate) fn new(
        conn: &Connection,
        layer_surface: &LayerSurface,
        width: u32,
        height: u32,
    ) -> Result<Self> {
        let instance = wgpu::Instance::default();

        let surface: wgpu::Surface<'static> = unsafe {
            let raw_display_handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                NonNull::new(conn.backend().display_ptr() as *mut _)
                    .ok_or_else(|| anyhow!("failed to derive Wayland raw display handle"))?,
            ));
            let raw_window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(
                NonNull::new(layer_surface.wl_surface().id().as_ptr() as *mut _)
                    .ok_or_else(|| anyhow!("failed to derive Wayland raw window handle"))?,
            ));

            let target = wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: Some(raw_display_handle),
                raw_window_handle,
            };
            instance
                .create_surface_unsafe(target)
                .context("failed to create GPU surface")?
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to build wgpu helper runtime")?;

        let adapter = runtime.block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: Some(&surface),
        }))?;

        let (device, queue) =
            runtime.block_on(adapter.request_device(&wgpu::DeviceDescriptor {
                label: Some("ut status ui device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::default(),
                experimental_features: wgpu::ExperimentalFeatures::disabled(),
                memory_hints: wgpu::MemoryHints::Performance,
                trace: wgpu::Trace::default(),
            }))?;

        let surface_size = wgpu::Extent3d {
            width: width.max(1),
            height: height.max(1),
            depth_or_array_layers: 1,
        };

        let mut surface_config = surface
            .get_default_config(&adapter, surface_size.width, surface_size.height)
            .ok_or_else(|| anyhow!("status UI surface is not supported by the selected adapter"))?;
        let alpha_mode = surface
            .get_capabilities(&adapter)
            .alpha_modes
            .into_iter()
            .find(|mode| {
                matches!(
                    mode,
                    wgpu::CompositeAlphaMode::PreMultiplied
                        | wgpu::CompositeAlphaMode::PostMultiplied
                        | wgpu::CompositeAlphaMode::Auto
                )
            })
            .unwrap_or(wgpu::CompositeAlphaMode::Auto);
        surface_config.alpha_mode = alpha_mode;
        surface_config.width = surface_size.width;
        surface_config.height = surface_size.height;
        surface.configure(&device, &surface_config);

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("ut status ui shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER_WGSL.into()),
        });

        let uniform_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("ut status ui uniforms"),
            size: UNIFORM_BUFFER_SIZE as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("ut status ui uniforms layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });

        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("ut status ui uniforms bind group"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform_buffer.as_entire_binding(),
            }],
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("ut status ui pipeline layout"),
            bind_group_layouts: &[Some(&uniform_layout)],
            immediate_size: 0,
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("ut status ui pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_config.format,
                    blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                strip_index_format: None,
                front_face: wgpu::FrontFace::Ccw,
                cull_mode: None,
                polygon_mode: wgpu::PolygonMode::Fill,
                unclipped_depth: false,
                conservative: false,
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });

        let state = Self {
            surface,
            device,
            queue,
            config: surface_config,
            pipeline,
            uniform_buffer,
            uniform_bind_group,
        };

        state.write_uniforms(&FrameParams {
            processing_elapsed: 0.0,
            audio: None,
            fade_alpha: 0.0,
            fbm_rotation_phase: 0.0,
            fbm_translation_phase: 0.0,
            elapsed: Duration::ZERO,
        });

        Ok(state)
    }

    pub(crate) fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }

        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
    }

    pub(crate) fn render(&mut self, params: FrameParams) -> Result<()> {
        self.write_uniforms(&params);

        let output = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(output)
            | wgpu::CurrentSurfaceTexture::Suboptimal(output) => output,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.surface.configure(&self.device, &self.config);
                match self.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(output)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(output) => output,
                    wgpu::CurrentSurfaceTexture::Timeout
                    | wgpu::CurrentSurfaceTexture::Occluded
                    | wgpu::CurrentSurfaceTexture::Outdated
                    | wgpu::CurrentSurfaceTexture::Lost
                    | wgpu::CurrentSurfaceTexture::Validation => return Ok(()),
                }
            }
            wgpu::CurrentSurfaceTexture::Timeout
            | wgpu::CurrentSurfaceTexture::Occluded
            | wgpu::CurrentSurfaceTexture::Validation => return Ok(()),
        };

        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("ut status ui encoder"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("ut status ui pass"),
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
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.draw(0..3, 0..1);
        }

        self.queue.submit(Some(encoder.finish()));
        output.present();
        Ok(())
    }

    pub(crate) fn write_uniforms(&self, params: &FrameParams) {
        let FrameParams {
            processing_elapsed,
            audio,
            fade_alpha,
            fbm_rotation_phase,
            fbm_translation_phase,
            elapsed,
        } = params;
        let snapshot = audio.cloned().unwrap_or_default();

        let mut bytes = Vec::with_capacity(UNIFORM_BUFFER_SIZE);
        // header = (elapsed, processing_elapsed, fade, _pad)
        push_f32(&mut bytes, elapsed.as_secs_f32());
        push_f32(&mut bytes, *processing_elapsed);
        push_f32(&mut bytes, fade_alpha.clamp(0.0, 1.0));
        push_f32(&mut bytes, 0.0);

        // audio = (voice pulse, peak, width, height)
        push_f32(&mut bytes, snapshot.level);
        push_f32(&mut bytes, snapshot.peak);
        push_f32(&mut bytes, self.config.width as f32);
        push_f32(&mut bytes, self.config.height as f32);

        // motion = (rotation phase, translation phase, _pad, _pad)
        push_vec4(
            &mut bytes,
            [*fbm_rotation_phase, *fbm_translation_phase, 0.0, 0.0],
        );

        self.queue.write_buffer(&self.uniform_buffer, 0, &bytes);
    }
}

fn push_f32(bytes: &mut Vec<u8>, value: f32) {
    bytes.extend_from_slice(&value.to_ne_bytes());
}

fn push_vec4(bytes: &mut Vec<u8>, values: [f32; 4]) {
    for value in values {
        push_f32(bytes, value);
    }
}

const UNIFORM_BUFFER_SIZE: usize = 48;

const SHADER_WGSL: &str = include_str!("status_overlay.wgsl");
