//! Fuzz target: full pipeline (parse → type-check) must never panic.
//!
//! Run with: cargo +nightly fuzz run fuzz_pipeline

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &str| {
    let program = match wag_syntax::parse(data) {
        Ok(p) => p,
        Err(_) => return,
    };
    // Type-checking must not panic regardless of input.
    let _ = wag_types::check(&program);
});
