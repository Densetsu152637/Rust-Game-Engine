//! Ray-guided image sources for early reflections (section 5.4.3).
//!
//! 1. `er_rays` Fibonacci-sphere rays leave the source and reflect specularly up to
//!    `order` times; the plane clusters they hit (at most [`MAX_PLANES`]) are kept.
//! 2. Every plane sequence up to `order` without consecutive repeats gives an image
//!    source. Its path is validated backwards from the listener: each segment must
//!    first hit the expected cluster at the expected point, and the last segment to
//!    the source must be clear.
//! 3. A path's amplitude per band is
//!    `sqrt(prod (1 - a_b)(1 - s)) * exp(-m_b L) / max(L, 1 m)` (energy reflectance
//!    of each wall, amplitude air absorption over the path length `L` in nepers per
//!    metre, spherical spreading relative to 1 m).
//! 4. The `max_paths` strongest paths (by summed band energy, ties by delay and
//!    sequence) are kept and returned sorted by `(delay, plane sequence)`.
//!
//! For a shoebox this yields `4n^2 + 2` paths of order `n` (6 and 18).

use crate::direct::ListenerFrame;
use crate::math::{self, Vec3};
use crate::rng::fibonacci_sphere;
use crate::world::ray_offset;
use crate::{AcousticWorld, BANDS, BandGains, PlaneCluster, ReflectionPath};

/// Most plane clusters discovered per source.
pub const MAX_PLANES: usize = 64;
/// Highest reflection order on the CPU.
pub const MAX_ORDER: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImageSourceSettings {
    /// `1..=MAX_ORDER`.
    pub order: u8,
    pub er_rays: u16,
    pub max_paths: usize,
    pub speed_of_sound: f32,
    pub air_np_per_m: BandGains,
}

/// One validated path with its plane sequence (for deterministic ordering).
#[derive(Clone, Debug, PartialEq)]
pub struct ImagePath {
    pub planes: Vec<u32>,
    pub path: ReflectionPath,
}

/// Plane clusters discovered from `source` (sorted by cluster index).
pub fn discover_planes(world: &AcousticWorld, source: Vec3, order: u8, er_rays: u16) -> Vec<u32> {
    let mut found: Vec<u32> = Vec::new();
    'rays: for i in 0..u32::from(er_rays) {
        let mut dir = fibonacci_sphere(i, u32::from(er_rays));
        let mut origin = source;
        for _ in 0..order.min(MAX_ORDER) {
            let Some(hit) = world.nearest(origin, dir, 0.0, f32::INFINITY, u8::MAX) else {
                continue 'rays;
            };
            if let Some(c) = world.hit_cluster(&hit)
                && !found.contains(&c)
            {
                if found.len() == MAX_PLANES {
                    break 'rays;
                }
                found.push(c);
            }
            let n = world.hit_normal(&hit, dir);
            let p = math::add(origin, math::scale(dir, hit.t));
            dir = math::sub(dir, math::scale(n, 2.0 * math::dot(dir, n)));
            origin = math::add(p, math::scale(n, ray_offset(p)));
        }
    }
    found.sort_unstable();
    found
}

/// All validated image-source paths from `source` to the listener.
pub fn image_sources(
    world: &AcousticWorld,
    source: Vec3,
    listener: &ListenerFrame,
    settings: &ImageSourceSettings,
) -> Vec<ImagePath> {
    let order = settings.order.min(MAX_ORDER);
    if order == 0 || settings.max_paths == 0 {
        return Vec::new();
    }
    let planes = discover_planes(world, source, order, settings.er_rays);
    let clusters = world.plane_clusters();
    let direct = math::length(math::sub(source, listener.position));
    let mut out = Vec::new();
    let mut seq: Vec<u32> = Vec::with_capacity(usize::from(order));
    let mut images: Vec<Vec3> = Vec::with_capacity(usize::from(order) + 1);
    images.push(source);
    enumerate(
        world,
        clusters,
        &planes,
        order,
        listener,
        direct,
        settings,
        &mut seq,
        &mut images,
        &mut out,
    );
    let strength = |p: &ImagePath| p.path.gains.iter().map(|g| g * g).sum::<f32>();
    out.sort_by(|a, b| {
        strength(b)
            .total_cmp(&strength(a))
            .then(a.path.delay_s.total_cmp(&b.path.delay_s))
            .then(a.planes.cmp(&b.planes))
    });
    out.truncate(settings.max_paths);
    out.sort_by(|a, b| {
        a.path
            .delay_s
            .total_cmp(&b.path.delay_s)
            .then(a.planes.cmp(&b.planes))
    });
    out
}

