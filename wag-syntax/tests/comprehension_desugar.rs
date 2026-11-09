//! Desugar-shape tests: verify the AST structure after comprehension desugaring.
//!
//! Uses `parse_desugared()` (parse + desugar, no lambda lift) so we can
//! inspect the `Lambda`/`Apply` nodes the desugar emits.

use wag_syntax::*;

/// Parse and extract the single function body.
fn body(src: &str) -> Expr {
    let prog = parse_desugared(src).unwrap_or_else(|e| panic!("parse failed: {e}"));
    assert_eq!(prog.items.len(), 1);
    match &prog.items[0] {
        Item::Func(f) => f.body.clone(),
        other => panic!("expected Func, got {other:?}"),
    }
}

/// Helper: assert `e` is `name arg1 arg2 ...` (flat application).
fn assert_apply(e: &Expr, name: &str, expected_args: usize) -> Vec<Expr> {
    match e {
        Expr::Apply { fun, args, .. } => {
            let Expr::Var { name: n, .. } = fun.as_ref() else {
                panic!("expected Var({name}), got {fun:?}");
            };
            assert_eq!(n, name, "function name mismatch");
            assert_eq!(args.len(), expected_args, "arg count mismatch for {name}");
            args.clone()
        }
        other => panic!("expected Apply({name}), got {other:?}"),
    }
}

/// Helper: assert `e` is `copy name` — sources are traversed via a copy so
/// the original binding survives in the enclosing body (§6.7).
fn assert_copy(e: &Expr, name: &str) {
    match e {
        Expr::Apply { fun, args, .. } => {
            let Expr::Var { name: n, .. } = fun.as_ref() else {
                panic!("expected copy({name}), got {fun:?}");
            };
            assert_eq!(n, "copy", "expected `copy`, got {n}");
            assert_eq!(args.len(), 1);
            assert!(matches!(&args[0], Expr::Var { name: v, .. } if v == name),
                "expected copy({name}), got {:?}", args[0]);
        }
        other => panic!("expected copy({name}), got {other:?}"),
    }
}

/// Helper: assert `e` is a Lambda with the given param names.
fn assert_lambda(e: &Expr, params: &[&str]) -> Expr {
    match e {
        Expr::Lambda { params: ps, body, .. } => {
            let names: Vec<&str> = ps.iter().map(|s| s.as_str()).collect();
            assert_eq!(names, params, "lambda params mismatch");
            body.as_ref().clone()
        }
        other => panic!("expected Lambda({params:?}), got {other:?}"),
    }
}

// ---- Map comprehension ----

#[test]
fn map_no_guard() {
    // [x <- xs | _ -> x * x]  →  map (\x -> x * x) (copy xs)
    let e = body("f = List Int -> List Int: xs -> [x <- xs | _ -> x * x]");
    let args = assert_apply(&e, "map", 2);
    let lam_body = assert_lambda(&args[0], &["x"]);
    // body is x * x
    assert!(matches!(lam_body, Expr::BinOp { op: BinOp::Mul, .. }));
    // source is traversed via a copy (§6.7)
    assert_copy(&args[1], "xs");
}

#[test]
fn map_with_guard() {
    // [x <- xs | x > 0 -> x]  →  map (\x -> x) (filter (\x -> x > 0) (copy xs))
    let e = body("f = List Int -> List Int: xs -> [x <- xs | x > 0 -> x]");
    let args = assert_apply(&e, "map", 2);
    // map lambda: \x -> x
    let map_body = assert_lambda(&args[0], &["x"]);
    assert!(matches!(map_body, Expr::Var { ref name, .. } if name == "x"));
    // filter layer
    let fargs = assert_apply(&args[1], "filter", 2);
    let guard_body = assert_lambda(&fargs[0], &["x"]);
    // guard is x > 0
    assert!(matches!(guard_body, Expr::BinOp { op: BinOp::Gt, .. }));
    // source is traversed via a copy (§6.7)
    assert_copy(&fargs[1], "xs");
}

