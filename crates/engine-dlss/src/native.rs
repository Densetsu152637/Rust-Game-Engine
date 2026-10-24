//! The only Rust unsafe boundary: borrowed wgpu-hal handles passed to a C ABI
//! compiled against NVIDIA's headers. Raw pointers never leave this module.
use crate::{Error, Feature, Frame, Quality, Result, SessionOptions, Status};
use ash::vk::{self, Handle};
use std::{
    ffi::{CString, c_char, c_void},
    ptr::NonNull,
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(windows)]
type Wide = u16;
#[cfg(not(windows))]
type Wide = u32;

#[repr(C)]
struct Image {
    image: u64,
    view: u64,
    width: u32,
    height: u32,
    format: u32,
}
#[repr(C)]
struct NativeFrame {
    jitter_x: f32,
    jitter_y: f32,
    motion_x: f32,
    motion_y: f32,
    delta_ms: f32,
    reset: u32,
}
unsafe extern "C" {
    fn engine_ngx_api_version() -> u32;
    fn engine_ngx_create_sr(context: *mut c_void, out: *mut *mut c_void) -> u32;
    fn engine_ngx_close_context(context: *mut c_void);
    fn engine_ngx_device_extensions(
        instance: u64,
        physical: u64,
        feature: u32,
        project: *const c_char,
        logs: *const Wide,
        plugins: *const Wide,
        names: *mut c_char,
        capacity: u32,
        count: *mut u32,
    ) -> u32;
    fn engine_ngx_open(
        instance: u64,
        physical: u64,
        device: u64,
        gipa: vk::PFN_vkGetInstanceProcAddr,
        gdpa: vk::PFN_vkGetDeviceProcAddr,
        project: *const c_char,
        logs: *const Wide,
        plugins: *const Wide,
        instance_extensions: *const *const c_char,
        instance_count: u32,
        device_extensions: *const *const c_char,
        device_count: u32,
        out: *mut *mut c_void,
        missing: *mut c_char,
        missing_capacity: u32,
    ) -> u32;
    fn engine_ngx_settings(
        session: *mut c_void,
        width: u32,
        height: u32,
        quality: i32,
        render_width: *mut u32,
        render_height: *mut u32,
        driver_major: *mut u32,
        driver_minor: *mut u32,
    ) -> u32;
    fn engine_ngx_evaluate(
        session: *mut c_void,
        command: u64,
        quality: i32,
        images: *const Image,
        frame: *const NativeFrame,
    ) -> u32;
    fn engine_ngx_close(session: *mut c_void);
}
static SESSION_ACTIVE: AtomicBool = AtomicBool::new(false);
struct Exclusive;
impl Drop for Exclusive {
    fn drop(&mut self) {
        SESSION_ACTIVE.store(false, Ordering::Release);
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
            size: wgpu::Extent3d {
                width: size[0],
                height: size[1],
                depth_or_array_layers: 1,
            },
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
}
struct Bridge(NonNull<c_void>);
impl Drop for Bridge {
    fn drop(&mut self) {
        // SAFETY: sole owner, synchronous evaluation and Session::drop wait for
        // submitted work; native close releases its objects in dependency order.
        unsafe {
            engine_ngx_close(self.0.as_ptr());
        }
    }
}

struct RuntimeBridge(NonNull<c_void>);
impl Drop for RuntimeBridge {
    fn drop(&mut self) {
        // SAFETY: the last Context reference is gone; feature owners retain a
        // Context, so all per-feature handles are already released.
        unsafe {
            engine_ngx_close_context(self.0.as_ptr());
        }
    }
}
struct Runtime {
    bridge: RuntimeBridge,
    device: wgpu::Device,
    queue: wgpu::Queue,
    device_name: String,
    driver_version: u32,
    _exclusive: Exclusive,
}
impl Drop for Runtime {
    fn drop(&mut self) {
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
    }
}
/// One thread-confined NGX runtime shared by SR, RR and frame-generation features.
/// Cloning this context retains the device/runtime; each feature owns independent
/// parameter maps and feature handles. Calls are serialized on its owner thread.
#[derive(Clone)]
pub struct Context {
    inner: Rc<Runtime>,
}
impl Context {
    /// Device/queue identity is checked before NGX is invoked. wgpu's native
    /// device and instance ownership are retained by the cloned device handle.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        options: &SessionOptions,
    ) -> Result<Self> {
        validate_options(options, device.limits().max_texture_dimension_2d)?;
        let project = CString::new(options.project_id.as_str())
            .map_err(|_| Error::InvalidInput("project ID contains NUL".into()))?;
        let logs = wide(&options.log_dir)?;
        let plugins = wide(&options.runtime_dir)?;
        let _exclusive = SESSION_ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Exclusive)
            .map_err(|_| {
                Error::Unavailable("only one NGX session may own the process runtime".into())
            })?;
        // SAFETY: caller guarantees device lifetime/exclusive NGX ownership;
        // this guard borrows the backend and no handle is retained by Rust.
        let hal = unsafe { device.as_hal::<wgpu::hal::api::Vulkan>() }.ok_or_else(|| {
            Error::Unavailable("DLSS SR adapter requires the Vulkan backend".into())
        })?;
        // SAFETY: borrowed HAL handles only; neither is destroyed or retained.
        let queue_hal = unsafe { queue.as_hal::<wgpu::hal::api::Vulkan>() }
            .ok_or_else(|| Error::InvalidInput("DLSS queue is not Vulkan".into()))?;
        if queue_hal.raw_device().handle() != hal.raw_device().handle() {
            return Err(Error::InvalidInput(
                "DLSS queue and device do not match".into(),
            ));
        }
        drop(queue_hal);
        let instance = hal.shared_instance();
        // SAFETY: the physical device is owned by this live Vulkan instance.
        let properties = unsafe {
            instance
                .raw_instance()
                .get_physical_device_properties(hal.raw_physical_device())
        };
        if properties.vendor_id != 0x10de {
            return Err(Error::Unavailable(
                "DLSS requires a supported NVIDIA GPU".into(),
            ));
        }
        let instance_ext: Vec<_> = instance.extensions().iter().map(|s| s.as_ptr()).collect();
        let device_ext: Vec<_> = hal
            .enabled_device_extensions()
            .iter()
            .map(|s| s.as_ptr())
            .collect();
        let mut raw = std::ptr::null_mut();
        let mut missing = [0 as c_char; 256];
        // SAFETY: all input strings/arrays and output storage live through this
        // synchronous call; signatures/layout are defined in native/ngx.cpp.
        let result = unsafe {
            engine_ngx_open(
                instance.raw_instance().handle().as_raw(),
                hal.raw_physical_device().as_raw(),
                hal.raw_device().handle().as_raw(),
                instance.entry().static_fn().get_instance_proc_addr,
                instance.raw_instance().fp_v1_0().get_device_proc_addr,
                project.as_ptr(),
                logs.as_ptr(),
                plugins.as_ptr(),
                instance_ext.as_ptr(),
                instance_ext.len() as u32,
                device_ext.as_ptr(),
                device_ext.len() as u32,
                &mut raw,
                missing.as_mut_ptr(),
                missing.len() as u32,
            )
        };
        if missing[0] != 0 {
            // SAFETY: bridge always writes a terminated string into this buffer.
            let missing = unsafe { std::ffi::CStr::from_ptr(missing.as_ptr()) }.to_string_lossy();
            return Err(Error::Unavailable(format!(
                "Vulkan device lacks required DLSS extension {missing}; recreate it with this extension"
            )));
        }
        check("initialization/capability", result)?;
        let bridge = RuntimeBridge(
            NonNull::new(raw)
                .ok_or_else(|| Error::Unavailable("NGX returned a null session".into()))?,
        );

