//! Restricted Rhai configuration for text scripts.

use rhai::Engine;
use rhai::module_resolvers::DummyModuleResolver;

mod instance;
mod program;
pub use instance::{HostBinding, TextInstance};
pub use program::{MAX_SOURCE_BYTES, TextProgram};

/// Creates a parser and evaluator with no ambient module resolution or output.
pub(crate) fn engine() -> Engine {
    let mut engine = Engine::new_raw();
    engine.set_module_resolver(DummyModuleResolver::new());
    engine.disable_symbol("import");
    engine.disable_symbol("eval");
    engine.on_print(|_| {});
    engine.on_debug(|_, _, _| {});

    engine.set_max_call_levels(16);
    engine.set_max_expr_depths(64, 32);
    engine.set_max_string_size(crate::MAX_STRING_BYTES);
    engine.set_max_array_size(512);
    engine.set_max_map_size(512);
    engine.set_max_variables(128);
    engine.set_max_functions(128);
    engine.set_max_modules(0);
    engine.set_max_strings_interned(1024);
    engine
}
