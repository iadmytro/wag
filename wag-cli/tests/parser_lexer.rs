//! Functional test for the WAG-in-WAG lexer (`examples/parser/src/lex.wag`).
//!
//! The lexer is a multi-module program (imports `prelude` + `token`), and a
//! `@main` driver cannot be used because D081 `@main` never self-terminates
//! (the CLI `run` waits for exit and times out). Instead we merge the real
//! `token.wag` + `lex.wag` into a single module (stripping their import/export
//! blocks), inject the prelude, and call `lex` as a pure function via the
//! Engine — then render the token list and assert on it.

use std::fs;
use std::path::PathBuf;

use wag_cli::resolve::{ModuleSet, ResolvedModule};
use wag_cli::{compile_multi_module, Diagnostic, PipelineStage};
use wag_interp::{Engine, Value};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn prelude_src() -> String {
    fs::read_to_string(workspace_root().join("prelude/prelude.wag"))
        .unwrap_or_else(|e| panic!("read prelude: {e}"))
}

fn parser_src(name: &str) -> String {
    fs::read_to_string(workspace_root().join("examples/parser/src").join(name))
        .unwrap_or_else(|e| panic!("read {name}: {e}"))
}

/// Remove top-level `import` / `export` blocks so two modules can be merged
/// into one. A block runs from a column-0 `import`/`export` line to the next
/// column-0 line.
fn strip_import_export(src: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in src.lines() {
        let col0 = !line.starts_with(' ') && !line.starts_with('\t');
        if col0 {
            skipping = line.trim() == "import" || line.trim() == "export";
            if !skipping {
                out.push_str(line);
                out.push('\n');
            }
        } else if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn compile_inline(src: &str, prelude: &str) -> Result<wag_cli::Compiled, Diagnostic> {
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
    let pprog = wag_syntax::parse_desugared(prelude).map_err(parse_err)?;
    modules.push(ResolvedModule {
        name: "prelude".into(),
        path: PathBuf::from("prelude.wag"),
        source: prelude.to_string(),
        program: pprog,
        imports: vec![],
        package: "<test>".into(),
    });

    let user_src = format!("import\n  prelude\n    ..\n{src}");
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

/// Merge the real token + lexer modules, add a render helper, compile with the
/// prelude, and return the rendered token list for `input`.
fn lex_render(input: &str) -> String {
    let token = strip_import_export(&parser_src("token.wag"));
    let lex = strip_import_export(&parser_src("lex.wag"));
    let render = "renderToksH = List Tok, String -> String: ts, acc -> match ts.popFirst\n  (None, _) -> acc\n  (Some t, rest) -> renderToksH rest (acc + \" \" + render t)\nrenderToks = List Tok -> String: ts -> renderToksH ts \"\"\nlexRender = String -> String: s -> renderToks (lex s)\n";
    let src = format!("{token}\n{lex}\n{render}");
    let compiled = compile_inline(&src, &prelude_src())
        .unwrap_or_else(|e| panic!("compile lexer: {e}"));
    let mut e = Engine::new(compiled.loaded());
    let v = e
        .call_name("lexRender", vec![Value::Str(input.to_string())])
        .unwrap_or_else(|e| panic!("call lexRender: {e}"));
    match v {
        Value::Str(s) => s,
        other => panic!("lexRender returned non-Str: {}", other.brief_desc()),
    }
}

#[test]
fn lexer_infix_expression() {
    let got = lex_render("1 + 2 * 3");
    assert_eq!(got.trim(), "IntLit:1 Op:+ IntLit:2 Op:* IntLit:3");
}

#[test]
fn lexer_identifier_and_call() {
    let got = lex_render("foo bar");
    assert_eq!(got.trim(), "Ident:foo Ident:bar");
}

#[test]
fn lexer_string_literal_with_escape() {
    // \n escape → newline codepoint 10 in the string body.
    let got = lex_render(r#""a\nb""#);
    assert!(got.contains("StrLit:"), "got: {got}");
}

#[test]
fn lexer_multi_char_operators() {
    let got = lex_render("a == b != c");
    assert_eq!(got.trim(), "Ident:a Op:== Ident:b Op:!= Ident:c");
}
