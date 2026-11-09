//! Ref feature — region-residency computation (D054) and the `Ref T` visibility
//! boundary + method/field disjointness (D056/D057). See spec §4.3.5, §6.9.

use wag_types::{check, Stage};

fn parse_check(src: &str) -> wag_types::TypedProgram {
    let mut p = wag_syntax::parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    // D060: early desugar of method-body bare names (pipeline stage).
    wag_syntax::desugar_method_bare_names(&mut p);
    check(&p).unwrap_or_else(|e| panic!("type error: {}\n{src}", e.message))
}

fn expect_boundary_err(src: &str, needle: &str) {
    let mut p = wag_syntax::parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\n{src}"));
    // D060: early desugar of method-body bare names (pipeline stage).
    wag_syntax::desugar_method_bare_names(&mut p);
    match check(&p) {
        Ok(t) => panic!("expected Boundary error, got OK bindings={:?}", t.bindings),
        Err(e) => {
            assert_eq!(
                e.stage,
                Stage::Boundary,
                "wrong stage: {:?} ({})",
                e.stage,
                e.message
            );
            assert!(
                e.message.contains(needle),
                "message missing `{needle}`: {}",
                e.message
            );
        }
    }
}

// ── A. Residency computation (D054) ────────────────────────────────────────

#[test]
fn residency_direct_ref_field() {
    let tp = parse_check("LNode = LEnd | LNodeC val Int next (Ref LNode)\nmain = 1\n");
    assert_eq!(tp.residency.get("LNode"), Some(&true));
}

#[test]
fn residency_transitive_through_declared_type() {
    // TA is resident (direct Ref); TB contains a TA field → transitively resident.
    let tp = parse_check("TA = TAC x (Ref TA)\nTB = TBC a TA\nmain = 1\n");
    assert_eq!(tp.residency.get("TA"), Some(&true));
    assert_eq!(tp.residency.get("TB"), Some(&true));
}

#[test]
fn residency_alias_chain() {
    // An alias whose expansion contains resident TA is itself resident; TC
    // reaches residency through the alias. (Bare-Name RHS like `X = Y` is a
    // nullary constructor, not an alias — aliases use application form.)
    let tp = parse_check("TA = TAC x (Ref TA)\nMyList = List TA\nTC = TCC m MyList\nmain = 1\n");
    assert_eq!(tp.residency.get("TA"), Some(&true));
    assert_eq!(tp.residency.get("MyList"), Some(&true));
    assert_eq!(tp.residency.get("TC"), Some(&true));
}

#[test]
fn residency_parametric_with_ref() {
    let tp = parse_check("PBox = PB inner (Ref T)\nmain = 1\n");
    assert_eq!(tp.residency.get("PBox"), Some(&true));
}

#[test]
fn residency_nested_in_builtin_container() {
    // A field of `List TA` where TA is resident: any field position counts (D054).
    let tp = parse_check("TA = TAC x (Ref TA)\nTD = TDC xs (List TA) n Int\nmain = 1\n");
    assert_eq!(tp.residency.get("TD"), Some(&true));
}

#[test]
fn residency_ref_free_recursive_stays_non_resident() {
    // §7 rule 14 permits self-referential user types. Without a `Ref` seed the
    // fixed point must converge to false — a cycle guard that assumed `true`
    // for in-progress names would wrongly mark these resident (surfaced by the
    // VM: region construction + link tokens crossing call boundaries).
    let tp = parse_check("RList = REmpty | RCons Int RList\nmain = 1\n");
    assert_eq!(tp.residency.get("RList"), Some(&false));
}

#[test]
fn residency_mutual_ref_free_stays_non_resident() {
    // Mutual recursion without `Ref` is likewise non-resident.
    let tp = parse_check("MA = MAC b MB\nMB = MBC a MA\nmain = 1\n");
    assert_eq!(tp.residency.get("MA"), Some(&false));
    assert_eq!(tp.residency.get("MB"), Some(&false));
}

#[test]
fn residency_non_resident_control() {
    let tp = parse_check("TP = TPC a Int b Bool\nTQ = TQC p TP flag Bool\nmain = 1\n");
    assert_eq!(tp.residency.get("TP"), Some(&false));
    assert_eq!(tp.residency.get("TQ"), Some(&false));
}

// ── B. Ref T visibility boundary (D056) + disjointness (D057) ──────────────

#[test]
fn boundary_fn_ref_param_rejected() {
    expect_boundary_err(
        "LNode = LEnd | LNodeC v Int next (Ref LNode)\nf = Ref LNode -> Int: x -> 0\nmain = f LEnd\n",
        "top-level function",
    );
}

#[test]
fn boundary_fn_ref_result_rejected() {
    expect_boundary_err(
        "LNode = LEnd | LNodeC v Int next (Ref LNode)\ng = Int -> Ref LNode: x -> LEnd\nmain = 1\n",
        "top-level function",
    );
}

#[test]
fn boundary_method_ref_param_rejected() {
    expect_boundary_err(
        "LNode = LEnd | LNodeC v Int next (Ref LNode)\nList = LH head (Ref LNode) len Int\n\
         List.append = Ref LNode -> List: n -> self\nmain = 1\n",
        "method signature",
    );
}

#[test]
fn boundary_method_field_disjointness() {
    expect_boundary_err(
        "TH = THC append Int len Int\nTH.append = Unit -> TH: u -> self\nmain = 1\n",
        "collides with a field",
    );
}

// ── Clean program: doc example infers, residency map populated ─────────────

#[test]
fn clean_doc_example_infers_with_residency() {
    let src = "\
LNode = LEnd | LNodeC val (Option Int) next (Ref LNode)
List = LH head (Ref LNode) tail (Ref LNode) len Int
listNew = List: -> LH LEnd LEnd 0
List.append = Int -> List: v -> self
main = listNew
";
    let tp = parse_check(src);
    assert_eq!(tp.residency.get("LNode"), Some(&true));
    assert_eq!(tp.residency.get("List"), Some(&true));
}
