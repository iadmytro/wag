//! Abstract syntax tree for WAG (full v2.16 surface syntax, Milestone 3).

use crate::Span;

/// A complete source file: optional header (`export`/`import`) + top-level items in order.
///
/// The header, when present, must appear before any definition (§3.1 of the module
/// design). A file with no header exports nothing and imports nothing (prelude only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    /// `export` block — names made public (private-by-default). `None` if absent.
    pub exports: Option<ExportBlock>,
    /// `import` block — modules and the names taken from them. `None` if absent.
    pub imports: Option<ImportBlock>,
    pub items: Vec<Item>,
    /// Source text the spans refer to, when known at the entry point. Lets
    /// diagnostics render line/column instead of raw byte offsets.
    pub source: Option<String>,
}

impl Program {
    /// Constructor declarations collected from `items` (same order as source).
    pub fn constructors(&self) -> Vec<&ConstructorDecl> {
        let mut out = Vec::new();
        for i in &self.items {
            match i {
                Item::Constructor(c) => out.push(c),
                Item::ParametricType(pt) => out.extend(pt.ctors.iter()),
                _ => {}
            }
        }
        out
    }

    /// Actor (`@`-declaration) definitions.
    pub fn actors(&self) -> Vec<&ActorDef> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Actor(a) => Some(a),
                _ => None,
            })
            .collect()
    }

    /// Group constructor decls by result type name (`Sig`'s `T`).
    pub fn constructors_by_type(&self) -> Vec<(String, Vec<&ConstructorDecl>)> {
        let mut map: Vec<(String, Vec<&ConstructorDecl>)> = Vec::new();
        for c in self.constructors() {
            let key = c.result_type.head_name().unwrap_or_else(|| "<unknown>".into());
            if let Some(slot) = map.iter_mut().find(|(k, _)| *k == key) {
                slot.1.push(c);
            } else {
                map.push((key, vec![c]));
            }
        }
        map
    }

    /// Type alias declarations (`UpperIdent = TypeExpr`).
    pub fn type_aliases(&self) -> Vec<&TypeAliasDecl> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::TypeAlias(a) => Some(a),
                _ => None,
            })
            .collect()
    }

    /// Method declarations (D056): `TypeName.name = ...`, in source order.
    pub fn methods(&self) -> Vec<&MethodDef> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Method(m) => Some(m),
                _ => None,
            })
            .collect()
    }

    /// Parametric type declarations (`UpperIdent type_vars` + block).
    pub fn parametric_types(&self) -> Vec<&ParametricTypeDecl> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::ParametricType(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    /// Typeclass declarations (D075): `Name C₁ … Cₙ = m₁ : sig₁ / …`.
    pub fn classes(&self) -> Vec<&ClassDecl> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Class(c) => Some(c),
                _ => None,
            })
            .collect()
    }

    /// Typeclass instance declarations (D075): `Name Constructor = …`.
    pub fn instances(&self) -> Vec<&InstanceDecl> {
        self.items
            .iter()
            .filter_map(|i| match i {
                Item::Instance(i) => Some(i),
                _ => None,
            })
            .collect()
    }
}

/// The kind of a header item, determined by the name form.
///
/// Ordering is Data < Actor < Function, matching the enforced item order
/// (Data → Actors → Functions) in `export` blocks and `import` groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ItemKind {
    /// A data type name (uppercase, e.g. `Pair`).
    Data,
    /// An actor name (`@`-prefixed, e.g. `@server`).
    Actor,
    /// A function name (lowercase, e.g. `length`).
    Function,
}

impl ItemKind {
    /// Determine the kind from a name and its actor flag.
    pub fn from_name(name: &str, is_actor: bool) -> ItemKind {
        if is_actor {
            ItemKind::Actor
        } else if name.starts_with(|c: char| c.is_ascii_uppercase()) {
            ItemKind::Data
        } else {
            ItemKind::Function
        }
    }
}

/// `export` block: which names are public. One item per line, Data → Actors → Functions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportBlock {
    pub items: Vec<ExportItem>,
    pub span: Span,
}

/// `import` block: modules and the names taken from them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportBlock {
    pub groups: Vec<ImportGroup>,
    pub span: Span,
}

/// One imported module and the names taken from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportGroup {
    /// Module name (file name without `.wag`).
    pub module: String,
    /// Names taken from the module (Data → Actors → Functions).
    pub items: Vec<ImportItem>,
    pub span: Span,
}

