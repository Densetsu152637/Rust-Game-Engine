use engine_ml::*;
use prost::Message;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tract_onnx::pb;
fn model() -> Model {
    Model::load(&fixtures::double_row(), LoadOptions::default()).unwrap()
}
fn input() -> Tensor {
    Tensor::row(&[1.0, -2.0, 0.5]).unwrap()
}
fn completion(runtime: &mut InferenceRuntime) -> Completion {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(result) = runtime.poll() {
            return result;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
}
#[test]
fn actual_onnx_inspection_and_cpu_inference() {
    let info = inspect(&fixtures::double_row(), &ModelLimits::default()).unwrap();
    assert_eq!(info.inputs[0].name, "input");
    assert_eq!(
        info.outputs[0].shape,
        [DimInfo::Fixed(1), DimInfo::Fixed(3)]
    );
    let output = model().run(&[input()]).unwrap();
    assert_eq!(output[0].as_f32().unwrap(), &[2.0, -4.0, 1.0]);
    assert_eq!(model().run(&[]).unwrap_err().code, "ml.input.count");
    assert_eq!(
        model()
            .run(&[Tensor::row(&[1.0]).unwrap()])
            .unwrap_err()
            .code,
        "ml.input.type"
    );
    assert!(!backend_status().gpu_available);
}
#[test]
fn rejects_bad_models_and_resources_before_execution() {
    assert!(inspect(b"garbage", &ModelLimits::default()).is_err());
    assert!(Model::load(&[], LoadOptions::default()).is_err());
    let bytes = fixtures::double_row();
    let tiny = ModelLimits {
        max_model_bytes: bytes.len() - 1,
        ..Default::default()
    };
    assert_eq!(inspect(&bytes, &tiny).unwrap_err().code, "ml.model.limit");
    let tiny = LoadOptions {
        limits: ModelLimits {
            max_tensor_elements: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    assert!(Model::load(&bytes, tiny).is_err());
    let tiny = ModelLimits {
        max_nodes: 0,
        ..Default::default()
    };
    assert_eq!(inspect(&bytes, &tiny).unwrap_err().code, "ml.model.limit");
    let mut proto = pb::ModelProto::decode(bytes.as_slice()).unwrap();
    proto
        .graph
        .as_mut()
        .unwrap()
        .initializer
        .push(pb::TensorProto {
            name: "secret".into(),
            dims: vec![1],
            data_type: 1,
            external_data: vec![pb::StringStringEntryProto {
                key: "location".into(),
                value: "outside.bin".into(),
            }],
            ..Default::default()
        });
    assert!(inspect(&proto.encode_to_vec(), &ModelLimits::default()).is_err());
    proto.graph.as_mut().unwrap().initializer.clear();
    proto.graph.as_mut().unwrap().node[0]
        .attribute
        .push(pb::AttributeProto {
            g: Some(pb::GraphProto::default()),
            ..Default::default()
        });
    assert!(inspect(&proto.encode_to_vec(), &ModelLimits::default()).is_err());
}
#[test]
fn bounded_jobs_cancel_poll_and_reuse() {
    let mut runtime = InferenceRuntime::new(RuntimeConfig {
        workers: 2,
        max_jobs: 1,
    })
    .unwrap();
    let model = Arc::new(model());
    let id = runtime.submit(model.clone(), vec![input()]).unwrap();
    assert_eq!(
        runtime
            .submit(model.clone(), vec![input()])
            .unwrap_err()
            .code,
        "ml.runtime.full"
    );
    runtime.cancel(id).unwrap();
    runtime.cancel(id).unwrap();
    assert_eq!(runtime.outstanding(), 1);
    let done = completion(&mut runtime);
    assert_eq!(done.id, id);
    assert_eq!(done.result.unwrap_err().code, "ml.job.cancelled");
    assert!(runtime.poll().is_none());
    assert_eq!(runtime.cancel(id).unwrap_err().code, "ml.job.unknown");
    let next = runtime.submit(model, vec![input()]).unwrap();
    assert_ne!(next, id);
    assert_eq!(
        completion(&mut runtime).result.unwrap()[0]
            .as_f32()
            .unwrap(),
        &[2.0, -4.0, 1.0]
    );
    assert!(
        InferenceRuntime::new(RuntimeConfig {
            workers: 0,
            max_jobs: 1
        })
        .is_err()
    );
}
#[test]
fn runtime_drop_acknowledges_queued_jobs() {
    let mut runtime = InferenceRuntime::new(RuntimeConfig {
        workers: 1,
        max_jobs: 4,
    })
    .unwrap();
    let model = Arc::new(model());
    for _ in 0..4 {
        runtime.submit(model.clone(), vec![input()]).unwrap();
    }
    drop(runtime);
}
#[test]
fn symbolic_inputs_need_valid_named_overrides() {
    let mut proto = pb::ModelProto::decode(fixtures::double_row().as_slice()).unwrap();
    let graph = proto.graph.as_mut().unwrap();
    for value in graph.input.iter_mut().chain(graph.output.iter_mut()) {
        let pb::type_proto::Value::TensorType(tensor) =
            value.r#type.as_mut().unwrap().value.as_mut().unwrap();
        tensor.shape.as_mut().unwrap().dim[0].value = Some(
            pb::tensor_shape_proto::dimension::Value::DimParam("batch".into()),
        );
    }
    let bytes = proto.encode_to_vec();
    assert!(Model::load(&bytes, LoadOptions::default()).is_err());
    let mut options = LoadOptions::default();
    options.input_shapes.insert("input".into(), vec![1, 3]);
    assert_eq!(
        Model::load(&bytes, options.clone())
            .unwrap()
            .run(&[input()])
            .unwrap()[0]
            .as_f32()
            .unwrap(),
        &[2.0, -4.0, 1.0]
    );
    options.input_shapes.insert("input".into(), vec![1, 4]);
    assert!(Model::load(&bytes, options.clone()).is_err());
    options.input_shapes.insert("unknown".into(), vec![1, 3]);
    assert!(Model::load(&bytes, options).is_err());
}
#[test]
fn intermediate_tensor_expansion_is_rejected() {
    let mut proto = pb::ModelProto::decode(fixtures::double_row().as_slice()).unwrap();
    let graph = proto.graph.as_mut().unwrap();
    graph.node = vec![
        pb::NodeProto {
            input: vec!["input".into()],
            output: vec!["column".into()],
            op_type: "Transpose".into(),
            ..Default::default()
        },
        pb::NodeProto {
            input: vec!["column".into(), "input".into()],
            output: vec!["expanded".into()],
            op_type: "MatMul".into(),
            ..Default::default()
        },
        pb::NodeProto {
            input: vec!["input".into(), "expanded".into()],
            output: vec!["output".into()],
            op_type: "MatMul".into(),
            ..Default::default()
        },
    ];
    let options = LoadOptions {
        limits: ModelLimits {
            max_tensor_elements: 3,
            ..Default::default()
        },
        ..Default::default()
    };
    let error = match Model::load(&proto.encode_to_vec(), options) {
        Ok(_) => panic!("oversized intermediate was accepted"),
        Err(error) => error,
    };
    assert_eq!(error.code, "ml.model.limit");
}
#[test]
fn malformed_graph_references_return_errors() {
    let mut proto = pb::ModelProto::decode(fixtures::double_row().as_slice()).unwrap();
    proto.graph.as_mut().unwrap().output[0].name = "missing".into();
    assert!(Model::load(&proto.encode_to_vec(), LoadOptions::default()).is_err());
    let mut proto = pb::ModelProto::decode(fixtures::double_row().as_slice()).unwrap();
    proto.graph.as_mut().unwrap().node[0].input[0] = "missing".into();
    assert!(Model::load(&proto.encode_to_vec(), LoadOptions::default()).is_err());
}

#[test]
fn output_metadata_must_match_actual_compiled_tensors() {
    let mut proto = pb::ModelProto::decode(fixtures::double_row().as_slice()).unwrap();
    let pb::type_proto::Value::TensorType(tensor) = proto.graph.as_mut().unwrap().output[0]
        .r#type
        .as_mut()
        .unwrap()
        .value
        .as_mut()
        .unwrap();
    tensor.elem_type = 7;
    assert!(Model::load(&proto.encode_to_vec(), LoadOptions::default()).is_err());
    let mut proto = pb::ModelProto::decode(fixtures::double_row().as_slice()).unwrap();
    let pb::type_proto::Value::TensorType(tensor) = proto.graph.as_mut().unwrap().output[0]
        .r#type
        .as_mut()
        .unwrap()
        .value
        .as_mut()
        .unwrap();
    tensor.shape.as_mut().unwrap().dim[1].value =
        Some(pb::tensor_shape_proto::dimension::Value::DimValue(2));
    assert!(Model::load(&proto.encode_to_vec(), LoadOptions::default()).is_err());
}

#[test]
fn stale_job_ids_cannot_address_a_different_runtime() {
    let model = Arc::new(model());
    let config = RuntimeConfig {
        workers: 1,
        max_jobs: 1,
    };
    let mut first = InferenceRuntime::new(config).unwrap();
    let stale = first.submit(model.clone(), vec![input()]).unwrap();
    let mut second = InferenceRuntime::new(config).unwrap();
    let current = second.submit(model, vec![input()]).unwrap();
    assert_ne!(stale, current);
    assert_eq!(second.cancel(stale).unwrap_err().code, "ml.job.unknown");
    assert_eq!(
        completion(&mut second).result.unwrap()[0].as_f32().unwrap(),
        &[2.0, -4.0, 1.0]
    );
}
