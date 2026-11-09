//! WAG surface syntax: lexer, layout, parser.
//!
//! - token stream + layout preprocessing (§2.1–§2.2)
//! - Pratt parser, AST, do/list/unary-minus desugar (§2.3, §6)
//! - actors/pragmas, `@Name` atoms, `.`/`with` record syntax, desugar template

#![allow(dead_code)]

mod ast;
mod comprehension_desugar;
mod desugar_main;
mod desugar_record;
mod layout;
mod lambda_lift;
mod lexer;
mod method_bare_names;
mod parser;
mod pretty;
mod token;
mod vec_arr_desugar;

pub use ast::*;
pub use desugar_main::{desugar_main, MAIN_BOOT, MAIN_END};
pub use desugar_record::{
    desugar_record_update_template, ResolvedFieldStep, ResolvedLeaf,
};
pub use layout::{layout, lex_with_layout, LayoutError};
pub use lexer::{lex, line_col_at, LineIndex, LexError};
pub use comprehension_desugar::desugar_comprehensions;
pub use lambda_lift::lift_lambdas;
pub use pretty::{debug_dump, debug_enabled, pretty_expr, pretty_program, pretty_type};
pub use method_bare_names::desugar_method_bare_names;
pub use vec_arr_desugar::desugar_vec_arr_methods;
pub use parser::{parse, parse_desugared, parse_tokens, finish_program, ParseError};
pub use token::{spanned, SpannedToken, Token};

/// Byte span into the source file (half-open `[start, end)`).
///
/// Threaded through tokens so ariadne can render diagnostics
/// without a later span retrofit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub const fn empty(at: usize) -> Self {
        Self {
            start: at,
            end: at,
        }
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            start: self.start.min(other.start),
            end: self.end.max(other.end),
        }
    }

    pub fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(self) -> bool {
        self.start >= self.end
    }
}

impl From<std::ops::Range<usize>> for Span {
    fn from(r: std::ops::Range<usize>) -> Self {
        Self::new(r.start, r.end)
    }
}

impl From<Span> for std::ops::Range<usize> {
    fn from(s: Span) -> Self {
        s.start..s.end
    }
}

/// Source file identity for multi-file diagnostics (path or synthetic name).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceId(pub String);

/// A value tagged with its source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spanned<T> {
    pub value: T,
    pub span: Span,
}

impl<T> Spanned<T> {
    pub fn new(value: T, span: Span) -> Self {
        Self { value, span }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Spanned<U> {
        Spanned {
            value: f(self.value),
            span: self.span,
        }
    }
}

/// Placeholder so ariadne is linked and the diagnostics path is exercised once.
pub fn report_placeholder_error(
    source_name: &str,
    source: &str,
    span: Span,
    message: &str,
) -> String {
    use ariadne::{Color, Label, Report, ReportKind, Source};

    let mut out = Vec::new();
    let _ = Report::build(ReportKind::Error, (source_name, span.start..span.end))
        .with_message(message)
        .with_label(
            Label::new((source_name, std::ops::Range::<usize>::from(span)))
                .with_message(message)
                .with_color(Color::Red),
        )
        .finish()
        .write((source_name, Source::from(source)), &mut out);
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_union_and_range_roundtrip() {
        let a = Span::new(1, 4);
        let b = Span::new(3, 10);
        assert_eq!(a.union(b), Span::new(1, 10));
        let r: std::ops::Range<usize> = a.into();
        assert_eq!(Span::from(r), a);
    }

    #[test]
    fn ariadne_placeholder_renders() {
        let src = "fact n = n\n";
        let rendered = report_placeholder_error("t.wag", src, Span::new(0, 4), "demo");
        assert!(rendered.contains("demo"), "{rendered}");
        assert!(
            rendered.contains("t.wag") || rendered.contains("fact"),
            "{rendered}"
        );
    }
}
