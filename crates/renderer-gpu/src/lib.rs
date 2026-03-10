//! GPU-accelerated renderer for vtx using wgpu + winit.
//!
//! Drop-in alternative to `vtx-renderer-tty`.  Each terminal cell is rendered
//! as a textured quad with foreground/background colours, using a simple 8x16
//! bitmap glyph atlas.

pub mod atlas;

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use vtx_core::cell::{Attr, Cell, Color};
use vtx_core::ipc::PaneRender;
use vtx_core::PaneId;
use wgpu::util::DeviceExt;
use winit::dpi::PhysicalSize;
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

// ── Border characters (same as TTY renderer) ────────────────────────────

const BORDER_V: char = '\u{2502}'; // │
const BORDER_H: char = '\u{2500}'; // ─

// ── Selection (mirrors the TTY renderer's Selection type) ───────────────

/// Screen-coordinate selection range (line-based).
#[derive(Debug, Clone, Copy)]
pub struct Selection {
    pub start_x: u16,
    pub start_y: u16,
    pub end_x: u16,
    pub end_y: u16,
}

impl Selection {
    fn normalized(&self) -> (u16, u16, u16, u16) {
        if self.start_y < self.end_y
            || (self.start_y == self.end_y && self.start_x <= self.end_x)
        {
            (self.start_x, self.start_y, self.end_x, self.end_y)
        } else {
            (self.end_x, self.end_y, self.start_x, self.start_y)
        }
    }

    fn contains(&self, x: u16, y: u16) -> bool {
        let (sx, sy, ex, ey) = self.normalized();
        if y < sy || y > ey {
            return false;
        }
        if sy == ey {
            return x >= sx && x <= ex;
        }
        if y == sy {
            return x >= sx;
        }
        if y == ey {
            return x <= ex;
        }
        true
    }
}

// ── Vertex data ─────────────────────────────────────────────────────────

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct CellInstance {
    grid_pos: [f32; 2],
    glyph_uv: [f32; 2],
    fg_color: [f32; 4],
    bg_color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    cell_size: [f32; 2],
    surface_size: [f32; 2],
    glyph_uv_size: [f32; 2],
    _pad: [f32; 2],
}

// ── Colour helpers ──────────────────────────────────────────────────────

/// Basic 16-colour ANSI palette (indices 0-15).  Indices 16-255 are
/// approximated as grey for simplicity.
fn color_to_rgba(color: &Color, is_fg: bool) -> [f32; 4] {
    match color {
        Color::Default => {
            if is_fg {
                [0.8, 0.8, 0.8, 1.0] // light grey foreground
            } else {
                [0.05, 0.05, 0.1, 1.0] // near-black background
            }
        }
        Color::Indexed(idx) => indexed_color(*idx),
        Color::Rgb(r, g, b) => [*r as f32 / 255.0, *g as f32 / 255.0, *b as f32 / 255.0, 1.0],
    }
}

fn indexed_color(idx: u8) -> [f32; 4] {
    #[rustfmt::skip]
    const ANSI16: [[f32; 3]; 16] = [
        [0.0,  0.0,  0.0 ], // 0  black
        [0.67, 0.0,  0.0 ], // 1  red
        [0.0,  0.67, 0.0 ], // 2  green
        [0.67, 0.33, 0.0 ], // 3  yellow/brown
        [0.0,  0.0,  0.67], // 4  blue
        [0.67, 0.0,  0.67], // 5  magenta
        [0.0,  0.67, 0.67], // 6  cyan
        [0.67, 0.67, 0.67], // 7  white (light grey)
        [0.33, 0.33, 0.33], // 8  bright black (dark grey)
        [1.0,  0.33, 0.33], // 9  bright red
        [0.33, 1.0,  0.33], // 10 bright green
        [1.0,  1.0,  0.33], // 11 bright yellow
        [0.33, 0.33, 1.0 ], // 12 bright blue
        [1.0,  0.33, 1.0 ], // 13 bright magenta
        [0.33, 1.0,  1.0 ], // 14 bright cyan
        [1.0,  1.0,  1.0 ], // 15 bright white
    ];

    if (idx as usize) < 16 {
        let c = ANSI16[idx as usize];
        [c[0], c[1], c[2], 1.0]
    } else if idx >= 232 {
        // Greyscale ramp 232-255
        let level = ((idx - 232) as f32 * 10.0 + 8.0) / 255.0;
        [level, level, level, 1.0]
    } else {
        // 216-colour cube (indices 16-231)
        let idx = idx - 16;
        let r = (idx / 36) % 6;
        let g = (idx / 6) % 6;
        let b = idx % 6;
        let to_f = |v: u8| if v == 0 { 0.0 } else { (55.0 + 40.0 * v as f32) / 255.0 };
        [to_f(r), to_f(g), to_f(b), 1.0]
    }
}

