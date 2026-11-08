//! Retained mesh records: the dirty-copy rule of the scene extraction.
//!
//! The runtime keeps one render record per entity with a `MeshRenderer`, keyed by `Entity`: the
//! [`MeshDraw3D`] the entity contributes to a frame, its authored LOD/occluder options, its
//! cached world-space box and the flags the culling needs. An extraction does not rebuild the
//! records from the world; it
//!
//! 1. walks the mesh renderers (one typed query, which only compares change ticks and checks
//!    which components exist) and rebuilds the record of an entity only when something it
//!    renders from changed since the previous extraction: the tick of `MeshRenderer`,
//!    `GeometryOverride`, `MeshMaterial`, `MeshLightmap`, `MeshLods`, `HiZOccluder`,
//!    `AuthoredId`, `Transform` or `Parent`, the set of those components it has, or an
//!    ancestor's `Transform` or `Parent`;
//! 2. drops the records of entities it did not meet (despawned, or without a `MeshRenderer`
//!    any more);
//! 3. culls over the records, with their cached boxes, and clones only what is drawn into the
//!    frame.
//!
//! The change ticks are this module's own (`last_run`, the tick of the previous extraction), as
//! a system keeps its own, so a frame that was not extracted (an occluded window) loses
//! nothing: the next extraction sees every change since the last one that ran. An extraction
//! that fails leaves `last_run` where it was, so everything is looked at again. The records of
//! everything are rebuilt after a scene transition, a change of the 2D lighting or fragment
//! renderer (they decide which meshes the 3D pass draws), a shader reload, or a long gap in
//! ticks.
//!
//! What a record does not depend on is the camera, the lights and the other entities; what
//! depends on state outside the entity is "volatile" and rebuilt on every extraction: a mesh
//! with a streamed texture, a lightmap (checked against the camera's indirect asset) and a
//! generated tile chunk (drawn by 2D or 3D depending on its owner).
use super::cull::{Aabb, Culler, MeshBounds, Verdict};
use super::render3d::{check_lightmap, is_drawn_mesh};
use super::*;
use crate::rendering3d::*;
use bevy_ecs::change_detection::Tick;
use bevy_ecs::entity::EntityHashMap;
use bevy_ecs::world::WorldId;
use glam::Mat4;

/// Which of the components a record is built from the entity has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Mask(u16);
impl Mask {
    /// The bit of a generated tile chunk (the last of [`Mask::of`]'s flags).
    const TILE: u16 = 1 << 9;
    fn of(present: [bool; 10]) -> Self {
        Self(
            present
                .iter()
                .enumerate()
                .fold(0, |bits, (i, present)| bits | (u16::from(*present) << i)),
        )
    }
}

fn is_newer<T: Component>(component: &Option<Ref<T>>, newer: &impl Fn(Tick) -> bool) -> bool {
    component
        .as_ref()
        .is_some_and(|component| newer(component.last_changed()))
}

/// The row of the walk over the mesh renderers: the entity, its `MeshRenderer`, then the other
/// components a record is built from (with their change ticks) and whether it is fractured or a
/// generated tile chunk.
type RowData = (
    Entity,
    Ref<'static, MeshRenderer>,
    Option<Ref<'static, crate::GeometryOverride>>,
    Option<Ref<'static, MeshMaterial>>,
    Option<Ref<'static, MeshLightmap>>,
    Option<Ref<'static, MeshLods>>,
    Option<Ref<'static, HiZOccluder>>,
    Option<Ref<'static, AuthoredId>>,
    Option<Ref<'static, Transform>>,
    Option<Ref<'static, crate::Parent>>,
    Has<crate::simulation::physics::Fractured>,
    Has<crate::world::tilemap::TileChunkRender>,
);

/// Register the components of [`RowData`]: a typed query over a component that was never
/// registered matches nothing, so the walk needs all of them known, used or not.
pub(super) fn register_components(world: &mut World) {
    world.register_component::<MeshRenderer>();
    world.register_component::<crate::GeometryOverride>();
    world.register_component::<MeshMaterial>();
    world.register_component::<MeshLightmap>();
    world.register_component::<MeshLods>();
    world.register_component::<HiZOccluder>();
    world.register_component::<AuthoredId>();
    world.register_component::<Transform>();
    world.register_component::<crate::Parent>();
    world.register_component::<crate::simulation::physics::Fractured>();
    world.register_component::<crate::world::tilemap::TileChunkRender>();
}

