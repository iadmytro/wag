//! Integration tests for constrained datatypes (D090).
//!
//! Covers: user-defined constrained datatypes, Char/String as prelude
//! constrained datatypes, Data1 representation, `==` for Char/String,
//! match patterns, and the Some/None derivation from the decider body.

use wag_interp::{load_checked, Engine, Value};

/// Compile source with the prelude merged in, load into a `LoadedProgram`.
fn load(src: &str) -> wag_interp::LoadedProgram {
    let checked = wag_check::check_with_prelude(src)
        .unwrap_or_else(|e| panic!("check failed: {e}\n---\n{src}\n---"));
    load_checked(checked.program(), checked.typed(), checked.cell_reuse())
}

/// Build a `None` value.
fn none(prog: &wag_interp::LoadedProgram) -> Value {
    Value::Data(prog.opt_none_id(), vec![])
}

/// Build a `Some(v)` value (Data1 for single-field).
fn some(prog: &wag_interp::LoadedProgram, v: Value) -> Value {
    Value::Data1(prog.opt_some_id(), Box::new(v))
}

/// Deref an Option-typed value to its payload.
fn opt_inner(prog: &wag_interp::LoadedProgram, v: &Value) -> Option<Value> {
    let some_id = prog.opt_some_id();
    match v {
        Value::Data(ctor, fields) if *ctor == some_id && !fields.is_empty() => {
            Some(fields[0].clone())
        }
        Value::Data1(ctor, f) if *ctor == some_id => Some(f.as_ref().clone()),
        _ => None,
    }
}

fn assert_deep(e: &mut Engine, name: &str, expected: Value) {
    let v = e.call_name(name, vec![]).unwrap();
    assert!(
        v == expected,
        "{name}: got {}, expected {}",
        v.brief_desc(),
        expected.brief_desc()
    );
}

// ============================================================================
// User-defined constrained datatype
// ============================================================================

const POSITIVE_SRC: &str = r#"
Positive = Int -> Option Positive: n -> n > 0

pos5 = Positive 5
posNeg = Positive (-1)
posZero = Positive 0
"#;

#[test]
fn user_constrained_datatype_valid() {
    let prog = load(POSITIVE_SRC);
    let mut e = Engine::new(&prog);
    let v = e.call_name("pos5", vec![]).unwrap();
    // Should be Some(Data1(Positive, 5))
    let inner = opt_inner(&prog, &v).expect("pos5 should be Some");
    let pos_id = prog.ctor_by_name.get("Positive").copied().unwrap();
    assert!(
        matches!(inner, Value::Data1(c, ref f) if c == pos_id && **f == Value::Int(5)),
        "pos5 inner: {}",
        inner.brief_desc()
    );
}

#[test]
fn user_constrained_datatype_invalid() {
    let prog = load(POSITIVE_SRC);
    let mut e = Engine::new(&prog);
    // n = -1 → decider false → None
    assert_deep(&mut e, "posNeg", none(&prog));
    // n = 0 → decider false (0 > 0 is false) → None
    assert_deep(&mut e, "posZero", none(&prog));
}

// ============================================================================
// Char: prelude constrained datatype
// ============================================================================

const CHAR_SRC: &str = r#"
cNul = codepointToChar 0
cH = codepointToChar 104
cMax = codepointToChar 0x10FFFF
cSurrogate = codepointToChar 0xD800
cSurrogateEnd = codepointToChar 0xDFFF
cTooBig = codepointToChar 0x110000
cNeg = codepointToChar (-1)
"#;

#[test]
fn char_valid_codepoints() {
    let prog = load(CHAR_SRC);
    let mut e = Engine::new(&prog);
    // 0 (NUL) is a valid scalar value.
    let v = e.call_name("cNul", vec![]).unwrap();
    let inner = opt_inner(&prog, &v).expect("cNul should be Some");
    assert_eq!(inner, prog.make_char(0));

    // 104 = 'h'
    let v = e.call_name("cH", vec![]).unwrap();
    let inner = opt_inner(&prog, &v).expect("cH should be Some");
    assert_eq!(inner, prog.make_char(104));

    // 0x10FFFF is the max valid codepoint.
    let v = e.call_name("cMax", vec![]).unwrap();
    let inner = opt_inner(&prog, &v).expect("cMax should be Some");
    assert_eq!(inner, prog.make_char(0x10FFFF));
}

#[test]
fn char_invalid_codepoints() {
    let prog = load(CHAR_SRC);
    let mut e = Engine::new(&prog);
    // Surrogates (0xD800..=0xDFFF) are not valid scalar values.
    assert_deep(&mut e, "cSurrogate", none(&prog));
    assert_deep(&mut e, "cSurrogateEnd", none(&prog));
    // Out of range.
    assert_deep(&mut e, "cTooBig", none(&prog));
    assert_deep(&mut e, "cNeg", none(&prog));
}

// ============================================================================
// String: prelude constrained datatype
// ============================================================================

const STRING_SRC: &str = r#"
sHello = "hello"
sEmpty = ""
sUtf8 = "hé"
"#;

