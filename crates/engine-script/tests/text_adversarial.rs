//! Adversarial tests of the Rhai text backend (design section 5, slice T2).
//!
//! Every test feeds hostile source or payloads to a `TextInstance` through a
//! recording host and asserts a deterministic, coded trap: never a host crash, a
//! stack overflow, a hang or a call into native code with invalid data. Each case
//! runs on a 1 MiB-stack thread with a watchdog, so a regression fails the test
//! instead of hanging or aborting the whole binary quietly.
#![cfg(feature = "text")]

use engine_script::{
    Budget, Fault, FnId, FnSig, ScriptHost, ScriptInstance, Trap, Trigger, Value, ValueType, codes,
    text::{HostBinding, MAX_SOURCE_BYTES, TextInstance, TextProgram},
};
use std::{
    sync::{Arc, mpsc},
    thread,
    time::Duration,
};

/// Records what reached the host; replies with a unit value.
#[derive(Default)]
struct Recorder {
    calls: Vec<(u32, Vec<Value>)>,
    logs: Vec<String>,
    emits: usize,
}

impl ScriptHost for Recorder {
    fn call(&mut self, function: FnId, args: &[Value]) -> Result<Value, Trap> {
        self.calls.push((function.0, args.to_vec()));
        Ok(Value::Unit)
    }
    fn emit(
        &mut self,
        _name: &str,
        _payload: Value,
        _target: Option<engine_script::EntityRef>,
    ) -> Result<(), Trap> {
        self.emits += 1;
        Ok(())
    }
    fn log(&mut self, message: &str) {
        self.logs.push(message.to_owned());
    }
    fn subject(&self) -> engine_script::EntityRef {
        engine_script::EntityRef(1)
    }
}

fn bindings() -> Vec<HostBinding> {
    let bind = |name: &str, id: u32, sig: FnSig| HostBinding {
        name: name.to_owned(),
        id: FnId(id),
        sig,
    };
    vec![
        bind("game.ping", 0, FnSig::new(&[("n", ValueType::Int)], None)),
        bind(
            "game.scale",
            1,
            FnSig::new(&[("x", ValueType::Float)], Some(ValueType::Float)),
        ),
        bind("game.name", 2, FnSig::new(&[("s", ValueType::Str)], None)),
        bind("game.blob", 3, FnSig::new(&[("j", ValueType::Json)], None)),
        bind(
            "game.touch",
            4,
            FnSig::new(&[("e", ValueType::Entity)], None),
        ),
        bind("game.move", 5, FnSig::new(&[("v", ValueType::Vec3)], None)),
    ]
}

/// Runs `body` on a 1 MiB stack with a watchdog. A panic in `body` is re-raised
/// with its own message.
fn guarded<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
    let (sender, receiver) = mpsc::channel();
    let handle = thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(move || {
            let _ = sender.send(body());
        })
        .expect("spawn test thread");
    match receiver.recv_timeout(Duration::from_secs(120)) {
        Ok(value) => value,
        Err(mpsc::RecvTimeoutError::Disconnected) => match handle.join() {
            Err(panic) => std::panic::resume_unwind(panic),
            Ok(()) => panic!("the test thread ended without a result"),
        },
        Err(mpsc::RecvTimeoutError::Timeout) => {
            panic!("the script run neither finished nor trapped within the watchdog")
        }
    }
}

struct Outcome {
    result: Result<(), Fault>,
    host: Recorder,
    budget: Budget,
}

fn run_with(source: Vec<u8>, trigger: OwnedTrigger, fuel: u64) -> Outcome {
    guarded(move || {
        let program = Arc::new(TextProgram::compile(&source).expect("source compiles"));
        let mut instance = TextInstance::new(program, bindings()).expect("instance");
        let mut host = Recorder::default();
        let mut budget = Budget::with_fuel(fuel);
        let result = match &trigger {
            OwnedTrigger::Start => instance.handle(&Trigger::Start, &mut host, &mut budget),
            OwnedTrigger::Update => {
                instance.handle(&Trigger::Update { dt: 0.1 }, &mut host, &mut budget)
            }
            OwnedTrigger::Event(name, payload) => instance.handle(
                &Trigger::Event {
                    name,
                    payload,
                    source: None,
                },
                &mut host,
                &mut budget,
            ),
        };
        Outcome {
            result,
            host,
            budget,
        }
    })
}

