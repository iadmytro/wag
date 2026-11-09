//! Internal type representation for monomorphic HM inference.

use std::collections::BTreeMap;
use std::fmt;

/// A monotype, possibly containing unification variables.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    /// Live unification metavariable. Exists only during inference; no exported
    /// type may contain one (A1b closedness invariant).
    Var(u32),
    /// Bound/quantified type variable: scheme quantifiers and datatype params.
    /// Opaque to substitution — never bound, never unified away. The index is
    /// local to the enclosing scheme (ctor schemes use 0..n; exported types use
    /// the global dense numbering assigned at assembly).
    Bound(u32),
    /// Nullary type name: `Int`, `Bool`, `Unit`, user type, …
    Con(String),
    /// Type application: `Option a`, `List a`, `Actor a`, `Pair a b`, …
    App(String, Vec<Ty>),
    /// Borrowed view `&T` (parameter positions).
    Borrow(Box<Ty>),
    /// Function type `t1, t2, … -> r` (flat, non-curried).
    Fun(Vec<Ty>, Box<Ty>),
    /// Application of a type constructor variable to arguments: `C E`, `Pair a b`.
    /// The u32 is the ctor-var index (bound in scheme space; live after instantiation).
    /// When unified with a concrete `App(name, args)`, the var is resolved via
    /// [`crate::unify::Subst::ctor_map`] and this node collapses to a plain `App`.
    CtorApp(u32, Vec<Ty>),
}

impl Ty {
    pub fn int() -> Self {
        Ty::Con("Int".into())
    }
    pub fn bool() -> Self {
        Ty::Con("Bool".into())
    }
    pub fn unit() -> Self {
        Ty::Con("Unit".into())
    }
    pub fn option(inner: Ty) -> Self {
        Ty::App("Option".into(), vec![inner])
    }
    pub fn list(inner: Ty) -> Self {
        Ty::App("List".into(), vec![inner])
    }
    /// Owned mutable pointer `Ref T` (linear, heap-resident).
    pub fn ref_(inner: Ty) -> Self {
        Ty::App("Ref".into(), vec![inner])
    }
    pub fn actor(inner: Ty) -> Self {
        Ty::App("Actor".into(), vec![inner])
    }
    pub fn pair(a: Ty, b: Ty) -> Self {
        Ty::App("Pair".into(), vec![a, b])
    }
    pub fn triple(a: Ty, b: Ty, c: Ty) -> Self {
        Ty::App("Triple".into(), vec![a, b, c])
    }

    pub fn head_name(&self) -> Option<&str> {
        match self {
            Ty::Con(n) => Some(n.as_str()),
            Ty::App(n, _) => Some(n.as_str()),
            Ty::Borrow(t) => t.head_name(),
            _ => None,
        }
    }

    /// Strip one outer borrow layer if present.
    pub fn strip_borrow(&self) -> &Ty {
        match self {
            Ty::Borrow(t) => t,
            other => other,
        }
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Var(i) => write!(f, "?{i}"),
            Ty::Bound(i) => write!(f, "t{i}"),
            Ty::Con(n) => write!(f, "{n}"),
            Ty::App(n, args) => {
                write!(f, "{n}")?;
                for a in args {
                    write!(f, " {a}")?;
                }
                Ok(())
            }
            Ty::Borrow(t) => write!(f, "&{t}"),
            Ty::Fun(ps, r) => {
                if ps.is_empty() {
                    write!(f, "() -> {r}")
                } else {
                    for (i, p) in ps.iter().enumerate() {
                        if i > 0 {
                            write!(f, ", ")?;
                        }
                        // Parenthesize nested functions.
                        match p {
                            Ty::Fun(_, _) => write!(f, "({p})")?,
                            _ => write!(f, "{p}")?,
                        }
                    }
                    write!(f, " -> {r}")
                }
            }
            Ty::CtorApp(v, args) => {
                write!(f, "?cv{v}")?;
                for a in args {
                    write!(f, " {a}")?;
                }
                Ok(())
            }
        }
    }
}

/// A typeclass constraint: the class `class_name` must be implemented by the
/// type constructor identified by `ctor_var` (index into the scheme's quantified
/// ctor vars). Resolved at call sites by matching the argument's head constructor
/// against registered instances (§3.12, D075).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Constraint {
    pub class_name: String,
    /// Index of the type-constructor variable in the enclosing scheme.
    pub ctor_var: u32,
}

