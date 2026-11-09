//! Fixture tokenize harness: every non-actor fixture tokenizes with layout.

use std::fs;
use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
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

#[test]
fn non_actor_fixtures_tokenize() {
    let dir = fixtures_dir();
    assert!(dir.is_dir(), "fixtures directory missing at {}", dir.display());

    let mut entries: Vec<PathBuf> = Vec::new();
    collect_wag_files(&dir, &mut entries);
    entries.sort();

    let mut ok = 0usize;
    let mut skipped_actor = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for path in &entries {
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                failures.push(format!("{}: read error: {e}", path.display()));
                continue;
            }
        };
        if is_actor_fixture(path, &text) {
            skipped_actor += 1;
            continue;
        }
        match wag_syntax::lex_with_layout(&text) {
            Ok(tokens) => {
                assert!(
                    !tokens.is_empty(),
                    "{}: expected some tokens",
                    path.display()
                );
                ok += 1;
            }
            Err(e) => failures.push(format!("{}: {e}", path.display())),
        }
    }

    assert!(
        failures.is_empty(),
        "fixture tokenize failures:\n{}",
        failures.join("\n")
    );
    assert!(ok > 0, "no non-actor fixtures tokenized");
    eprintln!(
        "fixtures_tokenize: {ok}/{ok} non-actor fixtures tokenized with layout \
         ({skipped_actor} actor fixtures skipped)"
    );
}
