//! Restricted Rhai configuration for text scripts.

use rhai::module_resolvers::DummyModuleResolver;
use rhai::{AST, CallFnOptions, Dynamic, Engine, Module, Scope, Shared};

/// Creates a parser and evaluator with no ambient module resolution or output.
pub fn engine() -> Engine {
    let mut engine = Engine::new_raw();
    engine.set_module_resolver(DummyModuleResolver::new());
    engine.disable_symbol("import");
    engine.disable_symbol("eval");
    engine.on_print(|_| {});
    engine.on_debug(|_, _, _| {});

    // Qualified namespaces need the module feature. This small registration is
    // replaced with the generated, typed host table after the API probe builds.
    let mut module = Module::new();
    module.set_native_fn("probe", |value: i64| {
        Ok::<_, Box<rhai::EvalAltResult>>(value)
    });
    engine.register_static_module("engine", Shared::new(module));
    engine
}

/// Proves that function bodies can run without evaluating top-level statements
/// and that a state value can be bound to `this` across calls.
pub fn call_probe(ast: &AST, state: &mut Dynamic) -> Result<(), Box<rhai::EvalAltResult>> {
    let engine = engine();
    let mut scope = Scope::new();
    let options = CallFnOptions::new().bind_this_ptr(state).eval_ast(false);
    engine.call_fn_with_options::<()>(options, &mut scope, ast, "on_update", (1_i64,))
}
