//! `AcousticWorldDocument`: the serialisable source of an acoustic world (section
//! 7.6, asset kind `"acoustics"`). It stores source geometry, never a trusted BVH:
//! loading replays it through [`AcousticWorldBuilder`], so every limit and
//! validation rule applies again, and the BVH is rebuilt.
//!
//! Replay order is fixed: materials, meshes, instances, volumes, then the proxy, so
//! `MeshId`s and `InstanceId`s in the document are the builder's ids. The content
//! hash is FNV-1a 64 over a canonical little-endian encoding of every field except
//! `content_hash` itself (floats by their bit patterns), written as
//! `"fnv1a64:<16 hex digits>"`.

use crate::{
    AcousticMaterial, AcousticMesh, AcousticVolume, AcousticWorld, AcousticWorldBuilder,
    AcousticsError, Affine3, Enclosure, Footprint, Footprint2D, MeshId, Mobility, Proxy2DSettings,
    ProxyMapping, ProxyPlane, Result, VolumeKind, VolumeShape, WorldLimits,
};

/// Current document version.
pub const DOCUMENT_VERSION: u32 = 1;
/// Largest serialized document accepted by loaders (section 7.6).
pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DocumentInstance {
    pub mesh: MeshId,
    pub transform: Affine3,
    pub mobility: Mobility,
    pub mask: u8,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DocumentVolume {
    pub volume: AcousticVolume,
    pub transform: Affine3,
}
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DocumentProxy {
    pub footprints: Vec<Footprint>,
    pub settings: Proxy2DSettings,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AcousticWorldDocument {
    pub version: u32,
    /// The scene the geometry was baked from (informational).
    #[serde(default)]
    pub source_scene: Option<String>,
    pub materials: Vec<AcousticMaterial>,
    #[serde(default)]
    pub meshes: Vec<AcousticMesh>,
    #[serde(default)]
    pub instances: Vec<DocumentInstance>,
    #[serde(default)]
    pub volumes: Vec<DocumentVolume>,
    #[serde(default)]
    pub proxy: Option<DocumentProxy>,
    /// [`Self::compute_hash`] of the other fields; empty skips the check.
    #[serde(default)]
    pub content_hash: String,
}

fn invalid(message: impl Into<String>) -> AcousticsError {
    AcousticsError::new("acoustics.world.invalid", message)
}

struct Fnv(u64);
impl Fnv {
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= u64::from(x);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }
    fn len(&mut self, n: usize) {
        self.bytes(&(n as u64).to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.u32(v.to_bits());
    }
    fn floats(&mut self, v: &[f32]) {
        self.len(v.len());
        for &x in v {
            self.f32(x);
        }
    }
    fn tag(&mut self, t: u8) {
        self.bytes(&[t]);
    }
}

impl AcousticWorldDocument {
    pub fn new(source_scene: Option<String>) -> Self {
        Self {
            version: DOCUMENT_VERSION,
            source_scene,
            materials: Vec::new(),
            meshes: Vec::new(),
            instances: Vec::new(),
            volumes: Vec::new(),
            proxy: None,
            content_hash: String::new(),
        }
    }

    /// Canonical content hash of everything except `content_hash`.
    pub fn compute_hash(&self) -> String {
        let mut h = Fnv(0xcbf2_9ce4_8422_2325);
        h.u32(self.version);
        match &self.source_scene {
            Some(s) => {
                h.tag(1);
                h.len(s.len());
                h.bytes(s.as_bytes());
            }
            None => h.tag(0),
        }
        h.len(self.materials.len());
        for m in &self.materials {
            h.floats(&m.absorption);
            h.f32(m.scattering);
            h.floats(&m.transmission);
        }
        h.len(self.meshes.len());
        for mesh in &self.meshes {
            h.len(mesh.positions.len());
            for p in &mesh.positions {
                h.floats(p);
            }
            h.len(mesh.triangles.len());
            for t in &mesh.triangles {
                t.iter().for_each(|&v| h.u32(v));
            }
            h.len(mesh.materials.len());
            for m in &mesh.materials {
                h.u32(u32::from(m.0));
            }
        }
        let mobility = |h: &mut Fnv, m: Mobility| h.tag(matches!(m, Mobility::Dynamic) as u8);
        h.len(self.instances.len());
        for i in &self.instances {
            h.u32(i.mesh.0);
            h.floats(&i.transform);
            mobility(&mut h, i.mobility);
            h.tag(i.mask);
        }
        h.len(self.volumes.len());
        for v in &self.volumes {
            match v.volume.shape {
                VolumeShape::Box { half_extents } => {
                    h.tag(0);
                    h.floats(&half_extents);
                }
                VolumeShape::Mesh(id) => {
                    h.tag(1);
                    h.u32(id.0);
                }
            }
            match v.volume.kind {
                VolumeKind::Solid => h.tag(0),
                VolumeKind::Room { open_faces } => {
                    h.tag(1);
                    h.tag(open_faces);
                }
            }
            h.u32(u32::from(v.volume.material.0));
            mobility(&mut h, v.volume.mobility);
            h.floats(&v.transform);
        }
        match &self.proxy {
            None => h.tag(0),
            Some(p) => {
                h.tag(1);
                h.len(p.footprints.len());
                for f in &p.footprints {
                    match &f.shape {
                        Footprint2D::Rect {
                            center,
                            half_extents,
                            rotation,
                        } => {
                            h.tag(0);
                            h.floats(center);
                            h.floats(half_extents);
                            h.f32(*rotation);
                        }
                        Footprint2D::Circle { center, radius } => {
                            h.tag(1);
                            h.floats(center);
                            h.f32(*radius);
                        }
                        Footprint2D::Capsule {
                            center,
                            half_height,
                            radius,
                            rotation,
                        } => {
                            h.tag(2);
                            h.floats(center);
                            h.floats(&[*half_height, *radius, *rotation]);
                        }
                        Footprint2D::Polygon { points } => {
                            h.tag(3);
                            h.len(points.len());
                            for p in points {
                                h.floats(p);
                            }
                        }
                    }
                    h.u32(u32::from(f.material.0));
                    mobility(&mut h, f.mobility);
                }
                let s = &p.settings;
                h.tag(matches!(s.plane, ProxyPlane::TopDown) as u8);
                h.floats(&[
                    s.units_per_meter,
                    s.depth_m,
                    s.wall_height_m,
                    s.ear_height_m,
                ]);
                for b in [s.enclose_depth, s.floor, s.ceiling, s.open_sky] {
                    h.tag(b as u8);
                }
                match s.enclosure {
                    Enclosure::None => h.tag(0),
                    Enclosure::Auto { margin_m } => {
                        h.tag(1);
                        h.f32(margin_m);
                    }
                    Enclosure::Bounds { min, max } => {
                        h.tag(2);
                        h.floats(&min);
                        h.floats(&max);
                    }
                }
                for m in [
                    s.material,
                    s.floor_material,
                    s.ceiling_material,
                    s.side_material,
                ] {
                    h.u32(u32::from(m.0));
                }
            }
        }
        format!("fnv1a64:{:016x}", h.0)
    }

    /// Sets `content_hash` from the current contents.
    pub fn seal(&mut self) {
        self.content_hash = self.compute_hash();
    }

    /// Checks the version and (when present) the hash, then replays the document
    /// into a builder under `limits`.
    pub fn to_builder(
        &self,
        limits: WorldLimits,
    ) -> Result<(AcousticWorldBuilder, Option<ProxyMapping>)> {
        if self.version != DOCUMENT_VERSION {
            return Err(invalid(format!(
                "unsupported acoustic world document version {}",
                self.version
            )));
        }
        if !self.content_hash.is_empty() && self.content_hash != self.compute_hash() {
            return Err(invalid("acoustic world document content hash mismatch"));
        }
        let mut b = AcousticWorldBuilder::new(limits);
        for m in &self.materials {
            b.add_material(*m)?;
        }
        for mesh in &self.meshes {
            b.add_mesh(mesh.clone())?;
        }
        for i in &self.instances {
            if i.mesh.0 as usize >= self.meshes.len() {
                return Err(invalid(format!("instance references mesh {}", i.mesh.0)));
            }
            b.add_instance(i.mesh, i.transform, i.mobility, i.mask)?;
        }
        for v in &self.volumes {
            if let VolumeShape::Mesh(id) = v.volume.shape
                && id.0 as usize >= self.meshes.len()
            {
                return Err(invalid(format!("volume references mesh {}", id.0)));
            }
            b.add_volume(v.volume, v.transform)?;
        }
        let proxy = match &self.proxy {
            Some(p) => Some(b.add_proxy_2d(&p.footprints, &p.settings)?),
            None => None,
        };
        Ok((b, proxy))
    }

    /// Validates and builds the world (the proxy mapping is also `world.proxy()`).
    pub fn build(&self, limits: WorldLimits) -> Result<AcousticWorld> {
        self.to_builder(limits)?.0.build()
    }
}
