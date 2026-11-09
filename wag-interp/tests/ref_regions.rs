//! Region-runtime tests: method dispatch, in-place region writes,
//! move-and-return, non-consuming match on handles/links (D056/D057).
//!
//! The program is the worked cursor-list example from spec §6.9
//! (mirrors `fixtures/regions/ref_cursor_walk.wag`, minus the actor harness) so these
//! tests assert real results, not just absence of panics.

use wag_check::check_with_prelude;
use wag_interp::{
    load_checked, prepare, Engine, Value,
    CTOR_PAIR2,
};

/// Region-resident cursor list: [10, 20, 30] built by single-step methods.
const SRC: &str = "\
WNode = WEnd | WNodeC val Int next (Ref WNode)
WH = WH head (Ref WNode) prev (Ref WNode) cur (Ref WNode) tail (Ref WNode) len Int

whNew = WH: ->
  a = WEnd
  b = WEnd
  c = WEnd
  d = WEnd
  WH a b c d 0

WH.init = Int -> WH: v ->
  n = WNodeC v WEnd
  head <- n
  cur <- n
  tail <- n
  len <- 1
  self

WH.append = Int -> WH: v ->
  n = WNodeC v WEnd
  tail.next <- n
  tail <- n
  len <- len + 1
  self

WH.next = Option WH: ->
  match cur
    WEnd     -> None
    WNodeC _ n ->
      match n
        WEnd       -> None
        WNodeC _ _ ->
          prev <- cur
          cur <- n
          Some self

WH.removeAt = (Option Int, WH): ->
  match cur
    WEnd     -> (None, self)
    WNodeC v rest ->
      spliced = match prev
        WEnd ->
          head <- rest
        _    ->
          prev.next <- rest
      len <- len - 1
      (Some v, self)

WH.curVal = Option Int: ->
  match cur
    WEnd     -> None
    WNodeC v _ -> Some v

listAtTail = WH -> WH: d ->
  c = copy d             -- rule 10: `d.next` consumes d; walk a copy, keep `d` for the base case
  n = c.next
  match n
    None   -> d
    Some e -> listAtTail e

build30 = WH: ->
  h0 = whNew
  h1 = h0.init 10
  h2 = h1.append 20
  h3 = h2.append 30
  h3

walkReport = (Int, Option Int): ->
  t = listAtTail (build30)
  (t.len, t.curVal)

removeLastReport = (Option Int, Int): ->
  t = listAtTail (build30)
  m = t.removeAt
  match m
    (r, h) -> (r, h.len)

emptyCurVal = Option Int: ->
  h = whNew
  h.curVal

emptyNext = Option WH: ->
  h = whNew
  h.next
";

/// Prelude `List.append` (D060): O(1) snoc through the tail link. Exercises
/// both the empty-list case (re-points head+tail) and the splice case.
const APPEND_SRC: &str = "\
appendTwo = Int: ->
  h0 = listNew
  h1 = h0.append 42
  h2 = h1.append 7
  h2.len

appendHeadVal = Int: ->
  h0 = listNew
  h1 = h0.append 42
  match h1.head
    LNodeC v _ -> v
    LEnd       -> -1

negAppend = Int: ->
  h0 = listNew
  h1 = h0.append -3
  match h1.peek
    Some v -> v
    None   -> 999
";

/// Prelude handle-list traversal loops over `List` (`fold`, D057 cursor
/// model): top-level tail recursion threading the handle through single-step
/// methods. Builtins are not first-class values, so `add` wraps `+`.
const TRAVERSAL_SRC: &str = "\
add = Int, Int -> Int: a, b -> a + b

build3 = List Int: ->
  h0 = listNew
  h1 = h0.append 1
  h2 = h1.append 2
  h3 = h2.append 3
  h3

foldSum = Int: ->
  fold add 0 (build3)

foldEmpty = Int: ->
  h = listNew
  fold add 9 h

double = Int -> Int: x -> x * 2

mapSum = Int: ->
  m = map double (build3)
  fold add 0 m

mapLen = Int: ->
  m = map double (build3)
  m.len

isEven = Int -> Bool: x -> x % 2 == 0

build4 = List Int: ->
  h0 = listNew
  h1 = h0.append 1
  h2 = h1.append 2
  h3 = h2.append 3
  h4 = h3.append 4
  h4