#[test]
fn map_multiple_guards() {
    // [x <- xs | x > 0, x < 100 -> x * 2]
    // → map (\x -> x*2) (filter (\x -> x<100) (filter (\x -> x>0) (copy xs)))
    let e = body("f = List Int -> List Int: xs -> [x <- xs | x > 0, x < 100 -> x * 2]");
    let args = assert_apply(&e, "map", 2);
    // Guards applied in reverse: outermost = first source guard (x > 0), innermost = last (x < 100)
    let outer_f = assert_apply(&args[1], "filter", 2);
    let outer_guard = assert_lambda(&outer_f[0], &["x"]);
    assert!(matches!(outer_guard, Expr::BinOp { op: BinOp::Gt, .. })); // x > 0
    // inner filter
    let inner_f = assert_apply(&outer_f[1], "filter", 2);
    let inner_guard = assert_lambda(&inner_f[0], &["x"]);
    assert!(matches!(inner_guard, Expr::BinOp { op: BinOp::Lt, .. })); // x < 100
    // source is traversed via a copy (§6.7)
    assert_copy(&inner_f[1], "xs");
}

// ---- Fold comprehension ----

#[test]
fn fold_no_guard() {
    // [x <- xs | acc = 0 | _ -> acc + x]  →  fold (\x acc -> acc + x) 0 (copy xs)
    let e = body("f = List Int -> Int: xs -> [x <- xs | acc = 0 | _ -> acc + x]");
    let args = assert_apply(&e, "fold", 3);
    let lam_body = assert_lambda(&args[0], &["x", "acc"]);
    // body is acc + x
    assert!(matches!(lam_body, Expr::BinOp { op: BinOp::Add, .. }));
    // init is 0
    assert!(matches!(args[1], Expr::Int { value: 0, .. }));
    // source is traversed via a copy (§6.7)
    assert_copy(&args[2], "xs");
}

#[test]
fn fold_with_guard() {
    // [x <- xs | acc = 10 | x > 3 -> acc * x]
    // → fold (\x acc -> acc * x) 10 (filter (\x -> x > 3) (copy xs))
    let e = body("f = List Int -> Int: xs -> [x <- xs | acc = 10 | x > 3 -> acc * x]");
    let args = assert_apply(&e, "fold", 3);
    assert_lambda(&args[0], &["x", "acc"]);
    assert!(matches!(args[1], Expr::Int { value: 10, .. }));
    // source is filtered
    let fargs = assert_apply(&args[2], "filter", 2);
    assert_lambda(&fargs[0], &["x"]);
    // source is traversed via a copy (§6.7)
    assert_copy(&fargs[1], "xs");
}

// ---- Zip comprehension ----

#[test]
fn zip_two() {
    // [x, y <- xs, ys | _ -> x + y]
    // → map (\__zip -> match __zip { Pair x y -> x + y }) (zip (copy xs) (copy ys))
    let e = body("f = List Int, List Int -> List Int: xs, ys -> [x, y <- xs, ys | _ -> x + y]");
    let args = assert_apply(&e, "map", 2);
    // The lambda destructures a Pair
    let lam_body = assert_lambda(&args[0], &["__zip"]);
    assert!(matches!(lam_body, Expr::Match { .. }));
    // The source is zip (copy xs) (copy ys) — each binding survives (§6.7)
    let zargs = assert_apply(&args[1], "zip", 2);
    assert_copy(&zargs[0], "xs");
    assert_copy(&zargs[1], "ys");
}

#[test]
fn zip_three() {
    // [a, b, c <- xs, ys, zs | _ -> a + b + c]
    // → map (lambda) (zip (copy xs) (zip (copy ys) (copy zs)))
    let e = body("f = List Int, List Int, List Int -> List Int: xs, ys, zs -> [a, b, c <- xs, ys, zs | _ -> a + b + c]");
    let args = assert_apply(&e, "map", 2);
    // nested zip: zip (copy xs) (zip (copy ys) (copy zs))
    let zargs = assert_apply(&args[1], "zip", 2);
    assert_copy(&zargs[0], "xs");
    let inner_zip = assert_apply(&zargs[1], "zip", 2);
    assert_copy(&inner_zip[0], "ys");
    assert_copy(&inner_zip[1], "zs");
}

#[test]
fn zip_with_guard() {
    // [x, y <- xs, ys | x > 0 -> x + y]
    // → map (lambda) (filter (lambda) (zip (copy xs) (copy ys)))
    let e = body("f = List Int, List Int -> List Int: xs, ys -> [x, y <- xs, ys | x > 0 -> x + y]");
    let args = assert_apply(&e, "map", 2);
    // filter layer
    let fargs = assert_apply(&args[1], "filter", 2);
    // filter lambda destructures Pair
    assert_lambda(&fargs[0], &["__zip"]);
    // source is zip (copy xs) (copy ys)
    let zargs = assert_apply(&fargs[1], "zip", 2);
    assert_copy(&zargs[0], "xs");
    assert_copy(&zargs[1], "ys");
}

