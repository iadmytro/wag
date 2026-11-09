//! Multi-module pipeline tests.
//!
//! Tests the full resolve → merge → type-check → check pipeline.

use std::sync::atomic::{AtomicU64, Ordering};
use wag_cli::resolve::resolve_modules;
use wag_cli::{compile_multi_module, run_entry_path, RunOptions};

static COUNTER: AtomicU64 = AtomicU64::new(5000);

fn make_dir(files: &[(&str, &str)]) -> std::path::PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("wag_mm_test_{}_{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, content) in files {
        std::fs::write(dir.join(name), content).unwrap();
    }
    dir
}

fn cleanup(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}

// ── Basic multi-module compile ──────────────────────────────────────────

#[test]
fn two_module_compile() {
    let entry = "import
  lib
    double

main = double 21
";
    let lib = "export
  double

double = x -> x * 2
";
    let dir = make_dir(&[("entry.wag", entry), ("lib.wag", lib)]);
    let set = resolve_modules(&dir.join("entry.wag")).expect("should resolve");
    cleanup(&dir);

    let checked = compile_multi_module(&set).expect("should compile");
    // The program should have both `main` and `double` as functions.
    let func_names: Vec<&str> = checked
        .typed()
        .bindings
        .keys()
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(func_names.contains(&"main"), "main should be in bindings");
    assert!(func_names.contains(&"double"), "double should be in bindings");
}

// ── Name collision across modules ───────────────────────────────────────

#[test]
fn cross_module_name_collision() {
    let lib = "export
  foo

foo = 1
";
    // Both modules define `foo` — collision.
    let entry = "import
  lib
    foo

foo = 2
main = 1
";
    let dir = make_dir(&[("entry.wag", entry), ("lib.wag", lib)]);
    let set = resolve_modules(&dir.join("entry.wag")).expect("should resolve");
    cleanup(&dir);

    let result = compile_multi_module(&set);
    let err = result.expect_err("collision should be rejected");
    assert!(
        err.message.contains("name collision"),
        "expected collision error, got: {}",
        err.message
    );
    assert!(err.message.contains("foo"), "error should mention the colliding name");
}

// ── Transitive multi-module ─────────────────────────────────────────────

#[test]
fn three_module_transitive() {
    let entry = "import
  mid
    add_one

main = add_one 41
";
    let mid = "export
  add_one

import
  base
    base_val

add_one = x -> x + base_val
";
    let base = "export
  base_val

base_val = 1
";
    let dir = make_dir(&[
        ("entry.wag", entry),
        ("mid.wag", mid),
        ("base.wag", base),
    ]);
    let set = resolve_modules(&dir.join("entry.wag")).expect("should resolve");
    cleanup(&dir);

    let checked = compile_multi_module(&set).expect("should compile");
    let func_names: Vec<&str> = checked
        .typed()
        .bindings
        .keys()
        .map(|(name, _)| name.as_str())
        .collect();
    assert!(func_names.contains(&"main"));
    assert!(func_names.contains(&"add_one"));
    assert!(func_names.contains(&"base_val"));
}

// ── Single module (no imports) still works ──────────────────────────────

#[test]
fn single_module_no_imports() {
    let entry = "main = 42
";
    let dir = make_dir(&[("entry.wag", entry)]);
    let set = resolve_modules(&dir.join("entry.wag")).expect("should resolve");
    cleanup(&dir);

    let checked = compile_multi_module(&set).expect("should compile");
    assert!(checked.typed().bindings.contains_key(&("main".to_string(), 0)));
}

// ── run_entry_path (full pipeline) ──────────────────────────────────────

#[test]
fn run_entry_path_checked() {
    let entry = "main = 1
";
    let dir = make_dir(&[("entry.wag", entry)]);
    let opts = RunOptions {
        emit_stdout: false,
        print_stats: false,
        stdin_lines: None,
        timeout: std::time::Duration::from_secs(5),
        scheduler: Default::default(),
        argv: vec![],
    };
    let result = run_entry_path(&dir.join("entry.wag"), opts);
    cleanup(&dir);
    // Should succeed (either Checked or Executed).
    result.expect("run_entry_path should succeed");
}

// ── Cross-module ctor name collision (per-module namespacing) ───────────

#[test]
fn cross_module_ctor_collision() {
    // Both modules define a ctor named `Circle` on different datatypes.
    let mod_a = "export
  make_circle
  shape_area

Shape = Circle Int | Square Int

make_circle = Int -> Shape: r -> Circle r
shape_area = Shape -> Int: s -> match s
  Circle r -> r * r
  Square w -> w * w
";
    let mod_b = "export
  make_tok
  tok_kind

Token = Circle Int | Number Int

make_tok = Int -> Token: n -> Circle n
tok_kind = Token -> Int: t -> match t
  Circle r -> r + 100
  Number n -> n
";
    let entry = "import
  a
    make_circle
    shape_area
  b
    make_tok
    tok_kind

main = (shape_area (make_circle 3)) + (tok_kind (make_tok 7))
";
    let dir = make_dir(&[("entry.wag", entry), ("a.wag", mod_a), ("b.wag", mod_b)]);
    let set = resolve_modules(&dir.join("entry.wag")).expect("should resolve");
    cleanup(&dir);

    let checked = compile_multi_module(&set).expect("ctor collision should be resolved by renaming");
    assert!(checked.typed().bindings.contains_key(&("main".to_string(), 0)));
}
