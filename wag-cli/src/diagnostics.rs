//! Diagnostic types, error conversions, hints, and ariadne rendering.

use ariadne::{Color, Label, Report, ReportKind, Source};
use wag_check::{CheckError, Rule};
use wag_runtime::RuntimeError;
use wag_syntax::{ParseError, Span};
use wag_types::{Stage as TypeStage, TypeError};
use wag_interp::VmError;

/// Named pipeline stage (for diagnostics and the regression harness).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PipelineStage {
    Resolve,
    Parse,
    Type,
    Check,
    Load,
    Runtime,
}

impl PipelineStage {
    pub fn as_str(self) -> &'static str {
        match self {
            PipelineStage::Resolve => "resolve",
            PipelineStage::Parse => "parse",
            PipelineStage::Type => "type",
            PipelineStage::Check => "check",
            PipelineStage::Load => "load",
            PipelineStage::Runtime => "runtime",
        }
    }
}

/// Structured rejection from any pipeline stage.
#[derive(Debug)]
pub struct Diagnostic {
    pub stage: PipelineStage,
    pub span: Span,
    pub message: String,
    /// Optional actionable hint for the user (e.g. "did you mean ...?").
    pub hint: Option<String>,
    /// Spec rule when the failure is from `wag-check` (e.g. rule 10).
    pub rule: Option<Rule>,
    /// Type-checker sub-stage when applicable.
    pub type_stage: Option<TypeStage>,
    /// Module the error belongs to (multi-module programs). `None` for single-module.
    pub module_name: Option<String>,
    /// Span of the expression/binding that introduced a free metavariable in this
    /// type error. When set, renders a secondary label pointing at the origin.
    pub origin_span: Option<Span>,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] ", self.stage.as_str())?;
        if let Some(m) = &self.module_name {
            write!(f, "in `{m}`: ")?;
        }
        write!(f, "{}", self.message)?;
        if let Some(r) = self.rule {
            write!(f, " ({})", rule_label(r))?;
        }
        if let Some(s) = self.type_stage {
            write!(f, " (type stage {s:?})")?;
        }
        write!(f, " at {}..{}", self.span.start, self.span.end)?;
        if let Some(h) = &self.hint {
            write!(f, "\n  hint: {h}")?;
        }
        Ok(())
    }
}

impl std::error::Error for Diagnostic {}

fn rule_label(r: Rule) -> &'static str {
    match r {
        Rule::TailCycle => "rule 1",
        Rule::Linearity => "rule 10",
        Rule::Borrow => "rule 11",
        Rule::FieldPath => "rule 15",
        Rule::Exhaustiveness => "rule 21 (exhaustiveness)",
        Rule::CopyBorrowPf => "copy/& well-formedness",
        Rule::Internal => "internal error",
    }
}

impl From<ParseError> for Diagnostic {
    fn from(e: ParseError) -> Self {
        let hint = parse_error_hint(&e.message);
        Diagnostic {
            stage: PipelineStage::Parse,
            span: e.span,
            message: e.message,
            hint,
            rule: None,
            type_stage: None,
            module_name: None,
            origin_span: None,
        }
    }
}

fn parse_error_hint(msg: &str) -> Option<String> {
    if msg.contains("unexpected end of input") {
        return Some(
            "Check for missing closing brackets, parentheses, or a missing blank line before a new definition."
                .into(),
        );
    }
    if msg.contains("expected Eq") || msg.contains("expected `=`") {
        return Some(
            "Did you forget a blank line before this definition? In WAG, definitions are separated by blank lines."
                .into(),
        );
    }
    if msg.contains("expected RParen") || msg.contains("expected `)`") {
        return Some("A `(` was not closed. Check your parentheses.".into());
    }
    if msg.contains("expected RBracket") || msg.contains("expected `]`") {
        return Some("A `[` was not closed. Check your brackets.".into());
    }
    if msg.contains("expected block end") {
        return Some(
            "A block is `{ ... }` or an indentation-delimited group of `name = value` bindings followed by a final expression. \
             The parser reached the end of a block but found this token instead of the closing brace (or dedent). \
             Check for an unclosed `{`, misaligned indentation, or a stray indented line that should be a comment (comments must start with `--`)."
                .into(),
        );
    }
    None
}