#[test]
fn string_literal_is_valid() {
    let prog = load(STRING_SRC);
    let mut e = Engine::new(&prog);
    // String literals go through the trusted path (make_string), not the decider.
    let v = e.call_name("sHello", vec![]).unwrap();
    assert_eq!(prog.value_to_string(&v), Some("hello".to_string()));

    let v = e.call_name("sEmpty", vec![]).unwrap();
    assert_eq!(prog.value_to_string(&v), Some(String::new()));

    let v = e.call_name("sUtf8", vec![]).unwrap();
    assert_eq!(prog.value_to_string(&v), Some("hé".to_string()));
}

// ============================================================================
// Char == (Data1 structural equality)
// ============================================================================

const CHAR_EQ_SRC: &str = r#"
a = codepointToChar 104
b = codepointToChar 104
c = codepointToChar 233

aEqB = do
  x <- a
  y <- b
  return (x == y)

aEqC = do
  x <- a
  y <- c
  return (x == y)
"#;

#[test]
fn char_eq_same_codepoint() {
    let prog = load(CHAR_EQ_SRC);
    let mut e = Engine::new(&prog);
    assert_deep(&mut e, "aEqB", some(&prog, Value::Bool(true)));
}

#[test]
fn char_eq_different_codepoint() {
    let prog = load(CHAR_EQ_SRC);
    let mut e = Engine::new(&prog);
    assert_deep(&mut e, "aEqC", some(&prog, Value::Bool(false)));
}

// ============================================================================
// String == (Data1 structural equality)
// ============================================================================

const STRING_EQ_SRC: &str = r#"
s1 = "hello"
s2 = "hello"
s3 = "world"

s1EqS2 = s1 == s2
s1EqS3 = s1 == s3
"#;

#[test]
fn string_eq_equal() {
    let prog = load(STRING_EQ_SRC);
    let mut e = Engine::new(&prog);
    assert_deep(&mut e, "s1EqS2", Value::Bool(true));
}

#[test]
fn string_eq_unequal() {
    let prog = load(STRING_EQ_SRC);
    let mut e = Engine::new(&prog);
    assert_deep(&mut e, "s1EqS3", Value::Bool(false));
}

// ============================================================================
// Char in match patterns
// ============================================================================

const CHAR_MATCH_SRC: &str = r#"
c = codepointToChar 104

result = match c
  None -> -1
  Some ch -> charToCodepoint ch
"#;

#[test]
fn char_match_some() {
    let prog = load(CHAR_MATCH_SRC);
    let mut e = Engine::new(&prog);
    assert_deep(&mut e, "result", Value::Int(104));
}

const CHAR_MATCH_NONE_SRC: &str = r#"
c = codepointToChar 0xD800

result = match c
  None -> -1
  Some ch -> charToCodepoint ch
"#;

#[test]
fn char_match_none() {
    let prog = load(CHAR_MATCH_NONE_SRC);
    let mut e = Engine::new(&prog);
    assert_deep(&mut e, "result", Value::Int(-1));
}

// ============================================================================
// String in match patterns (literal)
// ============================================================================

const STRING_MATCH_SRC: &str = r#"
s = "hello"

result = match s
  "hello" -> 1
  "world" -> 2
  _ -> 0
"#;

#[test]
fn string_match_literal_hit() {
    let prog = load(STRING_MATCH_SRC);
    let mut e = Engine::new(&prog);
    assert_deep(&mut e, "result", Value::Int(1));
}

const STRING_MATCH_MISS_SRC: &str = r#"
s = "goodbye"

result = match s
  "hello" -> 1
  "world" -> 2
  _ -> 0
"#;

#[test]
fn string_match_literal_miss() {
    let prog = load(STRING_MATCH_MISS_SRC);
    let mut e = Engine::new(&prog);
    assert_deep(&mut e, "result", Value::Int(0));
}

// ============================================================================
// Data1 representation: Char is Data1, String is Data1
// ============================================================================

#[test]
fn char_is_data1() {
    let prog = load("c = codepointToChar 104");
    let mut e = Engine::new(&prog);
    let v = e.call_name("c", vec![]).unwrap();
    // codepointToChar returns Option Char, so v is Some(Data1(Char, 104)).
    let inner = opt_inner(&prog, &v).expect("should be Some");
    assert!(
        matches!(inner, Value::Data1(_, _)),
        "Char inner should be Data1, got {}",
        inner.brief_desc()
    );
}

#[test]
fn string_is_data1() {
    let prog = load("s = \"hello\"");
    let mut e = Engine::new(&prog);
    let v = e.call_name("s", vec![]).unwrap();
    assert!(
        matches!(v, Value::Data1(_, _)),
        "String should be Data1, got {}",
        v.brief_desc()
    );
}

// ============================================================================
// Constrained datatype used in a computation
// ============================================================================

const COMPUTE_SRC: &str = r#"
Positive = Int -> Option Positive: n -> n > 0

p1 = Positive 42
p2 = Positive (-3)

describe = match p1
  None -> "non-positive"
  Some p -> "positive"

describeNeg = match p2
  None -> "non-positive"
  Some p -> "positive"
"#;

#[test]
fn constrained_datatype_in_match() {
    let prog = load(COMPUTE_SRC);
    let mut e = Engine::new(&prog);
    let v = e.call_name("describe", vec![]).unwrap();
    assert_eq!(prog.value_to_string(&v), Some("positive".to_string()));
    let v = e.call_name("describeNeg", vec![]).unwrap();
    assert_eq!(prog.value_to_string(&v), Some("non-positive".to_string()));
}
