//! Fixture parse harness: every non-actor fixture parses to an AST.

use std::fs;
use std::path::{Path, PathBuf};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("wag-cli parent")
        .join("fixtures")
}

/// Fixtures in the spec table (doc/wag-language-spec.md §8).
fn is_m2_fixture(name: &str) -> bool {
    matches!(
        name,
        "basics/factorial.wag"
            | "basics/fibonacci.wag"
            | "lists/sum_list.wag"
            | "lists/length.wag"
            | "lists/is_empty.wag"
            | "lists/reverse.wag"
            | "lists/map.wag"
            | "lists/filter.wag"
            | "lists/max_list.wag"
            | "basics/gcd.wag"
            | "basics/pow.wag"
            | "lists/contains.wag"
            | "lists/count.wag"
            | "lists/insertion_sort.wag"
            | "actors/counter_supervised.wag"
            | "io/main_std.wag"
            | "linearity/copy_vs_borrow.wag"
            | "lists/is_sorted.wag"
            | "actors/monitor.wag"
            | "basics/mutual_recursion.wag"
            | "basics/higher_order_cycles.wag"
            | "types/field_access.wag"
    )
}

fn is_rejected_later(name: &str) -> bool {
    name == "basics/higher_order_cycles.wag"
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
fn m2_fixtures_parse() {
    let dir = fixtures_dir();
    assert!(dir.is_dir(), "fixtures directory missing at {}", dir.display());

    let mut entries: Vec<PathBuf> = Vec::new();
    collect_wag_files(&dir, &mut entries);
    entries.sort();

    let mut ok = 0usize;
    let mut rejected_later = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for path in &entries {
        let name = path
            .strip_prefix(&dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if !is_m2_fixture(&name) {
            continue;
        }
        let text = match fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                failures.push(format!("{name}: read error: {e}"));
                continue;
            }
        };
        match wag_syntax::parse(&text) {
            Ok(prog) => {
                assert!(
                    !prog.items.is_empty(),
                    "{name}: expected at least one top-level item"
                );
                ok += 1;
                if is_rejected_later(&name) {
                    rejected_later += 1;
                }
            }
            Err(e) => failures.push(format!("{name}: {e}")),
        }
    }

    assert!(
        failures.is_empty(),
        "fixture parse failures:\n{}",
        failures.join("\n")
    );
    assert_eq!(ok, 22, "expected 22 fixtures to parse, got {ok}");
    eprintln!(
        "fixtures_m2: {ok}/22 fixtures parsed ({rejected_later} marked parse-ok / check-later)"
    );
}

/// Structural snapshot-style checks for representative fixtures.
#[test]
fn m2_ast_shape_snapshots() {
    let dir = fixtures_dir();

    // 8_1: two funcs, if + desugared do in helper
    {
        let src = fs::read_to_string(dir.join("basics/factorial.wag")).unwrap();
        let p = wag_syntax::parse(&src).expect("8_1");
        assert_eq!(p.items.len(), 2);
        match &p.items[0] {
            wag_syntax::Item::Func(f) => {
                assert_eq!(f.name, "factHelper");
                assert!(matches!(f.body, wag_syntax::Expr::If { .. }));
            }
            _ => panic!("expected func"),
        }
    }

    // 8_6: higher-order param type + flat application in body of map
    {
        let src = fs::read_to_string(dir.join("lists/map.wag")).unwrap();
        let p = wag_syntax::parse(&src).expect("8_6");
        let map = p
            .items
            .iter()
            .find_map(|i| match i {
                wag_syntax::Item::Func(f) if f.name == "specMap" => Some(f),
                _ => None,
            })
            .expect("specMap");
        match map.signature.as_ref().unwrap() {
            wag_syntax::Type::Func(params, _) => {
                assert!(matches!(params[0], wag_syntax::Type::Func(_, _)));
            }
            _ => panic!("map sig"),
        }
        // List migration (D062): the body is a Let chain (`o0 = listNew`) whose
        // tail is the flat specMapH application — look through the Lets.
        let mut expr = &map.body;
        while let wag_syntax::Expr::Bind { body, .. } = expr {
            expr = body;
        }
        match expr {
            wag_syntax::Expr::Apply { fun, args, .. } => {
                assert!(matches!(
                    fun.as_ref(),
                    wag_syntax::Expr::Var { name, .. } if name == "specMapH"
                ));
                assert_eq!(args.len(), 3);
            }
            other => panic!("map body: {other:?}"),
        }
    }

    // 8_13: multiple helpers present
    {
        let src = fs::read_to_string(dir.join("lists/insertion_sort.wag")).unwrap();
        let p = wag_syntax::parse(&src).expect("8_13");
        let names: Vec<_> = p
            .items
            .iter()
            .filter_map(|i| match i {
                wag_syntax::Item::Func(f) => Some(f.name.as_str()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"insertHelper"));
        assert!(names.contains(&"insertionSort"));
    }

    // 8_20: parses including rejected-later defs
    {
        let src = fs::read_to_string(dir.join("basics/higher_order_cycles.wag")).unwrap();
        let p = wag_syntax::parse(&src).expect("8_20");
        let names: Vec<_> = p
            .items
            .iter()
            .filter_map(|i| match i {
                wag_syntax::Item::Func(f) => Some(f.name.as_str()),
                _ => None,
            })
            .collect();
        assert!(names.contains(&"badApply"));
        assert!(names.contains(&"recur"));
        assert!(names.contains(&"useBoth"));
    }
}
