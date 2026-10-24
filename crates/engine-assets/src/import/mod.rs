use crate::*;
use base64::Engine;
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
};
mod animation;
mod geometry;

pub const IMPORTER_VERSION: &str = "engine-gltf/1";
#[derive(Clone, Debug)]
pub struct ImportLimits {
    pub max_source_bytes: usize,
    /// Sum of source and all loaded buffer/image bytes (embedded data counts too).
    pub max_total_bytes: usize,
    /// Aggregate document objects, primitive attributes, hierarchy edges, and channels.
    pub max_entities: usize,
    pub max_vertices: usize,
    pub max_indices: usize,
    /// Aggregate input keys across animation channels.
    pub max_animation_keys: usize,
    pub max_image_dimension: u32,
    pub max_decoded_image_bytes: usize,
}
impl Default for ImportLimits {
    fn default() -> Self {
        Self {
            max_source_bytes: 64 * 1024 * 1024,
            max_total_bytes: 256 * 1024 * 1024,
            max_entities: 100_000,
            max_vertices: 1_000_000,
            max_indices: 3_000_000,
            max_animation_keys: 1_000_000,
            max_image_dimension: 8192,
            max_decoded_image_bytes: 256 * 1024 * 1024,
        }
    }
}
fn invalid(message: &str) -> AssetError {
    AssetError::Invalid(message.into())
}
fn add_budget(total: &mut usize, count: usize, limit: usize, name: &'static str) -> Result<()> {
    *total = total
        .checked_add(count)
        .filter(|value| *value <= limit)
        .ok_or(AssetError::Limit(name))?;
    Ok(())
}
fn finite(values: impl IntoIterator<Item = f32>) -> Result<()> {
    if values.into_iter().all(f32::is_finite) {
        Ok(())
    } else {
        Err(invalid("nonfinite numeric data"))
    }
}

