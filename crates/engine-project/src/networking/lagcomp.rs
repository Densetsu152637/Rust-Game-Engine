//! Server-side lag compensation: `engine.NetworkHitboxes` poses are recorded every tick into
//! an `engine_net_predict::ColliderHistory` and queries rewind against it.
use super::{
    NetworkSession, components::NetworkHitboxes, delta::Side, error, settings::NetworkSettings,
};
use crate::{Error, Result, runtime::AuthoredId};
use bevy_ecs::prelude::*;
use engine_net_predict::{
    ColliderHistory, ColliderHistoryConfig, HitEntity, Hitbox, LagCompensator, Pose, QueryFilter,
    Ray, RewindError,
};
use engine_network::PeerId;
use glam::{Mat4, Quat, Vec3};
use serde::Serialize;
use std::collections::BTreeMap;

/// Nonsecret lag-compensation counters (part of the network status report).
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LagCompStatus {
    pub entities: usize,
    pub ticks_recorded: usize,
    pub oldest_tick: Option<u64>,
    pub newest_tick: Option<u64>,
    pub max_rewind_ticks: f64,
    pub queries: u64,
    pub clamped_queries: u64,
    pub rejected_queries: u64,
    pub record_errors: u64,
}

pub(crate) struct LagState {
    history: ColliderHistory,
    compensator: LagCompensator,
    ids: BTreeMap<String, HitEntity>,
    names: BTreeMap<HitEntity, String>,
    cached: BTreeMap<String, (u32, Vec<Hitbox>)>,
    next: HitEntity,
    pub(crate) status: LagCompStatus,
}

impl LagState {
    pub(crate) fn new(settings: &NetworkSettings) -> Result<Self> {
        let history = ColliderHistory::new(ColliderHistoryConfig {
            max_ticks: settings.history_ticks.max(2),
            max_entities: settings.max_entities,
            ..ColliderHistoryConfig::default()
        })
        .map_err(predict)?;
        let compensator = LagCompensator::new(settings.lag_rewind_ticks).map_err(predict)?;
        Ok(Self {
            history,
            compensator,
            ids: BTreeMap::new(),
            names: BTreeMap::new(),
            cached: BTreeMap::new(),
            next: 1,
            status: LagCompStatus {
                max_rewind_ticks: compensator.max_rewind_ticks(),
                ..LagCompStatus::default()
            },
        })
    }

    /// Records the poses of every `engine.NetworkHitboxes` entity for `tick`.
    pub(crate) fn record(&mut self, world: &World, tick: u64) {
        let mut poses = Vec::new();
        let mut seen = Vec::new();
        for entity in world.iter_entities() {
            let (Some(hitboxes), Some(id)) =
                (entity.get::<NetworkHitboxes>(), entity.get::<AuthoredId>())
            else {
                continue;
            };
            let Ok(matrix) = crate::world_transform(world, entity.id()) else {
                self.status.record_errors += 1;
                continue;
            };
            let (_, rotation, translation) =
                Mat4::from_cols_array_2d(&matrix.matrix).to_scale_rotation_translation();
            let boxes = hitboxes.to_hitboxes();
            let hit_id = *self.ids.entry(id.id.clone()).or_insert_with(|| {
                let id = self.next;
                self.next += 1;
                id
            });
            self.names.insert(hit_id, id.id.clone());
            if self.cached.get(&id.id) != Some(&(hitboxes.layers, boxes.clone())) {
                if self
                    .history
                    .set_hitboxes(hit_id, hitboxes.layers, boxes.clone())
                    .is_err()
                {
                    self.status.record_errors += 1;
                    continue;
                }
                self.cached.insert(id.id.clone(), (hitboxes.layers, boxes));
            }
            seen.push(id.id.clone());
            poses.push((hit_id, Pose::new(translation, rotation.normalize())));
        }
        let gone: Vec<String> = self
            .cached
            .keys()
            .filter(|id| !seen.contains(id))
            .cloned()
            .collect();
        for id in gone {
            self.cached.remove(&id);
            if let Some(hit) = self.ids.remove(&id) {
                self.history.remove_entity(hit);
                self.names.remove(&hit);
            }
        }
        if self.history.record(tick, poses).is_err() {
            self.status.record_errors += 1;
        }
        self.status.entities = self.history.entity_count();
        self.status.ticks_recorded = self.history.len();
        self.status.oldest_tick = self.history.oldest_tick();
        self.status.newest_tick = self.history.newest_tick();
    }
}

fn predict(error: engine_net_predict::PredictError) -> Error {
    Error::new("network.predict", error.to_string())
}
fn rewind(error: RewindError) -> Error {
    Error::new(
        "network.lagcomp.rejected",
        match error {
            RewindError::NonFinite => "claimed view tick is not finite",
            RewindError::FutureTick => "claimed view tick is in the server's future",
            RewindError::NoHistory => "no hitbox history is recorded yet",
        },
    )
}