filterSum = Int: ->
  f = filter isEven (build4)
  fold add 0 f

filterLen = Int: ->
  f = filter isEven (build4)
  f.len

dup = Int -> List Int: x ->
  h0 = listNew
  h1 = h0.append (copy x)
  h2 = h1.append x
  h2

flatMapSum = Int: ->
  m = flatMap dup (build3)
  fold add 0 m

flatMapLen = Int: ->
  m = flatMap dup (build3)
  m.len

lenOf3 = Int: ->
  length (build3)

lenEmpty = Int: ->
  length (listNew)

isPos = Int -> Bool: x -> x > 0
isBig = Int -> Bool: x -> x > 100

anyPos4 = Bool: ->
  any isPos (build4)

anyBig4 = Bool: ->
  any isBig (build4)

allPos4 = Bool: ->
  all isPos (build4)

allBig4 = Bool: ->
  all isBig (build4)

prepEmptyFirst = Int: ->
  h0 = listNew
  h1 = h0.prepend 9
  p = h1.popFirst
  match p
    (o, r2) -> match o
      None   -> -1
      Some v -> v

prepLen4 = Int: ->
  b = build3
  h1 = b.prepend 0
  h1.len

revFirst = Int: ->
  r = reverse (build3)
  p = r.popFirst
  match p
    (o, r2) -> match o
      None   -> -1
      Some v -> v

revLen = Int: ->
  r = reverse (build3)
  r.len

inner12 = List Int: ->
  h0 = listNew
  h1 = h0.append 1
  h2 = h1.append 2
  h2

buildPairs = List (List Int): ->
  h0 = listNew
  h1 = h0.append (inner12)
  h2 = h1.append (build3)
  h2

bpLen = Int: ->
  length (buildPairs)

bpFirstLen = Int: ->
  b = buildPairs
  p = b.popFirst
  match p
    (o, b2) -> match o
      None   -> -1
      Some xs -> length xs

aaSum = Int: ->
  a = appendAll inner12 build3
  fold add 0 a

concatSum = Int: ->
  c = concat (buildPairs)
  fold add 0 c

concatLen = Int: ->
  c = concat (buildPairs)
  c.len
";

/// Full pipeline incl. field desugaring (same as the CLI), so surface
/// reads like `t.len` and method dispatch reach the VM in final form.
fn compile() -> wag_interp::LoadedProgram {
    let checked = check_with_prelude(SRC).unwrap_or_else(|e| panic!("check failed: {e}"));
    load_checked(checked.program(), checked.typed(), checked.cell_reuse())
}

/// `check_with_prelude` merges the prelude (D024/D043), so this exercises the real
/// `List.append` from `prelude/prelude.wag`.
fn compile_append() -> wag_interp::LoadedProgram {
    let checked = check_with_prelude(APPEND_SRC).unwrap_or_else(|e| panic!("check failed: {e}"));
    load_checked(checked.program(), checked.typed(), checked.cell_reuse())
}

fn call0(prog: &wag_interp::LoadedProgram, name: &str) -> Value {
    let mut e = Engine::new(prog);
    e.call_name(name, vec![]).expect("call failed")
}

/// Call a no-arg fn and assert the result equals an expected owned value.
fn call0_deep(prog: &wag_interp::LoadedProgram, name: &str, expected: Value) {
    let mut e = Engine::new(prog);
    let actual = e.call_name(name, vec![]).expect("call failed");
    assert!(
        actual == expected,
        "mismatch: actual={:?} expected={:?}",
        actual.brief_desc(),
        expected.brief_desc(),
    );
}

#[test]
#[ignore = "DAG with back-pointers (prev links) is unrepresentable in D081 owned model — trees only"]
fn walk_to_tail_reports_len_and_cursor_value() {
    // Walk [10, 20, 30]: cursor ends on the last node. Expects (3, Some(30)).
    let prog = compile();
    call0_deep(
        &prog,
        "walkReport",
        Value::Data(CTOR_PAIR2, vec![Value::Int(3), Value::Data(prog.opt_some_id(), vec![Value::Int(30)])]),
    );
}

