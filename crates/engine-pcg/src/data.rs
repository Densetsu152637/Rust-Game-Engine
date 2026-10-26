//! Values that flow along graph edges: points, scalar fields and spawned instances.
use crate::noise::Noise;
use crate::surface::Surface;
use glam::{Quat, Vec3};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// One generated sample position with orientation and a density weight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub position: Vec3,
    pub rotation: Quat,
    pub scale: Vec3,
    /// Ground normal at the sample (up when unknown).
    pub normal: Vec3,
    /// Weight in `[0, 1]`; filters write it, self-pruning prefers higher values.
    pub density: f32,
    /// Stable per-point random seed assigned by the sampler.
    pub seed: u32,
}

impl Point {
    pub fn new(position: Vec3) -> Self {
        Self {
            position,
            rotation: Quat::IDENTITY,
            scale: Vec3::ONE,
            normal: Vec3::Y,
            density: 1.0,
            seed: 0,
        }
    }
}

/// What a spawned instance refers to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstanceKind {
    /// A project prefab asset id.
    Prefab,
    /// A project mesh asset id.
    #[default]
    Mesh,
}

/// A placed object: the final product of a graph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Instance {
    pub position: [f32; 3],
    /// Quaternion `[x, y, z, w]`.
    pub rotation: [f32; 4],
    pub scale: [f32; 3],
    pub kind: InstanceKind,
    pub asset: String,
}

/// Binary operation combining two fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MathOp {
    Add,
    Subtract,
    Multiply,
    Min,
    Max,
}

impl MathOp {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "add" => Self::Add,
            "subtract" => Self::Subtract,
            "multiply" => Self::Multiply,
            "min" => Self::Min,
            "max" => Self::Max,
            _ => return None,
        })
    }
    pub fn apply(self, a: f32, b: f32) -> f32 {
        match self {
            Self::Add => a + b,
            Self::Subtract => a - b,
            Self::Multiply => a * b,
            Self::Min => a.min(b),
            Self::Max => a.max(b),
        }
    }
}

/// Environment a field is evaluated in.
pub struct FieldContext<'a> {
    pub surface: &'a dyn Surface,
    /// Absolute position of the local origin; noise samples at `local + offset`.
    pub domain_offset: [f64; 3],
}

/// A scalar function of horizontal position, typically in `[0, 1]`.
#[derive(Clone, Debug)]
pub enum Field {
    Constant(f32),
    /// Noise remapped from `[0, 1]` to `[min, max]`.
    Noise {
        noise: Box<Noise>,
        min: f32,
        max: f32,
    },
    /// Ground height remapped from `[low, high]` to `[0, 1]` and clamped.
    Height {
        low: f32,
        high: f32,
    },
    /// Ground slope in degrees divided by 90.
    Slope,
    /// Paint layer weight (0 when the surface has no such layer).
    Layer(String),
    Math {
        op: MathOp,
        a: Arc<Field>,
        b: Arc<Field>,
    },
}

impl Field {
    pub fn eval(&self, cx: &FieldContext<'_>, x: f32, z: f32) -> f32 {
        match self {
            Self::Constant(v) => *v,
            Self::Noise { noise, min, max } => {
                let n = noise.sample_2d(
                    f64::from(x) + cx.domain_offset[0],
                    f64::from(z) + cx.domain_offset[2],
                );
                min + (max - min) * n
            }
            Self::Height { low, high } => {
                let span = high - low;
                if span.abs() < f32::EPSILON {
                    0.0
                } else {
                    ((cx.surface.height(x, z) - low) / span).clamp(0.0, 1.0)
                }
            }
            Self::Slope => {
                let up = cx.surface.normal(x, z).y.clamp(-1.0, 1.0);
                up.acos().to_degrees() / 90.0
            }
            Self::Layer(name) => cx.surface.layer_weight(name, x, z).unwrap_or(0.0),
            Self::Math { op, a, b } => op.apply(a.eval(cx, x, z), b.eval(cx, x, z)),
        }
    }
}

/// Typed value on a graph edge. Payloads are shared, so fan-out never copies.
#[derive(Clone, Debug)]
pub enum Data {
    Points(Arc<Vec<Point>>),
    Field(Arc<Field>),
    Instances(Arc<Vec<Instance>>),
}

impl Data {
    pub fn port_type(&self) -> crate::registry::PortType {
        use crate::registry::PortType;
        match self {
            Self::Points(_) => PortType::Points,
            Self::Field(_) => PortType::Field,
            Self::Instances(_) => PortType::Instances,
        }
    }
    pub fn points(points: Vec<Point>) -> Self {
        Self::Points(Arc::new(points))
    }
    pub fn field(field: Field) -> Self {
        Self::Field(Arc::new(field))
    }
    pub fn instances(instances: Vec<Instance>) -> Self {
        Self::Instances(Arc::new(instances))
    }
}
