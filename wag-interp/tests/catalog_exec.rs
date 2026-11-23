//! Milestone 5 execution tests — §8.1–§8.13, §8.17 + trap/arithmetic edges.

use std::path::PathBuf;
use std::thread;

use wag_interp::{
    load_checked, prepare, Engine, FnId, Value, VmError,
};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../fixtures")
}

fn read_fixture(name: &str) -> String {
    std::fs::read_to_string(fixtures_dir().join(name)).expect(name)
}

fn load_engine(srcs: &[&str]) -> (wag_interp::LoadedProgram, String) {
    let mut src = String::new();
    for s in srcs {
        src.push_str(s);
        src.push('\n');
    }
    // D088: always merge the prelude so prelude-declared ctors (Some/None) are
    // available to builtins that construct Option values at runtime.
    let checked = wag_check::check_with_prelude(&src).unwrap_or_else(|e| panic!("check failed: {e}"));
    let loaded = load_checked(checked.program(), checked.typed(), checked.cell_reuse());
    (loaded, src)
}

/// Like `load_engine` but with the WAG prelude merged in first — for tests that
/// exercise names defined in `prelude/prelude.wag` (e.g. stringSplit/stringJoin,
/// which are expressible in source and live there rather than as VM builtins).
fn load_engine_with_prelude(srcs: &[&str]) -> (wag_interp::LoadedProgram, String) {
    let mut src = String::new();
    for s in srcs {
        src.push_str(s);
        src.push('\n');
    }
    // check_with_prelude merges the prelude (D024/D043) and runs structural checks + desugaring,
    // so record updates / field access reach the VM already rewritten.
    let checked = wag_check::check_with_prelude(&src).unwrap_or_else(|e| panic!("check failed: {e}"));
    let loaded = load_checked(checked.program(), checked.typed(), checked.cell_reuse());
    (loaded, src)
}

fn eng_from(names: &[&str]) -> wag_interp::LoadedProgram {
    let parts: Vec<String> = names.iter().map(|n| read_fixture(n)).collect();
    let refs: Vec<&str> = parts.iter().map(|s| s.as_str()).collect();
    load_engine_with_prelude(&refs).0
}

/// Build a `None` value.
fn opt_none(prog: &wag_interp::LoadedProgram) -> Value {
    Value::Data(prog.opt_none_id(), vec![])
}

/// Build a `Some(v)` value.
fn opt_some(prog: &wag_interp::LoadedProgram, v: Value) -> Value {
    Value::Data1(prog.opt_some_id(), Box::new(v))
}

/// Deref an Option-typed value to its payload (`None` → `None`).
fn opt_inner(prog: &wag_interp::LoadedProgram, v: &Value) -> Option<Value> {
    let none_id = prog.opt_none_id();
    let some_id = prog.opt_some_id();
    match v {
        Value::Data(ctor, fields) if *ctor == none_id => None,
        Value::Data(ctor, fields) if *ctor == some_id && !fields.is_empty() => {
            Some(fields[0].clone())
        }
        Value::Data1(ctor, f) if *ctor == some_id => Some(f.as_ref().clone()),
        other => panic!("expected Option, got {other:?}"),
    }
}

/// Deref a closure value.
fn closure_parts(v: &Value) -> (FnId, Vec<Value>) {
    match v {
        Value::Closure(fn_id, captures) => (*fn_id, captures.clone()),
        other => panic!("expected Closure, got {other:?}"),
    }
}

/// Assert a raw `Int n` or an arm-lifted `Some n` (both legal since §1's
/// raw-arithmetic switch: functions may return bare `Int` or `Option Int`).
fn assert_some_int(prog: &wag_interp::LoadedProgram, v: &Value, n: i64) {
    let some_id = prog.opt_some_id();
    match v {
        Value::Int(x) if *x == n => {}
        Value::Data(ctor, fields) if *ctor == some_id && !fields.is_empty() => {
            match &fields[0] {
                Value::Int(x) if *x == n => {}
                other => panic!("expected Int {n} or Some({n}), got Some({other:?})"),
            }
        }
        Value::Data1(ctor, f) if *ctor == some_id => {
            match f.as_ref() {
                Value::Int(x) if *x == n => {}
                other => panic!("expected Int {n} or Some({n}), got Some({other:?})"),
            }
        }
        other => panic!("expected Int {n} or Some({n}), got {other:?}"),
    }
}

#[test]
fn m5_factorial() {
    let prog = eng_from(&["basics/factorial.wag"]);
    let mut e = Engine::new(&prog);
    assert_some_int(&prog, &e.call_name("factorial", vec![Value::Int(5)]).unwrap(), 120);
    assert_some_int(&prog, &e.call_name("factorial", vec![Value::Int(0)]).unwrap(), 1);
    assert_some_int(&prog, &e.call_name("factorial", vec![Value::Int(10)]).unwrap(), 3_628_800);
}

#[test]
fn m5_fibonacci() {
    let prog = eng_from(&["basics/fibonacci.wag"]);
    let mut e = Engine::new(&prog);
    // fib 10 = 55
    assert_some_int(&prog, &e.call_name("fib", vec![Value::Int(10)]).unwrap(), 55);
}

#[test]
fn m5_sum_list() {
    // Migrated to List (D062): borrowed peek/step walk; input stays reusable.
    let src = read_fixture("lists/sum_list.wag");
    let prog = load_engine_with_prelude(&[&src]).0;
    let mut e = Engine::new(&prog);
    let h = e.call_name("buildFive", vec![]).unwrap(); // [1, 2, 3, 4, 5]
    assert_some_int(&prog, &e.call_name("sumList", vec![h]).unwrap(), 15);
}

#[test]
fn m5_length() {
    let src = read_fixture("lists/length.wag");
    let prog = load_engine_with_prelude(&[&src]).0;
    let mut e = Engine::new(&prog);
    let h = e.call_name("buildThree", vec![]).unwrap(); // [3, -1, 4]
    assert_some_int(&prog, &e.call_name("specLength", vec![h]).unwrap(), 3);
}

