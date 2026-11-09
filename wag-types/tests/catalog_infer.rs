//! Milestone 4 — type inference, namespacing, arm-join (§3.6, no Option lift).
//! Recursive user types are permitted (§7 rule 14 classifies them non-pointer-free).

use std::fs;
use std::path::PathBuf;

use wag_types::{check, Stage, Ty};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("fixtures")
}

fn parse_check(src: &str) -> wag_types::TypedProgram {
    let mut p = wag_syntax::parse(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    // D060: early desugar of method-body bare names (pipeline stage).
    wag_syntax::desugar_method_bare_names(&mut p);
    check(&p).unwrap_or_else(|e| panic!("type error: {e}"))
}

fn parse_check_with_prelude(src: &str) -> wag_types::TypedProgram {
    let mut full = load_prelude();
    full.push('\n');
    // Strip any `import prelude ..` prefix — the prelude is already inlined.
    let stripped = src.strip_prefix("import\n  prelude\n    ..").map(|s| s.trim_start()).unwrap_or(src);
    full.push_str(stripped);
    parse_check(&full)
}

fn expect_err_stage(src: &str, stage: Stage) {
    let p = wag_syntax::parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    match check(&p) {
        Ok(t) => panic!(
            "expected error at {stage:?}, got OK bindings={:?}",
            t.bindings
        ),
        Err(e) => assert_eq!(
            e.stage, stage,
            "expected stage {stage:?}, got {:?} ({})",
            e.stage, e.message
        ),
    }
}

fn load(name: &str) -> String {
    fs::read_to_string(fixtures_dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Raw prelude source — for fixtures migrated to List (D062), which reference
/// prelude names. Merging mirrors `wag_check::check_str` (D024/D043).
fn load_prelude() -> String {
    fs::read_to_string(
        fixtures_dir()
            .parent()
            .unwrap()
            .join("prelude/prelude.wag"),
    )
    .expect("prelude/prelude.wag")
}

fn with_deps(name: &str) -> String {
    let mut src = String::new();
    match name {
        // List-migrated fixtures (D062) reference prelude names — merge the
        // prelude first, mirroring check_str (D024/D043). Batch 1 + batch 2 are
        // all self-contained over the prelude.
        "lists/sum_list.wag" | "lists/length.wag" | "lists/reverse.wag" | "lists/map.wag"
        | "lists/filter.wag" | "lists/max_list.wag" | "lists/contains.wag"
        | "lists/count.wag" | "lists/insertion_sort.wag" | "lists/is_sorted.wag" => {
            src.push_str(&load_prelude());
            src.push('\n');
        }
        "lists/is_empty.wag" => {
            // describe reuses sumList from 8_3 — merged with that fixture's own
            // testReport sliced off to avoid a name collision.
            src.push_str(&load_prelude());
            src.push('\n');
            let dep = load("lists/sum_list.wag");
            match dep.find("testReport") {
                Some(i) => src.push_str(&dep[..i]),
                None    => src.push_str(&dep),
            }
            src.push('\n');
        }
        _ => {}
    }
    src.push_str(&load(name));
    src
}

#[test]
fn m4_core_numeric_fixtures() {
    for name in [
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
        "lists/is_sorted.wag",
        "basics/mutual_recursion.wag",
    ] {
        let src = with_deps(name);
        let t = parse_check(&src);
        assert!(!t.bindings.is_empty(), "{name} produced no bindings");
    }
}

#[test]
fn m4_8_19_and_8_20_typecheck() {
    // 8_19 is self-contained.
    parse_check(&load("basics/mutual_recursion.wag"));

    // 8_20's helper set (map/reverse/length over a local `Lst` datatype,
    // since the built-in Cons/Empty list is gone) is inlined below so it
    // doesn't clash with the List-migrated fixtures' names.
    let src20 = r#"
Lst = Empty | Cons T (Lst T)

increment = Int -> Int: x -> x + 1

negate = Int -> Int: x -> 0 - x

mapHelper = (Int -> Int), Lst Int, Lst Int -> Lst Int: f, lst, acc -> match lst
  Empty -> reverse acc
  Cons h t -> mapHelper f t (Cons (f h) acc)

map = (Int -> Int), Lst Int -> Lst Int: f, lst -> mapHelper f lst Empty

reverseHelper = Lst Int, Lst Int -> Lst Int: lst, acc -> match lst
  Empty -> acc
  Cons h t -> reverseHelper t (Cons h acc)

reverse = Lst Int -> Lst Int: lst -> reverseHelper lst Empty

lengthHelper = &Lst Int, Int -> Int: lst, acc -> match lst
  Empty -> acc
  Cons h t -> lengthHelper t (acc + 1)

length = &Lst Int -> Int: lst -> lengthHelper lst 0

useBoth = Lst Int, Lst Int -> Lst Int: xs, ys ->
  incremented = map increment xs
  negated = map negate ys
  incremented

badApply = Int -> Int: x -> length (recur (Cons x Empty))

recur = Lst Int -> Lst Int: xs -> map badApply xs
"#;
    parse_check(src20);
}

#[test]
fn m4_actor_fixtures() {
    for name in [
        "actors/counter_msg.wag",
        "actors/counter_pragma.wag",
        "actors/counter_supervised.wag",
        "io/main_std.wag",
        "actors/monitor.wag",
    ] {
        // All actor fixtures need the prelude (D088: Option is prelude-declared).
        let src = load(name);
        parse_check_with_prelude(&src);
    }
}

#[test]
fn m4_field_access_fixture() {
    parse_check(&load("types/field_access.wag"));
}

#[test]
fn m4_actor_field_not_cyclic() {
    // §8.18 — Actor fields must not create a rule-14 cycle.
    parse_check(&load("actors/monitor.wag"));
}

#[test]
fn m4_recursive_usertype() {
    // §7 rule 14 permits self-referential user types (classified non-pointer-free).
    parse_check(&load("types/recursive_usertype.wag"));
}

#[test]
fn m4_recursive_usertype_mutual() {
    // §7 rule 14 permits mutual recursion (A ↔ B).
    parse_check(&load("types/recursive_usertype_mutual.wag"));
}

#[test]
fn m4_neg_duplicate_constructor() {
    expect_err_stage(&load("types/neg_duplicate_constructor.wag"), Stage::Namespacing);
}

#[test]
fn m4_fn_actor_shared_basename_coexist() {
    // A function and an actor sharing a base name coexist: actors occupy the
    // `@`-prefixed namespace, so `f` and `@f` are distinct atoms (§2).
    let src = r#"
Option = Some val T | None

f = Int -> Option Int: x -> Some x

@f = Int, Int -> Int: state, msg -> state + msg
"#;
    let t = parse_check(src);
    assert!(t.bindings.contains_key(&("f".to_string(), 1)));
    assert!(t.actors.contains_key("f"));
}

#[test]
fn m4_neg_type_mismatch_option_lift() {
    expect_err_stage(&load("types/neg_type_mismatch_option_lift.wag"), Stage::Inference);
}

#[test]
fn m4_no_option_lift_if() {
    // §3.6: no implicit T -> Option T arm-lift (the runtime returns what the
    // arm produced); arms must agree, so a bare `1` next to `None` is an error.
    let src = r#"
f = Int -> Option Int: x -> if
  x > 0 -> 1
  _ -> None
"#;
    expect_err_stage(src, Stage::Inference);
}

#[test]
fn m4_option_arms_explicit_some() {
    // The explicit form type-checks: `Some 1` and `None` both : Option Int.
    let src = r#"
Option = Some val T | None

f = Int -> Option Int: x -> if
  x > 0 -> Some 1
  _ -> None
"#;
    let t = parse_check(src);
    let ty = t.bindings.get(&("f".to_string(), 1)).unwrap();
    assert!(
        matches!(ty, Ty::Fun(ps, r) if ps.len()==1 && matches!(r.as_ref(), Ty::App(n, _) if n=="Option")),
        "got {ty}"
    );
}

#[test]
fn m4_factorial_type() {
    let t = parse_check(&load("basics/factorial.wag"));
    let fac = t
        .bindings
        .get(&("factorial".to_string(), 1))
        .expect("factorial");
    match fac {
        Ty::Fun(ps, r) => {
            assert_eq!(ps.len(), 1);
            assert!(matches!(&ps[0], Ty::Con(n) if n == "Int"));
            // Raw arithmetic (§1): factorial is Int -> Int, not Option Int.
            assert!(matches!(r.as_ref(), Ty::Con(n) if n == "Int"));
        }
        other => panic!("unexpected {other}"),
    }
}

// ---- String (§3 / §9) ----

#[test]
fn m4_string_literal_type() {
    let src = r#"
lenHello = stringLen "hello"
"#;
    let t = parse_check_with_prelude(src);
    let ty = t.bindings.get(&("lenHello".to_string(), 0)).unwrap();
    // §3.1 value/computation unity: a 0-arity binding denotes its result (a value).
    assert!(matches!(ty, Ty::Con(n) if n == "Int"), "got {ty}");
}

#[test]
fn m4_string_eq_types() {
    let src = r#"
same = "ab" == "ab"

diff = "ab" == "ac"
"#;
    let t = parse_check(src);
    // §3.1 value/computation unity: both are `Bool` values (0-arity bindings).
    for name in ["same", "diff"] {
        let ty = t.bindings.get(&((name.to_string()), 0)).unwrap();
        assert!(matches!(ty, Ty::Con(n) if n == "Bool"), "{name} = {ty}");
    }
}

#[test]
fn m4_string_eq_noncomparable_rejected() {
    // `Lst Int` is not a comparable type — `==` on it is rejected.
    expect_err_stage("bad = [1,2] == [1,2]\n", Stage::Inference);
}

#[test]
fn m4_vec_len_wrong_arg_rejected() {
    // `vecLen` expects a `Vec T`; a `Lst Int` is a type error.
    expect_err_stage("bad = vecLen [1,2]\n", Stage::Inference);
}

// ---- Char + string indexing (§3.10 / §3.11) ----

#[test]
fn m4_char_string_index_types() {
    let src = r#"
first = stringGet "hé" 0

byte0 = stringByteAt "hé" 0

cpH = codepointToChar 104
"#;
    let t = parse_check_with_prelude(src);
    // §3.1 value/computation unity: 0-arity bindings denote their results.
    let first = t.bindings.get(&("first".to_string(), 0)).unwrap();
    assert!(
        matches!(first, Ty::App(h, args) if h == "Option" && matches!(args.first(), Some(Ty::Con(n)) if n == "Char")),
        "first: got {first}"
    );
    let byte0 = t.bindings.get(&("byte0".to_string(), 0)).unwrap();
    assert!(
        matches!(byte0, Ty::App(h, args) if h == "Option" && matches!(args.first(), Some(Ty::Con(n)) if n == "Int")),
        "byte0: got {byte0}"
    );
    let cph = t.bindings.get(&("cpH".to_string(), 0)).unwrap();
    assert!(
        matches!(cph, Ty::App(h, args) if h == "Option" && matches!(args.first(), Some(Ty::Con(n)) if n == "Char")),
        "cpH: got {cph}"
    );
}

#[test]
fn m4_char_comparable() {
    // `Char == Char` / `Char != Char` is legal (Char is a comparable type).
    let src = r#"
hEqH = do
  a <- stringGet "hé" 0
  b <- codepointToChar 104
  return (a == b)
"#;
    let t = parse_check(src);
    let ty = t.bindings.get(&("hEqH".to_string(), 0)).unwrap();
    // §3.1 value/computation unity: 0-arity binding denotes its result.
    assert!(
        matches!(ty, Ty::App(h, args) if h == "Option" && matches!(args.first(), Some(Ty::Con(n)) if n == "Bool")),
        "got {ty}"
    );
}

// ---- Array T (§3.4 / §6.8) ----

#[test]
fn m4_arr_types() {
    let src = r#"
Option = Some val T | None

len3 = arrLen (arrNew 3)

slot0 = arrGet (arrNew 3) 0

set0 = arrSet (arrNew 3) 0 (Some 42)
"#;
    let t = parse_check(src);
    let len_ty = t.bindings.get(&("len3".to_string(), 0)).unwrap();
    // §3.1 value/computation unity: 0-arity bindings denote their results.
    assert!(
        matches!(len_ty, Ty::Con(n) if n == "Int"),
        "len3: got {len_ty}"
    );
    let s0 = t.bindings.get(&("slot0".to_string(), 0)).unwrap();
    assert!(
        matches!(
            s0,
            Ty::App(h, args) if h == "Option"
                && matches!(args.first(), Some(Ty::App(h2, _)) if h2 == "Option")
        ),
        "slot0: got {s0}"
    );
    let s1 = t.bindings.get(&("set0".to_string(), 0)).unwrap();
    assert!(matches!(s1, Ty::App(h, _) if h == "Arr"), "set0: got {s1}");
}

#[test]
fn m4_arr_not_comparable_rejected() {
    // `==` on `Arr T` is not allowed (Arr is not a comparable type).
    expect_err_stage("bad = (arrNew 2) == (arrNew 2)\n", Stage::Inference);
}

// ─── S1.5: Top-level generalization (§3.5, Option A) ──────────────────────

#[test]
fn m4_poly_identity_multiple_types() {
    // identity is in a singleton SCC → generalized.
    // Called with Lst Int and List Bool — must succeed.
    let tp = parse_check(
        r#"
Lst = Empty | Cons T (Lst T)

identity = xs -> xs
a = identity (Cons 1 Empty)
b = identity (Cons True Empty)
"#,
    );
    // Both bindings should have function types (not type variables).
    assert!(tp.bindings.contains_key(&("identity".to_string(), 1)));
    assert!(tp.bindings.contains_key(&("a".to_string(), 0)));
    assert!(tp.bindings.contains_key(&("b".to_string(), 0)));
}

#[test]
fn m4_poly_map_multiple_types() {
    // map + mapHelper: both in singleton SCCs (one-way dependency + self-recursion).
    // Called with different element types — must succeed.
    let tp = parse_check(
        r#"
Lst = Empty | Cons T (Lst T)

map = f, xs -> mapHelper f xs Empty
mapHelper = f, xs, acc -> match xs
  Empty -> acc
  Cons h t -> mapHelper f t (Cons (f h) acc)

double = x -> 2 * x
isBig = x -> x > 10

a = map double (Cons 1 (Cons 2 Empty))
b = map isBig (Cons 5 (Cons 20 Empty))
"#,
    );
    assert!(tp.bindings.contains_key(&("map".to_string(), 2)));
    assert!(tp.bindings.contains_key(&("a".to_string(), 0)));
    assert!(tp.bindings.contains_key(&("b".to_string(), 0)));
}

#[test]
fn m4_poly_wrapper_helper() {
    // reverse + reverseHelper: classic wrapper+helper pattern.
    // reverse → reverseHelper (one-way), reverseHelper → itself (self-loop).
    // Both are singleton SCCs → both generalized.
    let tp = parse_check(
        r#"
Lst = Empty | Cons T (Lst T)

reverse = xs -> reverseHelper xs Empty
reverseHelper = xs, acc -> match xs
  Empty -> acc
  Cons h t -> reverseHelper t (Cons h acc)

r1 = reverse (Cons 1 Empty)
r2 = reverse (Cons True Empty)
"#,
    );
    assert!(tp.bindings.contains_key(&("reverse".to_string(), 1)));
    assert!(tp.bindings.contains_key(&("r1".to_string(), 0)));
    assert!(tp.bindings.contains_key(&("r2".to_string(), 0)));
}

#[test]
fn m4_poly_monomorphic_still_works() {
    // abs is monomorphic (Int -> Int). Must still work.
    let tp = parse_check("abs = x -> if x < 0 -> 0 - x\n  _ -> x\na = abs 5\n");
    // abs: Int -> Int (raw subtraction, §1)
    let expected = Ty::Fun(vec![Ty::int()], Box::new(Ty::int()));
    assert_eq!(tp.bindings.get(&("abs".to_string(), 1)), Some(&expected));
}

#[test]
fn m4_poly_mutual_cycle_rejected() {
    // A true mutual-recursion cycle (foo ↔ bar) is NOT generalized.
    // Calling with different types must fail (monomorphic restriction).
    expect_err_stage(
        "Lst = Empty | Cons T (Lst T)\nfoo = xs -> bar (Cons 1 xs)\nbar = xs -> match xs\n  Empty -> Empty\n  Cons h t -> Cons h (foo t)\n\na = foo (Cons 1 Empty)\nb = foo (Cons True Empty)\n",
        Stage::Inference,
    );
}

#[test]
fn m4_poly_wrapper_calls_two_helpers_direct_call() {
    // Regression: wrapper calls TWO polymorphic helpers (reverse + mapH),
    // and user calls one helper (reverse) directly with a different type.
    // Topological order ensures helpers are generalized before the wrapper
    // constrains them.
    let tp = parse_check(
        r#"
Lst = Empty | Cons T (Lst T)

reverseH = xs, acc -> match xs
  Empty -> acc
  Cons h t -> reverseH t (Cons h acc)
reverse = xs -> reverseH xs Empty

mapH = f, xs, acc -> match xs
  Empty -> acc
  Cons h t -> mapH f t (Cons (f h) acc)
map = f, xs -> reverse (mapH f xs Empty)

big = x -> x > 10
a = map big (Cons 1 (Cons 20 Empty))
b = reverse (Cons 1 (Cons 2 (Cons 3 Empty)))
"#,
    );
    assert!(tp.bindings.contains_key(&("a".to_string(), 0)));
    assert!(tp.bindings.contains_key(&("b".to_string(), 0)));
}

#[test]
fn m4_poly_same_helper_two_wrappers_different_types() {
    // Two wrappers call the same polymorphic helper with different types.
    // The helper must be generalized so each wrapper gets an independent instantiation.
    let tp = parse_check(
        r#"
Lst = Empty | Cons T (Lst T)

mapH = f, xs, acc -> match xs
  Empty -> acc
  Cons h t -> mapH f t (Cons (f h) acc)
wrapInt = xs -> mapH idInt xs Empty
wrapBool = xs -> mapH idBool xs Empty

idInt = x -> x
idBool = x -> x

a = wrapInt (Cons 1 (Cons 2 Empty))
b = wrapBool (Cons True Empty)
"#,
    );
    assert!(tp.bindings.contains_key(&("a".to_string(), 0)));
    assert!(tp.bindings.contains_key(&("b".to_string(), 0)));
}

#[test]
fn m4_poly_helper_called_by_wrapper_and_directly() {
    // A polymorphic helper is called both by a wrapper AND directly by user code
    // with different element types. Both call sites must get independent instantiations.
    let tp = parse_check(
        r#"
Lst = Empty | Cons T (Lst T)

revH = xs, acc -> match xs
  Empty -> acc
  Cons h t -> revH t (Cons h acc)
rev = xs -> revH xs Empty

r1 = rev (Cons 1 (Cons 2 Empty))
r2 = rev (Cons True Empty)
r3 = revH (Cons 5 Empty) (Cons 9 Empty)
"#,
    );
    assert!(tp.bindings.contains_key(&("r1".to_string(), 0)));
    assert!(tp.bindings.contains_key(&("r2".to_string(), 0)));
    assert!(tp.bindings.contains_key(&("r3".to_string(), 0)));
}

#[test]
fn m4_poly_chain_three_levels() {
    // Three-level call chain: wrapper → mid → helper (self-recursive).
    // All three are singleton SCCs and must be generalized independently.
    let tp = parse_check(
        r#"
Lst = Empty | Cons T (Lst T)

buildH = f, xs, acc -> match xs
  Empty -> acc
  Cons h t -> buildH f t (Cons (f h) acc)
build = f, xs -> buildH f xs Empty
wrapInt = xs -> build idInt xs
wrapBool = xs -> build idBool xs

idInt = x -> x
idBool = x -> x

a = wrapInt (Cons 1 (Cons 2 Empty))
b = wrapBool (Cons True Empty)
"#,
    );
    assert!(tp.bindings.contains_key(&("a".to_string(), 0)));
    assert!(tp.bindings.contains_key(&("b".to_string(), 0)));
}

#[test]
fn m4_partial_application_type() {
    // A 2-param function applied to 1 arg yields a 1-param function value
    // (currying / partial application), matching the VM's `Value::Closure`.
    let tp = parse_check(
        r#"
f = a, b -> a + b
g = f 1
"#,
    );
    match tp.bindings.get(&("f".to_string(), 2)) {
        Some(Ty::Fun(params, _)) => assert_eq!(params.len(), 2, "f takes 2 params"),
        other => panic!("f should be a function type, got {other:?}"),
    }
    match tp.bindings.get(&("g".to_string(), 0)) {
        Some(Ty::Fun(params, ret)) => {
            assert_eq!(params.len(), 1, "g takes the remaining 1 param");
            assert_eq!(**ret, Ty::int(), "g returns Int");
        }
        other => panic!("g should be a function type, got {other:?}"),
    }
}

#[test]
fn m4_over_application_fails() {
    // Applying a 2-param function to 3 args is still an error.
    expect_err_stage("f = a, b -> a + b\ng = f 1 2 3", Stage::Inference);
}

#[test]
fn m4_partial_application_in_map() {
    // Partial application as a first-class value passed to a higher-order fn.
    let tp = parse_check(
        r#"
Lst = Empty | Cons T (Lst T)

addH = f, xs, acc -> match xs
  Empty -> acc
  Cons h t -> addH f t (Cons (f h) acc)
add = f, xs -> reverseH (addH f xs Empty) Empty
reverseH = xs, acc -> match xs
  Empty -> acc
  Cons h t -> reverseH t (Cons h acc)
plus10 = plus 10
plus = a, b -> a + b
res = add plus10 (Cons 1 (Cons 2 Empty))
"#,
    );
    assert!(tp.bindings.contains_key(&("res".to_string(), 0)));
    match tp.bindings.get(&("plus10".to_string(), 0)) {
        Some(Ty::Fun(params, ret)) => {
            assert_eq!(params.len(), 1);
            assert_eq!(**ret, Ty::int());
        }
        other => panic!("plus10 should be a function type, got {other:?}"),
    }
}

#[test]
fn nested_pattern_match_list_depth() {
    // Multiple arms for the same constructor (Cons) with different nested patterns.
    let tp = parse_check(
        "Lst = Empty | Cons T (Lst T)\nlistDepth = xs -> match xs\n  Empty -> 0\n  Cons h Empty -> 1\n  Cons h (Cons h2 t) -> 2\nres = listDepth (Cons 1 (Cons 2 Empty))\n");
    assert!(tp.bindings.contains_key(&("res".to_string(), 0)));
}

#[test]
fn nested_pattern_match_with_catchall() {
    // Catch-all arm makes the match exhaustive even with nested patterns.
    let tp = parse_check(
        "Lst = Empty | Cons T (Lst T)\nlistLen2 = xs -> match xs\n  Empty -> 0\n  Cons h Empty -> 1\n  Cons h t -> 99\nres = listLen2 (Cons 1 Empty)\n");
    assert!(tp.bindings.contains_key(&("res".to_string(), 0)));
}
