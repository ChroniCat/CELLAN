#![allow(unsafe_code)]

use std::{borrow::Cow, num::NonZeroU64, ops::Range};

use ahash::HashMap;
use epaint::{emath::NumExt, PaintCallbackInfo, Primitive, Vertex};

use wgpu::util::DeviceExt as _;

// Only implements Send + Sync on wasm32 in order to allow storing wgpu resources on the type map.
#[cfg(not(all(
    target_arch = "wasm32",
    not(feature = "fragile-send-sync-non-atomic-wasm"),
)))]
/// You can use this for storage when implementing [`CallbackTrait`].
pub type CallbackResources = type_map::concurrent::TypeMap;
#[cfg(all(
    target_arch = "wasm32",
    not(feature = "fragile-send-sync-non-atomic-wasm"),
))]
/// You can use this for storage when implementing [`CallbackTrait`].
pub type CallbackResources = type_map::TypeMap;

/// You can use this to do custom [`wgpu`] rendering in an egui app.
///
/// Implement [`CallbackTrait`] and call [`Callback::new_paint_callback`].
///
/// This can be turned into a [`epaint::PaintCallback`] and [`epaint::Shape`].
pub struct Callback(Box<dyn CallbackTrait>);

impl Callback {
    /// Creates a new [`epaint::PaintCallback`] from a callback trait instance.
    pub fn new_paint_callback(
        rect: epaint::emath::Rect,
        callback: impl CallbackTrait + 'static,
    ) -> epaint::PaintCallback {
        epaint::PaintCallback {
            rect,
            callback: std::sync::Arc::new(Self(Box::new(callback))),
        }
    }
}

/// A callback trait that can be used to compose an [`epaint::PaintCallback`] via [`Callback`]
/// for custom WGPU rendering.
///
/// Callbacks in [`Renderer`] are done in three steps:
/// * [`CallbackTrait::prepare`]: called for all registered callbacks before the main egui render pass.
/// * [`CallbackTrait::finish_prepare`]: called for all registered callbacks after all callbacks finished calling prepare.
/// * [`CallbackTrait::paint`]: called for all registered callbacks during the main egui render pass.
///
/// Each callback has access to an instance of [`CallbackResources`] that is stored in the [`Renderer`].
/// This can be used to store wgpu resources that need to be accessed during the [`CallbackTrait::paint`] step.
///
/// The callbacks implementing [`CallbackTrait`] itself must always be Send + Sync, but resources stored in
/// [`Renderer::callback_resources`] are not required to implement Send + Sync when building for wasm.
/// (this is because wgpu stores references to the JS heap in most of its resources which can not be shared with other threads).
///
///
/// # Command submission
///
/// ## Command Encoder
///
/// The passed-in [`wgpu::CommandEncoder`] is egui's and can be used directly to register
/// wgpu commands for simple use cases.
/// This allows reusing the same [`wgpu::CommandEncoder`] for all callbacks and egui
/// rendering itself.
///
/// ## Command Buffers
///
/// For more complicated use cases, one can also return a list of arbitrary
/// [`wgpu::CommandBuffer`]s and have complete control over how they get created and fed.
/// In particular, this gives an opportunity to parallelize command registration and
/// prevents a faulty callback from poisoning the main wgpu pipeline.
///
/// When using eframe, the main egui command buffer, as well as all user-defined
/// command buffers returned by this function, are guaranteed to all be submitted
/// at once in a single call.
///
/// Command Buffers returned by [`CallbackTrait::finish_prepare`] will always be issued *after*
/// those returned by [`CallbackTrait::prepare`].
/// Order within command buffers returned by [`CallbackTrait::prepare`] is dependent
/// on the order the respective [`epaint::Shape::Callback`]s were submitted in.
///
/// # Example
///
/// See the [`custom3d_wgpu`](https://github.com/emilk/egui/blob/master/crates/egui_demo_app/src/apps/custom3d_wgpu.rs) demo source for a detailed usage example.
pub trait CallbackTrait: Send + Sync {
    fn prepare(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _screen_descriptor: &ScreenDescriptor,
        _egui_encoder: &mut wgpu::CommandEncoder,
        _callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        Vec::new()
    }

    /// Called after all [`CallbackTrait::prepare`] calls are done.
    fn finish_prepare(
        &self,
        _device: &wgpu::Device,
        _queue: &wgpu::Queue,
        _egui_encoder: &mut wgpu::CommandEncoder,
        _callback_resources: &mut CallbackResources,
    ) -> Vec<wgpu::CommandBuffer> {
        Vec::new()
    }

    /// Called after all [`CallbackTrait::finish_prepare`] calls are done.
    ///
    /// It is given access to the [`wgpu::RenderPass`] so that it can issue draw commands
    /// into the same [`wgpu::RenderPass`] that is used for all other egui elements.
    fn paint(
        &self,
        info: PaintCallbackInfo,
        render_pass: &mut wgpu::RenderPass<'static>,
        callback_resources: &CallbackResources,
    );
}

/// Information about the screen used for rendering.
pub struct ScreenDescriptor {
    /// Size of the window in physical pixels.
    pub size_in_pixels: [u32; 2],

    /// High-DPI scale factor (pixels per point).
    pub pixels_per_point: f32,
}

impl ScreenDescriptor {
    /// size in "logical" points
    fn screen_size_in_points(&self) -> [f32; 2] {
        [
            self.size_in_pixels[0] as f32 / self.pixels_per_point,
            self.size_in_pixels[1] as f32 / self.pixels_per_point,
        ]
    }
}

