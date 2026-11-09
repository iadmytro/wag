//! Scratch: dump desugared method bodies (temporary).
use wag_check::{check, CheckedProgram};
use wag_types::check as type_check;

fn check_only(src: &str) -> Result<CheckedProgram, String> {
    let mut p = wag_syntax::parse(src).map_err(|e| format!("parse failed: {e}"))?;
    // D060: early desugar of method-body bare names (pipeline stage).
    wag_syntax::desugar_method_bare_names(&mut p);
    let t = type_check(&p).map_err(|e| e.message)?;
    check(&p, t).map_err(|e| e.to_string())
}

#[test]
fn dump() {
    let src = "\
Option = Some val T | None
LNode = LEnd | LNodeC val Int next (Ref LNode)
List = LH head (Ref LNode) prev (Ref LNode) cur (Ref LNode) len Int
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
main = 1
";
    let c = check_only(src).unwrap();
    for item in &c.program().items {
        if let wag_syntax::Item::Method(m) = item {
            println!("=== method {}.{} ===", m.type_name, m.name);
            println!("{}", wag_syntax::pretty_expr(&m.body));
        }
    }
}
