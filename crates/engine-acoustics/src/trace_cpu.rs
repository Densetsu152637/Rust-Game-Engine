//! The stochastic energy tracer of section 5.4.4 on the CPU. This is the reference
//! energy model the GPU tiers reproduce:
//!
//! - Ray `r` of a job uses the stream `Pcg::for_ray(job.seed, r)` (see [`crate::rng`]).
//!   It leaves `job.emitter` in `uniform_sphere(u1, u2)` with relative energy 1 per
//!   band; every deposit is scaled by `1 / job.rays`.
//! - Each segment is cast with [`AcousticWorld`]'s nearest-hit query (all instance
//!   masks), `t` in `(0, inf)`.
//! - Receiver: a sphere of radius `job.receiver_radius_m` at `job.receiver`. A
//!   segment that crosses it after at least one reflection deposits, per band,
//!   `e_b * exp(-2 m_b * t_mid) * 4 pi * l / V_rcv / rays` into bin
//!   `floor(((path + t_in + l/2) / c) / bin_s)` (dropped when `>= bins`), where `l` is
//!   the chord inside the sphere, `t_in` the entry distance along the segment,
//!   `t_mid = t_in + l/2`, `path` the length travelled before the segment, and
//!   `V_rcv = 4/3 pi R^3`. This normalises energy to the direct sound at 1 m.
//!   The emission segment (zero bounces) never deposits: the mixer renders it.
//! - A segment without a hit adds `mean_b(e_b) / rays` to `escaped` and ends the ray.
//! - A hit at distance `t`: `e_b *= exp(-2 m_b t)` (air; `m_b` is in nepers per
//!   metre of amplitude, so energy decays at twice the rate), `path += t`, then
//!   `e_b *= reflect_b` (`1 - absorption`). Segments from a reflection to a hit
//!   feed the mean free path. The ray ends at this hit when it has already
//!   reflected `max_bounces` times or `max_b e_b < 1e-6` (-60 dB relative to its
//!   initial energy); so `max_bounces` is the number of reflections and each
//!   reflected segment is traced (and may deposit or escape).
//!   Otherwise draw `u_scatter, u1, u2`; with `u_scatter < scattering` the new
//!   direction is `cosine_hemisphere(n, u1, u2)` about the normal facing the
//!   incoming ray, else the specular reflection `d - 2 (d.n) n`. The new origin is
//!   the hit point plus `n * ray_offset(hit)`.
//! - Rays are processed in fixed chunks of [`CHUNK_RAYS`]; each chunk accumulates
//!   in `f64` and chunks are summed in index order, so the result is bitwise
//!   independent of the thread count.

use crate::math::{self, Vec3};
use crate::rng::{Pcg, cosine_hemisphere, uniform_sphere};
use crate::world::ray_offset;
use crate::{
    AcousticWorld, BANDS, BackendError, EnergyHistogram, EnergyJob, TraceSettings, TraceStats,
};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

/// Rays per deterministic reduction chunk.
pub const CHUNK_RAYS: u32 = 1024;
/// A ray ends once its largest band energy falls below this fraction of its
/// initial energy (-60 dB).
pub const MAX_RELATIVE_FLOOR: f32 = 1e-6;
/// Largest ray count per CPU job (section 5.3).
pub const CPU_MAX_RAYS: u32 = 65_536;
/// Largest reflection count per ray (section 5.3).
pub const MAX_BOUNCES: u16 = 256;
/// Histogram bin width range in seconds (section 5.3).
pub const BIN_RANGE_S: (f32, f32) = (0.0005, 0.010);
/// Longest histogram in seconds (section 5.3).
pub const MAX_HISTOGRAM_S: f32 = 4.0;

pub(crate) fn request_error(message: impl Into<String>) -> BackendError {
    BackendError {
        code: "acoustics.request.invalid",
        message: message.into(),
        fatal: false,
    }
}