/// Uniform buffer used when rendering.
#[derive(Clone, Copy, Debug, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
struct UniformBuffer {
    screen_size_in_points: [f32; 2],
    dithering: u32,
    // Uniform buffers need to be at least 16 bytes in WebGL.
    // See https://github.com/gfx-rs/wgpu/issues/2072
    _padding: u32,
    cleartype_gamma: f32,
    cleartype_contrast: f32,
    _padding2: [u32; 2],
}

impl PartialEq for UniformBuffer {
    fn eq(&self, other: &Self) -> bool {
        self.screen_size_in_points == other.screen_size_in_points
            && self.dithering == other.dithering
            && self.cleartype_gamma.to_bits() == other.cleartype_gamma.to_bits()
            && self.cleartype_contrast.to_bits() == other.cleartype_contrast.to_bits()
    }
}

struct SlicedBuffer {
    buffer: wgpu::Buffer,
    slices: Vec<Range<usize>>,
    capacity: wgpu::BufferAddress,
}

pub struct Texture {
    /// The texture may be None if the `TextureId` is just a handle to a user-provided bind-group.
    pub texture: Option<wgpu::Texture>,

    /// Bindgroup for the texture + sampler.
    pub bind_group: wgpu::BindGroup,

    /// Options describing the sampler used in the bind group. This may be None if the `TextureId`
    /// is just a handle to a user-provided bind-group.
    pub options: Option<epaint::textures::TextureOptions>,
}

/// Renderer for a egui based GUI.
pub struct Renderer {
    pipeline: wgpu::RenderPipeline,
    cleartype_pipeline: Option<wgpu::RenderPipeline>,

    index_buffer: SlicedBuffer,
    vertex_buffer: SlicedBuffer,

    uniform_buffer: wgpu::Buffer,
    previous_uniform_buffer_content: UniformBuffer,
    uniform_bind_group: wgpu::BindGroup,
    texture_bind_group_layout: wgpu::BindGroupLayout,

    /// Map of egui texture IDs to textures and their associated bindgroups (texture view +
    /// sampler). The texture may be None if the `TextureId` is just a handle to a user-provided
    /// sampler.
    textures: HashMap<epaint::TextureId, Texture>,
    next_user_texture_id: u64,
    samplers: HashMap<epaint::textures::TextureOptions, wgpu::Sampler>,

    dithering: bool,

    /// Storage for resources shared with all invocations of [`CallbackTrait`]'s methods.
    ///
    /// See also [`CallbackTrait`].
    pub callback_resources: CallbackResources,
}

impl Renderer {
    /// Creates a renderer for a egui UI.
    ///
    /// `output_color_format` should preferably be [`wgpu::TextureFormat::Rgba8Unorm`] or
    /// [`wgpu::TextureFormat::Bgra8Unorm`], i.e. in gamma-space.
    pub fn new(
        device: &wgpu::Device,
        output_color_format: wgpu::TextureFormat,
        output_depth_format: Option<wgpu::TextureFormat>,
        msaa_samples: u32,
        dithering: bool,
    ) -> Self {
        profiling::function_scope!();

        let shader = wgpu::ShaderModuleDescriptor {
            label: Some("egui"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!("egui.wgsl"))),
        };
        let module = {
            profiling::scope!("create_shader_module");
            device.create_shader_module(shader)
        };