        // SAFETY: Vulkan specifies a terminated device-name array.
        let device_name = unsafe { std::ffi::CStr::from_ptr(properties.device_name.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        Ok(Self {
            inner: Rc::new(Runtime {
                bridge,
                device: device.clone(),
                queue: queue.clone(),
                device_name,
                driver_version: properties.driver_version,
                _exclusive,
            }),
        })
    }
    pub fn device(&self) -> &wgpu::Device {
        &self.inner.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.inner.queue
    }
    pub(crate) fn native_handle(&self) -> *mut c_void {
        self.inner.bridge.0.as_ptr()
    }
    pub fn device_name(&self) -> &str {
        &self.inner.device_name
    }
    pub fn driver_version(&self) -> u32 {
        self.inner.driver_version
    }
}
/// A single, thread-confined NGX SR session with owned input/output textures.
/// Every evaluation submits and waits; this conservative synchronization is
/// intentional until the renderer has an asynchronous lifetime integration.
/// Resize/quality changes require replacing the session and resetting history.
pub struct Session {
    bridge: Bridge,
    device: wgpu::Device,
    queue: wgpu::Queue,
    targets: [Target; 4],
    staging: [Target; 4],
    quality: Quality,
    status: Status,
    failure: Option<Error>,
    context: Context,
}
impl Session {
    /// Initialize an SR session after checking platform, GPU, plugin, extensions
    /// and the NGX driver's actual capability response.
    ///
    /// Device/queue identity and resource ownership are enforced internally.
    /// Exposed views refer to staging textures; native pointers never refer to
    /// resources a caller can destroy.
    pub fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        options: SessionOptions,
    ) -> Result<Self> {
        let context = Context::new(device, queue, &options)?;
        Self::with_context(context, options.output_size, options.quality)
    }
    pub fn with_context(context: Context, output_size: [u32; 2], quality: Quality) -> Result<Self> {
        let device = context.device();
        let queue = context.queue();
        if output_size
            .into_iter()
            .any(|x| x == 0 || x > device.limits().max_texture_dimension_2d)
        {
            return Err(Error::InvalidInput("invalid DLSS output dimensions".into()));
        }
        let mut raw = std::ptr::null_mut();
        // SAFETY: shared context is initialized, thread confined and retained by
        // this feature; the returned feature is uniquely owned by Bridge.
        check("create SR parameter map", unsafe {
            engine_ngx_create_sr(context.native_handle(), &mut raw)
        })?;
        let bridge =
            Bridge(NonNull::new(raw).ok_or_else(|| Error::Unavailable("null SR bridge".into()))?);
        let (mut width, mut height, mut major, mut minor) = (0, 0, 0, 0);
        // SAFETY: live session and uniquely borrowed output integers.
        check("optimal settings", unsafe {
            engine_ngx_settings(
                bridge.0.as_ptr(),
                output_size[0],
                output_size[1],
                quality.native(),
                &mut width,
                &mut height,
                &mut major,
                &mut minor,
            )
        })?;
        if width == 0
            || height == 0
            || width > device.limits().max_texture_dimension_2d
            || height > device.limits().max_texture_dimension_2d
        {
            return Err(Error::InvalidInput(
                "NGX returned invalid render dimensions".into(),
            ));
        }
        let render_size = [width, height];
        let status = Status {
            device_name: context.device_name().into(),
            driver_version: context.driver_version(),
            minimum_driver: [major, minor],
            // SAFETY: no pointers or mutable runtime state; returns header constant.
            sdk_api_version: unsafe { engine_ngx_api_version() },
            runtime_version: None,
            render_size,
            output_size,
            evaluated_frames: 0,
        };
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let targets = [
            Target::new(
                device,
                "DLSS scene HDR",
                render_size,
                wgpu::TextureFormat::Rgba16Float,
                vk::Format::R16G16B16A16_SFLOAT,
                false,
            ),
            Target::new(
                device,
                "DLSS clip depth",
                render_size,
                wgpu::TextureFormat::R32Float,
                vk::Format::R32_SFLOAT,
                false,
            ),
            Target::new(
                device,
                "DLSS motion",
                render_size,
                wgpu::TextureFormat::Rg16Float,
                vk::Format::R16G16_SFLOAT,
                false,
            ),
            Target::new(
                device,
                "DLSS output HDR",
                output_size,
                wgpu::TextureFormat::Rgba16Float,
                vk::Format::R16G16B16A16_SFLOAT,
                true,
            ),
        ];
        let staging = [
            Target::new(
                device,
                "DLSS staging color",
                render_size,
                wgpu::TextureFormat::Rgba16Float,
                vk::Format::R16G16B16A16_SFLOAT,
                false,
            ),
            Target::new(
                device,
                "DLSS staging depth",
                render_size,
                wgpu::TextureFormat::R32Float,
                vk::Format::R32_SFLOAT,
                false,
            ),
            Target::new(
                device,
                "DLSS staging motion",
                render_size,
                wgpu::TextureFormat::Rg16Float,
                vk::Format::R16G16_SFLOAT,
                false,
            ),
            Target::new(
                device,
                "DLSS staging output",
                output_size,
                wgpu::TextureFormat::Rgba16Float,
                vk::Format::R16G16B16A16_SFLOAT,
                true,
            ),
        ];
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(Error::Gpu(format!("DLSS resource creation: {error}")));
        }
        Ok(Self {
            bridge,
            device: device.clone(),
            queue: queue.clone(),
            targets,
            staging,
            quality,
            status,
            failure: None,
            context,
        })
    }
    pub fn context(&self) -> &Context {
        &self.context
    }
    pub fn status(&self) -> &Status {
        &self.status
    }
    /// Color HDR, linear clip depth in [0,1], and current-to-previous pixel motion.
    /// Submit all producers to the session's queue before calling evaluate.
    pub fn inputs(&self) -> [&wgpu::TextureView; 3] {
        [
            &self.staging[0].view,
            &self.staging[1].view,
            &self.staging[2].view,
        ]
    }
    pub fn output(&self) -> &wgpu::TextureView {
        &self.staging[3].view
    }
    /// Readback/resolve source. Destroying it causes later evaluation to fail;
    /// native resources remain private and cannot be freed through this handle.
    pub fn output_texture(&self) -> &wgpu::Texture {
        &self.staging[3].texture
    }
    pub fn evaluate(&mut self, frame: Frame) -> Result<()> {
        frame.validate()?;
        if let Some(error) = &self.failure {
            return Err(error.clone());
        }
        for target in &self.staging {
            // SAFETY: only testing liveness. No raw handle escapes the guard;
            // concurrent destruction cannot affect the private native targets.
            if unsafe { target.texture.as_hal::<wgpu::hal::api::Vulkan>() }.is_none() {
                return Err(Error::InvalidInput(
                    "DLSS staging texture was destroyed".into(),
                ));
            }
        }
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let result = self.evaluate_inner(frame);
        let validation = pollster::block_on(scope.pop());
        let result = match validation {
            Some(error) => Err(Error::Gpu(format!("DLSS GPU validation: {error}"))),
            None => result,
        };
        if let Err(error) = &result {
            self.failure = Some(error.clone());
        }
        if result.is_ok() {
            self.status.evaluated_frames += 1;
        }
        result
    }
    fn evaluate_inner(&mut self, frame: Frame) -> Result<()> {
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("NGX SR evaluation"),
            });
        for (source, destination) in self.staging[..3].iter().zip(&self.targets[..3]) {
            encoder.copy_texture_to_texture(
                source.texture.as_image_copy(),
                destination.texture.as_image_copy(),
                destination.texture.size(),
            );
        }
        // Make initialization visible to wgpu's tracker before native writes;
        // otherwise a subsequent read could lazily zero the native result.
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("initialize DLSS destination"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.targets[3].view,
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
        encoder.transition_resources(
            std::iter::empty(),
            self.targets
                .iter()
                .enumerate()
                .map(|(i, t)| wgpu::TextureTransition {
                    texture: &t.texture,
                    selector: None,
                    state: if i == 3 {
                        wgpu::TextureUses::STORAGE_READ_WRITE
                    } else {
                        wgpu::TextureUses::RESOURCE
                    },
                }),
        );
        let transitions = encoder.finish();
        // wgpu 30 forbids mixing its commands and raw commands in one encoder.
        // Submit the tracked transitions immediately before the native buffer.
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("NGX native commands"),
            });
        let mut images = Vec::with_capacity(4);
        for target in &self.targets {
            // SAFETY: private owned textures on our live device. Extract handles
            // sequentially: wgpu's snatch lock cannot be recursively acquired.
            // The session retains all resource owners through GPU completion.
            let image = {
                let hal = unsafe { target.texture.as_hal::<wgpu::hal::api::Vulkan>() }
                    .ok_or_else(|| Error::Gpu("DLSS texture was destroyed".into()))?;
                unsafe { hal.raw_handle().as_raw() }
            };
            // SAFETY: same ownership and lifetime as the image above.
            let view = {
                let hal = unsafe { target.view.as_hal::<wgpu::hal::api::Vulkan>() }
                    .ok_or_else(|| Error::Gpu("DLSS view unavailable".into()))?;
                unsafe { hal.raw_handle().as_raw() }
            };
            images.push(Image {
                image,
                view,
                width: target.texture.width(),
                height: target.texture.height(),
                format: target.format.as_raw() as u32,
            });
        }
        let native_frame = NativeFrame {
            jitter_x: frame.jitter[0],
            jitter_y: frame.jitter[1],
            motion_x: frame.motion_scale[0],
            motion_y: frame.motion_scale[1],
            delta_ms: frame.delta_ms,
            reset: u32::from(frame.reset || self.status.evaluated_frames == 0),
        };
        // SAFETY: no render pass is open; native code records into this command
        // buffer only, never ends it, and preserves resource states as required
        // by NGX. Owned textures stay alive through submission and completion.
        let result = unsafe {
            encoder.as_hal_mut::<wgpu::hal::api::Vulkan, _, _>(|hal| {
                let hal =
                    hal.ok_or_else(|| Error::Gpu("Vulkan command encoder unavailable".into()))?;
                check(
                    "evaluation",
                    engine_ngx_evaluate(
                        self.bridge.0.as_ptr(),
                        hal.raw_handle().as_raw(),
                        self.quality.native(),
                        images.as_ptr(),
                        &native_frame,
                    ),
                )
            })
        };

        if let Err(error) = result {
            self.failure = Some(error.clone());
            return Err(error);
        }
        let native = encoder.finish();
        let mut resolve = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("DLSS staging resolve"),
            });
        resolve.copy_texture_to_texture(
            self.targets[3].texture.as_image_copy(),
            self.staging[3].texture.as_image_copy(),
            self.targets[3].texture.size(),
        );
        let index = self.queue.submit([transitions, native, resolve.finish()]);
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: None,
            })
            .map_err(|e| Error::Gpu(format!("DLSS GPU completion: {e}")))?;
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        // NGX feature objects cannot be released while GPU work uses them.
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
    }
}
fn check(operation: &'static str, code: u32) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Error::Native { operation, code })
    }
}
fn validate_options(options: &SessionOptions, maximum: u32) -> Result<()> {
    if options.project_id.trim().is_empty()
        || options
            .output_size
            .into_iter()
            .any(|x| x == 0 || x > maximum)
    {
        return Err(Error::InvalidInput(
            "DLSS needs a project ID and valid nonzero output dimensions".into(),
        ));
    }
    for path in [&options.runtime_dir, &options.log_dir] {
        if !path.is_absolute() || !path.is_dir() {
            return Err(Error::InvalidInput(format!(
                "DLSS directory must be absolute and exist: {}",
                path.display()
            )));
        }
    }
    let name = if cfg!(windows) {
        "nvngx_dlss.dll"
    } else {
        "libnvidia-ngx-dlss.so.310.9.16"
    };
    if !options.runtime_dir.join(name).is_file() {
        return Err(Error::Unavailable(format!(
            "official DLSS runtime missing: {}",
            options.runtime_dir.join(name).display()
        )));
    }
    use std::io::Read;
    let mut magic = [0; 4];
    std::fs::File::open(options.runtime_dir.join(name))
        .and_then(|mut file| file.read_exact(&mut magic))
        .map_err(|e| Error::InvalidInput(format!("cannot read DLSS runtime header: {e}")))?;
    if (cfg!(windows) && magic[..2] != *b"MZ") || (cfg!(not(windows)) && magic != *b"\x7fELF") {
        return Err(Error::InvalidInput(
            "DLSS runtime is not a native library for this platform".into(),
        ));
    }
    Ok(())
}
fn wide(path: &std::path::Path) -> Result<Vec<Wide>> {
    #[cfg(windows)]
    let mut value: Vec<Wide> = {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str().encode_wide().collect()
    };
    #[cfg(not(windows))]
    let mut value: Vec<Wide> = path
        .to_str()
        .ok_or_else(|| Error::InvalidInput("DLSS path is not valid UTF-8".into()))?
        .chars()
        .map(|c| c as u32)
        .collect();
    if value.contains(&0) {
        return Err(Error::InvalidInput("DLSS path contains NUL".into()));
    }
    value.push(0);
    Ok(value)
}

