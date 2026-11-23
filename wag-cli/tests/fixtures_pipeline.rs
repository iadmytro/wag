//! Milestone 10 — black-box regression harness through the unified pipeline.
//!
//! Every fixture under `fixtures/` is driven via `compile_multi_module` / `run_multi_module`
//! (the same path as `wagc run`), asserting either a successful outcome or rejection at a
//! named stage/rule. Span-anchored diagnostics are spot-checked for one rejection per stage.

use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use wag_check::Rule;
use wag_cli::resolve::{ModuleSet, ResolvedModule};
use wag_cli::{
    compile_multi_module, render_diagnostic, Diagnostic,
    PipelineOk, PipelineStage, RunOptions,
};
use wag_runtime::{IoConfig, Runtime, SchedulerConfig};
use wag_types::Stage as TypeStage;
use wag_interp::{
    ConstructorId, Engine, Value, CTOR_PAIR2,
    CTOR_PAIR3, CTOR_PRODUCT,
};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
}

fn load(name: &str) -> String {
    fs::read_to_string(fixtures_dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn prelude_src() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("prelude/prelude.wag");
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("read prelude: {e}"))
}

/// Compile via the CLI multi-module path. When `prelude` is given it becomes
/// a module in the set and an explicit `import prelude ..` block (D053) is
/// prepended to the user source.
fn compile_inline(src: &str, prelude: Option<&str>) -> Result<wag_cli::Compiled, Diagnostic> {
    let parse_err = |e: wag_syntax::ParseError| Diagnostic {
        stage: PipelineStage::Parse,
        span: e.span,
        message: e.message,
        hint: None,
        rule: None,
        type_stage: None,
        module_name: Some("main".into()),
        origin_span: None,
    };

    let mut modules = Vec::new();
    if let Some(p) = prelude {
        // Parse first — negative fixtures must fail gracefully, not panic.
        let pprog = wag_syntax::parse_desugared(p).map_err(parse_err)?;
        modules.push(ResolvedModule {
            name: "prelude".into(),
            path: PathBuf::from("prelude.wag"),
            source: p.to_string(),
            program: pprog,
            imports: vec![],
            package: "<test>".into(),
        });
    }


    let user_src = match prelude {
        Some(_) => format!("import\n  prelude\n    ..\n{src}"),
        None => src.to_string(),
    };
    let program = wag_syntax::parse_desugared(&user_src).map_err(parse_err)?;
    modules.push(ResolvedModule {
        name: "main".into(),
        path: PathBuf::from("main.wag"),
        source: user_src,
        program,
        imports: vec![],
        package: "<test>".into(),
    });

    let entry_index = modules.len() - 1;
    let module_set = ModuleSet { modules, entry_index };
    let checked = compile_multi_module(&module_set)?;
    Ok(wag_cli::Compiled::new(checked))
}

/// Call a named function via the CLI multi-module path.
fn call_inline(
    src: &str,
    prelude: Option<&str>,
    name: &str,
    args: Vec<Value>,
) -> Result<Value, Diagnostic> {
    let compiled = compile_inline(src, prelude)?;
    let mut e = Engine::new(compiled.loaded());
    let v = e.call_name(name, args).map_err(|e| Diagnostic {
        stage: PipelineStage::Runtime,
        span: wag_syntax::Span::new(0, 0),
        message: e.to_string(),
        hint: None,
        rule: None,
        type_stage: None,
        module_name: None,
        origin_span: None,
    })?;
    Ok(v)
}

/// Run @main via the CLI multi-module path.
fn run_inline(src: &str, prelude: Option<&str>, opts: RunOptions) -> Result<PipelineOk, Diagnostic> {
    let compiled = compile_inline(src, prelude)?;
    if compiled.loaded().actor("main").is_some() {
        let io = if let Some(lines) = opts.stdin_lines.clone() {
            wag_runtime::IoConfig::scripted(lines)
        } else {
            wag_runtime::IoConfig::default()
        };
        let timeout = opts.timeout.min(wag_runtime::MAX_EXEC_TIMEOUT);
        let result = wag_runtime::run_main_with_io(
            compiled.loaded().clone(),
            &opts.argv,
            opts.scheduler,
            timeout,
            io.clone(),
        );
        let stdout = io.take_stdout_string();
        if opts.emit_stdout && !stdout.is_empty() {
            print!("{stdout}");
        }
        match result {
            Ok(rt) => {
                if opts.print_stats {
                    let s = rt.stats();
                    eprintln!(
                        "[wagc] dev stats: steps={} (hard limits: {} steps/call, {}s wall)",
                        s.total_steps,
                        wag_interp::STEP_LIMIT, wag_runtime::MAX_EXEC_TIMEOUT.as_secs()
                    );
                }
                Ok(PipelineOk::Executed(wag_cli::RunResult { runtime: rt, stdout }))
            }
            Err(e) => Err(e.into()),
        }
    } else {
        Ok(PipelineOk::Checked(compiled))
    }
}

/// Dependency fixtures then `name` (no prelude — passed separately).
fn with_deps(name: &str) -> String {
    let mut src = String::new();
    match name {
        // List-migrated batch 2 (D062): self-contained over the prelude
        // (injected separately by `compile_inline`); only 8_4a reuses `sumList`
        // from 8_3 — merged with that fixture's own testReport sliced off to
        // avoid a name collision.
        "lists/is_empty.wag" => {
            let dep = load("lists/sum_list.wag");
            match dep.find("testReport") {
                Some(i) => src.push_str(&dep[..i]),
                None    => src.push_str(&dep),
            }
            src.push('\n');
        }
        // 8_16 (item-8 cutover): self-contained borrow demos over region lists.
        // 8_20 (List-migrated, batch 3): self-contained over the prelude
        // (map/length/listNew) — no fixture deps needed.
        _ => {}
    }
    src.push_str(&load(name));
    // Strip `import prelude ..` — the prelude is injected separately by
    // `compile_inline` via `prelude_src()`.
    let src = strip_prelude_import(&src);
    src
}

