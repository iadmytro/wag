//! Name binding: build per-module effective namespaces, resolve aliases,
//! and detect import collisions.
//!
//! Design: `doc/wag-modules.md` §4 (Name resolution).
//!
//! Names enter a module's namespace from two sources: local declarations and
//! explicit imports (the prelude is an ordinary importable module, D053).
//! Two imports with the same effective name = collision (compile error), as is
//! a local declaration clashing with any imported name — the remedy in both
//! cases is an alias (`alias <- source`) or renaming.

use std::collections::{BTreeMap, BTreeSet};

use wag_syntax::{ImportItem, Program, Span};

use crate::resolve::ModuleSet;
use crate::{Diagnostic, PipelineStage};

/// Where an imported name comes from.
#[derive(Debug, Clone)]
pub struct ImportSource {
    /// The module the name was imported from.
    pub module: String,
    /// The original name (before aliasing).
    pub original: String,
    /// Whether it's an actor (`@`-prefixed in source).
    pub is_actor: bool,
}

/// The effective name space for a module after binding.
#[derive(Debug, Clone)]
pub struct Namespace {
    /// Local declaration names (functions, types, actors).
    pub local: BTreeSet<String>,
    /// Imported names: effective name → source info. The prelude is just
    /// another import source (D053).
    pub imported: BTreeMap<String, ImportSource>,
}

impl Namespace {
    /// Look up a name, returning its source.
    ///
    /// Returns `Some("local")` or `Some("<module>")`; `None` if not in scope.
    pub fn resolve(&self, name: &str) -> Option<&str> {
        if self.local.contains(name) {
            Some("local")
        } else if self.imported.contains_key(name) {
            Some(self.imported[name].module.as_str())
        } else {
            None
        }
    }
}

/// Bind names for all modules in the set.
///
/// For each module:
/// 1. Collect local names (functions, types, actors).
/// 2. Collect imported names (with aliases resolved, `All` expanded).
/// 3. Check for collisions: two imports with the same effective name, or a
///    local declaration clashing with an imported name (D053 — alias required).
/// 4. Build the effective namespace.
pub fn bind_names(
    module_set: &ModuleSet,
) -> Result<BTreeMap<String, Namespace>, Diagnostic> {
    let mut namespaces: BTreeMap<String, Namespace> = BTreeMap::new();

    for module in &module_set.modules {
        // 1. Local names (all kinds, for the namespace) plus the func/actor
        //    subset used for clash detection against imports.
        let local = collect_local_names(&module.program);
        let mut local_fn_actor: BTreeSet<String> = BTreeSet::new();
        for item in &module.program.items {
            match item {
                wag_syntax::Item::Func(f) => { local_fn_actor.insert(f.name.clone()); }
                wag_syntax::Item::Actor(a) => { local_fn_actor.insert(format!("@{}", a.name)); }
                _ => {}
            }
        }

        // 2. Imported names.
        let mut imported: BTreeMap<String, ImportSource> = BTreeMap::new();
        if let Some(ref ib) = module.program.imports {
            for group in &ib.groups {
                let source_module = &group.module;
                for item in &group.items {
                    match item {
                        ImportItem::Named {
                            name,
                            is_actor,
                            alias,
                            span,
                        } => {
                            let effective = alias.as_deref().unwrap_or(name.as_str());
                            let key = if *is_actor {
                                format!("@{effective}")
                            } else {
                                effective.to_string()
                            };
                            if let Some(existing) = imported.get(&key) {
                                // Collision: two imports with same effective name.
                                return Err(Diagnostic {
                                    stage: PipelineStage::Resolve,
                                    span: *span,
                                    message: format!(
                                        "import collision: `{key}` imported from both `{}` and `{}`",
                                        existing.module, source_module
                                    ),
                                    hint: Some(
                                        "Use an alias (`alias <- source`) to disambiguate."
                                            .into(),
                                    ),
                                    rule: None,
                                    type_stage: None,
                                    module_name: None,
                                    origin_span: None,
                                });
                            }
                            let true_src = true_source_module(module_set, source_module, &key);
                            imported.insert(
                                key,
                                ImportSource {
                                    module: true_src,
                                    original: name.clone(),
                                    is_actor: *is_actor,
                                },
                            );
                        }
                        ImportItem::All { span } => {
                            // Expand `..`: import all exported names from the source module.
                            let exported = exported_names(module_set, source_module, *span)?;
                            for key in exported {
                                if let Some(existing) = imported.get(&key) {
                                    return Err(Diagnostic {
                                        stage: PipelineStage::Resolve,
                                        span: *span,
                                        message: format!(
                                            "import collision: `{key}` imported from both `{}` and `{}`",
                                            existing.module, source_module
                                        ),
                                        hint: Some(
                                            "Use an alias (`alias <- source`) to disambiguate."
                                                .into(),
                                        ),
                                        rule: None,
                                        type_stage: None,
                                        module_name: None,
                                        origin_span: None,
                                    });
                                }
                                let true_src = true_source_module(module_set, source_module, &key);
                                imported.insert(
                                    key.clone(),
                                    ImportSource {
                                        module: true_src,
                                        original: key.clone(),
                                        is_actor: key.starts_with('@'),
                                    },
                                );
                            }
                        }
                    }
                }
            }
        }

        // 3b. Local-vs-imported clash (D053): a local func/actor definition with
        //     the same effective name as an import is an error — alias or rename.
        for key in &local_fn_actor {
            if let Some(src) = imported.get(key) {
                return Err(Diagnostic {
                    stage: PipelineStage::Resolve,
                    span: Span::new(0, 0),
                    message: format!(
                        "name clash: `{key}` is defined locally and imported from `{}`",
                        src.module
                    ),
                    hint: Some(format!(
                        "Rename the local definition or import under an alias (`alias <- {}`).",
                        src.original
                    )),
                    rule: None,
                    type_stage: None,
                    module_name: Some(module.name.clone()),
                    origin_span: None,
                });
            }
        }

        namespaces.insert(
            module.name.clone(),
            Namespace {
                local,
                imported,
            },
        );
    }

    Ok(namespaces)
}

