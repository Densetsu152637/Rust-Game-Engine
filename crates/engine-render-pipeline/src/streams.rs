//! Streamed (dynamic) textures for video and other per-frame sources.
//!
//! Two modes, both addressed by a stream name:
//! * CPU upload: the host passes the latest RGBA8 frame and its revision;
//!   the GPU texture is written in place only when the revision changes.
//! * External: the stream owns a GPU texture ([`GpuTexture`]) that the
//!   caller writes into itself (`queue.write_texture`, a copy from a
//!   hardware decoder, or rendering into it: the texture is renderable).
//!
//! Either way materials and sprites sample the same texture object every
//! frame, so nothing is re-created per frame.

use crate::resources::GpuTexture;
use anyhow::{Result, ensure};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Most streams one renderer keeps.
pub const MAX_STREAMS: usize = 64;

/// How a stream's texels arrive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamMode {
    CpuUpload,
    External,
}

struct Stream {
    mode: StreamMode,
    texture: Arc<GpuTexture>,
    revision: Option<u64>,
}

/// The renderer's streamed textures.
#[derive(Default)]
pub struct TextureStreams {
    streams: HashMap<String, Stream>,
}

impl TextureStreams {
    fn entry(
        &mut self,
        device: &wgpu::Device,
        name: &str,
        mode: StreamMode,
        size: [u32; 2],
    ) -> Result<&mut Stream> {
        ensure!(!name.is_empty(), "texture stream names must be nonempty");
        let fits = self
            .streams
            .get(name)
            .is_some_and(|s| s.mode == mode && s.texture.size == size);
        if !fits {
            ensure!(
                self.streams.contains_key(name) || self.streams.len() < MAX_STREAMS,
                "at most {MAX_STREAMS} texture streams"
            );
            let texture = GpuTexture::new(device, &format!("texture stream {name}"), size)?;
            self.streams.insert(
                name.to_owned(),
                Stream {
                    mode,
                    texture,
                    revision: None,
                },
            );
        }
        Ok(self.streams.get_mut(name).expect("inserted above"))
    }

    /// The CPU-upload stream `name` showing `pixels` (tightly packed sRGB
    /// RGBA8 of `size`) at `revision`. Uploads only when the revision (or
    /// size) changed since the last call.
    pub fn sync_cpu(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        name: &str,
        size: [u32; 2],
        revision: u64,
        pixels: &[u8],
    ) -> Result<Arc<GpuTexture>> {
        let stream = self.entry(device, name, StreamMode::CpuUpload, size)?;
        if stream.revision != Some(revision) {
            stream.texture.write(queue, pixels)?;
            stream.revision = Some(revision);
        }
        Ok(stream.texture.clone())
    }

    /// The external stream `name` of `size`: the returned texture is the
    /// handle the caller writes into (it persists until the size changes or
    /// the stream is dropped).
    pub fn external(
        &mut self,
        device: &wgpu::Device,
        name: &str,
        size: [u32; 2],
    ) -> Result<Arc<GpuTexture>> {
        Ok(self
            .entry(device, name, StreamMode::External, size)?
            .texture
            .clone())
    }

    pub fn get(&self, name: &str) -> Option<Arc<GpuTexture>> {
        self.streams.get(name).map(|s| s.texture.clone())
    }

    pub fn mode(&self, name: &str) -> Option<StreamMode> {
        self.streams.get(name).map(|s| s.mode)
    }

    /// Drop streams whose name is not in `live`.
    pub fn retain(&mut self, live: &HashSet<&str>) {
        self.streams.retain(|name, _| live.contains(name.as_str()));
    }

    pub fn len(&self) -> usize {
        self.streams.len()
    }

    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }
}
