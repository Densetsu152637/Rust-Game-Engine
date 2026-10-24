//! Per-view frame textures with documented formats.

/// HDR scene colour: linear RGB (values above 1 are kept), alpha coverage.
pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Scene depth: WebGPU depth 0 (near) .. 1 (far), not reversed.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// World-space unit normals in xyz, coverage in w (0 where nothing drew).
pub const NORMALS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Current-to-previous motion in render pixels, x right / y down.
pub const MOTION_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;

/// A texture and its default view.
#[derive(Clone, Debug)]
pub struct Target {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
}

impl Target {
    pub(crate) fn new(
        device: &wgpu::Device,
        label: &str,
        size: [u32; 2],
        format: wgpu::TextureFormat,
        usage: wgpu::TextureUsages,
    ) -> Self {
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
        Self { texture, view }
    }

    pub fn size(&self) -> [u32; 2] {
        [self.texture.width(), self.texture.height()]
    }
}

const COLOR_USAGE: wgpu::TextureUsages = wgpu::TextureUsages::RENDER_ATTACHMENT
    .union(wgpu::TextureUsages::TEXTURE_BINDING)
    .union(wgpu::TextureUsages::COPY_SRC)
    .union(wgpu::TextureUsages::COPY_DST)
    .union(wgpu::TextureUsages::STORAGE_BINDING);

/// The textures of one view. Render-size: `hdr`, `depth`, `normals`,
/// `motion`. Output-size: the two post-processing targets (and `upscaled`
/// when render and output sizes differ).
///
/// | texture | format | usage | available |
/// | --- | --- | --- | --- |
/// | `hdr` | `Rgba16Float` | attachment, sampled, storage, copy | always |
/// | `depth` | `Depth32Float` | attachment, sampled, copy src | always |
/// | `normals` | `Rgba16Float` | attachment, sampled, copy src | after the Prepass stage when a running feature needs normals |
/// | `motion` | `Rg16Float` | attachment, sampled, copy src | after the Prepass stage when a feature or the host needs motion |
pub struct FrameTargets {
    pub render_size: [u32; 2],
    pub output_size: [u32; 2],
    pub hdr: Target,
    pub depth: Target,
    pub normals: Option<Target>,
    pub motion: Option<Target>,
    /// Depth of the motion prepass (kept apart from scene depth).
    pub(crate) motion_depth: Option<Target>,
    pub(crate) upscaled: Option<Target>,
    pub(crate) post: [Option<Target>; 2],
}

impl FrameTargets {
    pub(crate) fn new(device: &wgpu::Device, render_size: [u32; 2], output_size: [u32; 2]) -> Self {
        Self {
            render_size,
            output_size,
            hdr: Target::new(device, "engine HDR scene colour", render_size, HDR_FORMAT, COLOR_USAGE),
            depth: Target::new(
                device,
                "engine scene depth",
                render_size,
                DEPTH_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
            ),
            normals: None,
            motion: None,
            motion_depth: None,
            upscaled: None,
            post: [None, None],
        }
    }

    pub(crate) fn ensure_normals(&mut self, device: &wgpu::Device) -> &Target {
        let size = self.render_size;
        self.normals.get_or_insert_with(|| {
            Target::new(
                device,
                "engine normals",
                size,
                NORMALS_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
            )
        })
    }

    pub(crate) fn ensure_motion(&mut self, device: &wgpu::Device) {
        let size = self.render_size;
        if self.motion.is_none() {
            self.motion = Some(Target::new(
                device,
                "engine motion vectors",
                size,
                MOTION_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
            ));
            self.motion_depth = Some(Target::new(
                device,
                "engine motion depth",
                size,
                DEPTH_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT,
            ));
        }
    }

    pub(crate) fn ensure_upscaled(&mut self, device: &wgpu::Device) -> &Target {
        let size = self.output_size;
        self.upscaled.get_or_insert_with(|| {
            Target::new(device, "engine upscaled HDR", size, HDR_FORMAT, COLOR_USAGE)
        })
    }

    pub(crate) fn ensure_post(&mut self, device: &wgpu::Device, index: usize) -> &Target {
        let size = self.output_size;
        self.post[index].get_or_insert_with(|| {
            Target::new(device, "engine HDR post-process", size, HDR_FORMAT, COLOR_USAGE)
        })
    }
}
