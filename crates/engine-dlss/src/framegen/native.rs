//! Frame-generation unsafe boundary: borrowed wgpu-hal handles of private
//! resources passed to native/ngx_dlssg.cpp, which is compiled against
//! NVIDIA's headers. Raw pointers never leave this module.
use super::{
    ColorFormat, FrameGenerationOptions, FrameGenerationStatus, Generated, GenerationFrame,
    GenerationInputs,
};
use crate::{Context, Error, Result, SessionOptions};
use ash::vk::{self, Handle};
use std::{ffi::c_void, ptr::NonNull};

#[repr(C)]
struct Image {
    image: u64,
    view: u64,
    width: u32,
    height: u32,
    format: u32,
}
#[repr(C)]
#[derive(Default)]
struct Caps {
    available: i32,
    needs_updated_driver: i32,
    init_result: i32,
    runtime_missing: i32,
    min_driver_major: u32,
    min_driver_minor: u32,
    multi_frame_max: u32,
}
#[repr(C)]
struct NativeFrame {
    view_to_clip: [f32; 16],
    clip_to_view: [f32; 16],
    clip_to_lens_clip: [f32; 16],
    clip_to_prev_clip: [f32; 16],
    prev_clip_to_clip: [f32; 16],
    jitter: [f32; 2],
    motion_scale: [f32; 2],
    pinhole: [f32; 2],
    position: [f32; 3],
    up: [f32; 3],
    right: [f32; 3],
    forward: [f32; 3],
    near_plane: f32,
    far_plane: f32,
    fov: f32,
    aspect: f32,
    reset: u32,
    depth_inverted: u32,
    camera_motion_included: u32,
    hdr: u32,
    orthographic: u32,
    multi_frame_count: u32,
    multi_frame_index: u32,
    frame_id: u64,
}
#[repr(C)]
struct Resources {
    images: [Image; 5],
    disable_buffer: u64,
    disable_size: u32,
}
unsafe extern "C" {
    fn engine_ngx_api_version() -> u32;
    fn engine_ngx_create_fg(
        context: *mut c_void,
        width: u32,
        height: u32,
        render_width: u32,
        render_height: u32,
        format: u32,
        hudless: u32,
        out: *mut *mut c_void,
        caps: *mut Caps,
    ) -> u32;
    fn engine_ngx_create_fg_feature(generator: *mut c_void, command: u64) -> u32;
    fn engine_ngx_evaluate_fg(
        generator: *mut c_void,
        command: u64,
        resources: *const Resources,
        frame: *const NativeFrame,
    ) -> u32;
    fn engine_ngx_fg_multi_frame_max(generator: *mut c_void) -> u32;
    fn engine_ngx_close_fg(generator: *mut c_void);
}

struct Bridge(NonNull<c_void>);
impl Drop for Bridge {
    fn drop(&mut self) {
        // SAFETY: sole owner; FrameGenerator::drop waited for all submitted
        // work, and the retained Context keeps NGX initialized until after.
        unsafe { engine_ngx_close_fg(self.0.as_ptr()) }
    }
}

struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    format: vk::Format,
}
impl Target {
    fn new(
        device: &wgpu::Device,
        label: &str,
        size: [u32; 2],
        format: wgpu::TextureFormat,
        native: vk::Format,
        output: bool,
    ) -> Self {
        let mut usage = wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::COPY_SRC
            | wgpu::TextureUsages::RENDER_ATTACHMENT;
        if output {
            usage |= wgpu::TextureUsages::STORAGE_BINDING;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: extent(size),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        Self {
            texture,
            view,
            format: native,
        }
    }
    fn native(&self) -> Result<Image> {
        // SAFETY: private texture on the generator's live device. Handles are
        // extracted one guard at a time because wgpu's snatch lock is not
        // recursive; the generator owns the resources through GPU completion.
        let image = {
            let hal = unsafe { self.texture.as_hal::<wgpu::hal::api::Vulkan>() }
                .ok_or_else(|| Error::Gpu("frame generation texture unavailable".into()))?;
            unsafe { hal.raw_handle().as_raw() }
        };
        // SAFETY: same ownership and lifetime as the image above.
        let view = {
            let hal = unsafe { self.view.as_hal::<wgpu::hal::api::Vulkan>() }
                .ok_or_else(|| Error::Gpu("frame generation view unavailable".into()))?;
            unsafe { hal.raw_handle().as_raw() }
        };
        Ok(Image {
            image,
            view,
            width: self.texture.width(),
            height: self.texture.height(),
            format: self.format.as_raw() as u32,
        })
    }
}

const BACKBUFFER: usize = 0;
const DEPTH: usize = 1;
const MOTION: usize = 2;
const HUDLESS: usize = 3;
const OUTPUT: usize = 4;
const DISABLE_SIZE: u64 = 4;

/// A thread-confined NGX DLSS Frame Generation feature with private native
/// resources. Every evaluation copies caller inputs into private textures,
/// records NGX, copies the generated frame into the caller's texture, then
/// submits and waits. Presentation and pacing remain the caller's job.
/// Size, format or HUD-less changes require a new generator.
pub struct FrameGenerator {
    bridge: Bridge,
    device: wgpu::Device,
    queue: wgpu::Queue,
    targets: [Target; 5],
    disable: wgpu::Buffer,
    readback: wgpu::Buffer,
    options: FrameGenerationOptions,
    status: FrameGenerationStatus,
    failure: Option<Error>,
    /// Whether the native feature exists (created on the first evaluation).
    created: bool,
    history: bool,
    /// Reset decision of the current real-frame pair, shared by its indices.
    pair_reset: bool,
    frame_id: u64,
    last: Option<super::Interpolation>,
    context: Context,
}
impl FrameGenerator {
    /// Initialize the shared NGX runtime from SR session options (which name
    /// the runtime/log directories) and create a frame generator on it.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        session: &SessionOptions,
        options: FrameGenerationOptions,
    ) -> Result<Self> {
        validate_runtime(&session.runtime_dir)?;
        Self::with_context(Context::new(device, queue, session)?, options)
    }
    /// Query the driver's FG capability and allocate private resources. The
    /// native feature itself is created on the first evaluation, which has a
    /// command buffer.
    pub fn with_context(context: Context, options: FrameGenerationOptions) -> Result<Self> {
        let device = context.device().clone();
        let queue = context.queue().clone();
        options.validate(device.limits().max_texture_dimension_2d)?;
        let (color, native_color) = match options.color_format {
            ColorFormat::Rgba8Unorm => {
                (wgpu::TextureFormat::Rgba8Unorm, vk::Format::R8G8B8A8_UNORM)
            }
            ColorFormat::Rgba16Float => (
                wgpu::TextureFormat::Rgba16Float,
                vk::Format::R16G16B16A16_SFLOAT,
            ),
        };
        let mut raw = std::ptr::null_mut();
        let mut caps = Caps::default();
        // SAFETY: the shared context is initialized, thread confined and
        // retained by this generator; output storage lives through the call.
        let code = unsafe {
            engine_ngx_create_fg(
                context.native_handle(),
                options.output_size[0],
                options.output_size[1],
                options.render_size[0],
                options.render_size[1],
                native_color.as_raw() as u32,
                u32::from(options.hudless),
                &mut raw,
                &mut caps,
            )
        };
        if caps.runtime_missing != 0 {
            return Err(Error::Unavailable(format!(
                "NGX could not find the DLSS Frame Generation runtime ({FG_RUNTIME}) in the context's \
                 runtime directory (FrameGeneration.FeatureInitResult=0x{:08x}); install NVIDIA's \
                 official DLSS SDK runtime beside the SR runtime",
                caps.init_result as u32
            )));
        }
        if code != 0 {
            return Err(Error::Unavailable(format!(
                "DLSS Frame Generation unavailable on {} (Vulkan driver 0x{:08x}): NGX result 0x{code:08x}, \
                 FrameGeneration.Available={}, NeedsUpdatedDriver={}, minimum driver {}.{}, FeatureInitResult=0x{:08x}",
                context.device_name(),
                context.driver_version(),
                caps.available,
                caps.needs_updated_driver,
                caps.min_driver_major,
                caps.min_driver_minor,
                caps.init_result as u32,
            )));
        }
        let bridge = Bridge(
            NonNull::new(raw)
                .ok_or_else(|| Error::Unavailable("NGX returned a null FG bridge".into()))?,
        );
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let (output, render) = (options.output_size, options.render_size);
        let targets = [
            Target::new(
                &device,
                "DLSS-G backbuffer",
                output,
                color,
                native_color,
                false,
            ),
            Target::new(
                &device,
                "DLSS-G depth",
                render,
                wgpu::TextureFormat::R32Float,
                vk::Format::R32_SFLOAT,
                false,
            ),
            Target::new(
                &device,
                "DLSS-G motion",
                render,
                wgpu::TextureFormat::Rg16Float,
                vk::Format::R16G16_SFLOAT,
                false,
            ),
            Target::new(
                &device,
                "DLSS-G HUD-less",
                if options.hudless { output } else { [1, 1] },
                color,
                native_color,
                false,
            ),
            Target::new(
                &device,
                "DLSS-G generated",
                output,
                color,
                native_color,
                true,
            ),
        ];
        let disable = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("DLSS-G disable interpolation"),
            size: DISABLE_SIZE,
            usage: wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("DLSS-G disable readback"),
            size: DISABLE_SIZE,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(Error::Gpu(format!("DLSS-G resource creation: {error}")));
        }
        let status = FrameGenerationStatus {
            device_name: context.device_name().into(),
            driver_version: context.driver_version(),
            minimum_driver: [caps.min_driver_major, caps.min_driver_minor],
            multi_frame_max: caps.multi_frame_max.max(1),
            // SAFETY: no pointers or runtime state; returns a header constant.
            sdk_api_version: unsafe { engine_ngx_api_version() },
            output_size: output,
            render_size: render,
            color_format: options.color_format,
            evaluated_frames: 0,
        };
        Ok(Self {
            bridge,
            device,
            queue,
            targets,
            disable,
            readback,
            options,
            status,
            failure: None,
            created: false,
            history: false,
            pair_reset: true,
            frame_id: 0,
            last: None,
            context,
        })
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
    pub fn status(&self) -> &FrameGenerationStatus {
        &self.status
    }
    /// Generate the frame between the previous real frame and `inputs` (the
    /// current real frame) into `output`, which needs `COPY_DST`, the color
    /// format and the output size. Producers of `inputs` must be submitted to
    /// this generator's queue first. A native or GPU failure poisons the
    /// generator; recreate it before retrying.
    pub fn evaluate(
        &mut self,
        inputs: &GenerationInputs<'_>,
        frame: &GenerationFrame,
        output: &wgpu::Texture,
    ) -> Result<Generated> {
        frame.validate()?;
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        self.check_order(frame.interpolation)?;
        self.check_inputs(inputs, output)?;
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = self.evaluate_inner(inputs, frame, output);
        let result = match pollster::block_on(scope.pop()) {
            Some(error) => Err(Error::Gpu(format!("DLSS-G GPU validation: {error}"))),
            None => result,
        };
        match &result {
            Ok(_) => {
                self.status.evaluated_frames += 1;
                self.history = true;
                self.last = Some(frame.interpolation);
                if frame.interpolation.index == 1 {
                    self.frame_id += 1;
                }
            }
            Err(error) => self.failure = Some(error.clone()),
        }
        result
    }
    fn check_order(&self, interpolation: super::Interpolation) -> Result<()> {
        let follows = |last: super::Interpolation| {
            last.count == interpolation.count && last.index + 1 == interpolation.index
        };
        if interpolation.index > 1 && !self.last.is_some_and(follows) {
            return Err(Error::InvalidInput(format!(
                "generated frame {} of {} must follow frame {} of the same real-frame pair",
                interpolation.index,
                interpolation.count,
                interpolation.index - 1
            )));
        }
        if interpolation.count > self.status.multi_frame_max {
            return Err(Error::Unavailable(format!(
                "{} generated frames requested; NGX reports at most {}",
                interpolation.count, self.status.multi_frame_max
            )));
        }
        Ok(())
    }
    fn check_inputs(&self, inputs: &GenerationInputs<'_>, output: &wgpu::Texture) -> Result<()> {
        if inputs.hudless.is_some() != self.options.hudless {
            return Err(Error::InvalidInput(format!(
                "this generator was created with hudless={}; supply the HUD-less input exactly then",
                self.options.hudless
            )));
        }
        let color = self.targets[BACKBUFFER].texture.format();
        let (out, render) = (self.options.output_size, self.options.render_size);
        let copy_src = wgpu::TextureUsages::COPY_SRC;
        let mut checks = vec![
            ("backbuffer", inputs.backbuffer, out, color, copy_src),
            (
                "depth",
                inputs.depth,
                render,
                wgpu::TextureFormat::R32Float,
                copy_src,
            ),
            (
                "motion",
                inputs.motion,
                render,
                wgpu::TextureFormat::Rg16Float,
                copy_src,
            ),
            ("output", output, out, color, wgpu::TextureUsages::COPY_DST),
        ];
        if let Some(hudless) = inputs.hudless {
            checks.push(("HUD-less", hudless, out, color, copy_src));
        }
        for (name, texture, size, format, usage) in checks {
            if texture.size() != extent(size)
                || texture.format() != format
                || texture.mip_level_count() != 1
                || texture.sample_count() != 1
                || texture.dimension() != wgpu::TextureDimension::D2
                || !texture.usage().contains(usage)
            {
                return Err(Error::InvalidInput(format!(
                    "DLSS-G {name} must be a single-sample 2D {format:?} {}x{} texture with {usage:?}",
                    size[0], size[1]
                )));
            }
            // SAFETY: only testing liveness; no raw handle escapes the guard.
            if unsafe { texture.as_hal::<wgpu::hal::api::Vulkan>() }.is_none() {
                return Err(Error::InvalidInput(format!(
                    "DLSS-G {name} texture was destroyed"
                )));
            }
        }
        Ok(())
    }
    fn evaluate_inner(
        &mut self,
        inputs: &GenerationInputs<'_>,
        frame: &GenerationFrame,
        output: &wgpu::Texture,
    ) -> Result<Generated> {
        let first = frame.interpolation.index == 1;
        let reset = if first {
            frame.reset || !self.history
        } else {
            self.pair_reset || frame.reset
        };
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("DLSS-G input copies"),
            });
        let sources = [
            (Some(inputs.backbuffer), BACKBUFFER),
            (Some(inputs.depth), DEPTH),
            (Some(inputs.motion), MOTION),
            (inputs.hudless, HUDLESS),
        ];
        for (source, index) in sources {
            if let Some(source) = source {
                let destination = &self.targets[index].texture;
                encoder.copy_texture_to_texture(
                    source.as_image_copy(),
                    destination.as_image_copy(),
                    destination.size(),
                );
            }
        }
        // Make the private destination initialized in wgpu's tracker before
        // native writes; otherwise a later copy could lazily zero NGX's result.
        // The HUD-less placeholder is cleared for the same reason when unused.
        let mut clears = vec![OUTPUT];
        if !self.options.hudless {
            clears.push(HUDLESS);
        }
        for index in clears {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("initialize DLSS-G target"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets[index].view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        encoder.clear_buffer(&self.disable, 0, None);
        encoder.transition_resources(
            std::iter::once(wgpu::BufferTransition {
                buffer: &self.disable,
                state: wgpu::BufferUses::STORAGE_READ_WRITE,
            }),
            self.targets
                .iter()
                .enumerate()
                .map(|(i, t)| wgpu::TextureTransition {
                    texture: &t.texture,
                    selector: None,
                    state: if i == OUTPUT {
                        wgpu::TextureUses::STORAGE_READ_WRITE
                    } else {
                        wgpu::TextureUses::RESOURCE
                    },
                }),
        );
        let transitions = encoder.finish();
        let mut images = Vec::with_capacity(5);
        for target in &self.targets {
            images.push(target.native()?);
        }
        // SAFETY: private buffer on our live device, retained through completion.
        let disable_buffer = {
            let hal = unsafe { self.disable.as_hal::<wgpu::hal::api::Vulkan>() }
                .ok_or_else(|| Error::Gpu("DLSS-G flag buffer unavailable".into()))?;
            unsafe { hal.raw_handle().as_raw() }
        };
        let resources = Resources {
            images: images
                .try_into()
                .map_err(|_| Error::Gpu("DLSS-G resource count".into()))?,
            disable_buffer,
            disable_size: DISABLE_SIZE as u32,
        };
        if !self.created {
            self.create_feature()?;
        }
        let frame_id = self.frame_id + u64::from(first);
        let native_frame = native_frame(frame, reset, frame_id);
        // wgpu 30 forbids mixing raw and wgpu commands in one encoder; the
        // tracked transitions are submitted immediately before this buffer.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("DLSS-G native commands"),
            });
        // SAFETY: no pass is open; native code records into this command buffer
        // only, never ends it, and leaves resources in the states transitioned
        // above. All referenced resources are private and live through the
        // synchronous submission below.
        unsafe {
            encoder.as_hal_mut::<wgpu::hal::api::Vulkan, _, _>(|hal| {
                let hal =
                    hal.ok_or_else(|| Error::Gpu("Vulkan command encoder unavailable".into()))?;
                check(
                    "frame generation evaluation",
                    engine_ngx_evaluate_fg(
                        self.bridge.0.as_ptr(),
                        hal.raw_handle().as_raw(),
                        &resources,
                        &native_frame,
                    ),
                )
            })
        }?;
        let native = encoder.finish();
        let mut resolve = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("DLSS-G output copy"),
            });
        resolve.copy_texture_to_texture(
            self.targets[OUTPUT].texture.as_image_copy(),
            output.as_image_copy(),
            output.size(),
        );
        resolve.copy_buffer_to_buffer(&self.disable, 0, &self.readback, 0, DISABLE_SIZE);
        let index = self.queue.submit([transitions, native, resolve.finish()]);
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            })
            .map_err(|e| Error::Gpu(format!("DLSS-G GPU completion: {e}")))?;
        let disabled = self.read_disable_flag()?;
        self.pair_reset = reset;
        Ok(if reset {
            Generated::NoHistory
        } else if disabled {
            Generated::Disabled
        } else {
            Generated::Presentable
        })
    }
    /// Record NGX feature creation in its own submission and wait for it, so
    /// the first evaluation sees a completed feature.
    fn create_feature(&mut self) -> Result<()> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("DLSS-G feature creation"),
            });
        // SAFETY: no pass is open; native code only records into this command
        // buffer, which is submitted and completed before it is reused.
        unsafe {
            encoder.as_hal_mut::<wgpu::hal::api::Vulkan, _, _>(|hal| {
                let hal =
                    hal.ok_or_else(|| Error::Gpu("Vulkan command encoder unavailable".into()))?;
                check(
                    "frame generation feature creation",
                    engine_ngx_create_fg_feature(self.bridge.0.as_ptr(), hal.raw_handle().as_raw()),
                )
            })
        }?;
        let index = self.queue.submit([encoder.finish()]);
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            })
            .map_err(|e| Error::Gpu(format!("DLSS-G feature creation completion: {e}")))?;
        self.created = true;
        // SAFETY: live, uniquely owned bridge; reads its own parameter map.
        self.status.multi_frame_max =
            unsafe { engine_ngx_fg_multi_frame_max(self.bridge.0.as_ptr()) };
        Ok(())
    }
    fn read_disable_flag(&self) -> Result<bool> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .map_err(|e| Error::Gpu(format!("DLSS-G flag readback: {e}")))?;
        rx.recv()
            .map_err(|_| Error::Gpu("DLSS-G flag readback dropped".into()))?
            .map_err(|e| Error::Gpu(format!("DLSS-G flag map: {e}")))?;
        let disabled = {
            let data = self
                .readback
                .slice(..)
                .get_mapped_range()
                .map_err(|e| Error::Gpu(format!("DLSS-G flag range: {e}")))?;
            data[0] != 0
        };
        self.readback.unmap();
        Ok(disabled)
    }
}
impl Drop for FrameGenerator {
    fn drop(&mut self) {
        // NGX feature objects cannot be released while GPU work uses them.
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
    }
}