enum OwnedTrigger {
    Start,
    Update,
    Event(String, serde_json::Value),
}

fn start(source: &str) -> Outcome {
    run_with(source.as_bytes().to_vec(), OwnedTrigger::Start, 10_000)
}

fn code(outcome: &Outcome) -> &'static str {
    outcome
        .result
        .as_ref()
        .expect_err("the script must trap")
        .code()
}

#[test]
fn infinite_loops_exhaust_fuel_in_every_loop_form() {
    for body in [
        "loop {}",
        "while true {}",
        "let i = 0; while i >= 0 { i += 0; }",
        "for i in 0..9223372036854775807 {}",
        "for a in 0..1000000 { for b in 0..1000000 {} }",
        "loop { let x = 1; }",
        "let n = 0; do { n += 1; } while n >= 0;",
    ] {
        let source = format!("fn on_start() {{ {body} }}");
        let outcome = start(&source);
        assert_eq!(code(&outcome), codes::FUEL_EXHAUSTED, "{body}");
        assert_eq!(outcome.budget.fuel, 0, "{body}: the whole budget was spent");
    }
}

#[test]
fn fuel_exhaustion_is_deterministic() {
    let run = || {
        let outcome = run_with(
            b"fn on_update(dt) { let n = 0; loop { n += 1; } }".to_vec(),
            OwnedTrigger::Update,
            5_000,
        );
        let fault = outcome.result.unwrap_err();
        (fault.code(), fault.site.map(|site| site.to_string()))
    };
    assert_eq!(run(), run());
}

#[test]
fn host_call_loops_are_bounded_by_the_declared_cost() {
    let outcome = start("fn on_start() { loop { game::ping(1); } }");
    assert_eq!(code(&outcome), codes::FUEL_EXHAUSTED);
    // Each call costs its declared fuel (1) plus the operations that reached it, so
    // the budget bounds the number of native calls well below the fuel.
    assert!(
        !outcome.host.calls.is_empty() && outcome.host.calls.len() <= 10_000,
        "{} native calls",
        outcome.host.calls.len()
    );
}

