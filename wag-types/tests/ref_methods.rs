//! Ref feature — method-body inference (D056/D057): bare-name receiver
//! resolution, write statements, surface dispatch, boundary rules.
//! See spec §4.3.5, §6.9.

use wag_types::{check, Ty};

fn parse_check(src: &str) -> wag_types::TypedProgram {
    let mut p = wag_syntax::parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    // D060: early desugar of method-body bare names (pipeline stage).
    wag_syntax::desugar_method_bare_names(&mut p);
    check(&p).unwrap_or_else(|e| panic!("type error: {}\n{src}", e.message))
}

fn expect_err(src: &str, needle: &str) {
    let mut p = wag_syntax::parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    // D060: early desugar of method-body bare names (pipeline stage).
    wag_syntax::desugar_method_bare_names(&mut p);
    match check(&p) {
        Ok(t) => panic!(
            "expected error containing `{needle}`, got OK ({} methods)",
            t.method_tys.len()
        ),
        Err(e) => assert!(
            e.message.contains(needle),
            "message missing `{needle}`: {}",
            e.message
        ),
    }
}

/// True when `ty` mentions type constructor `name` anywhere.
fn mentions(ty: &Ty, name: &str) -> bool {
    match ty {
        Ty::Con(n) | Ty::App(n, _) => n == name,
        Ty::Borrow(t) => mentions(t, name),
        Ty::Fun(ps, r) => ps.iter().any(|p| mentions(p, name)) || mentions(r, name),
        // A bound param or unresolved ctor var is opaque: cannot mention a specific constructor.
        Ty::CtorApp(..) | Ty::Var(_) | Ty::Bound(_) => false,
    }
}

const LIST: &str =
    "LNode = LEnd | LNodeC val Int next (Ref LNode)\nList = LH head (Ref LNode) len Int\n";

// ── A. Method schemes stored in method_tys ────────────────────────────────

#[test]
fn method_scheme_stored() {
    let tp = parse_check(&format!(
        "{LIST}List.append = Int -> List: v -> self\nmain = 1\n"
    ));
    let ty = tp
        .method_tys
        .get(&("List".into(), "append".into()))
        .unwrap();
    match ty {
        Ty::Fun(ps, r) => {
            assert_eq!(ps.len(), 1);
            assert!(matches!(&ps[0], Ty::Con(n) if n == "Int"));
            assert!(
                mentions(r.as_ref(), "List"),
                "result should be List: {r:?}"
            );
        }
        other => panic!("expected Fun, got {other:?}"),
    }
}

#[test]
fn method_nullary_scheme() {
    let tp = parse_check(&format!("{LIST}List.peekLen = Int: -> len\nmain = 1\n"));
    let ty = tp
        .method_tys
        .get(&("List".into(), "peekLen".into()))
        .unwrap();
    match ty {
        Ty::Fun(ps, r) => {
            assert!(ps.is_empty());
            assert!(matches!(r.as_ref(), Ty::Con(n) if n == "Int"));
        }
        other => panic!("expected Fun, got {other:?}"),
    }
}

// ── B. Worked example (D057 removeLast) ───────────────────────────────────

#[test]
fn worked_example_remove_last() {
    let src = r#"
Option = Some val T | None

LNode = LNodeC val Int next (Ref LNode)
  | LEnd
List = LH head (Ref LNode) prev (Ref LNode) cur (Ref LNode) len Int

List.next = Option List: ->
  match cur
    LEnd     -> None
    LNodeC _ n ->
      match n
        LEnd       -> None
        LNodeC _ _ ->
          prev <- cur
          cur <- n
          Some self

List.removeAt = (Option Int, List): ->
  match cur
    LEnd     -> (None, self)
    LNodeC v rest ->
      spliced = match prev
        LEnd ->
          head <- rest
        _    ->
          prev.next <- rest
      len <- len - 1
      (Some v, self)

listAtTail = List -> List: d ->
  match d.next
    None   -> d
    Some e -> listAtTail e

listRemoveLast = List -> (Option Int, List): c ->
  if c.len == 0 -> (None, c)
  _    -> (listAtTail c).removeAt
"#;
    let tp = parse_check(src);
    // Method schemes.
    match tp.method_tys.get(&("List".into(), "next".into())).unwrap() {
        Ty::Fun(ps, r) => {
            assert!(ps.is_empty());
            assert!(
                matches!(r.as_ref(), Ty::App(n, a) if n == "Option" && mentions(&a[0], "List"))
            );
        }
        other => panic!("next: expected Fun, got {other:?}"),
    }
    match tp
        .method_tys
        .get(&("List".into(), "removeAt".into()))
        .unwrap()
    {
        Ty::Fun(ps, r) => {
            assert!(ps.is_empty());
            // (Option Int, List) — product is App("Pair", …).
            let r = match r.as_ref() {
                Ty::App(n, a) if n == "Pair" && a.len() == 2 => a.clone(),
                other => panic!("expected Pair result, got {other:?}"),
            };
            assert!(mentions(&r[0], "Option"));
            assert!(mentions(&r[1], "List"));
        }
        other => panic!("removeAt: expected Fun, got {other:?}"),
    }
    // Top-level loop functions threaded the handle.
    assert!(tp.bindings.contains_key(&("listAtTail".into(), 1)));
    assert!(tp.bindings.contains_key(&("listRemoveLast".into(), 1)));
}

// ── C. Surface dispatch + bare names ──────────────────────────────────────

#[test]
fn surface_method_call_on_expression_receiver() {
    let src = format!(
        "{LIST}\nList.bump = List: ->\n  len <- len + 1\n  self\n\ng = List -> Int: h -> (h.bump).len\nmain = 1\n"
    );
    let tp = parse_check(&src);
    match tp.bindings.get(&("g".into(), 1)).unwrap() {
        Ty::Fun(ps, r) => {
            assert!(mentions(&ps[0], "List"));
            assert!(matches!(r.as_ref(), Ty::Con(n) if n == "Int"));
        }
        other => panic!("expected Fun, got {other:?}"),
    }
}

#[test]
fn bare_name_resolves_receiver_field() {
    // `len` in the body is a bare atom naming the receiver field (D057).
    let src = format!("{LIST}List.peekLen = Int: -> len\nmain = 1\n");
    parse_check(&src);
}

#[test]
fn write_stmt_updates_field_type() {
    // `len <- len + 2` then a data read of the same field must still type.
    let src = format!("{LIST}List.bump2 = Int: ->\n  len <- len + 2\n  len\nmain = 1\n");
    parse_check(&src);
}

// ── D. Boundary / v1 restrictions (negative) ─────────────────────────────

#[test]
fn link_result_rejected() {
    // No signature: inferred result is the raw link type → boundary rule.
    let src = format!("{LIST}List.peekLink = -> head\nmain = 1\n");
    expect_err(&src, "link value may not flow out of a method body");
}

#[test]
fn method_to_method_call_rejected() {
    let src = format!("{LIST}\nList.a = List: -> self\nList.b = List: -> self.a\nmain = 1\n");
    expect_err(&src, "method-to-method calls are not allowed in v1");
}

#[test]
fn param_count_mismatch_rejected() {
    let src = format!("{LIST}List.append = Int -> List: a, b -> self\nmain = 1\n");
    expect_err(&src, "params but signature has");
}

#[test]
fn polymorphic_sig_rejected() {
    // Single uppercase letters are type variables; method sigs must be mono.
    let src = format!("{LIST}List.id = T -> T: x -> self\nmain = 1\n");
    expect_err(
        &src,
        "polymorphic method signatures are not supported in v1",
    );
}
