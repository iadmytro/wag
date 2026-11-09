//! Ref feature — linearity over method bodies (D056/D057): `self` is
//! linear in move-and-return methods (exactly once per execution path), never
//! consumable in read-only methods; links are non-values.
//! See spec §4.3.5, §6.9.
//!
//! Uses `wag_check::check_pure` so the prelude is not merged — the worked
//! example uses the same type names as the prelude's ref-list section by design.

use wag_check::check_pure;

fn assert_ok(src: &str) {
    check_pure(src).unwrap_or_else(|e| panic!("expected ok, got {e}\nsource:\n{src}"));
}

fn assert_linearity_err(src: &str, needle: &str) {
    match check_pure(src).map_err(|e| e.to_string()) {
        Ok(_) => panic!("expected rule 10 error, got OK\nsource:\n{src}"),
        Err(msg) => assert!(
            msg.contains("rule 10") && msg.contains(needle),
            "expected diagnostic mentioning `rule 10` + `{needle}`, got: {msg}\nsource:\n{src}"
        ),
    }
}

const PRE: &str = "\
LNode = LEnd | LNodeC val Int next (Ref LNode)
List = LH head (Ref LNode) prev (Ref LNode) cur (Ref LNode) len Int
Option = Some val T | None
";

// ── Positive ───────────────────────────────────────────────────────────────

#[test]
fn worked_example_passes_check() {
    // Full D057 removeLast example through the whole pipeline.
    assert_ok(&format!(
        "{PRE}\nList.next = Option List: ->\n  match cur\n    LEnd     -> None\n    LNodeC _ n ->\n      match n\n        LEnd       -> None\n        LNodeC _ _ ->\n          prev <- cur\n          cur <- n\n          Some self\n\nList.removeAt = (Option Int, List): ->\n  match cur\n    LEnd     -> (None, self)\n    LNodeC v rest ->\n      spliced = match prev\n        LEnd ->\n          head <- rest\n        _    ->\n          prev.next <- rest\n      len <- len - 1\n      (Some v, self)\n\nlistAtTail = List -> List: d ->\n  c = copy d\n  n = c.next\n  match n\n    None   -> d\n    Some e -> listAtTail e\n\nlistRemoveLast = List -> (Option Int, List): c ->\n  if c.len == 0 -> (None, c)\n  _    -> (listAtTail c).removeAt\nmain = 1\n"
    ));
}

#[test]
fn self_once_per_arm_is_ok() {
    // Each execution path re-emits the handle exactly once — per-arm uses are
    // independent (match/if arms clone linearity state).
    assert_ok(&format!(
        "{PRE}\nList.bumpIf = Bool -> List: b ->\n  if b -> self\n  _    ->\n    len <- len + 1\n    self\nmain = 1\n"
    ));
}

#[test]
fn read_only_method_without_self_is_ok() {
    assert_ok(&format!("{PRE}\nList.peek = Int: -> len\nmain = 1\n"));
}

// ── Negative (rule 10) ─────────────────────────────────────────────────────

#[test]
fn self_double_use_same_path_rejected() {
    // Two `self` uses on one execution path — the second is use-after-move.
    assert_linearity_err(
        &format!("{PRE}\nList.bad = List: ->\n  x = self\n  y = self\n  x\nmain = 1\n"),
        "use after move of `self`",
    );
}

#[test]
fn self_in_read_only_method_rejected() {
    // Result does not mention the receiver handle → read-only; using `self`
    // would consume a value that is never re-emitted.
    assert_linearity_err(
        &format!("{PRE}\nList.bad = Int: ->\n  x = self\n  len\nmain = 1\n"),
        "does not return the receiver handle",
    );
}

#[test]
fn consuming_method_without_self_rejected() {
    // Result mentions the receiver handle → move-and-return; the body must
    // re-emit `self` (constructing a fresh handle is not how regions work).
    assert_linearity_err(
        &format!("{PRE}\nList.bad = List: -> LH head prev cur len\nmain = 1\n"),
        "must re-emit `self`",
    );
}

// ── Call-site receiver consumption (D069) ────────────────────────────────
// Regression: the call site used to `split_first()` on `method_tys`, which
// holds declared params only — dropping the first declared param and, for
// nullary methods, skipping receiver consumption entirely.

#[test]
fn nullary_move_and_return_consumes_receiver() {
    // `bump` is nullary move-and-return → the call consumes the handle;
    // reusing it (even via field read) is rule 10.
    assert_linearity_err(
        &format!("{PRE}\nList.bump = List: -> self\nbadUse = List -> Int: h ->\n  x = h.bump\n  y = h.len\n  y\nmain = 1\n"),
        "use after move of `h`",
    );
}

#[test]
fn nullary_move_and_return_single_use_ok() {
    assert_ok(&format!(
        "{PRE}\nList.bump = List: -> self\ngoodUse = List -> List: h -> h.bump\nmain = 1\n"
    ));
}

#[test]
fn read_only_nullary_method_does_not_consume() {
    // Result does not mention the receiver → read-only; repeated calls on the
    // same handle are fine.
    assert_ok(&format!(
        "{PRE}\nList.peekLen = Int: -> len\ntwice = List -> (Int, Int): h -> (h.peekLen, h.peekLen)\nmain = 1\n"
    ));
}

#[test]
fn method_borrow_first_param_accepted() {
    // `&T` first declared param: passing a borrow must be accepted and
    // non-consuming. Before the fix the param was dropped from the check,
    // so this was a spurious rule 11.
    assert_ok(&format!(
        "{PRE}\nList.tagged = &Int -> Int: q -> len\nuseIt = List, &Int -> Int: h, b -> h.tagged b\nmain = 1\n"
    ));
}
