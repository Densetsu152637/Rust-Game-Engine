//! Small self-contained ONNX fixtures generated from the same protobuf schema as the loader.
use prost::Message;
use tract_onnx::pb;
/// A real ONNX graph computing `output = input + input` for a `[1, 3]` f32 row.
pub fn double_row() -> Vec<u8> {
    fn value(name: &str) -> pb::ValueInfoProto {
        pb::ValueInfoProto {
            name: name.into(),
            r#type: Some(pb::TypeProto {
                value: Some(pb::type_proto::Value::TensorType(pb::type_proto::Tensor {
                    elem_type: 1,
                    shape: Some(pb::TensorShapeProto {
                        dim: [1, 3]
                            .into_iter()
                            .map(|n| pb::tensor_shape_proto::Dimension {
                                value: Some(pb::tensor_shape_proto::dimension::Value::DimValue(n)),
                                ..Default::default()
                            })
                            .collect(),
                    }),
                })),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
    pb::ModelProto {
        ir_version: 8,
        opset_import: vec![pb::OperatorSetIdProto {
            domain: String::new(),
            version: 13,
        }],
        graph: Some(pb::GraphProto {
            name: "double_row".into(),
            input: vec![value("input")],
            output: vec![value("output")],
            node: vec![pb::NodeProto {
                input: vec!["input".into(), "input".into()],
                output: vec!["output".into()],
                op_type: "Add".into(),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    }
    .encode_to_vec()
}
