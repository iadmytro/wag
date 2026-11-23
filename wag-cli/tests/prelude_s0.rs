//! S0 — prelude mechanics (D053): explicit import, name clash = error,
//! prelude calls, cross-boundary letrec scope.
//!
//! These drive the *real* `prelude/prelude.wag` through the unified pipeline
//! (`compile_multi_module`) with an explicit `import prelude ..`, independent of
//! the test's working directory.

use std::fs;
use std::path::PathBuf;

use wag_cli::{compile_multi_module, Diagnostic};
use wag_cli::resolve::{ModuleSet, ResolvedModule};
use wag_interp::{
    load_checked, Engine, Value,
    CTOR_PAIR2, CTOR_PAIR3, CTOR_PRODUCT,
};

fn prelude_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("prelude")
        .join("prelude.wag")
}

fn prelude_src() -> String {
    fs::read_to_string(prelude_path())
        .unwrap_or_else(|e| panic!("failed to load prelude {}: {e}", prelude_path().display()))
}

/// Compile inline source through the CLI's multi-module pipeline
/// (`compile_multi_module` → `infer_multi_module`), same as `wagc run`.
fn compile_inline(src: &str, prelude: Option<&str>) -> Result<wag_check::CheckedProgram, Diagnostic> {
    let parse_err = |e: wag_syntax::ParseError| Diagnostic {
        stage: wag_cli::PipelineStage::Parse,
        span: e.span,
        message: e.message,
        hint: None,
        rule: None,
        type_stage: None,
        module_name: None,
        origin_span: None,
    };

    let mut modules = Vec::new();
    if let Some(p) = prelude {
        let pprog = wag_syntax::parse_desugared(p).map_err(parse_err)?;
        modules.push(ResolvedModule {
            name: "prelude".into(),
            path: PathBuf::from("prelude.wag"),
            source: p.to_string(),
            program: pprog,
            imports: vec![],
            package: "<test>".into(),
        });
    }


    let user_src = match prelude {
        Some(_) => format!("import\n  prelude\n    ..\n{src}"),
        None => src.to_string(),
    };
    let program = wag_syntax::parse_desugared(&user_src).map_err(parse_err)?;
    modules.push(ResolvedModule {
        name: "main".into(),
        path: PathBuf::from("main.wag"),
        source: user_src,
        program,
        imports: vec![],
        package: "<test>".into(),
    });

    let entry_index = modules.len() - 1;
    let module_set = ModuleSet { modules, entry_index };
    compile_multi_module(&module_set)
}

/// A user redeclaring an imported prelude name is a compile error (D053 —
/// local vs any imported name; remedy: alias or rename).
#[test]
fn prelude_name_clash_is_error() {
    let src = "abs = Int -> Int: x -> x\n";
    assert!(
        compile_inline(src, Some(&prelude_src())).is_err(),
        "user redeclaring imported prelude abs should be rejected"
    );
}

/// User code may call a prelude function (raw arithmetic binds first, then `abs`).
#[test]
fn prelude_fn_callable_from_user_code() {
    let src = "my_abs = Int -> Int: x -> abs (x + 1)\n";
    assert!(
        compile_inline(src, Some(&prelude_src())).is_ok(),
        "user code calling prelude abs should compile"
    );
}

/// Mutually-recursive user functions that also call a prelude function share one letrec scope.
#[test]
fn prelude_user_share_letrec_scope_with_mutual_recursion() {
    let src = r#"
p = Int -> Int: n -> if 
  n == 0 -> abs 5
  _ -> q (n - 1)

q = Int -> Int: n -> if 
  n == 0 -> 0
  _ -> p (n - 1)
"#;
    assert!(
        compile_inline(src, Some(&prelude_src())).is_ok(),
        "mutually-recursive user fns calling prelude abs should compile"
    );
}

/// With no prelude, the same name is a legal user binding (proves the error above is
/// specifically the prelude reserving `abs`, not a generic parse/type rejection).
#[test]
fn same_name_is_legal_without_prelude() {
    let src = "abs = Int -> Int: x -> x\n";
    assert!(
        compile_inline(src, None).is_ok(),
        "defining abs with no prelude should compile"
    );
}

/// Compile the real prelude merged with `src` (full pipeline: parse → type → check →
/// load) into a `LoadedProgram`. The caller then builds an `Engine` over it in the same
/// scope so the program outlives the engine.
fn loaded_with_prelude(src: &str) -> wag_interp::LoadedProgram {
    let checked = compile_inline(src, Some(&prelude_src()))
        .unwrap_or_else(|d| panic!("compile failed: stage={:?} msg={}", d.stage, d.message));
    load_checked(checked.program(), checked.typed(), checked.cell_reuse())
}

fn list_ints(e: &Engine, v: &Value) -> Vec<i64> {
    e.list_to_ints(v).unwrap()
}

fn bool_of(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        other => panic!("expected Bool, got {other:?}"),
    }
}

fn int_of(v: &Value) -> i64 {
    match v {
        Value::Int(i) => *i,
        other => panic!("expected Int, got {other:?}"),
    }
}

fn str_of(e: &wag_interp::Engine, v: &Value) -> String {
    e.program.value_to_string(v)
        .unwrap_or_else(|| panic!("expected String value, got {v:?}"))
}

/// True if `v` is a `None`.
fn is_opt_none(e: &wag_interp::Engine, v: &Value) -> bool {
    matches!(v, Value::Data(c, _) if *c == e.program.opt_none_id())
}