/// Checks trace settings against section 5.3.
pub fn validate_trace_settings(s: &TraceSettings) -> Result<(), BackendError> {
    if !(s.bin_s.is_finite()
        && s.bin_s >= BIN_RANGE_S.0 * 0.999
        && s.bin_s <= BIN_RANGE_S.1 * 1.001)
    {
        return Err(request_error("histogram bin must be 0.5-10 ms"));
    }
    if s.bins == 0 || s.bins as f32 * s.bin_s > MAX_HISTOGRAM_S * 1.0001 {
        return Err(request_error("histogram must have bins and be at most 4 s"));
    }
    if !(s.speed_of_sound.is_finite() && (300.0..=400.0).contains(&s.speed_of_sound)) {
        return Err(request_error("speed of sound must be 300-400 m/s"));
    }
    if !s.air_np_per_m.iter().all(|m| m.is_finite() && *m >= 0.0) {
        return Err(request_error("air absorption must be finite and >= 0"));
    }
    Ok(())
}

/// Checks one job; `max_rays` is the backend's per-job ray limit.
pub fn validate_job(job: &EnergyJob, max_rays: u32) -> Result<(), BackendError> {
    if !math::finite3(job.emitter) || !math::finite3(job.receiver) {
        return Err(request_error("job positions must be finite"));
    }
    if !(job.receiver_radius_m.is_finite() && job.receiver_radius_m > 0.0) {
        return Err(request_error("receiver radius must be finite and positive"));
    }
    if job.rays == 0 || job.rays > max_rays {
        return Err(request_error(format!("rays per job must be 1-{max_rays}")));
    }
    if job.max_bounces > MAX_BOUNCES {
        return Err(request_error("max_bounces must be at most 256"));
    }
    Ok(())
}

#[derive(Clone)]
struct Chunk {
    bins: Vec<[f64; BANDS]>,
    escaped: f64,
    free_path_sum: f64,
    free_paths: u64,
    segments: u64,
}

/// Traces `jobs` with `threads` worker threads (1 runs inline). Fills `out[i]` for
/// `jobs[i]`.
pub fn trace_jobs(
    world: &AcousticWorld,
    settings: &TraceSettings,
    jobs: &[EnergyJob],
    threads: usize,
    out: &mut Vec<EnergyHistogram>,
) -> Result<TraceStats, BackendError> {
    let start = Instant::now();
    validate_trace_settings(settings)?;
    for job in jobs {
        validate_job(job, CPU_MAX_RAYS)?;
    }
    out.clear();
    let mut stats = TraceStats::default();
    for job in jobs {
        let (h, segments) = trace_job(world, settings, job, threads);
        stats.rays += u64::from(job.rays);
        stats.segments += segments;
        out.push(h);
    }
    stats.elapsed_us = start.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;
    Ok(stats)
}

fn trace_job(
    world: &AcousticWorld,
    settings: &TraceSettings,
    job: &EnergyJob,
    threads: usize,
) -> (EnergyHistogram, u64) {
    let chunks = job.rays.div_ceil(CHUNK_RAYS) as usize;
    let next = AtomicUsize::new(0);
    let run = || {
        let mut done = Vec::new();
        loop {
            let c = next.fetch_add(1, Ordering::Relaxed);
            if c >= chunks {
                break;
            }
            done.push((c, trace_chunk(world, settings, job, c as u32)));
        }
        done
    };
    let workers = threads.clamp(1, chunks.max(1));
    let mut results: Vec<(usize, Chunk)> = if workers == 1 {
        run()
    } else {
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..workers).map(|_| scope.spawn(run)).collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        })
    };
    results.sort_by_key(|(c, _)| *c);
    let mut total = Chunk {
        bins: vec![[0.0; BANDS]; settings.bins as usize],
        escaped: 0.0,
        free_path_sum: 0.0,
        free_paths: 0,
        segments: 0,
    };
    for (_, c) in &results {
        for (t, v) in total.bins.iter_mut().zip(&c.bins) {
            for b in 0..BANDS {
                t[b] += v[b];
            }
        }
        total.escaped += c.escaped;
        total.free_path_sum += c.free_path_sum;
        total.free_paths += c.free_paths;
        total.segments += c.segments;
    }
    (
        EnergyHistogram {
            bin_s: settings.bin_s,
            bins: total.bins.iter().map(|b| b.map(|v| v as f32)).collect(),
            rays: job.rays,
            escaped: total.escaped as f32,
            aborted: 0,
            mean_free_path_m: if total.free_paths > 0 {
                (total.free_path_sum / total.free_paths as f64) as f32
            } else {
                0.0
            },
        },
        total.segments,
    )
}

