# engine-render

The backend-neutral graphics adapter: `RenderDevice` (handle ownership, validation,
deferred destruction, frame ordering) over a `RenderBackend` trait, WGSL reflection
through naga, a command buffer, and `RecordingBackend`, a deterministic GPU-free
backend for tests.

## Error model

Errors fall into three classes, and only the last poisons the device.

* **Per-call errors.** A call that is rejected fails with its own error and changes
  nothing: `InvalidResource`, `InvalidDraw`, `StaleHandle`, `Shader`, `Unsupported`,
  `LimitExceeded`, and `Backend` (a validation error the backend raised for that
  call). The next call is judged on its own merits, so one bad authored asset
  never blocks valid ones.
* **Limit pre-validation.** `RenderCapabilities::limits` (`DeviceLimits`) carries the
  device's `max_bind_groups`, `max_bindings_per_bind_group`,
  `max_vertex_buffer_array_stride`, `max_vertex_attributes` and
  `max_uniform_buffer_binding_size`. `RenderDevice` checks them before a resource
  reaches the backend: `create_shader` checks every reflected binding (group index,
  binding number, uniform size), `create_mesh` and `update_mesh` check the vertex
  layout (stride, attribute count). A violation is `RenderError::LimitExceeded`
  naming the limit, the requested value and the maximum. Backends report the real
  device values; `RecordingBackend` reports `DeviceLimits::default()`, which equals
  wgpu's downlevel defaults, and `RecordingConfig::limits` overrides it.
* **Device loss.** `RenderError::DeviceLost` is the only error `RenderDevice` latches:
  afterwards every call returns it and the device and backend must be recreated.
  `Backend` errors are not latched.

## Tests

```text
cargo test -p engine-render
```

`tests/device.rs` covers the device contract and `tests/limits.rs` the limit
checks, both over the recording backend.