/// If `v` is a `Some(x)`, return the inner value.
fn opt_inner(e: &wag_interp::Engine, v: &Value) -> Option<Value> {
    if let Value::Data(c, fields) = v {
        if *c == e.program.opt_some_id() && fields.len() == 1 {
            return Some(fields[0].clone());
        }
    }
    if let Value::Data1(c, f) = v {
        if *c == e.program.opt_some_id() {
            return Some(f.as_ref().clone());
        }
    }
    None
}

/// If `v` is a product/pair, return its fields.
fn product_fields(v: &Value) -> Option<Vec<Value>> {
    if let Value::Data(c, fields) = v {
        if matches!(*c, CTOR_PAIR2 | CTOR_PAIR3 | CTOR_PRODUCT) {
            return Some(fields.clone());
        }
    }
    None
}

/// Prelude list ops — `take`/`drop` values (incl. edge cases n<=0 and n>len).
/// `take`/`drop` are not in the prelude: local popFirst-based loops.
#[test]
fn prelude_take_drop_values() {
    let src = "
takeN = Int, List Int -> List Int: n, h -> if n <= 0 -> listNew
    _          -> takeNLoop n h listNew
takeNLoop = Int, List Int, List Int -> List Int: n, h, out ->
  r = h.popFirst
  match r
    (o, h2) -> match o
      None   -> out
      Some v -> if n == 1 -> out.append v
                _          -> takeNLoop (n - 1) h2 (out.append v)
dropN = Int, List Int -> List Int: n, h -> if n <= 0 -> h
    _          -> dropNLoop n h
dropNLoop = Int, List Int -> List Int: n, h ->
  r = h.popFirst
  match r
    (o, h2) -> match o
      None   -> h2
      Some _ -> if n == 1 -> h2
                _          -> dropNLoop (n - 1) h2
xs = [1, 2, 3, 4, 5]
t2 = takeN 2 xs
n0 = takeN 0 xs
nbig = takeN 99 xs
d1 = dropN 1 xs
d0 = dropN 0 xs
dbig = dropN 99 xs
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    let t2 = e.call_name("t2", vec![]).unwrap();
    let n0 = e.call_name("n0", vec![]).unwrap();
    let nbig = e.call_name("nbig", vec![]).unwrap();
    let d1 = e.call_name("d1", vec![]).unwrap();
    let d0 = e.call_name("d0", vec![]).unwrap();
    let dbig = e.call_name("dbig", vec![]).unwrap();
    assert_eq!(list_ints(&e, &t2), vec![1, 2]);
    assert_eq!(list_ints(&e, &n0), Vec::<i64>::new());
    assert_eq!(list_ints(&e, &nbig), vec![1, 2, 3, 4, 5]);
    assert_eq!(list_ints(&e, &d1), vec![2, 3, 4, 5]);
    assert_eq!(list_ints(&e, &d0), vec![1, 2, 3, 4, 5]);
    assert_eq!(list_ints(&e, &dbig), Vec::<i64>::new());
}

/// Prelude `unzip` — inverse of `zip`: `unzip (zip xs ys)` recovers both lists.
///
/// A Pair of lists is a nested owned data value, which we destructure via the
/// `product_fields` helper below. We destructure it exactly once in WAG
/// (it is linear — each list usable exactly once) and run each recovered list through a
/// single-pass `lenSum` (one traversal yields both its length and element sum), giving a
/// nested Pair of `(length, sum)` pairs — all pointer-free `Int`s we can assert on.
/// `unzip` is not in the prelude: local popFirst-based loop; `lenSum` via `fold`.
#[test]
fn prelude_unzip_values() {
    let src = "
lsStep = Int, (Int, Int) -> (Int, Int): x, acc -> match acc
  Pair l s -> (l + 1, s + x)
lenSum = xs -> fold lsStep ((0, 0)) xs
unzipP = List (Int, Int) -> (List Int, List Int): ps -> unzipPLoop ps listNew listNew
unzipPLoop = List (Int, Int), List Int, List Int -> (List Int, List Int): ps, a, b ->
  r = ps.popFirst
  match r
    (o, p2) -> match o
      None   -> (a, b)
      Some p -> match p
        Pair x y -> unzipPLoop p2 (a.append x) (b.append y)
xs = [1, 2, 3]
ys = [10, 20, 30]
u = unzipP (zip xs ys)
res = match u
  Pair xs2 ys2 -> (lenSum xs2, lenSum ys2)
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    let res = e.call_name("res", vec![]).unwrap();
    // res = (Pair 3 6, Pair 3 60) — tuple literals evaluate to owned data values.
    let outer = product_fields(&res)
        .unwrap_or_else(|| panic!("expected nested Pair, got {}", res.brief_desc()));
    assert_eq!(outer.len(), 2);
    {
        let (al, as_) = pair_ints(&outer[0]);
        let (bl, bs_) = pair_ints(&outer[1]);
        assert_eq!(al, 3);
        assert_eq!(as_, 6);    // 1 + 2 + 3
        assert_eq!(bl, 3);
        assert_eq!(bs_, 60);   // 10 + 20 + 30
    }
}

/// Decompose a 2-tuple of two `Int`s.
fn pair_ints(v: &Value) -> (i64, i64) {
    let fields = product_fields(v)
        .unwrap_or_else(|| panic!("expected Pair Int Int, got {:?}", v.brief_desc()));
    assert_eq!(fields.len(), 2, "expected 2-field pair");
    (
        match &fields[0] {
            Value::Int(n) => *n,
            o => panic!("expected Int, got {o:?}"),
        },
        match &fields[1] {
            Value::Int(n) => *n,
            o => panic!("expected Int, got {o:?}"),
        },
    )
}