#[test]
#[ignore = "DAG with back-pointers (prev links) is unrepresentable in D081 owned model — trees only"]
fn remove_at_tail_reports_value_and_new_len() {
    // Unlink the last node through the `prev` link path. Expects (Some(30), 2).
    let prog = compile();
    call0_deep(
        &prog,
        "removeLastReport",
        Value::Data(CTOR_PAIR2, vec![Value::Data(prog.opt_some_id(), vec![Value::Int(30)]), Value::Int(2)]),
    );
}

#[test]
fn fresh_handle_is_empty() {
    // Read-only methods on a fresh handle: cursor at the sentinel.
    let prog = compile();
    call0_deep(&prog, "emptyCurVal", Value::Data(prog.opt_none_id(), vec![]));
    call0_deep(&prog, "emptyNext", Value::Data(prog.opt_none_id(), vec![]));
}

#[test]
fn prelude_append_grows_len() {
    // First append hits the empty case (re-points head+tail); second splices.
    let prog = compile_append();
    assert_eq!(call0(&prog, "appendTwo"), Value::Int(2));
}

#[test]
fn prelude_append_negative_literal_d064() {
    // D064: `h0.append -3` lexes `-3` as one negative IntLit argument.
    let prog = compile_append();
    assert_eq!(call0(&prog, "negAppend"), Value::Int(-3));
}

fn compile_traversal() -> wag_interp::LoadedProgram {
    let checked = check_with_prelude(TRAVERSAL_SRC).unwrap_or_else(|e| panic!("check failed: {e}"));
    load_checked(checked.program(), checked.typed(), checked.cell_reuse())
}

#[test]
fn prelude_hfold_sums_elements() {
    // fold (+) 0 over [1, 2, 3] == 6.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "foldSum"), Value::Int(6));
}

#[test]
fn prelude_hfold_empty_returns_init() {
    // Empty list: the fold never steps and returns the initial accumulator.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "foldEmpty"), Value::Int(9));
}

#[test]
fn prelude_hmap_maps_elements() {
    // map (*2) over [1, 2, 3] == [2, 4, 6]; sum via fold == 12.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "mapSum"), Value::Int(12));
}

#[test]
fn prelude_hmap_preserves_len() {
    // Mapping [1, 2, 3] keeps three elements.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "mapLen"), Value::Int(3));
}

#[test]
fn prelude_hfilter_keeps_matching_elements() {
    // Evens of [1, 2, 3, 4] == [2, 4]: sum 6, len 2.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "filterSum"), Value::Int(6));
    assert_eq!(call0(&prog, "filterLen"), Value::Int(2));
}

#[test]
fn prelude_hflatmap_flattens_images() {
    // dup over [1, 2, 3] == [1, 1, 2, 2, 3, 3]: sum 12, len 6.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "flatMapSum"), Value::Int(12));
    assert_eq!(call0(&prog, "flatMapLen"), Value::Int(6));
}

#[test]
fn prelude_hlength_reads_len_field() {
    // O(1) len-field read: 3 after three appends, 0 on a fresh handle.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "lenOf3"), Value::Int(3));
    assert_eq!(call0(&prog, "lenEmpty"), Value::Int(0));
}

#[test]
fn prelude_hany_reports_presence() {
    // [1, 2, 3, 4]: some element > 0, none > 100.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "anyPos4"), Value::Bool(true));
    assert_eq!(call0(&prog, "anyBig4"), Value::Bool(false));
}

#[test]
fn prelude_hall_reports_universality() {
    // [1, 2, 3, 4]: every element > 0, not every element > 100.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "allPos4"), Value::Bool(true));
    assert_eq!(call0(&prog, "allBig4"), Value::Bool(false));
}

#[test]
fn prelude_prepend_puts_value_at_front() {
    // Prepend to an empty list: the value is first; prepend to [1,2,3] grows len.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "prepEmptyFirst"), Value::Int(9));
    assert_eq!(call0(&prog, "prepLen4"), Value::Int(4));
}

#[test]
fn prelude_hreverse_reverses_elements() {
    // reverse [1, 2, 3] == [3, 2, 1]: first element is 3, len preserved.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "revFirst"), Value::Int(3));
    assert_eq!(call0(&prog, "revLen"), Value::Int(3));
}

#[test]
fn iso_bp_len() {
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "bpLen"), Value::Int(2));
}

