use crate::{DType, MlError, Result, Tensor, tensor::element_count};
use prost::Message;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};
use tract_onnx::{pb, prelude::*};

#[derive(Clone, Debug)]
pub struct ModelLimits {
    pub max_model_bytes: usize,
    pub max_nodes: usize,
    pub max_tensor_elements: usize,
}
impl Default for ModelLimits {
    fn default() -> Self {
        Self {
            max_model_bytes: 16 * 1024 * 1024,
            max_nodes: 1024,
            max_tensor_elements: 1024 * 1024,
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct LoadOptions {
    pub limits: ModelLimits,
    /// Concrete shapes by input name; required for symbolic or unknown dimensions.
    pub input_shapes: BTreeMap<String, Vec<usize>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DimInfo {
    Fixed(usize),
    Symbol(String),
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TensorInfo {
    pub name: String,
    pub dtype: DType,
    pub shape: Vec<DimInfo>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInfo {
    pub name: String,
    pub inputs: Vec<TensorInfo>,
    pub outputs: Vec<TensorInfo>,
    pub nodes: usize,
}

fn error(message: impl ToString) -> MlError {
    MlError::new("ml.model.invalid", message.to_string())
}
fn limit(message: impl Into<String>) -> MlError {
    MlError::new("ml.model.limit", message)
}
fn check_shape(shape: &[usize], limits: &ModelLimits) -> Result<()> {
    if shape.len() > 16
        || shape
            .iter()
            .any(|d| *d == 0 || *d > limits.max_tensor_elements)
        || element_count(shape)? > limits.max_tensor_elements
    {
        return Err(limit(
            "tensor rank or element count exceeds configured limit",
        ));
    }
    Ok(())
}
fn validate_tensor(t: &pb::TensorProto, limits: &ModelLimits) -> Result<()> {
    if !t.external_data.is_empty() || t.data_location.unwrap_or(0) != 0 {
        return Err(error("external tensor data is unsupported"));
    }
    let shape = t
        .dims
        .iter()
        .map(|d| usize::try_from(*d).map_err(error))
        .collect::<Result<Vec<_>>>()?;
    check_shape(&shape, limits)
}
fn decode(bytes: &[u8], limits: &ModelLimits) -> Result<pb::ModelProto> {
    if bytes.len() > limits.max_model_bytes {
        return Err(limit("model byte limit exceeded"));
    }
    let proto = pb::ModelProto::decode(bytes).map_err(error)?;
    if !proto.training_info.is_empty() || !proto.functions.is_empty() {
        return Err(error("training graphs and local functions are unsupported"));
    }
    let graph = proto.graph.as_ref().ok_or_else(|| error("missing graph"))?;
    if graph.node.len() > limits.max_nodes {
        return Err(limit("node limit exceeded"));
    }
    if graph.input.len() > 64 || graph.output.len() > 64 {
        return Err(limit("too many inputs or outputs"));
    }
    if !graph.sparse_initializer.is_empty() {
        return Err(error("sparse initializers are unsupported"));
    }
    for value in graph
        .input
        .iter()
        .chain(&graph.output)
        .chain(&graph.value_info)
    {
        let meta = tensor_info(value)?;
        if meta.shape.len() > 16 {
            return Err(limit("tensor rank limit exceeded"));
        }
        let fixed = meta
            .shape
            .iter()
            .filter_map(|d| {
                if let DimInfo::Fixed(n) = d {
                    Some(*n)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        check_shape(&fixed, limits)?;
    }
    for tensor in &graph.initializer {
        validate_tensor(tensor, limits)?;
    }
    let mut names = BTreeSet::new();
    for input in &graph.input {
        if input.name.is_empty() || !names.insert(input.name.as_str()) {
            return Err(error("empty or duplicate input name"));
        }
    }
    let mut initializers = BTreeSet::new();
    for tensor in &graph.initializer {
        if tensor.name.is_empty() || !initializers.insert(tensor.name.as_str()) {
            return Err(error("empty or duplicate initializer name"));
        }
        names.insert(tensor.name.as_str());
    }
    for node in &graph.node {
        if node
            .input
            .iter()
            .any(|name| !name.is_empty() && !names.contains(name.as_str()))
        {
            return Err(error("node refers to missing or non-topological input"));
        }
        if node.output.is_empty() {
            return Err(error("node has no output"));
        }
        for output in &node.output {
            if output.is_empty() || !names.insert(output.as_str()) {
                return Err(error("empty or duplicate node output"));
            }
        }
        for a in &node.attribute {
            if a.g.is_some()
                || !a.graphs.is_empty()
                || a.sparse_tensor.is_some()
                || !a.sparse_tensors.is_empty()
            {
                return Err(error("nested graphs and sparse attributes are unsupported"));
            }
            for t in a.t.iter().chain(a.tensors.iter()) {
                validate_tensor(t, limits)?;
            }
        }
    }
    let mut outputs = BTreeSet::new();
    for output in &graph.output {
        if !names.contains(output.name.as_str()) || !outputs.insert(output.name.as_str()) {
            return Err(error("missing or duplicate graph output"));
        }
    }
    if graph.output.is_empty() {
        return Err(error("graph has no output"));
    }
    Ok(proto)
}
fn tensor_info(value: &pb::ValueInfoProto) -> Result<TensorInfo> {
    let Some(pb::type_proto::Value::TensorType(t)) =
        value.r#type.as_ref().and_then(|t| t.value.as_ref())
    else {
        return Err(error("missing tensor type"));
    };
    let dtype = match t.elem_type {
        1 => DType::F32,
        7 => DType::I64,
        6 => DType::I32,
        2 => DType::U8,
        _ => return Err(error("unsupported tensor type")),
    };
    let shape = t
        .shape
        .as_ref()
        .ok_or_else(|| error("missing tensor rank"))?
        .dim
        .iter()
        .map(|d| match &d.value {
            Some(pb::tensor_shape_proto::dimension::Value::DimValue(d)) => {
                usize::try_from(*d).map(DimInfo::Fixed).map_err(error)
            }
            Some(pb::tensor_shape_proto::dimension::Value::DimParam(s)) => {
                Ok(DimInfo::Symbol(s.clone()))
            }
            None => Ok(DimInfo::Unknown),
        })
        .collect::<Result<_>>()?;
    Ok(TensorInfo {
        name: value.name.clone(),
        dtype,
        shape,
    })
}
fn info(proto: &pb::ModelProto) -> Result<ModelInfo> {
    let g = proto.graph.as_ref().ok_or_else(|| error("missing graph"))?;
    Ok(ModelInfo {
        name: g.name.clone(),
        nodes: g.node.len(),
        inputs: g
            .input
            .iter()
            .filter(|v| !g.initializer.iter().any(|t| t.name == v.name))
            .map(tensor_info)
            .collect::<Result<_>>()?,
        outputs: g.output.iter().map(tensor_info).collect::<Result<_>>()?,
    })
}
pub fn inspect(bytes: &[u8], limits: &ModelLimits) -> Result<ModelInfo> {
    info(&decode(bytes, limits)?)
}
fn read(path: &Path, limits: &ModelLimits) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path).map_err(|e| MlError::new("ml.model.io", e.to_string()))?;
    let bound = u64::try_from(limits.max_model_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let mut bytes = Vec::new();
    file.take(bound)
        .read_to_end(&mut bytes)
        .map_err(|e| MlError::new("ml.model.io", e.to_string()))?;
    if bytes.len() > limits.max_model_bytes {
        return Err(limit("model byte limit exceeded"));
    }
    Ok(bytes)
}
pub fn inspect_path(path: impl AsRef<Path>, limits: &ModelLimits) -> Result<ModelInfo> {
    inspect(&read(path.as_ref(), limits)?, limits)
}

pub struct Model {
    info: ModelInfo,
    plan: std::sync::Arc<TypedRunnableModel>,
    inputs: Vec<Vec<usize>>,
    limits: ModelLimits,
}
impl Model {
    pub fn load(bytes: &[u8], options: LoadOptions) -> Result<Self> {
        std::panic::catch_unwind(|| Self::load_inner(bytes, options))
            .unwrap_or_else(|_| Err(error("ONNX compiler panicked while loading model")))
    }
    fn load_inner(bytes: &[u8], options: LoadOptions) -> Result<Self> {
        let proto = decode(bytes, &options.limits)?;
        let info = info(&proto)?;
        if options
            .input_shapes
            .keys()
            .any(|name| !info.inputs.iter().any(|i| &i.name == name))
        {
            return Err(error("shape override names an unknown input"));
        }
        let mut graph = tract_onnx::onnx()
            .model_for_proto_model(&proto)
            .map_err(error)?;
        let mut inputs = Vec::new();
        for (index, input) in info.inputs.iter().enumerate() {
            let shape = if let Some(shape) = options.input_shapes.get(&input.name) {
                shape.clone()
            } else {
                input
                    .shape
                    .iter()
                    .map(|d| {
                        if let DimInfo::Fixed(n) = d {
                            Ok(*n)
                        } else {
                            Err(error("dynamic input requires a concrete shape override"))
                        }
                    })
                    .collect::<Result<Vec<_>>>()?
            };
            if shape.len() != input.shape.len()
                || shape
                    .iter()
                    .zip(&input.shape)
                    .any(|(n, d)| matches!(d, DimInfo::Fixed(f) if n != f))
            {
                return Err(error("shape override conflicts with declared input shape"));
            }
            check_shape(&shape, &options.limits)?;
            graph
                .set_input_fact(
                    index,
                    InferenceFact::dt_shape(input.dtype.to_tract(), shape.clone()),
                )
                .map_err(error)?;
            inputs.push(shape);
        }
        let typed = graph.into_typed().map_err(error)?;
        validate_facts(&typed, &options.limits)?;
        validate_outputs(&typed, &info.outputs)?;
        let typed = typed.into_optimized().map_err(error)?;
        validate_facts(&typed, &options.limits)?;
        validate_outputs(&typed, &info.outputs)?;
        let plan = typed.into_runnable().map_err(error)?;
        Ok(Self {
            info,
            plan,
            inputs,
            limits: options.limits,
        })
    }
    pub fn load_path(path: impl AsRef<Path>, options: LoadOptions) -> Result<Self> {
        Self::load(&read(path.as_ref(), &options.limits)?, options)
    }
    pub fn info(&self) -> &ModelInfo {
        &self.info
    }
    pub(crate) fn validate_inputs(&self, inputs: &[Tensor]) -> Result<()> {
        if inputs.len() != self.inputs.len() {
            return Err(MlError::new("ml.input.count", "wrong number of inputs"));
        }
        for ((tensor, shape), expected) in inputs.iter().zip(&self.inputs).zip(&self.info.inputs) {
            if tensor.shape() != shape || tensor.dtype() != expected.dtype {
                return Err(MlError::new(
                    "ml.input.type",
                    "input shape or dtype mismatch",
                ));
            }
        }
        Ok(())
    }
    pub fn run(&self, inputs: &[Tensor]) -> Result<Vec<Tensor>> {
        self.validate_inputs(inputs)?;
        let values = inputs
            .iter()
            .map(|t| Ok(t.to_tract()?.into_tvalue()))
            .collect::<Result<TVec<_>>>()?;
        self.plan
            .run(values)
            .map_err(|e| MlError::new("ml.inference.failed", e.to_string()))?
            .iter()
            .map(|v| {
                check_shape(v.shape(), &self.limits)?;
                Tensor::from_tract(v)
            })
            .collect()
    }
}
fn validate_facts(model: &TypedModel, limits: &ModelLimits) -> Result<()> {
    if model.nodes().len() > limits.max_nodes {
        return Err(limit("compiled node limit exceeded"));
    }
    for node in model.nodes() {
        for output in &node.outputs {
            let shape = output
                .fact
                .shape
                .as_concrete()
                .ok_or_else(|| error("dynamic intermediate tensors are unsupported"))?;
            check_shape(shape, limits)?;
        }
    }
    Ok(())
}

fn validate_outputs(model: &TypedModel, outputs: &[TensorInfo]) -> Result<()> {
    if model.output_outlets().map_err(error)?.len() != outputs.len() {
        return Err(error(
            "compiled output count conflicts with declared outputs",
        ));
    }
    for (index, declared) in outputs.iter().enumerate() {
        let fact = model.output_fact(index).map_err(error)?;
        let shape = fact
            .shape
            .as_concrete()
            .ok_or_else(|| error("dynamic outputs are unsupported"))?;
        if DType::from_tract(fact.datum_type) != Some(declared.dtype)
            || shape.len() != declared.shape.len()
            || shape.iter().zip(&declared.shape).any(|(actual, declared)| matches!(declared, DimInfo::Fixed(expected) if actual != expected))
        {
            return Err(error("compiled output shape or dtype conflicts with declared metadata"));
        }
    }
    Ok(())
}
