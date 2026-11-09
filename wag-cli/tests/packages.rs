//! Package (wag.toml) fixture harness — Phase 2 cross-package resolution.
//!
//! Drives `.wag` fixture directories with `wag.toml` manifests through the
//! full pipeline (`resolve_packages → merge → type-check → check → load`).

use std::path::PathBuf;
use std::time::Duration;

use wag_cli::{run_entry_path, PipelineOk, RunOptions};

fn packages_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
        .join("packages")
}

fn opts() -> RunOptions {
    RunOptions {
        emit_stdout: false,
        print_stats: false,
        stdin_lines: None,
        timeout: Duration::from_secs(5),
        scheduler: Default::default(),
        argv: vec![],
    }
}

// ── Positive fixtures ───────────────────────────────────────────────────

#[test]
fn cross_pkg_import() {
    let entry = packages_dir().join("cross_pkg").join("app").join("main.wag");
    let result = run_entry_path(&entry, opts());
    match result {
        Ok(PipelineOk::Checked(_)) => {}
        Ok(PipelineOk::Executed(_)) => {}
        Err((src, diag)) => panic!("cross_pkg failed: {diag}\nsource:\n{src}"),
    }
}

#[test]
fn transitive_deps() {
    let entry = packages_dir().join("transitive").join("app").join("main.wag");
    let result = run_entry_path(&entry, opts());
    match result {
        Ok(PipelineOk::Checked(_)) => {}
        Ok(PipelineOk::Executed(_)) => {}
        Err((src, diag)) => panic!("transitive failed: {diag}\nsource:\n{src}"),
    }
}

// ── Negative fixtures ───────────────────────────────────────────────────

#[test]
fn name_collision_across_packages() {
    let entry = packages_dir().join("collision").join("app").join("main.wag");
    let result = run_entry_path(&entry, opts());
    match result {
        Ok(_) => panic!("collision should have failed"),
        Err((_src, diag)) => {
            assert!(
                diag.message.contains("not unique"),
                "expected uniqueness error, got: {}",
                diag.message
            );
        }
    }
}
