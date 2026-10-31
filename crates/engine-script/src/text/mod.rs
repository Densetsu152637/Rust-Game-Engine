//! Restricted Rhai configuration for text scripts.

use rhai::Engine;
use rhai::module_resolvers::DummyModuleResolver;
use rhai::packages::{
    ArithmeticPackage, BasicArrayPackage, BasicIteratorPackage, BasicMapPackage, LogicPackage,
    Package,
};

mod instance;
mod program;
pub use instance::{HostBinding, TextInstance};
pub use program::{MAX_EVENTS, MAX_SOURCE_BYTES, TextProgram};

/// Native stack of the scoped worker thread that evaluates one handler call. Rhai
/// recurses on it for nested calls, expressions and (when cloning, measuring or
/// dropping) nested arrays and maps, which its size limits do not bound by depth:
/// 511 alternating array/map levels need between 1 and 2 MiB in a debug build, so
/// 1 MiB overflowed; 8 MiB keeps a 4x margin. Address space only, committed lazily.
pub const WORKER_STACK_BYTES: usize = 8 * 1024 * 1024;

/// Deepest script call stack (`Engine::set_max_call_levels`).
pub const MAX_CALL_LEVELS: usize = 16;
/// Expression nesting limits: `(global, inside functions)`.
pub const MAX_EXPR_DEPTHS: (usize, usize) = (64, 32);
/// Longest array a script may build.
pub const MAX_ARRAY_SIZE: usize = 512;
/// Largest object map a script may build.
pub const MAX_MAP_SIZE: usize = 512;
/// Most variables a script may declare in one scope chain.
pub const MAX_VARIABLES: usize = 128;
/// Most functions one source may define.
pub const MAX_FUNCTIONS: usize = 128;
/// Most distinct interned strings.
pub const MAX_INTERNED_STRINGS: usize = 1024;

/// Creates a parser and evaluator with no ambient module resolution or output.
pub(crate) fn engine() -> Engine {
    let mut engine = Engine::new_raw();
    ArithmeticPackage::new().register_into_engine(&mut engine);
    BasicIteratorPackage::new().register_into_engine(&mut engine);
    BasicArrayPackage::new().register_into_engine(&mut engine);
    BasicMapPackage::new().register_into_engine(&mut engine);
    LogicPackage::new().register_into_engine(&mut engine);
    engine.set_module_resolver(DummyModuleResolver::new());
    engine.disable_symbol("import");
    engine.disable_symbol("eval");
    engine.on_print(|_| {});
    engine.on_debug(|_, _, _| {});

    engine.set_max_call_levels(MAX_CALL_LEVELS);
    engine.set_max_expr_depths(MAX_EXPR_DEPTHS.0, MAX_EXPR_DEPTHS.1);
    engine.set_max_string_size(crate::MAX_STRING_BYTES);
    engine.set_max_array_size(MAX_ARRAY_SIZE);
    engine.set_max_map_size(MAX_MAP_SIZE);
    engine.set_max_variables(MAX_VARIABLES);
    engine.set_max_functions(MAX_FUNCTIONS);
    engine.set_max_modules(0);
    engine.set_max_strings_interned(MAX_INTERNED_STRINGS);
    engine
}
