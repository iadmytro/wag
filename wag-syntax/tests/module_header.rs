//! Module header: export/import block parsing.

use wag_syntax::*;

fn parse_ok(src: &str) -> Program {
    parse(src).unwrap_or_else(|e| panic!("parse failed: {e}\nsource:\n{src}"))
}

fn parse_err(src: &str) {
    assert!(
        parse(src).is_err(),
        "expected parse error, got Ok\nsource:\n{src}"
    );
}

#[test]
fn export_block_named_items() {
    let src = "export
  Pair
  @server
  len
Pair = Int
@server = state -> 0
len = x -> x
";
    let p = parse_ok(src);
    let exp = p.exports.as_ref().expect("export block");
    assert_eq!(exp.items.len(), 3);
    assert_eq!(exp.items[0].kind(), Some(ItemKind::Data));
    assert_eq!(exp.items[1].kind(), Some(ItemKind::Actor));
    assert_eq!(exp.items[2].kind(), Some(ItemKind::Function));
}

#[test]
fn export_block_all() {
    let src = "export
  ..
main = 1
";
    let p = parse_ok(src);
    let exp = p.exports.as_ref().expect("export block");
    assert_eq!(exp.items.len(), 1);
    assert!(matches!(exp.items[0], ExportItem::All { .. }));
}

#[test]
fn export_block_alias() {
    let src = "export
  mylen <- len
len = x -> x
";
    let p = parse_ok(src);
    let exp = p.exports.as_ref().expect("export block");
    assert_eq!(exp.items.len(), 1);
    match &exp.items[0] {
        ExportItem::Named { name, alias, .. } => {
            assert_eq!(name, "len");
            assert_eq!(alias.as_deref(), Some("mylen"));
        }
        _ => panic!("expected Named"),
    }
}

#[test]
fn export_block_with_signature() {
    let src = "export
  len Int -> Int
len = x -> x
";
    let p = parse_ok(src);
    let exp = p.exports.as_ref().expect("export block");
    assert_eq!(exp.items.len(), 1);
    match &exp.items[0] {
        ExportItem::Named { name, signature, .. } => {
            assert_eq!(name, "len");
            assert!(signature.is_some(), "expected signature");
        }
        _ => panic!("expected Named"),
    }
}

#[test]
fn export_order_enforced() {
    let src = "export
  len
  Pair
Pair = Int
len = x -> x
";
    parse_err(src);
}

#[test]
fn export_all_must_be_sole() {
    let src = "export
  ..
  Pair
Pair = Int
";
    parse_err(src);
}

#[test]
fn import_block_groups() {
    let src = "import
  list_math
    Pair
    @server
  string_ops
    split
main = 1
";
    let p = parse_ok(src);
    let imp = p.imports.as_ref().expect("import block");
    assert_eq!(imp.groups.len(), 2);
    assert_eq!(imp.groups[0].module, "list_math");
    assert_eq!(imp.groups[0].items.len(), 2);
    assert_eq!(imp.groups[1].module, "string_ops");
    assert_eq!(imp.groups[1].items.len(), 1);
}

#[test]
fn import_block_alias() {
    let src = "import
  list_math
    mylen <- len
main = 1
";
    let p = parse_ok(src);
    let imp = p.imports.as_ref().expect("import block");
    let item = &imp.groups[0].items[0];
    match item {
        ImportItem::Named { name, alias, .. } => {
            assert_eq!(name, "len");
            assert_eq!(alias.as_deref(), Some("mylen"));
        }
        _ => panic!("expected Named"),
    }
}

#[test]
fn import_block_all() {
    let src = "import
  json
    ..
main = 1
";
    let p = parse_ok(src);
    let imp = p.imports.as_ref().expect("import block");
    assert_eq!(imp.groups.len(), 1);
    assert_eq!(imp.groups[0].items.len(), 1);
    assert!(matches!(imp.groups[0].items[0], ImportItem::All { .. }));
}

#[test]
fn import_order_enforced() {
    let src = "import
  list_math
    len
    Pair
main = 1
";
    parse_err(src);
}

#[test]
fn import_before_export_is_error() {
    let src = "import
  list_math
    Pair
export
  Pair
main = 1
";
    parse_err(src);
}

#[test]
fn export_then_import() {
    let src = "export
  Pair
import
  list_math
    Pair
main = 1
";
    let p = parse_ok(src);
    assert!(p.exports.is_some());
    assert!(p.imports.is_some());
}

#[test]
fn no_header() {
    let src = "main = 1
";
    let p = parse_ok(src);
    assert!(p.exports.is_none());
    assert!(p.imports.is_none());
}

#[test]
fn actor_alias_same_kind() {
    let src = "export
  @srv <- @server
@server = state -> 0
";
    let p = parse_ok(src);
    let exp = p.exports.as_ref().expect("export block");
    match &exp.items[0] {
        ExportItem::Named { name, alias, is_actor, .. } => {
            assert!(is_actor);
            assert_eq!(name, "server");
            assert_eq!(alias.as_deref(), Some("srv"));
        }
        _ => panic!("expected Named"),
    }
}

#[test]
fn actor_data_alias_mismatch_is_error() {
    let src = "export
  @srv <- Pair
Pair = Int
";
    parse_err(src);
}
