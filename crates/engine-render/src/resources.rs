//! Resource descriptions and typed handles.
//!
//! Handles are plain copyable ids minted by [`crate::RenderDevice`]; ids are
//! never reused, so a stale handle can always be detected. Backends receive the
//! typed handle and key their own storage by [`ShaderHandle::id`] and friends.

use crate::error::{RenderError, Result};
use std::fmt;

/// Kinds of device-owned resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResourceKind {
    Shader,
    Mesh,
    Texture,
    /// Offscreen render target created through the device.
    Target,
}

impl fmt::Display for ResourceKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Shader => "shader",
            Self::Mesh => "mesh",
            Self::Texture => "texture",
            Self::Target => "target",
        })
    }
}

macro_rules! typed_handle {
    ($($(#[$doc:meta])* $name:ident => $kind:ident),* $(,)?) => {$(
        $(#[$doc])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(u64);
        impl $name {
            /// Wrap a raw id. Only [`crate::RenderDevice`] mints live handles;
            /// this exists for backend unit tests and diagnostics.
            pub const fn from_raw(id: u64) -> Self {
                Self(id)
            }
            pub const fn id(self) -> u64 {
                self.0
            }
        }
        impl From<$name> for ResourceId {
            fn from(handle: $name) -> Self {
                ResourceId::$kind(handle)
            }
        }
    )*};
}

typed_handle! {
    /// Handle to a shader created with [`crate::RenderDevice::create_shader`].
    ShaderHandle => Shader,
    /// Handle to a mesh created with [`crate::RenderDevice::create_mesh`].
    MeshHandle => Mesh,
    /// Handle to a sampled texture created with [`crate::RenderDevice::create_texture`].
    TextureHandle => Texture,
    /// Handle to an offscreen target created with [`crate::RenderDevice::create_target`].
    TargetHandle => Target,
}

/// Kind-tagged handle used where any resource may appear (destruction, errors).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResourceId {
    Shader(ShaderHandle),
    Mesh(MeshHandle),
    Texture(TextureHandle),
    Target(TargetHandle),
}

impl ResourceId {
    pub fn kind(self) -> ResourceKind {
        match self {
            Self::Shader(_) => ResourceKind::Shader,
            Self::Mesh(_) => ResourceKind::Mesh,
            Self::Texture(_) => ResourceKind::Texture,
            Self::Target(_) => ResourceKind::Target,
        }
    }
    pub fn raw(self) -> u64 {
        match self {
            Self::Shader(h) => h.id(),
            Self::Mesh(h) => h.id(),
            Self::Texture(h) => h.id(),
            Self::Target(h) => h.id(),
        }
    }
}

impl fmt::Display for ResourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.kind(), self.raw())
    }
}

// ---------------------------------------------------------------- vertices

/// Shader-facing vertex attribute formats. Offsets must be 4-byte aligned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VertexFormat {
    Float32,
    Float32x2,
    Float32x3,
    Float32x4,
    /// Four bytes read as normalized floats, for packed colors.
    Unorm8x4,
}