#[test]
fn m5_is_empty() {
    // Exercise the prelude's isEmpty alone (no fixture dependency).
    let src = r#"
isEmpty2 = List Int -> Bool: lst -> lst.isEmpty
"#;
    let prog = load_engine_with_prelude(&[src]).0;
    let mut e = Engine::new(&prog);
    let empty = e.list_from_ints(&[]).unwrap();
    let nonempty = e.list_from_ints(&[1]).unwrap();
    assert_eq!(
        e.call_name("isEmpty2", vec![empty]).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        e.call_name("isEmpty2", vec![nonempty]).unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn m5_nullary_binding_is_value() {
    // §3.1 value/computation unity: a 0-arity top-level binding denotes its
    // result directly, so it is usable where its result type is expected.
    let src = r#"
xs = [1, 2, 3, 4]
testLen = length xs
"#;
    let prog = load_engine_with_prelude(&[src]).0;
    let mut e = Engine::new(&prog);
    assert_some_int(&prog, &e.call_name("testLen", vec![]).unwrap(), 4);
}

#[test]
fn m5_comprehensions_over_nullary() {
    // Comprehensions over a 0-arity (value) binding, desugared to
    // map/filter/fold + lifted lambdas. Verifies runtime values.
    // map/filter/fold/zip resolve to the prelude (comprehension desugaring
    // targets those names); `xs` is a 0-arity value binding reused by every
    // comprehension below.
    let src = r#"
xs = [1, 2, 3, 4]
t1 = [x <- xs | _ -> x + 1]
t2 = [x <- xs | x > 2 -> x]
t3 = [x <- xs | x > 2 -> x * 10]
t4 = [x <- xs | acc = 0 | _ -> x + acc]
t5 = [x <- xs | acc = 0 | x > 2 -> x + acc]
t6 = [x <- xs, y <- xs | _ -> x + y]
lt1 = length t1
lt2 = length t2
lt3 = length t3
lt6 = length t6
"#;
    let prog = load_engine_with_prelude(&[src]).0;
    let mut e = Engine::new(&prog);
    // t1 = [2, 3, 4, 5] (map) → length 4
    assert_some_int(&prog, &e.call_name("lt1", vec![]).unwrap(), 4);
    // t2 = [3, 4] (filter) → length 2
    assert_some_int(&prog, &e.call_name("lt2", vec![]).unwrap(), 2);
    // t3 = [30, 40] (filter + map) → length 2
    assert_some_int(&prog, &e.call_name("lt3", vec![]).unwrap(), 2);
    // t4 = 10 (fold sum)
    assert_some_int(&prog, &e.call_name("t4", vec![]).unwrap(), 10);
    // t5 = 3 + 4 = 7 (fold sum with guard x > 2)
    assert_some_int(&prog, &e.call_name("t5", vec![]).unwrap(), 7);
    // t6 = [1,2,3,4] x [1,2,3,4] = 16 pairs (cartesian, via flatMap)
    assert_some_int(&prog, &e.call_name("lt6", vec![]).unwrap(), 16);
}

#[test]
fn m5_zip_comprehensions() {
    // Zip comprehensions: desugar to map/filter/fold over nested zip + a
    // lambda that destructures nested Pairs. Verifies runtime values, incl. the
    // shortest-list-wins zip semantics and 3-element zips.
    let src = r#"
xs = [1, 2, 3, 4]
ys = [10, 20, 30]   -- shorter than xs (3 vs 4)
zs = [100, 200, 300, 400, 500]
-- 2-element zip map: xs=[1,2,3,4] ys=[10,20,30] -> [11,22,33] (zip to shortest=3)
zmap = [x, y <- xs, ys | _ -> x + y]
-- 2-element zip with guard: x<3 -> x*y -> [1*10, 2*20] = [10,40]
zguard = [x, y <- xs, ys | x < 3 -> x * y]
-- 3-element zip: [1+10+100, 2+20+200, 3+30+300] = [111,222,333]
trip = [a, b, c <- xs, ys, zs | _ -> a + b + c]
-- zip fold (dot product): 1*10 + 2*20 + 3*30 = 10+40+90 = 140
zdot = [x, y <- xs, ys | s = 0 | _ -> x * y + s]
-- length checks
zml = length zmap
zgl = length zguard
trl = length trip
"#;
    let prog = load_engine_with_prelude(&[src]).0;
    let mut e = Engine::new(&prog);
    // zip map length: shortest of (4,3) = 3
    assert_some_int(&prog, &e.call_name("zml", vec![]).unwrap(), 3);
    // zip guard length: x in {1,2} (x<3 of the first 3) = 2
    assert_some_int(&prog, &e.call_name("zgl", vec![]).unwrap(), 2);
    // 3-element zip length: shortest of (4,3,5) = 3
    assert_some_int(&prog, &e.call_name("trl", vec![]).unwrap(), 3);
    // dot product value: 140
    assert_some_int(&prog, &e.call_name("zdot", vec![]).unwrap(), 140);
    // zip map values: [1+10, 2+20, 3+30] = [11, 22, 33] (zip to shortest=3)
    let zmap_v = e.call_name("zmap", vec![]).unwrap();
    assert_eq!(e.list_to_ints(&zmap_v).unwrap(), vec![11, 22, 33]);
    // 3-element zip values: [1+10+100, 2+20+200, 3+30+300] = [111, 222, 333]
    let trip_v = e.call_name("trip", vec![]).unwrap();
    assert_eq!(e.list_to_ints(&trip_v).unwrap(), vec![111, 222, 333]);
}

// Order-sensitive checksum over a List (fold consumes from `start`, so the
// cursor position of prepend-built results is irrelevant).
const WSUM_SRC: &str = "wsumStep = Int, Int -> Int: v, acc -> acc * 10 + v\nwsum = List Int -> Int: lst -> fold wsumStep 0 lst\n";

#[test]
fn m5_reverse() {
    // Migrated to List (D062): prepend-onto-fresh-accumulator walk.
    let src = read_fixture("lists/reverse.wag");
    let prog = load_engine_with_prelude(&[&src, WSUM_SRC]).0;
    let mut e = Engine::new(&prog);
    let lst = e.call_name("buildThree", vec![]).unwrap(); // [1, 2, 3]
    let out = e.call_name("specReverse", vec![lst]).unwrap();
    assert_some_int(&prog, &e.call_name("wsum", vec![out]).unwrap(), 321);
}

#[test]
fn m5_map() {
    // Migrated to List (D062): O(1) tail append — no reverse dependency.
    let src = read_fixture("lists/map.wag");
    let prog = load_engine_with_prelude(&[&src, WSUM_SRC]).0;
    let mut e = Engine::new(&prog);
    let lst = e.call_name("buildThree", vec![]).unwrap(); // [1, 2, 3]
    let f = Value::Fn(prog.fn_id("increment").unwrap());
    let out = e.call_name("specMap", vec![f, lst]).unwrap();
    assert_some_int(&prog, &e.call_name("wsum", vec![out]).unwrap(), 234);
}

#[test]
fn m5_filter() {
    // Migrated to List (D062): `specFilter` wraps prelude `filter`.
    let src = read_fixture("lists/filter.wag");
    let prog = load_engine_with_prelude(&[&src]).0;
    let mut e = Engine::new(&prog);
    let h = e.call_name("buildThree", vec![]).unwrap(); // [1, -3, 7]
    let f = Value::Fn(prog.fn_id("isPositive").unwrap());
    let out = e.call_name("specFilter", vec![f, h]).unwrap();
    assert_some_int(&prog, &e.call_name("length", vec![out]).unwrap(), 2);
}

#[test]
fn m5_max_list() {
    // Migrated to List (D062): first element seeds the borrowed walk.
    let src = read_fixture("lists/max_list.wag");
    let prog = load_engine_with_prelude(&[&src]).0;
    let mut e = Engine::new(&prog);
    let lst = e.call_name("buildFour", vec![]).unwrap(); // [3, -1, 9, 4]
    assert_some_int(&prog, &e.call_name("maxList", vec![lst]).unwrap(), 9);
    let empty = e.call_name("listNew", vec![]).unwrap();
    let got = e.call_name("maxList", vec![empty]).unwrap();
    assert!(got == opt_none(&prog), "expected None, got {got:?}");
}

#[test]
fn m5_gcd() {
    let prog = eng_from(&["basics/gcd.wag"]);
    let mut e = Engine::new(&prog);
    // gcd uses checked ops / Option-returning control flow in some transcriptions
    let v = e
        .call_name("gcd", vec![Value::Int(48), Value::Int(18)])
        .unwrap();
    match v {
        Value::Int(6) => {}
        ref r if opt_inner(&prog, r) == Some(Value::Int(6)) => {}
        other => panic!("unexpected gcd result {other:?}"),
    }
}

#[test]
fn m5_pow() {
    let prog = eng_from(&["basics/pow.wag"]);
    let mut e = Engine::new(&prog);
    assert_some_int(&prog, 
        &e.call_name("pow", vec![Value::Int(2), Value::Int(10)]).unwrap(),
        1024,
    );
}

#[test]
fn m5_contains() {
    // Migrated to List (D062): borrowed peek/step walk — the list survives both calls.
    let src = read_fixture("lists/contains.wag");
    let prog = load_engine_with_prelude(&[&src]).0;
    let mut e = Engine::new(&prog);
    let h = e.call_name("buildThree", vec![]).unwrap(); // [1, -3, 7]
    assert_eq!(
        e.call_name("contains", vec![Value::Int(7), h.clone()])
            .unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        e.call_name("contains", vec![Value::Int(5), h.clone()]).unwrap(),
        Value::Bool(false)
    );
}

#[test]
fn m5_count() {
    // Migrated to List (D062): borrowed peek/step walk.
    let src = read_fixture("lists/count.wag");
    let prog = load_engine_with_prelude(&[&src]).0;
    let mut e = Engine::new(&prog);
    let h = e.call_name("buildFour", vec![]).unwrap(); // [1, -3, 7, 1]
    assert_some_int(&prog, &e.call_name("count", vec![Value::Int(1), h]).unwrap(), 2);
}

#[test]
fn m5_insertion_sort() {
    // Migrated to List (D062): borrowed insertion-point walk + tail splice.
    let src = read_fixture("lists/insertion_sort.wag");
    let prog = load_engine_with_prelude(&[&src, WSUM_SRC]).0;
    let mut e = Engine::new(&prog);
    let lst = e.call_name("buildThree", vec![]).unwrap(); // [3, -1, 2]
    let out = e.call_name("insertionSort", vec![lst]).unwrap();
    // wsum of [-1, 2, 3] = ((0*10-1)*10+2)*10+3 = -77 (order-sensitive).
    assert_some_int(&prog, &e.call_name("wsum", vec![out]).unwrap(), -77);
}

#[test]
fn m5_is_sorted() {
    // Migrated to List (D062): borrowed peek/step walk over adjacent pairs.
    let src = read_fixture("lists/is_sorted.wag");
    let prog = load_engine_with_prelude(&[&src]).0;
    let mut e = Engine::new(&prog);
    let asc = e.call_name("buildAsc", vec![]).unwrap();
    let desc = e.call_name("buildDesc", vec![]).unwrap();
    assert_eq!(e.call_name("isSorted", vec![asc]).unwrap(), Value::Bool(true));
    assert_eq!(e.call_name("isSorted", vec![desc]).unwrap(), Value::Bool(false));
}

#[test]
fn m5_raw_arithmetic_traps() {
    // §1 raw arithmetic: total on in-range operands, fatal trap on overflow /
    // division-by-zero (Rust debug-mode semantics).
    let src = r#"
divTest = Int, Int -> Int: a, b -> a / b

addTest = Int, Int -> Int: a, b -> a + b
"#;
    let (_p, _t, loaded) = prepare(src).unwrap();
    let mut e = Engine::new(&loaded);
    // in-range: total
    assert_eq!(
        e.call_name("divTest", vec![Value::Int(10), Value::Int(3)])
            .unwrap(),
        Value::Int(3)
    );
    assert_eq!(
        e.call_name("addTest", vec![Value::Int(2), Value::Int(3)])
            .unwrap(),
        Value::Int(5)
    );
    // div by zero → trap
    assert!(matches!(
        e.call_name("divTest", vec![Value::Int(1), Value::Int(0)]),
        Err(VmError::Panic(_))
    ));
    // i64::MIN / -1 overflows → trap
    assert!(matches!(
        e.call_name(
            "divTest",
            vec![Value::Int(i64::MIN), Value::Int(-1)]
        ),
        Err(VmError::Panic(_))
    ));
    // overflow add → trap
    assert!(matches!(
        e.call_name(
            "addTest",
            vec![Value::Int(i64::MAX), Value::Int(1)]
        ),
        Err(VmError::Panic(_))
    ));
}

#[test]
fn m5_rem_operator() {
    let src = r#"
remTest = Int, Int -> Int: a, b -> a % b
"#;
    let (_p, _t, loaded) = prepare(src).unwrap();
    let mut e = Engine::new(&loaded);
    // normal remainder
    assert_eq!(
        e.call_name("remTest", vec![Value::Int(7), Value::Int(3)])
            .unwrap(),
        Value::Int(1)
    );
    // i64 truncation toward zero: -7 % 3 = -1
    assert_eq!(
        e.call_name("remTest", vec![Value::Int(-7), Value::Int(3)])
            .unwrap(),
        Value::Int(-1)
    );
    // division by zero → trap
    assert!(matches!(
        e.call_name("remTest", vec![Value::Int(5), Value::Int(0)]),
        Err(VmError::Panic(_))
    ));
    // i64::MIN % -1 overflows → trap
    assert!(matches!(
        e.call_name(
            "remTest",
            vec![Value::Int(i64::MIN), Value::Int(-1)]
        ),
        Err(VmError::Panic(_))
    ));
}

#[test]
fn m5_abs_prelude_fn() {
    // Mirrors prelude/prelude.wag `abs` (inlined since prepare() does not merge the prelude).
    let src = r#"
abs = Int -> Int: x -> if 
  x < 0 -> (0 - x)
  _ -> x
"#;
    let (_p, _t, loaded) = prepare(src).unwrap();
    let mut e = Engine::new(&loaded);
    // positive
    assert_eq!(
        e.call_name("abs", vec![Value::Int(5)]).unwrap(),
        Value::Int(5)
    );
    // zero
    assert_eq!(
        e.call_name("abs", vec![Value::Int(0)]).unwrap(),
        Value::Int(0)
    );
    // negative
    assert_eq!(
        e.call_name("abs", vec![Value::Int(-7)]).unwrap(),
        Value::Int(7)
    );
    // i64::MIN overflows on negation → trap
    assert!(matches!(
        e.call_name("abs", vec![Value::Int(i64::MIN)]),
        Err(VmError::Panic(_))
    ));
}




// Deep tail-recursive WAG builder (the builtin-list bridge is gone for List
// fixtures); kept under the 100k step budget: ~2 steps/element building +
// ~3/element walking.
const DEEP_SUM_SRC: &str = "\ndeepBuild = Int, List Int -> List Int: n, acc -> if n == 0 -> acc
  _ -> deepBuild (n - 1) (acc.append n)
dB = Int -> List Int: n ->
  a0 = listNew
  deepBuild n a0
deepSum = Int -> Int: n ->
  a0 = listNew
  sumList (deepBuild n a0)
";

#[test]
fn m5_trampoline_deep_sum() {
    let src = read_fixture("lists/sum_list.wag");
    let prog = load_engine_with_prelude(&[&src, DEEP_SUM_SRC]).0;
    // Run on a bounded-stack thread: the baseline nesting (deepSum frame →
    // value-position deepBuild trampoline → List method-dispatch chain) needs
    // ~600–800 KB of Rust frames in debug builds, but the trampoline must keep
    // the total flat — without it, deepBuild's self-call would nest that whole
    // chain per iteration and overflow.
    // D081: owned-model append is O(n) (chain extension), so n=3 000 keeps
    // total work (~9 M node-clones) tractable in debug while still being deep
    // enough to overflow a small stack without the trampoline.
    let handle = thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            let mut e = Engine::new(&prog);
            e.call_name("deepSum", vec![Value::Int(3_000)]).unwrap()
        })
        .expect("spawn");
    let v = handle.join().expect("thread panicked");
    // sum 1..=3000 = 4_501_500 — no overflow, deep tail iterations.
    assert_eq!(v, Value::Int(4_501_500));
}