/// Create a Vulkan device with the SR extensions requested by the installed NGX
/// runtime. Unsupported adapters and unknown extension requirements are errors;
/// the caller can create its ordinary wgpu device and retain normal rendering.
pub fn create_device(
    adapter: &wgpu::Adapter,
    desc: &wgpu::DeviceDescriptor<'_>,
    options: &SessionOptions,
) -> Result<(wgpu::Device, wgpu::Queue)> {
    create_device_for_features(adapter, desc, options, &[Feature::SuperResolution])
}
/// Query the union of device extensions for the requested native features. This
/// prepares a device; it does not claim any feature was initialized or evaluated.
pub fn create_device_for_features(
    adapter: &wgpu::Adapter,
    desc: &wgpu::DeviceDescriptor<'_>,
    options: &SessionOptions,
    features: &[Feature],
) -> Result<(wgpu::Device, wgpu::Queue)> {
    validate_options(options, adapter.limits().max_texture_dimension_2d)?;
    if adapter.get_info().vendor != 0x10de {
        return Err(Error::Unavailable(
            "DLSS requires an NVIDIA Vulkan adapter".into(),
        ));
    }
    if !adapter.features().contains(desc.required_features)
        || !desc.required_limits.check_limits(&adapter.limits())
    {
        return Err(Error::InvalidInput(
            "requested device features or limits exceed adapter capabilities".into(),
        ));
    }
    let project = CString::new(options.project_id.as_str())
        .map_err(|_| Error::InvalidInput("project ID contains NUL".into()))?;
    let logs = wide(&options.log_dir)?;
    let plugins = wide(&options.runtime_dir)?;
    let _exclusive = SESSION_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map(|_| Exclusive)
        .map_err(|_| Error::Unavailable("cannot query NGX while a session is active".into()))?;
    // SAFETY: guard borrowed only; no raw adapter/instance is destroyed.
    let hal = unsafe { adapter.as_hal::<wgpu::hal::api::Vulkan>() }
        .ok_or_else(|| Error::Unavailable("DLSS requires Vulkan".into()))?;
    let instance = hal.shared_instance();
    let mut required = Vec::new();
    for feature in features {
        let feature_id = match feature {
            Feature::SuperResolution => 1,
            Feature::FrameGeneration => 11,
            Feature::RayReconstruction => 13,
            Feature::NeuralRendering => {
                return Err(Error::Unavailable("no public neural-rendering ABI".into()));
            }
        };
        let mut names = [[0 as c_char; 256]; 16];
        let mut count = 0;
        // SAFETY: valid borrowed Vulkan handles and output storage, with exact C ABI.
        check("device extension query", unsafe {
            engine_ngx_device_extensions(
                instance.raw_instance().handle().as_raw(),
                hal.raw_physical_device().as_raw(),
                feature_id,
                project.as_ptr(),
                logs.as_ptr(),
                plugins.as_ptr(),
                names.as_mut_ptr().cast(),
                16,
                &mut count,
            )
        })?;
        for name in &names[..count as usize] {
            // SAFETY: bridge bounds the count and terminates every entry.
            required.push(unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }.to_owned());
        }
    }
    // SAFETY: physical device is borrowed from this instance.
    let supported = unsafe {
        instance
            .raw_instance()
            .enumerate_device_extension_properties(hal.raw_physical_device())
    }
    .map_err(|e| Error::Gpu(e.to_string()))?;
    let mut extensions = Vec::new();
    for name in &required {
        let name = name.as_c_str();
        if !supported.iter().any(|p| {
            // SAFETY: Vulkan extension properties contain terminated names.
            unsafe { std::ffi::CStr::from_ptr(p.extension_name.as_ptr()) == name }
        }) {
            return Err(Error::Unavailable(format!(
                "GPU lacks required DLSS extension {}",
                name.to_string_lossy()
            )));
        }
        // wgpu-hal requires static extension names. Keep a bounded verified SDK
        // allowlist rather than leaking runtime strings into process memory.
        let extension = match name.to_bytes() {
            b"VK_NVX_binary_import" => c"VK_NVX_binary_import",
            b"VK_NVX_image_view_handle" => c"VK_NVX_image_view_handle",
            b"VK_KHR_push_descriptor" => c"VK_KHR_push_descriptor",
            b"VK_KHR_buffer_device_address" => c"VK_KHR_buffer_device_address",
            _ => {
                return Err(Error::Unavailable(format!(
                    "unreviewed DLSS extension requirement {}",
                    name.to_string_lossy()
                )));
            }
        };
        extensions.push(extension);
    }
    if extensions.contains(&c"VK_KHR_buffer_device_address") {
        let mut address = vk::PhysicalDeviceBufferDeviceAddressFeatures::default();
        let mut properties = vk::PhysicalDeviceFeatures2::default().push_next(&mut address);
        // SAFETY: supported Vulkan physical device; this only queries features.
        unsafe {
            instance
                .raw_instance()
                .get_physical_device_features2(hal.raw_physical_device(), &mut properties);
        }
        if address.buffer_device_address == 0 {
            return Err(Error::Unavailable(
                "DLSS requires Vulkan buffer device addresses".into(),
            ));
        }
    }
    // SAFETY: all additional extensions were queried from NGX and confirmed
    // supported; existing features/extensions are untouched. The returned HAL
    // device is imported into the very adapter that created it.
    unsafe {
        let hal_ref = &*hal;
        let raw = hal
            .open_with_callback(
                desc.required_features,
                &desc.required_limits,
                &desc.memory_hints,
                Some(Box::new(move |args| {
                    for extension in extensions {
                        if !args.extensions.contains(&extension) {
                            args.extensions.push(extension);
                        }
                    }
                    *args.device_features =
                        hal_ref.physical_device_features(args.extensions, desc.required_features);
                })),
            )
            .map_err(|e| Error::Gpu(format!("DLSS device creation: {e}")))?;
        adapter
            .create_device_from_hal::<wgpu::hal::api::Vulkan>(raw, desc)
            .map_err(|e| Error::Gpu(format!("DLSS device import: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_dimensions_missing_and_invalid_runtime_before_loading() {
        let folder =
            std::env::temp_dir().join(format!("engine-dlss-options-{}", std::process::id()));
        std::fs::create_dir_all(&folder).expect("temporary directory");
        let mut options = SessionOptions {
            project_id: "test".into(),
            runtime_dir: folder.clone(),
            log_dir: folder.clone(),
            output_size: [0, 100],
            quality: Quality::Quality,
        };
        assert!(matches!(
            validate_options(&options, 4096),
            Err(Error::InvalidInput(_))
        ));
        options.output_size = [512, 512];
        assert!(matches!(
            validate_options(&options, 4096),
            Err(Error::Unavailable(_))
        ));
        let name = if cfg!(windows) {
            "nvngx_dlss.dll"
        } else {
            "libnvidia-ngx-dlss.so.310.9.16"
        };
        let library = folder.join(name);
        std::fs::write(&library, b"not a native library").expect("test library");
        assert!(matches!(
            validate_options(&options, 4096),
            Err(Error::InvalidInput(_))
        ));
        std::fs::remove_file(library).expect("remove test library");
        std::fs::remove_dir(folder).expect("remove temporary directory");
    }
}
