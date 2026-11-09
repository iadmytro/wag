//! Exhaustiveness check tests (§3.6).

use wag_check::{check_with_prelude, CheckError, Rule};

fn assert_ok(src: &str) {
    check_with_prelude(src).unwrap_or_else(|e| panic!("expected Ok, got: {e}"));
}

fn assert_err_exhaustive(src: &str) {
    let err = check_with_prelude(src).unwrap_err();
    let check_err = err
        .downcast_ref::<CheckError>()
        .expect("expected CheckError");
    assert_eq!(
        check_err.rule,
        Rule::Exhaustiveness,
        "expected Rule::Exhaustiveness, got: {:?}",
        check_err.rule
    );
}

#[test]
fn nested_pattern_match_list_depth() {
    // Multiple arms for the same constructor (N) with different nested patterns.
    assert_ok(
        "Node = Leaf | Branch Int Node\nlistDepth = xs -> match xs\n  Leaf -> 0\n  Branch h Leaf -> 1\n  Branch h (Branch h2 t) -> 2\nres = listDepth (Branch 1 (Branch 2 Leaf))\n",
    );
}

#[test]
fn nested_pattern_match_non_exhaustive() {
    // N arm only covers E tail — missing N-with-N tail.
    assert_err_exhaustive(
        "Node = Leaf | Branch Int Node\nbadDepth = match (Branch 1 (Branch 2 Leaf))\n  Leaf -> 0\n  Branch h Leaf -> 1\nres = badDepth\n",
    );
}

#[test]
fn nested_pattern_match_with_catchall() {
    // Catch-all arm makes the match exhaustive even with nested patterns.
    assert_ok(
        "Node = Leaf | Branch Int Node\nlistLen2 = xs -> match xs\n  Leaf -> 0\n  Branch h Leaf -> 1\n  Branch h t -> 99\nres = listLen2 ((Branch 1 Leaf))\n",
    );
}

#[test]
fn option_type_exhaustive() {
    // Option type has two constructors — both must be covered.
    assert_ok(
        "f = x -> match x\n  Some v -> v\n  None -> 0\nres = f (Some 42)\n",
    );
}

#[test]
fn option_type_missing_none() {
    // Missing None constructor.
    assert_err_exhaustive(
        "bad = x -> match x\n  Some v -> v\nres = bad (Some 1)\n",
    );
}

#[test]
fn missing_ctor_rejected() {
    // Missing a constructor of the matched type.
    assert_err_exhaustive(
        "Node = Leaf | Branch Int Node\nbad = match ((Branch 1 Leaf))\n  Leaf -> 0\nres = bad\n",
    );
}

#[test]
fn catchall_var_pattern() {
    // A variable pattern is a catch-all — match is exhaustive.
    assert_ok(
        "f = xs -> match xs\n  x -> 0\nres = f 42\n",
    );
}

#[test]
fn nested_field_coverage() {
    // N arms must cover all T cases in the tail field.
    // Leaf + Branch h Leaf + Branch h (Branch h2 t) covers all Node tails.
    assert_ok(
        "Node = Leaf | Branch Int Node\ndepth = xs -> match xs\n  Leaf -> 0\n  Branch h Leaf -> 1\n  Branch h (Branch h2 t) -> 2\nres = depth (Branch 1 (Branch 2 (Branch 3 Leaf)))\n",
    );
}

#[test]
fn nested_field_missing_ctor() {
    // Branch h Leaf only covers Leaf tail — missing Branch tail.
    assert_err_exhaustive(
        "Node = Leaf | Branch Int Node\nbad = xs -> match xs\n  Leaf -> 0\n  Branch h Leaf -> 1\nres = bad Leaf\n",
    );
}

#[test]
fn outbox_arm_required() {
    // Actor with @std property must handle @std.EndOfInput.
    // check_with_prelude auto-merges the prelude (which declares StdReply ctors).
    let src = "MainMsg = Start (List (List Int))\n@main = @std: Option Int, MainMsg -> Option Int: st, msg -> match msg\n  Start argv -> st\n  @std.Line chars -> st\n";
    assert_err_exhaustive(src);
}

#[test]
fn outbox_arm_satisfied() {
    // All @std outbox arms present (Line, EndOfInput).
    let src = "MainMsg = Start (List (List Int))\n@main = @std: Option Int, MainMsg -> Option Int: st, msg -> match msg\n  Start argv -> st\n  @std.Line chars -> st\n  @std.EndOfInput -> st\n";
    assert_ok(src);
}

#[test]
fn outbox_arm_file_satisfied() {
    // All @file outbox arms present (FileContent, FileError) — D082.
    let src = "MainMsg = Start (List (List Int))\n@main = @file: Option Int, MainMsg -> Option Int: st, msg -> match msg\n  Start argv -> st\n  @file.FileContent data -> st\n  @file.FileError err -> st\n";
    assert_ok(src);
}

#[test]
fn outbox_arm_file_required() {
    // Actor with @file property must handle @file.FileError.
    let src = "MainMsg = Start (List (List Int))\n@main = @file: Option Int, MainMsg -> Option Int: st, msg -> match msg\n  Start argv -> st\n  @file.FileContent data -> st\n";
    assert_err_exhaustive(src);
}
