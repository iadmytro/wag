//! Module resolution tests.
//!
//! Tests for: sibling discovery, DAG building, topological sort, cycle rejection.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use wag_cli::resolve::{resolve_modules, ModuleSet};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Create a temp directory with the given files (name → content).
/// Returns the path to the directory. The caller is responsible for cleanup.
fn make_module_dir(files: &[(&str, &str)]) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "wag_resolve_test_{}_{}",
        std::process::id(),
        n
    ));
    fs::create_dir_all(&dir).unwrap();
    for (name, content) in files {
        fs::write(dir.join(name), content).unwrap();
    }
    dir
}

fn cleanup(dir: &PathBuf) {
    let _ = fs::remove_dir_all(dir);
}

/// A minimal valid WAG program (single function, no imports/exports).
const SIMPLE: &str = "main = 1\n";

/// Entry that imports `lib` (takes `foo`).
const ENTRY_IMPORTS_LIB: &str = "import
  lib
    foo

main = 1
";

/// `lib` exports `foo`.
const LIB_EXPORTS_FOO: &str = "export
  foo

foo = 42
";

/// Entry that imports `mid`, which imports `base`.
const ENTRY_IMPORTS_MID: &str = "import
  mid
    bar

main = 1
";

const MID_IMPORTS_BASE: &str = "export
  bar

import
  base
    baz

bar = 1
";

const BASE_EXPORTS_BAZ: &str = "export
  baz

baz = 2
";

// ── Basic resolution ────────────────────────────────────────────────────

#[test]
fn single_module_no_imports() {
    let dir = make_module_dir(&[("entry.wag", SIMPLE)]);
    let result = resolve_modules(&dir.join("entry.wag"));
    cleanup(&dir);
    let set: ModuleSet = result.expect("single module should resolve");
    assert_eq!(set.modules.len(), 1);
    assert_eq!(set.entry_index, 0);
    assert_eq!(set.entry().name, "entry");
    assert!(set.entry().imports.is_empty());
}

#[test]
fn entry_imports_one_module() {
    let dir = make_module_dir(&[
        ("entry.wag", ENTRY_IMPORTS_LIB),
        ("lib.wag", LIB_EXPORTS_FOO),
    ]);
    let result = resolve_modules(&dir.join("entry.wag"));
    cleanup(&dir);
    let set: ModuleSet = result.expect("entry+lib should resolve");
    assert_eq!(set.modules.len(), 2);
    // Topo order: lib (callee) first, entry (caller) last.
    assert_eq!(set.modules[0].name, "lib");
    assert_eq!(set.modules[1].name, "entry");
    assert_eq!(set.entry_index, 1);
    assert_eq!(set.entry().imports, vec!["lib".to_string()]);
}

#[test]
fn transitive_imports() {
    let dir = make_module_dir(&[
        ("entry.wag", ENTRY_IMPORTS_MID),
        ("mid.wag", MID_IMPORTS_BASE),
        ("base.wag", BASE_EXPORTS_BAZ),
    ]);
    let result = resolve_modules(&dir.join("entry.wag"));
    cleanup(&dir);
    let set: ModuleSet = result.expect("transitive imports should resolve");
    assert_eq!(set.modules.len(), 3);
    // Topo order: base → mid → entry.
    assert_eq!(set.modules[0].name, "base");
    assert_eq!(set.modules[1].name, "mid");
    assert_eq!(set.modules[2].name, "entry");
    assert_eq!(set.entry_index, 2);
}

// ── Cycle detection ─────────────────────────────────────────────────────

#[test]
fn cycle_two_modules() {
    let a = "import
  b
    x

main = 1
";
    let b = "import
  a
    y

x = 1
";
    let dir = make_module_dir(&[("a.wag", a), ("b.wag", b)]);
    let result = resolve_modules(&dir.join("a.wag"));
    cleanup(&dir);
    let err = result.expect_err("cycle should be rejected");
    assert!(
        err.message.contains("import cycle"),
        "expected cycle error, got: {}",
        err.message
    );
    assert!(err.message.contains("a"), "cycle path should mention `a`");
    assert!(err.message.contains("b"), "cycle path should mention `b`");
    assert_eq!(err.stage, wag_cli::PipelineStage::Resolve);
}

#[test]
fn self_import() {
    let a = "import
  a
    x

main = 1
";
    let dir = make_module_dir(&[("a.wag", a)]);
    let result = resolve_modules(&dir.join("a.wag"));
    cleanup(&dir);
    // Self-import: `a` is in the resolved set (it's the entry), so the BFS
    // skips re-reading it. The DFS will find `a` in-progress → cycle.
    let err = result.expect_err("self-import should be rejected");
    assert!(
        err.message.contains("import cycle"),
        "expected cycle error, got: {}",
        err.message
    );
    assert!(err.message.contains("a"), "cycle path should mention `a`");
}

// ── Missing module ──────────────────────────────────────────────────────

#[test]
fn missing_module() {
    let entry = "import
  nonexistent
    foo

main = 1
";
    let dir = make_module_dir(&[("entry.wag", entry)]);
    let result = resolve_modules(&dir.join("entry.wag"));
    cleanup(&dir);
    let err = result.expect_err("missing module should be an error");
    assert!(
        err.message.contains("not found"),
        "expected 'not found', got: {}",
        err.message
    );
    assert!(
        err.message.contains("nonexistent"),
        "error should mention the missing module name"
    );
    assert!(
        err.hint.is_some(),
        "should provide a hint about creating the file"
    );
}

// ── Diamond dependency ──────────────────────────────────────────────────

#[test]
fn diamond_dependency() {
    // entry imports b and c; both import d.
    let entry = "import
  b
    x
  c
    y

main = 1
";
    let b = "export
  x

import
  d
    z

x = 1
";
    let c = "export
  y

import
  d
    z

y = 2
";
    let d = "export
  z

z = 3
";
    let dir = make_module_dir(&[
        ("entry.wag", entry),
        ("b.wag", b),
        ("c.wag", c),
        ("d.wag", d),
    ]);
    let result = resolve_modules(&dir.join("entry.wag"));
    cleanup(&dir);
    let set: ModuleSet = result.expect("diamond should resolve");
    assert_eq!(set.modules.len(), 4);
    // `d` must be first (both b and c depend on it).
    assert_eq!(set.modules[0].name, "d");
    // `entry` must be last.
    assert_eq!(set.modules[3].name, "entry");
    assert_eq!(set.entry_index, 3);
    // b and c are in between (order between them is unspecified).
    let mid_names: Vec<&str> = set.modules[1..3].iter().map(|m| m.name.as_str()).collect();
    assert!(mid_names.contains(&"b"));
    assert!(mid_names.contains(&"c"));
}

// ── Entry file not found ────────────────────────────────────────────────

#[test]
fn entry_file_not_found() {
    let dir = make_module_dir(&[]);
    let result = resolve_modules(&dir.join("nonexistent.wag"));
    cleanup(&dir);
    let err = result.expect_err("missing entry should be an error");
    assert!(
        err.message.contains("cannot read"),
        "expected 'cannot read', got: {}",
        err.message
    );
    assert_eq!(err.stage, wag_cli::PipelineStage::Resolve);
}