/// `&`/`|` on Bool — full truth tables.
#[test]
fn prelude_bool_and_or_truth_table() {
    let src = "
aTT = True & True
aTF = True & False
aFT = False & True
aFF = False & False
oFF = False | False
oFT = False | True
oTF = True | False
oTT = True | True
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    assert!(bool_of(&e.call_name("aTT", vec![]).unwrap()));
    assert!(!bool_of(&e.call_name("aTF", vec![]).unwrap()));
    assert!(!bool_of(&e.call_name("aFT", vec![]).unwrap()));
    assert!(!bool_of(&e.call_name("aFF", vec![]).unwrap()));
    assert!(!bool_of(&e.call_name("oFF", vec![]).unwrap()));
    assert!(bool_of(&e.call_name("oFT", vec![]).unwrap()));
    assert!(bool_of(&e.call_name("oTF", vec![]).unwrap()));
    assert!(bool_of(&e.call_name("oTT", vec![]).unwrap()));
}

/// Zip comprehensions through the real prelude — 2- and 3-element, guard, fold.
#[test]
fn prelude_zip_comprehension_values() {
    let src = "
xs = [1, 2, 3]
ys = [10, 20, 30, 40]
zmap = [x, y <- xs, ys | _ -> x + y]
zdot = [x, y <- xs, ys | s = 0 | _ -> x * y + s]
zs = [100, 200, 300]
trip = [a, b, c <- xs, ys, zs | _ -> a + b + c]
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    let zmap = e.call_name("zmap", vec![]).unwrap();
    let zdot = e.call_name("zdot", vec![]).unwrap();
    let trip = e.call_name("trip", vec![]).unwrap();
    // zip to shortest (3 of xs vs 4 of ys): [11, 22, 33]
    assert_eq!(list_ints(&e, &zmap), vec![11, 22, 33]);
    // dot product: 1*10 + 2*20 + 3*30 = 140
    match zdot {
        Value::Int(n) => assert_eq!(n, 140),
        _ => {
            let inner = opt_inner(&e, &zdot)
                .unwrap_or_else(|| panic!("expected Int 140 or Some(Int), got {}", zdot.brief_desc()));
            match inner {
                Value::Int(n) => assert_eq!(n, 140),
                o => panic!("expected Int 140, got Some({o:?})"),
            }
        }
    }
    // 3-element zip to shortest (3): [111, 222, 333]
    assert_eq!(list_ints(&e, &trip), vec![111, 222, 333]);
}

/// Predicate prelude functions: any, all, partition.
/// `any`/`all` are in the prelude; `partition` is not — local popFirst-based loop.
#[test]
fn prelude_predicates_values() {
    let src = r#"
isEven = x -> if
  x % 2 == 0 -> True
  _ -> False

xs = [1, 2, 3, 4]

a1 = any isEven xs
a2 = all isEven xs
a3 = any isEven [1, 3, 5]
a4 = all isEven [2, 4, 6]
lsStep = Int, (Int, Int) -> (Int, Int): x, acc -> match acc
  Pair l s -> (l + 1, s + x)
lenSum = xs -> fold lsStep ((0, 0)) xs
partP = (Int -> Bool), List Int -> (List Int, List Int): p, h -> partPLoop p h listNew listNew
partPLoop = (Int -> Bool), List Int, List Int, List Int -> (List Int, List Int): p, h, yes, no ->
  r = h.popFirst
  match r
    (o, h2) -> match o
      None   -> (yes, no)
      Some v -> if p (copy v) -> partPLoop p h2 (yes.append v) no
                _              -> partPLoop p h2 yes (no.append v)
p = partP isEven xs
res = match p
  Pair evens odds -> (lenSum evens, lenSum odds)
"#;
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);

    // any isEven [1,2,3,4] = True
    assert!(bool_of(&e.call_name("a1", vec![]).unwrap()), "any isEven [1,2,3,4] should be True");
    // all isEven [1,2,3,4] = False
    assert!(!bool_of(&e.call_name("a2", vec![]).unwrap()), "all isEven [1,2,3,4] should be False");
    // any isEven [1,3,5] = False
    assert!(!bool_of(&e.call_name("a3", vec![]).unwrap()), "any isEven [1,3,5] should be False");
    // all isEven [2,4,6] = True
    assert!(bool_of(&e.call_name("a4", vec![]).unwrap()), "all isEven [2,4,6] should be True");

    // partition isEven [1,2,3,4] = Pair [2,4] [1,3]
    // Destructure in WAG (Pair of lists is arena-resident, linear):
    //   p = Pair evens odds
    //   evensLen = length evens
    //   oddsLen = length odds
    //   evensSum = fold (+) 0 evens
    //   oddsSum = fold (+) 0 odds
    //   res = Pair (Pair evensLen evensSum) (Pair oddsLen oddsSum)
    let res = e.call_name("res", vec![]).unwrap();
    let outer = product_fields(&res)
        .unwrap_or_else(|| panic!("expected Pair, got {}", res.brief_desc()));
    assert_eq!(outer.len(), 2);
    {
        let (el, es) = pair_ints(&outer[0]);
        let (ol, os) = pair_ints(&outer[1]);
        assert_eq!(el, 2, "evens length");
        assert_eq!(es, 6, "evens sum (2+4)");
        assert_eq!(ol, 2, "odds length");
        assert_eq!(os, 4, "odds sum (1+3)");
    }
}

