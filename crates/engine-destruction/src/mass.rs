//! Volume, centroid and inertia of a closed triangle mesh (unit density).
use glam::{DMat3, DVec3};

pub(crate) struct Props {
    pub(crate) volume: f64,
    pub(crate) centroid: DVec3,
    /// Inertia tensor about the centroid at unit density, in mesh axes.
    pub(crate) inertia: DMat3,
}
fn outer(a: DVec3, b: DVec3) -> DMat3 {
    DMat3::from_cols(a * b.x, a * b.y, a * b.z)
}
pub(crate) fn mass_properties(verts: &[DVec3], tris: &[[u32; 3]]) -> Props {
    let used: Vec<DVec3> = tris.iter().flatten().map(|&i| verts[i as usize]).collect();
    let reference = if used.is_empty() {
        DVec3::ZERO
    } else {
        used.iter().copied().sum::<DVec3>() / used.len() as f64
    };
    let mut volume = 0.;
    let mut moment = DVec3::ZERO;
    let mut covariance = DMat3::ZERO;
    for t in tris {
        let a = verts[t[0] as usize] - reference;
        let b = verts[t[1] as usize] - reference;
        let c = verts[t[2] as usize] - reference;
        let v = a.dot(b.cross(c)) / 6.;
        let s = a + b + c;
        volume += v;
        moment += s * (v / 4.);
        covariance += (outer(a, a) + outer(b, b) + outer(c, c) + outer(s, s)) * (v / 20.);
    }
    if volume.abs() < 1e-300 {
        return Props {
            volume: 0.,
            centroid: reference,
            inertia: DMat3::ZERO,
        };
    }
    let centre = moment / volume;
    let about = covariance - outer(centre, centre) * volume;
    let trace = about.x_axis.x + about.y_axis.y + about.z_axis.z;
    Props {
        volume,
        centroid: reference + centre,
        inertia: DMat3::IDENTITY * trace - about,
    }
}