        let (initial_gamma, initial_contrast) = epaint::text::cleartype_blend_params();
        let uniform_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("egui_uniform_buffer"),
            contents: bytemuck::cast_slice(&[UniformBuffer {
                screen_size_in_points: [0.0, 0.0],
                dithering: u32::from(dithering),
                _padding: Default::default(),
                cleartype_gamma: initial_gamma,
                cleartype_contrast: initial_contrast,
                _padding2: Default::default(),
            }]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        let uniform_bind_group_layout = {
            profiling::scope!("create_bind_group_layout");
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("egui_uniform_bind_group_layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX | wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(std::mem::size_of::<UniformBuffer>() as _),
                        ty: wgpu::BufferBindingType::Uniform,
                    },
                    count: None,
                }],
            })
        };

        let uniform_bind_group = {
            profiling::scope!("create_bind_group");
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("egui_uniform_bind_group"),
                layout: &uniform_bind_group_layout,
                entries: &[wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &uniform_buffer,
                        offset: 0,
                        size: None,
                    }),
                }],
            })
        };

        let texture_bind_group_layout = {
            profiling::scope!("create_bind_group_layout");
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("egui_texture_bind_group_layout"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            multisampled: false,
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            })
        };

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("egui_pipeline_layout"),
            bind_group_layouts: &[&uniform_bind_group_layout, &texture_bind_group_layout],
            push_constant_ranges: &[],
        });

        let depth_stencil = output_depth_format.map(|format| wgpu::DepthStencilState {
            format,
            depth_write_enabled: false,
            depth_compare: wgpu::CompareFunction::Always,
            stencil: wgpu::StencilState::default(),
            bias: wgpu::DepthBiasState::default(),
        });

        let vertex_buffers = [wgpu::VertexBufferLayout {
            array_stride: 5 * 4,
            step_mode: wgpu::VertexStepMode::Vertex,
            // 0: vec2 position
            // 1: vec2 texture coordinates
            // 2: uint color
            attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Uint32],
        }];
        let primitive = wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            unclipped_depth: false,
            conservative: false,
            cull_mode: None,
            front_face: wgpu::FrontFace::default(),
            polygon_mode: wgpu::PolygonMode::default(),
            strip_index_format: None,
        };
        let multisample = wgpu::MultisampleState {
            alpha_to_coverage_enabled: false,
            count: msaa_samples,
            mask: !0,
        };

        let pipeline = {
            profiling::scope!("create_render_pipeline");
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("egui_pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    entry_point: Some("vs_main"),
                    module: &module,
                    buffers: &vertex_buffers,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                primitive,
                depth_stencil: depth_stencil.clone(),
                multisample,

                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(if output_color_format.is_srgb() {
                        log::warn!("Detected a linear (sRGBA aware) framebuffer {:?}. egui prefers Rgba8Unorm or Bgra8Unorm", output_color_format);
                        "fs_main_linear_framebuffer"
                    } else {
                        "fs_main_gamma_framebuffer" // this is what we prefer
                    }),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: output_color_format,
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Add,
                            },
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                multiview: None,
                cache: None,
            })
        };

        let cleartype_pipeline = if device
            .features()
            .contains(wgpu::Features::DUAL_SOURCE_BLENDING)
        {
            profiling::scope!("create_cleartype_render_pipeline");
            let ct_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("egui_cleartype"),
                source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(include_str!(
                    "egui_cleartype.wgsl"
                ))),
            });
            let ct_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("egui_cleartype_pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    entry_point: Some("vs_main"),
                    module: &ct_module,
                    buffers: &vertex_buffers,
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                },
                primitive,
                depth_stencil,
                multisample,
                fragment: Some(wgpu::FragmentState {
                    module: &ct_module,
                    entry_point: Some(if output_color_format.is_srgb() {
                        "fs_cleartype_linear_framebuffer"
                    } else {
                        "fs_cleartype_gamma_framebuffer"
                    }),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: output_color_format,
                        blend: Some(wgpu::BlendState {
                            color: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::One,
                                dst_factor: wgpu::BlendFactor::OneMinusSrc1,
                                operation: wgpu::BlendOperation::Add,
                            },
                            alpha: wgpu::BlendComponent {
                                src_factor: wgpu::BlendFactor::OneMinusDstAlpha,
                                dst_factor: wgpu::BlendFactor::One,
                                operation: wgpu::BlendOperation::Add,
                            },
                        }),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                }),
                multiview: None,
                cache: None,
            });
            epaint::text::set_dual_source_blending_supported(true);
            Some(ct_pipeline)
        } else {
            epaint::text::set_dual_source_blending_supported(false);
            None
        };

        const VERTEX_BUFFER_START_CAPACITY: wgpu::BufferAddress =
            (std::mem::size_of::<Vertex>() * 1024) as _;
        const INDEX_BUFFER_START_CAPACITY: wgpu::BufferAddress =
            (std::mem::size_of::<u32>() * 1024 * 3) as _;

        Self {
            pipeline,
            cleartype_pipeline,
            vertex_buffer: SlicedBuffer {
                buffer: create_vertex_buffer(device, VERTEX_BUFFER_START_CAPACITY),
                slices: Vec::with_capacity(64),
                capacity: VERTEX_BUFFER_START_CAPACITY,
            },
            index_buffer: SlicedBuffer {
                buffer: create_index_buffer(device, INDEX_BUFFER_START_CAPACITY),
                slices: Vec::with_capacity(64),
                capacity: INDEX_BUFFER_START_CAPACITY,
            },
            uniform_buffer,
            // Buffers on wgpu are zero initialized, so this is indeed its current state!
            previous_uniform_buffer_content: UniformBuffer {
                screen_size_in_points: [0.0, 0.0],
                dithering: 0,
                _padding: 0,
                cleartype_gamma: 0.0,
                cleartype_contrast: 0.0,
                _padding2: [0, 0],
            },
            uniform_bind_group,
            texture_bind_group_layout,
            textures: HashMap::default(),
            next_user_texture_id: 0,
            samplers: HashMap::default(),
            dithering,
            callback_resources: CallbackResources::default(),
        }
    }

    /// Executes the egui renderer onto an existing wgpu renderpass.
    ///
    /// Note that the lifetime of `render_pass` is `'static` which requires a call to [`wgpu::RenderPass::forget_lifetime`].
    /// This allows users to pass resources that live outside of the callback resources to the render pass.
    /// The render pass internally keeps all referenced resources alive as long as necessary.
    /// The only consequence of `forget_lifetime` is that any operation on the parent encoder will cause a runtime error
    /// instead of a compile time error.
    pub fn render(
        &self,
        render_pass: &mut wgpu::RenderPass<'static>,
        paint_jobs: &[epaint::ClippedPrimitive],
        screen_descriptor: &ScreenDescriptor,
    ) {
        profiling::function_scope!();

        let pixels_per_point = screen_descriptor.pixels_per_point;
        let size_in_pixels = screen_descriptor.size_in_pixels;

        // Whether or not we need to reset the render pass because a paint callback has just
        // run.
        let mut needs_reset = true;
        let subpixel_active =
            self.cleartype_pipeline.is_some() && epaint::text::cleartype_subpixel_active();
        let mut bound_cleartype = false;

        let mut index_buffer_slices = self.index_buffer.slices.iter();
        let mut vertex_buffer_slices = self.vertex_buffer.slices.iter();

        for epaint::ClippedPrimitive {
            clip_rect,
            primitive,
        } in paint_jobs
        {
            if needs_reset {
                render_pass.set_viewport(
                    0.0,
                    0.0,
                    size_in_pixels[0] as f32,
                    size_in_pixels[1] as f32,
                    0.0,
                    1.0,
                );
                render_pass.set_pipeline(&self.pipeline);
                bound_cleartype = false;
                render_pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                needs_reset = false;
            }

            {
                let rect = ScissorRect::new(clip_rect, pixels_per_point, size_in_pixels);

                if rect.width == 0 || rect.height == 0 {
                    // Skip rendering zero-sized clip areas.
                    if let Primitive::Mesh(_) = primitive {
                        // If this is a mesh, we need to advance the index and vertex buffer iterators:
                        index_buffer_slices.next().unwrap();
                        vertex_buffer_slices.next().unwrap();
                    }
                    continue;
                }

                render_pass.set_scissor_rect(rect.x, rect.y, rect.width, rect.height);
            }

            match primitive {
                Primitive::Mesh(mesh) => {
                    let index_buffer_slice = index_buffer_slices.next().unwrap();
                    let vertex_buffer_slice = vertex_buffer_slices.next().unwrap();

                    if let Some(Texture { bind_group, .. }) = self.textures.get(&mesh.texture_id) {
                        let want_cleartype =
                            subpixel_active && mesh.texture_id == epaint::TextureId::Managed(0);
                        if want_cleartype != bound_cleartype {
                            if want_cleartype {
                                if let Some(ct_pipeline) = self.cleartype_pipeline.as_ref() {
                                    render_pass.set_pipeline(ct_pipeline);
                                    bound_cleartype = true;
                                }
                            } else {
                                render_pass.set_pipeline(&self.pipeline);
                                bound_cleartype = false;
                            }
                        }
                        render_pass.set_bind_group(1, bind_group, &[]);
                        render_pass.set_index_buffer(
                            self.index_buffer.buffer.slice(
                                index_buffer_slice.start as u64..index_buffer_slice.end as u64,
                            ),
                            wgpu::IndexFormat::Uint32,
                        );
                        render_pass.set_vertex_buffer(
                            0,
                            self.vertex_buffer.buffer.slice(
                                vertex_buffer_slice.start as u64..vertex_buffer_slice.end as u64,
                            ),
                        );
                        render_pass.draw_indexed(0..mesh.indices.len() as u32, 0, 0..1);
                    } else {
                        log::warn!("Missing texture: {:?}", mesh.texture_id);
                    }
                }
                Primitive::Callback(callback) => {
                    let Some(cbfn) = callback.callback.downcast_ref::<Callback>() else {
                        // We already warned in the `prepare` callback
                        continue;
                    };

                    let info = PaintCallbackInfo {
                        viewport: callback.rect,
                        clip_rect: *clip_rect,
                        pixels_per_point,
                        screen_size_px: size_in_pixels,
                    };

                    let viewport_px = info.viewport_in_pixels();
                    if viewport_px.width_px > 0 && viewport_px.height_px > 0 {
                        profiling::scope!("callback");

                        needs_reset = true;

                        // We're setting a default viewport for the render pass as a
                        // courtesy for the user, so that they don't have to think about
                        // it in the simple case where they just want to fill the whole
                        // paint area.
                        //
                        // The user still has the possibility of setting their own custom
                        // viewport during the paint callback, effectively overriding this
                        // one.
                        render_pass.set_viewport(
                            viewport_px.left_px as f32,
                            viewport_px.top_px as f32,
                            viewport_px.width_px as f32,
                            viewport_px.height_px as f32,
                            0.0,
                            1.0,
                        );

                        cbfn.0.paint(info, render_pass, &self.callback_resources);
                    }
                }
            }
        }

        render_pass.set_scissor_rect(0, 0, size_in_pixels[0], size_in_pixels[1]);
    }

    /// Should be called before [`Self::render`].
    pub fn update_texture(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        id: epaint::TextureId,
        image_delta: &epaint::ImageDelta,
    ) {
        profiling::function_scope!();

        let width = image_delta.image.width() as u32;
        let height = image_delta.image.height() as u32;

        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        let data_color32 = match &image_delta.image {
            epaint::ImageData::Color(image) => {
                assert_eq!(
                    width as usize * height as usize,
                    image.pixels.len(),
                    "Mismatch between texture size and texel count"
                );
                Cow::Borrowed(&image.pixels)
            }
            epaint::ImageData::Font(image) => {
                assert_eq!(
                    width as usize * height as usize,
                    image.pixels.len(),
                    "Mismatch between texture size and texel count"
                );
                profiling::scope!("font -> sRGBA");
                Cow::Owned(image.srgba_pixels(None).collect::<Vec<epaint::Color32>>())
            }
        };
        let data_bytes: &[u8] = bytemuck::cast_slice(data_color32.as_slice());

        let queue_write_data_to_texture = |texture, origin| {
            profiling::scope!("write_texture");
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin,
                    aspect: wgpu::TextureAspect::All,
                },
                data_bytes,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * width),
                    rows_per_image: Some(height),
                },
                size,
            );
        };

        // Use same label for all resources associated with this texture id (no point in retyping the type)
        let label_str = format!("egui_texid_{id:?}");
        let label = Some(label_str.as_str());

        let (texture, origin, bind_group) = if let Some(pos) = image_delta.pos {
            // update the existing texture
            let Texture {
                texture,
                bind_group,
                options,
            } = self
                .textures
                .remove(&id)
                .expect("Tried to update a texture that has not been allocated yet.");
            let texture = texture.expect("Tried to update user texture.");
            let options = options.expect("Tried to update user texture.");
            let origin = wgpu::Origin3d {
                x: pos[0] as u32,
                y: pos[1] as u32,
                z: 0,
            };

            (
                texture,
                origin,
                // If the TextureOptions are the same as the previous ones, we can reuse the bind group. Otherwise we
                // have to recreate it.
                if image_delta.options == options {
                    Some(bind_group)
                } else {
                    None
                },
            )
        } else {
            // allocate a new texture
            let texture = {
                profiling::scope!("create_texture");
                device.create_texture(&wgpu::TextureDescriptor {
                    label,
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba8UnormSrgb, // Minspec for wgpu WebGL emulation is WebGL2, so this should always be supported.
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
                })
            };
            let origin = wgpu::Origin3d::ZERO;
            (texture, origin, None)
        };

        let bind_group = bind_group.unwrap_or_else(|| {
            let sampler = self
                .samplers
                .entry(image_delta.options)
                .or_insert_with(|| create_sampler(image_delta.options, device));
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label,
                layout: &self.texture_bind_group_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(
                            &texture.create_view(&wgpu::TextureViewDescriptor::default()),
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::Sampler(sampler),
                    },
                ],
            })
        });

        queue_write_data_to_texture(&texture, origin);
        self.textures.insert(
            id,
            Texture {
                texture: Some(texture),
                bind_group,
                options: Some(image_delta.options),
            },
        );
    }

    pub fn free_texture(&mut self, id: &epaint::TextureId) {
        if let Some(texture) = self.textures.remove(id).and_then(|t| t.texture) {
            texture.destroy();
        }
    }

    /// Get the WGPU texture and bind group associated to a texture that has been allocated by egui.
    ///
    /// This could be used by custom paint hooks to render images that have been added through
    /// [`epaint::Context::load_texture`](https://docs.rs/egui/latest/egui/struct.Context.html#method.load_texture).
    pub fn texture(&self, id: &epaint::TextureId) -> Option<&Texture> {
        self.textures.get(id)
    }

    /// Registers a [`wgpu::Texture`] with a [`epaint::TextureId`].
    ///
    /// This enables the application to reference the texture inside an image ui element.
    /// This effectively enables off-screen rendering inside the egui UI. Texture must have
    /// the texture format [`wgpu::TextureFormat::Rgba8UnormSrgb`].
    pub fn register_native_texture(
        &mut self,
        device: &wgpu::Device,
        texture: &wgpu::TextureView,
        texture_filter: wgpu::FilterMode,
    ) -> epaint::TextureId {
        self.register_native_texture_with_sampler_options(
            device,
            texture,
            wgpu::SamplerDescriptor {
                label: Some(format!("egui_user_image_{}", self.next_user_texture_id).as_str()),
                mag_filter: texture_filter,
                min_filter: texture_filter,
                ..Default::default()
            },
        )
    }

    /// Registers a [`wgpu::Texture`] with an existing [`epaint::TextureId`].
    ///
    /// This enables applications to reuse [`epaint::TextureId`]s.
    pub fn update_egui_texture_from_wgpu_texture(
        &mut self,
        device: &wgpu::Device,
        texture: &wgpu::TextureView,
        texture_filter: wgpu::FilterMode,
        id: epaint::TextureId,
    ) {
        self.update_egui_texture_from_wgpu_texture_with_sampler_options(
            device,
            texture,
            wgpu::SamplerDescriptor {
                label: Some(format!("egui_user_image_{}", self.next_user_texture_id).as_str()),
                mag_filter: texture_filter,
                min_filter: texture_filter,
                ..Default::default()
            },
            id,
        );
    }

    /// Registers a [`wgpu::Texture`] with a [`epaint::TextureId`] while also accepting custom
    /// [`wgpu::SamplerDescriptor`] options.
    ///
    /// This allows applications to specify individual minification/magnification filters as well as
    /// custom mipmap and tiling options.
    ///
    /// The texture must have the format [`wgpu::TextureFormat::Rgba8UnormSrgb`].
    /// Any compare function supplied in the [`wgpu::SamplerDescriptor`] will be ignored.
    #[allow(clippy::needless_pass_by_value)] // false positive
    pub fn register_native_texture_with_sampler_options(
        &mut self,
        device: &wgpu::Device,
        texture: &wgpu::TextureView,
        sampler_descriptor: wgpu::SamplerDescriptor<'_>,
    ) -> epaint::TextureId {
        profiling::function_scope!();

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            compare: None,
            ..sampler_descriptor
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(format!("egui_user_image_{}", self.next_user_texture_id).as_str()),
            layout: &self.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(texture),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        let id = epaint::TextureId::User(self.next_user_texture_id);
        self.textures.insert(
            id,
            Texture {
                texture: None,
                bind_group,
                options: None,
            },
        );
        self.next_user_texture_id += 1;

        id
    }

    /// Registers a [`wgpu::Texture`] with an existing [`epaint::TextureId`] while also accepting custom
    /// [`wgpu::SamplerDescriptor`] options.
    ///
    /// This allows applications to reuse [`epaint::TextureId`]s created with custom sampler options.
    #[allow(clippy::needless_pass_by_value)] // false positive
    pub fn update_egui_texture_from_wgpu_texture_with_sampler_options(
        &mut self,
        device: &wgpu::Device,
        texture: &wgpu::TextureView,
        sampler_descriptor: wgpu::SamplerDescriptor<'_>,
        id: epaint::TextureId,
    ) {
        profiling::function_scope!();

        let Texture {
            bind_group: user_texture_binding,
            ..
        } = self
            .textures
            .get_mut(&id)
            .expect("Tried to update a texture that has not been allocated yet.");

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            compare: None,
            ..sampler_descriptor
        });

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(format!("egui_user_image_{}", self.next_user_texture_id).as_str()),
            layout: &self.texture_bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(texture),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&sampler),
                },
            ],
        });

        *user_texture_binding = bind_group;
    }

    /// Uploads the uniform, vertex and index data used by the renderer.
    /// Should be called before [`Self::render`].
    ///
    /// Returns all user-defined command buffers gathered from [`CallbackTrait::prepare`] & [`CallbackTrait::finish_prepare`] callbacks.
    pub fn update_buffers(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        paint_jobs: &[epaint::ClippedPrimitive],
        screen_descriptor: &ScreenDescriptor,
    ) -> Vec<wgpu::CommandBuffer> {
        profiling::function_scope!();

        let screen_size_in_points = screen_descriptor.screen_size_in_points();
        let (cleartype_gamma, cleartype_contrast) = epaint::text::cleartype_blend_params();

        let uniform_buffer_content = UniformBuffer {
            screen_size_in_points,
            dithering: u32::from(self.dithering),
            _padding: Default::default(),
            cleartype_gamma,
            cleartype_contrast,
            _padding2: Default::default(),
        };
        if uniform_buffer_content != self.previous_uniform_buffer_content {
            profiling::scope!("update uniforms");
            queue.write_buffer(
                &self.uniform_buffer,
                0,
                bytemuck::cast_slice(&[uniform_buffer_content]),
            );
            self.previous_uniform_buffer_content = uniform_buffer_content;
        }

        // Determine how many vertices & indices need to be rendered, and gather prepare callbacks
        let mut callbacks = Vec::new();
        let (vertex_count, index_count) = {
            profiling::scope!("count_vertices_indices");
            paint_jobs.iter().fold((0, 0), |acc, clipped_primitive| {
                match &clipped_primitive.primitive {
                    Primitive::Mesh(mesh) => {
                        (acc.0 + mesh.vertices.len(), acc.1 + mesh.indices.len())
                    }
                    Primitive::Callback(callback) => {
                        if let Some(c) = callback.callback.downcast_ref::<Callback>() {
                            callbacks.push(c.0.as_ref());
                        } else {
                            log::warn!("Unknown paint callback: expected `egui_wgpu::Callback`");
                        };
                        acc
                    }
                }
            })
        };

        if index_count > 0 {
            profiling::scope!("indices", index_count.to_string().as_str());

            self.index_buffer.slices.clear();

            let required_index_buffer_size = (std::mem::size_of::<u32>() * index_count) as u64;
            if self.index_buffer.capacity < required_index_buffer_size {
                // Resize index buffer if needed.
                self.index_buffer.capacity =
                    (self.index_buffer.capacity * 2).at_least(required_index_buffer_size);
                self.index_buffer.buffer = create_index_buffer(device, self.index_buffer.capacity);
            }

            let index_buffer_staging = queue.write_buffer_with(
                &self.index_buffer.buffer,
                0,
                NonZeroU64::new(required_index_buffer_size).unwrap(),
            );

            let Some(mut index_buffer_staging) = index_buffer_staging else {
                panic!("Failed to create staging buffer for index data. Index count: {index_count}. Required index buffer size: {required_index_buffer_size}. Actual size {} and capacity: {} (bytes)", self.index_buffer.buffer.size(), self.index_buffer.capacity);
            };

            let mut index_offset = 0;
            for epaint::ClippedPrimitive { primitive, .. } in paint_jobs {
                match primitive {
                    Primitive::Mesh(mesh) => {
                        let size = mesh.indices.len() * std::mem::size_of::<u32>();
                        let slice = index_offset..(size + index_offset);
                        index_buffer_staging[slice.clone()]
                            .copy_from_slice(bytemuck::cast_slice(&mesh.indices));
                        self.index_buffer.slices.push(slice);
                        index_offset += size;
                    }
                    Primitive::Callback(_) => {}
                }
            }
        }
        if vertex_count > 0 {
            profiling::scope!("vertices", vertex_count.to_string().as_str());

            self.vertex_buffer.slices.clear();

            let required_vertex_buffer_size = (std::mem::size_of::<Vertex>() * vertex_count) as u64;
            if self.vertex_buffer.capacity < required_vertex_buffer_size {
                // Resize vertex buffer if needed.
                self.vertex_buffer.capacity =
                    (self.vertex_buffer.capacity * 2).at_least(required_vertex_buffer_size);
                self.vertex_buffer.buffer =
                    create_vertex_buffer(device, self.vertex_buffer.capacity);
            }

            let vertex_buffer_staging = queue.write_buffer_with(
                &self.vertex_buffer.buffer,
                0,
                NonZeroU64::new(required_vertex_buffer_size).unwrap(),
            );

            let Some(mut vertex_buffer_staging) = vertex_buffer_staging else {
                panic!("Failed to create staging buffer for vertex data. Vertex count: {vertex_count}. Required vertex buffer size: {required_vertex_buffer_size}. Actual size {} and capacity: {} (bytes)", self.vertex_buffer.buffer.size(), self.vertex_buffer.capacity);
            };

            let mut vertex_offset = 0;
            for epaint::ClippedPrimitive { primitive, .. } in paint_jobs {
                match primitive {
                    Primitive::Mesh(mesh) => {
                        let size = mesh.vertices.len() * std::mem::size_of::<Vertex>();
                        let slice = vertex_offset..(size + vertex_offset);
                        vertex_buffer_staging[slice.clone()]
                            .copy_from_slice(bytemuck::cast_slice(&mesh.vertices));
                        self.vertex_buffer.slices.push(slice);
                        vertex_offset += size;
                    }
                    Primitive::Callback(_) => {}
                }
            }
        }

        let mut user_cmd_bufs = Vec::new();
        {
            profiling::scope!("prepare callbacks");
            for callback in &callbacks {
                user_cmd_bufs.extend(callback.prepare(
                    device,
                    queue,
                    screen_descriptor,
                    encoder,
                    &mut self.callback_resources,
                ));
            }
        }
        {
            profiling::scope!("finish prepare callbacks");
            for callback in &callbacks {
                user_cmd_bufs.extend(callback.finish_prepare(
                    device,
                    queue,
                    encoder,
                    &mut self.callback_resources,
                ));
            }
        }

        user_cmd_bufs
    }
}

