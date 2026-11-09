//! Two-layer `@std` (D043) — the prelude declares the reply ctors
//! (`Line`/`EndOfInput` on `StdReply`) and the WAG `@std` actor; the checker
//! accepts outbox arms (`@std.Line`) without injecting them (D035 retired).

fn prelude() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../prelude/prelude.wag"
    ))
    .expect("prelude.wag readable")
}

fn user() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../fixtures/io/main_std.wag"
    ))
    .expect("io/main_std.wag readable")
}

#[test]
fn prelude_and_8_15_typecheck() {
    let user_src = user();
    let stripped = user_src.strip_prefix("import\n  prelude\n    ..").map(|s| s.trim_start()).unwrap_or(&user_src);
    let src = format!("{}\n\n{}", prelude(), stripped);
    let result = wag_types::check_str(&src);
    match result {
        Ok(_) => {}
        Err(e) => panic!("expected success, got: {e:?}"),
    }
}

#[test]
fn prelude_alone_typechecks() {
    let result = wag_types::check_str(&prelude());
    match result {
        Ok(_) => {}
        Err(e) => panic!("expected success, got: {e:?}"),
    }
}

#[test]
fn qualified_outbox_arm_without_property_rejected() {
    // `@std.Line` is only legal for an actor that has the @std property.
    let src = format!(
        "{}\n\nMainMsg = Start (List (List Int))\n@main = Option Int, MainMsg -> Option (Option Int): st, msg -> match msg\n  Start argv -> return st\n  @std.Line chars -> return st\n",
        prelude()
    );
    assert!(
        wag_types::check_str(&src).is_err(),
        "qualified @std.Line arm without the @std property should be rejected"
    );
}

#[test]
fn bare_outbox_arm_without_property_rejected() {
    // Bare `Line` arm in an actor without the @std property: Line is
    // prelude-declared (StdReply) but unifies against MainMsg → fails.
    let src = format!(
        "{}\n\nMainMsg = Start (List (List Int))\n@main = Option Int, MainMsg -> Option (Option Int): st, msg -> match msg\n  Start argv -> return st\n  Line chars -> return st\n",
        prelude()
    );
    assert!(
        wag_types::check_str(&src).is_err(),
        "bare Line arm without the @std property should be rejected"
    );
}
