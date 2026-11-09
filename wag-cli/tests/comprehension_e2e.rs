//! Compile-level regression coverage for list comprehensions (§6.7), driven
//! through the full pipeline (parse → desugar → lift → type → check).
//!
//! Focus: the cartesian-product case where a generator var is used in both the
//! guard and result lambdas — the shape that broke rule 10 before lambda
//! lifting's post-lift `copy` pass (D072) — plus every other comprehension form
//! in `fixtures/comprehensions.wag`.
//!
//! Comprehensions desugar to the prelude's region-list combinators
//! (`map`/`filter`/`fold`/`flatMap`/`zip`, §6.7.5); runtime value coverage
//! lives in `wag-interp/tests/catalog_exec.rs`.

use std::fs;
use std::path::PathBuf;

use wag_cli::{compile_multi_module, Diagnostic, PipelineStage};
use wag_cli::resolve::{ModuleSet, ResolvedModule};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
}

fn prelude_src() -> String {
    fs::read_to_string(
        fixtures_dir()
            .parent()
            .unwrap()
            .join("prelude/prelude.wag"),
    )
    .expect("prelude.wag missing")
}

/// Compile `src` with the prelude injected as a separate module (same shape as
/// the pipeline harness's `compile_inline`).
fn compile_with_prelude(src: &str) -> Result<wag_cli::Compiled, Diagnostic> {
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

    let p = prelude_src();
    let pprog = wag_syntax::parse_desugared(&p).map_err(parse_err)?;
    let user_src = format!("import\n  prelude\n    ..\n{src}");
    let program = wag_syntax::parse_desugared(&user_src).map_err(parse_err)?;

    let modules = vec![
        ResolvedModule {
            name: "prelude".into(),
            path: PathBuf::from("prelude.wag"),
            source: p,
            program: pprog,
            imports: vec![],
            package: "<test>".into(),
        },
        ResolvedModule {
            name: "main".into(),
            path: PathBuf::from("main.wag"),
            source: user_src.clone(),
            program,
            imports: vec![],
            package: "<test>".into(),
            },
    ];
    let module_set = ModuleSet { modules, entry_index: 1 };
    let checked = compile_multi_module(&module_set)?;
    Ok(wag_cli::Compiled::new(checked))
}

/// Every comprehension form in the fixture must pass parse → type → check.
/// This is the D072 regression: `cartProd` (outer var used in both the guard
/// and result lambdas) previously generated a rule-10 double use of the
/// captured var inside the lifted synthetic functions.
#[test]
fn comp_fixture_compiles() {
    let src = fs::read_to_string(fixtures_dir().join("lists/comprehensions.wag")).unwrap();
    compile_with_prelude(&src)
        .unwrap_or_else(|e| panic!("comprehensions.wag failed to compile: {e:?}"));
}