fn create_sampler(
    options: epaint::textures::TextureOptions,
    device: &wgpu::Device,
) -> wgpu::Sampler {
    let mag_filter = match options.magnification {
        epaint::textures::TextureFilter::Nearest => wgpu::FilterMode::Nearest,
        epaint::textures::TextureFilter::Linear => wgpu::FilterMode::Linear,
    };
    let min_filter = match options.minification {
        epaint::textures::TextureFilter::Nearest => wgpu::FilterMode::Nearest,
        epaint::textures::TextureFilter::Linear => wgpu::FilterMode::Linear,
    };
    let address_mode = match options.wrap_mode {
        epaint::textures::TextureWrapMode::ClampToEdge => wgpu::AddressMode::ClampToEdge,
        epaint::textures::TextureWrapMode::Repeat => wgpu::AddressMode::Repeat,
        epaint::textures::TextureWrapMode::MirroredRepeat => wgpu::AddressMode::MirrorRepeat,
    };
    device.create_sampler(&wgpu::SamplerDescriptor {
        label: Some(&format!(
            "egui sampler (mag: {mag_filter:?}, min {min_filter:?})"
        )),
        mag_filter,
        min_filter,
        address_mode_u: address_mode,
        address_mode_v: address_mode,
        ..Default::default()
    })
}

