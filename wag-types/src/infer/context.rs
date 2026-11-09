//! Context: inference state (type vars, env, ctors, per-module state).

use std::collections::{BTreeMap, BTreeSet, HashMap};

use wag_syntax::{Program, Span};

use crate::error::{OriginInfo, Stage, TypeError};
use crate::types::{Constraint, CtorInfo, Scheme, Ty, TypeCtors};
use crate::unify::{pretty_ty, unify, Subst};

use super::util::rename_vars;

pub(super) struct Context {
    pub(super) next_var: u32,
    pub(super) subst: Subst,
    /// (name, arity) → scheme. Local vars/params are arity 0; top-level
    /// functions keyed by param count (§3.11 / D052).
    pub(super) env: HashMap<(String, usize), Scheme>,
    pub(super) ctors: BTreeMap<String, CtorInfo>,
    pub(super) type_ctors: TypeCtors,
    /// Type alias map: alias name → type expression (resolved before inference).
    pub(super) aliases: HashMap<String, wag_syntax::Type>,
    /// Currently inside an @-declaration (for self typing).
    pub(super) actor_self: Option<Ty>,
    /// The current actor's `@`-property list (without `@`), set while typing
    /// its body. Drives the outbox pattern/exhaustiveness rules (D043).
    pub(super) cur_props: Vec<String>,
    /// Name of the current actor's Msg type (Con name), set while typing its
    /// body. Outbox exhaustiveness applies only to matches on this type.
    pub(super) cur_msg: Option<String>,
    /// Top-level bindings that are NOT in a mutual-recursion cycle (singleton SCC).
    /// These are eligible for generalization (§3.5, Option A).
    pub(super) singleton_sccs: std::collections::HashSet<String>,
    /// Match scrutinee types: span start → type name (for exhaustiveness in wag-check).
    pub(super) match_types: HashMap<Span, String>,
    /// Expression-type side table: `(item name, node span) → resolved type`.
    /// Recorded during inference so downstream ownership checks (`wag-check`
    /// linearity) consume the inferred type instead of re-deriving it
    /// heuristically. Item names are unique per program (duplicates are rejected
    /// intra-module and cross-module), node spans are unique within one source
    /// file, so the pair is globally unambiguous even when prelude/user span
    /// ranges overlap.
    pub(super) expr_types: HashMap<(String, Span), Ty>,
    /// Canonical name of the item whose body is currently being inferred
    /// (`"@name"` for actors). `None` outside item bodies; expressions are only
    /// inferred inside func/actor bodies, so this gates recording.
    pub(super) cur_item: Option<String>,
    /// Inferred method types: `(type_name, method name) → full Fun type`
    /// (D056/D057). Accumulated across modules; moved into
    /// `TypedProgram.method_tys` at the end.
    pub(super) method_tys: BTreeMap<(String, String), Ty>,
    /// Receiver type while a method body is being inferred (D057): head name
    /// plus the var indices of its declared params (empty when nullary).
    /// Bare atoms resolve against its fields and `self` denotes the handle.
    pub(super) method_receiver: Option<(String, Vec<u32>)>,
    /// Declared param names per parametric datatype (D061): used to bind the
    /// receiver's params when inferring/seedling method signatures.
    pub(super) type_params: HashMap<String, Vec<String>>,
    /// Method call table for surface dispatch (`l.append 42`, D056): receiver
    /// type name → method name → scheme of `Fun(recv :: ps, r)` — the
    /// instantiated receiver is always the first parameter (D061).
    pub(super) methods_table: BTreeMap<String, BTreeMap<String, Scheme>>,
    /// Operator → class name index (e.g. `"+"` → `"Add"`). Populated from
    /// class declarations where a method name is an operator symbol.
    pub(super) op_class_index: BTreeMap<String, String>,
    /// Class name → list of (instance type, impl func name). Populated from
    /// instance declarations. Used by `dispatch_binop` to find the matching
    /// implementation for a given operand type.
    pub(super) class_instances: BTreeMap<String, Vec<(Ty, String)>>,
    /// Schemes of synthetic op implementations (`__op_*`). Accumulated across
    /// modules and never reset: synthetic names are internal machinery, not
    /// source-level exports, so the per-module import env cannot carry them.
    pub(super) op_impl_env: BTreeMap<(String, usize), Scheme>,
    /// Resolved operator dispatch decisions: BinOp span → selected impl name.
    /// Consumed by the loader (wag-interp), which rewrites dispatched use-sites
    /// into applications of the overload body. Empty for native entries.
    pub(super) op_dispatch: HashMap<Span, String>,
    /// Metavars that may legitimately remain **free** at inference end and get
    /// generalized into a scheme: signature type vars (explicit `T`, `U`) and
    /// per-call-site instantiation vars. Everything else must be bound by the
    /// final substitution — the A1b closedness invariant (see [`super::validate`]).
    /// Cumulative across modules: metavar indices are globally unique.
    pub(super) expected_free: BTreeSet<u32>,
    /// Field accesses whose receiver was still a hole at inference time
    /// (Option C, A1b step 6): re-resolved against the concrete receiver after
    /// all items of the module are inferred. Per-module state.
    pub(super) deferred_fields: Vec<DeferredField>,
    /// Constraints accumulated from constrained function calls during the
    /// current body inference (D075, §3.12). Collected and attached to the
    /// resulting scheme in `infer_func`.
    pub(super) active_constraints: Vec<Constraint>,
}