/// Polymorphic type scheme for built-in constructors/primitives only.
/// User bindings are monomorphic (§3.2).
#[derive(Debug, Clone)]
pub struct Scheme {
    /// Bound variable ids (quantified).
    pub vars: Vec<u32>,
    pub body: Ty,
    /// Typeclass constraints (D075). Empty for non-class schemes.
    pub constraints: Vec<Constraint>,
}

impl Scheme {
    pub fn mono(ty: Ty) -> Self {
        Scheme {
            vars: vec![],
            body: ty,
            constraints: vec![],
        }
    }

    /// Create a scheme with explicit quantified vars and typeclass constraints (D075).
    pub fn constrained(vars: Vec<u32>, body: Ty, constraints: Vec<Constraint>) -> Self {
        Scheme {
            vars,
            body,
            constraints,
        }
    }
}

/// Constructor entry in the program-wide table.
#[derive(Debug, Clone)]
pub struct CtorInfo {
    pub name: String,
    /// Result type head name (`Option`, `List`, `CounterMsg`, …).
    pub type_name: String,
    /// Field types (monomorphic for user ctors; may contain declaration-space
    /// param vars — read them via [`CtorInfo::field_ty`], never raw).
    pub fields: Vec<FieldInfo>,
    /// Number of declared type params: `Var(0..n_params)` in field types and the
    /// scheme are placeholders for the receiver's actual args (A1b invariant).
    pub n_params: usize,
    /// Optional polymorphic scheme: ctor as a function `fields -> result`.
    pub scheme: Scheme,
}

impl CtorInfo {
    /// Field `i`'s type with the receiver's actual args substituted for its
    /// declared params (`Var(0..n_params)` → `args[0..]`). Params beyond
    /// `args.len()` stay raw declaration-space vars — a conservative fallback
    /// for callers without a fully applied type. This is the only sanctioned
    /// way to read field types from outside this crate (A1b invariant: no
    /// resolution step may emit unbound placeholder vars).
    pub fn field_ty(&self, i: usize, args: &[Ty]) -> Ty {
        subst_decl_args(&self.fields[i].ty, args)
    }
}

/// Substitute declaration-space params (`Bound(0..n)`) with the applied type
/// arguments. Intended for declared field types; a param index beyond `args.len()`
/// is left as-is (the conservative fallback documented on [`CtorInfo::field_ty`]).
fn subst_decl_args(ty: &Ty, args: &[Ty]) -> Ty {
    match ty {
        // Declaration-space param (A1b 3b): Bound(k) → k-th applied arg.
        Ty::Bound(b) => args
            .get(*b as usize)
            .cloned()
            .unwrap_or(Ty::Bound(*b)),
        // A live metavar in declaration space is a leak (A1b); leave it so the
        // closedness validator can flag it.
        Ty::Var(v) => Ty::Var(*v),
        Ty::Con(n) => Ty::Con(n.clone()),
        Ty::App(n, as_) => Ty::App(
            n.clone(),
            as_.iter().map(|a| subst_decl_args(a, args)).collect(),
        ),
        Ty::Borrow(t) => Ty::Borrow(Box::new(subst_decl_args(t, args))),
        Ty::Fun(ps, r) => Ty::Fun(
            ps.iter().map(|p| subst_decl_args(p, args)).collect(),
            Box::new(subst_decl_args(r, args)),
        ),
        Ty::CtorApp(v, as_) => Ty::CtorApp(
            *v,
            as_.iter().map(|a| subst_decl_args(a, args)).collect(),
        ),
    }
}

#[derive(Debug, Clone)]
pub struct FieldInfo {
    pub label: Option<String>,
    /// Declaration-space type — crate-internal; external readers must go through
    /// [`CtorInfo::field_ty`] (A1b invariant).
    pub(crate) ty: Ty,
}

impl FieldInfo {
    pub fn new(label: Option<String>, ty: Ty) -> Self {
        FieldInfo { label, ty }
    }
}

/// Per-ctor VM metadata derived once at type-check time (A1b invariant):
/// downstream consumers read this instead of declaration-space field types.
#[derive(Debug, Clone, Default)]
pub struct CtorMeta {
    /// `field_is_ref[i]`: declared type of field `i` is directly `Ref …`.
    pub field_is_ref: Vec<bool>,
}

/// Map type-name → constructors belonging to it.
pub type TypeCtors = BTreeMap<String, Vec<String>>;
