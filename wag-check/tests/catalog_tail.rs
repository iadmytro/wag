//! Milestone 8 — pass 2: tail-cycle law (rule 1).

use std::path::PathBuf;

use wag_check::{check_with_prelude, Rule};
use wag_interp::{load_checked, Engine, Value};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(name)).expect(name)
}

fn assert_ok(src: &str) {
    check_with_prelude(src).unwrap_or_else(|e| panic!("expected ok, got {e}"));
}

fn assert_err_rule(src: &str, rule: Rule) {
    match check_with_prelude(src) {
        Ok(_) => panic!("expected error for rule {rule:?}"),
        Err(e) => {
            let msg = e.to_string();
            let needle = match rule {
                Rule::TailCycle => "rule 1",
                other => panic!("unexpected rule in m8 test: {other:?}"),
            };
            assert!(
                msg.contains(needle),
                "expected diagnostic mentioning {needle}, got: {msg}"
            );
        }
    }
}

fn even_odd_src() -> String {
    r#"
isEvenHelper = Int -> Bool: n -> if
  n == 0 -> True
  _ -> isOddHelper (n - 1)

isOddHelper = Int -> Bool: n -> if
  n == 0 -> False
  _ -> isEvenHelper (n - 1)
"#
    .into()
}

fn ping_pong_bad_src() -> String {
    r#"
pingBad = Int -> Int: n -> if
  n == 0 -> 0
  _ -> (pongBad (n - 1)) + 1

pongBad = Int -> Int: n -> if
  n == 0 -> 0
  _ -> pingBad (n - 1)
"#
    .into()
}

fn use_both_src() -> String {
    // List-migrated (D062): 8_6 provides increment + specMap; the prelude
    // (merged by check_with_prelude) supplies listNew.
    let mut s = String::new();
    s.push_str(&read_fixture("lists/map.wag"));
    s.push('\n');
    s.push_str(
        r#"
negate = Int -> Int: x -> 0 - x

useBoth = List Int, List Int -> (List Int, List Int): xs, ys ->
  incremented = specMap increment xs
  negated = specMap negate ys
  (incremented, negated)
"#,
    );
    s
}

fn bad_apply_src() -> String {
    // List-migrated (D062): one-element list via append chain; the higher-order
    // cycle is recur → map(badApply) ↔ badApply → recur.
    let mut s = String::new();
    s.push_str(&read_fixture("lists/length.wag"));
    s.push('\n');
    s.push_str(
        r#"
badApply = Int -> Int: x ->
  l0 = listNew
  l1 = l0.append x
  specLength (recur l1)

recur = List Int -> List Int: xs -> map badApply xs
"#,
    );
    s
}

#[test]
fn m8_positive_even_odd_tail_cycle() {
    let checked = check_with_prelude(&even_odd_src()).unwrap();
    assert!(checked.cyclic_fns().contains("isEvenHelper"));
    assert!(checked.cyclic_fns().contains("isOddHelper"));
    assert!(checked
        .tail_info()
        .edges
        .iter()
        .any(|e| e.from == "isEvenHelper" && e.to == "isOddHelper" && e.is_tail));
    assert!(checked
        .tail_info()
        .edges
        .iter()
        .any(|e| e.from == "isOddHelper" && e.to == "isEvenHelper" && e.is_tail));
}

#[test]
fn m8_negative_ping_pong_non_tail_cycle() {
    assert_err_rule(&ping_pong_bad_src(), Rule::TailCycle);
}

#[test]
fn m8_positive_use_both() {
    assert_ok(&use_both_src());
}

#[test]
fn m8_negative_bad_apply_ho_cycle() {
    assert_err_rule(&bad_apply_src(), Rule::TailCycle);
}

#[test]
fn m8_positive_actor_counter_supervised() {
    assert_ok(&read_fixture("actors/counter_supervised.wag"));
}

#[test]
fn m8_positive_actor_monitor() {
    assert_ok(&read_fixture("actors/monitor.wag"));
}

#[test]
fn m8_vm_even_odd_uses_trampoline() {
    // Accepted mutual tail cycle runs via Control::TailCall (deep recursion).
    let checked = check_with_prelude(&even_odd_src()).unwrap();
    assert!(checked.cyclic_fns().contains("isEvenHelper"));
    let loaded = load_checked(checked.program(), checked.typed(), true);
    let mut eng = Engine::new(&loaded);
    let fid = loaded.fn_id("isEvenHelper").expect("isEvenHelper");
    // Large enough that a non-trampolined mutual recursion would overflow.
    let n = Value::Int(20_000);
    let v = eng.call(fid, vec![n]).expect("run");
    assert_eq!(v, Value::Bool(true));
}

#[test]
fn m8_full_8_19_rejected() {
    // Fixture mixes accepted + rejected defs; whole module must fail rule 1.
    assert_err_rule(&read_fixture("basics/mutual_recursion.wag"), Rule::TailCycle);
}