#[test]
fn m5_reverse_correct_large() {
    // Migrated to List (D062): bigBuild 500 yields [500, …, 1]; reversed is
    // [1, …, 500]. Check order-sensitive properties: sum, head (after rewind),
    // and last element.
    let src = read_fixture("lists/reverse.wag");
    const BIG_SRC: &str = "\nbigBuild = Int, List Int -> List Int: n, acc -> if n == 0 -> acc
  _ -> bigBuild (n - 1) (acc.append n)
bigRev = Int -> List Int: n ->
  a0 = listNew
  specReverse (bigBuild n a0)
addStep = Int, Int -> Int: v, acc -> acc + v
sumOf = List Int -> Int: lst -> fold addStep 0 lst
lastWalk = List Int -> Option Int: lst ->
  c = copy lst             -- rule 10: `c.step` consumes the copy; keep `lst` for the base case
  match c.step
    None   -> lst.peek
    Some t -> lastWalk t
bigRevHead = Int -> Option Int: n ->
  r0 = bigRev n
  r = r0.rewind
  r.peek
bigRevLast = Int -> Option Int: n ->
  r0 = bigRev n
  r = r0.rewind
  lastWalk r
";
    let prog = load_engine_with_prelude(&[&src, BIG_SRC]).0;
    let mut e = Engine::new(&prog);
    let n = 500i64;
    let rev = e.call_name("bigRev", vec![Value::Int(n)]).unwrap();
    assert_some_int(&prog, &e.call_name("sumOf", vec![rev]).unwrap(), 125_250); // sum 1..=500
    let got = e.call_name("bigRevHead", vec![Value::Int(n)]).unwrap();
    assert!(got == opt_some(&prog, Value::Int(1)), "expected Some(1), got {got:?}");
    let got = e.call_name("bigRevLast", vec![Value::Int(n)]).unwrap();
    assert!(got == opt_some(&prog, Value::Int(500)), "expected Some(500), got {got:?}");
}