/// A single export item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExportItem {
    /// A name (data / actor / function), optionally aliased, with an optional
    /// surfaced signature (functions only).
    Named {
        /// The source name (without `@` for actors).
        name: String,
        /// `true` if the name is an actor (`@`-prefixed).
        is_actor: bool,
        /// `alias <- source`: the alias (without `@` for actors). `None` if not aliased.
        alias: Option<String>,
        /// Surfaced signature (functions only). `None` if not repeated.
        signature: Option<Type>,
        /// Re-export source module: `export <name> from <module>`. `None` if
        /// the name is defined locally in this module.
        from: Option<String>,
        span: Span,
    },
    /// `..` — export everything.
    All { span: Span },
}

impl ExportItem {
    /// The kind of this item (for order enforcement). `All` is kind-agnostic.
    pub fn kind(&self) -> Option<ItemKind> {
        match self {
            ExportItem::Named { name, is_actor, .. } => Some(ItemKind::from_name(name, *is_actor)),
            ExportItem::All { .. } => None,
        }
    }

    /// The effective name (the alias if present, else the source name).
    pub fn effective_name(&self) -> Option<&str> {
        match self {
            ExportItem::Named { alias, name, .. } => {
                Some(alias.as_deref().unwrap_or(name.as_str()))
            }
            ExportItem::All { .. } => None,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            ExportItem::Named { span, .. } | ExportItem::All { span } => *span,
        }
    }
}

/// A single import item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportItem {
    /// A name (data / actor / function), optionally aliased.
    Named {
        /// The source name (without `@` for actors).
        name: String,
        /// `true` if the name is an actor (`@`-prefixed).
        is_actor: bool,
        /// `alias <- source`: the alias (without `@` for actors). `None` if not aliased.
        alias: Option<String>,
        span: Span,
    },
    /// `..` — all names of the kind.
    All { span: Span },
}

impl ImportItem {
    /// The kind of this item (for order enforcement). `All` is kind-agnostic.
    pub fn kind(&self) -> Option<ItemKind> {
        match self {
            ImportItem::Named { name, is_actor, .. } => Some(ItemKind::from_name(name, *is_actor)),
            ImportItem::All { .. } => None,
        }
    }

    /// The effective name (the alias if present, else the source name).
    pub fn effective_name(&self) -> Option<&str> {
        match self {
            ImportItem::Named { alias, name, .. } => {
                Some(alias.as_deref().unwrap_or(name.as_str()))
            }
            ImportItem::All { .. } => None,
        }
    }

    pub fn span(&self) -> Span {
        match self {
            ImportItem::Named { span, .. } | ImportItem::All { span } => *span,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Func(FuncDef),
    Constructor(ConstructorDecl),
    /// `@Name: Sig` + single equation (§5.5.2).
    Actor(ActorDef),
    /// `UpperIdent = TypeExpr` — type alias (e.g. `PairInt = Pair Int Int`).
    TypeAlias(TypeAliasDecl),
    /// `UpperIdent type_vars` + constructor block — parametric user type.
    ParametricType(ParametricTypeDecl),
    /// Method on a region-resident type (D056/D057): `TypeName.name = ...`.
    Method(MethodDef),
    /// Typeclass declaration (D075): `Name C₁ … Cₙ = m₁ : sig₁ / …`.
    Class(ClassDecl),
    /// Typeclass instance (D075): `Name Constructor = …`.
    Instance(InstanceDecl),
}

/// Method declaration on a region-resident type (D056/D057):
/// `TypeName.name = [typeSig:] params -> body`.
///
/// A method is not a function: it does not participate in §3.5 generalization,
/// its receiver is implicit (bare atoms in the body resolve against it), and
/// under D057 it is a single step — no recursion, no iteration. The dotted name
/// (`TypeName.name`) is its identity; calls are ordinary application on a field
/// expression (`l.append 42` ⇒ `Apply(Field(l, "append"), [42])`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodDef {
    /// The type the method is declared on (e.g. `List`).
    pub type_name: String,
    /// The method name (lowercase ident, e.g. `append`).
    pub name: String,
    pub signature: Option<Type>,
    pub params: Vec<String>,
    pub body: Expr,
    pub span: Span,
}

impl MethodDef {
    /// Dotted identity (`List.append`) — the key in the method table.
    pub fn dotted_name(&self) -> String {
        format!("{}.{}", self.type_name, self.name)
    }
}

/// Top-level function, unified declaration form (D034): the name appears exactly once.
///
/// `name: [properties:] [typeSig:] params = body`
///
/// - `properties`: optional open-ended list of property names (lowercase idents),
///   user-definable — e.g. `commutative, associative`. Checked by a separate
///   property-testing mechanic (future work).
/// - `typeSig`: optional type signature (`T1, T2 -> T`).
/// - `params = body`: the implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuncDef {
    pub name: String,
    /// Open-ended property names (e.g. `commutative`, `associative`). Empty if none.
    pub properties: Vec<String>,
    pub signature: Option<Type>,
    /// Class constraints from a `C₁ T₁, … => sig` annotation (§3.12). Empty if none.
    pub constraints: Vec<Type>,
    pub params: Vec<String>,
    pub body: Expr,
    pub span: Span,
}

