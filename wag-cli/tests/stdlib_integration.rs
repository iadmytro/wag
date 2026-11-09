//! Stdlib v1 integration tests — exercises the full pipeline (parse → type → check → load)
//! with prelude functions, S4 builtins, and S5 file I/O builtins together.
//!
//! These drive the real `prelude/prelude.wag` through `compile_multi_module`
//! (the CLI path), verifying that the combined surface type-checks.

use std::fs;
use std::path::PathBuf;

use wag_cli::{compile_multi_module, Diagnostic};
use wag_cli::resolve::{ModuleSet, ResolvedModule};

fn prelude_src() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("prelude")
        .join("prelude.wag");
    fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to load prelude: {e}"))
}

fn compile_inline(src: &str, prelude: Option<&str>) -> Result<wag_check::CheckedProgram, Diagnostic> {
    let parse_err = |e: wag_syntax::ParseError| Diagnostic {
        stage: wag_cli::PipelineStage::Parse,
        span: e.span,
        message: e.message,
        hint: None,
        rule: None,
        type_stage: None,
        module_name: None,
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
    compile_multi_module(&module_set)
}

fn check_ok(name: &str, src: &str) {
    match compile_inline(src, Some(&prelude_src())) {
        Ok(_) => {}
        Err(d) => panic!("{name} FAILED: {} (stage: {:?})", d.message, d.stage),
    }
}

fn check_err(name: &str, src: &str) {
    match compile_inline(src, Some(&prelude_src())) {
        Ok(_) => panic!("{name}: expected error but got OK"),
        Err(d) => {
            let _ = d; // error is expected
        }
    }
}

// ─── Prelude combinators ──────────────────────────────────────────────────────

#[test]
fn prelude_cons_snoc_reverse_all_compile() {
    check_ok(
        "reverse/length/isEmpty/range",
        r#"
a = reverse [1, 2, 3]
b = length [1, 2]
c = [1].isEmpty
d = range 0 5
"#,
    );
}

#[test]
fn prelude_fold_simple() {
    // fold with a function that preserves accumulator type (no checked arithmetic).
    check_ok(
        "fold",
        r#"
idF = x, acc -> acc
s = fold idF 0 [1, 2, 3]
"#,
    );
}

#[test]
fn prelude_reduce_simple() {
    // reduce is gone; use fold with an explicit initial value.
    check_ok(
        "fold-as-reduce",
        r#"
idF = x, acc -> acc
r = fold idF 0 [1, 2, 3]
"#,
    );
}

#[test]
fn prelude_map_simple() {
    // map with a function that returns the same type (identity-like).
    check_ok(
        "map",
        r#"
id = x -> x
a = map id [1, 2]
"#,
    );
}

#[test]
fn prelude_filter_simple() {
    check_ok(
        "filter",
        r#"
isTrue = x -> x == True
a = filter isTrue [True, False]
"#,
    );
}

// ─── S4: Tier-2 primitives ────────────────────────────────────────────────────

#[test]
fn s4_nowms_compiles() {
    check_ok("nowMs", "t = nowMs\n");
}

#[test]
fn s4_randrange_compiles() {
    check_ok("randRange", "r = lo, hi -> randRange lo hi\n");
}

#[test]
fn s4_envget_compiles() {
    check_ok(
        "envGet",
        r#"e = envGet "HOME""#,
    );
}

// ─── S5: File I/O ─────────────────────────────────────────────────────────────

#[test]
fn s5_file_open_compiles() {
    check_ok(
        "fileOpen",
        r#"
w = fileOpenWrite "/tmp/test.txt"
r = fileOpenRead "/tmp/test.txt"
"#,
    );
}

#[test]
fn s5_file_ops_compile() {
    check_ok(
        "fileRead/Write/Close",
        r#"
doRead = fd -> fileRead fd
doWrite = fd, data -> fileWrite fd data
doClose = fd -> fileClose fd
"#,
    );
}

// ─── Name clash (local vs imported prelude name is an error, D053) ─────────────────────────────────────────

#[test]
fn prelude_name_clash_map() {
    check_err("map clash", "map = f, xs -> xs\n");
}

#[test]
fn prelude_name_clash_fold() {
    check_err("fold clash", "fold = f, init, xs -> init\n");
}

#[test]
fn prelude_name_clash_range() {
    check_err("range clash", "range = lo, hi -> 0\n");
}

#[test]
fn prelude_name_clash_reverse() {
    check_err("reverse clash", "reverse = xs -> xs\n");
}

// ─── Combined: prelude + builtins together ────────────────────────────────────

#[test]
fn combined_prelude_and_builtins() {
    check_ok(
        "combined",
        r#"
stamp = nowMs
pick = lo, hi -> randRange lo hi
home = envGet "HOME"
f = fileOpenRead "/tmp/x"
w = fileOpenWrite "/tmp/y"
a = [1, 2]
b = length [1, 2]
"#,
    );
}

// ─── Negative: type errors ────────────────────────────────────────────────────

#[test]
fn negative_map_wrong_arg_type() {
    check_err("map with Int as fn", "bad = map 42 [1]\n");
}

#[test]
fn negative_randrange_type_error() {
    check_err(
        "randRange with String",
        r#"bad = randRange "hello" 5"#,
    );
}

#[test]
fn negative_file_read_wrong_type() {
    check_err(
        "fileRead with String",
        r#"bad = fileRead "not_a_handle""#,
    );
}