fn create_vertex_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    profiling::function_scope!();
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("egui_vertex_buffer"),
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        size,
        mapped_at_creation: false,
    })
}

fn create_index_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    profiling::function_scope!();
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("egui_index_buffer"),
        usage: wgpu::BufferUsages::INDEX | wgpu::BufferUsages::COPY_DST,
        size,
        mapped_at_creation: false,
    })
}

/// A Rect in physical pixel space, used for setting clipping rectangles.
struct ScissorRect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl ScissorRect {
    fn new(clip_rect: &epaint::Rect, pixels_per_point: f32, target_size: [u32; 2]) -> Self {
        // Transform clip rect to physical pixels:
        let clip_min_x = pixels_per_point * clip_rect.min.x;
        let clip_min_y = pixels_per_point * clip_rect.min.y;
        let clip_max_x = pixels_per_point * clip_rect.max.x;
        let clip_max_y = pixels_per_point * clip_rect.max.y;

        // Round to integer:
        let clip_min_x = clip_min_x.round() as u32;
        let clip_min_y = clip_min_y.round() as u32;
        let clip_max_x = clip_max_x.round() as u32;
        let clip_max_y = clip_max_y.round() as u32;

        // Clamp:
        let clip_min_x = clip_min_x.clamp(0, target_size[0]);
        let clip_min_y = clip_min_y.clamp(0, target_size[1]);
        let clip_max_x = clip_max_x.clamp(clip_min_x, target_size[0]);
        let clip_max_y = clip_max_y.clamp(clip_min_y, target_size[1]);

        Self {
            x: clip_min_x,
            y: clip_min_y,
            width: clip_max_x - clip_min_x,
            height: clip_max_y - clip_min_y,
        }
    }
}