/// `@Name` actor declaration with optional preceding pragma (§2.4, §5.5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActorDef {
    /// Name without the leading `@` (e.g. `Counter`, `main`).
    pub name: String,
    /// System actors this actor interacts with (properties section).
    /// e.g. `@std`, `@net`.
    pub properties: Vec<String>,
    pub signature: Option<Type>,
    pub params: Vec<String>,
    pub body: Expr,
    /// Zero or one pragma attached immediately above this declaration.
    pub pragma: Option<Pragma>,
    pub span: Span,
}

/// `@Name[arg = val, ...]` (§2.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pragma {
    pub name: String,
    pub args: Vec<PragmaArg>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PragmaArg {
    pub key: String,
    pub value: i64,
    pub span: Span,
}

/// `Name: Sig` constructor declaration (§3.1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstructorDecl {
    pub name: String,
    pub fields: Vec<FieldDecl>,
    pub result_type: Type,
    pub span: Span,
}

/// `UpperIdent = TypeExpr` — type alias (e.g. `PairInt = Pair Int Int`).
///
/// Resolved eagerly by the type checker before inference. Cycles are a
/// compile-time error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAliasDecl {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

/// `UpperIdent type_vars` + constructor block — parametric user type.
///
/// e.g.:
/// ```wag
/// MyPair a b
///   MyPair: a, b
/// ```
///
/// Type variables are lowercase identifiers scoped to this declaration.
/// Each constructor's field types may reference the type variables.
///
/// `ops` holds operator-overload definitions at the variant column (V1
/// syntax). A declaration with empty `ctors` and non-empty `ops` is a
/// **type extension**: it attaches overloads to an already-defined type
/// (e.g. a builtin like `Vec`) without redefining it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParametricTypeDecl {
    pub name: String,
    /// Type variable names (e.g. `["a", "b"]`).
    pub type_vars: Vec<String>,
    /// Constructors belonging to this type.
    pub ctors: Vec<ConstructorDecl>,
    pub span: Span,
}

/// Typeclass declaration (D075, §3.12, §6.10): `Name C₁ … Cₙ = m₁ : sig₁ / …`.
///
/// Binds each method name as a top-level function whose scheme is constrained
/// by the class. The type-constructor parameters (`C₁…Cₙ`) are in scope within
/// method signatures and may appear as the head of a type application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassDecl {
    /// Class name (e.g. `Map`).
    pub name: String,
    /// Type-constructor parameter names (e.g. `["C"]`). v1: exactly one.
    pub params: Vec<String>,
    /// Method signatures declared by this class.
    pub methods: Vec<ClassMethod>,
    pub span: Span,
}

/// A single method signature within a class declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassMethod {
    /// Method name (lowercase ident, e.g. `map`).
    pub name: String,
    /// The method's type signature (a `Type::Func` using the class params).
    pub sig: Type,
    pub span: Span,
}

/// Typeclass instance declaration (D075, §3.12, §6.10): `Name Constructor = …`.
///
/// Provides method implementations for one concrete container type. Helper
/// functions are top-level definitions written within the instance block for
/// organization; they are visible program-wide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceDecl {
    /// The class being instantiated (e.g. `Map`).
    pub class_name: String,
    /// The concrete type being instantiated (e.g. `String`, `Vec Int`).
    pub instance_ty: Type,
    /// Method implementations (one per class method, in any order).
    pub methods: Vec<FuncDef>,
    /// Optional helper functions (top-level bindings scoped syntactically here).
    pub helpers: Vec<FuncDef>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDecl {
    pub label: Option<String>,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// Type constructor or nullary type name (`Int`, `CounterMsg`, …).
    Name(String),
    /// Juxtaposed application: `Option Int`, `List Int`, `Pair A B`.
    App(Box<Type>, Vec<Type>),
    /// `&T` (parameter-only in well-formed programs).
    Borrow(Box<Type>),
    /// `ParamList -> T`
    Func(Vec<Type>, Box<Type>),
    /// N-ary product type: `(A, B, C, ...)`
    Product(Vec<Type>),
}

