//! Diagnostic: detect the cyclic-list bug in map/flatMap closures without hanging.
//! Walks the result list via `Engine::list_to_ints`; a cycle would manifest as
//! an error or non-terminating walk (caught by the step budget).

use wag_cli::resolve::{ModuleSet, ResolvedModule};
use wag_cli::{compile_multi_module, Diagnostic, PipelineStage};
use std::path::PathBuf;
use wag_interp::{Engine, Value};

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

fn bounded_walk(e: &Engine, start: &Value) -> Result<Vec<i64>, String> {
    e.list_to_ints(start).map_err(|err| err.to_string())
}

#[test]
fn diag_cartsum() {
    let prelude = std::fs::read_to_string("../prelude/prelude.wag").unwrap();
    let user = "rowOf = List Int, Int -> List Int: ys, x -> map (y -> x + y) ys\ncartSum = List Int, List Int -> List Int: xs, ys -> flatMap (x -> rowOf (copy ys) x) xs\n";
    let c = compile_inline(user, Some(&prelude)).unwrap();
    let mut e = Engine::new(c.loaded());
    let xs = e.list_from_ints(&[1, 2]).unwrap();
    let ys = e.list_from_ints(&[10, 20]).unwrap();
    let result = e.call_name("cartSum", vec![xs, ys]).unwrap();
    eprintln!("result = {:?}", result);
    match bounded_walk(&e, &result) {
        Ok(ints) => {
            eprintln!("cartSum OK: {ints:?}");
            assert_eq!(ints, vec![11, 21, 12, 22]);
        }
        Err(msg) => {
            eprintln!("cartSum FAIL: {msg}");
            panic!("{msg}");
        }
    }
}

#[test]
fn diag_cap_list_use_once() {
    // Capture a LIST (Ref) but use it only ONCE. If the trigger is "multiple use",
    // this should be OK (linear) even though it captures a Ref.
    let prelude = std::fs::read_to_string("../prelude/prelude.wag").unwrap();
    let user = "oneRow = List Int -> List Int: ys -> rowOf ys 1\nrowOf = List Int, Int -> List Int: ys, x -> map (y -> x + y) ys\n";
    let c = compile_inline(user, Some(&prelude)).unwrap();
    let mut e = Engine::new(c.loaded());
    let ys = e.list_from_ints(&[10, 20]).unwrap();
    let result = e.call_name("oneRow", vec![ys]).unwrap();
    match bounded_walk(&e, &result) {
        Ok(ints) => {
            eprintln!("oneRow OK: {ints:?}");
            assert_eq!(ints, vec![11, 21]);
        }
        Err(msg) => panic!("oneRow FAIL: {msg}"),
    }
}

#[test]
fn diag_cap_list_use_twice() {
    // Capture a LIST (Ref) and use it TWICE straight-line. The checker MUST
    // reject this (rule 10: use after move). Assert the error, not a cycle.
    let prelude = std::fs::read_to_string("../prelude/prelude.wag").unwrap();
    let user = "rowOf = List Int, Int -> List Int: ys, x -> map (y -> x + y) ys\ntwoRows = List Int -> List Int: ys -> appendAll [rowOf ys 1, rowOf ys 2]\n";
    let result = compile_inline(user, Some(&prelude));
    assert!(
        result.is_err(),
        "expected checker to reject straight-line double-use of ys (rule 10), but it compiled"
    );
    let err = result.unwrap_err().to_string();
    eprintln!("twoRows rejected (as expected): {err}");
}

#[test]
fn diag_simple_map_noncapturing() {
    // f = x*2, no capture. Should NOT cycle if the bug is closure-capture-specific.
    let prelude = std::fs::read_to_string("../prelude/prelude.wag").unwrap();
    let user = "double = List Int -> List Int: xs -> map (x -> x * 2) xs\n";
    let c = compile_inline(user, Some(&prelude)).unwrap();
    let mut e = Engine::new(c.loaded());
    let xs = e.list_from_ints(&[1, 2, 3]).unwrap();
    let result = e.call_name("double", vec![xs]).unwrap();
    match bounded_walk(&e, &result) {
        Ok(ints) => {
            eprintln!("double OK: {ints:?}");
            assert_eq!(ints, vec![2, 4, 6]);
        }
        Err(msg) => panic!("double FAIL: {msg}"),
    }
}

#[test]
fn diag_inrange_nocapture() {
    // inRange: single-generator filter+map. Closures capture NOTHING (only element x).
    // xs is used once. Should be linear/sound — but the original report says it hangs.
    let prelude = std::fs::read_to_string("../prelude/prelude.wag").unwrap();
    let user = "inRange = List Int -> List Int: xs -> [x <- xs | x > 0, x < 100 -> x * 2]\n";
    let c = compile_inline(user, Some(&prelude)).unwrap();
    let mut e = Engine::new(c.loaded());
    let xs = e.list_from_ints(&[3, -1, 50, 200, 7]).unwrap();
    let result = e.call_name("inRange", vec![xs]).unwrap();
    // expected: [3,50,7] all in (0,100) -> [6, 100, 14]
    match bounded_walk(&e, &result) {
        Ok(ints) => eprintln!("inRange OK: {ints:?}"),
        Err(msg) => eprintln!("inRange CYCLE: {msg}"),
    }
}

#[test]
fn diag_map_capturing_scalar() {
    // f captures a scalar x; map (y -> x + y) xs. Mirrors rowOf's inner map.
    let prelude = std::fs::read_to_string("../prelude/prelude.wag").unwrap();
    let user = "addx = Int, List Int -> List Int: x, xs -> map (y -> x + y) xs\n";
    let c = compile_inline(user, Some(&prelude)).unwrap();
    let mut e = Engine::new(c.loaded());
    let xs = e.list_from_ints(&[10, 20]).unwrap();
    let result = e.call_name("addx", vec![Value::Int(1), xs]).unwrap();
    match bounded_walk(&e, &result) {
        Ok(ints) => {
            eprintln!("addx OK: {ints:?}");
            assert_eq!(ints, vec![11, 21]);
        }
        Err(msg) => panic!("addx FAIL: {msg}"),
    }
}