// ---- String (§3 / §9): packed UTF-8 bytes, non-pointer-free, linear ----

fn str_prog() -> wag_interp::LoadedProgram {
    // Prelude required: stringToCodepoints/codepointsToString are VM builtins
    // over the prelude-declared List (need LH/LNodeC/LEnd ctors in-program).
    load_engine_with_prelude(&[
        r#"
lenHello = stringLen "hello"

lenUtf8 = stringLen "hé"

concatLen = stringLen (stringConcat "ab" "cd")

eqSame = "ab" == "ab"

eqDiff = "ab" == "ac"

cps = vecToList (stringToCodepoints "hé")

roundtrip = codepointsToString (stringToCodepoints "hé") == "hé"
"#
    ])
    .0
}

#[test]
fn m5_string_len_ascii() {
    let prog = str_prog();
    let mut e = Engine::new(&prog);
    let v = e.call_name("lenHello", vec![]).unwrap();
    assert!(matches!(v, Value::Int(5)), "got {v:?}");
}

#[test]
fn m5_string_len_utf8_codepoints() {
    let prog = str_prog();
    let mut e = Engine::new(&prog);
    // "hé" = 'h' + 'é' = 2 codepoints (D089: stringLen counts codepoints).
    let v = e.call_name("lenUtf8", vec![]).unwrap();
    assert!(matches!(v, Value::Int(2)), "got {v:?}");
}