impl VertexFormat {
    pub const fn byte_size(self) -> u32 {
        match self {
            Self::Float32 | Self::Unorm8x4 => 4,
            Self::Float32x2 => 8,
            Self::Float32x3 => 12,
            Self::Float32x4 => 16,
        }
    }
    /// Number of scalar components the shader sees.
    pub const fn components(self) -> u32 {
        match self {
            Self::Float32 => 1,
            Self::Float32x2 => 2,
            Self::Float32x3 => 3,
            Self::Float32x4 | Self::Unorm8x4 => 4,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VertexAttribute {
    pub location: u32,
    pub format: VertexFormat,
    pub offset: u32,
}

/// Explicit vertex input metadata for one interleaved vertex buffer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct VertexLayout {
    pub stride: u32,
    pub attributes: Vec<VertexAttribute>,
}

impl VertexLayout {
    pub fn new(stride: u32, attributes: impl IntoIterator<Item = VertexAttribute>) -> Self {
        Self {
            stride,
            attributes: attributes.into_iter().collect(),
        }
    }

    /// Check the stride, attribute bounds, alignment and location uniqueness.
    pub fn validate(&self) -> Result<()> {
        let invalid = |message: String| Err(RenderError::InvalidResource(message));
        if self.stride == 0 || !self.stride.is_multiple_of(4) {
            return invalid(format!(
                "vertex stride {} must be a nonzero multiple of 4",
                self.stride
            ));
        }
        if self.attributes.is_empty() {
            return invalid("vertex layout has no attributes".into());
        }
        for (index, attribute) in self.attributes.iter().enumerate() {
            let size = attribute.format.byte_size();
            if !attribute.offset.is_multiple_of(4) {
                return invalid(format!(
                    "attribute at location {} has unaligned offset {}",
                    attribute.location, attribute.offset
                ));
            }
            if attribute.offset > self.stride || size > self.stride - attribute.offset {
                return invalid(format!(
                    "attribute at location {} ({} bytes at offset {}) exceeds stride {}",
                    attribute.location, size, attribute.offset, self.stride
                ));
            }
            if self.attributes[..index]
                .iter()
                .any(|other| other.location == attribute.location)
            {
                return invalid(format!("duplicate vertex location {}", attribute.location));
            }
        }
        Ok(())
    }

    pub fn attribute(&self, location: u32) -> Option<&VertexAttribute> {
        self.attributes.iter().find(|a| a.location == location)
    }
}

// ----------------------------------------------------------------- shaders

/// Source language of a shader. WGSL is the engine's authoring language; each
/// backend consumes it (wgpu takes WGSL directly).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ShaderLanguage {
    Wgsl,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderSource {
    pub language: ShaderLanguage,
    pub code: String,
    pub vertex_entry: String,
    pub fragment_entry: String,
}

impl ShaderSource {
    pub const DEFAULT_VERTEX_ENTRY: &'static str = "vs_main";
    pub const DEFAULT_FRAGMENT_ENTRY: &'static str = "fs_main";

    /// WGSL with the conventional `vs_main` / `fs_main` entry points.
    pub fn wgsl(code: impl Into<String>) -> Self {
        Self {
            language: ShaderLanguage::Wgsl,
            code: code.into(),
            vertex_entry: Self::DEFAULT_VERTEX_ENTRY.into(),
            fragment_entry: Self::DEFAULT_FRAGMENT_ENTRY.into(),
        }
    }

    pub fn with_entry_points(
        mut self,
        vertex: impl Into<String>,
        fragment: impl Into<String>,
    ) -> Self {
        self.vertex_entry = vertex.into();
        self.fragment_entry = fragment.into();
        self
    }
}

/// A named vertex+fragment shader program.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShaderResource {
    pub name: String,
    pub source: ShaderSource,
}

impl ShaderResource {
    pub fn new(name: impl Into<String>, source: ShaderSource) -> Self {
        Self {
            name: name.into(),
            source,
        }
    }
}

// ------------------------------------------------------------------ meshes

/// CPU mesh data: interleaved vertex bytes described by a [`VertexLayout`], and
/// an optional `u32` index list. The primitive is always a triangle list.
#[derive(Clone, Debug, PartialEq)]
pub struct MeshResource {
    pub vertices: Vec<u8>,
    pub layout: VertexLayout,
    pub vertex_count: u32,
    pub indices: Vec<u32>,
}

impl MeshResource {
    /// Build a mesh, deriving `vertex_count` from the byte length and stride.
    pub fn new(vertices: Vec<u8>, layout: VertexLayout, indices: Vec<u32>) -> Result<Self> {
        layout.validate()?;
        let stride = layout.stride as usize;
        if !vertices.len().is_multiple_of(stride) {
            return Err(RenderError::InvalidResource(format!(
                "vertex data length {} is not a multiple of stride {stride}",
                vertices.len()
            )));
        }
        let vertex_count = u32::try_from(vertices.len() / stride)
            .map_err(|_| RenderError::InvalidResource("vertex count overflows u32".into()))?;
        let mesh = Self {
            vertices,
            layout,
            vertex_count,
            indices,
        };
        mesh.validate()?;
        Ok(mesh)
    }

    /// Number of elements a draw emits: indices when present, else vertices.
    pub fn draw_count(&self) -> u32 {
        if self.indices.is_empty() {
            self.vertex_count
        } else {
            self.indices.len() as u32
        }
    }

    pub fn is_indexed(&self) -> bool {
        !self.indices.is_empty()
    }

