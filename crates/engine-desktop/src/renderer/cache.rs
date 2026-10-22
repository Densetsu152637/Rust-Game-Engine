//! Arc-pointer keyed caches of uploaded meshes and textures.
//!
//! CPU assets are immutable `Arc` snapshots: retaining the same `Arc` means
//! unchanged; publish a new `Arc` for edits. Each entry retains its `Arc` so the
//! address cannot be reused by a different asset while the entry lives. Entries
//! absent from a frame are retired through [`RenderDevice::destroy`], which
//! defers the backend release across frames in flight.

use super::vertex::mesh_resource;
use anyhow::Result;
use engine_project::{
    content::{Mesh, Texture},
    runtime::Draw,
};
use engine_render::{
    MeshHandle, RenderBackend, RenderDevice, ResourceId, Texture2D, TextureHandle,
};
use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    sync::Arc,
};

/// Identity of an `Arc` allocation.
pub(crate) fn address<T>(value: &Arc<T>) -> usize {
    Arc::as_ptr(value) as usize
}

struct CachedMesh {
    source: Arc<Mesh>,
    /// `None` for an empty mesh: nothing is uploaded and its draws are skipped.
    handle: Option<MeshHandle>,
}

struct CachedTexture {
    _source: Arc<Texture>,
    handle: TextureHandle,
}

/// What [`ResourceCache::sync`] uploaded this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct UploadCounts {
    pub meshes: usize,
    pub textures: usize,
}

#[derive(Default)]
pub(crate) struct ResourceCache {
    meshes: HashMap<usize, CachedMesh>,
    textures: HashMap<usize, CachedTexture>,
}

impl ResourceCache {
    /// Whether `mesh` is already uploaded from this exact `Arc`.
    pub fn has_current_mesh(&self, mesh: &Arc<Mesh>) -> bool {
        self.meshes
            .get(&address(mesh))
            .is_some_and(|cached| Arc::ptr_eq(&cached.source, mesh))
    }

    pub fn mesh_count(&self) -> usize {
        self.meshes.len()
    }

    pub fn texture_count(&self) -> usize {
        self.textures.len()
    }

    /// Handle of the uploaded mesh; `None` when it is empty or not cached.
    pub fn mesh_handle(&self, mesh: &Arc<Mesh>) -> Option<MeshHandle> {
        self.meshes.get(&address(mesh))?.handle
    }

    pub fn texture_handle(&self, texture: &Arc<Texture>) -> Option<TextureHandle> {
        Some(self.textures.get(&address(texture))?.handle)
    }

    /// Upload what `draws` need and retire what they no longer reference.
    pub fn sync<B: RenderBackend>(
        &mut self,
        device: &mut RenderDevice<B>,
        draws: &[Draw],
    ) -> Result<UploadCounts> {
        let mut counts = UploadCounts::default();
        let mut live_meshes = HashSet::new();
        let mut live_textures = HashSet::new();
        for draw in draws {
            let key = address(&draw.mesh);
            live_meshes.insert(key);
            if let Entry::Vacant(slot) = self.meshes.entry(key) {
                let handle = if draw.mesh.vertices.is_empty() && draw.mesh.indices.is_empty() {
                    None
                } else {
                    Some(device.create_mesh(&mesh_resource(&draw.mesh)?)?)
                };
                slot.insert(CachedMesh {
                    source: draw.mesh.clone(),
                    handle,
                });
                counts.meshes += 1;
            }
            if let Some(texture) = &draw.texture {
                let key = address(texture);
                live_textures.insert(key);
                if let Entry::Vacant(slot) = self.textures.entry(key) {
                    let handle = device.create_texture(&Texture2D::new(
                        texture.width,
                        texture.height,
                        texture.pixels.clone(),
                    )?)?;
                    slot.insert(CachedTexture {
                        _source: texture.clone(),
                        handle,
                    });
                    counts.textures += 1;
                }
            }
        }
        let mut retired: Vec<ResourceId> = Vec::new();
        self.meshes.retain(|key, cached| {
            let keep = live_meshes.contains(key);
            if !keep && let Some(handle) = cached.handle {
                retired.push(handle.into());
            }
            keep
        });
        self.textures.retain(|key, cached| {
            let keep = live_textures.contains(key);
            if !keep {
                retired.push(cached.handle.into());
            }
            keep
        });
        for id in retired {
            device.destroy(id)?;
        }
        Ok(counts)
    }

