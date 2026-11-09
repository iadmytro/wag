//! Integration tests for the `wagc test` runner core (`run_test_file`).
//!
//! Each test builds an isolated `src/` + `test/` pair under a unique temp dir
//! (unique per test so parallel test threads don't collide) and checks that the
//! merged-module compile + `test_main` invocation reports Pass/Fail correctly.

use std::fs;
use std::path::{Path, PathBuf};

use wag_cli::{mirror_src_path, run_test_file, TestOutcome};

/// Unique temp root for one test: `<tmp>/wag_test_runner_<pid>_<tag>/{src,test}`.
fn tmp_root(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wag_test_runner_{}_{}", std::process::id(), tag));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(d.join("src")).unwrap();
    fs::create_dir_all(d.join("test")).unwrap();
    d
}

fn write_pair(root: &Path, src: &str, test: &str) {
    fs::write(root.join("src/foo.wag"), src).unwrap();
    fs::write(root.join("test/foo.wag"), test).unwrap();
}

#[test]
fn mirror_replaces_first_test_component() {
    let p = PathBuf::from("/x/examples/parser/test/parse.wag");
    assert_eq!(mirror_src_path(&p).as_deref(), Some(Path::new("/x/examples/parser/src/parse.wag")));
    // No `test` component → None.
    assert_eq!(mirror_src_path(Path::new("/x/src/foo.wag")), None);
}

#[test]
fn test_passes_when_asserts_hold() {
    // Multiple asserts exercise the block-sequencing path: bare Unit statements
    // followed by a final expression must desugar to `Seq` (sequenced for effect).
    // Regression guard for that.
    let root = tmp_root("pass");
    write_pair(
        &root,
        "double = Int -> Int: x -> x * 2\n",
        "test_main = Unit:  ->\n  assert (double 4 == 8) \"d1\"\n  assert (double 5 == 10) \"d2\"\n  ()\n",
    );
    match run_test_file(&root.join("test/foo.wag")) {
        Ok(TestOutcome::Pass) => {}
        other => panic!("expected Pass, got {other:?}"),
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_fails_when_assert_breaks() {
    let root = tmp_root("fail");
    write_pair(
        &root,
        "double = Int -> Int: x -> x * 2\n",
        "test_main = Unit:  ->\n  assert (double 4 == 9) \"wrong value\"\n  ()\n",
    );
    match run_test_file(&root.join("test/foo.wag")) {
        Ok(TestOutcome::Fail(msg)) => assert!(msg.contains("wrong value"), "got: {msg}"),
        other => panic!("expected Fail, got {other:?}"),
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_reaches_private_functions() {
    // `secret` is NOT exported from foo.wag; the merge must still expose it.
    let root = tmp_root("private");
    write_pair(
        &root,
        "secret = Int -> Int: x -> x + 100\n",
        "test_main = Unit:  ->\n  assert (secret 1 == 101) \"p\"\n  ()\n",
    );
    match run_test_file(&root.join("test/foo.wag")) {
        Ok(TestOutcome::Pass) => {}
        other => panic!("expected Pass via private access, got {other:?}"),
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn missing_mirror_is_an_error() {
    let root = tmp_root("nomirror");
    // Only the test file exists; no src/foo.wag.
    fs::write(root.join("test/foo.wag"), "test_main = Unit:  ->\n  ()\n").unwrap();
    assert!(run_test_file(&root.join("test/foo.wag")).is_err());
    let _ = fs::remove_dir_all(&root);
}
