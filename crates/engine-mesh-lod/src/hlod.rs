//! Hierarchical LOD: spatial clustering of static instances and proxy meshes.

use glam::{Mat3, Mat4, Vec3};

use crate::bounds::{Aabb, BoundingSphere};
use crate::error::{MeshLodError, Result};
use crate::lod::projected_error_pixels;
use crate::mesh::MeshData;
use crate::simplify::{SimplifyOptions, simplify};

/// One static mesh instance placed in the world.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HlodInstance {
    /// Index into the `meshes` slice given to [`build_hlod`].
    pub mesh: usize,
    /// Object-to-world transform (must be invertible).
    pub transform: Mat4,
}

/// Clustering, proxy and switch-distance settings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HlodOptions {
    /// A cluster is split while its world AABB has a side longer than this.
    pub max_cluster_extent: f32,
    /// A cluster is split while it has more members than this (>= 1).
    pub max_cluster_instances: usize,
    /// Proxy triangle count as a fraction of the merged count, in `(0, 1]`.
    pub proxy_ratio: f32,
    /// Hard cap on proxy triangles per cluster (>= 1).
    pub max_proxy_triangles: usize,
    /// Weights / locking for proxy simplification (targets are overridden).
    pub simplify: SimplifyOptions,
    /// Vertical FOV (radians) used for the switch distance.
    pub vertical_fov: f32,
    /// Viewport height (pixels) used for the switch distance.
    pub viewport_height: f32,
    /// Pixel error at which the proxy replaces its members.
    pub pixel_threshold: f32,
}

impl Default for HlodOptions {
    fn default() -> Self {
        Self {
            max_cluster_extent: 100.0,
            max_cluster_instances: 64,
            proxy_ratio: 0.1,
            max_proxy_triangles: 4096,
            simplify: SimplifyOptions::default(),
            vertical_fov: 60f32.to_radians(),
            viewport_height: 1080.0,
            pixel_threshold: 2.0,
        }
    }
}

impl HlodOptions {
    fn validate(&self) -> Result<()> {
        let bad = |reason: &str| {
            Err(MeshLodError::InvalidOption {
                reason: reason.into(),
            })
        };
        if !(self.max_cluster_extent.is_finite() && self.max_cluster_extent > 0.0) {
            return bad("max_cluster_extent must be finite and > 0");
        }
        if self.max_cluster_instances == 0 || self.max_proxy_triangles == 0 {
            return bad("max_cluster_instances and max_proxy_triangles must be >= 1");
        }
        if !(self.proxy_ratio.is_finite() && self.proxy_ratio > 0.0 && self.proxy_ratio <= 1.0) {
            return bad("proxy_ratio must be in (0, 1]");
        }
        if !(self.vertical_fov.is_finite()
            && self.vertical_fov > 0.0
            && self.vertical_fov < std::f32::consts::PI
            && self.viewport_height.is_finite()
            && self.viewport_height > 0.0
            && self.pixel_threshold.is_finite()
            && self.pixel_threshold > 0.0)
        {
            return bad("vertical_fov in (0, pi), viewport_height > 0 and pixel_threshold > 0 required");
        }
        Ok(())
    }
}

/// A cluster of instances replaced by one proxy at distance.
#[derive(Debug, Clone, PartialEq)]
pub struct HlodCluster {
    /// Ascending indices into the input instance slice.
    pub members: Vec<usize>,
    /// World-space bounds of all members.
    pub bounds: Aabb,
    pub bounding_sphere: BoundingSphere,
    /// World-space simplified proxy.
    pub proxy: MeshData,
    /// Triangles before proxy simplification.
    pub merged_triangles: usize,
    /// Approximate Hausdorff error of the proxy against the merged members.
    pub proxy_error: f32,
    /// Camera distance (to the cluster centre) beyond which the proxy is used.
    pub switch_distance: f32,
}

struct Item {
    index: usize,
    center: Vec3,
    bounds: Aabb,
}

/// Splits `items` into clusters by recursive spatial-midpoint subdivision.
fn cluster_items(items: Vec<Item>, opts: &HlodOptions) -> Vec<Vec<Item>> {
    let mut done = Vec::new();
    let mut stack = vec![items];
    while let Some(group) = stack.pop() {
        let bounds = group
            .iter()
            .map(|i| i.bounds)
            .reduce(|a, b| a.union(&b))
            .expect("non-empty group");
        let small = group.len() <= opts.max_cluster_instances && bounds.max_extent() <= opts.max_cluster_extent;
        if small || group.len() == 1 {
            done.push(group);
            continue;
        }
        let centers = Aabb::from_points(&group.iter().map(|i| i.center).collect::<Vec<_>>())
            .expect("non-empty group");
        let span = centers.max - centers.min;
        let axis = if span.x >= span.y && span.x >= span.z {
            0
        } else if span.y >= span.z {
            1
        } else {
            2
        };
        let mid = (centers.min[axis] + centers.max[axis]) * 0.5;
        let (mut lo, mut hi): (Vec<Item>, Vec<Item>) = group.into_iter().partition(|i| i.center[axis] <= mid);
        if lo.is_empty() || hi.is_empty() {
            // All centres coincide on the axis: split by order to guarantee progress.
            let mut all: Vec<Item> = lo.into_iter().chain(hi).collect();
            all.sort_by(|a, b| a.center[axis].total_cmp(&b.center[axis]).then(a.index.cmp(&b.index)));
            hi = all.split_off(all.len() / 2);
            lo = all;
        }
        stack.push(hi);
        stack.push(lo);
    }
    done
}

