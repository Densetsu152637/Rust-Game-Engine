//! Conservative projection of bounded built-in mesh geometry into Hi-Z queries.
use engine_render_hiz::VisibilityQuery;
use glam::{Mat4, Vec3};

/// Project every corner of a local AABB with the exact jittered matrix used
/// for camera rasterization. Any clipping or numerical uncertainty stays visible.
pub fn project_bounds(
    clip_from_world: Mat4,
    world_from_local: Mat4,
    min: Vec3,
    max: Vec3,
    size: [u32; 2],
) -> VisibilityQuery {
    let visible = || VisibilityQuery::new([0; 4], 0.0, true);
    if size.contains(&0) || !clip_from_world.is_finite() || !world_from_local.is_finite()
        || !min.is_finite() || !max.is_finite()
        || min.x > max.x || min.y > max.y || min.z > max.z
    {
        return visible();
    }
    let matrix = clip_from_world * world_from_local;
    let mut low = [f32::INFINITY; 3];
    let mut high = [f32::NEG_INFINITY; 2];
    for x in [min.x, max.x] {
        for y in [min.y, max.y] {
            for z in [min.z, max.z] {
                let clip = matrix * Vec3::new(x, y, z).extend(1.0);
                // A box crossing any clip plane can project beyond its
                // corners. Conservatively retain it rather than clip it here.
                if !clip.is_finite() || clip.w <= 0.0 || clip.z < 0.0
                    || clip.z > clip.w || clip.x.abs() > clip.w || clip.y.abs() > clip.w
                {
                    return visible();
                }
                let ndc = clip.truncate() / clip.w;
                let px = (ndc.x + 1.0) * 0.5 * size[0] as f32;
                let py = (1.0 - ndc.y) * 0.5 * size[1] as f32;
                low[0] = low[0].min(px);
                low[1] = low[1].min(py);
                low[2] = low[2].min(ndc.z);
                high[0] = high[0].max(px);
                high[1] = high[1].max(py);
            }
        }
    }
    // Padding covers pixel centres, float conversion and raster edge rules.
    let rect = [
        (low[0].floor() - 2.0) as i32,
        (low[1].floor() - 2.0) as i32,
        (high[0].ceil() + 2.0) as i32,
        (high[1].ceil() + 2.0) as i32,
    ];
    if rect[0] < 0 || rect[1] < 0 || rect[2] > size[0] as i32
        || rect[3] > size[1] as i32 || !low[2].is_finite()
    {
        return visible();
    }
    VisibilityQuery::new(rect, (low[2] - 1e-4).max(0.0), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipping_and_invalid_bounds_remain_visible() {
        let identity = Mat4::IDENTITY;
        for (min, max) in [
            (Vec3::new(-0.1, -0.1, -0.1), Vec3::new(0.1, 0.1, 0.1)),
            (Vec3::new(0.1, 0.0, 0.2), Vec3::new(-0.1, 0.0, 0.3)),
        ] {
            assert_ne!(project_bounds(identity, identity, min, max, [100, 100]).uncertain, 0);
        }
    }
    #[test]
    fn centered_box_has_padded_viewport_rectangle() {
        let query = project_bounds(Mat4::IDENTITY, Mat4::IDENTITY,
            Vec3::new(-0.1, -0.1, 0.2), Vec3::new(0.1, 0.1, 0.3), [100, 100]);
        assert_eq!(query.uncertain, 0);
        assert_eq!(query.rect, [43, 43, 57, 57]);
        assert!(query.nearest_depth < 0.2);
    }
}