#[test]
fn m5_string_concat_len() {
    let prog = str_prog();
    let mut e = Engine::new(&prog);
    let v = e.call_name("concatLen", vec![]).unwrap();
    assert!(matches!(v, Value::Int(4)), "got {v:?}");
}

#[test]
fn m5_string_eq() {
    let prog = str_prog();
    let mut e = Engine::new(&prog);
    let same = e.call_name("eqSame", vec![]).unwrap();
    assert!(matches!(same, Value::Bool(true)), "got {same:?}");
    let diff = e.call_name("eqDiff", vec![]).unwrap();
    assert!(matches!(diff, Value::Bool(false)), "got {diff:?}");
}

#[test]
fn m5_string_codepoints_roundtrip() {
    let prog = str_prog();
    let mut e = Engine::new(&prog);
    let cps = e.call_name("cps", vec![]).unwrap();
    assert_eq!(e.list_to_ints(&cps).unwrap(), vec![104, 233]);
    let rt = e.call_name("roundtrip", vec![]).unwrap();
    assert!(matches!(rt, Value::Bool(true)), "got {rt:?}");
}

// ---- stringSplit / stringJoin ----

fn split_join_prog() -> wag_interp::LoadedProgram {
    load_engine_with_prelude(&[
        r#"
rt1 = stringJoin 44 (stringSplit 44 "hello,world,foo") == "hello,world,foo"

rt2 = stringJoin 32 (stringSplit 32 "a b c") == "a b c"

empty = stringJoin 44 (stringSplit 44 "") == ""

noSep = stringJoin 44 (stringSplit 0 "hello") == "hello"
"#
    ])
    .0
}

#[test]
fn m5_string_split_join_roundtrip() {
    let prog = split_join_prog();
    let mut e = Engine::new(&prog);
    let v = e.call_name("rt1", vec![]).unwrap();
    assert!(matches!(v, Value::Bool(true)), "got {v:?}");
}

#[test]
fn m5_string_split_join_spaces() {
    let prog = split_join_prog();
    let mut e = Engine::new(&prog);
    let v = e.call_name("rt2", vec![]).unwrap();
    assert!(matches!(v, Value::Bool(true)), "got {v:?}");
}

#[test]
fn m5_string_split_join_empty() {
    let prog = split_join_prog();
    let mut e = Engine::new(&prog);
    let v = e.call_name("empty", vec![]).unwrap();
    assert!(matches!(v, Value::Bool(true)), "got {v:?}");
}

#[test]
fn m5_string_split_join_no_separator_found() {
    let prog = split_join_prog();
    let mut e = Engine::new(&prog);
    let v = e.call_name("noSep", vec![]).unwrap();
    assert!(matches!(v, Value::Bool(true)), "got {v:?}");
}

// ---- Char + string indexing (§3.10 / §3.11) ----

fn char_prog() -> wag_interp::LoadedProgram {
    load_engine_with_prelude(&[
        r#"
first = stringGet "hé" 0

second = stringGet "hé" 1

oob = stringGet "hé" 5

byte0 = stringByteAt "hé" 0

byte2 = stringByteAt "hé" 2

cpH = codepointToChar 104

cpBad = codepointToChar 55296

hEqH = do
  a <- stringGet "hé" 0
  b <- codepointToChar 104
  return (a == b)
"#
    ])
    .0
}

fn assert_deep(e: &mut Engine, name: &str, expected: Value) {
    let v = e.call_name(name, vec![]).unwrap();
    assert!(
        v == expected,
        "{name}: got {}, expected {}",
        v.brief_desc(),
        expected.brief_desc()
    );
}

#[test]
fn m5_string_get_codepoint() {
    let prog = char_prog();
    let mut e = Engine::new(&prog);
    let exp_first = opt_some(&prog, prog.make_char(104));
    assert_deep(&mut e, "first", exp_first);
    let exp_second = opt_some(&prog, prog.make_char(233));
    assert_deep(&mut e, "second", exp_second);
}

#[test]
fn m5_string_get_oob() {
    let prog = char_prog();
    let mut e = Engine::new(&prog);
    let exp_oob = opt_none(&prog);
    assert_deep(&mut e, "oob", exp_oob);
}

#[test]
fn m5_string_byte_index() {
    let prog = char_prog();
    let mut e = Engine::new(&prog);
    let exp_byte0 = opt_some(&prog, Value::Int(104));
    assert_deep(&mut e, "byte0", exp_byte0);
    // 'é' = 0xC3 0xA9; byte 2 is 0xA9 = 169.
    let exp_byte2 = opt_some(&prog, Value::Int(169));
    assert_deep(&mut e, "byte2", exp_byte2);
}

#[test]
fn m5_codepoint_to_char() {
    let prog = char_prog();
    let mut e = Engine::new(&prog);
    let exp_cpH = opt_some(&prog, prog.make_char(104));
    assert_deep(&mut e, "cpH", exp_cpH);
    // 55296 (0xD800) is a surrogate — invalid scalar value -> None.
    let exp_cpBad = opt_none(&prog);
    assert_deep(&mut e, "cpBad", exp_cpBad);
}

#[test]
fn m5_char_eq() {
    let prog = char_prog();
    let mut e = Engine::new(&prog);
    let exp_hEqH = opt_some(&prog, Value::Bool(true));
    assert_deep(&mut e, "hEqH", exp_hEqH);
}

// ---- Array T (§3.4 / §6.8) ----

fn arr_prog() -> wag_interp::LoadedProgram {
    load_engine(&[
        r#"
len3 = arrLen (arrNew 3)

slot0Empty = arrGet (arrNew 3) 0

oob = arrGet (arrNew 3) 5

set0read = arrGet (arrSet (arrNew 3) 0 (Some 42)) 0

set2read = arrGet (arrSet (arrNew 3) 2 (Some 7)) 2

slot1After = arrGet (arrSet (arrNew 3) 2 (Some 7)) 1
"#
    ])
    .0
}

#[test]
fn m5_array_new_len() {
    let prog = arr_prog();
    let mut e = Engine::new(&prog);
    assert_deep(&mut e, "len3", Value::Int(3));
}

