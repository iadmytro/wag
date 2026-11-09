//! Diagnostics for structural checks (pass 1 + pass 2).

use wag_syntax::Span;

/// Spec rule identifier referenced by a diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// §7 rule 1 — whole-program tail-cycle law.
    TailCycle = 1,
    /// §7 rule 10 / §4.3.1a — linear ownership / use-after-move.
    Linearity = 10,
    /// §7 rule 11 / §4.3.1b — borrow well-formedness.
    Borrow = 11,
    /// §7 rule 15 — field path through multi-constructor type.
    FieldPath = 15,
    /// §3.6 — match exhaustiveness (all constructors covered).
    Exhaustiveness = 21,
    /// Internal invariant violation (e.g. a node type inference should have
    /// recorded in the `expr_types` side table is missing). Not a program error.
    Internal = 99,
    /// Ill-formed `copy` / `&` on a pointer-free type (§3.1).
    CopyBorrowPf = 0,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckError {
    pub rule: Rule,
    /// Primary span — the illegal use site.
    pub span: Span,
    pub message: String,
    /// Secondary span + label (rule 10 use-after-move): where the value was
    /// last consumed. Rendered as a linked label by `wag-cli`.
    pub origin: Option<(Span, String)>,
    /// Originating module name (D067). `None` for single-module checks or
    /// whole-program errors that carry no item attribution.
    pub module: Option<String>,
    /// Source text the spans refer to, when known at the entry point. Lets
    /// plain `Display` render line/column instead of raw byte offsets (tests
    /// and panics only see `Display`, not the CLI's ariadne rendering).
    pub source: Option<String>,
}

impl CheckError {
    pub fn new(rule: Rule, span: Span, message: impl Into<String>) -> Self {
        CheckError {
            rule,
            span,
            message: message.into(),
            origin: None,
            module: None,
            source: None,
        }
    }

    /// Stamp the originating module (D067). Used by `check_modules` at the
    /// per-item boundary so deep `?` sites stay untouched.
    pub fn with_module(mut self, module: impl Into<String>) -> Self {
        self.module = Some(module.into());
        self
    }

    /// Attach the source text the spans refer to (entry points that have it).
    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// `line:col` (1-based) for a byte offset into this error's source, or
    /// `None` when no source is attached.
    fn line_col(&self, byte: usize) -> Option<(usize, usize)> {
        let src = self.source.as_ref()?;
        let end = byte.min(src.len());
        let mut line = 1;
        let mut last_nl = 0;
        for (i, b) in src.bytes().take(end).enumerate() {
            if b == b'\n' {
                line += 1;
                last_nl = i + 1;
            }
        }
        Some((line, end - last_nl + 1))
    }
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rule = match self.rule {
            Rule::TailCycle => "rule 1",
            Rule::Linearity => "rule 10",
            Rule::Borrow => "rule 11",
            Rule::FieldPath => "rule 15",
            Rule::Exhaustiveness => "rule 21 (exhaustiveness)",
            Rule::CopyBorrowPf => "copy/& well-formedness",
            Rule::Internal => "internal error",
        };
        match self.line_col(self.span.start) {
            Some((l, c)) => write!(f, "{} ({}) at line {l}, col {c}", self.message, rule)?,
            None => write!(
                f,
                "{} ({}) at {}..{}",
                self.message, rule, self.span.start, self.span.end
            )?,
        }
        if let Some((o, why)) = &self.origin {
            match self.line_col(o.start) {
                Some((l, c)) => write!(f, " [origin: {why} at line {l}, col {c}]")?,
                None => write!(f, " [origin: {why} at {}..{}]", o.start, o.end)?,
            }
        }
        if let Some(m) = &self.module {
            write!(f, " [module: {m}]")?;
        }
        Ok(())
    }
}

impl std::error::Error for CheckError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_with_source_renders_line_col() {
        // "line one\n" (9) + "line two\n" (9) → line 3 starts at byte 18.
        let src = "line one\nline two\nline three";
        let e = CheckError::new(Rule::Linearity, Span::new(18, 23), "use after move")
            .with_source(src);
        assert_eq!(
            e.to_string(),
            "use after move (rule 10) at line 3, col 1"
        );
    }

    #[test]
    fn display_without_source_keeps_byte_range() {
        let e = CheckError::new(Rule::Linearity, Span::new(18, 23), "use after move");
        assert_eq!(e.to_string(), "use after move (rule 10) at 18..23");
    }

    #[test]
    fn display_origin_uses_line_col_when_source_attached() {
        let src = "ab\ncd";
        let e = CheckError {
            rule: Rule::Linearity,
            span: Span::new(3, 4),
            message: "use after move of `x`".into(),
            origin: Some((Span::new(0, 1), "moved here".into())),
            module: None,
            source: Some(src.into()),
        };
        assert_eq!(
            e.to_string(),
            "use after move of `x` (rule 10) at line 2, col 1 [origin: moved here at line 1, col 1]"
        );
    }
}
