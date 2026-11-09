//! Fixture inventory harness: every non-actor fixture is present and valid UTF-8.
//!
//! Later milestones extend this loop (tokenize → parse → typecheck → …)
//! instead of inventing a new runner.

use std::fs;
use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
    // tests/ → wag-cli/ → workspace root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("wag-cli parent")
        .join("fixtures")
}

fn is_actor_fixture(path: &Path, text: &str) -> bool {
    text.lines().any(|l| l.contains("ACTOR FIXTURE"))
        || path
            .components()
            .any(|c| c == std::path::Component::Normal("actors".as_ref()))
}

#[test]
fn non_actor_fixtures_present_and_utf8() {
    let dir = fixtures_dir();
    assert!(
        dir.is_dir(),
        "fixtures directory missing at {}",
        dir.display()
    );

    let mut entries: Vec<PathBuf> = Vec::new();
    collect_wag_files(&dir, &mut entries);
    entries.sort();

    assert!(
        !entries.is_empty(),
        "expected at least one .wag fixture under {}",
        dir.display()
    );

    let mut ok = 0usize;
    let mut skipped_actor = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for path in &entries {
        let bytes = match fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                failures.push(format!("{}: read error: {e}", path.display()));
                continue;
            }
        };
        let text = match std::str::from_utf8(&bytes) {
            Ok(t) => t,
            Err(e) => {
                failures.push(format!("{}: invalid UTF-8: {e}", path.display()));
                continue;
            }
        };
        if is_actor_fixture(path, text) {
            skipped_actor += 1;
            continue;
        }
        if text.trim().is_empty() {
            failures.push(format!("{}: empty fixture", path.display()));
            continue;
        }
        ok += 1;
    }

    assert!(
        failures.is_empty(),
        "fixture harness failures:\n{}",
        failures.join("\n")
    );
    assert!(
        ok > 0,
        "no non-actor fixtures checked under {}",
        dir.display()
    );

    eprintln!(
        "fixtures_inventory: {ok}/{ok} non-actor fixture files present and readable \
         ({skipped_actor} actor fixtures skipped)"
    );
}

fn collect_wag_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.is_dir() {
                collect_wag_files(&p, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("wag") {
                out.push(p);
            }
        }
    }
}

/// Full inventory: every expected fixture name exists on disk (including actors).
#[test]
fn expected_fixture_names_exist() {
    let dir = fixtures_dir();
    let expected = [
        "basics/factorial.wag",
        "basics/fibonacci.wag",
        "lists/sum_list.wag",
        "lists/length.wag",
        "lists/is_empty.wag",
        "lists/reverse.wag",
        "lists/map.wag",
        "lists/filter.wag",
        "lists/max_list.wag",
        "basics/gcd.wag",
        "basics/pow.wag",
        "lists/contains.wag",
        "lists/count.wag",
        "lists/insertion_sort.wag",
        "actors/counter_supervised.wag",
        "io/main_std.wag",
        "linearity/copy_vs_borrow.wag",
        "lists/is_sorted.wag",
        "actors/monitor.wag",
        "basics/mutual_recursion.wag",
        "basics/higher_order_cycles.wag",
        "types/field_access.wag",
    ];
    let mut missing = Vec::new();
    for name in expected {
        if !dir.join(name).is_file() {
            missing.push(name);
        }
    }
    assert!(
        missing.is_empty(),
        "missing fixtures: {}",
        missing.join(", ")
    );
}
