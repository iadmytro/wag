//! Public API: TypedProgram, infer_program, infer_multi_module, ModuleInput.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use wag_syntax::{ActorDef, ConstructorDecl, FuncDef, Item, MethodDef, Program, Span};

use crate::error::{OriginInfo, Stage, TypeError};
use crate::types::{CtorInfo, CtorMeta, Scheme, Ty, TypeCtors};

use crate::unify::{pretty_ty, unify};

use super::context::{Context, DeferredField};
use super::infer_expr::deref_ref;
use super::topo::{build_call_graph, is_in_cycle, topo_infer_order};

/// Build a `Type` from a constructor's field types (juxtaposed application).
/// Used to reclassify single-ctor parametric types as aliases when the ctor
/// name is an existing type (§3.2 disambiguation: deferred resolution).
fn ctor_fields_to_type(ctor: &ConstructorDecl) -> wag_syntax::Type {
    use wag_syntax::Type;
    if ctor.fields.is_empty() {
        Type::Name(ctor.name.clone())
    } else {
        let args: Vec<Type> = ctor.fields.iter().map(|f| f.ty.clone()).collect();
        Type::App(Box::new(Type::Name(ctor.name.clone())), args)
    }
}

/// Determine whether a single-constructor, non-parametric `ParametricTypeDecl`
/// should be reclassified as a type alias. This is the case when the ctor name
/// is already a known type name in `type_ctors` (e.g. `String = Arr Int`).
fn is_alias_reclassification(pt: &wag_syntax::ParametricTypeDecl, ctx: &Context) -> bool {
    pt.type_vars.is_empty()
        && pt.ctors.len() == 1
        && ctx.type_ctors.contains_key(&pt.ctors[0].name)
}

