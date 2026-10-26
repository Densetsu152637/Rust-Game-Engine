# Machine learning

`engine-ml` loads self-contained ONNX models and executes real CPU inference with
tract 0.23.9. GPU inference is unavailable; `backend_status()` reports that explicitly.
The native `engine-project::ml` adapter is excluded on wasm32.

```rust
use engine_ml::{fixtures, LoadOptions, Model, Tensor};
let model = Model::load(&fixtures::double_row(), LoadOptions::default())?;
let output = model.run(&[Tensor::row(&[1.0, -2.0, 0.5])?])?;
assert_eq!(output[0].as_f32(), Some([2.0, -4.0, 1.0].as_slice()));
```

`inspect` / `inspect_path` decode metadata without compiling a plan. `Model::load`
validates and compiles it. Engine tensors support f32, i64, i32 and u8, row-major
storage, vector rows and RGBA8-to-normalized-NCHW conversion. Input order follows
`ModelInfo::inputs`; initializer-backed graph inputs are excluded. Symbolic inputs
need an explicit concrete `LoadOptions::input_shapes` override by name. Overrides
must preserve fixed dimensions. Compiled outputs must match declared dtypes, ranks
and fixed dimensions; symbolic output dimensions may resolve to concrete values.
Dynamic intermediate tensors are rejected.

Limits default to 16 MiB encoded model bytes, 1,024 graph/compiled nodes and
1,048,576 elements per constant/intermediate/input/output tensor. Dimensions must
be positive (scalars use an empty shape). Rank is capped
at 16 and input/output counts at 64. File reads stop after the byte limit plus one.
External tensor data, sparse constants, nested graphs, local functions and training
graphs and invalid graph references are rejected before handing the protobuf to
tract. Unsupported operators or
types return errors. These are in-process admission limits, not a security sandbox:
protobuf parsing, tract translation/optimization, scratch allocations, total tensor
memory and execution time do not have hard OS quotas. Only load models from trusted
sources; use an isolated process for hostile models or hard memory/time budgets.

`InferenceRuntime::new(RuntimeConfig { workers: 1, max_jobs: 16 })` creates a fixed
CPU worker pool. `submit(Arc<Model>, Vec<Tensor>)` validates inputs before queuing
and returns a process-wide unique `JobId`, including across runtime shutdown
and reinstallation. Identifier exhaustion returns an error instead of wrapping;
handles from an earlier runtime cannot address jobs in its replacement. `poll()`
never waits and yields each completion once.
The bound counts queued, running and completed-but-unpolled jobs, preventing an
unconsumed completion backlog. Worker counts are 1..=32 and job limits 1..=1024.
`cancel(id)` marks a queued/running/completed-but-unpolled job cancelled; a running
tract call finishes and its result is discarded. Capacity is released only after
worker acknowledgement is polled. Cancellation is idempotent until that poll;
subsequent uses of the identifier report `ml.job.unknown`. Drop marks outstanding
jobs cancelled, closes the queue and joins workers. Drop/shutdown can block while
an active CPU inference finishes; perform them outside the frame loop.

Native scripts install `engine_project::ml::install(world, config)` explicitly,
load ONNX assets with `load_asset(world, asset_id, options)` using the project's
existing asset/path policy, then `submit`, `poll` and `cancel` through that module.
`available(world)` reports whether workers are installed. Missing services return
`ml.runtime.unavailable`; backend CPU support does not mean a service is installed.
`shutdown(world)` removes the service and joins workers. Reinstalling replaces and
joins the old runtime. Completion errors retain the stable `ml.*` error codes.
The service uses the authoritative ECS world and does not block a frame waiting for
inference. Loading/compiling a model is synchronous and should happen during loading.

Focused validation: `cargo test -p engine-ml` and
`cargo test -p engine-project --test ml`. Fixtures are generated ONNX protobuf bytes
with a genuine Add graph, not mocked outputs or downloaded model assets.

The implementation retains the WIP's pure Rust tract dependency instead of adding
an ONNX Runtime shared-library deployment requirement. A fixed worker pool and
bounded retained jobs keep native scripts independent of an async executor.
There is no synthetic GPU fallback: adding a GPU backend requires its own verified
execution path and lifecycle tests. Arbitrary ONNX control flow and data-dependent
shapes remain outside this API's concrete-shape execution contract.
