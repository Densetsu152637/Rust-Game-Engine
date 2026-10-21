# Engine tooling

The native compiler builds external Rust gameplay and engine crates together into
an executable generation. `CompileRequest.main_source` is native Rust supplied by
the engine runner; it calls the project's `gameplay::configure` function directly.
The game can use public Rust engine types, arbitrary components and render passes.
There is no shared-library Rust ABI, command-language interpreter, or serialized
gameplay interface. Native authored code has the user's normal process privileges.

`NativeCompiler::compile` invokes the selected Cargo/rustc executables, captures
real Cargo JSON diagnostics, and returns a copied executable at a unique path.
The compiler explicitly resolves a generation Cargo.lock using Cargo metadata,
then builds with `--locked`. The SHA-256 generation fingerprint includes this
resolved lock, dirty files from every local Cargo package (including external path
dependencies), workspace manifests/lock/compiler configuration, compiler identity,
entry source, selected target/features/profile and relevant compiler environment.
Git revision alone is insufficient. Generated build/cache and `cmake-build-*`
directories are excluded. Source changes during compilation reject the candidate;
source symlinks are rejected rather than followed outside roots. The generated
manifest/lock and generation metadata remain beside the executable for diagnosis.

Cargo dependency output is cached, while generation binaries remain separate so
an old Windows process never locks the next candidate's executable. Failed or
cancelled compilation preserves the last successful artifact. GenerationManager
compiles and prepares a candidate behind a startup barrier before stopping the
previous process. No candidate gameplay runs before clean old-generation teardown.
Compile/readiness/cancellation failure preserves the old process and artifact.
Running games receive an owned `ENGINE_GENERATION_STOP_FILE`; runtime adapters
check it at phase boundaries to allow normal ECS/audio/GPU teardown. After a bounded
grace interval, shutdown force-terminates the owned Windows job or Unix process
group. A forced or failed old shutdown prevents candidate activation. If releasing
the candidate startup pipe fails after old teardown, the error is reported and no
game remains running; old game state cannot be resurrected. Native job ownership
also terminates Cargo's compiler/build-script descendants on cancellation without
process enumeration or shell kill helpers. A normal reload restarts game state;
this API makes no claim of in-memory state migration across generations.

Run `cargo test -p engine-tooling` from the engine workspace. Compiler integration
tests build actual external fixture crates, execute their generated binaries,
introduce compiler errors, change dirty engine sources/locks/features, and check
child failure/cancellation and previous-generation retention.

CLI, MCP, asset/package operations and the finalized runtime entry adapter follow
after the engine-project and desktop runner contracts are integrated. They are
not implemented by this compiler milestone.