// ---- Cartesian comprehension ----

#[test]
fn cartesian_two_generators() {
    // [x <- xs, y <- ys | _ -> x + y]
    // → flatMap (\x -> map (\y -> x + y) (copy ys)) (copy xs)
    let e = body("f = List Int, List Int -> List Int: xs, ys -> [x <- xs, y <- ys | _ -> x + y]");
    let args = assert_apply(&e, "flatMap", 2);
    let lam_body = assert_lambda(&args[0], &["x"]);
    // inner is map (\y -> x + y) (copy ys)
    let inner_args = assert_apply(&lam_body, "map", 2);
    let inner_lam = assert_lambda(&inner_args[0], &["y"]);
    assert!(matches!(inner_lam, Expr::BinOp { op: BinOp::Add, .. }));
    // inner source is traversed via a copy (§6.7)
    assert_copy(&inner_args[1], "ys");
    // outer source
    assert_copy(&args[1], "xs");
}

#[test]
fn cartesian_with_guard() {
    // [x <- xs, y <- ys | x + y > 10 -> x * y]
    // → flatMap (\x -> map (\y -> x*y) (filter (\y -> x+y>10) (copy ys))) (copy xs)
    let e = body("f = List Int, List Int -> List Int: xs, ys -> [x <- xs, y <- ys | x + y > 10 -> x * y]");
    let args = assert_apply(&e, "flatMap", 2);
    let lam_body = assert_lambda(&args[0], &["x"]);
    // inner: map (\y -> x*y) (filter (\y -> x+y>10) ys)
    let inner_args = assert_apply(&lam_body, "map", 2);
    assert_lambda(&inner_args[0], &["y"]);
    let fargs = assert_apply(&inner_args[1], "filter", 2);
    assert_lambda(&fargs[0], &["y"]);
    // inner source is traversed via a copy (§6.7)
    assert_copy(&fargs[1], "ys");
}

// ---- Edge cases ----

#[test]
fn nested_comprehension() {
    // f xs = [x <- xs | _ -> [y <- ys | _ -> x + y]]
    // outer: map (\x -> inner) (copy xs)
    // inner: map (\y -> x + y) (copy ys)
    let e = body("f = List Int, List Int -> List (List Int): xs, ys -> [x <- xs | _ -> [y <- ys | _ -> x + y]]");
    let args = assert_apply(&e, "map", 2);
    let lam_body = assert_lambda(&args[0], &["x"]);
    // inner comprehension desugared to map
    let inner_args = assert_apply(&lam_body, "map", 2);
    assert_lambda(&inner_args[0], &["y"]);
    assert_copy(&inner_args[1], "ys");
    // outer source is traversed via a copy (§6.7)
    assert_copy(&args[1], "xs");
}

#[test]
fn comprehension_in_let() {
    // f xs = ys = [x <- xs | _ -> x * 2] ; ys
    let e = body("f = List Int -> List Int: xs ->\n  ys = [x <- xs | _ -> x * x]\n  ys");
    // Should be Let { value: map(...), body: ys }
    assert!(matches!(e, Expr::Bind { .. }));
}

#[test]
fn comprehension_result_not_var() {
    // [x <- xs | _ -> 1]  →  map (\x -> 1) (copy xs)
    let e = body("f = List Int -> List Int: xs -> [x <- xs | _ -> 1]");
    let args = assert_apply(&e, "map", 2);
    let lam_body = assert_lambda(&args[0], &["x"]);
    assert!(matches!(lam_body, Expr::Int { value: 1, .. }));
}

// ---- Error cases (rejected at parse time) ----

#[test]
fn zip_arity_mismatch_rejected() {
    let src = "f = List Int, List Int -> List Int: xs, ys -> [x, y <- xs | _ -> x + y]";
    assert!(parse_desugared(src).is_err());
}

#[test]
fn multi_acc_rejected() {
    let src = "f = List Int -> Int: xs -> [x <- xs | a = 0, b = 0 | _ -> a + b]";
    assert!(parse_desugared(src).is_err());
}

#[test]
fn cartesian_fold_rejected() {
    let src = "f: List Int, List Int -> Int: xs ys = [x <- xs, y <- ys | acc = 0 | _ -> acc + x + y]";
    assert!(parse_desugared(src).is_err());
}