// Look at the feature flag for an explanation.
#[cfg(not(all(
    target_arch = "wasm32",
    not(feature = "fragile-send-sync-non-atomic-wasm"),
)))]
#[test]
fn renderer_impl_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Renderer>();
}

#[cfg(test)]
mod cleartype_tests {
    use super::*;

    fn cpu_cleartype_gamma_blend(
        v: [f32; 4],
        tex_rgb: [f32; 3],
        dst: [f32; 3],
        gamma: f32,
        enhanced_contrast: f32,
    ) -> ([f32; 4], [f32; 4], [f32; 3]) {
        let gamma = gamma.max(1.0);
        let enhanced_contrast = enhanced_contrast.max(0.0);
        let alpha_denom = v[3].max(1e-5);
        let fg = [
            (v[0] / alpha_denom).clamp(0.0, 1.0),
            (v[1] / alpha_denom).clamp(0.0, 1.0),
            (v[2] / alpha_denom).clamp(0.0, 1.0),
        ];
        let lum = fg[0] * 0.299 + fg[1] * 0.587 + fg[2] * 0.114;

        let mut eff = [0.0_f32; 3];
        for i in 0..3 {
            let t = tex_rgb[i];
            let eff_dark = (t + (enhanced_contrast * 0.4) * t * (1.0 - t)).clamp(0.0, 1.0);
            let eff_light = if t <= 0.0 {
                0.0
            } else {
                t.max(1e-6).powf(1.0 / gamma)
            };
            eff[i] = eff_dark * (1.0 - lum) + eff_light * lum;
        }
        let max_eff = eff[0].max(eff[1]).max(eff[2]);
        let out_color = [v[0] * eff[0], v[1] * eff[1], v[2] * eff[2], v[3] * max_eff];
        let out_mask = [v[3] * eff[0], v[3] * eff[1], v[3] * eff[2], v[3] * max_eff];
        let blended = [
            out_color[0] + (1.0 - out_mask[0]) * dst[0],
            out_color[1] + (1.0 - out_mask[1]) * dst[1],
            out_color[2] + (1.0 - out_mask[2]) * dst[2],
        ];
        (out_color, out_mask, blended)
    }