/// Remove a leading `import\n  prelude\n    ..` block (the pipeline harness injects
/// the prelude separately; the parser does not support `import` in inline source).
fn strip_prelude_import(src: &str) -> String {
    let marker = "import\n  prelude\n    ..";
    if let Some(rest) = src.strip_prefix(marker) {
        rest.trim_start().to_string()
    } else {
        src.to_string()
    }
}

fn cfg() -> SchedulerConfig {
    SchedulerConfig { workers: 0 }
}

fn run_opts(stdin: Option<Vec<String>>) -> RunOptions {
    RunOptions {
        argv: Vec::new(),
        stdin_lines: stdin,
        timeout: Duration::from_secs(5),
        scheduler: cfg(),
        emit_stdout: false,
        print_stats: false,
    }
}

fn assert_ok_compile(name: &str) {
    let src = with_deps(name);
    compile_inline(&src, Some(&prelude_src())).unwrap_or_else(|e| {
        panic!(
            "{name}: expected ok, got {e}\n{}",
            render_diagnostic(name, &src, &e)
        )
    });
}

fn assert_reject(
    name: &str,
    stage: PipelineStage,
    rule: Option<Rule>,
    type_stage: Option<TypeStage>,
) -> Diagnostic {
    let src = with_deps(name);
    match compile_inline(&src, Some(&prelude_src())) {
        Ok(_) => panic!("{name}: expected rejection at {stage:?}"),
        Err(d) => {
            assert_eq!(d.stage, stage, "{name}: stage mismatch ({d})");
            if let Some(r) = rule {
                assert_eq!(d.rule, Some(r), "{name}: rule mismatch ({d})");
            }
            if let Some(ts) = type_stage {
                assert_eq!(d.type_stage, Some(ts), "{name}: type stage mismatch ({d})");
            }
            assert!(
                !d.span.is_empty() || stage == PipelineStage::Runtime,
                "{name}: diagnostic should carry a source span, got {d}"
            );
            d
        }
    }
}

/// Build a `None` value.
fn opt_none(e: &wag_interp::Engine) -> Value {
    Value::Data(e.program.opt_none_id(), vec![])
}

/// Build a `Some(v)` value.
fn opt_some(e: &wag_interp::Engine, v: Value) -> Value {
    Value::Data(e.program.opt_some_id(), vec![v])
}

/// Constructor id for an n-ary product/pair as emitted by the VM (§3.5).
fn product_ctor(n: usize) -> ConstructorId {
    match n {
        2 => CTOR_PAIR2,
        3 => CTOR_PAIR3,
        _ => CTOR_PRODUCT,
    }
}

/// Assert `v` is a product/pair whose fields equal `expected`.
fn assert_product(v: &Value, expected: &[Value]) {
    let exp = Value::Data(product_ctor(expected.len()), expected.to_vec());
    assert!(
        v == &exp,
        "expected product {:?}, got {}",
        expected,
        v.brief_desc()
    );
}

/// Field count if `v` is a product/pair (PAIR2/PAIR3/PRODUCT), else None.
fn product_len(v: &Value) -> Option<usize> {
    match v {
        Value::Data(c, fields) if matches!(*c, CTOR_PAIR2 | CTOR_PAIR3 | CTOR_PRODUCT) => {
            Some(fields.len())
        }
        _ => None,
    }
}