#[test]
fn m5_array_empty_slot_and_oob() {
    let prog = arr_prog();
    let mut e = Engine::new(&prog);
    // Empty slot -> Some None. Out of bounds -> None.
    let exp_slot0Empty = opt_some(&prog, opt_none(&prog));
    assert_deep(&mut e, "slot0Empty", exp_slot0Empty);
    let exp_oob = opt_none(&prog);
    assert_deep(&mut e, "oob", exp_oob);
}

#[test]
fn m5_array_set_get() {
    let prog = arr_prog();
    let mut e = Engine::new(&prog);
    let exp_set0read = opt_some(&prog, opt_some(&prog, Value::Int(42)));
    assert_deep(&mut e, "set0read", exp_set0read);
    let exp_set2read = opt_some(&prog, opt_some(&prog, Value::Int(7)));
    assert_deep(&mut e, "set2read", exp_set2read);
    // Slot 1 was untouched by setting slot 2 -> still Some None.
    let exp_slot1After = opt_some(&prog, opt_none(&prog));
    assert_deep(&mut e, "slot1After", exp_slot1After);
}

// ---------------------------------------------------------------------------
// Bitwise operators (§S1): ^ bitXor, | bitOr, & bitAnd, << shiftLeft, >> shiftRight
// Total (return Int, not Option). >> is an arithmetic (signed) shift.
// ---------------------------------------------------------------------------

fn assert_int(v: &Value, n: i64) {
    match v {
        Value::Int(x) if *x == n => {}
        other => panic!("expected Int({n}), got {other:?}"),
    }
}

fn bitwise_prog() -> wag_interp::LoadedProgram {
    load_engine(&[
        "xor = Int, Int -> Int: a, b -> a ^ b\n",
        "bor = Int, Int -> Int: a, b -> a | b\n",
        "band = Int, Int -> Int: a, b -> a & b\n",
        "shl = Int, Int -> Int: a, b -> a << b\n",
        "shr = Int, Int -> Int: a, b -> a >> b\n",
        "precedence = Int -> Int: n -> n | 2 & 3 ^ 4 << 5\n",
    ])
    .0
}

#[test]
fn m5_bitwise_xor_or_and() {
    let prog = bitwise_prog();
    let mut e = Engine::new(&prog);
    assert_int(&e.call_name("xor", vec![Value::Int(1), Value::Int(2)]).unwrap(), 3);
    assert_int(&e.call_name("xor", vec![Value::Int(5), Value::Int(3)]).unwrap(), 6);
    assert_int(&e.call_name("bor", vec![Value::Int(1), Value::Int(2)]).unwrap(), 3);
    assert_int(&e.call_name("bor", vec![Value::Int(4), Value::Int(1)]).unwrap(), 5);
    assert_int(&e.call_name("band", vec![Value::Int(3), Value::Int(5)]).unwrap(), 1);
    assert_int(&e.call_name("band", vec![Value::Int(6), Value::Int(3)]).unwrap(), 2);
}

#[test]
fn m5_bitwise_shifts() {
    let prog = bitwise_prog();
    let mut e = Engine::new(&prog);
    assert_int(&e.call_name("shl", vec![Value::Int(1), Value::Int(3)]).unwrap(), 8);
    assert_int(&e.call_name("shl", vec![Value::Int(3), Value::Int(2)]).unwrap(), 12);
    assert_int(&e.call_name("shr", vec![Value::Int(16), Value::Int(2)]).unwrap(), 4);
    assert_int(&e.call_name("shr", vec![Value::Int(28), Value::Int(2)]).unwrap(), 7);
    // Arithmetic (signed) shift: -8 >> 1 = -4.
    assert_int(&e.call_name("shr", vec![Value::Int(-8), Value::Int(1)]).unwrap(), -4);
}

#[test]
fn m5_bitwise_precedence() {
    let prog = bitwise_prog();
    let mut e = Engine::new(&prog);
    // n | 2 & 3 ^ 4 << 5  ⇒  n | ((2 & 3) ^ (4 << 5))   [& tighter than ^, ^ tighter than |]
    // 4 << 5 = 128; 2 & 3 = 2; 2 ^ 128 = 130; n | 130.
    assert_int(&e.call_name("precedence", vec![Value::Int(1)]).unwrap(), 131);
    assert_int(&e.call_name("precedence", vec![Value::Int(0)]).unwrap(), 130);
}

// ─── S4: Tier-2 primitives ────────────────────────────────────────────────────

#[test]
fn m5_s4_now_ms_returns_positive_int() {
    let prog = load_engine(&["t = nowMs"]).0;
    let mut e = Engine::new(&prog);
    let v = e.call_name("t", vec![]).unwrap();
    match v {
        Value::Int(ms) => assert!(ms > 0, "nowMs should be positive, got {ms}"),
        other => panic!("expected Int, got {other:?}"),
    }
}

#[test]
fn m5_s4_rand_range_in_bounds() {
    let prog = load_engine(&["r = lo, hi -> randRange lo hi"]).0;
    let mut e = Engine::new(&prog);
    for _ in 0..100 {
        let v = e.call_name("r", vec![Value::Int(10), Value::Int(20)]).unwrap();
        match v {
            Value::Int(n) => {
                assert!((10..=20).contains(&n), "randRange(10,20) out of bounds: {n}");
            }
            other => panic!("expected Int, got {other:?}"),
        }
    }
}

#[test]
fn m5_s4_rand_range_single_value() {
    let prog = load_engine(&["r = lo, hi -> randRange lo hi"]).0;
    let mut e = Engine::new(&prog);
    let v = e.call_name("r", vec![Value::Int(5), Value::Int(5)]).unwrap();
    assert_int(&v, 5);
}

#[test]
fn m5_s4_rand_range_invalid_traps() {
    let prog = load_engine(&["r = lo, hi -> randRange lo hi"]).0;
    let mut e = Engine::new(&prog);
    let result = e.call_name("r", vec![Value::Int(10), Value::Int(5)]);
    assert!(result.is_err(), "randRange(10,5) should trap");
}

#[test]
fn m5_s4_env_get_known_var() {
    // PATH is set in virtually all environments.
    let prog = load_engine(&["p = envGet \"PATH\""]).0;
    let mut e = Engine::new(&prog);
    let v = e.call_name("p", vec![]).unwrap();
    match opt_inner(&prog, &v) {
        Some(_) => {} // Some(String) — good
        None => {
            // PATH might not be set in some CI environments; that's acceptable.
        }
    }
}

