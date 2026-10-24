use super::*;
use gltf::{
    Semantic,
    accessor::{DataType, Dimensions},
};

pub(super) fn import_meshes(
    document: &gltf::Document,
    buffers: &[Vec<u8>],
    limits: &ImportLimits,
) -> Result<Vec<ImportedMesh>> {
    let mut meshes = Vec::new();
    let mut vertex_count = 0;
    let mut index_count = 0;
    for mesh in document.meshes() {
        let mut primitives = Vec::new();
        for primitive in mesh.primitives() {
            if primitive.mode() != gltf::mesh::Mode::Triangles {
                return Err(AssetError::Unsupported("non-triangle primitive".into()));
            }
            if primitive.morph_targets().next().is_some() {
                return Err(AssetError::Unsupported("morph targets".into()));
            }
            let position = primitive
                .get(&Semantic::Positions)
                .ok_or_else(|| invalid("primitive missing positions"))?;
            let count = position.count();
            if count > u32::MAX as usize {
                return Err(AssetError::Limit("vertex index range"));
            }
            add_budget(&mut vertex_count, count, limits.max_vertices, "vertices")?;
            for (semantic, accessor) in primitive.attributes() {
                if accessor.count() != count {
                    return Err(invalid("vertex attribute count mismatch"));
                }
                validate_attribute(&semantic, &accessor)?;
            }
            if primitive.get(&Semantic::Joints(0)).is_some()
                != primitive.get(&Semantic::Weights(0)).is_some()
            {
                return Err(invalid("joints and weights must be paired"));
            }
            if let Some(indices) = primitive.indices() {
                if indices.dimensions() != Dimensions::Scalar
                    || !matches!(
                        indices.data_type(),
                        DataType::U8 | DataType::U16 | DataType::U32
                    )
                    || indices.normalized()
                {
                    return Err(invalid("invalid index accessor format"));
                }
                add_budget(
                    &mut index_count,
                    indices.count(),
                    limits.max_indices,
                    "indices",
                )?;
            } else {
                add_budget(&mut index_count, count, limits.max_indices, "indices")?;
            }
            let reader = primitive.reader(|buffer| Some(buffers[buffer.index()].as_slice()));
            let positions: Vec<_> = reader
                .read_positions()
                .ok_or_else(|| invalid("cannot read positions"))?
                .collect();
            let normals = reader.read_normals().map(|v| v.collect::<Vec<_>>());
            let tangents = reader.read_tangents().map(|v| v.collect::<Vec<_>>());
            let uv0 = reader
                .read_tex_coords(0)
                .map(|v| v.into_f32().collect::<Vec<_>>());
            let uv1 = reader
                .read_tex_coords(1)
                .map(|v| v.into_f32().collect::<Vec<_>>());
            let colors = reader
                .read_colors(0)
                .map(|v| v.into_rgba_f32().collect::<Vec<_>>());
            let joints = reader
                .read_joints(0)
                .map(|v| v.into_u16().collect::<Vec<_>>());
            let weights = reader
                .read_weights(0)
                .map(|v| v.into_f32().collect::<Vec<_>>());
            let indices = reader
                .read_indices()
                .map(|v| v.into_u32().collect::<Vec<_>>())
                .unwrap_or_else(|| (0..count as u32).collect());
            if indices.is_empty()
                || !indices.len().is_multiple_of(3)
                || indices.iter().any(|&i| i as usize >= count)
            {
                return Err(invalid("invalid triangle indices"));
            }
            let mut vertices = Vec::with_capacity(count);
            for (i, position) in positions.into_iter().enumerate() {
                let vertex = ImportedVertex {
                    position,
                    normal: normals.as_ref().map(|v| v[i]),
                    tangent: tangents.as_ref().map(|v| v[i]),
                    uv0: uv0.as_ref().map(|v| v[i]),
                    uv1: uv1.as_ref().map(|v| v[i]),
                    color: colors.as_ref().map(|v| v[i]),
                    joints: joints.as_ref().map(|v| v[i]),
                    weights: weights.as_ref().map(|v| v[i]),
                };
                finite(
                    vertex
                        .position
                        .into_iter()
                        .chain(vertex.normal.into_iter().flatten())
                        .chain(vertex.tangent.into_iter().flatten())
                        .chain(vertex.uv0.into_iter().flatten())
                        .chain(vertex.uv1.into_iter().flatten())
                        .chain(vertex.color.into_iter().flatten())
                        .chain(vertex.weights.into_iter().flatten()),
                )?;
                if let Some(weights) = vertex.weights {
                    let sum = weights.iter().sum::<f32>();
                    if weights.iter().any(|&v| !(0.0..=1.0).contains(&v))
                        || !sum.is_finite()
                        || sum <= 0.0
                    {
                        return Err(invalid("invalid skin weights"));
                    }
                }
                vertices.push(vertex);
            }
            primitives.push(ImportedPrimitive {
                vertices,
                indices,
                material: primitive.material().index(),
            });
        }
        meshes.push(ImportedMesh {
            name: mesh.name().map(str::to_owned),
            primitives,
        });
    }
    Ok(meshes)
}
fn validate_attribute(semantic: &Semantic, accessor: &gltf::Accessor<'_>) -> Result<()> {
    let data = accessor.data_type();
    let dim = accessor.dimensions();
    let float = data == DataType::F32 && !accessor.normalized();
    let normalized =
        float || (matches!(data, DataType::U8 | DataType::U16) && accessor.normalized());
    let supported = match semantic {
        Semantic::Positions | Semantic::Normals => float && dim == Dimensions::Vec3,
        Semantic::Tangents => float && dim == Dimensions::Vec4,
        Semantic::TexCoords(0 | 1) => normalized && dim == Dimensions::Vec2,
        Semantic::Colors(0) => normalized && matches!(dim, Dimensions::Vec3 | Dimensions::Vec4),
        Semantic::Joints(0) => {
            matches!(data, DataType::U8 | DataType::U16)
                && !accessor.normalized()
                && dim == Dimensions::Vec4
        }
        Semantic::Weights(0) => normalized && dim == Dimensions::Vec4,
        _ => {
            return Err(AssetError::Unsupported(format!(
                "vertex attribute {semantic:?}"
            )));
        }
    };
    if !supported {
        return Err(invalid("invalid vertex attribute format"));
    }
    Ok(())
}
pub(super) fn validate_skin_vertices(
    nodes: &[ImportedNode],
    meshes: &[ImportedMesh],
    skins: &[ImportedSkin],
) -> Result<()> {
    // Shared mesh instances must not multiply validation work by vertex count.
    let bounds: Vec<_> = meshes
        .iter()
        .map(|mesh| {
            let mut maximum = 0_u16;
            let mut complete = true;
            for primitive in &mesh.primitives {
                for vertex in &primitive.vertices {
                    if let Some(joints) = vertex.joints {
                        for joint in joints {
                            maximum = maximum.max(joint);
                        }
                    } else {
                        complete = false;
                    }
                }
            }
            (maximum, complete)
        })
        .collect();
    for node in nodes {
        if let (Some(mesh), Some(skin)) = (node.mesh, node.skin) {
            let joint_count = skins[skin].joints.len();
            if !bounds[mesh].1 {
                return Err(invalid("skinned mesh missing joint attributes"));
            }
            if bounds[mesh].0 as usize >= joint_count {
                return Err(invalid("vertex joint index outside skin"));
            }
        }
    }
    Ok(())
}