#[test]
fn prelude_range_last_values() {
    // `last` is not in the prelude: walk to the end via borrowed step/peek.
    let src = r#"
r1 = range 1 5
r2 = range 5 1
rLen1 = length r1
rLen2 = length r2
lastWalk = List Int -> Option Int: lst -> match lst.peek
  None   -> None
  Some v -> match lst.step
    None   -> Some v
    Some t -> lastWalk t
last1 = lastWalk [10, 20, 30]
lastEmpty = lastWalk []
"#;
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);

    // range 1 5 has length 5
    assert_eq!(int_of(&e.call_name("rLen1", vec![]).unwrap()), 5, "range 1 5 length");
    // range 5 1 is empty (lo > hi)
    assert_eq!(int_of(&e.call_name("rLen2", vec![]).unwrap()), 0, "range 5 1 length");

    // last of [10, 20, 30] = Some 30
    let l1 = e.call_name("last1", vec![]).unwrap();
    {
        let inner = opt_inner(&e, &l1)
            .unwrap_or_else(|| panic!("expected OptSome, got {}", l1.brief_desc()));
        assert_eq!(int_of(&inner), 30, "last [10,20,30]");
    }

    // last of [] = None
    let l2 = e.call_name("lastEmpty", vec![]).unwrap();
    assert!(is_opt_none(&e, &l2), "last Empty should be None");
}

#[test]
fn prelude_repeat_values() {
    // `repeat` is not in the prelude: n copies of x via map over a range.
    let src = r#"
r1 = map (x -> 7) (range 1 3)
r1Len = length r1
r1Sum = fold (a, b -> a + b) 0 r1
r2 = map (x -> 1) (range 1 0)
r2Len = length r2
r3 = map (x -> 5) (range 1 (-1))
r3Len = length r3
"#;
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);

    assert_eq!(int_of(&e.call_name("r1Len", vec![]).unwrap()), 3, "repeat 3 7 length");
    assert_eq!(int_of(&e.call_name("r1Sum", vec![]).unwrap()), 21, "repeat 3 7 sum = 21");
    assert_eq!(int_of(&e.call_name("r2Len", vec![]).unwrap()), 0, "repeat 0 1 length");
    assert_eq!(int_of(&e.call_name("r3Len", vec![]).unwrap()), 0, "repeat -1 5 length");
}

#[test]
fn prelude_zipwith_values() {
    // `zipWith` is not in the prelude: express it as map over `zip`.
    let src = r#"
zw1 = map (p -> match p
  Pair a b -> a + b
) (zip [1, 2, 3] [10, 20, 30])
zw1Len = length zw1
zw1Sum = fold (a, b -> a + b) 0 zw1
zw2 = map (p -> match p
  Pair a b -> a + b
) (zip [1, 2] [10])
zw2Len = length zw2
"#;
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);

    assert_eq!(int_of(&e.call_name("zw1Len", vec![]).unwrap()), 3, "zipWith length");
    assert_eq!(int_of(&e.call_name("zw1Sum", vec![]).unwrap()), 66, "zipWith sum = 11+22+33");
    assert_eq!(int_of(&e.call_name("zw2Len", vec![]).unwrap()), 1, "zipWith shortest");
}

#[test]
fn prelude_take_drop_while_values() {
    // takeWhile/dropWhile are not in the prelude: local popFirst-based loops.
    let src = r#"
isPos = x -> if
  x > 0 -> True
  _ -> False
isNeg = x -> if
  x < 0 -> True
  _ -> False
takeW = (Int -> Bool), List Int -> List Int: p, h -> takeWLoop p h listNew
takeWLoop = (Int -> Bool), List Int, List Int -> List Int: p, h, out ->
  r = h.popFirst
  match r
    (o, h2) -> match o
      None   -> out
      Some v -> if p (copy v) -> takeWLoop p h2 (out.append v)
                _      -> out
dropW = (Int -> Bool), List Int -> List Int: p, h -> dropWLoop p h
dropWLoop = (Int -> Bool), List Int -> List Int: p, h ->
  r = h.popFirst
  match r
    (o, h2) -> match o
      None   -> h2
      Some v -> if p (copy v) -> dropWLoop p h2
                _      -> h2.prepend v
tw = takeW isPos [1, 2, 3, -4, 5]
twLen = length tw
dw = dropW isNeg [-1, -2, 3, 4]
dwLen = length dw
"#;
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);

    assert_eq!(int_of(&e.call_name("twLen", vec![]).unwrap()), 3, "takeWhile [1,2,3,-4,5] = 3");
    assert_eq!(int_of(&e.call_name("dwLen", vec![]).unwrap()), 2, "dropWhile [-1,-2,3,4] with isNeg = 2");
}