    #[test]
    fn uniform_buffer_layout_and_cleartype_eq() {
        assert_eq!(std::mem::size_of::<UniformBuffer>(), 32);
        let a = UniformBuffer {
            screen_size_in_points: [1280.0, 720.0],
            dithering: 1,
            _padding: 0,
            cleartype_gamma: 1.8,
            cleartype_contrast: 0.5,
            _padding2: [0, 0],
        };
        let mut b = a;
        assert_eq!(a, b);
        b.cleartype_gamma = 2.2;
        assert_ne!(a, b);
        b.cleartype_gamma = 1.8;
        b.cleartype_contrast = 1.0;
        assert_ne!(a, b);
    }

    #[test]
    fn cleartype_wgsl_dual_source_contract() {
        let src = include_str!("egui_cleartype.wgsl");
        assert!(
            !src.lines()
                .any(|line| line.trim_start().starts_with("enable ")),
            "naga 24.0 rejects `enable dual_source_blending;`"
        );
        assert!(src.contains("@location(0) color: vec4<f32>"));
        assert!(src.contains("@location(0) @second_blend_source mask: vec4<f32>"));
        assert!(src.contains("fn fs_cleartype_gamma_framebuffer("));
        assert!(src.contains("fn fs_cleartype_linear_framebuffer("));
        assert!(src.contains("cleartype_gamma: f32"));
        assert!(src.contains("cleartype_contrast: f32"));
    }

