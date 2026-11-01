//! Per-view frame textures with documented formats.

/// HDR scene colour: linear RGB (values above 1 are kept), alpha coverage.
pub const HDR_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Scene depth: WebGPU depth 0 (near) .. 1 (far), not reversed.
pub const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// World-space unit normals in xyz, coverage in w (0 where nothing drew).
pub const NORMALS_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Current-to-previous motion in render pixels, x right / y down.
pub const MOTION_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rg16Float;

/// Format of the four screen-lighting inputs ([`ScreenLighting`]).
pub const SCREEN_LIGHTING_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Values the screen-lighting inputs hold before any feature writes them
/// (and the 1x1 placeholders bound when no feature needs them): shadow
/// `(1, 0, 0, 0)`, ambient occlusion `(1, 0, 0, 0)`, indirect `(0, 0, 0, 0)`,
/// reflection `(0, 0, 0, -1)`. With these the lit result is unchanged.
pub const SCREEN_LIGHTING_DEFAULTS: [[f32; 4]; 4] = [
    [1.0, 0.0, 0.0, 0.0],
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0, 0.0],
    [0.0, 0.0, 0.0, -1.0],
];

/// Screen-space lighting inputs of one view (render size,
/// [`SCREEN_LIGHTING_FORMAT`]): written by Prepass features (storage writes
/// or copies) and read by `engine::lighting` in the Opaque and Transparent
/// stages through `engine::view` bindings 7..=10. Allocated when a running
/// feature declares [`crate::FeatureNeeds::screen_lighting`]; cleared to
/// [`SCREEN_LIGHTING_DEFAULTS`] every frame.
///
/// | texture | binding | contents |
/// | --- | --- | --- |
/// | `shadow` | 7 | x: visibility of the first directional light; y: 1 when x replaces its shadow-map visibility, 0 when it multiplies it |
/// | `ambient_occlusion` | 8 | x: multiplier of the diffuse ambient / indirect term |
/// | `indirect` | 9 | rgb: incident diffuse radiance (irradiance / pi) replacing the constant ambient where a >= 0.5 |
/// | `reflection` | 10 | rgb: specular radiance along the mirror direction where a >= 0; a: the roughness cutoff (surfaces rougher than it ignore the texel) |
#[derive(Clone, Debug)]
pub struct ScreenLighting {
    pub shadow: Target,
    pub ambient_occlusion: Target,
    pub indirect: Target,
    pub reflection: Target,
}

impl ScreenLighting {
    /// The four targets in binding order (7..=10).
    pub fn targets(&self) -> [&Target; 4] {
        [
            &self.shadow,
            &self.ambient_occlusion,
            &self.indirect,
            &self.reflection,
        ]
    }
}

/// Format of the three surface-material targets ([`SurfaceMaterial`]).
pub const SURFACE_MATERIAL_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// Shaded surface description of the built-in lit materials, one texel per
/// render pixel, written after the Opaque stage by `engine.surface_material`
/// (depth-tested against the final scene depth, so only the visible surface
/// of each pixel is described). Custom-shader materials and unlit materials
/// write nothing: their texels keep coverage 0 and are non-reflective.
/// Allocated while `engine.ssr` runs in the view and freed when it stops;
/// cleared to zero every frame.
///
/// | texture | contents |
/// | --- | --- |
/// | `normal_roughness` | xyz: world-space unit normal after normal mapping, facing the viewer; w: perceptual roughness 0..1 |
/// | `f0_coverage` | rgb: specular F0 (`mix(0.04, albedo, metallic)`); a: 1 where a lit surface drew, else 0 |
/// | `fallback_specular` | rgb: the specular term the opaque pass already added (probe / environment specular; zero without native indirect lighting) |
#[derive(Clone, Debug)]
pub struct SurfaceMaterial {
    pub normal_roughness: Target,
    pub f0_coverage: Target,
    pub fallback_specular: Target,
}