/// One deferred field access (A1b step 6). `recv` is the receiver hole as seen
/// at inference time; `result` is the fresh result hole handed to the caller —
/// re-resolution unifies it with the field's actual type once `recv` is known.
pub(super) struct DeferredField {
    pub span: Span,
    pub recv: Ty,
    pub field: String,
    pub result: Ty,
}

impl Context {
    pub(super) fn new() -> Self {
        Context {
            next_var: 0,
            subst: Subst::new(),
            env: HashMap::new(),
            ctors: BTreeMap::new(),
            type_ctors: BTreeMap::new(),
            aliases: HashMap::new(),
            actor_self: None,
            cur_props: Vec::new(),
            cur_msg: None,
            singleton_sccs: std::collections::HashSet::new(),
            match_types: HashMap::new(),
            expr_types: HashMap::new(),
            cur_item: None,
            method_tys: BTreeMap::new(),
            method_receiver: None,
            type_params: HashMap::new(),
            methods_table: BTreeMap::new(),
            op_class_index: BTreeMap::new(),
            class_instances: BTreeMap::new(),
            op_impl_env: BTreeMap::new(),
            op_dispatch: HashMap::new(),
            expected_free: BTreeSet::new(),
            deferred_fields: Vec::new(),
            active_constraints: Vec::new(),
        }
    }

    /// Reset all per-module state (leaves global state: `next_var`, `ctors`, `type_ctors` intact).
    /// Call before inferring each module in a multi-module program.
    /// Clears only bindings — recorded provenance is kept, because metavar indices
    /// are globally unique and a metavar from an earlier module can still surface
    /// free in a later module's error (§7 provenance).
    pub(super) fn reset_module_state(&mut self) {
        self.subst.clear_bindings();
        self.actor_self = None;
        self.method_receiver = None;
        self.cur_props = Vec::new();
        self.cur_msg = None;
        self.singleton_sccs.clear();
        self.deferred_fields.clear();
    }

    /// Allocate a fresh metavariable and record what introduced it, so free-var
    /// letters in error messages can be explained to the user (§7 provenance).
    pub(super) fn fresh_with(&mut self, origin: OriginInfo) -> Ty {
        let v = self.next_var;
        self.next_var += 1;
        self.subst.record_origin(v, origin);
        Ty::Var(v)
    }

    /// Dump the full inference state (program + env + subst) to stderr, for
    /// debugging type errors when `WAG_DEBUG` is set.
    pub(super) fn dump_debug(&self, program: &Program, err: &TypeError) {
        eprintln!("\n===== WAG_DEBUG: type inference context =====");
        eprintln!("error: {err}");
        eprintln!("\n--- program (post-desugar, post-lift) ---");
        eprintln!("{}", wag_syntax::pretty_program(program));

        eprintln!("\n--- type environment ((name, arity) -> resolved type) ---");
        let mut keys: Vec<&(String, usize)> = self.env.keys().collect();
        keys.sort();
        for key in keys {
            let (name, arity) = key;
            let scheme = &self.env[key];
            let qv: Vec<String> = scheme.vars.iter().map(|v| format!("?{v}")).collect();
            let quant = if qv.is_empty() {
                String::new()
            } else {
                format!("forall [{}]: ", qv.join(", "))
            };
            eprintln!(
                "  ({name}, {arity})  =  {quant}{}",
                pretty_ty(&scheme.body, &self.subst)
            );
        }

        eprintln!("\n--- substitution (raw) ---");
        let _ = dbg!(&self.subst);
    }

    pub(super) fn instantiate(&mut self, scheme: &Scheme) -> Ty {
        if scheme.vars.is_empty() && scheme.constraints.is_empty() {
            return scheme.body.clone();
        }
        // Ctor vars (from class constraints) are resolved during unification at
        // call sites, not instantiated to fresh type variables (D075, §3.12).
        let ctor_var_idxs: BTreeSet<u32> =
            scheme.constraints.iter().map(|c| c.ctor_var).collect();
        let mut map = HashMap::new();
        for &v in &scheme.vars {
            if ctor_var_idxs.contains(&v) {
                continue;
            }
            // Reuse the origin recorded when this type variable was introduced, so
            // instantiated (per-call) metavariables stay self-describing instead of
            // leaking a bare letter (§7 provenance).
            let origin = self
                .subst
                .origin_of(v)
                .cloned()
                .unwrap_or_else(|| OriginInfo::anonymous(Span::empty(0)));
            let inst = self.fresh_with(origin);
            if let Ty::Var(iv) = &inst {
                // Instantiation vars may stay free (e.g. `a = identity`); the
                // validator must not flag them as leaks (A1b).
                self.expected_free.insert(*iv);
            }
            map.insert(v, inst);
        }
        // Record constraints from constrained schemes (D075): they propagate to
        // the enclosing function's scheme via `active_constraints`.
        for c in &scheme.constraints {
            self.active_constraints.push(c.clone());
        }
        rename_vars(&scheme.body, &map)
    }