/// Regression: prelude list combinators are polymorphic and work on nested lists.
/// `filter : forall a. (a -> Bool), List a -> List a` — must work on `List (List Int)`.
/// `reverse : forall a. List a -> List a` — must work on `List (List Int)`.
#[test]
fn prelude_polymorphic_nested_list() {
    // tail (local, via popFirst) on List (List Int): Option (List (List Int))
    let src = "
tailW = List (List Int) -> Option (List (List Int)): h -> match h.popFirst
  (Some _, h2) -> Some h2
  (None, _)    -> None
xs = [[1, 2], [3]]
t1 = tailW xs
t1Len = match t1
  Some rest -> length rest
  None -> -1
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    // tail [[1,2],[3]] = Some [[3]] → length 1
    assert_eq!(int_of(&e.call_name("t1Len", vec![]).unwrap()), 1, "tail nested list length");

    // filter on List (List Int): keep non-empty sub-lists
    let src = "
notEmpty = List Int -> Bool: xs -> if
  xs.isEmpty -> False
  _ -> True
xs = [[1], [], [2, 3]]
f1 = filter notEmpty xs
f1Len = length f1
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    // filter non-empty from [[1],[],[2,3]] = [[1],[2,3]] → length 2
    assert_eq!(int_of(&e.call_name("f1Len", vec![]).unwrap()), 2, "filter nested list length");

    // reverse on List (List Int)
    let src = "
xs = [[1], [2, 3]]
r1 = reverse xs
r1Len = length r1
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    // reverse [[1],[2,3]] = [[2,3],[1]] → length 2
    assert_eq!(int_of(&e.call_name("r1Len", vec![]).unwrap()), 2, "reverse nested list length");

    // map on List (List Int) with a function List Int -> Int
    let src = "
xs = [[1, 2], [3, 4]]
m1 = map length xs
m1Sum = fold (a, b -> a + b) 0 m1
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    // map length [[1,2],[3,4]] = [2,2] → sum 4
    assert_eq!(int_of(&e.call_name("m1Sum", vec![]).unwrap()), 4, "map length nested list sum");
}

/// `&`, `|`, `!` overloaded on Bool (logical AND/OR/NOT).
#[test]
fn bool_ops_amp_pipe_not() {
    // Truth table for & (logical AND)
    let src = "
a1 = True & True
a2 = True & False
a3 = False & True
a4 = False & False
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    assert!(bool_of(&e.call_name("a1", vec![]).unwrap()), "True & True");
    assert!(!bool_of(&e.call_name("a2", vec![]).unwrap()), "True & False");
    assert!(!bool_of(&e.call_name("a3", vec![]).unwrap()), "False & True");
    assert!(!bool_of(&e.call_name("a4", vec![]).unwrap()), "False & False");

    // Truth table for | (logical OR)
    let src = "
b1 = True | True
b2 = True | False
b3 = False | True
b4 = False | False
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    assert!(bool_of(&e.call_name("b1", vec![]).unwrap()), "True | True");
    assert!(bool_of(&e.call_name("b2", vec![]).unwrap()), "True | False");
    assert!(bool_of(&e.call_name("b3", vec![]).unwrap()), "False | True");
    assert!(!bool_of(&e.call_name("b4", vec![]).unwrap()), "False | False");

    // ! (logical NOT)
    let src = "
c1 = !True
c2 = !False
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    assert!(!bool_of(&e.call_name("c1", vec![]).unwrap()), "!True = False");
    assert!(bool_of(&e.call_name("c2", vec![]).unwrap()), "!False = True");

    // Precedence: & binds tighter than |  (same as Int)
    // True | False & False  =  True | (False & False)  =  True | False  =  True
    let src = "
d1 = True | False & False
d2 = False | True & True
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    assert!(bool_of(&e.call_name("d1", vec![]).unwrap()), "True | (False & False) = True");
    assert!(bool_of(&e.call_name("d2", vec![]).unwrap()), "False | (True & True) = True");

    // Mixed with comparisons (parens needed: & binds tighter than ==)
    // (1 == 1) & (2 == 3)  =  True & False  =  False
    let src = "
e1 = (1 == 1) & (2 == 3)
e2 = (1 == 1) | (2 == 3)
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    assert!(!bool_of(&e.call_name("e1", vec![]).unwrap()), "(1==1) & (2==3) = False");
    assert!(bool_of(&e.call_name("e2", vec![]).unwrap()), "(1==1) | (2==3) = True");

    // Int bitwise still works
    let src = "
f1 = 5 & 3
f2 = 5 | 3
f3 = bitNot 0
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    assert_eq!(int_of(&e.call_name("f1", vec![]).unwrap()), 1, "5 & 3 = 1");
    assert_eq!(int_of(&e.call_name("f2", vec![]).unwrap()), 7, "5 | 3 = 7");
    assert_eq!(int_of(&e.call_name("f3", vec![]).unwrap()), -1, "bitNot 0 = -1");

    // `!` as a direct function argument (no parens needed)
    let src = "
not = Bool -> Bool: b -> !b
n1 = not True
n2 = not False
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    assert!(!bool_of(&e.call_name("n1", vec![]).unwrap()), "not True = False");
    assert!(bool_of(&e.call_name("n2", vec![]).unwrap()), "not False = True");
}

/// `&` on mixed Bool/Int must be a type error.
#[test]
fn bool_ops_mixed_type_rejected() {
    let src = "
x = True & 5
";
    let result = compile_inline(src, Some(&prelude_src()));
    assert!(result.is_err(), "True & 5 should be a type error");

    let src = "
y = !5.0
";
    // 5.0 is not a valid literal in WAG, so this should fail at parse or type level
    let result = compile_inline(src, Some(&prelude_src()));
    assert!(result.is_err(), "!5.0 should fail");
}