#[test]
fn print_floods_are_bounded_by_fuel() {
    let outcome = start(r#"fn on_start() { loop { print("x"); } }"#);
    assert_eq!(code(&outcome), codes::FUEL_EXHAUSTED);
    assert!(outcome.host.logs.len() <= 10_000);
    assert!(outcome.host.logs.iter().all(|line| line == "x"));
}

#[test]
fn unbounded_recursion_traps_at_the_call_depth_limit() {
    for body in [
        "fn f(n) { f(n + 1) } fn on_start() { f(0); }",
        "fn a(n) { b(n) } fn b(n) { a(n) } fn on_start() { a(0); }",
        "fn f(n) { 1 + f(n + 1) } fn on_start() { f(0); }",
        "fn f(n) { if n >= 0 { return f(n + 1); } } fn on_start() { f(0); }",
        r#"fn f() { call(Fn("f")); } fn on_start() { f(); }"#,
        r#"fn f() { let p = Fn("f"); p.call(); } fn on_start() { f(); }"#,
    ] {
        let outcome = start(body);
        assert_eq!(code(&outcome), codes::CALL_DEPTH, "{body}");
    }
}

/// `(1 + (1 + ... 1))` nested `depth` levels.
fn nested(depth: usize) -> String {
    format!("{}1{}", "(1 + ".repeat(depth), ")".repeat(depth))
}

/// The deepest nesting a function body may use before the parser rejects it.
fn deepest_body(template: impl Fn(usize) -> String) -> usize {
    (1..200)
        .take_while(|depth| TextProgram::compile(template(*depth).as_bytes()).is_ok())
        .last()
        .expect("depth 1 compiles")
}

#[test]
fn the_deepest_permitted_script_runs_on_a_one_mebibyte_stack() {
    // The call-level limit with every frame evaluating the deepest expression the
    // parser accepts. The 1 MiB worker stack must hold it (debug and release).
    let template = |depth: usize| {
        format!(
            "fn dive(n) {{ if n == 0 {{ return 0; }} let x = {}; dive(n - 1) }}              fn on_start() {{ dive(14); game::ping(1); }}",
            nested(depth)
        )
    };
    let depth = guarded(move || deepest_body(template));
    assert!(depth >= 10, "the expression depth limit is usable: {depth}");
    let source = template(depth);
    let outcome = run_with(source.into_bytes(), OwnedTrigger::Start, 1_000_000);
    outcome.result.as_ref().unwrap_or_else(|fault| {
        panic!("a script inside every limit must run: {fault}");
    });
    assert_eq!(outcome.host.calls.len(), 1);
    // The same shape one frame deeper than the limit traps instead of overflowing.
    let source = format!(
        "fn dive(n) {{ if n == 0 {{ return 0; }} let x = {}; dive(n - 1) }}          fn on_start() {{ dive(100); }}",
        nested(depth)
    );
    assert_eq!(code(&start(&source)), codes::CALL_DEPTH);
}

#[test]
fn over_deep_expressions_never_reach_the_evaluator() {
    let source = |depth: usize| {
        format!(
            "fn on_start() {{ let x = {}1{}; }}",
            "(".repeat(depth),
            ")".repeat(depth)
        )
    };
    guarded(move || {
        for depth in [200usize, 5_000, 100_000] {
            let error = TextProgram::compile(source(depth).as_bytes())
                .err()
                .expect("over-deep source is rejected at parse time");
            assert_eq!(error.code, codes::TEXT_PARSE, "depth {depth}");
        }
        for nesting in ["[", "#{a:", "!", "-", "if true {"] {
            let source = format!("fn on_start() {{ let x = {}1; }}", nesting.repeat(20_000));
            let error = TextProgram::compile(source.as_bytes())
                .err()
                .expect("nested source is rejected");
            assert_eq!(error.code, codes::TEXT_PARSE, "{nesting}");
        }
    });
}

#[test]
fn string_array_and_map_growth_traps_with_the_memory_code() {
    for body in [
        r#"let s = "x"; loop { s += s; }"#,
        r#"let s = "x"; for i in 0..100 { s = s + s; }"#,
        "let a = []; loop { a.push(1); }",
        "let a = [1]; loop { a += a; }",
        "let m = #{}; loop { m.mixin(#{ a: m }); }",
        // Nested amplification: every level doubles the total element count.
        "let a = [1]; for i in 0..30 { a = [a, a]; }",
        "let a = [0]; for i in 0..500 { a.push(a); }",
        "let m = #{k: 1}; for i in 0..30 { m = #{a: m, b: m}; }",
    ] {
        let source = format!("fn on_start() {{ {body} }}");
        let outcome = start(&source);
        assert_eq!(code(&outcome), codes::MEMORY_LIMIT, "{body}");
    }
}

#[test]
fn literals_and_definitions_beyond_the_limits_fail_at_parse_time() {
    let big_string = format!("fn on_start() {{ let s = \"{}\"; }}", "x".repeat(5_000));
    let big_array = format!(
        "fn on_start() {{ let a = [{}]; }}",
        vec!["1"; 600].join(",")
    );
    let big_map = format!(
        "fn on_start() {{ let m = #{{{}}}; }}",
        (0..600)
            .map(|n| format!("k{n}: 1"))
            .collect::<Vec<_>>()
            .join(",")
    );
    let many_functions: String = (0..200).map(|n| format!("fn f{n}() {{}} ")).collect();
    guarded(move || {
        for source in [big_string, big_array, big_map, many_functions] {
            let error = TextProgram::compile(source.as_bytes())
                .err()
                .unwrap_or_else(|| panic!("must be rejected: {}", &source[..60]));
            assert_eq!(error.code, codes::TEXT_PARSE);
        }
    });
}

#[test]
fn too_many_variables_trap_with_the_memory_code() {
    let lets: String = (0..200).map(|n| format!("let v{n} = 1;")).collect();
    let outcome = start(&format!("fn on_start() {{ {lets} }}"));
    assert_eq!(code(&outcome), codes::MEMORY_LIMIT);
}

#[test]
fn growing_persistent_state_past_the_limit_traps() {
    // Each value is under the string limit; together they exceed the string budget
    // of one container (rhai counts nested strings), long before the 64 KiB state
    // check that runs when a handler returns.
    let filler = "x".repeat(4_000);
    let assignments: String = (0..24).map(|n| format!("this.k{n} = big;")).collect();
    let source = format!("fn on_start() {{ let big = \"{filler}\"; {assignments} }}");
    let outcome = start(&source);
    assert_eq!(code(&outcome), codes::MEMORY_LIMIT);
}

#[test]
fn import_eval_and_module_attempts_are_rejected_or_trap() {
    guarded(|| {
        for source in [
            r#"import "std" as s; fn on_start() {}"#,
            r#"fn on_start() { import "x" as y; }"#,
            r#"fn on_start() { eval("1 + 1"); }"#,
            r#"fn on_start() { let e = eval; }"#,
        ] {
            let error = TextProgram::compile(source.as_bytes())
                .err()
                .unwrap_or_else(|| panic!("must be rejected: {source}"));
            assert_eq!(error.code, codes::TEXT_PARSE, "{source}");
        }
    });
    // Namespaces that are not host functions do not exist at run time.
    for body in [
        r#"std::fs::read_to_string("secret.txt");"#,
        r#"os::exec("cmd");"#,
        r#"fs::read("x");"#,
        r#"process::exit(1);"#,
        r#"net::connect("127.0.0.1");"#,
        r#"timestamp();"#,
        r#"let t = Instance::now();"#,
        r#"sleep(10.0);"#,
        r#"rand();"#,
        r#"engine::exec("calc");"#,
        r#"engine::emit = 1;"#,
        r#"global::secret = 1;"#,
    ] {
        let outcome = start(&format!("fn on_start() {{ {body} }}"));
        assert_eq!(code(&outcome), codes::HOST_FAILED, "{body}");
        assert!(outcome.host.calls.is_empty(), "{body}");
    }
}

#[test]
fn top_level_statements_are_never_executed() {
    let outcome = start(
        r#"
        game::ping(666);
        print("top level ran");
        loop {}
        fn on_start() {}
    "#,
    );
    // Compilation keeps functions only, so the top-level loop never runs.
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    // Module statements are inert for the same reason: they are never evaluated.
    let inert = start(r#"export let x = 1; fn on_start() { game::ping(1); }"#);
    assert!(inert.result.is_ok(), "{:?}", inert.result);
    assert_eq!(inert.host.calls.len(), 1);
    assert!(outcome.host.calls.is_empty());
    assert!(outcome.host.logs.is_empty());
}

#[test]
fn unknown_functions_and_wrong_arities_trap_without_native_calls() {
    for body in [
        "game::missing(1);",
        "engine::missing();",
        "game::ping();",
        "game::ping(1, 2);",
        "no_such_function(1);",
        "game::ping.call(1);",
        "let f = Fn(\"nope\"); call(f);",
    ] {
        let outcome = start(&format!("fn on_start() {{ {body} }}"));
        assert_eq!(code(&outcome), codes::HOST_FAILED, "{body}");
        assert!(outcome.host.calls.is_empty(), "{body}");
    }
}

#[test]
fn invalid_host_arguments_never_reach_the_native_function() {
    let long_name = "x".repeat(4_096);
    for (body, expected) in [
        (r#"game::ping("seven");"#, codes::HOST_ARGUMENT),
        ("game::ping(1.5);", codes::HOST_ARGUMENT),
        ("game::ping(true);", codes::HOST_ARGUMENT),
        ("game::ping(());", codes::HOST_ARGUMENT),
        ("game::scale(1);", codes::HOST_ARGUMENT),
        ("game::scale(0.0 / 0.0);", codes::HOST_ARGUMENT),
        ("game::scale(1.0e308 * 10.0);", codes::HOST_ARGUMENT),
        ("game::name(5);", codes::HOST_ARGUMENT),
        ("game::touch(42);", codes::HOST_ARGUMENT),
        ("game::touch(true);", codes::HOST_ARGUMENT),
        ("game::touch([1, 2, 3]);", codes::HOST_ARGUMENT),
        ("game::touch(\"engine\");", codes::HOST_ARGUMENT),
        ("game::move([1.0, 2.0]);", codes::HOST_ARGUMENT),
        ("game::move([1.0, 2.0, \"z\"]);", codes::HOST_ARGUMENT),
        ("game::move([0.0 / 0.0, 0.0, 0.0]);", codes::HOST_ARGUMENT),
        ("game::move([1.0e300, 0.0, 0.0]);", codes::HOST_ARGUMENT),
        ("game::blob(|x| x);", codes::HOST_ARGUMENT),
        ("game::blob(Fn(\"on_start\"));", codes::HOST_ARGUMENT),
    ] {
        let outcome = start(&format!("fn on_start() {{ {body} }}"));
        assert_eq!(code(&outcome), expected, "{body}");
        assert!(
            outcome.host.calls.is_empty(),
            "native fn was invoked: {body}"
        );
    }
    // Boundary sizes: a JSON array of 512 items exceeds the bridge's node budget.
    let outcome =
        start("fn on_start() { let a = []; for i in 0..512 { a.push(1); } game::blob(a); }");
    assert_eq!(code(&outcome), codes::MEMORY_LIMIT);
    assert!(outcome.host.calls.is_empty());
    // A 4096-byte string is exactly the limit and passes.
    let outcome = start(&format!("fn on_start() {{ game::name(\"{long_name}\"); }}"));
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(outcome.host.calls.len(), 1);
    // `()` is the only way to name "no entity"; it reaches the host as handle 0.
    let outcome = start("fn on_start() { game::touch(()); }");
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(
        outcome.host.calls,
        [(4, vec![Value::Entity(engine_script::EntityRef(0))])]
    );
    // Valid calls pass through with the exact values.
    let outcome = start(
        "fn on_start() { game::ping(7); game::move([1.0, 2.0, 3.0]); \
         game::blob(#{a: [1, 2.5, \"s\", true, ()]}); }",
    );
    assert!(outcome.result.is_ok(), "{:?}", outcome.result);
    assert_eq!(outcome.host.calls[0], (0, vec![Value::Int(7)]));
    assert_eq!(
        outcome.host.calls[1],
        (5, vec![Value::Vec3([1.0, 2.0, 3.0])])
    );
    assert_eq!(outcome.host.calls.len(), 3);
}

#[test]
fn hostile_event_payloads_trap_before_the_handler_runs() {
    let source = br#"
        const events = ["game.hit"];
        fn on_event(name, payload, source) { game::ping(1); }
    "#
    .to_vec();
    let mut deep = serde_json::json!(1);
    for _ in 0..200 {
        deep = serde_json::json!([deep]);
    }
    let wide = serde_json::Value::Array(vec![serde_json::json!(true); 600]);
    let mut keys = serde_json::Map::new();
    for n in 0..600 {
        keys.insert(format!("k{n}"), serde_json::json!(n));
    }
    let long = serde_json::Value::String("x".repeat(100_000));
    for payload in [deep, wide, serde_json::Value::Object(keys), long] {
        let outcome = run_with(
            source.clone(),
            OwnedTrigger::Event("game.hit".to_owned(), payload),
            10_000,
        );
        assert_eq!(code(&outcome), codes::MEMORY_LIMIT);
        assert!(outcome.host.calls.is_empty());
    }
    // A small payload is delivered.
    let outcome = run_with(
        source,
        OwnedTrigger::Event("game.hit".to_owned(), serde_json::json!({"n": 1})),
        10_000,
    );
    assert!(outcome.result.is_ok());
    assert_eq!(outcome.host.calls.len(), 1);
}

#[test]
fn oversize_and_malformed_sources_are_rejected_without_parsing_cost() {
    guarded(|| {
        let cases: Vec<(Vec<u8>, &str)> = vec![
            (vec![b' '; MAX_SOURCE_BYTES + 1], "oversize"),
            (vec![0xff, 0xfe, b'f', b'n'], "not utf-8"),
            (b"fn on_start( {".to_vec(), "truncated"),
            (b"fn on_start() { let x = ".to_vec(), "unterminated"),
            (b"\"unterminated".to_vec(), "unterminated string"),
            (vec![0; 4_096], "nul bytes"),
            (
                "fn on_start() { let \u{202e}x = 1; }".as_bytes().to_vec(),
                "bidi",
            ),
        ];
        for (source, label) in cases {
            let error = TextProgram::compile(&source)
                .err()
                .unwrap_or_else(|| panic!("{label} must be rejected"));
            assert_eq!(error.code, codes::TEXT_PARSE, "{label}");
        }
        // The largest legal source still compiles.
        let mut source = b"fn on_start() {} //".to_vec();
        source.resize(MAX_SOURCE_BYTES, b'x');
        TextProgram::compile(&source).expect("a source at the limit compiles");
        // Handler contract violations.
        for (source, label) in [
            ("fn on_start(a) {}", "on_start arity"),
            ("fn on_update() {}", "on_update arity"),
            ("fn on_event(a, b) {}", "on_event arity"),
            (
                "const events = [\"a.b\"]; fn on_start() {}",
                "events without on_event",
            ),
        ] {
            let error = TextProgram::compile(source.as_bytes())
                .err()
                .unwrap_or_else(|| panic!("{label} must be rejected"));
            assert_eq!(error.code, codes::TEXT_HANDLER, "{label}");
        }
        for (source, label) in [
            ("const events = 5; fn on_event(a, b, c) {}", "not an array"),
            ("const events = [5]; fn on_event(a, b, c) {}", "not strings"),
            (
                "const events = [\"A B\"]; fn on_event(a, b, c) {}",
                "bad name",
            ),
            (
                "const events = [\"a.b\", \"a.b\"]; fn on_event(a, b, c) {}",
                "duplicate",
            ),
        ] {
            let error = TextProgram::compile(source.as_bytes())
                .err()
                .unwrap_or_else(|| panic!("{label} must be rejected"));
            assert_eq!(error.code, codes::TEXT_EVENTS, "{label}");
        }
        let many: String = (0..65).map(|n| format!("\"e.n{n}\",")).collect();
        let source = format!("const events = [{many}]; fn on_event(a, b, c) {{}}");
        assert_eq!(
            TextProgram::compile(source.as_bytes()).err().unwrap().code,
            codes::TEXT_EVENTS
        );
    });
}

#[test]
fn timer_misuse_traps_deterministically() {
    for (body, expected) in [
        (r#"engine::after(-1.0, "t");"#, codes::HOST_ARGUMENT),
        (r#"engine::after(3601.0, "t");"#, codes::HOST_ARGUMENT),
        (r#"engine::after(0.0 / 0.0, "t");"#, codes::HOST_ARGUMENT),
        (r#"engine::after(1.0, "missing");"#, codes::TEXT_HANDLER),
        (r#"engine::after(1.0, "takes_arg");"#, codes::TEXT_HANDLER),
        (r#"engine::after(1.0, "on_event");"#, codes::TEXT_HANDLER),
        (r#"engine::after(1, "t");"#, codes::HOST_FAILED),
        (
            r#"for i in 0..65 { engine::after(1.0, "t"); }"#,
            codes::MEMORY_LIMIT,
        ),
    ] {
        let source = format!(
            "fn t() {{}} fn takes_arg(x) {{}} fn on_event(a, b, c) {{}} \
             fn on_start() {{ {body} }}"
        );
        let outcome = start(&source);
        assert_eq!(code(&outcome), expected, "{body}");
    }
}

#[test]
fn timer_capacity_is_exact_and_released_by_due_timers() {
    guarded(|| {
        let source = br#"
            fn t() {}
            fn refill() { for i in 0..64 { engine::after(5.0, "t"); } }
            fn on_start() { for i in 0..64 { engine::after(0.5, "t"); } }
            fn on_update(dt) {}
            fn one_more() { engine::after(1.0, "t"); }
        "#;
        let program = Arc::new(TextProgram::compile(source).unwrap());
        let mut instance = TextInstance::new(program, vec![]).unwrap();
        let mut host = Recorder::default();
        instance
            .handle(&Trigger::Start, &mut host, &mut Budget::default())
            .expect("64 pending timers are allowed");
        assert_eq!(instance.debug().pending, 64);
        // Nothing is due yet, so the pending set stays full.
        instance
            .handle(
                &Trigger::Update { dt: 0.1 },
                &mut host,
                &mut Budget::default(),
            )
            .unwrap();
        assert_eq!(instance.debug().pending, 64);
        // All 64 become due together and are consumed by the update.
        instance
            .handle(
                &Trigger::Update { dt: 1.0 },
                &mut host,
                &mut Budget::default(),
            )
            .unwrap();
        assert_eq!(instance.debug().pending, 0);

        // 64 due timers whose callbacks each schedule a replacement: the old timers no
        // longer count, so every replacement is accepted.
        let source = br#"
            fn again() { engine::after(5.0, "again"); }
            fn on_start() { for i in 0..64 { engine::after(0.0, "again"); } }
        "#;
        let program = Arc::new(TextProgram::compile(source).unwrap());
        let mut instance = TextInstance::new(program, vec![]).unwrap();
        instance
            .handle(&Trigger::Start, &mut host, &mut Budget::default())
            .unwrap();
        assert_eq!(instance.debug().pending, 64);
        instance
            .handle(
                &Trigger::Update { dt: 0.1 },
                &mut host,
                &mut Budget::default(),
            )
            .expect("replacements for due timers are accepted");
        assert_eq!(instance.debug().pending, 64);

        // A callback that schedules a 65th timer while 64 are pending is refused.
        let source = br#"
            fn t() {}
            fn on_start() { for i in 0..64 { engine::after(5.0, "t"); } }
            fn on_update(dt) { engine::after(5.0, "t"); }
        "#;
        let program = Arc::new(TextProgram::compile(source).unwrap());
        let mut instance = TextInstance::new(program, vec![]).unwrap();
        instance
            .handle(&Trigger::Start, &mut host, &mut Budget::default())
            .unwrap();
        let fault = instance
            .handle(
                &Trigger::Update { dt: 0.1 },
                &mut host,
                &mut Budget::default(),
            )
            .unwrap_err();
        assert_eq!(fault.code(), codes::MEMORY_LIMIT);
        assert_eq!(
            instance.debug().pending,
            64,
            "the refused timer was not added"
        );
    });
}

#[test]
fn a_trapped_run_leaves_the_instance_reusable_and_the_host_consistent() {
    guarded(|| {
        let source = br#"
            fn on_start() { this.n = 1; }
            fn on_update(dt) {
                game::ping(this.n);
                this.n += 1;
                if this.n == 3 { loop {} }
            }
        "#;
        let program = Arc::new(TextProgram::compile(source).unwrap());
        let mut instance = TextInstance::new(program, bindings()).unwrap();
        let mut host = Recorder::default();
        instance
            .handle(&Trigger::Start, &mut host, &mut Budget::default())
            .unwrap();
        for _ in 0..2 {
            let mut budget = Budget::with_fuel(2_000);
            let _ = instance.handle(&Trigger::Update { dt: 0.1 }, &mut host, &mut budget);
        }
        // Run 1 pinged 1, run 2 pinged 2 then trapped; no extra native call leaked.
        assert_eq!(
            host.calls.iter().map(|c| c.1.clone()).collect::<Vec<_>>(),
            [vec![Value::Int(1)], vec![Value::Int(2)]]
        );
    });
}

#[test]
fn identical_runs_spend_identical_fuel() {
    let run = || {
        let outcome = run_with(
            b"fn on_update(dt) { let t = 0; for i in 0..100 { t += i; } game::ping(t); }".to_vec(),
            OwnedTrigger::Update,
            10_000,
        );
        outcome.result.expect("runs");
        (outcome.budget.fuel, outcome.host.calls.clone())
    };
    assert_eq!(run(), run());
}

#[test]
fn only_the_allowlisted_packages_are_available() {
    // Packages that are NOT enabled stay unreachable: scripts cannot call
    // functions from the standard string, time, debugging or fn-pointer packages.
    for body in [
        r#"let s = "abc"; s.to_upper();"#,
        r#"let s = "abc"; s.len();"#,
        r#"timestamp();"#,
        r#"to_string(1);"#,
        r#"let a = [1,2]; a.to_string();"#,
        r#"sleep(1);"#,
    ] {
        let outcome = start(&format!("fn on_start() {{ {body} }}"));
        let fault = outcome
            .result
            .as_ref()
            .err()
            .unwrap_or_else(|| panic!("{body} must not be available"));
        assert_eq!(fault.code(), codes::HOST_FAILED, "{body}");
    }
}

#[test]
fn deeply_nested_data_cannot_overflow_the_worker_stack() {
    // Rhai's size limits count nested elements but not depth, and it clones, measures
    // and drops nested containers recursively. 511 levels of each container kind is
    // the deepest value its limits admit; it must neither overflow the worker stack
    // nor outlive the call on the host's stack.
    for body in [
        "let m = #{k: 1}; for i in 0..511 { m = #{ a: m }; }",
        "let m = #{}; for i in 0..511 { m.mixin(#{ a: m }); }",
        "let a = [0]; for i in 0..511 { a = [a]; }",
        "let a = [0]; for i in 0..511 { a = [#{ k: a }]; }",
        "for n in 0..8 { this.v = [0]; for i in 0..511 { this.v = [#{ k: this.v }]; } }",
    ] {
        let outcome = run_with(
            format!("fn on_start() {{ {body} }}").into_bytes(),
            OwnedTrigger::Start,
            1_000_000,
        );
        // Local values vanish with the call; deep persistent state is discarded and
        // reported as a memory trap.
        let code = outcome.result.as_ref().err().map(Fault::code);
        assert!(
            matches!(code, None | Some(codes::MEMORY_LIMIT)),
            "{body}: {code:?}"
        );
    }
    // One level past the element limit is refused by rhai itself.
    let outcome = start("fn on_start() { let a = [0]; for i in 0..600 { a = [a]; } }");
    assert_eq!(code(&outcome), codes::MEMORY_LIMIT);
    let outcome = start("fn on_start() { let m = #{k: 1}; for i in 0..600 { m = #{ a: m }; } }");
    assert_eq!(code(&outcome), codes::MEMORY_LIMIT);
}

#[test]
fn discarded_deep_state_leaves_the_instance_usable() {
    guarded(|| {
        let source = br#"
            fn on_start() { this.v = [0]; }
            fn on_update(dt) {
                for i in 0..511 { this.v = [#{ k: this.v }]; }
            }
            fn on_stop() { game::ping(1); }
        "#;
        let program = Arc::new(TextProgram::compile(source).unwrap());
        let mut instance = TextInstance::new(program, bindings()).unwrap();
        let mut host = Recorder::default();
        instance
            .handle(&Trigger::Start, &mut host, &mut Budget::default())
            .unwrap();
        let fault = instance
            .handle(
                &Trigger::Update { dt: 0.1 },
                &mut host,
                &mut Budget::with_fuel(1_000_000),
            )
            .unwrap_err();
        assert_eq!(fault.code(), codes::MEMORY_LIMIT);
        assert!(
            instance.debug().state_bytes < 1024,
            "the deep state was discarded"
        );
        instance
            .handle(&Trigger::Stop, &mut host, &mut Budget::default())
            .unwrap();
        assert_eq!(host.calls.len(), 1);
    });
}
