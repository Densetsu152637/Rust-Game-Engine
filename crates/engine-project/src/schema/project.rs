use super::*;

impl Project {
    pub fn load(manifest: impl AsRef<Path>) -> Result<Self> {
        let manifest = fs::canonicalize(manifest)?;
        let root = manifest
            .parent()
            .ok_or_else(|| Error::new("project.path.invalid", "Manifest has no parent"))?
            .to_owned();
        let raw: Manifest = serde_json::from_slice(&read_bounded(&manifest, 16 * 1024 * 1024)?)?;
        if raw.schema != 1 {
            return Err(Error::new(
                "project.schema.unsupported",
                "Only project schema 1 is supported",
            ));
        }
        if !identifier(&raw.name) || raw.assets.len() > 65536 {
            return Err(Error::new(
                "project.invalid",
                "Invalid project name or asset capacity exceeded",
            ));
        }
        let mut assets = BTreeMap::new();
        let mut paths = BTreeSet::new();
        for asset in raw.assets {
            if !identifier(&asset.id) || assets.contains_key(&asset.id) {
                return Err(Error::new(
                    "project.asset.id.duplicate",
                    "Invalid or duplicate asset ID",
                ));
            }
            if ![
                "script",
                "mesh",
                "texture",
                "font",
                "font_face",
                "font_config",
                "string_table",
                "caption_track",
                "locale_config",
                "audio",
                "video",
                "scene",
                "shader",
                "prefab",
                "model",
                "animation",
                "navigation",
                "acoustics",
                "lut",
                "graph",
            ]
            .contains(&asset.kind.as_str())
            {
                return Err(Error::new("project.asset.kind", "Unsupported asset kind"));
            }
            let file = confined_file(&root, &asset.path)?;
            if !paths.insert(file.to_string_lossy().to_lowercase()) {
                return Err(Error::new(
                    "project.asset.path.duplicate",
                    "Asset paths must be unique",
                ));
            }
            if asset.kind == "script" && file.extension().and_then(|e| e.to_str()) != Some("rs") {
                return Err(Error::new(
                    "project.script.language",
                    "Native scripts must be Rust .rs sources",
                ));
            }
            assets.insert(asset.id.clone(), asset);
        }
        let scene = load_scene(&root, &raw.startup_scene)?;
        let mut bindings = raw.input_bindings;
        for (action, key) in &raw.input_actions {
            bindings.entry(action.clone()).or_insert_with(|| {
                vec![if key.contains(':') {
                    key.clone()
                } else {
                    format!("Key:{key}")
                }]
            });
        }
        let validation = validate_bindings(&bindings, false);
        if !validation.errors.is_empty() {
            return Err(Error::new(
                "project.input.invalid",
                validation.errors.join("; "),
            ));
        }
        let rust_manifest = raw
            .rust
            .as_ref()
            .map(|r| r.manifest.clone())
            .unwrap_or_else(|| PathBuf::from("Cargo.toml"));
        if raw.rust.is_some() {
            confined_file(&root, &rust_manifest)?;
        }
        let project = Self {
            component_catalog: if let Some(path) = &raw.component_schemas {
                crate::catalog::ComponentCatalog::new(serde_json::from_slice(&read_bounded(
                    &confined_file(&root, path)?,
                    1024 * 1024,
                )?)?)?
            } else {
                crate::catalog::ComponentCatalog::new(Vec::new())?
            },
            schema: 1,
            name: raw.name,
            root,
            manifest,
            startup_scene: raw.startup_scene,
            rust_manifest,
            assets,
            input_actions: raw.input_actions,
            input_bindings: bindings,
            scene,
        };
        project.validate()?;
        Ok(project)
    }
    pub fn rust_manifest(&self) -> Result<PathBuf> {
        confined_file(&self.root, &self.rust_manifest)
    }
    pub fn resolve_asset(&self, id: &str) -> Result<PathBuf> {
        let asset = self
            .assets
            .get(id)
            .ok_or_else(|| Error::new("project.asset.unknown", format!("Unknown asset {id}")))?;
        confined_file(&self.root, &asset.path)
    }
    pub fn require_asset(&self, id: &str, kind: &str) -> Result<&Asset> {
        let asset = self
            .assets
            .get(id)
            .ok_or_else(|| Error::new("project.asset.unknown", format!("Unknown asset {id}")))?;
        if asset.kind != kind {
            return Err(Error::new(
                "project.asset.kind",
                format!("Asset {id} must be {kind}"),
            ));
        }
        Ok(asset)
    }
    pub fn validate_scene(&self, scene: &Scene) -> Result<()> {
        scene.validate()?;
        for entity in &scene.entities {
            self.component_catalog.validate_entity(scene, entity)?;
            let c = &entity.components;
            for lut in c
                .post_process
                .iter()
                .flat_map(|p| p.asset_refs())
                .chain(c.post_process_volume.iter().flat_map(|v| v.asset_refs()))
            {
                self.require_asset(lut, "lut")?;
            }
            if let Some(material) = &c.material {
                if let Some(shader) = &material.shader {
                    if self
                        .assets
                        .get(shader)
                        .is_some_and(|asset| asset.kind == "graph")
                    {
                        let graph = crate::graphs::load_graph(self, shader)?;
                        if graph.kind != engine_material_graph::KIND {
                            return Err(Error::new(
                                "project.asset.kind",
                                format!("Asset {shader} must be shader or a material graph"),
                            ));
                        }
                    } else {
                        self.require_asset(shader, "shader")?;
                    }
                }
                if let Some(texture) = &material.texture {
                    match texture.strip_prefix(crate::rendering3d::STREAM_TEXTURE_PREFIX) {
                        // Streams are registered natively at runtime.
                        Some(name) if crate::rendering3d::valid_stream_name(name) => {}
                        Some(_) => {
                            return Err(Error::new(
                                "project.asset.stream",
                                format!("Invalid texture stream reference {texture}"),
                            ));
                        }
                        None => {
                            self.require_asset(texture, "texture")?;
                        }
                    }
                }
                for texture in [
                    &material.metallic_roughness_texture,
                    &material.normal_texture,
                ]
                .into_iter()
                .flatten()
                {
                    self.require_asset(texture, "texture")?;
                }
            }
            if let Some(s) = &c.script {
                self.require_asset(&s.asset, "script")?;
            }
            if let Some(m) = &c.mesh_renderer {
                self.require_asset(&m.mesh, "mesh")?;
                if let Some(t) = &m.texture {
                    self.require_asset(t, "texture")?;
                }
            }
            if let Some(s) = &c.sprite_renderer {
                self.require_asset(&s.texture, "texture")?;
            }
        }
        crate::acoustics::validate_scene(self, scene)?;
        crate::networking::validate_scene(scene)?;
        crate::ai::validate_scene(self, scene)?;
        Ok(())
    }
    /// Validate all declared scenes/content, not only the startup scene.
    pub fn validate(&self) -> Result<()> {
        self.validate_scene(&self.scene)?;
        for asset in self.assets.values() {
            let file = self.resolve_asset(&asset.id)?;
            match asset.kind.as_str() {
                "scene" | "prefab" => self.validate_scene(&load_scene(&self.root, &asset.path)?)?,
                "model" => {
                    crate::model_import::load_model(&file)?;
                }
                "animation" => {
                    crate::simulation::load_clip(self, &asset.id)?;
                }
                "navigation" => {
                    crate::simulation::load_navmesh(self, &asset.id)?;
                }
                "acoustics" => {
                    let bytes =
                        read_bounded(&file, crate::acoustics::MAX_BAKED_DOCUMENT_BYTES as u64)?;
                    crate::acoustics::validate_document(&asset.id, &bytes)?;
                }
                "graph" => crate::graphs::validate_asset(self, &asset.id)?,
                "mesh" => {
                    crate::content::load_mesh(&file)?;
                }
                "texture" => {
                    crate::content::load_texture(&file)?;
                }
                "font" => {
                    let font = crate::content::load_font(&file)?;
                    self.require_asset(&font.texture, "texture")?;
                    let texture =
                        crate::content::load_texture(&self.resolve_asset(&font.texture)?)?;
                    font.validate(texture.width, texture.height)?;
                }
                "font_face" => {
                    crate::content::load_font_face(&file)?;
                }
                "font_config" => {
                    let config = crate::content::load_font_config(&file)?;
                    let faces = self
                        .assets
                        .values()
                        .filter(|a| a.kind == "font_face")
                        .map(|a| a.id.clone())
                        .collect();
                    config.validate_against(&faces)?;
                }
                "string_table" => {
                    crate::localization::load_string_table(&file)?;
                }
                "caption_track" => {
                    crate::captions::load_caption_track(&file)?;
                }
                "script" => {
                    read_bounded(&file, 1024 * 1024)?;
                }
                _ => {}
            }
        }
        crate::localization::validate_project(self)
    }
}