impl Type {
    pub fn head_name(&self) -> Option<String> {
        match self {
            Type::Name(n) => Some(n.clone()),
            Type::App(h, _) => h.head_name(),
            Type::Borrow(t) => t.head_name(),
            Type::Func(_, r) => r.head_name(),
            Type::Product(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Gt,
    Lt,
    Ge,
    Le,
    Eq,
    Ne,
    BitAnd,
    BitOr,
    BitXor,
    BitNot,
    ShiftLeft,
    ShiftRight,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Int {
        value: i64,
        span: Span,
    },
    /// Double-quoted string literal (§2.2) — a `String` value (§3).
    Str {
        value: String,
        span: Span,
    },
    /// Lowercase / keyword-as-ident (`copy`, `send`, …) / `_foo`.
    Var {
        name: String,
        span: Span,
    },
    /// Uppercase atom (`Some`, `Empty`, `Unit`, user ctor) before application.
    Upper {
        name: String,
        span: Span,
    },
    /// `@Name` actor reference atom (§2.3).
    AtName {
        name: String,
        span: Span,
    },
    Self_ {
        span: Span,
    },
    /// `()` — the Unit value (§3).
    Unit {
        span: Span,
    },
    /// Flat n-ary application (§2.3).
    Apply {
        fun: Box<Expr>,
        args: Vec<Expr>,
        span: Span,
    },
    /// Actor message send: `ident@ msg` (§5.5.3). Target is always a variable
    /// of type `Actor Msg`; the message expression is enqueued asynchronously.
    Send {
        target: Box<Expr>,
        msg: Box<Expr>,
        span: Span,
    },
    BinOp {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    /// Single-step postfix field access `e.field` (§3.1.2).
    /// Chains are left-associated trees: `p.a.b` ⇒ `Field(Field(p,a), b)`.
    Field {
        base: Box<Expr>,
        field: String,
        span: Span,
    },
    /// Record update: `with recv { clauses }` or single-clause `path = expr` (§3.1.2).
    /// Not desugared at parse time — see [`crate::desugar::desugar_record_update_template`].
    RecordUpdate {
        receiver: Box<Expr>,
        clauses: Vec<UpdateClause>,
        span: Span,
    },
    /// Resolved single-field in-place update (emitted by checker for simple `with`).
    /// VM evaluates as an arena write — no new cell allocation.
    SetField {
        receiver: Box<Expr>,
        field_idx: usize,
        value: Box<Expr>,
        span: Span,
    },
    /// In-place multi-step write into a region (D057/D058).
    /// Emitted by the checker for method-body write statements (`path <- expr`
    /// over `self`) when the target type is resident. The VM walks `path` from
    /// the receiver handle's entry cell through link fields and writes in place.
    RegionWrite {
        receiver: Box<Expr>,
        path: Vec<usize>,
        value: Box<Expr>,
        span: Span,
    },
    Borrow {
        inner: Box<Expr>,
        span: Span,
    },
    Bind {
        name: String,
        value: Box<Expr>,
        body: Box<Expr>,
        span: Span,
    },
    /// Sequence: evaluate `first`, discard its value, then evaluate `second`.
    /// The block's value is `second`'s. Emitted for bare expression statements
    /// and method-body writes (§6.5 block bindings).
    Seq {
        first: Box<Expr>,
        second: Box<Expr>,
        span: Span,
    },
    If {
        arms: Vec<IfArm>,
        span: Span,
    },
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
        span: Span,
    },
    /// Before desugar; rewritten to nested `match` (§6.5).
    Do {
        stmts: Vec<DoStmt>,
        span: Span,
    },
    /// Persistent AST node for `[...]` literals (no desugaring; item 8, §6.7.2).
    ListLit {
        elems: Vec<Expr>,
        span: Span,
    },
    /// `(params -> body)` — lambda expression.
    /// Before desugar; rewritten to synthetic function + partial application (lambda lifting).
    Lambda {
        params: Vec<String>,
        body: Box<Expr>,
        span: Span,
    },
    /// `[generators | acc_inits? | guards -> result]` — comprehension.
    /// Before desugar; rewritten to map/filter/flatMap/fold + lambdas.
    Comprehension {
        generators: Vec<Generator>,
        accs: Vec<AccInit>,
        guards: Vec<Expr>,
        result: Box<Expr>,
        span: Span,
    },
    /// `(a, b, ...)` — product value expression.
    Product {
        elems: Vec<Expr>,
        span: Span,
    },
}

/// A generator in a comprehension: `x <- xs` or `x, y <- xs, ys`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Generator {
    /// Bound variable names.
    pub vars: Vec<String>,
    /// Source expressions (one per var for zip, one per generator for cartesian).
    pub sources: Vec<Expr>,
    pub span: Span,
}

/// An accumulator initialization in a fold comprehension: `acc = init`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccInit {
    pub name: String,
    pub init: Expr,
    pub span: Span,
}

/// One clause inside a `with` block (or the sole clause of a bare update).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateClause {
    /// `.a.b = expr` — leaf assignment.
    Leaf {
        /// Relative path segments (without dots), non-empty.
        path: Vec<String>,
        value: Expr,
        span: Span,
    },
    /// `.a.b` followed by a nested clause block (group header).
    Group {
        path: Vec<String>,
        clauses: Vec<UpdateClause>,
        span: Span,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IfArm {
    /// `None` means `_` catch-all (sugar for `True`).
    pub condition: Option<Expr>,
    pub body: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchArm {
    pub pattern: Pat,
    pub body: Expr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DoStmt {
    /// `x <- e` — monadic bind over `Option` (§6.5): short-circuits the block to
    /// `None` when `e` is `None`, otherwise binds `x` to the unwrapped value.
    /// Distinct from a plain `=` let-binding.
    Bind { name: String, expr: Expr },
    /// `return e` — the block's final value; desugars to `Some e` (§6.5).
    Return(Expr),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pat {
    Wildcard {
        span: Span,
    },
    Var {
        name: String,
        span: Span,
    },
    /// Constructor pattern `Name p1 ... pn` (including nullary).
    /// `qualifier` is the actor name for qualified patterns (e.g. `@std.Line`).
    Ctor {
        name: String,
        qualifier: Option<String>,
        args: Vec<Pat>,
        span: Span,
    },
    /// Native product destructuring `(p1, p2 [, …])` (§3.6 / syntax unification).
    /// Matches a `Pair`/`Triple`/heap-`Product` value; binds each element pattern
    /// against the corresponding field. Replaces the old builtin `Pair`/`Triple`
    /// ctor syntax. Arity is 2–7 (implementation limit, §VM): arity 2/3 map to the
    /// inline `Pair`/`Triple`, larger tuples to a heap-backed `Product`.
    Product {
        elems: Vec<Pat>,
        span: Span,
    },
    /// Literal pattern — pins a specific value in an `==`-supporting position
    /// (§9 literal-pattern extension). Used both as a constructor-field pin
    /// (`Cons (Op "!") rest`) and as a whole scrutinee in value match
    /// (`match name; "if" -> …`). Desugars to an `==` comparison.
    Literal {
        lit: LitPat,
        span: Span,
    },
}

/// A literal pattern value (§9): only `Int` and `String` have lexer tokens;
/// `Bool` literals are the `True`/`False` constructors, `Char` has no literal
/// syntax yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LitPat {
    Int(i64),
    Str(String),
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int { span, .. }
            | Expr::Str { span, .. }
            | Expr::Var { span, .. }
            | Expr::Upper { span, .. }
            | Expr::AtName { span, .. }
            | Expr::Self_ { span }
            | Expr::Unit { span }
            | Expr::Apply { span, .. }
            | Expr::BinOp { span, .. }
            | Expr::Field { span, .. }
            | Expr::RecordUpdate { span, .. }
            | Expr::SetField { span, .. }
            | Expr::RegionWrite { span, .. }
            | Expr::Borrow { span, .. }
            | Expr::Bind { span, .. }
            | Expr::Seq { span, .. }
            | Expr::If { span, .. }
            | Expr::Match { span, .. }
            | Expr::Do { span, .. }
            | Expr::ListLit { span, .. }
            | Expr::Lambda { span, .. }
            | Expr::Comprehension { span, .. }
            | Expr::Product { span, .. }
            | Expr::Send { span, .. } => *span,
        }
    }
}

impl UpdateClause {
    pub fn span(&self) -> Span {
        match self {
            UpdateClause::Leaf { span, .. } | UpdateClause::Group { span, .. } => *span,
        }
    }

    pub fn path(&self) -> &[String] {
        match self {
            UpdateClause::Leaf { path, .. } | UpdateClause::Group { path, .. } => path,
        }
    }
}