fn trace_chunk(world: &AcousticWorld, s: &TraceSettings, job: &EnergyJob, chunk: u32) -> Chunk {
    let mut out = Chunk {
        bins: vec![[0.0; BANDS]; s.bins as usize],
        escaped: 0.0,
        free_path_sum: 0.0,
        free_paths: 0,
        segments: 0,
    };
    let inv_rays = 1.0 / f64::from(job.rays);
    let r = job.receiver_radius_m;
    let v_rcv = 4.0 / 3.0 * std::f64::consts::PI * f64::from(r).powi(3);
    let deposit_scale = 4.0 * std::f64::consts::PI / v_rcv * inv_rays;
    let air2 = s.air_np_per_m.map(|m| 2.0 * m);
    let first = chunk * CHUNK_RAYS;
    let last = (first + CHUNK_RAYS).min(job.rays);
    for ray in first..last {
        let mut rng = Pcg::for_ray(job.seed, ray);
        let (u1, u2) = (rng.next_f32(), rng.next_f32());
        let mut dir = uniform_sphere(u1, u2);
        let mut origin = job.emitter;
        let mut e = [1.0f32; BANDS];
        let mut path = 0.0f32;
        let mut bounces = 0u16;
        loop {
            let hit = world.nearest(origin, dir, 0.0, f32::INFINITY, u8::MAX);
            out.segments += 1;
            let seg = hit.map_or(f32::INFINITY, |h| h.t);
            if bounces >= 1
                && let Some((t_in, l)) = sphere_chord(origin, dir, job.receiver, r, seg)
            {
                let t_mid = t_in + 0.5 * l;
                let time = (path + t_mid) / s.speed_of_sound;
                let bin = (time / s.bin_s) as usize;
                if bin < out.bins.len() {
                    let w = f64::from(l) * deposit_scale;
                    for b in 0..BANDS {
                        out.bins[bin][b] += f64::from(e[b] * (-air2[b] * t_mid).exp()) * w;
                    }
                }
            }
            let Some(hit) = hit else {
                out.escaped += f64::from(e.iter().sum::<f32>() / BANDS as f32) * inv_rays;
                break;
            };
            if bounces >= 1 {
                out.free_path_sum += f64::from(seg);
                out.free_paths += 1;
            }
            path += seg;
            let material = world.gpu_material(world.hit_material(&hit));
            for b in 0..BANDS {
                e[b] *= (-air2[b] * seg).exp() * material.reflect[b];
            }
            if bounces >= job.max_bounces
                || e.iter().fold(0.0f32, |a, &v| a.max(v)) < MAX_RELATIVE_FLOOR
            {
                break;
            }
            bounces += 1;
            let n = world.hit_normal(&hit, dir);
            let p = math::add(origin, math::scale(dir, seg));
            let (us, u1, u2) = (rng.next_f32(), rng.next_f32(), rng.next_f32());
            dir = if us < material.scattering {
                cosine_hemisphere(n, u1, u2)
            } else {
                math::sub(dir, math::scale(n, 2.0 * math::dot(dir, n)))
            };
            origin = math::add(p, math::scale(n, ray_offset(p)));
        }
    }
    out
}

/// Chord of the ray `origin + t dir` (unit `dir`) through a sphere, clipped to
/// `t` in `[0, t_max]`: `(t_in, length)`.
#[inline]
pub(crate) fn sphere_chord(
    origin: Vec3,
    dir: Vec3,
    center: Vec3,
    radius: f32,
    t_max: f32,
) -> Option<(f32, f32)> {
    let oc = math::sub(origin, center);
    let b = math::dot(dir, oc);
    let c = math::dot(oc, oc) - radius * radius;
    let disc = b * b - c;
    if disc <= 0.0 {
        return None;
    }
    let sq = disc.sqrt();
    let t_in = (-b - sq).max(0.0);
    let t_out = (-b + sq).min(t_max);
    (t_out > t_in).then_some((t_in, t_out - t_in))
}