/// Expand aliased types inside all builtin env schemes and ctor field types so
/// they agree with declaration-site expansion (§3.2 transparency). Builtin
/// registrations name `String` directly; once a program aliases it, every
/// builtin scheme/ctor must see the expansion too, or literals/patterns
/// (which do expand) would mismatch them.
fn expand_builtin_aliases(ctx: &mut Context) -> Result<(), TypeError> {
    for scheme in ctx.env.values_mut() {
        scheme.body = super::util::expand_ty_aliases(&scheme.body, &ctx.aliases, 0)?;
    }
    for ctor in ctx.ctors.values_mut() {
        for f in &mut ctor.fields {
            f.ty = super::util::expand_ty_aliases(&f.ty, &ctx.aliases, 0)?;
        }
        ctor.scheme.body = super::util::expand_ty_aliases(&ctor.scheme.body, &ctx.aliases, 0)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct TypedProgram {
    /// Composite-keyed function/actor types: `(name, arity) → Ty`.
    /// Functions keyed by param count; an actor contributes its behavior under
    /// `("@name", arity)` and its handle under `("@name", 0)`. Actors live in a
    /// separate namespace from functions (§2: `@foo` and `foo` are distinct atoms),
    /// so a func `std` and an actor `@std` may coexist.
    pub bindings: BTreeMap<(String, usize), Ty>,
    /// Constructor table (name → info), including builtins.
    pub constructors: BTreeMap<String, CtorInfo>,
    /// type name → constructor names.
    pub type_ctors: TypeCtors,
    /// Actor name (no `@`) → Actor MsgTy. System predecls included (`Std`, `Net`, …).
    pub actors: BTreeMap<String, Ty>,
    /// Actor name → outbox constructor names (D043).
    /// Used by `wag-check` exhaustiveness to require outbox arms.
    pub actor_outbox: BTreeMap<String, Vec<String>>,
    /// Match scrutinee types: the match expression's full `Span` → its resolved type
    /// name (only for concrete Con/App scrutinees). Populated during inference; consumed
    /// by `wag-check` exhaustiveness. Keyed by the whole span — not just `.start` —
    /// because prelude and user items are merged with overlapping span ranges, so two
    /// distinct matches can share a start offset but never an exact (start, end) pair.
    /// A missing entry means either a type-variable scrutinee or a compiler-generated
    /// (desugared field-access) match; both skip the check.
    pub match_types: HashMap<Span, String>,
    /// Expression-type side table: `(item name, node span) → resolved type`,
    /// recorded during inference (§3.2). Item names are unique per program and
    /// node spans are unique within one source file, so the pair is globally
    /// unambiguous even where prelude/user span ranges overlap. Field-access
    /// nodes record the ownership view — `&T` for non-pointer-free results
    /// (§3.3/§4.3.3 borrowed field views) — not the bare unified type.
    /// Consumed by `wag-check` linearity (rule 10/11)
    /// instead of heuristic re-derivation. Desugared synthetic subtrees reuse
    /// their root source node's span, so a lookup at the replacement subtree's
    /// root returns the source node's recorded type.
    pub expr_types: HashMap<(String, Span), Ty>,
    /// Region-residency per declared datatype/alias (D054): `name → resident?`.
    /// Computed over surface declarations before inference; consumed by the checker
    /// and VM region machinery.
    pub residency: BTreeMap<String, bool>,
    /// Method types: `(type_name, method name) → full Fun type` (D056/D057).
    /// Methods are not top-level bindings — they don't appear in `bindings`, the
    /// call graph, or export lists.
    pub method_tys: BTreeMap<(String, String), Ty>,
    /// Declared param names per parametric datatype (D061): lets checker/VM
    /// consumers rebuild a receiver's applied type (`App(name, […])`) for
    /// methods on parametric types.
    pub type_params: BTreeMap<String, Vec<String>>,
    /// Pointer-free per declared type name (§3.2), derived once (A1b invariant):
    /// checker/VM consumers read this table instead of walking declaration-space
    /// field types. See `crate::pf`.
    pub pointer_free_types: BTreeMap<String, bool>,
    /// Per-ctor VM metadata (`field_is_ref`), derived once (A1b invariant).
    pub ctor_meta: BTreeMap<String, CtorMeta>,
    /// Resolved operator dispatch decisions: BinOp span → selected impl name.
    /// Consumed by the loader to rewrite dispatched use-sites into applications.
    pub op_dispatch: HashMap<Span, String>,
}

pub fn infer_program(program: &Program) -> Result<TypedProgram, TypeError> {
    let mut ctx = Context::new();
    match infer_program_impl(program, &mut ctx) {
        Ok(tp) => Ok(tp),
        Err(e) => {
            if wag_syntax::debug_enabled() {
                ctx.dump_debug(program, &e);
            }
            Err(e)
        }
    }
}

/// Driver: global setup (once per program) then per-module inference. Today a
/// program is one module; the module loop is the seam the module system
/// will extend to multiple modules in DAG order (D044 extended).
fn infer_program_impl(program: &Program, ctx: &mut Context) -> Result<TypedProgram, TypeError> {
    // Collect non-parametric constructors for registration.
    let user_ctors: Vec<ConstructorDecl> = program
        .items
        .iter()
        .filter_map(|i| match i {
            Item::Constructor(c) => Some(c.clone()),
            _ => None,
        })
        .collect();

    // Namespacing check covers ALL constructors (including parametric).
    // setup_global no longer re-checks; this is the single namespacing gate.
    let all_ctors: Vec<(String, ConstructorDecl)> = program
        .constructors()
        .iter()
        .map(|c| ("main".to_string(), (*c).clone()))
        .collect();
    ctx.check_namespacing(&all_ctors)?;

    // Collect type aliases (resolved before inference).
    for item in &program.items {
        if let Item::TypeAlias(a) = item {
            ctx.aliases.insert(a.name.clone(), a.ty.clone());
        }
    }

    setup_global(&user_ctors, ctx)?;

    // Register parametric types (polymorphic constructors).
    // §3.2 disambiguation: a single-ctor, non-parametric type whose ctor name
    // is an existing type name is reclassified as a type alias.
    for item in &program.items {
        if let Item::ParametricType(pt) = item {
            if is_alias_reclassification(pt, ctx) {
                let ty = ctor_fields_to_type(&pt.ctors[0]);
                ctx.aliases.insert(pt.name.clone(), ty);
            } else {
                ctx.add_parametric_type(pt)?;
            }
        }
    }

    // Eagerly validate all aliases: resolve each to detect cycles.
    for (name, ty) in &ctx.aliases {
        super::util::resolve_aliases(ty, &ctx.aliases, 0).map_err(|mut e| {
            e.message = format!("type alias `{name}`: {}", e.message);
            e
        })?;
    }

    // Builtin schemes/ctors name aliased types directly (e.g. `String`);
    // expand them so they agree with declaration-site expansion (§3.2).
    expand_builtin_aliases(ctx)?;

    let (raw_bindings, mut actor_tys) = infer_module(&program.items, ctx)?;
    resolve_deferred_fields(ctx)?;
    finalize_expr_types(ctx);
    validate_closedness(ctx, &raw_bindings, &actor_tys)?;
    // Convert schemes to concrete types, preserving composite keys.
    let mut bindings: BTreeMap<(String, usize), Ty> = raw_bindings
        .into_iter()
        .map(|((name, arity), scheme)| ((name, arity), ctx.subst.apply(&scheme.body)))
        .collect();
    let actor_outbox = collect_actor_outbox(&program.items);
    let residency = crate::resident::compute_residency(program);
    // A1b: derive VM metadata once (pointer-free table + per-ctor field_is_ref).
    let pointer_free_types = crate::pf::compute_pointer_free(&ctx.ctors);
    let ctor_meta = derive_ctor_meta(&ctx.ctors);
    let mut method_tys = std::mem::take(&mut ctx.method_tys)
        .into_iter()
        .map(|(k, ty)| (k, ctx.subst.apply(&ty)))
        .collect();
    let mut expr_types = std::mem::take(&mut ctx.expr_types);
    // A1b 3c: global dense renumbering + structural no-Var backstop.
    renumber_exported(
        &mut bindings,
        &mut method_tys,
        &mut expr_types,
        &mut actor_tys,
        &ctx.expected_free,
    )?;
    Ok(TypedProgram {
        bindings,
        constructors: ctx.ctors.clone(),
        type_ctors: ctx.type_ctors.clone(),
        actors: actor_tys,
        actor_outbox,
        match_types: ctx.match_types.clone(),
        expr_types,
        residency,
        method_tys,
        type_params: ctx
            .type_params
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        pointer_free_types,
        ctor_meta,
        op_dispatch: ctx.op_dispatch.clone(),
    })
}

/// A1b invariant (always on): no resolution step may emit a type containing
/// unbound placeholder vars. Every live metavar must be bound by the final
/// substitution or belong to an expected-free class that gets generalized.
/// See [`super::validate`].
fn validate_closedness(
    ctx: &Context,
    raw_bindings: &BTreeMap<(String, usize), Scheme>,
    actor_tys: &BTreeMap<String, Ty>,
) -> Result<(), TypeError> {
    let errs = super::validate::validate(ctx, raw_bindings, actor_tys);
    if !errs.is_empty() {
        // Anchor at the first offending metavar's origin so the diagnostic
        // points at the code that introduced the hole; full detail in context.
        return Err(
            TypeError::new(
                Stage::Inference,
                errs[0].span,
                format!("cannot infer complete types for {} exported binding(s)/type(s)", errs.len()),
            )
            .with_context(format!("closedness violation(s): {}", errs.iter().map(|e| e.msg.clone()).collect::<Vec<_>>().join("; "))),
        );
    }
    Ok(())
}

/// A1b 3c: global dense renumbering at assembly. After inference, every free
/// `Var(v)` left in the exported tables is rewritten to `Bound(dense)` via one
/// shared map; a structural check then proves no `Ty::Var` survives anywhere.
/// The pre-conversion semantic checks (`check_generalized`, closedness) must run
/// first — after conversion, leaked body metavars would be indistinguishable
/// from signature vars. Ctor tables need no conversion (Bound-only, self-
/// contained local numbering).
///
/// Phase-boundary invariant: after `validate_closedness` passes, every free
/// metavar in the exported tables is expected-free (signature/instantiation).
/// Anything else is a compiler bug — fail hard *before* conversion, where it
/// would become indistinguishable from a legitimate signature var.
fn renumber_exported(
    bindings: &mut BTreeMap<(String, usize), Ty>,
    method_tys: &mut BTreeMap<(String, String), Ty>,
    expr_types: &mut HashMap<(String, Span), Ty>,
    actors: &mut BTreeMap<String, Ty>,
    expected_free: &BTreeSet<u32>,
) -> Result<(), TypeError> {
    // Types are already final-subst-applied at this point — no subst arg needed.
    let mut vars = Vec::new();
    for ty in bindings
        .values()
        .chain(method_tys.values())
        .chain(expr_types.values())
        .chain(actors.values())
    {
        vars.extend(crate::unify::free_var_indices(ty));
    }
    vars.sort();
    vars.dedup();
    let unexpected: Vec<u32> = vars.iter().copied().filter(|v| !expected_free.contains(v)).collect();
    if !unexpected.is_empty() {
        return Err(TypeError::new(
            Stage::Inference,
            Span::new(0, 0),
            format!(
                "internal error (compiler bug): exported types contain unexpected metavar(s) {:?} — please report",
                unexpected
            ),
        ));
    }
    // Dense map: sorted unique old → 0..n-1.
    let map: BTreeMap<u32, u32> = vars.iter().enumerate().map(|(i, &v)| (v, i as u32)).collect();

    for ty in bindings.values_mut() {
        *ty = crate::unify::var_to_bound(ty, &map);
    }
    for ty in method_tys.values_mut() {
        *ty = crate::unify::var_to_bound(ty, &map);
    }
    for ty in expr_types.values_mut() {
        *ty = crate::unify::var_to_bound(ty, &map);
    }
    for ty in actors.values_mut() {
        *ty = crate::unify::var_to_bound(ty, &map);
    }

    // Post-conversion backstop (always on): no `Ty::Var` may remain anywhere.
    let mut leaked: Vec<u32> = Vec::new();
    for ty in bindings
        .values()
        .chain(method_tys.values())
        .chain(expr_types.values())
        .chain(actors.values())
    {
        leaked.extend(crate::unify::free_var_indices(ty));
    }
    if !leaked.is_empty() {
        return Err(TypeError::new(
            Stage::Inference,
            Span::new(0, 0),
            format!("internal error: unbound metavar(s) {:?} survived renumbering", leaked),
        ));
    }
    Ok(())
}

/// A1b: per-ctor `field_is_ref` from declared field types (direct `Ref …` test).
fn derive_ctor_meta(ctors: &BTreeMap<String, CtorInfo>) -> BTreeMap<String, CtorMeta> {
    ctors
        .iter()
        .map(|(name, c)| {
            (
                name.clone(),
                CtorMeta {
                    field_is_ref: c.fields.iter().map(|f| matches!(f.ty, Ty::Borrow(_))).collect(),
                },
            )
        })
        .collect()
}

/// Option C (A1b step 6): re-resolve field accesses whose receiver was still a
/// hole at inference time. Runs after all items of the module are inferred, so
/// the final substitution has settled every receiver that can settle: apply it,
/// resolve the field against the concrete receiver (`bind_ctor_scheme` via
/// `resolve_field_on`, or the `.last` List rule), and unify the deferred result
/// hole with the actual field type. Entries are processed to a fixpoint because
/// one entry's resolution can bind another entry's receiver (mutual recursion:
/// parameter holes alias earlier entries' result holes). A receiver that is
/// still a hole after a no-progress pass was never constrained (e.g. an
/// unannotated binding whose parameter is generalized at call sites) — error
/// rather than silently generalize unsoundly (§4).
fn resolve_deferred_fields(ctx: &mut Context) -> Result<(), TypeError> {
    let mut pending = std::mem::take(&mut ctx.deferred_fields);
    while !pending.is_empty() {
        let n = pending.len();
        let mut next = Vec::with_capacity(n);
        for d in pending.drain(..) {
            if re_resolve_one(ctx, &d)? {
                next.push(d); // receiver still a hole — retry on a later pass
            }
        }
        if next.len() == n {
            // No progress: every remaining receiver is an unresolvable hole.
            let d = &next[0];
            return Err(TypeError::new(
                Stage::Inference,
                d.span,
                format!(
                    "cannot infer receiver type of field `.{}` — add a signature to the binding",
                    d.field
                ),
            ));
        }
        pending = next;
    }
    Ok(())
}

/// Re-resolve one deferred access. Returns `Ok(true)` when the receiver was
/// still a hole (retry later), `Ok(false)` when resolved, or an error for a
/// concrete-but-incompatible receiver.
fn re_resolve_one(ctx: &mut Context, d: &DeferredField) -> Result<bool, TypeError> {
    let recv = ctx.subst.apply(&d.recv);
    let base = deref_ref(&recv.strip_borrow().clone());
    if matches!(base, Ty::Var(_)) {
        return Ok(true);
    }
    let target: Ty = if d.field == "last" {
        // `.last` on `List T` → `List T` (§3.3.1).
        match &base {
            Ty::App(name, args) if name == "List" => Ty::App(name.clone(), args.clone()),
            other => {
                return Err(TypeError::new(
                    Stage::Inference,
                    d.span,
                    format!(
                        "`.last` is only valid on `List T`, not `{}`",
                        pretty_ty(other, &ctx.subst)
                    ),
                ))
            }
        }
    } else {
        let type_name = match &base {
            Ty::Con(n) => n.clone(),
            Ty::App(n, _) => n.clone(),
            other => {
                return Err(TypeError::new(
                    Stage::Inference,
                    d.span,
                    format!(
                        "cannot project field `{}` from `{}`",
                        d.field,
                        pretty_ty(other, &ctx.subst)
                    ),
                ))
            }
        };
        ctx.resolve_field_on(&type_name, &base, &d.field, d.span)?
    };
    let result = ctx.subst.apply(&d.result);
    unify(&mut ctx.subst, &result, &target, d.span)?;
    Ok(false)
}

/// Re-apply the module's final substitution to every recorded expression type.
/// Types are recorded mid-inference (possibly with metavariables that later
/// unifications resolve); calling this at each module boundary leaves entries
/// maximally resolved. Metavar indices are globally unique and the subst is
/// reset per module, so re-applying an earlier module's already-resolved
/// entries through a fresh subst is a no-op for them.
fn finalize_expr_types(ctx: &mut Context) {
    let keys: Vec<(String, Span)> = ctx.expr_types.keys().cloned().collect();
    for k in keys {
        if let Some(ty) = ctx.expr_types.get(&k) {
            ctx.expr_types.insert(k, ctx.subst.apply(ty));
        }
    }
}

/// Collect outbox constructor names for each actor with properties.
fn collect_actor_outbox(items: &[Item]) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for item in items {
        if let Item::Actor(a) = item {
            let ctors: Vec<String> = a
                .properties
                .iter()
                .flat_map(|p| crate::outbox::outbox_ctors(p))
                .map(|s| s.to_string())
                .collect();
            if !ctors.is_empty() {
                out.insert(a.name.clone(), ctors);
            }
        }
    }
    out
}

/// A single module's input for multi-module inference.
#[derive(Debug, Clone)]
pub struct ModuleInput {
    /// Module name (for error messages).
    pub name: String,
    /// The module's items (functions, constructors, actors).
    pub items: Vec<Item>,
    /// Import aliases: `(visible_name, source_name)` pairs.
    /// `visible_name` is the name used in this module; `source_name` is the
    /// name in the exporting module.
    pub import_aliases: Vec<(String, String)>,
    /// Effective exported names in canonical key form (resolved from the export
    /// block, `..` expanded): functions/types bare, actors `@`-prefixed — the same
    /// keys used in `TypedProgram.bindings`. Only these are visible to dependent
    /// modules. Empty = nothing exported.
    pub exports: Vec<String>,
}

/// Multi-module inference: infer each module in DAG order with per-module
/// namespaces. The constructor table is global; the func/actor env is
/// per-module (the visibility boundary).
///
/// `modules` must be in callee-first topological order. The prelude is an
/// ordinary module in the set (D053) — it simply appears first, since every
/// importer depends on it.
pub fn infer_multi_module(modules: &[ModuleInput]) -> Result<TypedProgram, TypeError> {
    let mut ctx = Context::new();

    // 1. Collect type aliases from all modules.
    for module in modules {
        for item in &module.items {
            if let Item::TypeAlias(a) = item {
                ctx.aliases.insert(a.name.clone(), a.ty.clone());
            }
        }
    }

    // 2. Setup global: builtins + non-parametric constructors (from all modules).
    //    Namespacing check covers ALL constructors (including parametric),
    //    per-module uniqueness (same name allowed across modules).
    let all_ctors: Vec<(String, ConstructorDecl)> = modules
        .iter()
        .flat_map(|m| {
            m.items.iter().map(move |i| match i {
                Item::Constructor(c) => vec![(m.name.clone(), c.clone())],
                Item::ParametricType(pt) => pt
                    .ctors
                    .iter()
                    .map(|c| (m.name.clone(), c.clone()))
                    .collect(),
                _ => Vec::new(),
            })
        })
        .flatten()
        .collect();
    let non_param_ctors: Vec<ConstructorDecl> = modules
        .iter()
        .flat_map(|m| m.items.iter())
        .filter_map(|i| match i {
            Item::Constructor(c) => Some(c.clone()),
            _ => None,
        })
        .collect();
    ctx.check_namespacing(&all_ctors)?;
    setup_global(&non_param_ctors, &mut ctx)?;

    // 3. Register parametric types (polymorphic constructors).
    // §3.2 disambiguation: single-ctor, non-parametric types whose ctor name
    // is an existing type name are reclassified as aliases.
    for module in modules {
        for item in &module.items {
            if let Item::ParametricType(pt) = item {
                if is_alias_reclassification(pt, &ctx) {
                    let ty = ctor_fields_to_type(&pt.ctors[0]);
                    ctx.aliases.insert(pt.name.clone(), ty);
                } else {
                    ctx.add_parametric_type(pt)?;
                }
            }
        }
    }

    // Builtin schemes/ctors name aliased types directly (e.g. `String`);
    // expand them so they agree with declaration-site expansion (§3.2). Runs
    // after step 3 so reclassified parametric aliases are included.
    expand_builtin_aliases(&mut ctx)?;

    // 4. Save the base env (predecls: @std, @net, @SysStd, @SysNet).
    let base_env = ctx.env.clone();

    // 5. Infer each module in DAG order with per-module env.
    let mut all_bindings: BTreeMap<(String, usize), Scheme> = BTreeMap::new();
    let mut all_actors = BTreeMap::new();
    // Per-name index for import resolution: name → all (arity, scheme) pairs
    // of the exported overloads. Imports are per-name — every arity of an
    // imported name is carried together (D052/D053).
    let mut global_names: HashMap<String, Vec<(usize, Scheme)>> = HashMap::new();

    for module in modules {
        // Build per-module env: base predecls + imported names.
        let mut env = base_env.clone();
        for (visible, source) in &module.import_aliases {
            if let Some(schemes) = global_names.get(source) {
                for (arity, scheme) in schemes {
                    env.insert((visible.clone(), *arity), scheme.clone());
                }
            }
        }

        // Reset per-module state (env is set explicitly below).
        ctx.reset_module_state();
        ctx.env = env;

        // Infer this module. Any TypeError bubbling up originates here, so
        // attribute it to `module.name` for multi-module error messages.
        let (bindings, actors) =
            infer_module(&module.items, &mut ctx).map_err(|e| e.with_module(&module.name))?;
        resolve_deferred_fields(&mut ctx)
            .map_err(|e| e.with_module(&module.name))?;
        finalize_expr_types(&mut ctx);

        // Synthetic class-method impls (`__op_*`, `__bind_Monad_*`, etc.) are
        // internal machinery, not source-level exports — accumulate their schemes
        // so dispatch can resolve them from any dependent module.
        for ((name, arity), scheme) in &bindings {
            if name.starts_with("__op_") || name.contains("_Monad_") {
                ctx.op_impl_env.insert((name.clone(), *arity), scheme.clone());
            }
        }

        // Register this module's names for cross-module resolution.
        // Only exported names are visible to dependent modules (private by default).
        let export_set: std::collections::HashSet<&str> =
            module.exports.iter().map(|s| s.as_str()).collect();
        for ((name, arity), scheme) in &bindings {
            if export_set.contains(name.as_str()) {
                global_names
                    .entry(name.clone())
                    .or_default()
                    .push((*arity, scheme.clone()));
            }
        }
        // Actor handles (`("@name", 0)`) and behaviors (`("@name", arity)`) are in
        // `bindings` under canonical keys, so the loop above registers them when the
        // export set contains `"@name"`. System predecls live only in the base env,
        // never in module bindings — they are not user exports.

        all_bindings.extend(bindings);
        all_actors.extend(actors);
    }

    validate_closedness(&ctx, &all_bindings, &all_actors)?;

    // Convert schemes to concrete types, preserving composite keys.
    let mut bindings: BTreeMap<(String, usize), Ty> = all_bindings
        .into_iter()
        .map(|((name, arity), scheme)| ((name, arity), ctx.subst.apply(&scheme.body)))
        .collect();

    let all_outbox: BTreeMap<String, Vec<String>> = modules
        .iter()
        .flat_map(|m| collect_actor_outbox(&m.items))
        .collect();
    // Merge per-module residency maps (v1 region work is single-module; later modules
    // extend earlier ones on name collision).
    let mut residency: BTreeMap<String, bool> = BTreeMap::new();
    for m in modules.iter() {
        residency.extend(crate::resident::compute_residency_items(&m.items));
    }
    let mut method_tys = std::mem::take(&mut ctx.method_tys)
        .into_iter()
        .map(|(k, ty)| (k, ctx.subst.apply(&ty)))
        .collect();
    // A1b: derive VM metadata once (pointer-free table + per-ctor field_is_ref).
    let pointer_free_types = crate::pf::compute_pointer_free(&ctx.ctors);
    let ctor_meta = derive_ctor_meta(&ctx.ctors);
    let mut expr_types = std::mem::take(&mut ctx.expr_types);
    // A1b 3c: global dense renumbering + structural no-Var backstop.
    renumber_exported(
        &mut bindings,
        &mut method_tys,
        &mut expr_types,
        &mut all_actors,
        &ctx.expected_free,
    )?;
    Ok(TypedProgram {
        bindings,
        constructors: ctx.ctors.clone(),
        type_ctors: ctx.type_ctors.clone(),
        actors: all_actors,
        actor_outbox: all_outbox,
        match_types: ctx.match_types.clone(),
        expr_types,
        residency,
        method_tys,
        type_params: ctx
            .type_params
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        pointer_free_types,
        ctor_meta,
        op_dispatch: ctx.op_dispatch.clone(),
    })
}

/// Global setup, run once per program: builtins, user constructors
/// (registration), and the predeclared system actors. The constructor table
/// is global; ctor names are unique *per module* (the same name may appear in
/// different modules — import aliasing disambiguates at use sites). The
/// func/actor env is per-module (the visibility boundary, set in
/// `infer_module`). Namespacing is checked by the caller before this runs.
fn setup_global(user_ctors: &[ConstructorDecl], ctx: &mut Context) -> Result<(), TypeError> {
    ctx.install_builtins();

    // Register user constructors.
    // (Self-referential user types are permitted — §7 rule 14 classifies them
    // non-pointer-free via the inference heuristic, it no longer forbids them.)
    for c in user_ctors {
        ctx.add_user_ctor(c)?;
    }

    // Predeclare @std / @net (WAG prelude actors, D043) and the native
    // @SysStd / @SysNet proxies. `@Name` for a declared actor is unified with
    // its real Msg type in `infer_actor`; these predecls let references
    // resolve before (or without) the prelude merge. (@SysConn is not
    // predeclared: only the native @SysNet router addresses it, so user code
    // — and the prelude — cannot name it.)
    ctx.env.insert(
        ("@std".to_string(), 0),
        Scheme::mono(Ty::actor(Ty::Con("StdMsg".into()))),
    );
    ctx.env.insert(
        ("@net".to_string(), 0),
        Scheme::mono(Ty::actor(Ty::Con("NetMsg".into()))),
    );
    ctx.env.insert(
        ("@SysStd".to_string(), 0),
        Scheme::mono(Ty::actor(Ty::Con("SysStdMsg".into()))),
    );
    ctx.env.insert(
        ("@SysFile".to_string(), 0),
        Scheme::mono(Ty::actor(Ty::Con("SysFileMsg".into()))),
    );
    ctx.env.insert(
        ("@SysNet".to_string(), 0),
        Scheme::mono(Ty::actor(Ty::Con("SysNetMsg".into()))),
    );
    // User-facing prelude wrappers (lowercase) — D078: system actor handles.
    ctx.env.insert(
        ("@std".to_string(), 0),
        Scheme::mono(Ty::actor(Ty::Con("StdMsg".into()))),
    );
    ctx.env.insert(
        ("@file".to_string(), 0),
        Scheme::mono(Ty::actor(Ty::Con("FileMsg".into()))),
    );
    ctx.env.insert(
        ("@net".to_string(), 0),
        Scheme::mono(Ty::actor(Ty::Con("NetMsg".into()))),
    );

    Ok(())
}

/// Find the span of the first top-level item satisfying `pred`.
fn item_span<F>(items: &[Item], pred: F) -> Option<Span>
where
    F: Fn(&Item) -> bool,
{
    items.iter().find(|i| pred(i)).map(|i| match i {
        Item::Func(f) => f.span,
        Item::Actor(a) => a.span,
        _ => Span::empty(0),
    })
}

/// Extract the first parameter type of a `Ty::Fun`.
fn first_param(ty: Ty) -> Option<Ty> {
    if let Ty::Fun(ps, _) = ty {
        Some(ps[0].clone())
    } else {
        None
    }
}

/// Extract the return type of a `Ty::Fun`.
fn ret_of(ty: Ty) -> Option<Ty> {
    if let Ty::Fun(_, ret) = ty {
        Some(*ret)
    } else {
        None
    }
}

/// D081 v2: after `@main` desugar, its body becomes three items
/// (`main_boot` / `@main` / `main_end`) each carrying the *same* State
/// (§5.4). Unify their State types so boot's result threads through run and
/// into end. No-op unless all three exist (a desugared `@main`).
fn tie_main_state(ctx: &mut Context, items: &[Item]) -> Result<(), TypeError> {
    let boot_span = item_span(items, |i| matches!(i, Item::Func(f) if f.name == "main_boot"));
    let main_span = item_span(items, |i| matches!(i, Item::Actor(a) if a.name == "main"));
    let end_span = item_span(items, |i| matches!(i, Item::Func(f) if f.name == "main_end"));

    // Bail out unless all three items exist (a desugared `@main`).
    if boot_span.is_none() || main_span.is_none() || end_span.is_none() {
        return Ok(());
    }
    // `main_span` is checked above for existence; it is unused below (boot/end
    // spans are more precise for the unification errors we report).
    let boot_span = boot_span.unwrap();
    let end_span = end_span.unwrap();

    // Pull the State field out of each resolved env type.
    let boot_state = ctx.env
        .get(&("main_boot".to_string(), 1))
        .cloned()
        .map(|s| ctx.subst.apply(&s.body))
        .and_then(ret_of);
    let main_state = ctx.env
        .get(&("@main".to_string(), 2))
        .cloned()
        .map(|s| ctx.subst.apply(&s.body))
        .and_then(first_param);
    let end_state = ctx.env
        .get(&("main_end".to_string(), 2))
        .cloned()
        .map(|s| ctx.subst.apply(&s.body))
        .and_then(first_param);

    // boot's return and @main's State (== its return) must agree.
    // Using shared borrows so `main_state` may participate in the next unify too.
    if let (Some(bs), Some(ms)) = (&boot_state, &main_state) {
        unify(&mut ctx.subst, bs, ms, boot_span)?;
    }
    // The State @main threads must be what end receives.
    if let (Some(ms), Some(es)) = (&main_state, &end_state) {
        unify(&mut ctx.subst, ms, es, end_span)?;
    }
    Ok(())
}

/// Infer one module's funcs and actors against the shared context. The module's
/// top-level names are allocated in `ctx.env` (the visibility boundary — a name
/// not in it is unbound), inferred in callee-first topological order (D044), and
/// generalized per singleton-SCC (§3.5). Returns the module's frozen bindings
/// and actor types. `ctx.singleton_sccs` is reset per call so it is per-module.
type ModuleResults = (BTreeMap<(String, usize), Scheme>, BTreeMap<String, Ty>);

fn infer_module(items: &[Item], ctx: &mut Context) -> Result<ModuleResults, TypeError> {
    // Ref T visibility boundary + method/field disjointness (D056/D057): purely
    // syntactic declaration-level checks, run before inference.
    crate::boundary::check_signatures(items)?;
    crate::boundary::check_method_field_disjointness(items)?;

    // Collect top-level funcs and actors — whole module is one letrec.
    let mut funcs: Vec<FuncDef> = Vec::new();
    let mut actors: Vec<&ActorDef> = Vec::new();
    let mut methods: Vec<&MethodDef> = Vec::new();
    let mut param_types: Vec<&wag_syntax::ParametricTypeDecl> = Vec::new();
    let mut classes: Vec<&wag_syntax::ClassDecl> = Vec::new();
    let mut instances: Vec<&wag_syntax::InstanceDecl> = Vec::new();
    for item in items {
        match item {
            Item::Func(f) => funcs.push(f.clone()),
            Item::Actor(a) => actors.push(a),
            // Methods (D056) are not top-level functions: they are keyed by
            // dotted name per receiver type and resolved at call sites. Their
            // bodies get a dedicated typing pass (receiver resolution, link
            // boundary rule) — see spec §4.3.5, §6.9.
            Item::Method(m) => methods.push(m),
            Item::Constructor(_) => {}
            Item::TypeAlias(_) => {}
            Item::ParametricType(pt) => param_types.push(pt),
            Item::Class(c) => classes.push(c),
            Item::Instance(i) => instances.push(i),
        }
    }

    // Namespacing: top-level names are unique; actors occupy the `@`-prefixed
    // namespace, so a func `std` and an actor `@std` may coexist (§2: distinct atoms).
    // Arity overloading (§3.11 / D051): same function name at different arities is allowed.
    let mut seen_top: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_func_arity: Vec<(String, usize)> = Vec::new();
    for f in &funcs {
        let arity = f.params.len();
        // Reject same name + same arity (true duplicate).
        if seen_func_arity
            .iter()
            .any(|(n, a)| n == &f.name && *a == arity)
        {
            return Err(TypeError::new(
                Stage::Namespacing,
                f.span,
                format!(
                    "duplicate top-level definition `{}` at arity {}",
                    f.name, arity
                ),
            ));
        }
        seen_top.insert(f.name.clone());
        seen_func_arity.push((f.name.clone(), arity));
    }
    for a in &actors {
        if !seen_top.insert(format!("@{}", a.name)) {
            return Err(TypeError::new(
                Stage::Namespacing,
                a.span,
                format!("duplicate actor definition `@{}`", a.name),
            ));
        }
    }

    // Allocate monomorphic metavars for every top-level binding (D052: keyed by (name, arity)).
    for f in &funcs {
        let tv = ctx.fresh_with(OriginInfo::new(format!("type of `{}`", f.name), f.span));
        ctx.env
            .insert((f.name.clone(), f.params.len()), Scheme::mono(tv));
    }
    for a in &actors {
        let beh = ctx.fresh_with(OriginInfo::new(
            format!("behavior of `@{}`", a.name),
            a.span,
        ));
        // Behavior keyed `("@name", arity)` — separate namespace from functions.
        ctx.env
            .insert((format!("@{}", a.name), a.params.len()), Scheme::mono(beh));
        // Creation function: State -> Actor Msg (arity 1).
        // Both metavars unified when the actor body is typed.
        let state = ctx.fresh_with(OriginInfo::new(format!("State of `@{}`", a.name), a.span));
        let msg = ctx.fresh_with(OriginInfo::new(format!("Msg of `@{}`", a.name), a.span));
        ctx.env.insert(
            (format!("@{}", a.name), 1),
            Scheme::mono(Ty::Fun(vec![state], Box::new(Ty::actor(msg)))),
        );
    }

    // Seed each annotated function's metavar with its signature type before
    // inferring any body. This lets forward/cyclic references resolve to the
    // declared shape instead of an unresolved metavar: without it, matching or
    // field-accessing a not-yet-inferred partner's product/ctor result sees a bare
    // `Var` and errors with "not a product" (§3.2/§6.4 shape-requiring constructs).
    // Concrete signatures seed a mono scheme; polymorphic ones (D061) seed a
    // generalized scheme so call sites instantiate independently.
    for f in &funcs {
        if f.signature.is_none() {
            continue;
        }
        match ctx.signature_parts(f) {
            Ok(sp) if sp.ret.is_some() => {
                let (ps, ret) = (sp.params, sp.ret.unwrap());
                let ann = Ty::Fun(ps, Box::new(ret));
                let scheme = if sp.var_idxs.is_empty() {
                    Scheme::mono(ann)
                } else {
                    Scheme {
                        vars: super::util::collect_free_vars(&ann, &ctx.subst),
                        body: ann,
                    constraints: vec![],                    }
                };
                ctx.env.insert((f.name.clone(), f.params.len()), scheme);
            }
            // A signature failing here means infer_func will report the
            // precise error during body inference; do not produce a duplicate.
            _ => {}
        }
    }

    // Seed method call types so surface calls (`l.append 42`) resolve during
    // function-body inference (D056/D061). Schemes are `Fun(recv :: ps, r)` — the
    // instantiated receiver is always the first parameter; parametric receivers
    // bind their declared params to fresh vars. Unresolvable signatures or arity
    // mismatches are skipped — `infer_method` reports the precise error.
    // Accumulated across modules (v1: no export filtering).
    for m in &methods {
        let sig = match m.signature.as_ref() {
            Some(s) => s,
            None => continue,
        };
        let mut var_names = std::collections::BTreeSet::new();
        super::util::collect_type_var_names(sig, &mut var_names);
        // Bind each type-var name to a fresh Var (same order as `resolve_signature`).
        let mut var_map: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for name in &var_names {
            let Ty::Var(v) = ctx.fresh_with(crate::error::OriginInfo::new(
                format!("type variable `{name}`"),
                m.span,
            )) else {
                unreachable!("fresh_with always yields a Var")
            };
            // Signature vars are generalized, never unified away (A1b).
            ctx.expected_free.insert(v);
            var_map.insert(name.clone(), v);
        }
        let sig_ty = match super::util::resolve_aliases(sig, &ctx.aliases, 0)
            .and_then(|resolved| super::util::surface_to_ty_with_vars(&resolved, &var_map))
        {
            Ok(t) => t,
            Err(_) => continue, // unresolvable signature: same as above.
        };
        let (ps, r) = match sig_ty {
            Ty::Fun(ps, r) if ps.len() == m.params.len() => (ps, r),
            other if m.params.is_empty() => (vec![], Box::new(other)),
            _ => continue, // arity mismatch: infer_method reports the error.
        };
        // Receiver type: `App(name, [Var per declared param])` or bare `Con`.
        // Params absent from the signature get fresh (implicitly quantified) vars.
        let recv_idxs = ctx.receiver_var_idxs(
            &m.type_name,
            &format!("{}.{}", m.type_name, m.name),
            m.span,
            &var_map,
        );
        let recv = if recv_idxs.is_empty() {
            Ty::Con(m.type_name.clone())
        } else {
            Ty::App(
                m.type_name.clone(),
                recv_idxs.into_iter().map(Ty::Var).collect(),
            )
        };
        let mut full = vec![recv];
        full.extend(ps.iter().cloned());
        let fun = Ty::Fun(full, r);
        ctx.methods_table
            .entry(m.type_name.clone())
            .or_default()
            .insert(
                m.name.clone(),
                Scheme {
                    vars: super::util::collect_free_vars(&fun, &ctx.subst),
                    body: fun,
                constraints: vec![],                },
            );
    }

    // Typeclass declarations (D075, §3.12): register constrained schemes for
    // each class method as top-level bindings. Instance methods/helpers are
    // added to the funcs list so they get inferred like regular functions.
    for cls in &classes {
        // Assign ctor var indices: param[0] → 0, param[1] → 1, …
        let mut ctor_vars: HashMap<String, u32> = HashMap::new();
        for (i, p) in cls.params.iter().enumerate() {
            ctor_vars.insert(p.clone(), i as u32);
        }
        for m in &cls.methods {
            // Collect type variable names from the signature (lowercase names
            // not in class params).
            let mut var_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            super::util::collect_type_var_names(&m.sig, &mut var_names);
            // Remove class param names (they are ctor vars, not type vars).
            var_names.retain(|n| !ctor_vars.contains_key(n));
            // Assign fresh Var indices.
            let mut var_map: HashMap<String, u32> = HashMap::new();
            for name in &var_names {
                let Ty::Var(v) = ctx.fresh_with(crate::error::OriginInfo::new(
                    format!("type variable `{name}` in class method `{}`", m.name),
                    m.span,
                )) else {
                    unreachable!("fresh_with always yields a Var")
                };
                ctx.expected_free.insert(v);
                var_map.insert(name.clone(), v);
            }
            // Convert the signature to an internal Ty with ctor vars.
            let sig_ty = match super::util::surface_to_ty_with_ctor_vars(
                &m.sig, &var_map, &ctor_vars,
            ) {
                Ok(t) => t,
                Err(_) => continue,
            };
            // Extract params and return from the Fun type.
            let (ps, r) = match sig_ty {
                Ty::Fun(ps, r) => (ps, r),
                other => (vec![], Box::new(other)),
            };
            let body = Ty::Fun(ps.clone(), r);
            // v1: single class param → ctor_var is always 0.
            let constraint = crate::types::Constraint {
                class_name: cls.name.clone(),
                ctor_var: 0,
            };
            let scheme = Scheme {
                vars: super::util::collect_free_vars(&body, &ctx.subst),
                body,
                constraints: vec![constraint],
            };
            ctx.env.insert((m.name.clone(), ps.len()), scheme);
            // Index operator-named methods for dispatch_binop lookup.
            if is_op_name(&m.name) {
                ctx.op_class_index.insert(m.name.clone(), cls.name.clone());
            }
        }
    }

    // Instance declarations: add methods and helpers to the funcs list so they
    // are inferred in the topological pass. Operator-named methods get synthetic
    // names (`__op_Class_TypeStr`) to avoid env collisions across instances.
    for inst in &instances {
        let type_str = wag_syntax::pretty_type(&inst.instance_ty).replace(' ', "");
        // Parametric instance heads (§3.12): single-uppercase-letter names are
        // type variables (§2) — allocate live vars so `dispatch_binop` can
        // unify them against concrete operand types (e.g. `Eq (Option T)`).
        let mut var_names: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        super::util::collect_type_var_names(&inst.instance_ty, &mut var_names);
        let mut var_map: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for name in &var_names {
            let Ty::Var(v) = ctx.fresh_with(crate::error::OriginInfo::new(
                format!("type variable `{name}` in instance `{}` {}", inst.class_name, type_str),
                inst.span,
            )) else {
                unreachable!("fresh_with always yields a Var")
            };
            ctx.expected_free.insert(v);
            var_map.insert(name.clone(), v);
        }
        let inst_ty = match super::util::surface_to_ty_with_vars(&inst.instance_ty, &var_map) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for m in &inst.methods {
            if is_op_name(&m.name) {
                let synthetic = format!("__op_{}_{}", inst.class_name, type_str);
                ctx.class_instances
                    .entry(inst.class_name.clone())
                    .or_default()
                    .push((inst_ty.clone(), synthetic.clone()));
                let mut renamed = m.clone();
                renamed.name = synthetic;
                funcs.push(renamed);
            } else if inst.class_name == "Monad" {
                // Monad methods are internal machinery (called via __monad_bind /
                // __monad_pure desugaring), so use synthetic names to keep them
                // out of the user-facing env and in op_impl_env for cross-module
                // dispatch.
                let synthetic = format!("__{}_Monad_{}", m.name, type_str);
                ctx.class_instances
                    .entry(inst.class_name.clone())
                    .or_default()
                    .push((inst_ty.clone(), synthetic.clone()));
                let mut renamed = m.clone();
                renamed.name = synthetic;
                funcs.push(renamed);
            } else {
                // Other non-operator methods: keep the original name so the
                // inferred (concrete) type overwrites the polymorphic class
                // signature in the env, making argument mismatches detectable
                // at call sites. Also register in class_instances for dispatch.
                ctx.class_instances
                    .entry(inst.class_name.clone())
                    .or_default()
                    .push((inst_ty.clone(), m.name.clone()));
                funcs.push(m.clone());
            }
        }
        for h in &inst.helpers {
            funcs.push(h.clone());
        }
    }

    // Generalization eligibility: singleton SCCs (reset per module). These are
    // eligible for polymorphic generalization (§3.5, Option A).
    ctx.singleton_sccs.clear();
    let call_graph = build_call_graph(&funcs);
    for f in &funcs {
        if !is_in_cycle(&f.name, &call_graph) {
            ctx.singleton_sccs.insert(f.name.clone());
        }
    }

    // D081 v2: pre-register `@main`'s creation function type so that `main_boot`
    // (inferred as a plain function *before* actors) can resolve `self` to the
    // actor's handle type. Without this, `self` in `main_boot`'s body is a free
    // variable and the closedness check fails on the `RequestLine` reply target.
    if let Some(main_actor) = actors.iter().find(|a| a.name == "main") {
        if let Some(sig) = &main_actor.signature {
            if let wag_syntax::Type::Func(params, _ret) = sig {
                if params.len() == 2 {
                    let state_ty = super::util::surface_to_ty(&params[0])?;
                    let msg_ty = super::util::surface_to_ty(&params[1])?;
                    ctx.env.insert(
                        ("@main".to_string(), 1),
                        Scheme::mono(Ty::Fun(
                            vec![state_ty],
                            Box::new(Ty::actor(msg_ty)),
                        )),
                    );
                }
            }
        }
    }

    // Infer each binding in topological order (callees before callers), so that
    // helper functions are generalized before their callers constrain them.
    // Synthetic instance methods (`__op_*`, `__*Monad_*`) are inferred first:
    // they're only reachable via operator dispatch (not explicit calls), so the
    // call graph has no edge to them and topo order can't guarantee they're
    // available when a caller's `==`/`<=` etc. is resolved.
    let order = topo_infer_order(&funcs, &call_graph);
    let (inst_idxs, rest_idxs): (Vec<_>, Vec<_>) =
        order.into_iter().partition(|&i| funcs[i].name.starts_with("__"));
    for &idx in inst_idxs.iter().chain(rest_idxs.iter()) {
        ctx.infer_func(&funcs[idx])?;
    }
    for a in &actors {
        ctx.infer_actor(a)?;
    }
    // D081 v2: tie @main's three items to one State (§5.4). Done after all
    // inference so each item's State is already pinned to its body's type.
    tie_main_state(ctx, items)?;
    // Method bodies (D056/D057): single-step operations on the receiver handle.
    for m in &methods {
        ctx.infer_method(m)?;
    }

    // Freeze results: return schemes (preserving quantified type vars).
    let mut bindings = BTreeMap::new();
    for f in &funcs {
        let key = (f.name.clone(), f.params.len());
        let scheme = ctx.env.get(&key).cloned().ok_or_else(|| {
            TypeError::new(
                Stage::Inference,
                f.span,
                format!("internal: `{}` not in env", f.name),
            )
        })?;
        bindings.insert(key, scheme);
    }
    let mut actor_tys = BTreeMap::new();
    for a in &actors {
        // Behavior is keyed `("@name", arity)` — separate namespace from functions.
        let bkey = (format!("@{}", a.name), a.params.len());
        let beh_scheme = ctx.env.get(&bkey).cloned().ok_or_else(|| {
            TypeError::new(
                Stage::Inference,
                a.span,
                format!("internal: `@{}` not in env", a.name),
            )
        })?;
        let beh = ctx.subst.apply(&beh_scheme.body);
        bindings.insert(bkey.clone(), Scheme::mono(beh.clone()));
        // Creation function at arity 1: State -> Actor Msg.
        let ckey = (format!("@{}", a.name), 1);
        if let Some(cscheme) = ctx.env.get(&ckey).cloned() {
            let cty = ctx.subst.apply(&cscheme.body);
            bindings.insert(ckey.clone(), Scheme::mono(cty.clone()));
            // Extract Actor Msg from the creation function's return type.
            if let Ty::Fun(_, ret) = &cty {
                if let Ty::App(name, args) = ret.as_ref() {
                    if name == "Actor" && args.len() == 1 {
                        actor_tys.insert(a.name.clone(), ret.as_ref().clone());
                    }
                }
            }
        }
    }
    // System predecls, keyed by bare name like user actors.
    actor_tys.insert("Std".into(), Ty::actor(Ty::Con("StdMsg".into())));
    actor_tys.insert("Net".into(), Ty::actor(Ty::Con("NetMsg".into())));
    actor_tys.insert("SysStd".into(), Ty::actor(Ty::Con("SysStdMsg".into())));
    actor_tys.insert("SysNet".into(), Ty::actor(Ty::Con("SysNetMsg".into())));

    // Boundary for implicit signatures: no inferred scheme may contain Ref (D056).
    crate::boundary::check_schemes(&bindings, items)?;

    Ok((bindings, actor_tys))
}

/// Returns true if `name` is an operator symbol (e.g. `"+"`, `"=="`).
fn is_op_name(name: &str) -> bool {
    matches!(
        name,
        "+" | "-" | "*" | "/" | "%" | ">" | "<" | ">=" | "<=" | "==" | "!=" | "!" | "^" | "|" | "&" | "<<" | ">>"
    )
}
