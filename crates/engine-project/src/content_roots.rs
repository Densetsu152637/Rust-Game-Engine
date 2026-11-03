//! Additional read-only content roots (mods, installed Workshop items).
//!
//! A root is a directory with a `content.json` manifest listing leaf data assets. Mounted
//! assets are addressed by the full id `@<mount>/<id>` and resolved through
//! [`Project::resolve_asset`](crate::Project::resolve_asset). The project root always wins
//! (project ids never start with `@`), and every path is re-checked on each resolution.
//! See `docs/asset-pipeline.md`, "Additional read-only content roots".

use crate::{Asset, Error, Result, confined_file};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    sync::{Arc, RwLock},
};

/// Most mounted roots.
pub const MAX_MOUNTS: usize = 64;
/// Most assets one root may declare.
pub const MAX_ROOT_ASSETS: usize = 4096;
/// Most assets across all mounted roots.
pub const MAX_MOUNTED_ASSETS: usize = 16384;
/// Most `font` assets one root may declare (each is parsed at registration).
pub const MAX_ROOT_FONTS: usize = 16;
/// The manifest file name inside a root.
pub const MANIFEST_NAME: &str = "content.json";
const MANIFEST_LIMIT: u64 = 1024 * 1024;
const MAX_EVENTS: usize = 256;
/// Kinds a mounted root may provide: passive leaf data that names no other asset (a bitmap
/// font names a texture of its own root, checked at registration).
pub const ALLOWED_KINDS: [&str; 6] = ["mesh", "texture", "audio", "lut", "string_table", "font"];

/// Bumped by every successful registration of a mount.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RootRevision(pub u64);

/// What changed in the mounted roots; drained with [`ContentRoots::drain_events`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ContentRootEvent {
    Registered {
        mount: String,
        revision: RootRevision,
    },
    Replaced {
        mount: String,
        revision: RootRevision,
    },
    Unregistered {
        mount: String,
    },
}

/// A mounted root, as listed by [`ContentRoots::mounts`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountInfo {
    pub mount: String,
    pub root: PathBuf,
    pub revision: RootRevision,
    pub assets: usize,
}

#[derive(Debug)]
struct Mount {
    declared_parent: PathBuf,
    declared_root: PathBuf,
    /// Canonical root, fixed at registration; every resolution must still reach it.
    root: PathBuf,
    assets: BTreeMap<String, Asset>,
    revision: RootRevision,
}

#[derive(Debug, Default)]
struct State {
    mounts: BTreeMap<String, Mount>,
    next_revision: u64,
    events: Vec<ContentRootEvent>,
}

/// A shared handle on the mounted roots. Clones (and clones of the owning `Project`) see
/// the same mounts.
#[derive(Clone, Debug, Default)]
pub struct ContentRoots(Arc<RwLock<State>>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RootManifest {
    schema: u32,
    assets: Vec<Asset>,
}

fn err(code: &str, message: impl Into<String>) -> Error {
    Error::new(code, message)
}

fn is_link(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Symlinks, junctions and every other reparse point.
        meta.file_type().is_symlink() || meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

fn reject_link(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)
        .map_err(|e| err("content_root.path", format!("{}: {e}", path.display())))?;
    if is_link(&meta) {
        return Err(err(
            "content_root.link",
            format!("{} is a link or reparse point", path.display()),
        ));
    }
    Ok(())
}

fn valid_mount(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Checks the root path itself: absolute, strictly below `declared_parent`, no link
/// anywhere between them, a directory, and still equal to the parent joined with the
/// relative path once canonical. Returns the canonical root.
fn validate_root(root: &Path, declared_parent: &Path) -> Result<PathBuf> {
    if !root.is_absolute() || !declared_parent.is_absolute() {
        return Err(err("content_root.path", "Root and parent must be absolute"));
    }
    let relative = root.strip_prefix(declared_parent).map_err(|_| {
        err(
            "content_root.escape",
            "The root is not below its declared parent",
        )
    })?;
    let mut current = declared_parent.to_owned();
    let mut depth = 0;
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(err(
                "content_root.escape",
                "The root path may only descend from its declared parent",
            ));
        };
        current.push(part);
        reject_link(&current)?;
        depth += 1;
    }
    if depth == 0 {
        return Err(err(
            "content_root.escape",
            "The root must be a directory below its declared parent",
        ));
    }
    let parent = fs::canonicalize(declared_parent)
        .map_err(|e| err("content_root.path", format!("declared parent: {e}")))?;
    let canonical =
        fs::canonicalize(root).map_err(|e| err("content_root.path", format!("root: {e}")))?;
    if !canonical.starts_with(&parent)
        || canonical.components().count() != parent.components().count() + depth
        || !canonical.is_dir()
    {
        return Err(err(
            "content_root.escape",
            "The root does not resolve to a directory directly below its declared parent",
        ));
    }
    Ok(canonical)
}

