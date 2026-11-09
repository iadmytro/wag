//! Type-checking diagnostics.

use wag_syntax::Span;

/// Stage at which a diagnostic was produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// §3.1.1 constructor/type namespace uniqueness.
    Namespacing,
    /// `Ref T` visibility boundary / method-field disjointness (D056/D057).
    Boundary,
    /// Ordinary unification / annotation mismatch / exhaustiveness.
    Inference,
}

/// Provenance for a metavariable: what expression or binding introduced it and
/// where. Recorded at `fresh()` time so that free-var letters in error messages
/// can be explained to the user instead of leaked as bare indices (§7 provenance).
#[derive(Debug, Clone)]
pub struct OriginInfo {
    /// Human-readable description of the introducing binding/expression, e.g.
    /// "parameter `x`" or "return type of `f`". Rendered inline in messages.
    pub desc: String,
    /// Span of the introducing expression/binding (for a diagnostic hint/label).
    pub span: Span,
}

impl OriginInfo {
    pub fn new(desc: impl Into<String>, span: Span) -> Self {
        OriginInfo {
            desc: desc.into(),
            span,
        }
    }

    /// Provenance for an ephemeral metavariable with no meaningful source site.
    pub(super) fn anonymous(span: Span) -> Self {
        OriginInfo {
            desc: "an inferred type".into(),
            span,
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct TypeError {
    pub message: String,
    pub span: Span,
    pub stage: Stage,
    /// Optional causal note (what to inspect / why this happened). Kept as a
    /// first-class field so the renderer can surface it directly instead of
    /// scraping it back out of the free-form `message` string.
    pub context: Option<String>,
    /// Owning module name, for multi-module programs. `None` for single-module
    /// programs and the prelude. Carried into `Diagnostic.module_name`.
    pub module: Option<String>,
    /// Span of the expression/binding that introduced a free metavariable in this
    /// error (provenance). When set, the diagnostic renders a secondary label at
    /// the origin so the user sees *where* an unknown type comes from (§7).
    pub origin_span: Option<Span>,
    /// Source text the spans refer to, when known at the entry point. Lets
    /// plain `Display` render line/column instead of raw byte offsets (tests
    /// and panics only see `Display`, not the CLI's ariadne rendering).
    pub source: Option<String>,
}

impl std::fmt::Debug for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TypeError")
            .field("message", &self.message)
            .field("span", &self.span)
            .field("stage", &self.stage)
            .field("context", &self.context)
            .field("module", &self.module)
            .field("origin_span", &self.origin_span)
            .field(
                "source",
                &self.source.as_ref().map(|s| format!("<{} bytes>", s.len())),
            )
            .finish()
    }
}

impl TypeError {
    pub fn new(stage: Stage, span: Span, message: impl Into<String>) -> Self {
        TypeError {
            message: message.into(),
            span,
            stage,
            context: None,
            module: None,
            origin_span: None,
            source: None,
        }
    }

    /// Attach a causal note rendered alongside the headline `message`. An empty
    /// string is ignored (no-op), so callers can always chain `.with_context(
    /// maybe_provenance.unwrap_or_default())`.
    pub fn with_context(mut self, ctx: impl Into<String>) -> Self {
        let s = ctx.into();
        if !s.is_empty() {
            self.context = Some(s);
        }
        self
    }

    /// Attribute this error to an owning module (multi-module programs).
    pub fn with_module(mut self, module: impl Into<String>) -> Self {
        self.module = Some(module.into());
        self
    }

    /// Anchor this error at the span of the expression/binding that introduced a
    /// free metavariable (provenance), so the renderer can point at the origin.
    /// `None` leaves no origin label.
    pub fn with_origin_span(mut self, span: Option<Span>) -> Self {
        self.origin_span = span;
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

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line_col(self.span.start) {
            Some((l, c)) => write!(f, "{} (stage {:?}) at line {l}, col {c}", self.message, self.stage)?,
            None => write!(f, "{} (stage {:?}) at {}..{}", self.message, self.stage, self.span.start, self.span.end)?,
        }
        if let Some(ctx) = &self.context {
            write!(f, " [{ctx}]")?;
        }
        if let Some(o) = &self.origin_span {
            match self.line_col(o.start) {
                Some((l, c)) => write!(f, " [origin: line {l}, col {c}]")?,
                None => write!(f, " [origin: {}..{}]", o.start, o.end)?,
            }
        }
        if let Some(m) = &self.module {
            write!(f, " [module: {m}]")?;
        }
        Ok(())
    }
}

impl std::error::Error for TypeError {}