/// `show: Int -> String` — convert integers to strings.
#[test]
fn show_int_to_string() {
    let src = "
s0 = show 0
s42 = show 42
sNeg = show (0 - 42)
s1 = show 1
s9 = show 9
s10 = show 10
s123 = show 123
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    let r0 = e.call_name("s0", vec![]).unwrap();
    let r42 = e.call_name("s42", vec![]).unwrap();
    let r_neg = e.call_name("sNeg", vec![]).unwrap();
    let r1 = e.call_name("s1", vec![]).unwrap();
    let r9 = e.call_name("s9", vec![]).unwrap();
    let r10 = e.call_name("s10", vec![]).unwrap();
    let r123 = e.call_name("s123", vec![]).unwrap();
    assert_eq!(str_of(&e, &r0), "0");
    assert_eq!(str_of(&e, &r42), "42");
    assert_eq!(str_of(&e, &r_neg), "-42");
    assert_eq!(str_of(&e, &r1), "1");
    assert_eq!(str_of(&e, &r9), "9");
    assert_eq!(str_of(&e, &r10), "10");
    assert_eq!(str_of(&e, &r123), "123");
}

#[test]
fn map_two_instantiations() {
    // map with TWO different type instantiations in the same program.
    // First: Int -> Int (PF->PF), Second: List Int -> Int (non-PF->PF)
    let src = "
inc = x -> x + 1
subSum = xs -> fold (x, acc -> acc + x) 0 xs

xs = [1, 2, 3]
r1 = map inc xs

xss = [[1, 2], [3, 4]]
r2 = map subSum xss
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    // r1 = [2, 3, 4]
    let r1 = e.call_name("r1", vec![]).unwrap();
    assert_eq!(list_ints(&e, &r1), vec![2, 3, 4]);
    // r2 = [3, 7]
    let r2 = e.call_name("r2", vec![]).unwrap();
    assert_eq!(list_ints(&e, &r2), vec![3, 7]);
}

#[test]
fn map_two_instantiations_in_bodies() {
    // Same as map_two_instantiations but map is called INSIDE function bodies.
    let src = "
inc = x -> x + 1
subSum = xs -> fold (x, acc -> acc + x) 0 xs

t1 = List Int -> Int: xs -> fold (x, acc -> acc + x) 0 (map inc xs)

t2 = List (List Int) -> Int: xss -> fold (x, acc -> acc + x) 0 (map subSum xss)

xs1 = [1, 2, 3]
r1 = t1 xs1

xss1 = [[1, 2], [3, 4]]
r2 = t2 xss1
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    let r1 = e.call_name("r1", vec![]).unwrap();
    assert_eq!(int_of(&r1), 9, "t1: sum of [2,3,4]");
    let r2 = e.call_name("r2", vec![]).unwrap();
    assert_eq!(int_of(&r2), 10, "t2: sum of [3,7]");
}

/// D068 — `copy`-based test-and-keep combinators run correctly on pointer-free
/// element types (Int): filter keeps matching elements, map-over-range builds n
/// copies, isAlphaNum/isWhitespace reuse their Int argument via `copy`.
#[test]
fn prelude_copy_based_combinators_on_int_lists() {
    let src = "
gt5 = x -> x > 5
f1 = filter gt5 [1, 99, 3]
n2 = length (map (x -> 7) (range 1 3))
an = isAlphaNum 65
nw = isWhitespace 42
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    let f1 = e.call_name("f1", vec![]).unwrap();
    assert_eq!(list_ints(&e, &f1), vec![99]);
    assert_eq!(int_of(&e.call_name("n2", vec![]).unwrap()), 3);
    assert!(bool_of(&e.call_name("an", vec![]).unwrap()));
    assert!(!bool_of(&e.call_name("nw", vec![]).unwrap()));
}

/// Option helpers: `optionDefault`, `optionMap`, `optionFilter`, `optionBind`.
#[test]
fn prelude_option_helpers() {
    let src = r#"
 dNone = optionDefault 0 None
 dSome = optionDefault 0 (Some 5)
 mNone = optionMap (x -> x + 1) None
 mSome = optionMap (x -> x + 1) (Some 4)
 fPass = optionFilter (x -> x > 3) (Some 5)
 fFail = optionFilter (x -> x > 3) (Some 2)
 bNone = optionBind (x -> Some (x * 2)) None
 bSome = optionBind (x -> Some (x * 2)) (Some 7)
"#;
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);

    assert_eq!(int_of(&e.call_name("dNone", vec![]).unwrap()), 0, "optionDefault None");
    assert_eq!(int_of(&e.call_name("dSome", vec![]).unwrap()), 5, "optionDefault Some");

    let mN = e.call_name("mNone", vec![]).unwrap();
    assert!(is_opt_none(&e, &mN), "optionMap None = None");
    let mS = e.call_name("mSome", vec![]).unwrap();
    {
        let inner = opt_inner(&e, &mS)
            .unwrap_or_else(|| panic!("expected OptSome, got {}", mS.brief_desc()));
        assert_eq!(int_of(&inner), 5, "optionMap Some 4 +1");
    }

    let fP = e.call_name("fPass", vec![]).unwrap();
    {
        let inner = opt_inner(&e, &fP)
            .unwrap_or_else(|| panic!("expected OptSome, got {}", fP.brief_desc()));
        assert_eq!(int_of(&inner), 5, "optionFilter pass");
    }
    let fF = e.call_name("fFail", vec![]).unwrap();
    assert!(is_opt_none(&e, &fF), "optionFilter fail = None");

    let bN = e.call_name("bNone", vec![]).unwrap();
    assert!(is_opt_none(&e, &bN), "optionBind None = None");
    let bS = e.call_name("bSome", vec![]).unwrap();
    {
        let inner = opt_inner(&e, &bS)
            .unwrap_or_else(|| panic!("expected OptSome, got {}", bS.brief_desc()));
        assert_eq!(int_of(&inner), 14, "optionBind Some 7 *2");
    }
}

