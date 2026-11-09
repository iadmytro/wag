//! D067 — module-aware checking: check errors carry their originating module.

use wag_check::{check_modules, ModuleView};
use wag_syntax::parse;
use wag_types::{infer_multi_module, ModuleInput};

/// Infer a two-module program (callee-first) and run the module-aware check.
fn check2(lib_src: &str, lib_exports: &[&str], main_src: &str, imports: &[(&str, &str)]) -> Result<
    wag_check::CheckedProgram,
    wag_check::CheckError,
> {
    let modules = vec![
        ModuleInput {
            name: "lib".into(),
            items: parse(lib_src).unwrap().items,
            import_aliases: Vec::new(),
            exports: lib_exports.iter().map(|s| s.to_string()).collect(),
        },
        ModuleInput {
            name: "main".into(),
            items: parse(main_src).unwrap().items,
            import_aliases: imports.iter().map(|(v, s)| (v.to_string(), s.to_string())).collect(),
            exports: Vec::new(),
        },
    ];
    let typed = infer_multi_module(&modules).unwrap();
    let views: Vec<ModuleView> = modules
        .iter()
        .map(|m| ModuleView { name: Some(m.name.as_str()), items: &m.items })
        .collect();
    check_modules(&views, typed)
}

/// A rule-10 violation in the *library* module is stamped `lib`, not `main`.
#[test]
fn linearity_error_is_stamped_with_its_module() {
    // `dup` reuses its owned data param twice — full linearity (D-A, §4.3.2).
    let err = check2("dup = T -> (T, T): x -> (x, x)", &["dup"], "run = Int -> (Int, Int): n -> dup n", &[("dup", "dup")]).unwrap_err();
    assert_eq!(err.module.as_deref(), Some("lib"));
}

/// A violation in the *main* module is stamped `main`.
#[test]
fn main_module_error_is_stamped_main() {
    // `(n, id n)` uses owned `n` twice — the second use violates rule 10.
    let err = check2("id = T -> T: x -> x", &["id"], "run = Int -> (Int, Int): n -> (n, id n)", &[("id", "id")]).unwrap_err();
    assert_eq!(err.module.as_deref(), Some("main"));
}

/// A clean two-module program passes with no attribution needed.
#[test]
fn clean_two_module_program_passes() {
    let res = check2(
        "dup = T -> (T, Int): x -> (x, 0)",
        &["dup"],
        "run = Int -> (Int, Int): n -> dup n",
        &[("dup", "dup")],
    );
    assert!(res.is_ok());
}