impl From<TypeError> for Diagnostic {
    fn from(e: TypeError) -> Self {
        // Prefer a structured causal note attached at construction; fall back to
        // message-pattern hints only when the type stage did not supply one.
        let hint = e.context.or_else(|| type_error_hint(&e.message));
        Diagnostic {
            stage: PipelineStage::Type,
            span: e.span,
            message: e.message,
            hint,
            rule: None,
            type_stage: Some(e.stage),
            module_name: e.module,
            origin_span: e.origin_span,
        }
    }
}

/// Provide actionable hints for common type error patterns.
/// Prelude function names for "did you mean" suggestions.
const PRELUDE_NAMES: &[&str] = &[
    "isEmpty", "peek", "step", "rewind", "append", "prepend", "popFirst",
    "length", "reverse", "map", "filter", "fold", "concat", "flatMap", "zip",
    "any", "all", "abs", "stringSplit", "stringJoin", "listNew",
];

/// Simple Levenshtein distance (for short identifier suggestions).
fn levenshtein(a: &str, b: &str) -> usize {
    let (la, lb) = (a.len(), b.len());
    if la == 0 { return lb; }
    if lb == 0 { return la; }
    let mut prev: Vec<usize> = (0..=lb).collect();
    let mut curr = vec![0usize; lb + 1];
    for i in 1..=la {
        curr[0] = i;
        for j in 1..=lb {
            let cost = if a.as_bytes()[i - 1] == b.as_bytes()[j - 1] { 0 } else { 1 };
            curr[j] = (curr[j - 1] + 1).min(prev[j] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[lb]
}

/// Suggest the closest prelude name within a threshold.
fn did_you_mean(name: &str) -> Option<&'static str> {
    let mut best: Option<(&'static str, usize)> = None;
    for &p in PRELUDE_NAMES {
        let d = levenshtein(name, p);
        if d <= 3 && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((p, d));
        }
    }
    best.map(|(n, _)| n)
}

fn type_error_hint(msg: &str) -> Option<String> {
    // List vs scalar mismatch
    if msg.contains("expected") && msg.contains("List") && msg.contains("found") {
        return Some(
            "A list is expected here. Use list syntax [1, 2, 3] or listNew."
                .into(),
        );
    }
    if msg.contains("Actor") && msg.contains("expected") && msg.contains("found") {
        return Some(
            "send expects an actor reference as its first argument."
                .into(),
        );
    }
    if msg.contains("expected") && (msg.contains("a -> b") || msg.contains("Int -> Int")) {
        return Some(
            "The first argument of map, filter, fold must be a function. \
             Define a named function and pass its name (not a value)."
                .into(),
        );
    }
    if let Some(name) = extract_unknown_name(msg) {
        if let Some(sugg) = did_you_mean(name) {
            return Some(format!("Unknown name {}. Did you mean {}?", name, sugg));
        }
    }
    None
}

/// Extract the unknown name from an error message like "unresolved name `foo`".
fn extract_unknown_name(msg: &str) -> Option<&str> {
    // Pattern: "unresolved name `X`" or "unknown name `X`"
    for keyword in ["unresolved name `", "unknown name `"] {
        if let Some(idx) = msg.find(keyword) {
            let rest = &msg[idx + keyword.len()..];
            if let Some(end) = rest.find('`') {
                return Some(&rest[..end]);
            }
        }
    }
    None
}

impl From<CheckError> for Diagnostic {
    fn from(e: CheckError) -> Self {
        // Rule 10 use-after-move carries a secondary span (where the value was
        // consumed); render it as the linked origin label.
        let (origin_span, hint) = match e.origin {
            Some((span, why)) => (Some(span), Some(why)),
            None => (None, None),
        };
        Diagnostic {
            stage: PipelineStage::Check,
            span: e.span,
            message: e.message,
            hint,
            rule: Some(e.rule),
            type_stage: None,
            // D067: check errors carry their originating module; rendering
            // picks the right source via `ModuleSet::source_for`.
            module_name: e.module,
            origin_span,
        }
    }
}

impl From<VmError> for Diagnostic {
    fn from(e: VmError) -> Self {
        Diagnostic {
            stage: PipelineStage::Runtime,
            span: Span::new(0, 0),
            message: e.to_string(),
            hint: None,
            rule: None,
            type_stage: None,
            module_name: None,
            origin_span: None,
        }
    }
}

impl From<RuntimeError> for Diagnostic {
    fn from(e: RuntimeError) -> Self {
        Diagnostic {
            stage: PipelineStage::Runtime,
            span: Span::new(0, 0),
            message: e.to_string(),
            hint: None,
            rule: None,
            type_stage: None,
            module_name: None,
            origin_span: None,
        }
    }
}

/// Convert a source **byte** offset to a **character** index.
///
/// Spans in this codebase are byte ranges produced by the lexer (see
/// `wag-syntax/src/lexer.rs`), but ariadne positions labels by character
/// (code-point) index into `Source::from(&str)`. Converting here keeps
/// highlighting correct for any file containing multibyte UTF-8 — e.g. the
/// em-dashes and arrows in WAG comments — where a raw byte offset would land on
/// the wrong line/column.
fn byte_to_char(source: &str, byte_off: usize) -> usize {
    if byte_off >= source.len() {
        return source.chars().count();
    }
    let mut char_idx = 0usize;
    let mut byte_idx = 0usize;
    for ch in source.chars() {
        if byte_idx >= byte_off {
            break;
        }
        byte_idx += ch.len_utf8();
        char_idx += 1;
    }
    char_idx
}

/// Render a diagnostic with ariadne (span-anchored when possible).
pub fn render_diagnostic(source_name: &str, source: &str, diag: &Diagnostic) -> String {
    let mut out = Vec::new();
    let range = if diag.span.is_empty() && source.is_empty() {
        0..0
    } else if diag.span.is_empty() && diag.span.start == 0 {
        // Truly unknown span (0..0) — point at the first character.
        0..source.len().min(1)
    } else {
        let start = diag.span.start.min(source.len());
        let end = diag.span.end.min(source.len()).max(start);
        if start == end {
            // Zero-width span at a known position — highlight one char.
            start..(start + 1).min(source.len())
        } else {
            start..end
        }
    };
    // ariadne labels positions by character index, but `range` is in byte offsets.
    let range = byte_to_char(source, range.start)..byte_to_char(source, range.end);

    let mut msg = match &diag.module_name {
        Some(m) => format!("[{}] in `{m}`: {}", diag.stage.as_str(), diag.message),
        None => format!("[{}] {}", diag.stage.as_str(), diag.message),
    };
    if let Some(r) = diag.rule {
        msg.push_str(&format!(" ({})", rule_label(r)));
    }
    if let Some(s) = diag.type_stage {
        msg.push_str(&format!(" (type stage {s:?})"));
    }

    // Provenance: when a free metavariable introduced this error, anchor a
    // secondary label at the expression/binding that created it (§7).
    let mut report = Report::build(ReportKind::Error, (source_name, range.clone()))
        .with_message(&msg)
        .with_label(
            Label::new((source_name, range))
                .with_message(&msg)
                .with_color(Color::Red),
        );
    if let Some(orig_span) = diag.origin_span {
        let ostart = orig_span.start.min(source.len());
        let oend = orig_span.end.min(source.len()).max(ostart);
        let orange_bytes = if ostart == oend {
            ostart..(ostart + 1).min(source.len())
        } else {
            ostart..oend
        };
        let orange = byte_to_char(source, orange_bytes.start)..byte_to_char(source, orange_bytes.end);
        let origin_msg = diag.hint.clone().unwrap_or_else(|| "where this type is introduced".into());
        report = report.with_label(
            Label::new((source_name, orange))
                .with_message(origin_msg)
                .with_color(Color::Cyan),
        );
    }
    let _ = report.finish().write((source_name, Source::from(source)), &mut out);

    // Surface the structured causal note / actionable hint (first-class on
    // `TypeError`, threaded through `Diagnostic.hint`) below the ariadne report.
    if let Some(h) = &diag.hint {
        out.extend(b"\n  hint: ");
        out.extend(h.as_bytes());
    }

    String::from_utf8_lossy(&out).into_owned()
}

