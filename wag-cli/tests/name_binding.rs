//! Name binding tests (D053 prelude-as-module).
//!
//! Tests for: local names, imported names, aliases, `All` expansion,
//! collision detection, and the explicit-prelude rule — a local definition
//! clashing with any imported name (including the prelude) is an error.

use std::sync::atomic::{AtomicU64, Ordering};

use wag_cli::bind::bind_names;
use wag_cli::resolve::{resolve_modules, ModuleSet, ResolvedModule};
use wag_syntax::parse_desugared;

static COUNTER: AtomicU64 = AtomicU64::new(1000);

/// Create a temp directory with the given files. Returns (dir_path, entry_path).
fn make_dir(files: &[(&str, &str)]) -> (std::path::PathBuf, std::path::PathBuf) {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("wag_bind_test_{}_{}", std::process::id(), n));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, content) in files {
        std::fs::write(dir.join(name), content).unwrap();
    }
    let entry = dir.join("entry.wag");
    (dir, entry)
}

fn cleanup(dir: &std::path::Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// Minimal prelude module with a few names (`export ..` makes them importable).
const PRELUDE: &str = "export\n  ..\n\nint_val = 0\nbool_val = 1\n";

fn prelude_module() -> ResolvedModule {
    let program = parse_desugared(PRELUDE).expect("prelude should parse");
    ResolvedModule {
        name: "prelude".to_string(),
        path: std::path::PathBuf::from("prelude.wag"),
        source: PRELUDE.to_string(),
        program,
        imports: Vec::new(),
        package: "<test>".into(),
    }
}


fn make_single_module(name: &str, program: &wag_syntax::Program) -> ModuleSet {
    let module = ResolvedModule {
        name: name.to_string(),
        path: std::path::PathBuf::from(name),
        source: String::new(),
        program: program.clone(),
        imports: Vec::new(),
        package: "<test>".into(),
    };
    ModuleSet { modules: vec![module], entry_index: 0 }
}

// ── Local names ─────────────────────────────────────────────────────────

#[test]
fn local_names_collected() {
    let src = "foo = 1\nBar = Bar Int\n@main = Int -> Int: x -> x\n";
    let program = parse_desugared(src).expect("should parse");
    let module_set = make_single_module("test", &program);

    let ns = bind_names(&module_set).expect("should bind");
    let namespace = &ns["test"];

    assert!(namespace.local.contains("foo"), "function name `foo` should be local");
    assert!(namespace.local.contains("Bar"), "type name `Bar` should be local");
    assert!(namespace.local.contains("@main"), "actor name `@main` should be local");
}

// ── Imported names (named) ──────────────────────────────────────────────

#[test]
fn imported_named() {
    let entry = "import\n  lib\n    foo\n\nmain = 1\n";
    let lib = "export\n  foo\n\nfoo = 42\n";
    let (dir, entry_path) = make_dir(&[("entry.wag", entry), ("lib.wag", lib)]);
    let set = resolve_modules(&entry_path).expect("should resolve");
    cleanup(&dir);

    let ns = bind_names(&set).expect("should bind");
    let entry_ns = &ns["entry"];

    assert!(entry_ns.imported.contains_key("foo"), "`foo` should be imported");
    assert_eq!(entry_ns.imported["foo"].module, "lib");
    assert_eq!(entry_ns.imported["foo"].original, "foo");
}

// ── Aliasing ────────────────────────────────────────────────────────────

#[test]
fn imported_aliased() {
    let entry = "import\n  lib\n    bar <- foo\n\nmain = 1\n";
    let lib = "export\n  foo\n\nfoo = 42\n";
    let (dir, entry_path) = make_dir(&[("entry.wag", entry), ("lib.wag", lib)]);
    let set = resolve_modules(&entry_path).expect("should resolve");
    cleanup(&dir);

    let ns = bind_names(&set).expect("should bind");
    let entry_ns = &ns["entry"];

    // The effective name is `bar` (the alias), not `foo`.
    assert!(entry_ns.imported.contains_key("bar"), "alias `bar` should be imported");
    assert!(!entry_ns.imported.contains_key("foo"), "original `foo` should NOT be in namespace");
    assert_eq!(entry_ns.imported["bar"].original, "foo");
    assert_eq!(entry_ns.imported["bar"].module, "lib");
}

// ── All imports ─────────────────────────────────────────────────────────

#[test]
fn all_import_expands() {
    let entry = "import\n  lib\n    ..\n\nmain = 1\n";
    let lib = "export\n  foo\n  bar\n\nfoo = 1\nbar = 2\n";
    let (dir, entry_path) = make_dir(&[("entry.wag", entry), ("lib.wag", lib)]);
    let set = resolve_modules(&entry_path).expect("should resolve");
    cleanup(&dir);

    let ns = bind_names(&set).expect("should bind");
    let entry_ns = &ns["entry"];

    assert!(entry_ns.imported.contains_key("foo"), "`foo` should be imported via `..`");
    assert!(entry_ns.imported.contains_key("bar"), "`bar` should be imported via `..`");
}

#[test]
fn all_import_from_export_all() {
    // Source module exports `..` (all local names).
    let entry = "import\n  lib\n    ..\n\nmain = 1\n";
    let lib = "export\n  ..\n\nfoo = 1\nbar = 2\n";
    let (dir, entry_path) = make_dir(&[("entry.wag", entry), ("lib.wag", lib)]);
    let set = resolve_modules(&entry_path).expect("should resolve");
    cleanup(&dir);

    let ns = bind_names(&set).expect("should bind");
    let entry_ns = &ns["entry"];

    assert!(entry_ns.imported.contains_key("foo"), "`foo` should be imported via `..` from `export ..`");
    assert!(entry_ns.imported.contains_key("bar"), "`bar` should be imported via `..` from `export ..`");
}

// ── Collision detection ─────────────────────────────────────────────────

#[test]
fn import_collision_same_name() {
    let entry = "import\n  a\n    foo\n  b\n    foo\n\nmain = 1\n";
    let a = "export\n  foo\n\nfoo = 1\n";
    let b = "export\n  foo\n\nfoo = 2\n";
    let (dir, entry_path) = make_dir(&[
        ("entry.wag", entry),
        ("a.wag", a),
        ("b.wag", b),
    ]);
    let set = resolve_modules(&entry_path).expect("should resolve");
    cleanup(&dir);

    let result = bind_names(&set);
    let err = result.expect_err("collision should be an error");
    assert!(
        err.message.contains("import collision"),
        "expected collision error, got: {}",
        err.message
    );
    assert!(err.message.contains("foo"), "error should mention the colliding name");
}

#[test]
fn import_collision_different_names_no_error() {
    // Different names from different modules — no collision.
    let entry = "import\n  a\n    foo\n  b\n    bar\n\nmain = 1\n";
    let a = "export\n  foo\n\nfoo = 1\n";
    let b = "export\n  bar\n\nbar = 2\n";
    let (dir, entry_path) = make_dir(&[
        ("entry.wag", entry),
        ("a.wag", a),
        ("b.wag", b),
    ]);
    let set = resolve_modules(&entry_path).expect("should resolve");
    cleanup(&dir);

    let ns = bind_names(&set).expect("should bind");
    let entry_ns = &ns["entry"];
    assert!(entry_ns.imported.contains_key("foo"));
    assert!(entry_ns.imported.contains_key("bar"));
}

// ── Local vs imported: error (D053) ─────────────────────────────────────

#[test]
fn local_clashes_with_import_is_error() {
    // A local definition with the same name as an import is a compile error.
    let entry = "import\n  lib\n    foo\n\nfoo = 42\nmain = 1\n";
    let lib = "export\n  foo\n\nfoo = 1\n";
    let (dir, entry_path) = make_dir(&[("entry.wag", entry), ("lib.wag", lib)]);
    let set = resolve_modules(&entry_path).expect("should resolve");
    cleanup(&dir);

    let err = bind_names(&set).expect_err("local-vs-import clash should be an error");
    assert!(err.message.contains("name clash"), "got: {}", err.message);
    assert!(err.message.contains("foo"));
}

#[test]
fn local_clashes_with_prelude_import_is_error() {
    // Same rule for the prelude: importing it makes its names reserved.
    let entry = "import\n  prelude\n    ..\n\nint_val = 99\nmain = 1\n";
    let (dir, entry_path) = make_dir(&[("entry.wag", entry), ("prelude.wag", PRELUDE)]);
    let set = resolve_modules(&entry_path).expect("should resolve");
    cleanup(&dir);

    let err = bind_names(&set).expect_err("local-vs-prelude clash should be an error");
    assert!(err.message.contains("name clash"), "got: {}", err.message);
    assert!(err.message.contains("int_val"));
}

// ── Prelude (explicit import, D053) ─────────────────────────────────────

#[test]
fn prelude_not_visible_without_import() {
    // The prelude is an ordinary module: its names are not in scope unless
    // the entry imports it.
    let src = "main = 1\n";
    let program = parse_desugared(src).expect("should parse");
    let mut modules = vec![prelude_module()];
    modules.push(ResolvedModule {
        name: "entry".to_string(),
        path: std::path::PathBuf::from("entry.wag"),
        source: src.to_string(),
        program,
        imports: Vec::new(),
        package: "<test>".into(),
    });
    let set = ModuleSet { modules, entry_index: 1 };

    let ns = bind_names(&set).expect("should bind");
    assert_eq!(ns["entry"].resolve("int_val"), None, "prelude name must not leak without import");
}

#[test]
fn prelude_imported_explicitly() {
    // `import prelude ..` brings the names in; they resolve to module "prelude".
    let entry = "import\n  prelude\n    ..\n\nmain = int_val + bool_val\n";
    let (dir, entry_path) = make_dir(&[("entry.wag", entry), ("prelude.wag", PRELUDE)]);
    let set = resolve_modules(&entry_path).expect("should resolve");
    cleanup(&dir);

    let ns = bind_names(&set).expect("should bind");
    let entry_ns = &ns["entry"];

    assert!(entry_ns.imported.contains_key("int_val"), "`int_val` should be imported from prelude");
    assert_eq!(entry_ns.resolve("int_val"), Some("prelude"));
}

// ── Re-export (`export name from module`) ──────────────────────────────

#[test]
fn reexport_resolves_to_true_source() {
    // leaf defines `foo`; facade re-exports it; entry imports from facade.
    // The binding must point to `leaf` (the original definition site).
    let leaf = "export\n  foo\n\nfoo = 42\n";
    let facade = "export\n  foo from leaf\n";
    let entry = "import\n  facade\n    foo\n\nmain = foo\n";
    let (dir, entry_path) = make_dir(&[
        ("entry.wag", entry),
        ("facade.wag", facade),
        ("leaf.wag", leaf),
    ]);
    let set = resolve_modules(&entry_path).expect("should resolve");
    cleanup(&dir);

    let ns = bind_names(&set).expect("should bind");
    let entry_ns = &ns["entry"];

    assert!(entry_ns.imported.contains_key("foo"), "`foo` should be imported");
    assert_eq!(
        entry_ns.resolve("foo"),
        Some("leaf"),
        "re-exported name must resolve to the true source module (leaf), not the facade"
    );
}

#[test]
fn reexport_all_expansion_uses_true_source() {
    // `import facade ..` should expand re-exported names with their true source.
    let leaf = "export\n  foo\n  bar\n\nfoo = 1\nbar = 2\n";
    let facade = "export\n  foo from leaf\n  bar from leaf\n";
    let entry = "import\n  facade\n    ..\n\nmain = foo + bar\n";
    let (dir, entry_path) = make_dir(&[
        ("entry.wag", entry),
        ("facade.wag", facade),
        ("leaf.wag", leaf),
    ]);
    let set = resolve_modules(&entry_path).expect("should resolve");
    cleanup(&dir);

    let ns = bind_names(&set).expect("should bind");
    let entry_ns = &ns["entry"];

    assert_eq!(entry_ns.resolve("foo"), Some("leaf"));
    assert_eq!(entry_ns.resolve("bar"), Some("leaf"));
}