    #[test]
    fn cleartype_blend_invariants_and_subpixel_weighting() {
        let dst = [0.85, 0.90, 0.95];

        // 1. Opaque untextured solid shape tex=(1,1,1) matches standard blend identically.
        let (_, _, solid_opaque) =
            cpu_cleartype_gamma_blend([0.2, 0.4, 0.6, 1.0], [1.0, 1.0, 1.0], dst, 1.8, 0.5);
        for (got, want) in solid_opaque.iter().zip([0.2_f32, 0.4, 0.6]) {
            assert!((got - want).abs() < 1e-6);
        }

        // 2. Semi-transparent solid shape tex=(1,1,1) matches premultiplied over identically.
        let alpha = 0.4_f32;
        let v = [0.2 * alpha, 0.5 * alpha, 0.8 * alpha, alpha];
        let (_, _, solid_semi) = cpu_cleartype_gamma_blend(v, [1.0, 1.0, 1.0], dst, 1.8, 0.5);
        for i in 0..3 {
            let expected = v[i] + (1.0 - alpha) * dst[i];
            assert!((solid_semi[i] - expected).abs() < 1e-6);
        }

        // 3. Additive shape (v.a == 0) with tex=(1,1,1) preserves dst + v.rgb identically.
        let (_, mask_add, solid_add) =
            cpu_cleartype_gamma_blend([0.1, 0.2, 0.3, 0.0], [1.0, 1.0, 1.0], dst, 1.8, 0.5);
        assert!(mask_add.iter().all(|m| *m == 0.0));
        for i in 0..3 {
            let expected = [0.1, 0.2, 0.3][i] + dst[i];
            assert!((solid_add[i] - expected).abs() < 1e-6);
        }

        // 4. Empty glyph texel tex=(0,0,0) leaves destination completely unchanged and NaN-free.
        let (col0, mask0, empty) =
            cpu_cleartype_gamma_blend([0.0, 0.0, 0.0, 1.0], [0.0, 0.0, 0.0], dst, 1.8, 0.5);
        assert!(col0.iter().all(|c| *c == 0.0));
        assert!(mask0.iter().all(|m| *m == 0.0));
        assert_eq!(empty, dst);

        // 5. Luminance-aware weighting: white text on dark lifts mid-coverage more than dark on light.
        let (_, mask_dark, _) =
            cpu_cleartype_gamma_blend([0.0, 0.0, 0.0, 1.0], [0.5, 0.5, 0.5], [1.0; 3], 1.8, 0.5);
        let (_, mask_light, _) =
            cpu_cleartype_gamma_blend([1.0, 1.0, 1.0, 1.0], [0.5, 0.5, 0.5], [0.0; 3], 1.8, 0.5);
        assert!(mask_dark[0] > 0.54 && mask_dark[0] < 0.56);
        assert!(mask_light[0] > mask_dark[0] + 0.1);

        // 6. Asymmetric subpixel mask blends R, G, B channels independently.
        let (_, mask_sub, blended_sub) =
            cpu_cleartype_gamma_blend([0.0, 0.0, 0.0, 1.0], [1.0, 0.5, 0.0], [1.0; 3], 1.8, 0.5);
        assert!((mask_sub[0] - 1.0).abs() < 1e-6);
        assert!(mask_sub[1] > 0.5 && mask_sub[1] < 0.6);
        assert!(mask_sub[2].abs() < 1e-6);
        assert!(blended_sub[0].abs() < 1e-6);
        assert!(blended_sub[1] > 0.4 && blended_sub[1] < 0.5);
        assert!((blended_sub[2] - 1.0).abs() < 1e-6);
    }
}