#[allow(clippy::too_many_arguments)]
fn enumerate(
    world: &AcousticWorld,
    clusters: &[PlaneCluster],
    planes: &[u32],
    order: u8,
    listener: &ListenerFrame,
    direct: f32,
    settings: &ImageSourceSettings,
    seq: &mut Vec<u32>,
    images: &mut Vec<Vec3>,
    out: &mut Vec<ImagePath>,
) {
    for &p in planes {
        if seq.last() == Some(&p) {
            continue;
        }
        let image = clusters[p as usize].reflect_point(*images.last().unwrap_or(&[0.0; 3]));
        seq.push(p);
        images.push(image);
        if let Some(path) = validate(world, clusters, seq, images, listener, direct, settings) {
            out.push(ImagePath {
                planes: seq.clone(),
                path,
            });
        }
        if seq.len() < usize::from(order) {
            enumerate(
                world, clusters, planes, order, listener, direct, settings, seq, images, out,
            );
        }
        seq.pop();
        images.pop();
    }
}

fn validate(
    world: &AcousticWorld,
    clusters: &[PlaneCluster],
    seq: &[u32],
    images: &[Vec3],
    listener: &ListenerFrame,
    direct: f32,
    settings: &ImageSourceSettings,
) -> Option<ReflectionPath> {
    let mut point = listener.position;
    let mut t_min = 0.0f32;
    let mut energy = [1.0f32; BANDS];
    let mut arrival = None;
    for k in (0..seq.len()).rev() {
        let plane = &clusters[seq[k] as usize];
        let target = images[k + 1];
        let dir = math::sub(target, point);
        let denom = math::dot(plane.normal, dir);
        if denom.abs() < 1e-9 {
            return None;
        }
        let s = -plane.signed_distance(point) / denom;
        if !(s > 1e-6 && s < 1.0 - 1e-6) {
            return None;
        }
        let reflection = math::add(point, math::scale(dir, s));
        let unit = math::normalize(dir)?;
        let expected = math::length(math::sub(reflection, point));
        let tolerance = 1e-3 + 1e-4 * expected;
        let hit = world.nearest(point, unit, t_min, expected + tolerance, u8::MAX)?;
        if (hit.t - expected).abs() > tolerance || world.hit_cluster(&hit) != Some(seq[k]) {
            return None;
        }
        if arrival.is_none() {
            arrival = Some(unit);
        }
        let m = world.material(plane.material)?;
        for b in 0..BANDS {
            energy[b] *= (1.0 - m.absorption[b]) * (1.0 - m.scattering);
        }
        point = reflection;
        t_min = 10.0 * ray_offset(point);
    }
    // Last segment back to the source must be clear.
    let to_source = math::sub(images[0], point);
    let remaining = math::length(to_source);
    let unit = math::normalize(to_source)?;
    if remaining > t_min
        && world
            .nearest(point, unit, t_min, remaining - t_min, u8::MAX)
            .is_some()
    {
        return None;
    }
    let length = math::length(math::sub(*images.last()?, listener.position));
    let mut gains = [0.0; BANDS];
    for b in 0..BANDS {
        gains[b] = energy[b].sqrt() * (-settings.air_np_per_m[b] * length).exp() / length.max(1.0);
    }
    Some(ReflectionPath {
        order: seq.len() as u8,
        delay_s: (length - direct) / settings.speed_of_sound,
        length_m: length,
        gains,
        direction: listener.to_local(arrival?),
    })
}