/// Import a file under `project_root`. Relative source paths are project-relative;
/// glTF URIs are source-relative. No working-directory URI resolution occurs.
pub fn import_gltf(
    project_root: &Path,
    source: &Path,
    limits: &ImportLimits,
) -> Result<ImportedScene> {
    let root = project_root.canonicalize()?;
    let path = if source.is_absolute() {
        source.to_path_buf()
    } else {
        root.join(source)
    }
    .canonicalize()?;
    if !path.starts_with(&root) {
        return Err(AssetError::OutsideProject);
    }
    let mut context = Context {
        root,
        parent: path
            .parent()
            .ok_or_else(|| invalid("source has no parent"))?
            .to_path_buf(),
        limits,
        total: 0,
        dependencies: BTreeMap::new(),
    };
    let bytes = context.read_file(&path, limits.max_source_bytes)?;
    let parsed = gltf::Gltf::from_slice(&bytes).map_err(|e| invalid(&e.to_string()))?;
    let document = &parsed.document;
    if let Some(extension) = document.extensions_required().next() {
        return Err(AssetError::Unsupported(format!(
            "required extension {extension}"
        )));
    }
    validate_entity_budget(document, limits)?;
    let mut buffers = Vec::new();
    for buffer in document.buffers() {
        let data = match buffer.source() {
            gltf::buffer::Source::Bin => {
                let data = parsed
                    .blob
                    .as_ref()
                    .ok_or_else(|| invalid("missing GLB binary chunk"))?;
                context.charge(data.len())?;
                data.clone()
            }
            gltf::buffer::Source::Uri(uri) => context.uri(uri)?,
        };
        if data.len() < buffer.length() {
            return Err(invalid("buffer shorter than declared byteLength"));
        }
        buffers.push(data);
    }
    validate_accessors(document, &buffers, limits)?;
    let nodes = import_nodes(document)?;
    let meshes = geometry::import_meshes(document, &buffers, limits)?;
    let skins = animation::import_skins(document, &buffers)?;
    geometry::validate_skin_vertices(&nodes, &meshes, &skins)?;
    let clips = animation::import_clips(document, &buffers, limits)?;
    let images = import_images(document, &buffers, &mut context)?;
    let materials = document
        .materials()
        .map(import_material)
        .collect::<Result<_>>()?;
    let textures = document
        .textures()
        .map(|texture| {
            let sampler = texture.sampler();
            ImportedTexture {
                image: texture.source().index(),
                mag_filter: sampler.mag_filter().map(|v| v.as_gl_enum()),
                min_filter: sampler.min_filter().map(|v| v.as_gl_enum()),
                wrap_s: sampler.wrap_s().as_gl_enum(),
                wrap_t: sampler.wrap_t().as_gl_enum(),
            }
        })
        .collect();
    let dependencies: Vec<_> = context.dependencies.into_values().collect();
    let fingerprint = fingerprint(&dependencies);
    Ok(ImportedScene {
        nodes,
        scenes: document
            .scenes()
            .map(|s| s.nodes().map(|n| n.index()).collect())
            .collect(),
        default_scene: document.default_scene().map(|s| s.index()),
        meshes,
        materials,
        images,
        textures,
        skins,
        clips,
        dependencies,
        fingerprint,
    })
}
struct Context<'a> {
    root: PathBuf,
    parent: PathBuf,
    limits: &'a ImportLimits,
    total: usize,
    dependencies: BTreeMap<PathBuf, ImportedDependency>,
}
impl Context<'_> {
    fn charge(&mut self, bytes: usize) -> Result<()> {
        add_budget(
            &mut self.total,
            bytes,
            self.limits.max_total_bytes,
            "total input bytes",
        )
    }
    fn read_file(&mut self, path: &Path, max: usize) -> Result<Vec<u8>> {
        let canonical = path.canonicalize()?;
        if !canonical.starts_with(&self.root) {
            return Err(AssetError::OutsideProject);
        }
        let remaining = self
            .limits
            .max_total_bytes
            .saturating_sub(self.total)
            .min(max);
        let file = File::open(&canonical)?;
        if file.metadata()?.len() > remaining as u64 {
            return Err(AssetError::Limit("input bytes"));
        }
        let mut bytes = Vec::new();
        file.take((remaining as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > remaining {
            return Err(AssetError::Limit("input bytes"));
        }
        self.charge(bytes.len())?;
        let relative = canonical
            .strip_prefix(&self.root)
            .map_err(|_| AssetError::OutsideProject)?
            .to_path_buf();
        self.dependencies.insert(
            relative.clone(),
            ImportedDependency {
                path: relative,
                fingerprint: CacheFingerprint(*blake3::hash(&bytes).as_bytes()),
                byte_len: bytes.len(),
            },
        );
        Ok(bytes)
    }
    fn uri(&mut self, uri: &str) -> Result<Vec<u8>> {
        if let Some(data) = uri.strip_prefix("data:") {
            let (header, payload) = data
                .split_once(',')
                .ok_or_else(|| invalid("malformed data URI"))?;
            if !header.ends_with(";base64") {
                return Err(AssetError::Unsupported("non-base64 data URI".into()));
            }
            let upper = payload
                .len()
                .checked_div(4)
                .and_then(|v| v.checked_mul(3))
                .and_then(|v| v.checked_add(3))
                .ok_or(AssetError::Limit("data URI"))?;
            if upper.saturating_sub(3) > self.limits.max_total_bytes.saturating_sub(self.total) {
                return Err(AssetError::Limit("data URI"));
            }
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(payload)
                .map_err(|_| invalid("invalid base64 data URI"))?;
            self.charge(bytes.len())?;
            return Ok(bytes);
        }
        let decoded = decode_uri_path(uri)?;
        let relative = Path::new(&decoded);
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
        {
            return Err(AssetError::OutsideProject);
        }
        let path = self.parent.join(relative);
        self.read_file(&path, self.limits.max_total_bytes)
    }
}
fn decode_uri_path(uri: &str) -> Result<String> {
    let mut decoded = Vec::new();
    let mut bytes = uri.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes
                .next()
                .and_then(|v| (v as char).to_digit(16))
                .ok_or_else(|| invalid("invalid URI escape"))?;
            let low = bytes
                .next()
                .and_then(|v| (v as char).to_digit(16))
                .ok_or_else(|| invalid("invalid URI escape"))?;
            decoded.push((high * 16 + low) as u8);
        } else {
            decoded.push(byte);
        }
    }
    let decoded = String::from_utf8(decoded).map_err(|_| invalid("non-UTF8 asset URI"))?;
    if decoded.contains([':', '\\', '\0', '?', '#']) {
        return Err(AssetError::Unsupported(
            "nonlocal or ambiguous asset URI".into(),
        ));
    }
    Ok(decoded)
}
fn fingerprint(dependencies: &[ImportedDependency]) -> CacheFingerprint {
    let mut hash = blake3::Hasher::new();
    hash.update(IMPORTER_VERSION.as_bytes());
    for dep in dependencies {
        let path = dep.path.to_string_lossy().replace('\\', "/");
        hash.update(&(path.len() as u64).to_le_bytes());
        hash.update(path.as_bytes());
        hash.update(&(dep.byte_len as u64).to_le_bytes());
        hash.update(&dep.fingerprint.0);
    }
    CacheFingerprint(*hash.finalize().as_bytes())
}
fn validate_entity_budget(document: &gltf::Document, limits: &ImportLimits) -> Result<()> {
    let mut count = 0;
    for n in [
        document.nodes().len(),
        document.meshes().len(),
        document.materials().len(),
        document.textures().len(),
        document.images().len(),
        document.skins().len(),
        document.animations().len(),
        document.scenes().len(),
        document.accessors().len(),
        document.views().len(),
        document.buffers().len(),
        document.samplers().len(),
    ] {
        add_budget(&mut count, n, limits.max_entities, "document entities")?;
    }
    for node in document.nodes() {
        add_budget(
            &mut count,
            node.children().count(),
            limits.max_entities,
            "node edges",
        )?;
    }
    for scene in document.scenes() {
        add_budget(
            &mut count,
            scene.nodes().count(),
            limits.max_entities,
            "scene roots",
        )?;
    }
    for skin in document.skins() {
        add_budget(
            &mut count,
            skin.joints().count(),
            limits.max_entities,
            "skin joints",
        )?;
    }
    for mesh in document.meshes() {
        for primitive in mesh.primitives() {
            add_budget(
                &mut count,
                1 + primitive.attributes().count(),
                limits.max_entities,
                "primitives and attributes",
            )?;
        }
    }
    for clip in document.animations() {
        add_budget(
            &mut count,
            clip.channels().count() + clip.samplers().count(),
            limits.max_entities,
            "animation entities",
        )?;
    }
    Ok(())
}
fn validate_accessors(
    document: &gltf::Document,
    buffers: &[Vec<u8>],
    limits: &ImportLimits,
) -> Result<()> {
    for view in document.views() {
        let end = view
            .offset()
            .checked_add(view.length())
            .ok_or_else(|| invalid("buffer view range overflow"))?;
        if end > buffers[view.buffer().index()].len() || end > view.buffer().length() {
            return Err(invalid("buffer view outside buffer"));
        }
    }
    for accessor in document.accessors() {
        if accessor.sparse().is_some() {
            return Err(AssetError::Unsupported("sparse accessors".into()));
        }
        let max = limits
            .max_vertices
            .max(limits.max_indices)
            .max(limits.max_animation_keys.saturating_mul(3))
            .max(limits.max_entities);
        if accessor.count() == 0 || accessor.count() > max {
            return Err(AssetError::Limit("accessor elements"));
        }
        let view = accessor
            .view()
            .ok_or_else(|| invalid("accessor missing buffer view"))?;
        let stride = view.stride().unwrap_or(accessor.size());
        if stride < accessor.size() {
            return Err(invalid("accessor stride smaller than element"));
        }
        let end = (accessor.count() - 1)
            .checked_mul(stride)
            .and_then(|v| v.checked_add(accessor.offset()))
            .and_then(|v| v.checked_add(accessor.size()))
            .ok_or_else(|| invalid("accessor range overflow"))?;
        if end > view.length() {
            return Err(invalid("accessor outside buffer view"));
        }
    }
    Ok(())
}
fn import_nodes(document: &gltf::Document) -> Result<Vec<ImportedNode>> {
    let mut nodes = Vec::new();
    for node in document.nodes() {
        let (translation, rotation, scale) = node.transform().decomposed();
        let matrix = node.transform().matrix();
        finite(
            translation
                .into_iter()
                .chain(rotation)
                .chain(scale)
                .chain(matrix.into_iter().flatten()),
        )?;
        if rotation.iter().map(|x| x * x).sum::<f32>() < 1e-12 {
            return Err(invalid("zero node quaternion"));
        }
        nodes.push(ImportedNode {
            name: node.name().map(str::to_owned),
            parent: None,
            children: node.children().map(|n| n.index()).collect(),
            transform: ImportedTransform {
                translation,
                rotation,
                scale,
            },
            matrix,
            mesh: node.mesh().map(|m| m.index()),
            skin: node.skin().map(|s| s.index()),
        });
    }
    for node in document.nodes() {
        for child in node.children() {
            if nodes[child.index()].parent.replace(node.index()).is_some() {
                return Err(invalid("node has multiple parents"));
            }
        }
    }
    let mut states = vec![0_u8; nodes.len()];
    for start in 0..nodes.len() {
        let mut current = Some(start);
        let mut chain = Vec::new();
        while let Some(index) = current {
            if states[index] == 2 {
                break;
            }
            if states[index] == 1 {
                return Err(invalid("node hierarchy cycle"));
            }
            states[index] = 1;
            chain.push(index);
            current = nodes[index].parent;
        }
        for index in chain {
            states[index] = 2;
        }
    }
    for scene in document.scenes() {
        for root in scene.nodes() {
            if nodes[root.index()].parent.is_some() {
                return Err(invalid("scene root has a parent"));
            }
        }
    }
    Ok(nodes)
}
fn import_images(
    document: &gltf::Document,
    buffers: &[Vec<u8>],
    context: &mut Context<'_>,
) -> Result<Vec<ImportedImage>> {
    let mut images = Vec::new();
    let mut decoded_total = 0;
    for image in document.images() {
        let bytes = match image.source() {
            gltf::image::Source::Uri { uri, .. } => context.uri(uri)?,
            gltf::image::Source::View { view, .. } => buffers[view.buffer().index()]
                [view.offset()..view.offset() + view.length()]
                .to_vec(),
        };
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(AssetError::Unsupported(
                "only PNG image decoding is enabled".into(),
            ));
        }
        let (width, height) =
            image::ImageReader::with_format(std::io::Cursor::new(&bytes), image::ImageFormat::Png)
                .into_dimensions()
                .map_err(|e| invalid(&e.to_string()))?;
        if width > context.limits.max_image_dimension || height > context.limits.max_image_dimension
        {
            return Err(AssetError::Limit("image dimensions"));
        }
        let output_bytes = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(4))
            .ok_or(AssetError::Limit("decoded image bytes"))?;
        // Check the RGBA conversion allocation before decoding any pixel data.
        if output_bytes
            > context
                .limits
                .max_decoded_image_bytes
                .saturating_sub(decoded_total)
        {
            return Err(AssetError::Limit("decoded image bytes"));
        }
        let mut reader =
            image::ImageReader::with_format(std::io::Cursor::new(bytes), image::ImageFormat::Png);
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(context.limits.max_image_dimension);
        limits.max_image_height = Some(context.limits.max_image_dimension);
        limits.max_alloc = Some(
            context
                .limits
                .max_decoded_image_bytes
                .saturating_sub(decoded_total) as u64,
        );
        reader.limits(limits);
        let decoded = reader
            .decode()
            .map_err(|e| invalid(&e.to_string()))?
            .into_rgba8();
        add_budget(
            &mut decoded_total,
            decoded.as_raw().len(),
            context.limits.max_decoded_image_bytes,
            "decoded image bytes",
        )?;
        images.push(ImportedImage {
            name: image.name().map(str::to_owned),
            width: decoded.width(),
            height: decoded.height(),
            rgba8: decoded.into_raw(),
        });
    }
    Ok(images)
}
fn import_material(material: gltf::Material<'_>) -> Result<ImportedMaterial> {
    let pbr = material.pbr_metallic_roughness();
    let reference = |info: gltf::texture::Info<'_>| TextureReference {
        texture: info.texture().index(),
        tex_coord: info.tex_coord(),
    };
    let imported = ImportedMaterial {
        name: material.name().map(str::to_owned),
        base_color: pbr.base_color_factor(),
        base_color_texture: pbr.base_color_texture().map(reference),
        metallic: pbr.metallic_factor(),
        roughness: pbr.roughness_factor(),
        metallic_roughness_texture: pbr.metallic_roughness_texture().map(reference),
        normal_texture: material.normal_texture().map(|v| TextureReference {
            texture: v.texture().index(),
            tex_coord: v.tex_coord(),
        }),
        normal_scale: material.normal_texture().map_or(1.0, |v| v.scale()),
        occlusion_texture: material.occlusion_texture().map(|v| TextureReference {
            texture: v.texture().index(),
            tex_coord: v.tex_coord(),
        }),
        occlusion_strength: material.occlusion_texture().map_or(1.0, |v| v.strength()),
        emissive: material.emissive_factor(),
        emissive_texture: material.emissive_texture().map(reference),
        alpha_mode: match material.alpha_mode() {
            gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
            gltf::material::AlphaMode::Mask => AlphaMode::Mask,
            gltf::material::AlphaMode::Blend => AlphaMode::Blend,
        },
        alpha_cutoff: material.alpha_cutoff().unwrap_or(0.5),
        double_sided: material.double_sided(),
    };
    finite(
        imported
            .base_color
            .into_iter()
            .chain(imported.emissive)
            .chain([
                imported.metallic,
                imported.roughness,
                imported.normal_scale,
                imported.occlusion_strength,
                imported.alpha_cutoff,
            ]),
    )?;
    Ok(imported)
}