    /// Bind an instantiated ctor scheme against the receiver's actual type and
    /// return `(field types, result type)`, all resolved through the current
    /// subst. This is the only sanctioned way to read field types out of a ctor
    /// scheme — instantiate → unify result vs `binder_ty` → apply subst to
    /// fields — so no resolution step can emit unbound placeholder vars (A1b
    /// invariant). Pass `None` as binder for outbox arms, which are not unified
    /// with the scrutinee (spec §5.5.7); field types then stay fresh live holes.
    pub(super) fn bind_ctor_scheme(
        &mut self,
        scheme: &Scheme,
        binder_ty: Option<&Ty>,
        span: Span,
    ) -> Result<(Vec<Ty>, Ty), TypeError> {
        let inst = self.instantiate(scheme);
        // inst is either `Fun(fields) -> Result` or a nullary result.
        let (field_tys, result) = match inst {
            Ty::Fun(ps, r) => (ps, *r),
            r => (vec![], r),
        };
        if let Some(binder) = binder_ty {
            unify(&mut self.subst, binder, &result, span)?;
        }
        Ok((
            field_tys.into_iter().map(|t| self.subst.apply(&t)).collect(),
            self.subst.apply(&result),
        ))
    }

    /// Receiver var indices for a method on a parametric datatype (D061): one
    /// per declared param of `type_name` — shared with the signature's
    /// `var_map` when the name occurs there, fresh (implicitly quantified)
    /// otherwise. Shared by the `methods_table` seeder and `infer_method` so
    /// both always agree (A1b step 4).
    pub(super) fn receiver_var_idxs(
        &mut self,
        type_name: &str,
        label: &str,
        span: Span,
        var_map: &HashMap<String, u32>,
    ) -> Vec<u32> {
        let mut out = Vec::new();
        for pname in self.type_params.get(type_name).cloned().unwrap_or_default() {
            if let Some(&v) = var_map.get(&pname) {
                out.push(v);
            } else {
                let Ty::Var(v) = self.fresh_with(OriginInfo::new(
                    format!("receiver param `{pname}` of `{label}`"),
                    span,
                )) else {
                    unreachable!("fresh_with always yields a Var")
                };
                // Implicitly quantified receiver params may stay free (A1b).
                self.expected_free.insert(v);
                out.push(v);
            }
        }
        out
    }

    /// Look up a bare variable reference — D052.
    /// First tries `(name, 0)` (local vars/params), then falls back to any arity
    /// (top-level functions are keyed by their param count).
    pub(super) fn lookup(&mut self, name: &str, span: Span) -> Result<Ty, TypeError> {
        let scheme = if let Some(s) = self.env.get(&(name.to_string(), 0)) {
            s.clone()
        } else {
            // Fall back: search for any entry with this name (top-level function).
            self.env
                .iter()
                .find(|((n, _), _)| n == name)
                .map(|(_, s)| s.clone())
                .ok_or_else(|| {
                    TypeError::new(Stage::Inference, span, format!("unbound variable `{name}`"))
                })?
        };
        let ty = self.instantiate(&scheme);
        // §3.1 value/computation unity: a 0-arg function is its result.
        if let Ty::Fun(ref params, ref ret) = ty {
            if params.is_empty() {
                return Ok(ret.as_ref().clone());
            }
        }
        Ok(ty)
    }

    /// Look up a name at a specific arity (for Apply resolution) — D052.
    pub(super) fn lookup_arity(
        &mut self,
        name: &str,
        arity: usize,
        span: Span,
    ) -> Result<Ty, TypeError> {
        let scheme = self
            .env
            .get(&(name.to_string(), arity))
            .cloned()
            .ok_or_else(|| {
                if arity == 0 {
                    TypeError::new(Stage::Inference, span, format!("unbound variable `{name}`"))
                } else {
                    TypeError::new(
                        Stage::Inference,
                        span,
                        format!("no overload of `{name}` at arity {arity}"),
                    )
                }
            })?;
        Ok(self.instantiate(&scheme))
    }

    /// Resolve an `@name` actor reference (D078).
    /// System actors keep a handle at arity 0; user actors expose the
    /// creation function at arity 1. The behavior entry (arity ≥ 2) is
    /// internal and never resolved here.
    pub(super) fn lookup_actor(&mut self, name: &str, span: Span) -> Result<Ty, TypeError> {
        let key = format!("@{name}");
        if let Some(scheme) = self.env.get(&(key.clone(), 0)).cloned() {
            return Ok(self.instantiate(&scheme));
        }
        if let Some(scheme) = self.env.get(&(key, 1)).cloned() {
            return Ok(self.instantiate(&scheme));
        }
        Err(TypeError::new(
            Stage::Inference,
            span,
            format!("unbound actor `@{name}`"),
        ))
    }
}