/// A file inside a mounted root: the project confinement rules plus no link on any path
/// component (mod content is untrusted).
fn root_file(root: &Path, relative: &Path) -> Result<PathBuf> {
    let file = confined_file(root, relative)?;
    let mut current = root.to_owned();
    for component in relative.components() {
        current.push(component);
        reject_link(&current)?;
    }
    Ok(file)
}

fn local_id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
}

fn load_assets(root: &Path) -> Result<BTreeMap<String, Asset>> {
    let manifest = root_file(root, Path::new(MANIFEST_NAME))?;
    let bytes = crate::schema::read_bounded(&manifest, MANIFEST_LIMIT)?;
    let raw: RootManifest =
        serde_json::from_slice(&bytes).map_err(|e| err("content_root.manifest", e.to_string()))?;
    if raw.schema != 1 {
        return Err(err(
            "content_root.manifest",
            "Only content root schema 1 is supported",
        ));
    }
    if raw.assets.len() > MAX_ROOT_ASSETS {
        return Err(err(
            "content_root.limit",
            format!("At most {MAX_ROOT_ASSETS} assets per root"),
        ));
    }
    let mut assets = BTreeMap::new();
    let mut paths = BTreeSet::new();
    for asset in raw.assets {
        if !local_id_ok(&asset.id) || assets.contains_key(&asset.id) {
            return Err(err(
                "content_root.asset.id",
                format!("Invalid or duplicate asset id {:?}", asset.id),
            ));
        }
        if !ALLOWED_KINDS.contains(&asset.kind.as_str()) {
            return Err(err(
                "content_root.kind",
                format!("Kind {:?} is not allowed in a content root", asset.kind),
            ));
        }
        let file = root_file(root, &asset.path)?;
        if !paths.insert(file.to_string_lossy().to_lowercase()) {
            return Err(err(
                "content_root.asset.path",
                "Asset paths must be unique within a root",
            ));
        }
        assets.insert(asset.id.clone(), asset);
    }
    // A bitmap font may only name a texture of its own root.
    if assets.values().filter(|a| a.kind == "font").count() > MAX_ROOT_FONTS {
        return Err(err(
            "content_root.limit",
            format!("At most {MAX_ROOT_FONTS} fonts per root"),
        ));
    }
    for asset in assets.values().filter(|a| a.kind == "font") {
        let font = crate::content::load_font(&root_file(root, &asset.path)?)?;
        if assets
            .get(&font.texture)
            .is_none_or(|t| t.kind != "texture")
        {
            return Err(err(
                "content_root.font",
                format!(
                    "Font {:?} must name a texture asset of the same root",
                    asset.id
                ),
            ));
        }
    }
    Ok(assets)
}