fn append_instance(dst: &mut MeshData, mesh: &MeshData, transform: &Mat4, with_normals: bool, with_uvs: bool) -> Result<()> {
    let base = u32::try_from(dst.positions.len()).map_err(|_| MeshLodError::TooManyVertices)?;
    base.checked_add(u32::try_from(mesh.positions.len()).map_err(|_| MeshLodError::TooManyVertices)?)
        .ok_or(MeshLodError::TooManyVertices)?;
    let normal_matrix = Mat3::from_mat4(*transform).inverse().transpose();
    dst.positions
        .extend(mesh.positions.iter().map(|p| transform.transform_point3(*p)));
    if with_normals && let (Some(dn), Some(sn)) = (dst.normals.as_mut(), mesh.normals.as_ref()) {
        dn.extend(sn.iter().map(|n| (normal_matrix * *n).normalize_or_zero()));
    }
    if with_uvs && let (Some(du), Some(su)) = (dst.uvs.as_mut(), mesh.uvs.as_ref()) {
        du.extend(su.iter().copied());
    }
    let mirrored = Mat3::from_mat4(*transform).determinant() < 0.0;
    for t in mesh.indices.chunks_exact(3) {
        if mirrored {
            dst.indices.extend([base + t[0], base + t[2], base + t[1]]);
        } else {
            dst.indices.extend([base + t[0], base + t[1], base + t[2]]);
        }
    }
    Ok(())
}

/// Builds HLOD clusters for static `instances` of `meshes`.
///
/// Instances are grouped by recursive spatial-midpoint splits of their world
/// AABB centres until each group has at most `max_cluster_instances` members
/// and a bounds side of at most `max_cluster_extent` (a single instance larger
/// than the extent still forms its own cluster). Each cluster's members are
/// merged in world space (normals/UVs are kept only when every member has
/// them; mirrored transforms have their winding corrected) and simplified to
/// `min(ceil(merged * proxy_ratio), max_proxy_triangles)` triangles.
/// Clusters are returned ordered by their first member index.
pub fn build_hlod(meshes: &[MeshData], instances: &[HlodInstance], options: &HlodOptions) -> Result<Vec<HlodCluster>> {
    options.validate()?;
    if instances.is_empty() {
        return Ok(Vec::new());
    }
    for mesh in meshes {
        mesh.validate()?;
    }
    let mut items = Vec::with_capacity(instances.len());
    for (index, inst) in instances.iter().enumerate() {
        let mesh = meshes.get(inst.mesh).ok_or(MeshLodError::InvalidOption {
            reason: format!("instance {index} references mesh {} out of range", inst.mesh),
        })?;
        if !inst.transform.is_finite() {
            return Err(MeshLodError::NonFiniteValue {
                attribute: "instance transform",
                index,
            });
        }
        if Mat3::from_mat4(inst.transform).determinant().abs() < 1e-12 {
            return Err(MeshLodError::InvalidOption {
                reason: format!("instance {index} has a singular transform"),
            });
        }
        let bounds = mesh.bounds().ok_or(MeshLodError::EmptyMesh)?.transformed(&inst.transform);
        items.push(Item {
            index,
            center: bounds.center(),
            bounds,
        });
    }

    let mut groups = cluster_items(items, options);
    for g in &mut groups {
        g.sort_by_key(|i| i.index);
    }
    groups.sort_by_key(|g| g[0].index);

    let mut clusters = Vec::with_capacity(groups.len());
    for group in groups {
        let members: Vec<usize> = group.iter().map(|i| i.index).collect();
        let bounds = group
            .iter()
            .map(|i| i.bounds)
            .reduce(|a, b| a.union(&b))
            .expect("non-empty group");
        let all_normals = members.iter().all(|&m| meshes[instances[m].mesh].normals.is_some());
        let all_uvs = members.iter().all(|&m| meshes[instances[m].mesh].uvs.is_some());
        let mut merged = MeshData {
            normals: all_normals.then(Vec::new),
            uvs: all_uvs.then(Vec::new),
            ..MeshData::default()
        };
        for &m in &members {
            let inst = &instances[m];
            append_instance(&mut merged, &meshes[inst.mesh], &inst.transform, all_normals, all_uvs)?;
        }
        let merged_triangles = merged.triangle_count();
        let target = ((merged_triangles as f64 * f64::from(options.proxy_ratio)).ceil() as usize)
            .clamp(1, options.max_proxy_triangles);
        let mut sopts = options.simplify;
        sopts.target_triangles = Some(target);
        sopts.max_error = None;
        let result = simplify(&merged, &sopts)?;
        let sphere = BoundingSphere::from_aabb(&bounds);
        let px_scale = projected_error_pixels(1.0, 1.0, options.vertical_fov, options.viewport_height);
        let error_distance = result.achieved_error * px_scale / options.pixel_threshold;
        clusters.push(HlodCluster {
            members,
            bounds,
            bounding_sphere: sphere,
            merged_triangles,
            proxy_error: result.achieved_error,
            switch_distance: error_distance + sphere.radius,
            proxy: result.mesh,
        });
    }
    Ok(clusters)
}