impl SurfaceMaterial {
    /// The three targets in binding order.
    pub fn targets(&self) -> [&Target; 3] {
        [
            &self.normal_roughness,
            &self.f0_coverage,
            &self.fallback_specular,
        ]
    }
}

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
/// | `screen_lighting` | `Rgba16Float` x 4 | attachment, sampled, storage, copy | when a running feature needs screen lighting ([`ScreenLighting`]) |
/// | `surface_material` | `Rgba16Float` x 3 | attachment, sampled, copy src | after the OpaqueEffects stage while `engine.ssr` runs ([`SurfaceMaterial`]) |
pub struct FrameTargets {
    pub render_size: [u32; 2],
    pub output_size: [u32; 2],
    pub hdr: Target,
    /// Allocated only when an opaque effect needs a distinct full-image destination.
    pub(crate) scene_scratch: Option<Target>,
    pub depth: Target,
    /// Isolated certified current-frame occluder depth for optional Hi-Z.
    pub(crate) occluder_depth: Option<Target>,
    pub normals: Option<Target>,
    pub motion: Option<Target>,
    /// Screen-lighting inputs, when a running feature needs them.
    pub screen_lighting: Option<ScreenLighting>,
    /// Shaded surface description for screen-space effects, when a running
    /// feature needs it.
    pub surface_material: Option<SurfaceMaterial>,
    /// Depth of the motion prepass (kept apart from scene depth).
    pub(crate) motion_depth: Option<Target>,
    pub(crate) upscaled: Option<Target>,
    pub(crate) post: [Option<Target>; 2],
}

impl FrameTargets {
    pub(crate) fn scene_color(&self, index: usize) -> &Target {
        if index == 0 {
            &self.hdr
        } else {
            self.scene_scratch.as_ref().expect("scene source exists")
        }
    }
    pub(crate) fn new(device: &wgpu::Device, render_size: [u32; 2], output_size: [u32; 2]) -> Self {
        Self {
            render_size,
            output_size,
            hdr: Target::new(
                device,
                "engine HDR scene colour",
                render_size,
                HDR_FORMAT,
                COLOR_USAGE,
            ),
            scene_scratch: None,
            depth: Target::new(
                device,
                "engine scene depth",
                render_size,
                DEPTH_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
            ),
            occluder_depth: None,
            normals: None,
            motion: None,
            screen_lighting: None,
            surface_material: None,
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

    pub(crate) fn ensure_occluder_depth(&mut self, device: &wgpu::Device) -> &Target {
        self.occluder_depth.get_or_insert_with(|| {
            Target::new(
                device,
                "engine current-frame occluder depth",
                self.render_size,
                DEPTH_FORMAT,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            )
        })
    }

    pub(crate) fn ensure_screen_lighting(&mut self, device: &wgpu::Device) {
        let size = self.render_size;
        if self.screen_lighting.is_none() {
            let target =
                |label| Target::new(device, label, size, SCREEN_LIGHTING_FORMAT, COLOR_USAGE);
            self.screen_lighting = Some(ScreenLighting {
                shadow: target("engine screen shadow"),
                ambient_occlusion: target("engine screen ambient occlusion"),
                indirect: target("engine screen indirect"),
                reflection: target("engine screen reflection"),
            });
        }
    }

    pub(crate) fn ensure_surface_material(&mut self, device: &wgpu::Device) {
        let size = self.render_size;
        if self.surface_material.is_none() {
            let target = |label| {
                Target::new(
                    device,
                    label,
                    size,
                    SURFACE_MATERIAL_FORMAT,
                    wgpu::TextureUsages::RENDER_ATTACHMENT
                        | wgpu::TextureUsages::TEXTURE_BINDING
                        | wgpu::TextureUsages::COPY_SRC,
                )
            };
            self.surface_material = Some(SurfaceMaterial {
                normal_roughness: target("engine surface normal roughness"),
                f0_coverage: target("engine surface f0 coverage"),
                fallback_specular: target("engine surface fallback specular"),
            });
        }
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

    pub(crate) fn ensure_scene_scratch(&mut self, device: &wgpu::Device) -> &Target {
        let size = self.render_size;
        self.scene_scratch.get_or_insert_with(|| {
            Target::new(
                device,
                "engine opaque effects HDR",
                size,
                HDR_FORMAT,
                COLOR_USAGE,
            )
        })
    }

    pub(crate) fn ensure_post(&mut self, device: &wgpu::Device, index: usize) -> &Target {
        let size = self.output_size;
        self.post[index].get_or_insert_with(|| {
            Target::new(
                device,
                "engine HDR post-process",
                size,
                HDR_FORMAT,
                COLOR_USAGE,
            )
        })
    }
}

/// Display-colour ping-pong targets; separate from scene HDR and available to
/// overlay-only views. Format/size exactly match the caller's output.
pub(crate) struct CompositeTargets {
    pub size: [u32; 2],
    pub format: wgpu::TextureFormat,
    pub colors: [Target; 2],
}
impl CompositeTargets {
    pub fn new(device: &wgpu::Device, size: [u32; 2], format: wgpu::TextureFormat) -> Self {
        Self {
            size,
            format,
            colors: [0, 1].map(|i| {
                Target::new(
                    device,
                    &format!("engine final composite {i}"),
                    size,
                    format,
                    wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
                )
            }),
        }
    }
}