    /// Retire everything (the device is about to be torn down or replaced).
    pub fn clear<B: RenderBackend>(&mut self, device: &mut RenderDevice<B>) -> Result<()> {
        for (_, cached) in self.meshes.drain() {
            if let Some(handle) = cached.handle {
                device.destroy(handle)?;
            }
        }
        for (_, cached) in self.textures.drain() {
            device.destroy(cached.handle)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::content::Vertex;
    use engine_project::runtime::{DrawSpace, DrawState};
    use engine_render::DeviceConfig;
    use engine_render::headless::RecordingBackend;

    fn device() -> RenderDevice<RecordingBackend> {
        RenderDevice::new(RecordingBackend::new(), DeviceConfig::default()).unwrap()
    }

    fn triangle() -> Arc<Mesh> {
        let vertex = Vertex {
            position: [0.0; 3],
            uv: [0.0; 2],
            color: [1.0; 4],
        };
        Arc::new(Mesh {
            vertices: vec![vertex; 3],
            indices: vec![],
        })
    }

    fn texture() -> Arc<Texture> {
        Arc::new(Texture {
            width: 1,
            height: 1,
            pixels: vec![255; 4],
        })
    }

    fn draw(id: &str, mesh: &Arc<Mesh>, texture: Option<&Arc<Texture>>) -> Draw {
        Draw {
            id: id.into(),
            mesh: mesh.clone(),
            texture: texture.cloned(),
            layer: 0,
            space: DrawSpace::World,
            clip: None,
            state: DrawState::default(),
        }
    }

    #[test]
    fn same_arc_is_uploaded_once_and_new_arcs_upload_again() {
        let mut device = device();
        let mut cache = ResourceCache::default();
        let (mesh, atlas) = (triangle(), texture());
        let draws = [draw("a", &mesh, Some(&atlas))];
        let first = cache.sync(&mut device, &draws).unwrap();
        assert_eq!(
            first,
            UploadCounts {
                meshes: 1,
                textures: 1
            }
        );
        let again = cache.sync(&mut device, &draws).unwrap();
        assert_eq!(again, UploadCounts::default());
        assert!(cache.has_current_mesh(&mesh));
        // An equal but distinct snapshot is a different asset.
        let edited = Arc::new((*mesh).clone());
        assert!(!cache.has_current_mesh(&edited));
        let republished = cache
            .sync(&mut device, &[draw("a", &edited, Some(&atlas))])
            .unwrap();
        assert_eq!(
            republished,
            UploadCounts {
                meshes: 1,
                textures: 0
            }
        );
        assert_eq!(
            cache.mesh_count(),
            1,
            "the unreferenced snapshot was evicted"
        );
    }

    #[test]
    fn absent_resources_are_retired_through_the_device() {
        let mut device = device();
        let mut cache = ResourceCache::default();
        let (mesh, atlas) = (triangle(), texture());
        cache
            .sync(&mut device, &[draw("a", &mesh, Some(&atlas))])
            .unwrap();
        assert_eq!(device.counts().meshes, 1);
        assert_eq!(device.counts().textures, 1);
        cache.sync(&mut device, &[]).unwrap();
        assert_eq!((cache.mesh_count(), cache.texture_count()), (0, 0));
        let counts = device.counts();
        assert_eq!((counts.meshes, counts.textures), (0, 0));
        assert_eq!(counts.pending_destruction, 2, "release is deferred");
    }

    #[test]
    fn empty_meshes_are_cached_without_a_device_resource() {
        let mut device = device();
        let mut cache = ResourceCache::default();
        let empty = Arc::new(Mesh::default());
        let counts = cache.sync(&mut device, &[draw("e", &empty, None)]).unwrap();
        assert_eq!(counts.meshes, 1);
        assert_eq!(cache.mesh_count(), 1);
        assert_eq!(cache.mesh_handle(&empty), None);
        assert_eq!(device.counts().meshes, 0);
    }

    #[test]
    fn clear_retires_everything() {
        let mut device = device();
        let mut cache = ResourceCache::default();
        let (mesh, atlas) = (triangle(), texture());
        cache
            .sync(&mut device, &[draw("a", &mesh, Some(&atlas))])
            .unwrap();
        cache.clear(&mut device).unwrap();
        assert_eq!(cache.mesh_count() + cache.texture_count(), 0);
        assert_eq!(device.counts().pending_destruction, 2);
    }
}
