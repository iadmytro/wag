use wag_cli::resolve::{ModuleSet, ResolvedModule};
use wag_cli::{compile_multi_module, Diagnostic, PipelineStage};
use std::path::PathBuf;
use wag_interp::Engine;

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

#[test]
fn min6_nested_closure_with_prelude() {
    let prelude = std::fs::read_to_string("../prelude/prelude.wag").unwrap();
    let user = r#"
rowOf = List Int, Int -> List Int: ys, x -> map (y -> x + y) ys
cartSum = List Int, List Int -> List Int: xs, ys -> flatMap (x -> rowOf ys x) xs
"#;
    let c = compile_inline(user, Some(&prelude)).unwrap_or_else(|e| {
        eprintln!("compile error: {:?}", e);
        panic!("compile failed")
    });
    let mut e = Engine::new(c.loaded());
    let xs = e.list_from_ints(&[1, 2]).unwrap();
    let ys = e.list_from_ints(&[10, 20]).unwrap();
    let result = e.call_name("cartSum", vec![xs, ys]).unwrap();
    let ints = e.list_to_ints(&result).unwrap();
    assert_eq!(ints, vec![11, 21, 12, 22]);
}
