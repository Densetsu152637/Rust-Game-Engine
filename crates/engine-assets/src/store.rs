use crate::{AssetError, CacheFingerprint, Result};
use std::marker::PhantomData;

/// A process-local typed handle. Handles belong to the store that issued them.
#[derive(Debug)]
pub struct AssetHandle<T> {
    index: usize,
    generation: u64,
    marker: PhantomData<fn() -> T>,
}
impl<T> Copy for AssetHandle<T> {}
impl<T> PartialEq for AssetHandle<T> {
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index && self.generation == other.generation
    }
}
impl<T> Eq for AssetHandle<T> {}
impl<T> std::hash::Hash for AssetHandle<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.index.hash(state);
        self.generation.hash(state);
    }
}
impl<T> Clone for AssetHandle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

pub struct LoadedAsset<T> {
    pub value: T,
    pub fingerprint: CacheFingerprint,
}
struct Slot<T> {
    generation: u64,
    asset: Option<LoadedAsset<T>>,
}
/// Runtime cache only; caller maps stable project manifest IDs to these handles.
pub struct AssetStore<T> {
    slots: Vec<Slot<T>>,
    free: Vec<usize>,
}
impl<T> Default for AssetStore<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            free: Vec::new(),
        }
    }
}
impl<T> AssetStore<T> {
    pub fn load(&mut self, asset: LoadedAsset<T>) -> AssetHandle<T> {
        let index = if let Some(index) = self.free.pop() {
            self.slots[index].asset = Some(asset);
            index
        } else {
            self.slots.push(Slot {
                generation: 0,
                asset: Some(asset),
            });
            self.slots.len() - 1
        };
        AssetHandle {
            index,
            generation: self.slots[index].generation,
            marker: PhantomData,
        }
    }
    pub fn get(&self, handle: AssetHandle<T>) -> Option<&T> {
        self.entry(handle).map(|asset| &asset.value)
    }
    pub fn fingerprint(&self, handle: AssetHandle<T>) -> Option<CacheFingerprint> {
        self.entry(handle).map(|asset| asset.fingerprint)
    }
    fn entry(&self, handle: AssetHandle<T>) -> Option<&LoadedAsset<T>> {
        self.slots
            .get(handle.index)
            .filter(|slot| slot.generation == handle.generation)?
            .asset
            .as_ref()
    }
    /// Validates the handle first; a failed loader leaves value and fingerprint intact.
    pub fn reload(
        &mut self,
        handle: AssetHandle<T>,
        loader: impl FnOnce() -> Result<LoadedAsset<T>>,
    ) -> Result<()> {
        if self.entry(handle).is_none() {
            return Err(AssetError::InvalidHandle);
        }
        let replacement = loader()?;
        self.slots[handle.index].asset = Some(replacement);
        Ok(())
    }
    pub fn unload(&mut self, handle: AssetHandle<T>) -> Result<T> {
        if self.entry(handle).is_none() {
            return Err(AssetError::InvalidHandle);
        }
        let slot = &mut self.slots[handle.index];
        let asset = slot.asset.take().ok_or(AssetError::InvalidHandle)?;
        // Retire exhausted generations rather than allowing stale handle resurrection.
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            self.free.push(handle.index);
        }
        Ok(asset.value)
    }
}