/// What one mesh renderer contributes to a frame.
struct Record {
    /// The extraction that last met the entity.
    seen: u64,
    mask: Mask,
    /// Rebuilt on every extraction (see the module documentation).
    volatile: bool,
    /// The 3D pass draws it. Without, the record only remembers that it does not.
    draw: Option<MeshDraw3D>,
    options: Option<MeshDrawOptions3D>,
    /// World box for the culling; `None` when the mesh cannot be bounded or has a custom shader.
    bounds: Option<Aabb>,
    casts_shadows: bool,
}

/// What the last extraction changed in the retained records: the unit a render thread holding a
/// copy of them would be sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExtractionDelta {
    /// Entities whose record was rebuilt (new, changed or volatile).
    pub upserted: Vec<Entity>,
    /// Entities whose record was dropped.
    pub removed: Vec<Entity>,
    /// Every record was rebuilt: a holder of the previous records starts over.
    pub reset: bool,
}

#[derive(Default)]
pub(super) struct Retained {
    records: EntityHashMap<Record>,
    /// Tick of the previous successful extraction.
    last_run: Option<Tick>,
    world: Option<WorldId>,
    scene_revision: u64,
    lit_2d: bool,
    fragments_drawn: bool,
    shader_epoch: u64,
    pass: u64,
    delta: ExtractionDelta,
}

/// What a pass needs besides the world.
pub(super) struct Pass<'a> {
    pub culler: Option<&'a Culler>,
    pub lit_2d: bool,
    pub fragments_drawn: bool,
    pub indirect: Option<&'a crate::indirect::IndirectAsset>,
    /// Rebuild every record (a from-scratch extraction).
    pub scratch: bool,
}

impl Runtime {
    /// What the last scene extraction rebuilt and dropped in the retained mesh records.
    pub fn extraction_delta(&self) -> ExtractionDelta {
        self.retained.borrow().delta.clone()
    }
    /// [`Self::extract_scene_frame`] with every retained record rebuilt first. The frame equals
    /// the one `extract_scene_frame` gives; this is the reference it is checked against.
    pub fn extract_scene_frame_from_scratch(&self) -> Result<SceneRenderFrame> {
        self.scratch.set(true);
        let frame = self.extract_scene_frame();
        self.scratch.set(false);
        frame
    }