    pub fn validate(&self) -> Result<()> {
        let invalid = |message: String| Err(RenderError::InvalidResource(message));
        self.layout.validate()?;
        let expected = (self.vertex_count as usize).checked_mul(self.layout.stride as usize);
        if expected != Some(self.vertices.len()) {
            return invalid(format!(
                "vertex data length {} does not match {} vertices of stride {}",
                self.vertices.len(),
                self.vertex_count,
                self.layout.stride
            ));
        }
        if self.vertex_count == 0 {
            return invalid("mesh has no vertices".into());
        }
        if self.indices.len() > u32::MAX as usize {
            return invalid("index count overflows u32".into());
        }
        if let Some(bad) = self.indices.iter().find(|i| **i >= self.vertex_count) {
            return invalid(format!(
                "index {bad} exceeds vertex count {}",
                self.vertex_count
            ));
        }
        if !self.draw_count().is_multiple_of(3) {
            return invalid("mesh must contain complete triangles".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- textures

/// CPU RGBA8 image, row-major, top row first. Texels are sRGB-encoded: sampling
/// returns linear values and offscreen targets are sRGB, so a texel written
/// through an unmodified sample round-trips byte-exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texture2D {
    pub width: u32,
    pub height: u32,
    pub rgba8: Vec<u8>,
}

impl Texture2D {
    pub fn new(width: u32, height: u32, rgba8: Vec<u8>) -> Result<Self> {
        let texture = Self {
            width,
            height,
            rgba8,
        };
        texture.validate()?;
        Ok(texture)
    }

    /// A single opaque white texel, useful as a default binding.
    pub fn white() -> Self {
        Self {
            width: 1,
            height: 1,
            rgba8: vec![255; 4],
        }
    }

    pub fn byte_len(&self) -> Option<usize> {
        (self.width as usize)
            .checked_mul(self.height as usize)?
            .checked_mul(4)
    }

    pub fn validate(&self) -> Result<()> {
        if self.width == 0 || self.height == 0 {
            return Err(RenderError::InvalidResource(
                "texture dimensions must be positive".into(),
            ));
        }
        if self.byte_len() != Some(self.rgba8.len()) {
            return Err(RenderError::InvalidResource(format!(
                "texture {}x{} requires {:?} RGBA8 bytes, got {}",
                self.width,
                self.height,
                self.byte_len(),
                self.rgba8.len()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> VertexLayout {
        VertexLayout::new(
            20,
            [
                VertexAttribute {
                    location: 0,
                    format: VertexFormat::Float32x3,
                    offset: 0,
                },
                VertexAttribute {
                    location: 1,
                    format: VertexFormat::Float32x2,
                    offset: 12,
                },
            ],
        )
    }

    #[test]
    fn valid_layout_is_accepted_and_attributes_are_found_by_location() {
        let layout = layout();
        layout.validate().unwrap();
        assert_eq!(layout.attribute(1).unwrap().offset, 12);
        assert!(layout.attribute(7).is_none());
    }

    #[test]
    fn layout_rejects_zero_stride_overflowing_unaligned_and_duplicate_attributes() {
        assert!(VertexLayout::new(0, []).validate().is_err());
        assert!(VertexLayout::new(16, []).validate().is_err());
        let mut overflow = layout();
        overflow.stride = 16;
        assert!(
            overflow
                .validate()
                .unwrap_err()
                .to_string()
                .contains("exceeds stride")
        );
        let mut unaligned = layout();
        unaligned.attributes[1].offset = 10;
        assert!(
            unaligned
                .validate()
                .unwrap_err()
                .to_string()
                .contains("unaligned")
        );
        let mut duplicate = layout();
        duplicate.attributes[1].location = 0;
        assert!(
            duplicate
                .validate()
                .unwrap_err()
                .to_string()
                .contains("duplicate")
        );
    }

    #[test]
    fn mesh_derives_vertex_count_and_checks_indices_and_triangles() {
        let mesh = MeshResource::new(vec![0; 60], layout(), vec![]).unwrap();
        assert_eq!((mesh.vertex_count, mesh.draw_count()), (3, 3));
        assert!(!mesh.is_indexed());
        let indexed = MeshResource::new(vec![0; 60], layout(), vec![0, 1, 2, 2, 1, 0]).unwrap();
        assert_eq!(indexed.draw_count(), 6);
        assert!(MeshResource::new(vec![0; 59], layout(), vec![]).is_err());
        assert!(
            MeshResource::new(vec![0; 60], layout(), vec![0, 1, 3])
                .unwrap_err()
                .to_string()
                .contains("exceeds vertex count")
        );
        assert!(
            MeshResource::new(vec![0; 40], layout(), vec![])
                .unwrap_err()
                .to_string()
                .contains("complete triangles")
        );
        assert!(
            MeshResource::new(vec![], layout(), vec![])
                .unwrap_err()
                .to_string()
                .contains("no vertices")
        );
    }

    #[test]
    fn texture_requires_exact_rgba8_byte_count() {
        assert!(Texture2D::new(2, 2, vec![0; 16]).is_ok());
        assert!(Texture2D::new(2, 2, vec![0; 15]).is_err());
        assert!(Texture2D::new(0, 2, vec![]).is_err());
        assert!(Texture2D::new(u32::MAX, u32::MAX, vec![]).is_err());
        assert_eq!(Texture2D::white().rgba8, [255; 4]);
    }

    #[test]
    fn handles_carry_kind_and_raw_id_and_never_compare_across_kinds() {
        let mesh = MeshHandle::from_raw(4);
        let texture = TextureHandle::from_raw(4);
        let (a, b): (ResourceId, ResourceId) = (mesh.into(), texture.into());
        assert_ne!(a, b);
        assert_eq!((a.kind(), a.raw()), (ResourceKind::Mesh, 4));
        assert_eq!(b.to_string(), "texture#4");
    }

    #[test]
    fn shader_source_defaults_to_conventional_entry_points() {
        let source = ShaderSource::wgsl("// code");
        assert_eq!(
            (source.vertex_entry.as_str(), source.fragment_entry.as_str()),
            ("vs_main", "fs_main")
        );
        let custom = source.with_entry_points("v", "f");
        assert_eq!(
            (custom.vertex_entry.as_str(), custom.fragment_entry.as_str()),
            ("v", "f")
        );
    }
}