/// Consuming list ops: `sum`, `min`, `max`.
#[test]
fn prelude_sum_min_max() {
    let src = r#"
 s1 = sum [1, 2, 3, 4]
 sEmpty = sum []
 mMin = min [3, 1, 4, 1, 5]
 mMax = max [3, 1, 4, 1, 5]
 mMinE = min []
 mMaxE = max []
"#;
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);

    assert_eq!(int_of(&e.call_name("s1", vec![]).unwrap()), 10, "sum [1,2,3,4]");
    assert_eq!(int_of(&e.call_name("sEmpty", vec![]).unwrap()), 0, "sum [] = 0");

    let mn = e.call_name("mMin", vec![]).unwrap();
    {
        let inner = opt_inner(&e, &mn)
            .unwrap_or_else(|| panic!("expected OptSome, got {}", mn.brief_desc()));
        assert_eq!(int_of(&inner), 1, "min [3,1,4,1,5]");
    }
    let mx = e.call_name("mMax", vec![]).unwrap();
    {
        let inner = opt_inner(&e, &mx)
            .unwrap_or_else(|| panic!("expected OptSome, got {}", mx.brief_desc()));
        assert_eq!(int_of(&inner), 5, "max [3,1,4,1,5]");
    }
    let mnE = e.call_name("mMinE", vec![]).unwrap();
    assert!(is_opt_none(&e, &mnE), "min [] = None");
    let mxE = e.call_name("mMaxE", vec![]).unwrap();
    assert!(is_opt_none(&e, &mxE), "max [] = None");
}

/// Consuming list ops: `drop`, `take` (prelude versions).
#[test]
fn prelude_drop_take() {
    let src = r#"
 d1 = drop 2 [10, 20, 30, 40]
 d0 = drop 0 [10, 20, 30]
 dBig = drop 99 [10, 20]
 t1 = take 2 [10, 20, 30, 40]
 t0 = take 0 [10, 20, 30]
 tBig = take 99 [10, 20]
"#;
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);

    let d1 = e.call_name("d1", vec![]).unwrap();
    assert_eq!(list_ints(&e, &d1), vec![30, 40], "drop 2");
    let d0 = e.call_name("d0", vec![]).unwrap();
    assert_eq!(list_ints(&e, &d0), vec![10, 20, 30], "drop 0");
    let dB = e.call_name("dBig", vec![]).unwrap();
    assert_eq!(list_ints(&e, &dB), Vec::<i64>::new(), "drop 99");
    let t1 = e.call_name("t1", vec![]).unwrap();
    assert_eq!(list_ints(&e, &t1), vec![10, 20], "take 2");
    let t0 = e.call_name("t0", vec![]).unwrap();
    assert_eq!(list_ints(&e, &t0), Vec::<i64>::new(), "take 0");
    let tB = e.call_name("tBig", vec![]).unwrap();
    assert_eq!(list_ints(&e, &tB), vec![10, 20], "take 99");
}

/// Consuming list ops: `partition`, `listCount`.
#[test]
fn prelude_partition_listcount() {
    let src = r#"
 isEven = x -> if
   x % 2 == 0 -> True
   _ -> False
 p = partition isEven [1, 2, 3, 4, 5, 6]
 res = match p
   Pair evens odds -> (length evens, length odds)
 c1 = listCount isEven [1, 2, 3, 4, 5, 6]
 c0 = listCount isEven [1, 3, 5]
"#;
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);

    let res = e.call_name("res", vec![]).unwrap();
    {
        let (el, ol) = pair_ints(&res);
        assert_eq!(el, 3, "partition evens length");
        assert_eq!(ol, 3, "partition odds length");
    }

    assert_eq!(int_of(&e.call_name("c1", vec![]).unwrap()), 3, "listCount isEven [1..6] = 3");
    assert_eq!(int_of(&e.call_name("c0", vec![]).unwrap()), 0, "listCount isEven [1,3,5] = 0");
}

/// Borrowed list ops: `nth`, `find`, `last`.
#[test]
fn prelude_nth_find_last() {
    // Use range-based lists to avoid the generic+literal inference issue.
    let src = r#"
 n0 = nth 0 (range 10 30)
 n1 = nth 1 (range 10 30)
 nBig = nth 99 (range 10 20)
 isPos = Int -> Bool: x -> if
   x > 0 -> True
   _ -> False
 f1 = find isPos (range (-5) 8)
 fNone = find isPos (range (-5) (-3))
 l1 = last (range 10 30)
 lE = last (range 5 1)
"#;
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);

    let n0v = e.call_name("n0", vec![]).unwrap();
    {
        let inner = opt_inner(&e, &n0v)
            .unwrap_or_else(|| panic!("expected OptSome, got {}", n0v.brief_desc()));
        assert_eq!(int_of(&inner), 30, "nth 0 of range(10,30) = 30 (descending)");
    }
    let n1v = e.call_name("n1", vec![]).unwrap();
    {
        let inner = opt_inner(&e, &n1v)
            .unwrap_or_else(|| panic!("expected OptSome, got {}", n1v.brief_desc()));
        assert_eq!(int_of(&inner), 29, "nth 1 of range(10,30) = 29");
    }
    let nB = e.call_name("nBig", vec![]).unwrap();
    assert!(is_opt_none(&e, &nB), "nth 99 of range(10,20) = None");

    // range(-5,8) = [8,7,...,-5]; first positive is 8.
    let f1v = e.call_name("f1", vec![]).unwrap();
    {
        let inner = opt_inner(&e, &f1v)
            .unwrap_or_else(|| panic!("expected OptSome, got {}", f1v.brief_desc()));
        assert_eq!(int_of(&inner), 8, "find isPos in range(-5,8)");
    }
    // range(-5,-3) = [-3,-4,-5]; no positives.
    let fN = e.call_name("fNone", vec![]).unwrap();
    assert!(is_opt_none(&e, &fN), "find isPos in range(-5,-3) = None");

    // last of range(10,30) = [30,...,10] → 10.
    let l1v = e.call_name("l1", vec![]).unwrap();
    {
        let inner = opt_inner(&e, &l1v)
            .unwrap_or_else(|| panic!("expected OptSome, got {}", l1v.brief_desc()));
        assert_eq!(int_of(&inner), 10, "last (range 10 30)");
    }
    let lE = e.call_name("lE", vec![]).unwrap();
    assert!(is_opt_none(&e, &lE), "last of empty range = None");
}