#[test]
fn m5_s4_env_get_missing_var() {
    let prog = load_engine(&["p = envGet \"WAG_TEST_DEFINITELY_MISSING_12345\""]).0;
    let mut e = Engine::new(&prog);
    let v = e.call_name("p", vec![]).unwrap();
    match opt_inner(&prog, &v) {
        None => {}
        other => panic!("expected None, got {other:?}"),
    }
}

// ─── S5: File I/O ─────────────────────────────────────────────────────────────

#[test]
fn m5_s5_file_write_read_roundtrip() {
    let path = "/tmp/wag_m5_test_roundtrip.txt";
    // Use a simple 2-element list to avoid parser complexity.
    let prog = load_engine_with_prelude(&[
        &format!("w = fileOpenWrite \"{path}\""),
        "doWrite = fd -> fileWrite fd [72, 101]",
        &format!("r = fileOpenRead \"{path}\""),
        "doRead = fd -> fileRead fd",
    ])
    .0;
    let mut e = Engine::new(&prog);
    // Open for write, write "hello" (5 bytes), verify count.
    let w = e.call_name("w", vec![]).unwrap();
    let fd = match opt_inner(&prog, &w) {
        Some(Value::Int(n)) => n,
        other => panic!("expected Int fd, got {other:?}"),
    };
    let written = e.call_name("doWrite", vec![Value::Int(fd)]).unwrap();
    assert_int(&written, 2);
    // Verify the file content on disk.
    let content = std::fs::read(path).unwrap();
    assert_eq!(content, vec![72u8, 101]);
    // Open for read, verify we get Some (not None/EOF).
    let r = e.call_name("r", vec![]).unwrap();
    let rfd = match opt_inner(&prog, &r) {
        Some(Value::Int(n)) => n,
        other => panic!("expected Int fd, got {other:?}"),
    };
    let data = e.call_name("doRead", vec![Value::Int(rfd)]).unwrap();
    match opt_inner(&prog, &data) {
        Some(_) => {} // Some(List Int) — non-empty read
        None => panic!("fileRead returned None (EOF) but file has content"),
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn m5_s5_file_open_nonexistent_returns_none() {
    let prog = load_engine(&["r = fileOpenRead \"/tmp/wag_definitely_missing_xyz_12345\""]).0;
    let mut e = Engine::new(&prog);
    let v = e.call_name("r", vec![]).unwrap();
    match opt_inner(&prog, &v) {
        None => {}
        other => panic!("expected None for missing file, got {other:?}"),
    }
}

#[test]
fn m5_s5_file_close_returns_zero() {
    let path = "/tmp/wag_m5_test_close.txt";
    let prog = load_engine(&[
        &format!("w = fileOpenWrite \"{path}\""),
        "c = fd -> fileClose fd",
    ])
    .0;
    let mut e = Engine::new(&prog);
    let w = e.call_name("w", vec![]).unwrap();
    match opt_inner(&prog, &w) {
        Some(Value::Int(fd)) => {
            let r = e.call_name("c", vec![Value::Int(fd)]).unwrap();
            assert_int(&r, 0);
        }
        other => panic!("expected Some(fd), got {other:?}"),
    }
    let _ = std::fs::remove_file(path);
}

#[test]
fn m5_s5_file_close_invalid_handle() {
    let prog = load_engine(&["c = fd -> fileClose fd"]).0;
    let mut e = Engine::new(&prog);
    let v = e.call_name("c", vec![Value::Int(9999)]).unwrap();
    assert_int(&v, -1);
}

// ─── Closures & Partial Application ─────────────────────────────────────────

#[test]
fn m1_partial_apply_produces_closure() {
    // first a b = a (arity 2, returns Int directly).
    let prog = load_engine(&["first = a, b -> a"]).0;
    let mut e = Engine::new(&prog);
    // Call first with only 1 arg → should produce a Closure, not error.
    let v = e.call_name("first", vec![Value::Int(10)]).unwrap();
    let (fn_id, pre_args) = closure_parts(&v);
    assert_eq!(pre_args, vec![Value::Int(10)]);
    let entry = prog.get_fn(fn_id);
    assert_eq!(entry.params.len(), 2);
}

#[test]
fn m1_closure_call_concatenates_args() {
    // pair a b = Pair a b — but Pair needs 2 args. Let's use a simple selector.
    // sel which val = if which -> val, _ -> 0  (but if needs Bool)
    // Simplest: first a b = a
    let prog = load_engine(&["first = a, b -> a"]).0;
    let mut e = Engine::new(&prog);
    // Partial apply: first 42 → Closure { fn_id, pre_args: [42] }
    let v = e.call_name("first", vec![Value::Int(42)]).unwrap();
    let (fn_id, pre_args) = closure_parts(&v);
    assert_eq!(pre_args, vec![Value::Int(42)]);
    // Now call the closure with arg 99: concat [42] + [99] = [42, 99] → first 42 99 = 42
    let result = e.call(fn_id, vec![Value::Int(42), Value::Int(99)]).unwrap();
    assert_eq!(result, Value::Int(42));
}

#[test]
fn m1_full_apply_still_works() {
    // Ensure normal 2-arg calls still work (no regression).
    let prog = load_engine(&["first = a, b -> a"]).0;
    let mut e = Engine::new(&prog);
    let v = e.call_name("first", vec![Value::Int(3), Value::Int(4)]).unwrap();
    assert_eq!(v, Value::Int(3));
}

#[test]
fn m1_too_many_args_errors() {
    let prog = load_engine(&["first = a, b -> a"]).0;
    let mut e = Engine::new(&prog);
    let result = e.call_name("first", vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
    assert!(result.is_err());
}

#[test]
fn m1_closure_in_env_and_call() {
    // Simulate: f = first 10, then f 5 → 10
    let prog = load_engine(&["first = a, b -> a"]).0;
    let mut e = Engine::new(&prog);
    let v = e.call_name("first", vec![Value::Int(10)]).unwrap();
    let (fn_id, mut args) = closure_parts(&v);
    // Call: pre_args [10] + new_arg [5] → [10, 5] → first 10 5 = 10
    args.push(Value::Int(5));
    let result = e.call(fn_id, args).unwrap();
    assert_eq!(result, Value::Int(10));
}

#[test]
fn m1_partial_apply_3arg_fn() {
    // sel a b c = a (arity 3, returns first arg)
    let prog = load_engine(&["sel = a, b, c -> a"]).0;
    let mut e = Engine::new(&prog);
    
    // 1 arg → Closure
    let v1 = e.call_name("sel", vec![Value::Int(1)]).unwrap();
    let (_f1, p1) = closure_parts(&v1);
    assert_eq!(p1, vec![Value::Int(1)]);

    // 2 args → Closure
    let v2 = e.call_name("sel", vec![Value::Int(1), Value::Int(2)]).unwrap();
    let (_f2, p2) = closure_parts(&v2);
    assert_eq!(p2, vec![Value::Int(1), Value::Int(2)]);
    
    // 3 args → Int (full application)
    let v3 = e.call_name("sel", vec![Value::Int(1), Value::Int(2), Value::Int(3)]).unwrap();
    assert_eq!(v3, Value::Int(1));
}

#[test]
fn m1_closure_deep_eq() {
    let prog = load_engine(&["f = a, b -> a"]).0;
    let mut e = Engine::new(&prog);
    let v1 = e.call_name("f", vec![Value::Int(5)]).unwrap();
    let v2 = e.call_name("f", vec![Value::Int(5)]).unwrap();
    assert!(v1 == v2);

    let v3 = e.call_name("f", vec![Value::Int(6)]).unwrap();
    assert!(!(v1 == v3));
}

#[test]
fn m5_match_conditional_in_arm() {
    // Guards removed from match (§6): use `if` inside the arm body instead.
    let src = r#"
firstPos = List Int -> Int: xs -> match xs.peek
  None -> (-1)
  Some h -> if h > 0 -> h
    _ -> match xs.step
      None -> (-1)
      Some t -> firstPos t
"#;
    let prog = load_engine_with_prelude(&[src]).0;
    let mut e = Engine::new(&prog);
    // [ -5, -3, 7, 2 ] -> first positive is 7
    let lst = e.list_from_ints(&[-5, -3, 7, 2]).unwrap();
    assert_some_int(&prog, &e.call_name("firstPos", vec![lst]).unwrap(), 7);
    // [ -5, -3 ] -> no positive -> -1
    let lst2 = e.list_from_ints(&[-5, -3]).unwrap();
    assert_some_int(&prog, &e.call_name("firstPos", vec![lst2]).unwrap(), -1);
    // [ 4 ] -> first element 4
    let lst3 = e.list_from_ints(&[4]).unwrap();
    assert_some_int(&prog, &e.call_name("firstPos", vec![lst3]).unwrap(), 4);
}

#[test]
fn m5_match_conditional_falls_through() {
    // Conditional inside arm: if condition is False, the else-branch fires.
    let src = r#"
g = List Int -> Int: xs -> match xs.peek
  None -> (-1)
  Some h -> if h < 0 -> 100
    _ -> h
"#;
    let prog = load_engine_with_prelude(&[src]).0;
    let mut e = Engine::new(&prog);
    // [ 5, 6 ] -> 5<0 is False -> else branch -> 5
    let lst = e.list_from_ints(&[5, 6]).unwrap();
    assert_some_int(&prog, &e.call_name("g", vec![lst]).unwrap(), 5);
    // [ -5, 6 ] -> -5<0 is True -> 100
    let lst2 = e.list_from_ints(&[-5, 6]).unwrap();
    assert_some_int(&prog, &e.call_name("g", vec![lst2]).unwrap(), 100);
}

// ─── Value-position partial application (regression: flatMap with captured lambda) ───

#[test]
fn m1_partial_apply_in_value_position() {
    // `wrap x = first x` — "first x" is in VALUE position (function body),
    // 1 arg to a 2-param fn → must produce a Closure, not call with missing arg.
    let prog = load_engine(&["first = a, b -> a", "wrap = x -> first x"]).0;
    let mut e = Engine::new(&prog);
    let v = e.call_name("wrap", vec![Value::Int(42)]).unwrap();
    let (fn_id, pre_args) = closure_parts(&v);
    assert_eq!(pre_args, vec![Value::Int(42)]);
    let entry = prog.get_fn(fn_id);
    assert_eq!(entry.name, "first");
    assert_eq!(entry.params.len(), 2);
}

#[test]
fn m1_partial_apply_in_value_position_then_call() {
    // Verify the closure produced in value position is callable.
    let prog = load_engine(&["first = a, b -> a", "wrap = x -> first x"]).0;
    let mut e = Engine::new(&prog);
    let v = e.call_name("wrap", vec![Value::Int(42)]).unwrap();
    let (fn_id, mut args) = closure_parts(&v);
    // Call the closure with the remaining arg: first 42 99 = 42
    args.push(Value::Int(99));
    let result = e.call(fn_id, args).unwrap();
    assert_eq!(result, Value::Int(42));
}

#[test]
fn m1_nested_partial_apply_value_position() {
    // Simulates the flatMap scenario:
    //   outer x y = inner (y -> x + y) y   [after lambda lifting]
    //   inner f xs = ... applies f to elements
    // Here: `make_f x = add x` where add has 2 params.
    // Then apply the result to another value.
    let prog = load_engine(&[
        "add = a, b -> a + b",
        "make_f = x -> add x",       // value-position partial apply → Closure
        "apply_f = f, x -> f x",      // call the closure
    ]).0;
    let mut e = Engine::new(&prog);
    // make_f 10 → Closure { add, [10] }
    let cf = e.call_name("make_f", vec![Value::Int(10)]).unwrap();
    let (_fn_id, pre_args) = closure_parts(&cf);
    assert_eq!(pre_args, vec![Value::Int(10)]);
    // apply_f cf 5 → 10 + 5 = 15
    let result = e.call_name("apply_f", vec![cf, Value::Int(5)]).unwrap();
    assert_eq!(result, Value::Int(15));
}

#[test]
fn m1_flatmap_capture_repro() {
    // Exact scenario: flatMap with a capturing lambda → after lambda lifting,
    // the lifted fn is partially applied in value position.
    let src = r#"
testCapture = xs, ys -> flatMap (x -> map (y -> x + y) ys) xs
"#;
    let loaded = load_engine_with_prelude(&[src]).0;
    let mut e = Engine::new(&loaded);
    let lst_c = e.list_from_ints(&[1, 2]).unwrap();
    let lst_d = e.list_from_ints(&[10, 20]).unwrap();
    let cs = e.call_name("testCapture", vec![lst_c, lst_d])
        .unwrap_or_else(|err| panic!("runtime error: {err}"));
    let out = e.list_to_ints(&cs).unwrap();
    assert_eq!(out, vec![11, 21, 12, 22]);
}