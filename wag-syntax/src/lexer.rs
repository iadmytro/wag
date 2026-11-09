//! Hand-written lexer for WAG (§2.2). Comments are stripped and never emitted.

use crate::token::{spanned, SpannedToken, Token};
use crate::Span;

/// Lexical error with a source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LexError {
    pub span: Span,
    pub message: String,
}

impl LexError {
    fn new(span: Span, message: impl Into<String>) -> Self {
        Self {
            span,
            message: message.into(),
        }
    }
}

/// Lex `source` into a token stream (comments and whitespace discarded).
/// Each token carries a byte span into `source`.
pub fn lex(source: &str) -> Result<Vec<SpannedToken>, LexError> {
    let bytes = source.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();
    // 1-based column, tracking newlines for layout.
    let mut line_start = 0usize;
    let mut _line = 1usize;
    // True while whitespace/comments have been seen since the last emitted
    // token (D064 negative-literal disambiguation).
    let mut ws_since_last_token = true;

    while i < bytes.len() {
        let b = bytes[i];

        // Newline
        if b == b'\n' {
            i += 1;
            line_start = i;
            _line += 1;
            ws_since_last_token = true;
            continue;
        }
        if b == b'\r' {
            i += 1;
            ws_since_last_token = true;
            continue;
        }

        // Whitespace
        if b == b' ' || b == b'\t' {
            i += 1;
            ws_since_last_token = true;
            continue;
        }

        // Line comment: --
        if b == b'-' && bytes.get(i + 1) == Some(&b'-') {
            i += 2;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            ws_since_last_token = true;
            continue;
        }

        // Block comment: {- ... -} (nestable)
        if b == b'{' && bytes.get(i + 1) == Some(&b'-') {
            let start = i;
            i += 2;
            let mut depth = 1i32;
            while i < bytes.len() && depth > 0 {
                if bytes[i] == b'{' && bytes.get(i + 1) == Some(&b'-') {
                    depth += 1;
                    i += 2;
                    continue;
                }
                if bytes[i] == b'-' && bytes.get(i + 1) == Some(&b'}') {
                    depth -= 1;
                    i += 2;
                    continue;
                }
                if bytes[i] == b'\n' {
                    line_start = i + 1;
                    _line += 1;
                }
                i += 1;
            }
            if depth != 0 {
                return Err(LexError::new(
                    Span::new(start, i),
                    "unclosed block comment",
                ));
            }
            ws_since_last_token = true;
            continue;
        }

        let start = i;
        let col = i - line_start + 1; // 1-based
        let _ = col;

        // Identifiers and keywords
        if is_ident_start(b) {
            i += 1;
            while i < bytes.len() && is_ident_continue(bytes[i]) {
                i += 1;
            }
            let text = &source[start..i];
            // Handle-name suffix: non-keyword ident immediately followed by `@`
            // (not followed by an ident-start char, which would be ident@ident).
            if !is_keyword(text) && bytes.get(i) == Some(&b'@') {
                let after_at = i + 1;
                if after_at < bytes.len() && is_ident_start(bytes[after_at]) {
                    return Err(LexError::new(
                        Span::new(start, after_at),
                        "ident@ident is not a valid token",
                    ));
                }
                let end = i + 1; // include the @ in the span
                out.push(spanned(Token::IdentAt(text.to_string()), Span::new(start, end)));
                i = end;
                ws_since_last_token = false;
                continue;
            }
            let tok = keyword_or_ident(text);
            out.push(spanned(tok, Span::new(start, i)));
            ws_since_last_token = false;
            continue;
        }

        // Numbers: decimal or 0x hex
        if b.is_ascii_digit() {
            if b == b'0' && matches!(bytes.get(i + 1).map(|c| c.to_ascii_lowercase()), Some(b'x')) {
                let hex_start = i;
                i += 2;
                let digits_start = i;
                while i < bytes.len() && bytes[i].is_ascii_hexdigit() {
                    i += 1;
                }
                if i == digits_start {
                    return Err(LexError::new(
                        Span::new(hex_start, i),
                        "hex literal needs at least one digit",
                    ));
                }
                let hex = &source[digits_start..i];
                let value = i64::from_str_radix(hex, 16).map_err(|_| {
                    LexError::new(Span::new(hex_start, i), "hex literal out of range for Int")
                })?;
                out.push(spanned(Token::IntLit(value), Span::new(hex_start, i)));
                ws_since_last_token = false;
                continue;
            }

            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let dec = &source[start..i];
            let value: i64 = dec.parse().map_err(|_| {
                LexError::new(Span::new(start, i), "decimal literal out of range for Int")
            })?;
            out.push(spanned(Token::IntLit(value), Span::new(start, i)));
            ws_since_last_token = false;
            continue;
        }

        // String literal: "..." with escapes (§2.2). Source is UTF-8; the
        // unescaped content is collected as bytes and validated once at the end.
        if b == b'"' {
            let start = i;
            i += 1; // consume opening quote
            let mut buf: Vec<u8> = Vec::new();
            let mut terminated = false;
            while i < bytes.len() {
                let c = bytes[i];
                if c == b'"' {
                    i += 1;
                    terminated = true;
                    break;
                }
                if c == b'\\' {
                    i += 1;
                    let Some(e) = bytes.get(i) else {
                        return Err(LexError::new(
                            Span::new(start, i),
                            "unterminated escape in string literal",
                        ));
                    };
                    match e {
                        b'n' => {
                            buf.push(b'\n');
                            i += 1;
                        }
                        b't' => {
                            buf.push(b'\t');
                            i += 1;
                        }
                        b'r' => {
                            buf.push(b'\r');
                            i += 1;
                        }
                        b'\\' => {
                            buf.push(b'\\');
                            i += 1;
                        }
                        b'"' => {
                            buf.push(b'"');
                            i += 1;
                        }
                        b'\'' => {
                            buf.push(b'\'');
                            i += 1;
                        }
                        b'u' => {
                            i += 1;
                            if i + 4 > bytes.len() {
                                return Err(LexError::new(
                                    Span::new(start, i),
                                    "`\\u` needs exactly 4 hex digits",
                                ));
                            }
                            let hex = &bytes[i..i + 4];
                            if !hex.iter().all(|b| b.is_ascii_hexdigit()) {
                                return Err(LexError::new(
                                    Span::new(start, i),
                                    "`\\u` needs exactly 4 hex digits",
                                ));
                            }
                            let cp =
                                i64::from_str_radix(std::str::from_utf8(hex).unwrap(), 16)
                                    .map_err(|_| {
                                        LexError::new(Span::new(start, i), "invalid `\\u` escape")
                                    })?;
                            let ch = char::from_u32(cp as u32).ok_or_else(|| {
                                LexError::new(Span::new(start, i), "`\\u` not a valid codepoint")
                            })?;
                            let mut tmp = [0u8; 4];
                            buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                            i += 4;
                        }
                        _ => {
                            return Err(LexError::new(
                                Span::new(start, i),
                                format!("unknown string escape \\{}", *e as char),
                            ))
                        }
                    }
                    continue;
                }
                // Regular character: copy its full UTF-8 sequence.
                let ch = source[i..].chars().next().ok_or_else(|| {
                    LexError::new(Span::new(start, i), "malformed UTF-8 in string literal")
                })?;
                let mut tmp = [0u8; 4];
                buf.extend_from_slice(ch.encode_utf8(&mut tmp).as_bytes());
                i += ch.len_utf8();
            }
            if !terminated {
                return Err(LexError::new(
                    Span::new(start, i),
                    "unterminated string literal",
                ));
            }
            let s = String::from_utf8(buf).map_err(|_| {
                LexError::new(Span::new(start, i), "string literal is not valid UTF-8")
            })?;
            out.push(spanned(Token::StringLit(s), Span::new(start, i)));
            ws_since_last_token = false;
            continue;
        }

        // Negative integer literal (D064): `-<digit>` is one IntLit when
        // whitespace precedes it and the previous token ends an operand — so
        // `f -3` applies f to −3, while compact `x-3` and spaced `x - 3`
        // stay binary subtraction. Negative literals are decimal-only: a hex
        // literal (`-0xFF`) stays Minus + hex in every position.
        let is_hex_literal = bytes.get(i + 1) == Some(&b'0')
            && matches!(bytes.get(i + 2), Some(b'x') | Some(b'X'))
            && bytes
                .get(i + 3)
                .is_some_and(|c| c.is_ascii_hexdigit());
        if b == b'-' && bytes.get(i + 1).is_some_and(|c| c.is_ascii_digit())
            && !is_hex_literal
            && ws_since_last_token
            && matches!(
                out.last().map(|t| &t.value),
                Some(
                    Token::Ident(_)
                        | Token::UpperIdent(_)
                        | Token::IntLit(_)
                        | Token::StringLit(_)
                        | Token::RParen
                        | Token::RBracket
                        | Token::SelfKw
                )
            )
        {
            let neg_start = i;
            i += 1; // consume '-'
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let dec = &source[neg_start + 1..i];
            let value: i64 = dec.parse().map_err(|_| {
                LexError::new(Span::new(neg_start, i), "decimal literal out of range for Int")
            })?;
            out.push(spanned(Token::IntLit(-value), Span::new(neg_start, i)));
            ws_since_last_token = false;
            continue;
        }

        // Multi-char operators
        if let Some((tok, len)) = two_char_op(bytes, i) {
            i += len;
            out.push(spanned(tok, Span::new(start, i)));
            ws_since_last_token = false;
            continue;
        }

        // Single-char tokens
        if let Some(tok) = single_char_op(b) {
            i += 1;
            out.push(spanned(tok, Span::new(start, i)));
            ws_since_last_token = false;
            continue;
        }

        return Err(LexError::new(
            Span::new(start, start + 1),
            format!("unexpected character {:?}", b as char),
        ));
    }

    Ok(out)
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

fn is_ident_continue(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn is_keyword(text: &str) -> bool {
    matches!(
        text,
        "if" | "match" | "do" | "return" | "self" | "copy"
            | "when" | "export" | "import" | "from" | "_"
    )
}

fn keyword_or_ident(text: &str) -> Token {
    match text {
        "if" => Token::If,
        "match" => Token::Match,
        "do" => Token::Do,
        "return" => Token::Return,
        "self" => Token::SelfKw,
        "copy" => Token::Copy,
        "when" => Token::When,
        "export" => Token::Export,
        "import" => Token::Import,
        "from" => Token::From,
        "_" => Token::Wildcard,
        _ => {
            let first = text.as_bytes()[0];
            if first.is_ascii_uppercase() {
                Token::UpperIdent(text.to_string())
            } else {
                // lowercase or `_foo`
                Token::Ident(text.to_string())
            }
        }
    }
}

fn two_char_op(bytes: &[u8], i: usize) -> Option<(Token, usize)> {
    let a = *bytes.get(i)?;
    let b = *bytes.get(i + 1)?;
    let tok = match (a, b) {
        (b'>', b'=') => Token::Ge,
        (b'<', b'=') => Token::Le,
        (b'=', b'=') => Token::EqEq,
        (b'!', b'=') => Token::Ne,
        (b'-', b'>') => Token::Arrow,
        (b'=', b'>') => Token::FatArrow,
        (b'<', b'-') => Token::LeftArrow,
        (b'<', b'<') => Token::LtLt,
        (b'>', b'>') => Token::GtGt,
        (b'.', b'.') => Token::DotDot,
        _ => return None,
    };
    Some((tok, 2))
}

fn single_char_op(b: u8) -> Option<Token> {
    Some(match b {
        b'+' => Token::Plus,
        b'-' => Token::Minus,
        b'*' => Token::Star,
        b'/' => Token::Slash,
        b'%' => Token::Percent,
        b'>' => Token::Gt,
        b'<' => Token::Lt,
        b':' => Token::Colon,
        b'=' => Token::Eq,
        b'(' => Token::LParen,
        b')' => Token::RParen,
        b'[' => Token::LBracket,
        b']' => Token::RBracket,
        b'{' => Token::LBrace,
        b'}' => Token::RBrace,
        b';' => Token::Semi,
        b',' => Token::Comma,
        b'@' => Token::At,
        b'&' => Token::Amp,
        b'!' => Token::Bang,
        b'.' => Token::Dot,
        b'^' => Token::Caret,
        b'|' => Token::Pipe,
        _ => return None,
    })
}

/// Precomputed line/character index for O(log n) line/col lookup.
///
/// The free [`line_col_at`] re-scans the source from byte 0 on every call
/// (O(n) each). The layout pass and the ctor-block parser call it per token,
/// which made the whole pass O(n²) in source size. Building this index once
/// (O(n)) and binary-searching (O(log n) per call) fixes that while preserving
/// the exact character-based column semantics of [`line_col_at`] (columns count
/// characters, not bytes, so multi-byte UTF-8 is handled correctly).
#[derive(Debug, Clone)]
pub struct LineIndex {
    /// Total byte length of the source (for EOF spans).
    len: usize,
    /// Byte offset where each line begins (line 1 → 0).
    line_starts: Vec<usize>,
    /// Byte offset of each character start.
    char_starts: Vec<usize>,
}

impl LineIndex {
    /// Build the index in a single O(n) pass over `source`.
    pub fn new(source: &str) -> Self {
        let mut line_starts = vec![0usize];
        let mut char_starts = Vec::new();
        for (i, ch) in source.char_indices() {
            char_starts.push(i);
            if ch == '\n' {
                line_starts.push(i + 1);
            }
        }
        Self {
            len: source.len(),
            line_starts,
            char_starts,
        }
    }

    /// Total byte length of the indexed source.
    pub fn source_len(&self) -> usize {
        self.len
    }

    /// 1-based `(line, col)` for a byte offset — same result as
    /// [`line_col_at`], in O(log n) instead of O(n).
    pub fn line_col_at(&self, offset: usize) -> (usize, usize) {
        // Last line whose start is <= `offset` (0-based line index).
        let line0 = self.line_starts.partition_point(|&s| s <= offset) - 1;
        let line = line0 + 1; // 1-based
        let line_start = self.line_starts[line0];
        // Characters strictly before `offset` minus those before the line start.
        let col = self
            .char_starts
            .partition_point(|&s| s < offset)
            - self.char_starts.partition_point(|&s| s < line_start)
            + 1;
        (line, col)
    }
}

/// Column (1-based) and line (1-based) for each byte offset.
///
/// Convenience wrapper over [`LineIndex`]; builds a fresh index per call (O(n)),
/// so hot loops should build one [`LineIndex`] and reuse it.
pub fn line_col_at(source: &str, offset: usize) -> (usize, usize) {
    LineIndex::new(source).line_col_at(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::Token;

    fn kinds(src: &str) -> Vec<Token> {
        lex(src).expect("lex").into_iter().map(|t| t.value).collect()
    }

    #[test]
    fn keywords_and_idents() {
        assert_eq!(
            kinds("if match do return self copy"),
            vec![
                Token::If,
                Token::Match,
                Token::Do,
                Token::Return,
                Token::SelfKw,
                Token::Copy,
            ]
        );
        assert_eq!(
            kinds("x mapHelper _foo"),
            vec![
                Token::Ident("x".into()),
                Token::Ident("mapHelper".into()),
                Token::Ident("_foo".into()),
            ]
        );
        assert_eq!(
            kinds("Int Option Some"),
            vec![
                Token::UpperIdent("Int".into()),
                Token::UpperIdent("Option".into()),
                Token::UpperIdent("Some".into()),
            ]
        );
        assert_eq!(kinds("_"), vec![Token::Wildcard]);
    }

    #[test]
    fn module_header_keywords_and_dotdot() {
        assert_eq!(
            kinds("export import"),
            vec![Token::Export, Token::Import]
        );
        assert_eq!(kinds(".."), vec![Token::DotDot]);
        // `..` is not confused with a single `.`.
        assert_eq!(
            kinds(". .."),
            vec![Token::Dot, Token::DotDot]
        );
    }

    #[test]
    fn integers_decimal_and_hex() {
        assert_eq!(kinds("0 42"), vec![Token::IntLit(0), Token::IntLit(42)]);
        assert_eq!(
            kinds("0xFF 0x1a2b"),
            vec![Token::IntLit(0xFF), Token::IntLit(0x1a2b)]
        );
    }

    #[test]
    fn minus_not_part_of_literal() {
        assert_eq!(
            kinds("-7"),
            vec![Token::Minus, Token::IntLit(7)]
        );
    }

    #[test]
    fn operators_including_eq_and_dot() {
        assert_eq!(
            kinds("+ - * / > < >= <= == != -> : <- = ( ) [ ] { } ; , @ & ."),
            vec![
                Token::Plus,
                Token::Minus,
                Token::Star,
                Token::Slash,
                Token::Gt,
                Token::Lt,
                Token::Ge,
                Token::Le,
                Token::EqEq,
                Token::Ne,
                Token::Arrow,
                Token::Colon,
                Token::LeftArrow,
                Token::Eq,
                Token::LParen,
                Token::RParen,
                Token::LBracket,
                Token::RBracket,
                Token::LBrace,
                Token::RBrace,
                Token::Semi,
                Token::Comma,
                Token::At,
                Token::Amp,
                Token::Dot,
            ]
        );
    }

    #[test]
    fn pragma_eq_is_ordinary_eq_token() {
        assert_eq!(
            kinds("@[arena_bytes = 1048576]"),
            vec![
                Token::At,
                Token::LBracket,
                Token::Ident("arena_bytes".into()),
                Token::Eq,
                Token::IntLit(1048576),
                Token::RBracket,
            ]
        );
    }

    #[test]
    fn comments_stripped_nested_block() {
        let src = "a {- outer {- inner -} still -} b -- line\nc";
        assert_eq!(
            kinds(src),
            vec![
                Token::Ident("a".into()),
                Token::Ident("b".into()),
                Token::Ident("c".into()),
            ]
        );
    }

    #[test]
    fn spans_track_bytes() {
        let toks = lex("fact n").unwrap();
        assert_eq!(toks[0].span, Span::new(0, 4));
        assert_eq!(toks[1].span, Span::new(5, 6));
    }

    #[test]
    fn string_literals_and_escapes() {
        assert_eq!(kinds("\"hello\""), vec![Token::StringLit("hello".into())]);
        // \n \t \r escapes.
        assert_eq!(kinds("\"a\\nb\""), vec![Token::StringLit("a\nb".into())]);
        assert_eq!(kinds("\"t\\t\""), vec![Token::StringLit("t\t".into())]);
        assert_eq!(kinds("\"r\\r\""), vec![Token::StringLit("r\r".into())]);
        // \\ \" \' escapes.
        assert_eq!(kinds("\"\\\\\""), vec![Token::StringLit("\\".into())]);
        assert_eq!(kinds("\"q\\\"x\""), vec![Token::StringLit("q\"x".into())]);
        assert_eq!(kinds("\"s\\'x\""), vec![Token::StringLit("s'x".into())]);
        // \uXXXX escape (A == 0x41).
        assert_eq!(kinds("\"\\u0041\""), vec![Token::StringLit("A".into())]);
        // Raw non-ASCII UTF-8 passes through.
        assert_eq!(kinds("\"hé\""), vec![Token::StringLit("hé".into())]);
        // Empty string.
        assert_eq!(kinds("\"\""), vec![Token::StringLit(String::new())]);
    }

    #[test]
    fn unterminated_string_is_error() {
        assert!(lex("\"abc").is_err());
        assert!(lex("\"a\\").is_err()); // unterminated escape
    }

    // D064: `-<digit>` is one negative IntLit only when whitespace precedes it
    // and the previous token ends an operand; otherwise `-` stays Minus.
    #[test]
    fn negative_literal_after_operand_with_space() {
        assert_eq!(
            kinds("f -3"),
            vec![Token::Ident("f".into()), Token::IntLit(-3)]
        );
        // Method-call head: the motivating case (`h0.append -3`).
        assert_eq!(
            kinds("h0.append -3"),
            vec![
                Token::Ident("h0".into()),
                Token::Dot,
                Token::Ident("append".into()),
                Token::IntLit(-3),
            ]
        );
        // After other operand-ending tokens.
        assert_eq!(
            kinds("(x) -2"),
            vec![Token::LParen, Token::Ident("x".into()), Token::RParen, Token::IntLit(-2)]
        );
        assert_eq!(
            kinds("[a] -1"),
            vec![
                Token::LBracket,
                Token::Ident("a".into()),
                Token::RBracket,
                Token::IntLit(-1),
            ]
        );
    }

    #[test]
    fn negative_literal_stays_binary_minus() {
        // Compact `x-3`: no whitespace before `-` → subtraction.
        assert_eq!(
            kinds("x-3"),
            vec![Token::Ident("x".into()), Token::Minus, Token::IntLit(3)]
        );
        // Spaced `x - 3`: trailing space after `-` → subtraction.
        assert_eq!(
            kinds("x - 3"),
            vec![Token::Ident("x".into()), Token::Minus, Token::IntLit(3)]
        );
        // After an operator / opener: unary path unchanged.
        assert_eq!(
            kinds("x + -3"),
            vec![
                Token::Ident("x".into()),
                Token::Plus,
                Token::Minus,
                Token::IntLit(3),
            ]
        );
        assert_eq!(
            kinds("[1, -2]"),
            vec![
                Token::LBracket,
                Token::IntLit(1),
                Token::Comma,
                Token::Minus,
                Token::IntLit(2),
                Token::RBracket,
            ]
        );
        assert_eq!(
            kinds("f(-3)"),
            vec![
                Token::Ident("f".into()),
                Token::LParen,
                Token::Minus,
                Token::IntLit(3),
                Token::RParen,
            ]
        );
        // Hex literals are never negative-literal candidates (D064 is
        // decimal-only): `f -0xFF` stays subtraction, not a mangled apply.
        assert_eq!(
            kinds("f -0xFF"),
            vec![Token::Ident("f".into()), Token::Minus, Token::IntLit(255)]
        );
    }

    #[test]
    fn negative_literal_newline_continuation() {
        // Indented continuation: whitespace (newline) before, glued digit → literal.
        assert_eq!(
            kinds("h0.append\n  -3"),
            vec![
                Token::Ident("h0".into()),
                Token::Dot,
                Token::Ident("append".into()),
                Token::IntLit(-3),
            ]
        );
        // Line-broken binary subtraction keeps Minus (space after `-`).
        assert_eq!(
            kinds("x = y\n  - 3"),
            vec![
                Token::Ident("x".into()),
                Token::Eq,
                Token::Ident("y".into()),
                Token::Minus,
                Token::IntLit(3),
            ]
        );
    }
}
