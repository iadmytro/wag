//! Multi-module fixture harness.
//!
//! Drives `.wag` fixture directories through the full multi-module pipeline
//! (`resolve → merge → type-check → check → load`).

use std::path::PathBuf;
use std::time::Duration;

use wag_cli::{run_entry_path, PipelineOk, RunOptions};

fn modules_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
        .join("modules")
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
fn two_module() {
    let entry = modules_dir().join("two_module").join("entry.wag");
    let result = run_entry_path(&entry, opts());
    match result {
        Ok(PipelineOk::Checked(_)) => {}
        Ok(PipelineOk::Executed(_)) => {}
        Err((src, diag)) => panic!("two_module failed: {diag}\nsource:\n{src}"),
    }
}

#[test]
fn three_module_transitive() {
    let entry = modules_dir().join("three_module").join("entry.wag");
    let result = run_entry_path(&entry, opts());
    match result {
        Ok(PipelineOk::Checked(_)) => {}
        Ok(PipelineOk::Executed(_)) => {}
        Err((src, diag)) => panic!("three_module failed: {diag}\nsource:\n{src}"),
    }
}

#[test]
fn alias_import() {
    let entry = modules_dir().join("alias").join("entry.wag");
    let result = run_entry_path(&entry, opts());
    match result {
        Ok(PipelineOk::Checked(_)) => {}
        Ok(PipelineOk::Executed(_)) => {}
        Err((src, diag)) => panic!("alias failed: {diag}\nsource:\n{src}"),
    }
}

#[test]
fn all_import() {
    let entry = modules_dir().join("all_import").join("entry.wag");
    let result = run_entry_path(&entry, opts());
    match result {
        Ok(PipelineOk::Checked(_)) => {}
        Ok(PipelineOk::Executed(_)) => {}
        Err((src, diag)) => panic!("all_import failed: {diag}\nsource:\n{src}"),
    }
}

#[test]
fn reexport() {
    let entry = modules_dir().join("reexport").join("entry.wag");
    let result = run_entry_path(&entry, opts());
    match result {
        Ok(PipelineOk::Checked(_)) => {}
        Ok(PipelineOk::Executed(_)) => {}
        Err((src, diag)) => panic!("reexport failed: {diag}\nsource:\n{src}"),
    }
}

// ── Negative fixtures ───────────────────────────────────────────────────

#[test]
fn collision_rejected() {
    let entry = modules_dir().join("collision").join("entry.wag");
    let result = run_entry_path(&entry, opts());
    match result {
        Ok(_) => panic!("collision should be rejected"),
        Err((_src, diag)) => {
            assert!(
                diag.message.contains("name collision"),
                "expected collision error, got: {}",
                diag.message
            );
        }
    }
}

#[test]
fn private_access_rejected() {
    let entry = modules_dir().join("private_access").join("entry.wag");
    let result = run_entry_path(&entry, opts());
    match result {
        Ok(_) => panic!("importing a non-exported name should be rejected"),
        Err((_src, diag)) => {
            // The name `private_fn` is not in the import env, so the type
            // checker should report an unresolved/unknown name.
            assert!(
                diag.message.contains("private_fn") || diag.message.contains("not found") || diag.message.contains("unknown"),
                "expected unresolved name error, got: {}",
                diag.message
            );
        }
    }
}