#[test]
fn iso_bp_pop_first_inner_len() {
    // popFirst on List (List Int): first inner list [1, 2] has len 2.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "bpFirstLen"), Value::Int(2));
}

#[test]
fn iso_append_all_sum() {
    // appendAll [1, 2] onto [1, 2, 3] == [1, 2, 3, 1, 2]: sum 9.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "aaSum"), Value::Int(9));
}

#[test]
fn prelude_hconcat_flattens_nested_lists() {
    // concat [[1, 2], [1, 2, 3]] == [1, 2, 1, 2, 3]: sum 9, len 5.
    let prog = compile_traversal();
    assert_eq!(call0(&prog, "concatSum"), Value::Int(9));
    assert_eq!(call0(&prog, "concatLen"), Value::Int(5));
}

#[test]
fn prelude_append_head_value() {
    // Read the snoc'd value back through the head Ref field.
    // `LNodeC.val` is bare `T` (D061): no Option wrapper.
    let prog = compile_append();
    assert_eq!(call0(&prog, "appendHeadVal"), Value::Int(42));
}

/// Borrowed traversal (D062): peek/step cursor walk + rewind; foldB/mapB
/// leave the source reusable — double use without consumption.
const BORROW_SRC: &str = "\
add = Int, Int -> Int: a, b -> a + b
double = Int -> Int: x -> x * 2

build3 = List Int: ->
  h0 = listNew
  h1 = h0.append 1
  h2 = h1.append 2
  h3 = h2.append 3
  h3

peekFirst = Option Int: ->
  h = build3
  h.peek

peekEmpty = Option Int: ->
  h = listNew
  h.peek

-- peek/step walk; sums the values seen. Cursor ends on the last node.
stepSumLoop = List Int, Int -> Int: h, acc ->
  p = h.peek
  match p
    None   -> acc
    Some v -> match h.step
      None     -> acc + v
      Some h2  -> stepSumLoop h2 (acc + v)

stepSum = Int: ->
  stepSumLoop (build3) 0

-- After a full walk the cursor sits on the last node; rewind restores front.
-- rule 10: pass a copy so `h` survives the consuming walk.
rewindPeekFront = Int: ->
  h = build3
  s = stepSumLoop (copy h) 0
  r = h.rewind
  match r.peek
    None   -> -1
    Some v -> v

-- One step forward: the cursor now sees element 2.
stepPeekSecond = Int: ->
  h = build3
  match h.step
    None     -> -1
    Some h2  -> match h2.peek
      None   -> -2
      Some v -> v

foldBSum = Int: ->
  foldB add 0 (build3)

foldBEmpty = Int: ->
  foldB add 9 (listNew)

-- Double use: the same source is mapped twice; both images sum to 12.
-- rule 10: each `mapB` consumes its owned handle, so both get copies.
mapBDoubleUse = Int: ->
  xs = build3
  m1 = mapB double (copy xs)
  s1 = fold add 0 m1
  m2 = mapB double (copy xs)
  s2 = fold add 0 m2
  s1 + s2

-- The source survives a borrowed map: rewound, it still sums to 6.
sourceIntactAfterMapB = Int: ->
  xs = build3
  m = mapB double (copy xs)
  r = xs.rewind
  fold add 0 r
";

fn compile_borrow() -> wag_interp::LoadedProgram {
    let checked = check_with_prelude(BORROW_SRC).unwrap_or_else(|e| panic!("check failed: {e}"));
    load_checked(checked.program(), checked.typed(), checked.cell_reuse())
}

#[test]
fn borrow_peek_reports_front_and_empty() {
    // peek is non-consuming: front of [1,2,3] is 1; empty handle gives None.
    let prog = compile_borrow();
    call0_deep(&prog, "peekFirst", Value::Data(prog.opt_some_id(), vec![Value::Int(1)]));
    call0_deep(&prog, "peekEmpty", Value::Data(prog.opt_none_id(), vec![]));
}

#[test]
fn borrow_step_walk_sums_all_elements() {
    // A manual peek/step walk visits every element exactly once.
    let prog = compile_borrow();
    assert_eq!(call0(&prog, "stepSum"), Value::Int(6));
}

#[test]
fn borrow_rewind_restores_front_after_full_walk() {
    // After walking to the last node, rewind puts the cursor back on element 1.
    let prog = compile_borrow();
    assert_eq!(call0(&prog, "rewindPeekFront"), Value::Int(1));
}