    /// Bring the retained records up to date and add the drawn ones to `frame`.
    pub(super) fn collect_meshes(
        &self,
        frame: &mut SceneRenderFrame,
        pass: &Pass,
    ) -> Result<ExtractionStats> {
        let world = self.app.world();
        let mut retained = self.retained.borrow_mut();
        let retained = &mut *retained;
        let mut bounds = self.mesh_bounds.borrow_mut();
        // A fresh tick, so that a change made after this extraction is newer than `this_run`.
        let this_run = world
            .as_unsafe_world_cell_readonly()
            .increment_change_tick();
        let shader_epoch = self.shader_epoch.get();
        let full = pass.scratch
            || retained.world != Some(world.id())
            || retained.scene_revision != self.scene_revision
            || retained.lit_2d != pass.lit_2d
            || retained.fragments_drawn != pass.fragments_drawn
            || retained.shader_epoch != shader_epoch
            || retained
                .last_run
                .is_none_or(|last| this_run.get().wrapping_sub(last.get()) > u32::MAX / 4);
        let last_run = retained.last_run.unwrap_or(Tick::new(0));
        let newer = |tick: Tick| tick.is_newer_than(last_run, this_run);
        let lookup_changed = world
            .get_resource_ref::<EntityLookup>()
            .is_some_and(|lookup| newer(lookup.last_changed()));
        if full {
            retained.records.clear();
        }
        retained.pass += 1;
        let epoch = retained.pass;
        let mut delta = ExtractionDelta {
            reset: full,
            ..ExtractionDelta::default()
        };
        let mut stats = ExtractionStats {
            culled: pass.culler.is_some(),
            ..ExtractionStats::default()
        };
        if let Some(mut query) = world.try_query_filtered::<RowData, ()>() {
            for row in query.iter(world) {
                stats.meshes += 1;
                let (
                    entity,
                    renderer,
                    geometry,
                    material,
                    lightmap,
                    lods,
                    occluder,
                    id,
                    transform,
                    parent,
                    fractured,
                    tile,
                ) = row;
                let mask = Mask::of([
                    transform.is_some(),
                    parent.is_some(),
                    material.is_some(),
                    geometry.is_some(),
                    lightmap.is_some(),
                    lods.is_some(),
                    occluder.is_some(),
                    id.is_some(),
                    fractured,
                    tile,
                ]);
                let changed = match retained.records.get(&entity) {
                    None => true,
                    Some(record) => {
                        record.volatile
                            || record.mask != mask
                            || newer(renderer.last_changed())
                            || is_newer(&geometry, &newer)
                            || is_newer(&material, &newer)
                            || is_newer(&lightmap, &newer)
                            || is_newer(&lods, &newer)
                            || is_newer(&occluder, &newer)
                            || is_newer(&id, &newer)
                            || is_newer(&transform, &newer)
                            || is_newer(&parent, &newer)
                            || parent.as_ref().is_some_and(|parent| {
                                lookup_changed || ancestors_changed(world, &parent.0, &newer)
                            })
                    }
                };
                if changed {
                    stats.changed += 1;
                }
                if changed || full {
                    let mut record = self.build_record(
                        world,
                        entity,
                        mask,
                        pass,
                        &mut bounds,
                        &mut frame.streams,
                    )?;
                    record.seen = epoch;
                    retained.records.insert(entity, record);
                    stats.copied += 1;
                    delta.upserted.push(entity);
                }
                let record = retained
                    .records
                    .get_mut(&entity)
                    .expect("the record was just built or kept");
                record.seen = epoch;
                let Some(draw) = &record.draw else {
                    stats.not_3d += 1;
                    continue;
                };
                if let (Some(culler), Some(world_box)) = (pass.culler, &record.bounds) {
                    match culler.decide(world_box, record.casts_shadows) {
                        Verdict::Visible => {}
                        Verdict::CastsIntoView => stats.kept_for_shadow += 1,
                        Verdict::Outside => {
                            stats.outside_frustum += 1;
                            continue;
                        }
                    }
                }
                frame.meshes.push(draw.clone());
                if let Some(options) = &record.options {
                    frame.mesh_options.insert(draw.id.clone(), options.clone());
                }
            }
        }
        retained.records.retain(|entity, record| {
            let keep = record.seen == epoch;
            if !keep {
                delta.removed.push(*entity);
            }
            keep
        });
        stats.drawn = frame.meshes.len();
        stats.retained = retained.records.len();
        retained.last_run = Some(this_run);
        retained.world = Some(world.id());
        retained.scene_revision = self.scene_revision;
        retained.lit_2d = pass.lit_2d;
        retained.fragments_drawn = pass.fragments_drawn;
        retained.shader_epoch = shader_epoch;
        retained.delta = delta;
        bounds.sweep();
        Ok(stats)
    }

