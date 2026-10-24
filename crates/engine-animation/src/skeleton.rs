use crate::{LocalPose, Mat4, Vec3};
use anyhow::{Result, ensure};

#[derive(Clone, Debug)]
pub struct Skeleton {
    parents: Vec<Option<usize>>,
    inverse_bind: Vec<Mat4>,
    rest: Vec<LocalPose>,
    order: Vec<usize>,
}
impl Skeleton {
    pub fn new(
        parents: Vec<Option<usize>>,
        inverse_bind: Vec<Mat4>,
        rest: Vec<LocalPose>,
    ) -> Result<Self> {
        let count = parents.len();
        ensure!(
            count > 0 && count <= 4096 && inverse_bind.len() == count && rest.len() == count,
            "invalid skeleton sizes"
        );
        for pose in &rest {
            pose.validate()?;
        }
        ensure!(
            inverse_bind
                .iter()
                .all(|m| m.is_finite() && m.determinant().abs() > 1e-12),
            "invalid inverse bind matrix"
        );
        ensure!(
            parents.iter().all(|p| p.is_none_or(|p| p < count)),
            "parent index out of range"
        );
        // Iterative topological sort supports arbitrary authored joint order.
        let mut order = Vec::with_capacity(count);
        let mut seen = vec![false; count];
        while order.len() < count {
            let before = order.len();
            for (i, parent) in parents.iter().enumerate() {
                if !seen[i] && parent.is_none_or(|p| seen[p]) {
                    seen[i] = true;
                    order.push(i);
                }
            }
            ensure!(order.len() > before, "cyclic skeleton");
        }
        Ok(Self {
            parents,
            inverse_bind,
            rest,
            order,
        })
    }
    pub fn joint_count(&self) -> usize {
        self.parents.len()
    }
    pub fn rest_pose(&self) -> &[LocalPose] {
        &self.rest
    }
    pub fn global_matrices(&self, pose: &[LocalPose]) -> Result<Vec<Mat4>> {
        ensure!(pose.len() == self.joint_count(), "pose/skeleton mismatch");
        let mut matrices = vec![Mat4::IDENTITY; pose.len()];
        for &i in &self.order {
            pose[i].validate()?;
            matrices[i] =
                self.parents[i].map_or(Mat4::IDENTITY, |p| matrices[p]) * pose[i].matrix();
            ensure!(matrices[i].is_finite(), "global matrix overflow");
        }
        Ok(matrices)
    }
    /// Matrices are in skeleton space; callers apply the entity/world transform separately.
    pub fn skin_matrices(&self, pose: &[LocalPose]) -> Result<Vec<Mat4>> {
        let matrices: Vec<_> = self
            .global_matrices(pose)?
            .into_iter()
            .zip(&self.inverse_bind)
            .map(|(global, inverse)| global * *inverse)
            .collect();
        ensure!(
            matrices.iter().all(|m| m.is_finite()),
            "skin matrix overflow"
        );
        Ok(matrices)
    }
}
/// Four-weight linear blend skinning. Returns deformed positions and normals.
pub fn skin_vertices(
    positions: &[Vec3],
    normals: &[Vec3],
    joints: &[[u16; 4]],
    weights: &[[f32; 4]],
    matrices: &[Mat4],
) -> Result<(Vec<Vec3>, Vec<Vec3>)> {
    ensure!(
        positions.len() == normals.len()
            && positions.len() == joints.len()
            && positions.len() == weights.len(),
        "skin input sizes differ"
    );
    ensure!(
        matrices
            .iter()
            .all(|m| m.is_finite() && m.determinant().abs() > 1e-12),
        "invalid skin matrix"
    );
    let normal_matrices: Vec<_> = matrices.iter().map(|m| m.inverse().transpose()).collect();
    let mut out_positions = Vec::with_capacity(positions.len());
    let mut out_normals = Vec::with_capacity(positions.len());
    for i in 0..positions.len() {
        ensure!(
            positions[i].is_finite() && normals[i].is_finite(),
            "nonfinite vertex"
        );
        ensure!(
            weights[i].iter().all(|w| w.is_finite() && *w >= 0.),
            "invalid skin weight"
        );
        let total: f32 = weights[i].iter().sum();
        ensure!(total.is_finite() && total > 1e-6, "zero skin weights");
        let mut p = Vec3::ZERO;
        let mut n = Vec3::ZERO;
        for k in 0..4 {
            let w = weights[i][k] / total;
            if w == 0. {
                continue;
            }
            let joint = usize::from(joints[i][k]);
            ensure!(joint < matrices.len(), "skin joint out of range");
            p += matrices[joint].transform_point3(positions[i]) * w;
            n += normal_matrices[joint].transform_vector3(normals[i]) * w;
        }
        ensure!(p.is_finite() && n.is_finite(), "skin result overflow");
        out_positions.push(p);
        out_normals.push(n.normalize_or_zero());
    }
    Ok((out_positions, out_normals))
}