#[test]
fn borrow_step_advances_cursor() {
    // One step forward: peek now sees the second element.
    let prog = compile_borrow();
    assert_eq!(call0(&prog, "stepPeekSecond"), Value::Int(2));
}

#[test]
fn borrow_hfoldb_folds_without_consuming() {
    // foldB (+) 0 over [1,2,3] == 6; empty list returns the init.
    let prog = compile_borrow();
    assert_eq!(call0(&prog, "foldBSum"), Value::Int(6));
    assert_eq!(call0(&prog, "foldBEmpty"), Value::Int(9));
}

#[test]
fn borrow_hmapb_double_use_without_consuming() {
    // The same source is mapped twice — the borrowed walk rewinds on entry,
    // so both images are complete: 12 + 12 == 24.
    let prog = compile_borrow();
    assert_eq!(call0(&prog, "mapBDoubleUse"), Value::Int(24));
}

#[test]
fn borrow_source_intact_after_mapb() {
    // A borrowed map leaves the source list intact: rewound, it still sums to 6.
    let prog = compile_borrow();
    assert_eq!(call0(&prog, "sourceIntactAfterMapB"), Value::Int(6));
}

#[test]
fn undesugared_guard_error_carries_span() {
    // A field read loaded WITHOUT the check pass (`prepare`, no `wag_check`)
    // hits the VM desugaring guard — the error must name the node and carry
    // its source span so the offending location is findable (§7).
    let src = "Box = B v Int\nreadV = Box -> Int: b -> b.v\nmain = readV (B 3)\n";
    let (_p, _t, prog) = prepare(src).unwrap();
    let mut e = Engine::new(&prog);
    let err = e.call_name("main", vec![]).unwrap_err().to_string();
    assert!(err.contains("field access requires desugaring"), "{err}");
    // `b.v` in the source above spans 39..42.
    assert!(err.contains("@ 39..42"), "missing span in: {err}");
}

/// D-B move optimization: linear (owned-data) args are moved out of the caller's
/// env instead of deep-cloned — nested (non-tail) calls, move-and-return method
/// receivers, and read-only methods that must NOT consume.
const DB_SRC: &str = "\
WNode = WEnd | WNodeC val Int next (Ref WNode)
WH = WH head (Ref WNode) prev (Ref WNode) cur (Ref WNode) tail (Ref WNode) len Int

whNew = WH: ->
  a = WEnd
  b = WEnd
  c = WEnd
  d = WEnd
  WH a b c d 0

WH.init = Int -> WH: v ->
  n = WNodeC v WEnd
  head <- n
  cur <- n
  tail <- n
  len <- 1
  self

WH.curVal = Option Int: ->
  match cur
    WEnd     -> None
    WNodeC v _ -> Some v

innerLen = WH -> Int: h -> h.len

outerAddOne = WH -> Int: h ->
  innerLen h + 1        -- nested (non-tail) call: `h` is moved into `innerLen`

dbNestedMove = Int: ->
  h0 = whNew
  h1 = h0.init 5        -- move-and-return receiver: `h0` consumed by init
  outerAddOne h1        -- nested call moves `h1`; not reused afterwards

dbReadonlyReuse = Int: ->
  h0 = whNew
  h1 = h0.init 5
  p = h1.curVal         -- read-only method (Option Int): receiver NOT consumed
  l = h1.len            -- reuse after a read-only call
  match p
    Some v -> v + l
    None   -> -1
";

fn compile_db() -> wag_interp::LoadedProgram {
    let checked = check_with_prelude(DB_SRC).unwrap_or_else(|e| panic!("check failed: {e}"));
    load_checked(checked.program(), checked.typed(), checked.cell_reuse())
}

#[test]
fn d_b_nested_call_moves_linear_arg() {
    // [5]: append → len 1; nested `innerLen h` moves the handle; +1 == 2.
    assert_eq!(call0(&compile_db(), "dbNestedMove"), Value::Int(2));
}

#[test]
fn d_b_readonly_method_does_not_consume_receiver() {
    // curVal returns Option Int (no WH mention) → h1 survives for `h1.len`.
    assert_eq!(call0(&compile_db(), "dbReadonlyReuse"), Value::Int(6));
}