// ── GpuRenderer ─────────────────────────────────────────────────────────

/// GPU-accelerated terminal renderer.
pub struct GpuRenderer {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    uniform_buffer: wgpu::Buffer,
    instance_buffer: wgpu::Buffer,
    instance_capacity: usize,

    /// Grid dimensions derived from window size.
    screen_cols: u16,
    screen_rows: u16,

    /// Back buffer of cells (same double-buffer idea as TtyRenderer, but we
    /// re-upload the whole grid each frame since GPU bandwidth is cheap).
    back: Vec<Cell>,
}

impl GpuRenderer {
    /// Create a new GPU renderer.  This opens a window via winit and
    /// initialises wgpu.  Call from a thread that owns the event-loop or
    /// use `pollster::block_on` for the async wgpu initialisation.
    pub fn new(event_loop: &ActiveEventLoop) -> Result<Self, Box<dyn std::error::Error>> {
        let window_attrs = Window::default_attributes()
            .with_title("vtx — GPU")
            .with_inner_size(PhysicalSize::new(1280u32, 800u32));
        let window = Arc::new(event_loop.create_window(window_attrs)?);

        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            ..Default::default()
        });

        let surface = instance.create_surface(window.clone())?;

        let (device, queue, adapter) = pollster::block_on(async {
            let adapter = instance
                .request_adapter(&wgpu::RequestAdapterOptions {
                    power_preference: wgpu::PowerPreference::HighPerformance,
                    compatible_surface: Some(&surface),
                    force_fallback_adapter: false,
                })
                .await
                .ok_or("no suitable GPU adapter")?;

            let (device, queue) = adapter
                .request_device(&wgpu::DeviceDescriptor {
                    label: Some("vtx-gpu"),
                    required_features: wgpu::Features::empty(),
                    required_limits: wgpu::Limits::default(),
                    ..Default::default()
                }, None)
                .await?;

            Ok::<_, Box<dyn std::error::Error>>((device, queue, adapter))
        })?;

        let size = window.inner_size();
        let surface_caps = surface.get_capabilities(&adapter);
        let format = surface_caps
            .formats
            .iter()
            .find(|f| f.is_srgb())
            .copied()
            .unwrap_or(surface_caps.formats[0]);

        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: surface_caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        // ── Glyph atlas texture ─────────────────────────────────────────
        let atlas_pixels = atlas::rasterize_atlas();
        let atlas_texture = device.create_texture_with_data(
            &queue,
            &wgpu::TextureDescriptor {
                label: Some("glyph_atlas"),
                size: wgpu::Extent3d {
                    width: atlas::ATLAS_W,
                    height: atlas::ATLAS_H,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            wgpu::util::TextureDataOrder::LayerMajor,
            &atlas_pixels,
        );
        let atlas_view = atlas_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let atlas_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        // ── Uniform buffer ──────────────────────────────────────────────
        let cols = size.width / atlas::GLYPH_W;
        let rows = size.height / atlas::GLYPH_H;
        let (uv_w, uv_h) = atlas::glyph_uv_size();

        let uniforms = Uniforms {
            cell_size: [atlas::GLYPH_W as f32, atlas::GLYPH_H as f32],
            surface_size: [size.width as f32, size.height as f32],
            glyph_uv_size: [uv_w, uv_h],
            _pad: [0.0; 2],
        };

        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("uniforms"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // ── Bind group layout + bind group ──────────────────────────────
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vtx_bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vtx_bg"),
            layout: &bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: uniform_buffer.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Sampler(&atlas_sampler),
                },
            ],
        });

        // ── Shader + pipeline ───────────────────────────────────────────
        let shader_src = include_str!("shaders/shader.wgsl");
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vtx_shader"),
            source: wgpu::ShaderSource::Wgsl(shader_src.into()),
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("vtx_pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("vtx_pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<CellInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &[
                        // grid_pos
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        // glyph_uv
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8,
                            shader_location: 1,
                        },
                        // fg_color
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 16,
                            shader_location: 2,
                        },
                        // bg_color
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
                            offset: 32,
                            shader_location: 3,
                        },
                    ],
                }],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: config.format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        // ── Instance buffer (pre-allocate for a reasonable grid) ────────
        let initial_capacity = (cols * rows) as usize;
        let instance_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("instances"),
            size: (initial_capacity * std::mem::size_of::<CellInstance>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let screen_cols = cols as u16;
        let screen_rows = rows as u16;
        let grid_size = (screen_cols as usize) * (screen_rows as usize);

        Ok(GpuRenderer {
            window,
            surface,
            device,
            queue,
            config,
            pipeline,
            bind_group,
            uniform_buffer,
            instance_buffer,
            instance_capacity: initial_capacity,
            screen_cols,
            screen_rows,
            back: vec![Cell::default(); grid_size],
        })
    }

    /// Current grid dimensions.
    pub fn size(&self) -> (u16, u16) {
        (self.screen_cols, self.screen_rows)
    }

    /// Reference to the underlying window.
    pub fn window(&self) -> &Window {
        &self.window
    }

    /// Handle a window resize event.
    pub fn handle_resize(&mut self, new_size: PhysicalSize<u32>) {
        if new_size.width == 0 || new_size.height == 0 {
            return;
        }

        self.config.width = new_size.width;
        self.config.height = new_size.height;
        self.surface.configure(&self.device, &self.config);

        self.screen_cols = (new_size.width / atlas::GLYPH_W) as u16;
        self.screen_rows = (new_size.height / atlas::GLYPH_H) as u16;

        let grid_size = self.screen_cols as usize * self.screen_rows as usize;
        self.back = vec![Cell::default(); grid_size];

        // Update uniforms
        let (uv_w, uv_h) = atlas::glyph_uv_size();
        let uniforms = Uniforms {
            cell_size: [atlas::GLYPH_W as f32, atlas::GLYPH_H as f32],
            surface_size: [new_size.width as f32, new_size.height as f32],
            glyph_uv_size: [uv_w, uv_h],
            _pad: [0.0; 2],
        };
        self.queue
            .write_buffer(&self.uniform_buffer, 0, bytemuck::bytes_of(&uniforms));
    }

    /// Render a frame — same signature as `TtyRenderer::render_frame`.
    pub fn render_frame(
        &mut self,
        panes: &[PaneRender],
        focused: PaneId,
        borders: &[(u16, u16, u16, bool)],
        status: &str,
        _total_rows: u16,
        prefix_active: bool,
        selection: Option<&Selection>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let cols = self.screen_cols;
        let rows = self.screen_rows;

        // Clear back buffer
        self.back.fill(Cell::default());

        // Stamp borders
        for &(x, y, length, horizontal) in borders {
            if horizontal {
                for i in 0..length {
                    self.set_back(x + i, y, Cell {
                        c: BORDER_H,
                        fg: Color::Indexed(8),
                        bg: Color::Default,
                        attr: Attr::empty(),
                    });
                }
            } else {
                for i in 0..length {
                    self.set_back(x, y + i, Cell {
                        c: BORDER_V,
                        fg: Color::Indexed(8),
                        bg: Color::Default,
                        attr: Attr::empty(),
                    });
                }
            }
        }

        // Stamp pane content
        for pane in panes {
            for (row_idx, row) in pane.content.iter().enumerate() {
                let y = pane.y + row_idx as u16;
                if y >= rows {
                    break;
                }
                for (col_idx, cell) in row.iter().enumerate() {
                    let x = pane.x + col_idx as u16;
                    if x >= cols {
                        break;
                    }
                    self.set_back(x, y, cell.clone());
                }
            }
        }

        // Apply selection highlight
        if let Some(sel) = selection {
            for y in 0..rows.saturating_sub(1) {
                for x in 0..cols {
                    if sel.contains(x, y) {
                        let idx = y as usize * cols as usize + x as usize;
                        if idx < self.back.len() {
                            let cell = &mut self.back[idx];
                            std::mem::swap(&mut cell.fg, &mut cell.bg);
                            if cell.fg == Color::Default {
                                cell.fg = Color::Rgb(0, 0, 0);
                            }
                            if cell.bg == Color::Default {
                                cell.bg = Color::Rgb(200, 200, 255);
                            }
                        }
                    }
                }
            }
        }

        // Status bar
        let status_y = rows.saturating_sub(1);
        let prefix_indicator = if prefix_active { " [PREFIX]" } else { "" };
        let time = utc_time();
        let left = format!(" {status}{prefix_indicator}");
        let right = format!(" {time} ");
        let total_len = left.len() + right.len();
        let padding = if (cols as usize) > total_len {
            cols as usize - total_len
        } else {
            0
        };
        let full_status = format!("{left}{:padding$}{right}", "", padding = padding);

        let status_fg = Color::Rgb(180, 210, 255);
        let status_bg = Color::Rgb(40, 40, 40);
        for (i, ch) in full_status.chars().enumerate() {
            if i >= cols as usize {
                break;
            }
            self.set_back(i as u16, status_y, Cell {
                c: ch,
                fg: status_fg,
                bg: status_bg,
                attr: Attr::empty(),
            });
        }

        // Build instance data from the back buffer
        let grid_size = cols as usize * rows as usize;
        let mut instances: Vec<CellInstance> = Vec::with_capacity(grid_size);

        for y in 0..rows {
            for x in 0..cols {
                let idx = y as usize * cols as usize + x as usize;
                let cell = &self.back[idx];

                let (uv_u, uv_v) = atlas::glyph_uv(cell.c);
                let fg = color_to_rgba(&cell.fg, true);
                let bg = color_to_rgba(&cell.bg, false);

                // Handle REVERSE attribute
                let (fg, bg) = if cell.attr.contains(Attr::REVERSE) {
                    (bg, fg)
                } else {
                    (fg, bg)
                };

                // Handle BOLD — brighten fg slightly
                let fg = if cell.attr.contains(Attr::BOLD) {
                    [
                        (fg[0] * 1.3).min(1.0),
                        (fg[1] * 1.3).min(1.0),
                        (fg[2] * 1.3).min(1.0),
                        fg[3],
                    ]
                } else {
                    fg
                };

                // Handle DIM — darken fg
                let fg = if cell.attr.contains(Attr::DIM) {
                    [fg[0] * 0.5, fg[1] * 0.5, fg[2] * 0.5, fg[3]]
                } else {
                    fg
                };

                instances.push(CellInstance {
                    grid_pos: [x as f32, y as f32],
                    glyph_uv: [uv_u, uv_v],
                    fg_color: fg,
                    bg_color: bg,
                });
            }
        }

        // Ensure instance buffer is large enough
        let required_size = instances.len() * std::mem::size_of::<CellInstance>();
        if instances.len() > self.instance_capacity {
            self.instance_buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("instances"),
                size: required_size as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.instance_capacity = instances.len();
        }

        self.queue.write_buffer(
            &self.instance_buffer,
            0,
            bytemuck::cast_slice(&instances),
        );

        // Render
        let output = self.surface.get_current_texture()?;
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vtx_enc"),
            });

        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("vtx_pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.05,
                            g: 0.05,
                            b: 0.1,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });

            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.set_vertex_buffer(0, self.instance_buffer.slice(..));
            // 6 vertices per quad (two triangles), one instance per cell
            pass.draw(0..6, 0..instances.len() as u32);
        }

        self.queue.submit(std::iter::once(encoder.finish()));
        output.present();

        // Cursor handling — request redraw with cursor position info
        // (In a full integration this would drive a cursor overlay; for now
        // the bitmap renderer doesn't draw a blinking cursor.)
        let _focused_pane = panes.iter().find(|p| p.id == focused);

        Ok(())
    }

    #[inline]
    fn set_back(&mut self, x: u16, y: u16, cell: Cell) {
        let idx = y as usize * self.screen_cols as usize + x as usize;
        if idx < self.back.len() {
            self.back[idx] = cell;
        }
    }

    /// Clean up resources.
    pub fn cleanup(&mut self) {
        // wgpu resources are dropped automatically, but this method exists
        // to mirror TtyRenderer's interface.
        tracing::info!("GPU renderer cleanup");
    }
}

impl Drop for GpuRenderer {
    fn drop(&mut self) {
        self.cleanup();
    }
}

// ── Utility ─────────────────────────────────────────────────────────────

fn utc_time() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    format!("{h:02}:{m:02}:{s:02}")
}
