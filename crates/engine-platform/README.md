# Engine platform

Explicit host capability reporting and native packaging validation without a
window, device or SDK dependency. `HostPlatform::current()` identifies the
compiled target. `capabilities()` describes implemented engine host paths;
`require(Capability)` returns an explicit error for missing mobile, browser and
other host integrations. This does not probe available hardware.

`PackageHost::current()` validates native desktop packaging and supplies
serializable platform/architecture/launcher metadata. It identifies the packager,
not the binary ABI of an externally supplied executable.

See [platforms](../../docs/platforms.md) for the CLI, supported paths and limits.
Run `cargo test -p engine-platform` and
`cargo clippy -p engine-platform --all-targets -- -D warnings`.