/// Resolve the true source module for a name exported by `module_name`.
///
/// If the module re-exports the name (`export name from X`), returns `X`
/// (the original definition site). Otherwise returns `module_name` itself.
fn true_source_module(module_set: &ModuleSet, module_name: &str, name: &str) -> String {
    let module = match module_set.modules.iter().find(|m| m.name == module_name) {
        Some(m) => m,
        None => return module_name.to_string(),
    };
    let Some(eb) = &module.program.exports else {
        return module_name.to_string();
    };
    for item in &eb.items {
        if let wag_syntax::ExportItem::Named {
            name: n,
            alias,
            from,
            is_actor,
            ..
        } = item
        {
            let effective = alias.as_deref().unwrap_or(n.as_str());
            let key = if *is_actor {
                format!("@{effective}")
            } else {
                effective.to_string()
            };
            if key == name {
                return from.clone().unwrap_or_else(|| module_name.to_string());
            }
        }
    }
    module_name.to_string()
}

/// Collect local declaration names from a program.
///
/// - Functions: `Item::Func` → name
/// - Types: `Item::Constructor` → result type head name
/// - Actors: `Item::Actor` → `@name`
fn collect_local_names(program: &Program) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for item in &program.items {
        match item {
            wag_syntax::Item::Func(f) => {
                names.insert(f.name.clone());
            }
            wag_syntax::Item::Constructor(c) => {
                names.insert(c.name.clone());
            }
            wag_syntax::Item::Actor(a) => {
                names.insert(format!("@{}", a.name));
            }
            wag_syntax::Item::TypeAlias(a) => {
                names.insert(a.name.clone());
            }
            wag_syntax::Item::ParametricType(p) => {
                names.insert(p.name.clone());
            }
            // Methods (D056) are not top-level names — they live in the
            // dotted namespace of their receiver type.
            wag_syntax::Item::Method(_) => {}
            wag_syntax::Item::Class(c) => {
                names.insert(c.name.clone());
            }
            wag_syntax::Item::Instance(_) => {}
        }
    }
    names
}

