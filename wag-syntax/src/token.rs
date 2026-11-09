//! Token kinds and spanned tokens for WAG (§2.2).

use crate::Span;

/// Lexical token kind. Layout may insert virtual `{` `;` `}` with empty spans.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Token {
    /// Lowercase / `_`-initial identifier (value or function name).
    Ident(String),
    /// Handle-name identifier: lowercase ident followed by trailing `@` (§2.2).
    /// Used as send target in expression position; marks actor-typed names in
    /// parameter and pattern positions.
    IdentAt(String),
    /// Uppercase-initial identifier (type or constructor).
    UpperIdent(String),
    /// Integer literal value. Positive from decimal/hex literals; negative
    /// only via the D064 `-<digit>` lexing rule.
    IntLit(i64),
    /// Double-quoted string literal, unescaped (§2.2). UTF-8 source text.
    StringLit(String),

    // Keywords
    If,
    Match,
    Do,
    Return,
    SelfKw,
    Copy,
    /// Match-arm guard introducer (§6.5): `pattern when <bool> -> body`.
    When,
    /// Module header: opens the `export` block (names made public).
    Export,
    /// Module header: opens the `import` block (modules + names taken).
    Import,
    /// Re-export: `export <name> from <module>` — the name is re-exported
    /// from another module (the consumer's import resolves to the true source).
    From,

    /// Standalone `_` wildcard (not `_foo`).
    Wildcard,

    // Operators & punctuation
    Plus,
    Minus,
    Star,
    Slash,
    Percent, // %
    Gt,
    Lt,
    Ge,
    Le,
    EqEq,
    Ne,
    Arrow,      // ->
    FatArrow,   // =>  (constrained signature, §3.12)
    Colon,      // :
    LeftArrow,  // <-
    Eq,        // =
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Semi,
    Comma,
    At,     // @
    Amp,    // &
    Bang,   // !  (bitNot / logical NOT)
    Dot,    // .
    DotDot, // ..
    Caret,  // ^  (bitXor)
    Pipe,   // |  (bitOr)
    LtLt,   // << (shiftLeft)
    GtGt,   // >> (shiftRight)

    /// Layout-inserted `{` (empty span at insertion point).
    VirtualLBrace,
    /// Layout-inserted `;`.
    VirtualSemi,
    /// Layout-inserted `}`.
    VirtualRBrace,
}

impl Token {
    pub fn is_layout_keyword(&self) -> bool {
        matches!(self, Token::Do | Token::If | Token::Match)
    }

    /// Keywords that open layout immediately after the keyword (`do`, `if`, `export`).
    pub fn opens_layout_after_self(&self) -> bool {
        matches!(self, Token::Do | Token::If | Token::Export)
    }

    /// Opens the import block (a block whose group-headers open item sub-blocks).
    pub fn is_import_opener(&self) -> bool {
        matches!(self, Token::Import)
    }

    /// Keywords that open layout after an atomic receiver/scrutinee (`match`).
    pub fn opens_layout_after_atom(&self) -> bool {
        matches!(self, Token::Match)
    }

    pub fn is_virtual(&self) -> bool {
        matches!(
            self,
            Token::VirtualLBrace | Token::VirtualSemi | Token::VirtualRBrace
        )
    }
}

/// Token with source span. Virtual layout tokens use an empty span at the
/// insertion point (start of the following real token, or EOF).
pub type SpannedToken = crate::Spanned<Token>;

pub fn spanned(token: Token, span: Span) -> SpannedToken {
    crate::Spanned::new(token, span)
}