impl ContentRoots {
    fn read(&self) -> std::sync::RwLockReadGuard<'_, State> {
        self.0.read().unwrap_or_else(|p| p.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, State> {
        self.0.write().unwrap_or_else(|p| p.into_inner())
    }

    /// Mounts `root` as `mount`, or replaces that mount. The root must lie strictly below
    /// `declared_parent` with no link between them. Everything is validated before the
    /// mount changes; on failure the previous registration (if any) stays.
    pub fn register(
        &self,
        mount: &str,
        root: &Path,
        declared_parent: &Path,
    ) -> Result<RootRevision> {
        if !valid_mount(mount) {
            return Err(err(
                "content_root.mount",
                "A mount name is 1 to 64 characters of letters, digits, '.', '_' or '-'",
            ));
        }
        let canonical = validate_root(root, declared_parent)?;
        let assets = load_assets(&canonical)?;
        let mut state = self.write();
        let replacing = state.mounts.contains_key(mount);
        if !replacing && state.mounts.len() >= MAX_MOUNTS {
            return Err(err(
                "content_root.limit",
                format!("At most {MAX_MOUNTS} mounted roots"),
            ));
        }
        let others: usize = state
            .mounts
            .iter()
            .filter(|(name, _)| name.as_str() != mount)
            .map(|(_, m)| m.assets.len())
            .sum();
        if others + assets.len() > MAX_MOUNTED_ASSETS {
            return Err(err(
                "content_root.limit",
                format!("At most {MAX_MOUNTED_ASSETS} mounted assets in total"),
            ));
        }
        state.next_revision += 1;
        let revision = RootRevision(state.next_revision);
        state.mounts.insert(
            mount.to_owned(),
            Mount {
                declared_parent: declared_parent.to_owned(),
                declared_root: root.to_owned(),
                root: canonical,
                assets,
                revision,
            },
        );
        let mount = mount.to_owned();
        let event = if replacing {
            ContentRootEvent::Replaced { mount, revision }
        } else {
            ContentRootEvent::Registered { mount, revision }
        };
        if state.events.len() >= MAX_EVENTS {
            state.events.remove(0);
        }
        state.events.push(event);
        Ok(revision)
    }

    /// Removes a mount; its ids stop resolving at once. False if it was not mounted.
    pub fn unregister(&self, mount: &str) -> bool {
        let mut state = self.write();
        if state.mounts.remove(mount).is_none() {
            return false;
        }
        if state.events.len() >= MAX_EVENTS {
            state.events.remove(0);
        }
        state.events.push(ContentRootEvent::Unregistered {
            mount: mount.to_owned(),
        });
        true
    }

    /// The mounted roots, ordered by mount name.
    pub fn mounts(&self) -> Vec<MountInfo> {
        self.read()
            .mounts
            .iter()
            .map(|(mount, m)| MountInfo {
                mount: mount.clone(),
                root: m.root.clone(),
                revision: m.revision,
                assets: m.assets.len(),
            })
            .collect()
    }

    /// Takes the events recorded since the last call.
    pub fn drain_events(&self) -> Vec<ContentRootEvent> {
        std::mem::take(&mut self.write().events)
    }

    /// The declared asset of a full id (`@mount/id`), without touching the disk.
    pub fn asset(&self, full_id: &str) -> Option<Asset> {
        let (mount, id) = split_id(full_id)?;
        self.read().mounts.get(mount)?.assets.get(id).cloned()
    }

    /// Copies what a resolution needs, so the lock is not held across filesystem access.
    fn snapshot(&self, full_id: &str) -> Result<Snapshot> {
        let unknown = || err("project.asset.unknown", format!("Unknown asset {full_id}"));
        let (mount, id) = split_id(full_id).ok_or_else(unknown)?;
        let state = self.read();
        let entry = state.mounts.get(mount).ok_or_else(unknown)?;
        let asset = entry.assets.get(id).ok_or_else(unknown)?;
        Ok(Snapshot {
            mount: mount.to_owned(),
            revision: entry.revision,
            declared_root: entry.declared_root.clone(),
            declared_parent: entry.declared_parent.clone(),
            root: entry.root.clone(),
            path: asset.path.clone(),
            kind: asset.kind.clone(),
        })
    }

    /// Re-validates the root path and the file named by a snapshot (no lock held).
    fn check(snapshot: &Snapshot) -> Result<PathBuf> {
        let canonical = validate_root(&snapshot.declared_root, &snapshot.declared_parent)?;
        if canonical != snapshot.root {
            return Err(err(
                "content_root.escape",
                "The root no longer resolves to where it was registered",
            ));
        }
        root_file(&snapshot.root, &snapshot.path)
    }

    /// Resolves a full id to a canonical file, re-validating the root path and the file.
    ///
    /// The check and the caller's later open are separate steps, so a hostile replacement
    /// racing between them is not prevented (see the design note); mounted folders are
    /// written by Steam, not by untrusted code running on the machine.
    pub fn resolve(&self, full_id: &str) -> Result<PathBuf> {
        Self::check(&self.snapshot(full_id)?)
    }

    /// The full id of the texture a mounted bitmap font draws with: `@<mount>/<texture>`.
    ///
    /// A font file names its texture by local id, and consumers that resolve
    /// `font.texture` as-is would look it up in the project namespace, where a project
    /// asset of the same id would be drawn instead. This re-reads the font (through the
    /// same link and confinement checks), requires the texture to be a texture asset of the
    /// same root, and returns the namespaced id to resolve.
    pub fn font_texture_id(&self, full_font_id: &str) -> Result<String> {
        let snapshot = self.snapshot(full_font_id)?;
        if snapshot.kind != "font" {
            return Err(err(
                "content_root.font",
                format!("{full_font_id} is not a font asset"),
            ));
        }
        let font = crate::content::load_font(&Self::check(&snapshot)?)?;
        let state = self.read();
        let entry = state.mounts.get(&snapshot.mount);
        let same_root = entry.is_some_and(|m| {
            m.revision == snapshot.revision
                && m.assets
                    .get(&font.texture)
                    .is_some_and(|t| t.kind == "texture")
        });
        if !same_root {
            return Err(err(
                "content_root.font",
                format!("Font {full_font_id} must name a texture asset of the same root"),
            ));
        }
        Ok(format!("@{}/{}", snapshot.mount, font.texture))
    }
}

struct Snapshot {
    mount: String,
    revision: RootRevision,
    declared_root: PathBuf,
    declared_parent: PathBuf,
    root: PathBuf,
    path: PathBuf,
    kind: String,
}

fn split_id(full_id: &str) -> Option<(&str, &str)> {
    full_id.strip_prefix('@')?.split_once('/')
}
