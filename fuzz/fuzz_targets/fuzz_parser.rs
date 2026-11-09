//! Fuzz target: the WAG lexer/parser must never panic on arbitrary input.
//!
//! Run with: cargo +nightly fuzz run fuzz_parser

#![no_main]

libfuzzer_sys::fuzz_target!(|data: &str| {
    // Must not panic; either Ok or Err is acceptable.
    let _ = wag_syntax::parse(data);
});