/// A lag-compensated ray query claimed by `shooter`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LagRaycast {
    /// Peer whose own entities (as granted by `assign_owner`) are ignored.
    pub shooter: PeerId,
    /// Server tick the shooter claims to have seen (fractional ticks interpolate). Untrusted:
    /// it is clamped to `lag_rewind_ticks` and to the recorded history.
    pub view_tick: f64,
    pub ray: Ray,
    /// Entities are tested when their `NetworkHitboxes.layers` intersect this mask.
    pub layer_mask: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LagHit {
    pub entity_id: String,
    pub part: u16,
    pub distance: f32,
    pub point: [f32; 3],
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LagRaycastResult {
    /// Tick the query actually used.
    pub rewind_tick: f64,
    /// The claim was older than allowed and was clamped.
    pub clamped: bool,
    pub hit: Option<LagHit>,
}

fn with_lag<T>(
    world: &mut World,
    run: impl FnOnce(&mut LagState, &[HitEntity], u64) -> std::result::Result<T, RewindError>,
    shooter: PeerId,
) -> Result<T> {
    let mut session = world
        .get_resource_mut::<NetworkSession>()
        .ok_or_else(|| error("No network session"))?;
    let Some(delta) = session.delta.as_mut() else {
        return Err(error(
            "Lag compensation requires a delta-mode server session",
        ));
    };
    let Side::Server(server) = &mut delta.side else {
        return Err(error(
            "Lag compensation requires a delta-mode server session",
        ));
    };
    let current = server.last_tick;
    let owned: Vec<HitEntity> = server
        .owners
        .iter()
        .filter(|(_, peer)| **peer == shooter)
        .filter_map(|(id, _)| server.lag.ids.get(id).copied())
        .collect();
    server.lag.status.queries += 1;
    run(&mut server.lag, &owned, current).map_err(|e| {
        server.lag.status.rejected_queries += 1;
        rewind(e)
    })
}

/// Rewound ray cast. Returns the first hit among `engine.NetworkHitboxes` entities at the pose
/// they had at `view_tick`, ignoring the shooter's own entities.
pub fn lag_compensated_raycast(world: &mut World, query: &LagRaycast) -> Result<LagRaycastResult> {
    let query = *query;
    with_lag(
        world,
        |lag, exclude, current| {
            let filter = QueryFilter {
                layer_mask: query.layer_mask,
                exclude,
            };
            let (rewound, hit) = lag.compensator.raycast(
                &lag.history,
                query.view_tick,
                current,
                query.ray,
                &filter,
            )?;
            if rewound.clamped {
                lag.status.clamped_queries += 1;
            }
            Ok(LagRaycastResult {
                rewind_tick: rewound.tick,
                clamped: rewound.clamped,
                hit: hit.and_then(|h| {
                    Some(LagHit {
                        entity_id: lag.names.get(&h.entity)?.clone(),
                        part: h.part,
                        distance: h.distance,
                        point: h.point.to_array(),
                    })
                }),
            })
        },
        query.shooter,
    )
}

/// Rewound sphere overlap: authored IDs and parts of every hitbox touching the sphere.
pub fn lag_compensated_overlap_sphere(
    world: &mut World,
    shooter: PeerId,
    view_tick: f64,
    center: [f32; 3],
    radius: f32,
    layer_mask: u32,
) -> Result<(LagRaycastResult, Vec<(String, u16)>)> {
    with_lag(
        world,
        |lag, exclude, current| {
            let filter = QueryFilter {
                layer_mask,
                exclude,
            };
            let (rewound, hits) = lag.compensator.overlap_sphere(
                &lag.history,
                view_tick,
                current,
                Vec3::from_array(center),
                radius,
                &filter,
            )?;
            if rewound.clamped {
                lag.status.clamped_queries += 1;
            }
            let hits = hits
                .into_iter()
                .filter_map(|h| Some((lag.names.get(&h.entity)?.clone(), h.part)))
                .collect();
            Ok((
                LagRaycastResult {
                    rewind_tick: rewound.tick,
                    clamped: rewound.clamped,
                    hit: None,
                },
                hits,
            ))
        },
        shooter,
    )
}

/// Pose of one hitbox entity at a (fractional) recorded tick.
pub fn hitbox_pose_at(world: &World, entity_id: &str, tick: f64) -> Option<([f32; 3], [f32; 4])> {
    let session = world.get_resource::<NetworkSession>()?;
    let Side::Server(server) = &session.delta.as_ref()?.side else {
        return None;
    };
    let hit = *server.lag.ids.get(entity_id)?;
    let pose = server.lag.history.pose_at(hit, tick)?;
    let rotation: Quat = pose.rotation;
    Some((pose.translation.to_array(), rotation.to_array()))
}