/// Check for cross-module name collisions: the same top-level name defined
/// in two different modules.
///
/// This is distinct from import collisions (two imports with the same effective
/// name in one module, checked by [`bind_names`]).
pub fn check_cross_module_collisions(
    module_set: &ModuleSet,
) -> Result<(), Diagnostic> {
    // Key includes arity for functions so overloads (same name, different param count)
    // are permitted (D051). Non-function items use arity 0.
    let mut seen: BTreeMap<(String, usize), (String, Span)> = BTreeMap::new();
    for module in &module_set.modules {
        // The built-in prelude is exempt (D053): a user module may privately
        // define a name the prelude also has, as long as it doesn't import it.
        if module.name == "prelude" {
            continue;
        }
        for item in &module.program.items {
            let (name, arity) = match item {
                wag_syntax::Item::Func(f) => (f.name.clone(), f.params.len()),
                // Constructor names are per-module: two modules may define the
                // same ctor name; import aliasing disambiguates at use sites.
                wag_syntax::Item::Constructor(_) => continue,
                wag_syntax::Item::Actor(a) => (format!("@{}", a.name), 0),
                wag_syntax::Item::TypeAlias(a) => (a.name.clone(), 0),
                wag_syntax::Item::ParametricType(p) => (p.name.clone(), 0),
                // Methods (D056) are not top-level names — skip collision checking.
                wag_syntax::Item::Method(_) => continue,
                wag_syntax::Item::Class(c) => (c.name.clone(), 0),
                wag_syntax::Item::Instance(_) => continue,
            };
            let span = match item {
                wag_syntax::Item::Func(f) => f.span,
                wag_syntax::Item::Constructor(c) => c.span,
                wag_syntax::Item::TypeAlias(a) => a.span,
                wag_syntax::Item::ParametricType(p) => p.span,
                wag_syntax::Item::Actor(a) => a.span,
                wag_syntax::Item::Method(m) => m.span,
                wag_syntax::Item::Class(c) => c.span,
                wag_syntax::Item::Instance(i) => i.span,
            };
            if let Some((prev_module, _prev_span)) = seen.get(&(name.clone(), arity)) {
                return Err(Diagnostic {
                    stage: PipelineStage::Resolve,
                    span,
                    message: format!(
                        "name collision: `{name}` defined in both `{prev_module}` and `{}`",
                        module.name
                    ),
                    hint: Some(
                        "Rename one of the definitions or use an import alias."
                            .into(),
                    ),
                    rule: None,
                    type_stage: None,
                    module_name: None,
                    origin_span: None,
                });
            }
            seen.insert((name, arity), (module.name.clone(), span));
        }
    }
    Ok(())
}

/// Get the exported names of a module in canonical key form (resolved through
/// its export block): functions/types bare, actors `@`-prefixed — the same keys
/// used in `TypedProgram.bindings`.
///
/// - No export block → empty (nothing exported).
/// - `export ..` → all local names.
/// - `export foo, bar` → the named items (effective names).
pub fn exported_names(
    module_set: &ModuleSet,
    module_name: &str,
    span: Span,
) -> Result<Vec<String>, Diagnostic> {
    let module = module_set
        .modules
        .iter()
        .find(|m| m.name == module_name)
        .ok_or_else(|| Diagnostic {
            stage: PipelineStage::Resolve,
            span,
            message: format!("module `{module_name}` not found in resolved set"),
            hint: None,
            rule: None,
            type_stage: None,
            module_name: None,
            origin_span: None,
        })?;

    match &module.program.exports {
        None => Ok(Vec::new()),
        Some(eb) => {
            let mut result = Vec::new();
            for item in &eb.items {
                match item {
                    wag_syntax::ExportItem::Named { name, is_actor, alias, .. } => {
                        let effective = alias.as_deref().unwrap_or(name.as_str());
                        result.push(if *is_actor {
                            format!("@{effective}")
                        } else {
                            effective.to_string()
                        });
                    }
                    wag_syntax::ExportItem::All { .. } => {
                        // Export all local names (actors already `@`-prefixed).
                        result.extend(collect_local_names(&module.program));
                    }
                }
            }
            Ok(result)
        }
    }
}
