//! Drive the authored `ParticleEmitter` components into the pipeline's
//! once-per-scene-tick GPU particle simulation.
//!
//! Nothing here runs from `render_scene`: hosts call
//! [`GpuRenderer::advance_particles`](super::GpuRenderer::advance_particles)
//! right after each scene tick, so drawing a tick for several views, eyes or
//! generated frames never advances the simulation.

use anyhow::{Context, Result};
use engine_project::particles::{
    MAX_PARTICLE_DT, ParticleBlend3D, ParticleEmitterInstance, ParticleFrame,
};
use engine_render_pipeline::{
    ParticleBlend, ParticleEmitter, ParticleTick, ParticleTickReport, ScenePipeline,
};
use glam::{Mat4, Vec3, Vec4};

/// The native emitter of an extracted one: position from the world
/// transform, launch axis rotated into world space. The matrix already
/// carries the applied floating origin, like the extracted camera and meshes.
pub(super) fn native(instance: &ParticleEmitterInstance) -> ParticleEmitter {
    let authored = &instance.emitter;
    let world = Mat4::from_cols_array_2d(&instance.world);
    ParticleEmitter {
        id: instance.id.clone(),
        position: world.w_axis.truncate(),
        direction: world.transform_vector3(Vec3::from(authored.direction)),
        capacity: authored.capacity,
        spawn_rate: authored.spawn_rate,
        lifetime: authored.lifetime,
        speed: authored.speed,
        spread: authored.spread,
        spawn_extent: authored.spawn_extent,
        gravity: Vec3::from(authored.gravity),
        drag: authored.drag,
        size: authored.size,
        start_color: Vec4::from(authored.start_color),
        end_color: Vec4::from(authored.end_color),
        blend: match authored.blend {
            ParticleBlend3D::Alpha => ParticleBlend::Alpha,
            ParticleBlend3D::Additive => ParticleBlend::Additive,
        },
        seed: authored.seed,
    }
}

/// Simulate `frame`'s tick on `pipeline`. A frame before the first scene
/// tick (`dt == 0`) has nothing to simulate, and a repeated `frame.tick` is a
/// no-op inside the pipeline. Scene steps longer than the renderer accepts
/// are clamped rather than rejected, so a stalled frame cannot fail the run.
pub(super) fn advance(
    pipeline: &mut ScenePipeline,
    frame: &ParticleFrame,
) -> Result<ParticleTickReport> {
    if frame.dt.is_nan() || frame.dt <= 0.0 {
        return Ok(ParticleTickReport {
            tick: frame.tick,
            ..Default::default()
        });
    }
    let emitters: Vec<_> = frame.emitters.iter().map(native).collect();
    pipeline
        .advance_particles(
            ParticleTick {
                id: frame.tick,
                dt: frame.dt.min(MAX_PARTICLE_DT),
            },
            &emitters,
        )
        .context("advancing the particle simulation")
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine_project::particles::ParticleEmitter as Authored;

    fn instance(world: Mat4, emitter: Authored) -> ParticleEmitterInstance {
        ParticleEmitterInstance {
            id: "spark".to_owned(),
            emitter,
            world: world.to_cols_array_2d(),
        }
    }

    #[test]
    fn the_entity_transform_places_and_aims_the_emitter() {
        let world = Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0))
            * Mat4::from_rotation_z(-std::f32::consts::FRAC_PI_2);
        let authored = Authored {
            capacity: 32,
            seed: 7,
            blend: ParticleBlend3D::Additive,
            ..Default::default()
        };
        let native = native(&instance(world, authored));
        assert_eq!(native.id, "spark");
        assert_eq!(native.position, Vec3::new(1.0, 2.0, 3.0));
        // Local +Y rotated a quarter turn clockwise about Z points along +X.
        assert!((native.direction - Vec3::X).length() < 1e-5, "{native:?}");
        assert_eq!((native.capacity, native.seed), (32, 7));
        assert_eq!(native.blend, ParticleBlend::Additive);
        native.validate().unwrap();
    }

    #[test]
    fn a_degenerate_transform_is_rejected_by_native_validation() {
        let flat = Mat4::from_scale(Vec3::ZERO);
        let native = native(&instance(flat, Authored::default()));
        assert!(native.validate().is_err(), "zero direction");
    }
}