    /// The record of `entity`, read from the world.
    fn build_record(
        &self,
        world: &World,
        entity: Entity,
        mask: Mask,
        pass: &Pass,
        bounds: &mut MeshBounds,
        streams: &mut Vec<StreamFrame>,
    ) -> Result<Record> {
        let entity = world.entity(entity);
        let mut record = Record {
            seen: 0,
            mask,
            volatile: mask.0 & Mask::TILE != 0,
            draw: None,
            options: None,
            bounds: None,
            casts_shadows: true,
        };
        if !is_drawn_mesh(world, &entity, pass.lit_2d, pass.fragments_drawn) {
            return Ok(record);
        }
        let renderer = entity
            .get::<MeshRenderer>()
            .expect("drawn meshes have a MeshRenderer");
        let mesh = entity
            .get::<crate::GeometryOverride>()
            .map(|geometry| &geometry.0)
            .or_else(|| self.content.meshes.get(&renderer.mesh))
            .ok_or_else(|| Error::new("render.mesh.unknown", &renderer.mesh))?
            .clone();
        let model = crate::world_transform(world, entity.id())?.matrix;
        let id = entity
            .get::<AuthoredId>()
            .map(|id| id.id.clone())
            .unwrap_or_else(|| format!("entity:{}", entity.id().to_bits()));
        let material = entity.get::<MeshMaterial>().cloned().unwrap_or_default();
        material.validate()?;
        // A custom shader may move its vertices outside the mesh's box: no culling box.
        if material.shader.is_none()
            && let Some(local) = self.local_bounds(bounds, &entity, &mesh)
        {
            record.bounds = Some(local.transformed(&Mat4::from_cols_array_2d(&model)));
        }
        record.casts_shadows = material.cast_shadows;
        let texture_id = material.texture.as_ref().or(renderer.texture.as_ref());
        record.volatile |= texture_id.is_some_and(|id| id.starts_with(STREAM_TEXTURE_PREFIX));
        let texture = texture_id
            .map(|id| self.resolve_texture(id, streams))
            .transpose()?;
        let shader = self.material_shader(&material)?;
        let lightmap = entity
            .get::<MeshLightmap>()
            .map(|binding| check_lightmap(&id, binding, &mesh, shader.is_some(), pass.indirect))
            .transpose()?
            .cloned();
        record.volatile |= lightmap.is_some();
        let resolve = |id: &Option<String>| -> Result<Option<Arc<Texture>>> {
            id.as_ref()
                .map(|id| {
                    self.content
                        .textures
                        .get(id)
                        .cloned()
                        .ok_or_else(|| Error::new("render.texture.unknown", id))
                })
                .transpose()
        };
        record.draw = Some(MeshDraw3D {
            id: id.clone(),
            mesh,
            texture,
            metallic_roughness_texture: resolve(&material.metallic_roughness_texture)?,
            normal_texture: resolve(&material.normal_texture)?,
            model,
            material,
            shader,
            lightmap: lightmap.clone(),
        });
        // A runtime geometry override (skinning, World-generated meshes) replaces level zero,
        // so authored static levels and the occluder marker do not apply to it.
        let overridden = entity.get::<crate::GeometryOverride>().is_some();
        let authored_lods = entity.get::<MeshLods>().filter(|_| !overridden);
        let marked = entity
            .get::<HiZOccluder>()
            .is_some_and(|marker| marker.enabled)
            && !overridden;
        if authored_lods.is_some() || marked {
            let levels = authored_lods
                .map(|lods| {
                    lods.meshes
                        .iter()
                        .map(|id| {
                            self.content
                                .meshes
                                .get(id)
                                .cloned()
                                .ok_or_else(|| Error::new("render.mesh.unknown", id))
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();
            if lightmap.is_some()
                && let Some(level) = levels.iter().position(|level| level.uv2.is_empty())
            {
                return Err(Error::new(
                    "render.lightmap.uv2",
                    format!(
                        "{id}: MeshLods level {} has no second UV set; a lightmapped mesh needs UV2 on every level",
                        level + 1
                    ),
                ));
            }
            record.options = Some(MeshDrawOptions3D {
                levels,
                policy: authored_lods.map_or_else(
                    || engine_mesh_lod::ScreenSizeLods {
                        thresholds: vec![1.0],
                        cull_below: None,
                        hysteresis: 0.0,
                    },
                    MeshLods::policy,
                ),
                occluder: marked,
            });
        }
        Ok(record)
    }
}

/// Whether the `Transform` or `Parent` of any ancestor, starting at the parent named `name`,
/// changed (or the chain is broken, which resolving the child reports).
fn ancestors_changed(world: &World, name: &str, newer: &impl Fn(Tick) -> bool) -> bool {
    let mut name = name.to_owned();
    for _ in 0..257 {
        let Some(parent) = crate::services::find_entity(world, &name) else {
            return true;
        };
        let Ok(parent) = world.get_entity(parent) else {
            return true;
        };
        let Some(transform) = parent.get_ref::<Transform>() else {
            return true;
        };
        if newer(transform.last_changed()) {
            return true;
        }
        match parent.get_ref::<crate::Parent>() {
            None => return false,
            Some(next) => {
                if newer(next.last_changed()) {
                    return true;
                }
                name = next.0.clone();
            }
        }
    }
    true
}
