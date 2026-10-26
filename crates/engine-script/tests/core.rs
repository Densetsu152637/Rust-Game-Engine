//! The script core: values, registry validation, budgets, codes.
use engine_script::{
    Budget, Capabilities, EntityRef, FnSig, HostRegistry, NativeCx, ScriptError, Trap, Value,
    ValueType, codes, valid_function_name,
};
use std::collections::BTreeSet;

#[derive(Default)]
struct World {
    invoked: u32,
}

fn bump(cx: &mut NativeCx<'_, World>, _args: &[Value]) -> Result<Value, ScriptError> {
    cx.world.invoked += 1;
    Ok(Value::Unit)
}

fn registry() -> HostRegistry<World> {
    let mut registry = HostRegistry::new();
    registry
        .register(
            "game.hit",
            FnSig::new(
                &[
                    ("target", ValueType::Entity),
                    ("amount", ValueType::Float),
                    ("label", ValueType::Str),
                    ("extra", ValueType::Json),
                ],
                None,
            ),
            bump,
        )
        .unwrap();
    registry
}

fn call(args: &[Value]) -> (Result<Value, ScriptError>, u32) {
    let registry = registry();
    let (id, _) = registry.resolve("game.hit").unwrap();
    let mut world = World::default();
    let caps = Capabilities::default();
    let mut cx = NativeCx {
        world: &mut world,
        subject: EntityRef(1),
        caps: &caps,
    };
    let result = registry.invoke(id, &mut cx, args);
    (result, world.invoked)
}

fn good() -> Vec<Value> {
    vec![
        Value::Entity(EntityRef(3)),
        Value::Float(1.0),
        Value::str("ok"),
        Value::json(serde_json::json!({"a": 1})),
    ]
}

#[test]
fn invalid_arguments_never_reach_the_native_function() {
    assert_eq!(
        call(&good()),
        (Ok(Value::Unit), 1),
        "a valid call runs once"
    );
    let mut bad = Vec::new();
    let mut with = |index: usize, value: Value| {
        let mut args = good();
        args[index] = value;
        bad.push(args);
    };
    with(0, Value::Int(3)); // wrong type
    with(1, Value::Float(f64::NAN));
    with(1, Value::Float(f64::INFINITY));
    with(2, Value::str(&"x".repeat(4097))); // oversize string
    with(3, Value::json(serde_json::json!("y".repeat(5000)))); // oversize json string
    let mut deep = serde_json::json!(1);
    for _ in 0..20 {
        deep = serde_json::json!([deep]);
    }
    with(3, Value::json(deep)); // too deep
    bad.push(good()[..3].to_vec()); // arity
    for args in bad {
        let (result, invoked) = call(&args);
        assert_eq!(result.unwrap_err().code, codes::HOST_ARGUMENT);
        assert_eq!(
            invoked, 0,
            "the recorder proves the native fn was not invoked"
        );
    }
}

#[test]
fn registration_rules() {
    let mut registry: HostRegistry<World> = HostRegistry::new();
    let sig = || FnSig::new(&[], None);
    assert!(registry.register("game.a", sig(), bump).is_ok());
    assert_eq!(
        registry.register("game.a", sig(), bump).unwrap_err().code,
        codes::FUNCTION_DUPLICATE
    );
    for bad in [
        "engine.self",
        "nodots",
        "Game.x",
        "game.",
        "a.b.c.d.e",
        "game.bad-name",
    ] {
        assert_eq!(
            registry.register(bad, sig(), bump).unwrap_err().code,
            codes::FUNCTION_INVALID,
            "{bad}"
        );
    }
    assert!(
        registry
            .register("game.zero_cost", FnSig::new(&[], None).cost(0), bump)
            .is_err()
    );
    assert!(
        registry
            .register("game.pure_void", FnSig::new(&[], None).pure(), bump)
            .is_err(),
        "pure functions return a value"
    );
    assert!(
        registry
            .register_builtin("engine.self", sig(), bump)
            .is_ok()
    );
    assert!(registry.register_builtin("game.x", sig(), bump).is_err());
    assert!(valid_function_name("game.hit_all"));
}

#[test]
fn budgets_and_traps() {
    let mut budget = Budget::with_fuel(150);
    assert!(budget.charge(100).is_ok());
    assert_eq!(budget.charge(51), Err(Trap::Fuel));
    assert_eq!(budget.fuel, 0);
    assert_eq!(Budget::with_fuel(1).fuel, 100, "authored fuel is clamped");
    assert_eq!(Budget::with_fuel(u64::MAX).fuel, 1_000_000);
    assert_eq!(Trap::Memory.code(), codes::MEMORY_LIMIT);
    assert_eq!(
        Trap::Capability("x".into()).code(),
        codes::CAPABILITY_DENIED
    );
}

#[test]
fn codes_are_unique_and_namespaced() {
    let all: BTreeSet<_> = codes::ALL.iter().collect();
    assert_eq!(all.len(), codes::ALL.len());
    assert!(codes::ALL.iter().all(|c| c.starts_with("script.")));
}

#[test]
fn value_conversions() {
    for ty in ValueType::ALL {
        assert_eq!(ValueType::from_port_type(&ty.port_type()), Some(ty));
        assert_eq!(ValueType::parse(ty.name()), Some(ty));
    }
    assert_eq!(
        Value::from_json(ValueType::Vec3, &serde_json::json!([1, 2, 3])),
        Some(Value::Vec3([1.0, 2.0, 3.0]))
    );
    assert_eq!(
        Value::from_json(ValueType::Int, &serde_json::json!(1.5)),
        None
    );
    assert_eq!(
        Value::from_json(ValueType::Entity, &serde_json::json!(1)),
        None
    );
    assert!(Value::Float(1.0).byte_size() >= 16);
}