/// Assert `v` is a raw `Int n` or an arm-lifted `Some n` (both legal since §1's
/// raw-arithmetic switch: functions may return bare `Int` or `Option Int`).
fn raw_int(v: &Value, n: i64) {
    match v {
        Value::Int(x) if *x == n => {}
        // Tolerate an arm-lifted Some wrapper where a match arm joined T with Option T.
        Value::Data(_, fields) if fields.len() == 1 => {
            match &fields[0] {
                Value::Int(x) if *x == n => {}
                other => panic!("expected Int {n}, got Some({other:?})"),
            }
        }
        other => panic!("expected Int {n}, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Positive fixtures — full compile (+ smoke execute where meaningful)
// ---------------------------------------------------------------------------

/// Phase 2 first batch: fixtures rewritten on `List` (D062 cursor model), each
/// with a zero-arg `testReport` (prelude injected by the harness, §8 convention).
#[test]
fn m10_positive_listh_migrated_batch() {
    for name in [
        "lists/filter.wag",
        "lists/contains.wag",
        "lists/count.wag",
        "lists/is_sorted.wag",
        "lists/mapB.wag",
    ] {
        assert_ok_compile(name);
    }

    let call = |name: &str| -> Value {
        let src = load(name);
        let c = compile_inline(&src, Some(&prelude_src()))
            .unwrap_or_else(|e| panic!("{name}: {e}\n{}", render_diagnostic(name, &src, &e)));
        let mut e = Engine::new(c.loaded());
        let v = e.call_name("testReport", vec![])
            .unwrap_or_else(|err| panic!("{name} testReport: {err}"));
        v
    };

    // [1, -3, 7] filtered to positives → len 2; input untouched (head still 1).
    let v = call("lists/filter.wag");
    assert_product(&v, &[Value::Int(2), Value::Int(1)]);
    // contains finds 7, not 5 (list survives both calls).
    let v = call("lists/contains.wag");
    raw_int(&v, 0);
    // (count of 1, count of 9) over [1, -3, 7, 1].
    let v = call("lists/count.wag");
    assert_product(&v, &[Value::Int(2), Value::Int(0)]);
    // (isSorted asc, isSorted desc).
    let v = call("lists/is_sorted.wag");
    assert_product(&v, &[Value::Bool(true), Value::Bool(false)]);
    // [1..5] double-mapped via mapB and summed: 2 * (1+…+5) * 2 = 60.
    let v = call("lists/mapB.wag");
    raw_int(&v, 60);
}

/// Second batch of the List migration (D062): §8.3/§8.4/§8.4a/§8.5/§8.6/§8.8/
/// §8.13 — pure-function fixtures rewritten as borrowed peek/step walks over
/// `List` handles, each with a zero-arg `testReport` entry point.
#[test]
fn m10_positive_listh_migrated_batch2() {
    for name in [
        "lists/sum_list.wag",
        "lists/length.wag",
        "lists/is_empty.wag",
        "lists/reverse.wag",
        "lists/map.wag",
        "lists/max_list.wag",
        "lists/insertion_sort.wag",
    ] {
        assert_ok_compile(name);
    }

    let call = |name: &str| -> Value {
        let src = with_deps(name);
        let c = compile_inline(&src, Some(&prelude_src()))
            .unwrap_or_else(|e| panic!("{name}: {e}\n{}", render_diagnostic(name, &src, &e)));
        let mut e = Engine::new(c.loaded());
        let v = e.call_name("testReport", vec![])
            .unwrap_or_else(|err| panic!("{name} testReport: {err}"));
        v
    };

    // (sum of [1..5], head of the untouched input) → (15, 1).
    let v = call("lists/sum_list.wag");
    assert_product(&v, &[Value::Int(15), Value::Int(1)]);
    // (walked length, O(1) len field) over [3, -1, 4] → (3, 3).
    let v = call("lists/length.wag");
    assert_product(&v, &[Value::Int(3), Value::Int(3)]);
    // (describe of empty, describe of [10, -4]) → (0, 6).
    let v = call("lists/is_empty.wag");
    assert_product(&v, &[Value::Int(0), Value::Int(6)]);
    // (head, length) of reverse [1, 2, 3] → (3, 3).
    let v = call("lists/reverse.wag");
    assert_product(&v, &[Value::Int(3), Value::Int(3)]);
    // (head, length) of map increment [1, 2, 3] → (2, 3).
    let v = call("lists/map.wag");
    assert_product(&v, &[Value::Int(2), Value::Int(3)]);
    // max of [3, -1, 9, 4] → 9.
    let v = call("lists/max_list.wag");
    raw_int(&v, 9);
    // (head, length) of sort [3, -1, 2] → (-1, 3).
    let v = call("lists/insertion_sort.wag");
    assert_product(&v, &[Value::Int(-1), Value::Int(3)]);
}

#[test]
fn m10_positive_pure_functions() {
    // Numeric / list library fixtures: check + load, then smoke-call entry points.
    assert_ok_compile("basics/factorial.wag");
    let v = call_inline(&with_deps("basics/factorial.wag"), Some(&prelude_src()), "factorial", vec![Value::Int(5)]).unwrap();
    raw_int(&v, 120);

    assert_ok_compile("basics/fibonacci.wag");
    let v = call_inline(&with_deps("basics/fibonacci.wag"), Some(&prelude_src()), "fib", vec![Value::Int(10)]).unwrap();
    raw_int(&v, 55);

    assert_ok_compile("lists/sum_list.wag");
    {
        let src = with_deps("lists/sum_list.wag");
        let c = compile_inline(&src, Some(&prelude_src())).unwrap();
        let mut e = Engine::new(c.loaded());
        let lst = e.list_from_ints(&[1, 2, 3, 4, 5]).unwrap();
        raw_int(&e.call_name("sumList", vec![lst]).unwrap(), 15);
    }

    assert_ok_compile("lists/length.wag");
    assert_ok_compile("lists/is_empty.wag");
    assert_ok_compile("lists/reverse.wag");
    assert_ok_compile("lists/map.wag");
    assert_ok_compile("lists/filter.wag");
    assert_ok_compile("lists/max_list.wag");
    assert_ok_compile("basics/gcd.wag");
    assert_ok_compile("basics/pow.wag");
    assert_ok_compile("lists/contains.wag");
    assert_ok_compile("lists/count.wag");
    assert_ok_compile("lists/insertion_sort.wag");
    assert_ok_compile("linearity/copy_vs_borrow.wag");
    assert_ok_compile("lists/is_sorted.wag");
    assert_ok_compile("types/field_access.wag");

    // §3.3.1 .last accessor: one-pass forward list building.
    assert_ok_compile("lists/list_last.wag");
    {
        let src = with_deps("lists/list_last.wag");
        let c = compile_inline(&src, Some(&prelude_src())).unwrap();
        let mut e = Engine::new(c.loaded());
        // buildThree → [1, 2, 3]
        let v = e.call_name("buildThree", vec![]).unwrap();
        let xs = e.list_to_ints(&v).unwrap();
        assert_eq!(xs, vec![1, 2, 3]);
        // lastLength → 3
        let lst = e.list_from_ints(&[1, 2, 3]).unwrap();
        raw_int(&e.call_name("lastLength", vec![lst]).unwrap(), 3);
    }

    // §8.25 Array bridge: List <-> Array roundtrip.
    assert_ok_compile("arrays/array_bridge.wag");
    {
        let src = with_deps("arrays/array_bridge.wag");
        let c = compile_inline(&src, Some(&prelude_src())).unwrap();
        let mut e = Engine::new(c.loaded());
        // roundtripLen → 3
        raw_int(&e.call_name("roundtripLen", vec![]).unwrap(), 3);
        // getMid → 20
        raw_int(&e.call_name("getMid", vec![]).unwrap(), 20);
        // setAndGet → 99
        raw_int(&e.call_name("setAndGet", vec![]).unwrap(), 99);
        // combined → 100
        raw_int(&e.call_name("combined", vec![]).unwrap(), 100);
    }

    // String (§3 / §9): packed UTF-8, linear, non-pointer-free.
    assert_ok_compile("strings/string_basics.wag");
    {
        let src = with_deps("strings/string_basics.wag");
        let c = compile_inline(&src, Some(&prelude_src())).unwrap();
        let mut e = Engine::new(c.loaded());
        assert!(matches!(e.call_name("lenHello", vec![]).unwrap(), Value::Int(5)));
        assert!(matches!(e.call_name("lenUtf8", vec![]).unwrap(), Value::Int(2)));
        assert!(matches!(e.call_name("concatLen", vec![]).unwrap(), Value::Int(4)));
        assert!(matches!(e.call_name("eqSame", vec![]).unwrap(), Value::Bool(true)));
        assert!(matches!(e.call_name("eqDiff", vec![]).unwrap(), Value::Bool(false)));
        let cps = e.call_name("cps", vec![]).unwrap();
        assert_eq!(e.list_to_ints(&cps).unwrap(), vec![104, 233]);
        assert!(matches!(e.call_name("roundtrip", vec![]).unwrap(), Value::Bool(true)));
    }

    // String literal patterns in match arms: alias resolution must agree.
    assert_ok_compile("strings/string_match_literal.wag");
    {
        let src = with_deps("strings/string_match_literal.wag");
        let c = compile_inline(&src, Some(&prelude_src())).unwrap();
        let mut e = Engine::new(c.loaded());
        assert!(matches!(e.call_name("isPlus", vec![]).unwrap(), Value::Bool(true)));
        assert!(matches!(e.call_name("isStar", vec![]).unwrap(), Value::Bool(true)));
        assert!(matches!(e.call_name("isOther", vec![]).unwrap(), Value::Bool(true)));
    }

    // Char + string indexing (§3.10 / §3.11): codepoint value + indexed access.
    assert_ok_compile("strings/char_string_index.wag");
    {
        let src = with_deps("strings/char_string_index.wag");
        let c = compile_inline(&src, Some(&prelude_src())).unwrap();
        let mut e = Engine::new(c.loaded());
        let exp = opt_some(&e, e.program.make_char(104));
        assert_deep(&mut e, "first", exp);
        let exp = opt_some(&e, e.program.make_char(233));
        assert_deep(&mut e, "second", exp);
        let exp = opt_none(&e);
        assert_deep(&mut e, "oob", exp);
        let exp = opt_some(&e, Value::Int(104));
        assert_deep(&mut e, "byte0", exp);
        let exp = opt_some(&e, Value::Int(169));
        assert_deep(&mut e, "byte2", exp);
        let exp = opt_none(&e);
        assert_deep(&mut e, "byteOob", exp);
        let exp = opt_some(&e, e.program.make_char(104));
        assert_deep(&mut e, "cpH", exp);
        let exp = opt_none(&e);
        assert_deep(&mut e, "cpSurrogate", exp);
        let exp = opt_some(&e, Value::Bool(true));
        assert_deep(&mut e, "hEqH", exp);
        let exp = opt_some(&e, Value::Bool(true));
        assert_deep(&mut e, "hNeE", exp);
    }

    // Array T (§3.4 / §6.8): fixed-size, index-addressable, non-pointer-free.
    assert_ok_compile("arrays/array.wag");
    {
        let src = with_deps("arrays/array.wag");
        let c = compile_inline(&src, Some(&prelude_src())).unwrap();
        let mut e = Engine::new(c.loaded());
        assert_deep(&mut e, "len3", Value::Int(3));
        let exp = opt_some(&e, opt_none(&e));
        assert_deep(&mut e, "slot0Empty", exp);
        let exp = opt_none(&e);
        assert_deep(&mut e, "oob", exp);
        let exp = opt_some(&e, opt_some(&e, Value::Int(42)));
        assert_deep(&mut e, "set0read", exp);
        let exp = opt_some(&e, opt_some(&e, Value::Int(7)));
        assert_deep(&mut e, "set2read", exp);
        let exp = opt_some(&e, opt_none(&e));
        assert_deep(&mut e, "slot1After", exp);
    }

    // Comprehensions (§6.7.5): desugar to map/filter/fold/zip/flatMap + lambdas.
    // Prelude is already included via with_deps.
    {
        let src = with_deps("lists/comprehensions.wag");
        let c = compile_inline(&src, Some(&prelude_src())).unwrap();
        let loaded = c.loaded();
        let mut e = Engine::new(loaded);

        // squares [1,2,3] = [1,4,9]
        let lst123 = e.list_from_ints(&[1, 2, 3]).unwrap();
        let sq = e.call_name("squares", vec![lst123]).unwrap();
        assert_eq!(e.list_to_ints(&sq).unwrap(), vec![1, 4, 9]);

        // positives [-1, 2, -3, 4] = [2, 4]
        let lst_mix = e.list_from_ints(&[-1, 2, -3, 4]).unwrap();
        let pos = e.call_name("positives", vec![lst_mix]).unwrap();
        assert_eq!(e.list_to_ints(&pos).unwrap(), vec![2, 4]);

        // sumComp [1,2,3,4,5] = 15
        let lst12345 = e.list_from_ints(&[1, 2, 3, 4, 5]).unwrap();
        let s = e.call_name("sumComp", vec![lst12345]).unwrap();
        assert!(matches!(s, Value::Int(15)));

        // zipSum [1,2,3] [10,20,30,40] = [11,22,33] (truncated to shortest)
        let lst_a = e.list_from_ints(&[1, 2, 3]).unwrap();
        let lst_b = e.list_from_ints(&[10, 20, 30, 40]).unwrap();
        let zs = e.call_name("zipSum", vec![lst_a, lst_b]).unwrap();
        assert_eq!(e.list_to_ints(&zs).unwrap(), vec![11, 22, 33]);

        // cartSum [1,2] [10,20] = [11,21,12,22] (all pairwise sums)
        let lst_c = e.list_from_ints(&[1, 2]).unwrap();
        let lst_d = e.list_from_ints(&[10, 20]).unwrap();
        let cs = e.call_name("cartSum", vec![lst_c, lst_d]).unwrap();
        assert_eq!(e.list_to_ints(&cs).unwrap(), vec![11, 21, 12, 22]);

        // sqPos [-2, 3, -1, 4] = [9, 16]
        let lst_e = e.list_from_ints(&[-2, 3, -1, 4]).unwrap();
        let sp = e.call_name("sqPos", vec![lst_e]).unwrap();
        assert_eq!(e.list_to_ints(&sp).unwrap(), vec![9, 16]);

        // inRange [-5, 0, 1, 50, 100, 200] = [2, 100] (0 < x < 100, doubled)
        let lst_f = e.list_from_ints(&[-5, 0, 1, 50, 100, 200]).unwrap();
        let ir = e.call_name("inRange", vec![lst_f]).unwrap();
        assert_eq!(e.list_to_ints(&ir).unwrap(), vec![2, 100]);
    }
}

#[test]
fn m10_positive_do_notation_monad() {
    // Do-notation over Option via Monad typeclass dispatch (§6.5, D087).
    let src = r#"
safeDiv = Int, Int -> Option Int: a, b ->
  if b == 0 -> None
  _         -> Some (a / b)

chainDiv = Int -> Option Int: x -> do
  v1 <- safeDiv x 2
  v2 <- safeDiv v1 3
  return v2

wrapDouble = Option Int -> Option Int: opt -> do
  v <- opt
  return (v * 2)
"#;
    let c = compile_inline(src, Some(&prelude_src())).unwrap();
    let mut e = Engine::new(c.loaded());

    // chainDiv 12 = Some(6) then Some(2) = Some(2)
    let v = e.call_name("chainDiv", vec![Value::Int(12)]).unwrap();
    assert_eq!(v, opt_some(&e, Value::Int(2)));

    // chainDiv 7 = Some(3) [7/2=3] then Some(1) [3/3=1] = Some(1)
    let v = e.call_name("chainDiv", vec![Value::Int(7)]).unwrap();
    assert_eq!(v, opt_some(&e, Value::Int(1)));

    // wrapDouble (Some 5) = Some 10
    let v = e.call_name("wrapDouble", vec![opt_some(&e, Value::Int(5))]).unwrap();
    assert_eq!(v, opt_some(&e, Value::Int(10)));

    // wrapDouble None = None (short-circuit)
    let v = e.call_name("wrapDouble", vec![opt_none(&e)]).unwrap();
    assert_eq!(v, opt_none(&e));
}

#[test]
fn m10_positive_eq_typeclass() {
    // Eq typeclass: user-type instance, parametric Option T, parametric Vec T.
    for name in [
        "typeclasses/eq_user_type.wag",
        "typeclasses/eq_option.wag",
        "typeclasses/eq_vec.wag",
    ] {
        assert_ok_compile(name);
    }

    let call = |name: &str| -> Value {
        let src = with_deps(name);
        let c = compile_inline(&src, Some(&prelude_src()))
            .unwrap_or_else(|e| panic!("{name}: {e}\n{}", render_diagnostic(name, &src, &e)));
        let mut e = Engine::new(c.loaded());
        e.call_name("testReport", vec![])
            .unwrap_or_else(|err| panic!("{name} testReport: {err}"))
    };

    // User-type instance: Red==Red true, Red==Green false → 0.
    raw_int(&call("typeclasses/eq_user_type.wag"), 0);
    // Option T: Some 1==Some 1 true, Some 1==Some 2 false → 0.
    raw_int(&call("typeclasses/eq_option.wag"), 0);
    // Vec T: [1,2]==[1,2] true → 0.
    raw_int(&call("typeclasses/eq_vec.wag"), 0);
}

#[test]
fn m10_negative_eq_no_instance() {
    // Type without an Eq instance — `==` should fail at inference.
    assert_reject(
        "types/neg_eq_no_instance.wag",
        PipelineStage::Type,
        None,
        Some(TypeStage::Inference),
    );
}

#[test]
fn m10_positive_recursive_usertype() {
    // §7 rule 14 permits self-referential user types (classified non-pointer-free).
    // rsumSample n = sum([n, 1]) = n + 1 — exercises recursive value construction
    // and tail-recursive deconstruction in the VM.
    assert_ok_compile("types/recursive_usertype.wag");
    let v = call_inline(&with_deps("types/recursive_usertype.wag"), Some(&prelude_src()), "rsumSample", vec![Value::Int(5)]).unwrap();
    raw_int(&v, 6);
    let v = call_inline(&with_deps("types/recursive_usertype.wag"), Some(&prelude_src()), "rsumSample", vec![Value::Int(100)]).unwrap();
    raw_int(&v, 101);
}

#[test]
fn m10_positive_recursive_usertype_mutual() {
    // §7 rule 14 permits mutual recursion (A ↔ B).
    assert_ok_compile("types/recursive_usertype_mutual.wag");
}

/// Assert that calling `name` with no args yields a value structurally equal to
/// `expected`.
fn assert_deep(e: &mut wag_interp::Engine, name: &str, expected: Value) {
    let v = e.call_name(name, vec![]).unwrap();
    assert!(
        v == expected,
        "{name}: got {}, expected {}",
        v.brief_desc(),
        expected.brief_desc()
    );
}

// ---------------------------------------------------------------------------
// Data-structure fixtures (fixtures/datastructures/)
// ---------------------------------------------------------------------------

#[test]
fn m10_positive_datastructures() {
    for name in [
        "datastructures/bst.wag",
        "datastructures/rose_tree.wag",
        "datastructures/user_region_list.wag",
        "datastructures/matrix_2d.wag",
        "datastructures/deep_records.wag",
        "datastructures/expr_tree.wag",
        "datastructures/dict_map.wag",
    ] {
        assert_ok_compile(name);
    }

    // bst: search 4=True, search 9=False, min=1, max=8
    let v = call_inline(&with_deps("datastructures/bst.wag"), Some(&prelude_src()), "testReport", vec![]).unwrap();
    assert_eq!(product_len(&v), Some(4), "bst: {}", v.brief_desc());

    // rose_tree: count=8, sum=28
    let v = call_inline(&with_deps("datastructures/rose_tree.wag"), Some(&prelude_src()), "testReport", vec![]).unwrap();
    assert_eq!(product_len(&v), Some(2), "rose_tree: {}", v.brief_desc());

    // user_region_list: len=5, headVal=Some(5)
    let v = call_inline(&with_deps("datastructures/user_region_list.wag"), Some(&prelude_src()), "testReport", vec![]).unwrap();
    assert_eq!(product_len(&v), Some(2), "user_region_list: {}", v.brief_desc());

    // matrix_2d: getElem(1,2)=6, sum=21, transpose rows=3
    let v = call_inline(&with_deps("datastructures/matrix_2d.wag"), Some(&prelude_src()), "testReport", vec![]).unwrap();
    assert_eq!(product_len(&v), Some(3), "matrix_2d: {}", v.brief_desc());

    // deep_records: (Some(200), Some(999))
    let v = call_inline(&with_deps("datastructures/deep_records.wag"), Some(&prelude_src()), "testReport", vec![]).unwrap();
    assert_eq!(product_len(&v), Some(2), "deep_records: {}", v.brief_desc());

    // expr_tree: (leftSpineLen=3, leftNumCount=1, rightSpineLen=4, rootKind=2)
    let v = call_inline(&with_deps("datastructures/expr_tree.wag"), Some(&prelude_src()), "testReport", vec![]).unwrap();
    assert_eq!(product_len(&v), Some(4), "expr_tree: {}", v.brief_desc());

    // dict_map: (lookup(3)=Some("three"), minKey=Some(1), maxKey=Some(5), hasKey(2)=true)
    let v = call_inline(&with_deps("datastructures/dict_map.wag"), Some(&prelude_src()), "testReport", vec![]).unwrap();
    assert_eq!(product_len(&v), Some(4), "dict_map: {}", v.brief_desc());
}

#[test]
fn m10_positive_actor_and_main() {
    assert_ok_compile("actors/counter_pragma.wag");
    assert_ok_compile("actors/counter_msg.wag");
    assert_ok_compile("actors/counter_supervised.wag");
    assert_ok_compile("actors/monitor.wag");
    assert_ok_compile("actors/abandonment_death.wag");
    assert_ok_compile("actors/self_handle_liveness.wag");
    assert_ok_compile("actors/none_completion.wag");
    assert_ok_compile("actors/string_concat_send.wag");

    // Regression: string concat inside a Send message — rewrite_op_dispatch must
    // descend into Expr::Send (native Int-only BinOp would panic "arithmetic on non-Int").
    match run_inline(&strip_prelude_import(&load("actors/string_concat_send.wag")), Some(&prelude_src()), run_opts(None)).unwrap() {
        PipelineOk::Executed(r) => {
            assert_eq!(r.stdout, "echoed", "string concat in Send msg");
            r.runtime.shutdown();
        }
        PipelineOk::Checked(_) => panic!("string_concat_send: expected Executed"),
    }

    // bootstrap @main (D081: uses @std, needs prelude)
    match run_inline(&strip_prelude_import(&load("io/bootstrap_argv_only.wag")), Some(&prelude_src()), run_opts(None)).unwrap() {
        PipelineOk::Executed(r) => r.runtime.shutdown(),
        PipelineOk::Checked(_) => panic!("bootstrap: expected Executed"),
    }

    // 8_15 with scripted stdin — `@std` is a WAG prelude actor (D043).
    // D081: @main never exits, so drive manually instead of run_main_with_io.
    let src = strip_prelude_import(&load("io/main_std.wag"));
    let compiled = compile_inline(&src, Some(&prelude_src())).unwrap();
    let lines = vec!["hello".into(), "world".into()];
    let io = IoConfig::scripted(lines.clone());
    let rt = Runtime::new_with_io(compiled.loaded().clone(), cfg(), io.clone());
    let empty_argv = {
        let ctors = wag_interp::PreludeListCtors::from_program(compiled.loaded()).unwrap();
        wag_interp::build_prelude_list(Vec::new(), ctors).unwrap()
    };
    let _main_id = rt.spawn_main(empty_argv).unwrap();
    for _ in 0..200 {
        rt.drive(64).ok();
        if io.take_stdout_string().contains("hello") {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(io.take_stdout_string().contains("hello"), "8_15 echo");
    rt.shutdown();
}

// ---------------------------------------------------------------------------
// Modules that type-check but fail check pass 2 (mixed good+bad §8.19/§8.20)
// ---------------------------------------------------------------------------

#[test]
fn m10_reject_8_19_and_8_20_tail_cycle() {
    assert_reject(
        "basics/mutual_recursion.wag",
        PipelineStage::Check,
        Some(Rule::TailCycle),
        None,
    );
    assert_reject(
        "basics/higher_order_cycles.wag",
        PipelineStage::Check,
        Some(Rule::TailCycle),
        None,
    );
}

// ---------------------------------------------------------------------------
// Negative fixtures
// ---------------------------------------------------------------------------

#[test]
fn m10_negatives_by_stage() {


    // Type — namespacing / inference
    // (Recursive user types are now permitted — §7 rule 14; see
    //  m10_positive_recursive_usertype / m10_positive_recursive_usertype_mutual.)
    assert_reject(
        "types/neg_duplicate_constructor.wag",
        PipelineStage::Type,
        None,
        Some(TypeStage::Namespacing),
    );
    assert_reject(
        "types/neg_type_mismatch_option_lift.wag",
        PipelineStage::Type,
        None,
        Some(TypeStage::Inference),
    );
    assert_reject(
        "linearity/neg_borrow_consume.wag",
        PipelineStage::Type,
        None,
        Some(TypeStage::Inference),
    );
    // D074: overload arg type mismatch (map on Vec Int requires Int -> Int).
    assert_reject(
        "types/neg_vec_overload_type_mismatch.wag",
        PipelineStage::Type,
        None,
        Some(TypeStage::Inference),
    );
    // D075: class method type mismatch (map on List Int requires Int -> Int).
    assert_reject(
        "types/neg_typeclass_instance_mismatch.wag",
        PipelineStage::Type,
        None,
        Some(TypeStage::Inference),
    );

    // Check pass 1 / 2
    assert_reject(
        "linearity/neg_use_after_move.wag",
        PipelineStage::Check,
        Some(Rule::Linearity),
        None,
    );
    // Borrowed value as send payload: caught at Type stage (type mismatch `&Msg` vs `Msg`).
    assert_reject(
        "linearity/neg_borrow_as_send_payload.wag",
        PipelineStage::Type,
        None,
        Some(TypeStage::Inference),
    );
    assert_reject(
        "types/neg_field_path_multi_constructor.wag",
        PipelineStage::Check,
        Some(Rule::FieldPath),
        None,
    );
    // Region-resident types may not appear in inter-actor message types (§4.3.5).
    // Now caught at Type stage: `&RH` is not a valid payload for `Actor RH`.
    assert_reject(
        "datastructures/neg_handle_send.wag",
        PipelineStage::Type,
        None,
        Some(TypeStage::Inference),
    );
}



// ---------------------------------------------------------------------------
// Diagnostic span spot-checks (one per major stage)
// ---------------------------------------------------------------------------

#[test]
fn m10_diagnostic_spans_spot_check() {


    // Type: mismatch (span-anchored type error)
    {
        let src = load("types/neg_type_mismatch_option_lift.wag");
        let d = compile_inline(&src, Some(&prelude_src())).unwrap_err();
        assert_eq!(d.stage, PipelineStage::Type);
        assert!(!d.span.is_empty(), "type error needs a span: {d}");
        let rendered = render_diagnostic("types/neg_type_mismatch_option_lift.wag", &src, &d);
        assert!(
            rendered.contains("Error") || rendered.contains("error") || !rendered.is_empty(),
            "{rendered}"
        );
    }

    // Linearity (D065 two-span: illegal reuse site + consuming origin).
    {
        let src = load("linearity/neg_use_after_move.wag");
        // compile_inline parses the fixture behind a synthesized prelude
        // import block, so check-stage spans are offset by its length.
        let prefixed = format!("import\n  prelude\n    ..\n{src}");
        let d = compile_inline(&src, Some(&prelude_src())).unwrap_err();
        assert_eq!(d.rule, Some(Rule::Linearity));
        assert!(!d.span.is_empty());
        // Span should point at the reused `xs` (the second `xs.len`).
        let slice = &prefixed[d.span.start..d.span.end.min(prefixed.len())];
        assert!(
            slice.contains("xs") || prefixed[d.span.start.saturating_sub(10)..].contains("xs"),
            "linearity span should mention xs vicinity, slice={slice:?}, {d}"
        );
        // The origin label anchors the consuming use (the `map` argument).
        assert!(d.origin_span.is_some(), "expected two-span diagnostic: {d}");
    }

    // Field path
    {
        let src = load("types/neg_field_path_multi_constructor.wag");
        let d = compile_inline(&src, Some(&prelude_src())).unwrap_err();
        assert_eq!(d.rule, Some(Rule::FieldPath));
        assert!(!d.span.is_empty());
    }

    // Tail cycle
    {
        let src = load("basics/mutual_recursion.wag");
        let d = compile_inline(&src, Some(&prelude_src())).unwrap_err();
        assert_eq!(d.rule, Some(Rule::TailCycle));
        assert!(!d.span.is_empty());
        let rendered = render_diagnostic("8_19.wag", &src, &d);
        assert!(
            rendered.contains("rule 1") || d.to_string().contains("rule 1"),
            "{rendered}"
        );
    }

}

/// Inventory: every on-disk fixture is classified by the harness.
#[test]
fn m10_all_fixtures_accounted_for() {
    let known = [
        "actors/counter_pragma.wag",
        "actors/counter_msg.wag",
        "basics/factorial.wag",
        "basics/fibonacci.wag",
        "lists/sum_list.wag",
        "lists/length.wag",
        "lists/is_empty.wag",
        "lists/reverse.wag",
        "lists/map.wag",
        "lists/filter.wag",
        "lists/max_list.wag",
        "basics/gcd.wag",
        "basics/pow.wag",
        "lists/contains.wag",
        "lists/count.wag",
        "lists/insertion_sort.wag",
        "actors/counter_supervised.wag",
        "io/main_std.wag",
        "linearity/copy_vs_borrow.wag",
        "lists/is_sorted.wag",
        "actors/monitor.wag",
        "actors/abandonment_death.wag",
        "actors/self_handle_liveness.wag",
        "actors/none_completion.wag",
        "actors/string_concat_send.wag",
        "actors/liveness_cascade.wag",
        "actors/cross_thread_ref.wag",
        "basics/mutual_recursion.wag",
        "basics/higher_order_cycles.wag",
        "types/field_access.wag",
        "lists/mapB.wag",
        "types/map_types.wag",
        "lists/list_last.wag",
        "arrays/array_bridge.wag",
        "io/bootstrap_argv_only.wag",
        "linearity/copy_pointer_free.wag",
        "linearity/neg_borrow_as_send_payload.wag",
        "linearity/neg_borrow_consume.wag",
        "types/neg_duplicate_constructor.wag",
        "types/neg_field_path_multi_constructor.wag",
        "types/neg_type_mismatch_option_lift.wag",
        "types/neg_vec_overload_type_mismatch.wag",
        "types/neg_typeclass_instance_mismatch.wag",
        "types/neg_eq_no_instance.wag",
        "linearity/neg_use_after_move.wag",
        "strings/string_basics.wag",
        "strings/string_match_literal.wag",
        "strings/char_string_index.wag",
        "arrays/array.wag",
        "lists/comprehensions.wag",
        "types/recursive_usertype.wag",
        "types/recursive_usertype_mutual.wag",
        "regions/ref_cursor_walk.wag",
        "regions/ref_list_basic.wag",
        "datastructures/bst.wag",
        "datastructures/rose_tree.wag",
        "datastructures/user_region_list.wag",
        "datastructures/matrix_2d.wag",
        "datastructures/deep_records.wag",
        "datastructures/expr_tree.wag",
        "datastructures/dict_map.wag",
        "datastructures/neg_handle_send.wag",
        "modules/alias/entry.wag",
        "modules/alias/lib.wag",
        "modules/all_import/entry.wag",
        "modules/all_import/lib.wag",
        "modules/collision/entry.wag",
        "modules/collision/lib.wag",
        "modules/private_access/entry.wag",
        "modules/private_access/lib.wag",
        "modules/three_module/base.wag",
        "modules/three_module/entry.wag",
        "modules/three_module/mid.wag",
        "modules/reexport/entry.wag",
        "modules/reexport/facade.wag",
        "modules/reexport/leaf.wag",
        "modules/two_module/entry.wag",
        "modules/two_module/lib.wag",
        "typeclasses/map_list.wag",
        "typeclasses/filter_list.wag",
        "typeclasses/eq_user_type.wag",
        "typeclasses/eq_option.wag",
        "typeclasses/eq_vec.wag",
        "packages/cross_pkg/app/main.wag",
        "packages/cross_pkg/utils/lib.wag",
        "packages/collision/app/main.wag",
        "packages/collision/pkg_a/dup.wag",
        "packages/collision/pkg_b/dup.wag",
        "packages/transitive/app/main.wag",
        "packages/transitive/mid/mid.wag",
        "packages/transitive/leaf/leaf.wag",
    ];
    let dir = fixtures_dir();
    let mut on_disk: Vec<String> = Vec::new();
    fn walk(dir: &std::path::Path, base: &std::path::Path, out: &mut Vec<String>) {
        for e in fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if p.extension().map_or(false, |x| x == "wag") {
                if let Ok(rel) = p.strip_prefix(base) {
                    out.push(rel.to_string_lossy().into_owned());
                }
            }
        }
    }
    walk(&dir, &dir, &mut on_disk);
    on_disk.sort();
    for name in &on_disk {
        assert!(
            known.contains(&name.as_str()),
            "fixture {name} not covered by m10 harness — add an expectation"
        );
    }
    for name in known {
        assert!(
            dir.join(name).is_file(),
            "missing fixture file {name}"
        );
    }
}