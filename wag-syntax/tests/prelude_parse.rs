use wag_syntax::*;

#[test]
fn prelude_parses() {
    let src = std::fs::read_to_string("../prelude/prelude.wag").unwrap();
    match parse(&src) {
        Ok(p) => println!("OK: {} items", p.items.len()),
        Err(e) => panic!("PRELUDE PARSE FAILED: {e}\n"),
    }
}
