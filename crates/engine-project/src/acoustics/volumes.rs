//! `reverb_source: "volumes"`: analytic late reverb from `room` volumes.
//!
//! The room containing the listener supplies Eyring RT60 per band. The late level
//! is the diffuse field energy relative to the direct sound at 1 m (the
//! normalisation of the traced energy model): `1 / d_c^2` with the critical
//! distance `d_c^2 = 0.161 V / (16 pi T)`. Rooms are treated as axis-aligned
//! boxes (rotation ignored), which the user guide states.
use super::collect::{Collected, VolumeGeometry};
use super::components::VolumeKind;
use engine_acoustics::BandGains;
use engine_audio::acoustic::{AcousticFrame, ReverbParams};

#[derive(Clone, Debug)]
pub struct Room {
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub reverb: ReverbParams,
}
/// Shared with the worker-side parameter hook.
#[derive(Debug, Default)]
pub struct RoomSet {
    pub rooms: Vec<Room>,
    pub listener: [f32; 3],
    /// Overwrite the frame's reverb (only when the source is `volumes`).
    pub active: bool,
}
impl RoomSet {
    /// Replace the traced reverb by the room around the listener, or switch the
    /// late field off when the listener stands in no room.
    pub fn apply(&self, frame: &mut AcousticFrame) {
        if !self.active {
            return;
        }
        let l = self.listener;
        frame.reverb = self
            .rooms
            .iter()
            .find(|r| (0..3).all(|a| r.min[a] <= l[a] && l[a] <= r.max[a]))
            .map_or_else(ReverbParams::default, |r| r.reverb);
    }
}

fn axis_scale(t: &[f32; 12], column: usize) -> f32 {
    (t[column].powi(2) + t[4 + column].powi(2) + t[8 + column].powi(2)).sqrt()
}

/// Rooms of the collected world. Rooms whose material or RT60 cannot be resolved
/// are skipped (the estimate functions report `acoustics.*` errors).
pub fn rooms(collected: &Collected, air_np_per_m: BandGains) -> Vec<Room> {
    let mut rooms = vec![];
    for v in &collected.volumes {
        let (VolumeKind::Room, VolumeGeometry::Box { half_extents }) = (v.kind, &v.geometry) else {
            continue;
        };
        let Ok(material) = collected.materials[v.material].resolve() else {
            continue;
        };
        let size = [
            2.0 * half_extents[0] * axis_scale(&v.transform, 0),
            2.0 * half_extents[1] * axis_scale(&v.transform, 1),
            2.0 * half_extents[2] * axis_scale(&v.transform, 2),
        ];
        let centre = [v.transform[3], v.transform[7], v.transform[11]];
        let volume = size[0] * size[1] * size[2];
        let areas = [
            size[1] * size[2],
            size[1] * size[2],
            size[0] * size[2],
            size[0] * size[2],
            size[0] * size[1],
            size[0] * size[1],
        ];
        // Open faces behave as perfect absorbers (sound leaves the room).
        let surfaces: Vec<_> = areas
            .iter()
            .enumerate()
            .map(|(face, area)| {
                let open = v.open_mask >> face & 1 == 1;
                (*area, if open { [1.0; 4] } else { material.absorption })
            })
            .collect();
        let Ok(rt60) = engine_acoustics::eyring_rt60(volume, &surfaces, air_np_per_m) else {
            continue;
        };
        let total_area: f32 = areas.iter().sum();
        let mut level = [0.0; 4];
        for (l, t) in level.iter_mut().zip(rt60) {
            *l = 16.0 * std::f32::consts::PI * t / (0.161 * volume);
        }
        rooms.push(Room {
            min: [0, 1, 2].map(|a| centre[a] - size[a] * 0.5),
            max: [0, 1, 2].map(|a| centre[a] + size[a] * 0.5),
            reverb: ReverbParams {
                enabled: true,
                rt60_s: rt60,
                level,
                predelay_s: 0.0,
                mean_free_path_m: 4.0 * volume / total_area,
            },
        });
    }
    rooms
}