/// Official runtime file NGX loads for DLSS-G. Linux packaging of DLSS-G is not
/// validated here; NGX reports a missing runtime through FeatureInitResult.
const FG_RUNTIME: &str = if cfg!(windows) {
    "nvngx_dlssg.dll"
} else {
    "the NGX DLSS-G library"
};
fn validate_runtime(directory: &std::path::Path) -> Result<()> {
    if !cfg!(windows) {
        return Ok(());
    }
    let path = directory.join(FG_RUNTIME);
    let mut magic = [0u8; 2];
    use std::io::Read;
    std::fs::File::open(&path)
        .and_then(|mut file| file.read_exact(&mut magic))
        .map_err(|e| {
            Error::Unavailable(format!(
                "official DLSS Frame Generation runtime missing or unreadable: {} ({e})",
                path.display()
            ))
        })?;
    if magic != *b"MZ" {
        return Err(Error::InvalidInput(format!(
            "{} is not a native Windows library",
            path.display()
        )));
    }
    Ok(())
}
fn native_frame(frame: &GenerationFrame, reset: bool, frame_id: u64) -> NativeFrame {
    let c = &frame.camera;
    let flat = |m: [[f32; 4]; 4]| -> [f32; 16] {
        let mut out = [0.; 16];
        for (row, values) in m.iter().enumerate() {
            out[row * 4..row * 4 + 4].copy_from_slice(values);
        }
        out
    };
    NativeFrame {
        view_to_clip: flat(c.view_to_clip),
        clip_to_view: flat(c.clip_to_view),
        clip_to_lens_clip: flat(c.clip_to_lens_clip),
        clip_to_prev_clip: flat(c.clip_to_prev_clip),
        prev_clip_to_clip: flat(c.prev_clip_to_clip),
        jitter: frame.jitter,
        motion_scale: frame.motion_scale,
        pinhole: c.pinhole_offset,
        position: c.position,
        up: c.up,
        right: c.right,
        forward: c.forward,
        near_plane: c.near,
        far_plane: c.far,
        fov: c.fov_y,
        aspect: c.aspect,
        reset: u32::from(reset),
        depth_inverted: u32::from(frame.depth_inverted),
        camera_motion_included: u32::from(frame.camera_motion_included),
        hdr: u32::from(frame.hdr),
        orthographic: u32::from(frame.orthographic),
        multi_frame_count: frame.interpolation.count,
        multi_frame_index: frame.interpolation.index,
        frame_id,
    }
}
fn extent(size: [u32; 2]) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width: size[0],
        height: size[1],
        depth_or_array_layers: 1,
    }
}
fn check(operation: &'static str, code: u32) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::Native { operation, code })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    #[cfg(windows)]
    fn rejects_missing_and_malformed_frame_generation_runtime() {
        let folder =
            std::env::temp_dir().join(format!("engine-dlss-fg-runtime-{}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("temporary directory");
        let error = validate_runtime(&folder).expect_err("missing runtime");
        assert!(matches!(&error, Error::Unavailable(m) if m.contains("nvngx_dlssg.dll")));
        let library = folder.join("nvngx_dlssg.dll");
        std::fs::write(&library, b"not a library").expect("test library");
        assert!(matches!(
            validate_runtime(&folder),
            Err(Error::InvalidInput(_))
        ));
        std::fs::remove_file(library).expect("remove test library");
        std::fs::remove_dir(folder).expect("remove temporary directory");
    }
    #[test]
    fn native_frame_flattens_rows_and_carries_flags() {
        let mut frame = GenerationFrame::default();
        frame.camera.clip_to_prev_clip[1][2] = 7.;
        frame.interpolation = crate::Interpolation { index: 2, count: 3 };
        let native = native_frame(&frame, true, 9);
        assert_eq!(native.clip_to_prev_clip[6], 7.);
        assert_eq!(native.reset, 1);
        assert_eq!((native.multi_frame_index, native.multi_frame_count), (2, 3));
        assert_eq!(native.frame_id, 9);
    }
}