// ---------------------------------------------------------------------------
// D074: compile-time static-dispatch operator overloading
// ---------------------------------------------------------------------------

/// D074: Vec Int `(+)` concatenation via the extension-block overload.
#[test]
fn d074_vec_int_plus_concatenates() {
    let src = "
v1 = vecFromList [1, 2, 3]
v2 = vecFromList [4, 5]
result = vecToList (v1 + v2)
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    let v = e.call_name("result", vec![]).unwrap();
    assert_eq!(list_ints(&e, &v), vec![1, 2, 3, 4, 5]);
}

/// D088: `vecGet` bounds-checked read — `Some x` in range, `None` out of range.
#[test]
fn d088_vec_get_bounds() {
    let src = "
v = vecPush (vecPush (vecEmpty) 10) 20
g0 = vecGet v 0
g1 = vecGet v 1
g2 = vecGet v 5
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    let g0 = e.call_name("g0", vec![]).unwrap();
    assert_eq!(int_of(&opt_inner(&e, &g0).expect("vecGet v 0 = Some")), 10);
    let g1 = e.call_name("g1", vec![]).unwrap();
    assert_eq!(int_of(&opt_inner(&e, &g1).expect("vecGet v 1 = Some")), 20);
    let g2 = e.call_name("g2", vec![]).unwrap();
    assert!(is_opt_none(&e, &g2), "vecGet out of range = None");
}

/// D074: String concatenation via `stringConcat` (`+` is Int-only).
#[test]
fn d074_string_concat() {
    let src = "s = stringConcat \"ab\" \"cd\"\n";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    let v = e.call_name("s", vec![]).unwrap();
    assert_eq!(str_of(&e, &v), "abcd");
}

/// D074: `bitNot` on Int (named replacement for the removed Int overload of `!`).
#[test]
fn d074_bit_not_int() {
    let src = "a = bitNot 0\nb = bitNot 5\n";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    assert_eq!(int_of(&e.call_name("a", vec![]).unwrap()), -1, "bitNot 0 = -1");
    assert_eq!(int_of(&e.call_name("b", vec![]).unwrap()), -6, "bitNot 5 = -6");
}

/// D074: `!` is Bool-only (logical NOT); the Int branch was removed.
#[test]
fn d074_bang_is_bool_only() {
    let src = "a = !True\nb = !False\n";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    assert_eq!(bool_of(&e.call_name("a", vec![]).unwrap()), false, "!True = False");
    assert_eq!(bool_of(&e.call_name("b", vec![]).unwrap()), true, "!False = True");
}

/// `map` on a List resolves to the top-level polymorphic prelude function
/// (only operators are overloaded; named functions are not).
#[test]
fn d074_map_on_list_uses_toplevel() {
    let src = "
double = x -> x * 2
result = map double [1, 2, 3]
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    let v = e.call_name("result", vec![]).unwrap();
    assert_eq!(list_ints(&e, &v), vec![2, 4, 6]);
}

/// `vecSlice` clamps lo/hi to [0, len] and returns the sub-range as a fresh Vec.
#[test]
fn prelude_vec_slice() {
    let src = "
v1 = vecFromList [10, 20, 30, 40, 50]
v2 = vecFromList [10, 20, 30, 40, 50]
v3 = vecFromList [10, 20, 30, 40, 50]
v4 = vecFromList [10, 20, 30, 40, 50]
v5 = vecFromList [10, 20, 30, 40, 50]
s1 = vecSlice v1 1 3
s2 = vecSlice v2 0 2
s3 = vecSlice v3 3 5
s4 = vecSlice v4 4 2
s5 = vecSlice v5 0 99
r1 = vecToList s1
r2 = vecToList s2
r3 = vecToList s3
r4 = vecToList s4
r5 = vecToList s5
";
    let loaded = loaded_with_prelude(src);
    let mut e = Engine::new(&loaded);
    let r1 = e.call_name("r1", vec![]).unwrap();
    let r2 = e.call_name("r2", vec![]).unwrap();
    let r3 = e.call_name("r3", vec![]).unwrap();
    let r4 = e.call_name("r4", vec![]).unwrap();
    let r5 = e.call_name("r5", vec![]).unwrap();
    assert_eq!(list_ints(&e, &r1), vec![20, 30]);
    assert_eq!(list_ints(&e, &r2), vec![10, 20]);
    assert_eq!(list_ints(&e, &r3), vec![40, 50]);
    assert_eq!(list_ints(&e, &r4), vec![]);
    assert_eq!(list_ints(&e, &r5), vec![10, 20, 30, 40, 50]);
}