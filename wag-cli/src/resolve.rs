//! Module resolution: discover sibling `.wag` files, build the import DAG,
//! topologically sort (callees first), and reject cycles.
//!
//! Design: `doc/wag-modules.md` §5 (Cycles & inference order), §6 (pipeline).
//!
//! A module's name is its file name without the `.wag` extension (§2.1).
//! `import <name>` resolves to `<name>.wag` in the entry file's directory
//! (§2.4 single-package v1). The built-in `prelude` module additionally falls
//! back to its conventional location when no sibling file exists (D053).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use wag_syntax::{parse_desugared, ImportBlock, ImportGroup, ImportItem, Program, Span};

use crate::manifest::{self, Manifest};
use crate::{Diagnostic, PipelineStage};

/// A resolved module: name, path, source, parsed program.
#[derive(Debug)]
pub struct ResolvedModule {
    /// Module name (file name without `.wag`).
    pub name: String,
    /// File path.
    pub path: PathBuf,
    /// Source text.
    pub source: String,
    /// Parsed AST (desugared, not lambda-lifted).
    pub program: Program,
    /// Module names imported by this module (from its import block).
    pub imports: Vec<String>,
    /// Package name this module belongs to.
    pub package: String,
}

/// The graph of packages in the build: package name → root directory.
///
/// Search order for `import <name>` from a module in package P:
/// P's own root first, then each of P's declared deps (in manifest order).
#[derive(Debug)]
pub struct PackageGraph {
    /// Package name → root directory.
    pub roots: BTreeMap<String, PathBuf>,
    /// Package name → ordered list of dep package names (from manifest `[deps]`).
    pub deps: BTreeMap<String, Vec<String>>,
}

impl PackageGraph {
    /// Build a single-package graph (Phase 1 behavior: all modules in one dir).
    pub fn single(root: PathBuf) -> Self {
        let name = root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let mut roots = BTreeMap::new();
        roots.insert(name.clone(), root);
        PackageGraph {
            roots,
            deps: BTreeMap::new(),
        }
    }

    /// Search directories for a module import from package `pkg`:
    /// the package's own root first, then each dep's root (in manifest order).
    pub fn search_dirs(&self, pkg: &str) -> Vec<&PathBuf> {
        let mut dirs = Vec::new();
        if let Some(root) = self.roots.get(pkg) {
            dirs.push(root);
        }
        if let Some(deps) = self.deps.get(pkg) {
            for dep in deps {
                if let Some(root) = self.roots.get(dep) {
                    dirs.push(root);
                }
            }
        }
        dirs
    }

    /// All package roots (for name-uniqueness checks).
    pub fn all_roots(&self) -> impl Iterator<Item = &PathBuf> {
        self.roots.values()
    }

    /// Determine which package a file path belongs to (the package whose root
    /// is a prefix of the path). Returns `None` if the path is not under any
    /// known package root.
    pub fn package_for_path(&self, path: &Path) -> Option<String> {
        let canonical = path.canonicalize().ok()?;
        // Longest-prefix match: a nested package root wins over a shorter one.
        let mut best: Option<(&String, usize)> = None;
        for (name, root) in &self.roots {
            let Ok(root_canon) = root.canonicalize() else {
                continue;
            };
            if canonical.starts_with(&root_canon) {
                let len = root_canon.components().count();
                if best.map(|(_, l)| len > l).unwrap_or(true) {
                    best = Some((name, len));
                }
            }
        }
        best.map(|(name, _)| name.clone())
    }
}

/// The set of all resolved modules, in topological order (callees first).
#[derive(Debug)]
pub struct ModuleSet {
    /// Modules in DAG order: dependencies before dependents.
    /// The entry module is last.
    pub modules: Vec<ResolvedModule>,
    /// Index of the entry module in `modules`.
    pub entry_index: usize,
}

impl ModuleSet {
    /// The entry module.
    pub fn entry(&self) -> &ResolvedModule {
        &self.modules[self.entry_index]
    }

    /// All modules except the entry (library modules), in topo order.
    pub fn libraries(&self) -> impl Iterator<Item = &ResolvedModule> {
        self.modules
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.entry_index)
            .map(|(_, m)| m)
    }

    /// Source text of the named module, if it is part of this set. Used to render
    /// an error against the module that owns it (see `diag.module_name`), so a
    /// location is never highlighted inside the entry file for another module's
    /// error.
    pub fn source_for(&self, name: &str) -> Option<&str> {
        self.modules
            .iter()
            .find(|m| m.name == name)
            .map(|m| m.source.as_str())
    }
}

/// Resolve the entry module and all transitive imports (single-package, Phase 1).
///
/// 1. Read + parse the entry file.
/// 2. For each `import <name>`, look up `<name>.wag` in the entry's directory.
/// 3. Recursively resolve each imported module's imports.
/// 4. Build the DAG and topologically sort (callees first).
/// 5. Reject import cycles (path in error).
pub fn resolve_modules(entry_path: &Path) -> Result<ModuleSet, Diagnostic> {
    let entry_name = module_name_from_path(entry_path);
    let entry_source = std::fs::read_to_string(entry_path).map_err(|e| Diagnostic {
        stage: PipelineStage::Resolve,
        span: Span::new(0, 0),
        message: format!("cannot read entry module `{entry_name}`: {e}"),
        hint: None,
        rule: None,
        type_stage: None,
        module_name: None,
        origin_span: None,
    })?;
    // Route through `From<ParseError>` so parse errors get an actionable hint
    // from `parse_error_hint` instead of a bare message.
    let entry_program = parse_desugared(&entry_source).map_err(Diagnostic::from)?;
    let root = entry_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let graph = PackageGraph::single(root);
    resolve_with_entry(entry_path, entry_name, entry_source, entry_program, &graph)
}

/// Like [`resolve_modules`] but uses an already-parsed entry program. Used by the
/// test runner, which merges test items + imports into the entry before resolving:
/// any module the *test* imports (but the source did not) must still enter the DAG,
/// ordered callees-first so cross-module resolution succeeds.
///
/// Imported modules are read from disk as usual; only the entry program is
/// overridden. The on-disk entry source is used for error rendering.
pub fn resolve_modules_with_entry(
    entry_path: &Path,
    entry_program: Program,
) -> Result<ModuleSet, Diagnostic> {
    let entry_name = module_name_from_path(entry_path);
    let entry_source = std::fs::read_to_string(entry_path).map_err(|e| Diagnostic {
        stage: PipelineStage::Resolve,
        span: Span::new(0, 0),
        message: format!("cannot read entry module `{entry_name}`: {e}"),
        hint: None,
        rule: None,
        type_stage: None,
        module_name: None,
        origin_span: None,
    })?;
    let root = entry_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let graph = PackageGraph::single(root);
    resolve_with_entry(entry_path, entry_name, entry_source, entry_program, &graph)
}

/// Resolve the entry module and all transitive imports across packages (Phase 2).
///
/// Discovers the package root via `wag.toml`, loads the manifest (if any),
/// recursively resolves path dependencies, and builds the full module graph.
/// Falls back to single-package behavior when no `wag.toml` is found.
pub fn resolve_packages(entry_path: &Path) -> Result<ModuleSet, Diagnostic> {
    let entry_name = module_name_from_path(entry_path);
    let entry_source = std::fs::read_to_string(entry_path).map_err(|e| Diagnostic {
        stage: PipelineStage::Resolve,
        span: Span::new(0, 0),
        message: format!("cannot read entry module `{entry_name}`: {e}"),
        hint: None,
        rule: None,
        type_stage: None,
        module_name: None,
        origin_span: None,
    })?;
    let mut entry_program = parse_desugared(&entry_source).map_err(Diagnostic::from)?;
    ensure_prelude_import(&mut entry_program);

    let root = manifest::find_package_root(entry_path);
    let graph = build_package_graph(&root)?;
    resolve_with_entry(entry_path, entry_name, entry_source, entry_program, &graph)
}

/// The prelude is foundational (D088): the runtime assumes its List ctors are
/// present (wag-runtime entry), so `wagc run` merges it even when the entry
/// does not import it. An explicit `import prelude ..` is left untouched.
fn ensure_prelude_import(program: &mut Program) {
    let has_prelude = program
        .imports
        .as_ref()
        .is_some_and(|ib| ib.groups.iter().any(|g| g.module == "prelude"));
    if has_prelude {
        return;
    }
    let zero = Span::new(0, 0);
    let group = ImportGroup {
        module: "prelude".into(),
        items: vec![ImportItem::All { span: zero }],
        span: zero,
    };
    match &mut program.imports {
        Some(ib) => ib.groups.push(group),
        None => program.imports = Some(ImportBlock {
            groups: vec![group],
            span: zero,
        }),
    }
}

/// Build the full package graph starting from `root`.
///
/// If `wag.toml` exists, loads it and recursively resolves path dependencies.
/// If not, creates a single-package graph (Phase 1 behavior).
fn build_package_graph(root: &Path) -> Result<PackageGraph, Diagnostic> {
    let manifest_path = root.join("wag.toml");
    if !manifest_path.exists() {
        return Ok(PackageGraph::single(root.to_path_buf()));
    }
    let manifest = manifest::load_manifest(root)?;
    build_graph_from_manifest(root, &manifest)
}

/// Recursively build a `PackageGraph` from a manifest, following path deps.
fn build_graph_from_manifest(
    root: &Path,
    manifest: &Manifest,
) -> Result<PackageGraph, Diagnostic> {
    let mut roots: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut deps: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut visited: BTreeMap<String, PathBuf> = BTreeMap::new();

    // DFS through the dependency graph.
    let mut stack: Vec<(String, PathBuf)> = vec![(manifest.name.clone(), root.to_path_buf())];
    while let Some((name, pkg_root)) = stack.pop() {
        if visited.contains_key(&name) {
            continue;
        }
        visited.insert(name.clone(), pkg_root.clone());

        // Load this package's manifest (if it has one) to get its deps.
        let pkg_manifest_path = pkg_root.join("wag.toml");
        let (dep_names, dep_paths): (Vec<String>, std::collections::BTreeMap<String, PathBuf>) =
            if pkg_manifest_path.exists() {
                let m = manifest::load_manifest(&pkg_root)?;
                let names = m.deps.keys().cloned().collect();
                (names, m.deps.clone())
            } else {
                (Vec::new(), std::collections::BTreeMap::new())
            };

        roots.insert(name.clone(), pkg_root.clone());
        deps.insert(name.clone(), dep_names.clone());

        // Push deps onto the stack for recursive resolution.
        for dep_name in &dep_names {
            if !visited.contains_key(dep_name) {
                let dep_root = match dep_paths.get(dep_name) {
                    Some(rel) => pkg_root.join(rel),
                    None => continue,
                };
                let dep_root = dep_root.canonicalize().map_err(|e| Diagnostic {
                        stage: PipelineStage::Resolve,
                        span: Span::new(0, 0),
                        message: format!(
                            "cannot resolve dependency `{dep_name}` from `{}`: {e}",
                            pkg_root.display()
                        ),
                        hint: Some(format!(
                            "Check that the path in `[deps]` of `{}` points to a valid package directory.",
                            pkg_root.display()
                        )),
                        rule: None,
                        type_stage: None,
                        module_name: None,
                        origin_span: None,
                    })?;
                stack.push((dep_name.clone(), dep_root));
            }
        }
    }

    Ok(PackageGraph { roots, deps })
}

/// Shared resolution body for all `resolve_*` entry points.
fn resolve_with_entry(
    entry_path: &Path,
    entry_name: String,
    entry_source: String,
    entry_program: Program,
    graph: &PackageGraph,
) -> Result<ModuleSet, Diagnostic> {
    // Determine the entry's package: the package whose root is an ancestor
    // of the entry path.
    let entry_pkg = graph
        .package_for_path(entry_path)
        .unwrap_or_else(|| {
            // Fallback: use the first (only) package in a single-package graph.
            graph.roots.keys().next().cloned().unwrap_or_default()
        });

    // 1. Seed the resolved set with the (possibly overridden) entry module.

    // 2-3. Recursively resolve all imports (BFS).
    let mut resolved: HashMap<String, ResolvedModule> = HashMap::new();
    let mut queue: Vec<(String, String)> = vec![(entry_name.clone(), entry_pkg.clone())];
    resolved.insert(
        entry_name.clone(),
        ResolvedModule {
            name: entry_name.clone(),
            path: entry_path.to_path_buf(),
            source: entry_source,
            program: entry_program,
            imports: Vec::new(), // filled below
            package: entry_pkg.clone(),
        },
    );

    // Fill in the entry module's imported-module names (from its import block).
    {
        let imports = extract_import_names(&resolved[&entry_name].program);
        resolved.get_mut(&entry_name).unwrap().imports = imports.clone();
        for imp in imports {
            queue.push((imp, entry_pkg.clone()));
        }
    }

    while let Some((name, importer_pkg)) = queue.pop() {
        if resolved.contains_key(&name) {
            continue;
        }
        // Search the importer's package dirs (own root first, then deps).
        let mut path: Option<PathBuf> = None;
        for dir in graph.search_dirs(&importer_pkg) {
            let candidate = dir.join(format!("{name}.wag"));
            if candidate.exists() {
                path = Some(candidate);
                break;
            }
        }
        // Built-in `prelude` module falls back to its conventional location
        // when no file exists in any package dir (D053).
        if path.is_none() && name == "prelude" {
            path = builtin_prelude_path();
        }
        let path = match path {
            Some(p) => p,
            None => {
                let searched: Vec<String> = graph
                    .search_dirs(&importer_pkg)
                    .iter()
                    .map(|d| format!("`{}`", d.display()))
                    .collect();
                return Err(Diagnostic {
                    stage: PipelineStage::Resolve,
                    span: Span::new(0, 0),
                    message: format!(
                        "module `{name}` not found (searched: {})",
                        searched.join(", ")
                    ),
                    hint: Some(format!(
                        "Create `{name}.wag` in one of the searched directories or remove the import."
                    )),
                    rule: None,
                    type_stage: None,
                    module_name: None,
                    origin_span: None,
                });
            }
        };

        // Determine which package this module belongs to.
        let module_pkg = graph
            .package_for_path(&path)
            .unwrap_or_else(|| importer_pkg.clone());

        let source = std::fs::read_to_string(&path).map_err(|e| Diagnostic {
            stage: PipelineStage::Resolve,
            span: Span::new(0, 0),
            message: format!("cannot read module `{name}`: {e}"),
            hint: None,
            rule: None,
            type_stage: None,
            module_name: None,
            origin_span: None,
        })?;
        // Attribute the error to this module via the structured `module_name`
        // field (not a `[name]` prefix glued onto the message text), so the
        // renderer can show the correct file + location. Reuses `From<ParseError>`
        // for span + hint parity with single-module parse errors.
        let program = parse_desugared(&source).map_err(|e| {
            let mut d: Diagnostic = e.into();
            d.module_name = Some(name.clone());
            d
        })?;
        let imports = extract_import_names(&program);
        resolved.insert(
            name.clone(),
            ResolvedModule {
                name: name.clone(),
                path: path.clone(),
                source,
                program,
                imports: imports.clone(),
                package: module_pkg.clone(),
            },
        );
        for imp in imports {
            queue.push((imp, module_pkg.clone()));
        }
    }

    // 4. Name-uniqueness check: scan all package dirs for `.wag` files and
    // reject if two files in different packages share the same stem (D077
    // global-ctor-uniqueness philosophy applied to module names).
    check_name_uniqueness(graph, &resolved)?;

    // 5. Topo sort (DFS, callees first) with cycle detection.
    let order = topo_sort(&resolved)?;

    // Build the ModuleSet in topo order.
    let modules: Vec<ResolvedModule> = order
        .into_iter()
        .map(|name| resolved.remove(&name).unwrap())
        .collect();
    let entry_index = modules
        .iter()
        .position(|m| m.name == entry_name)
        .expect("entry module must be in the resolved set");

    Ok(ModuleSet {
        modules,
        entry_index,
    })
}

/// Check that no two `.wag` files in different package dirs share the same
/// module name. Only flags names that are actually in the resolved set (i.e.
/// reachable from the entry) to avoid false positives on unused files.
fn check_name_uniqueness(
    graph: &PackageGraph,
    resolved: &HashMap<String, ResolvedModule>,
) -> Result<(), Diagnostic> {
    // For each resolved module, check if any other package dir contains a
    // file with the same stem.
    for (name, module) in resolved {
        for (pkg_name, root) in &graph.roots {
            if *pkg_name == module.package {
                continue; // same package — already handled by the HashMap
            }
            let candidate = root.join(format!("{name}.wag"));
            if candidate.exists() {
                let cand_canon = candidate.canonicalize().ok();
                let mod_canon = module.path.canonicalize().ok();
                if cand_canon != mod_canon {
                    return Err(Diagnostic {
                        stage: PipelineStage::Resolve,
                        span: Span::new(0, 0),
                        message: format!(
                            "module name `{name}` is not unique: found in both package `{}` and package `{pkg_name}`",
                            module.package
                        ),
                        hint: Some(
                            "Module names must be unique across all packages in the dependency graph."
                                .into(),
                        ),
                        rule: None,
                        type_stage: None,
                        module_name: None,
                        origin_span: None,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Locate the built-in prelude source (D053): CWD-relative `prelude/prelude.wag`
/// first (dev layout), then the copy shipped next to this crate (repo layout —
/// works under `cargo test` regardless of CWD).
pub fn builtin_prelude_path() -> Option<PathBuf> {
    let cwd = PathBuf::from("prelude/prelude.wag");
    if cwd.exists() {
        return Some(cwd);
    }
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../prelude/prelude.wag");
    manifest.exists().then_some(manifest)
}

/// Extract the list of imported module names from a parsed program.
fn extract_import_names(program: &Program) -> Vec<String> {
    let mut names = program
        .imports
        .as_ref()
        .map(|ib| ib.groups.iter().map(|g| g.module.clone()).collect::<Vec<_>>())
        .unwrap_or_default();
    // Re-export source modules (`export name from X`) must also enter the
    // import DAG so they are resolved, parsed, and topologically sorted.
    if let Some(eb) = &program.exports {
        for item in &eb.items {
            if let wag_syntax::ExportItem::Named { from: Some(module), .. } = item {
                names.push(module.clone());
            }
        }
    }
    names
}

/// Module name from a file path: file stem without extension.
fn module_name_from_path(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// DFS topological sort (callees first). Returns the order or a cycle error.
///
/// Edge direction: if module A imports module B, then B must come before A.
/// DFS visits dependencies first (post-order), which gives callees-first order.
fn topo_sort(
    resolved: &HashMap<String, ResolvedModule>,
) -> Result<Vec<String>, Diagnostic> {
    let mut visited: HashMap<String, u8> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut stack: Vec<String> = Vec::new();

    // Visit all nodes (the graph may be disconnected).
    let names: Vec<String> = resolved.keys().cloned().collect();
    for name in &names {
        if visited.get(name).copied().unwrap_or(0) == 0 {
            dfs(name, resolved, &mut visited, &mut order, &mut stack)?;
        }
    }

    Ok(order)
}

fn dfs(
    name: &str,
    resolved: &HashMap<String, ResolvedModule>,
    visited: &mut HashMap<String, u8>,
    order: &mut Vec<String>,
    stack: &mut Vec<String>,
) -> Result<(), Diagnostic> {
    match visited.get(name).copied().unwrap_or(0) {
        2 => return Ok(()),
        1 => {
            // Cycle detected: extract the path from `stack`.
            let cycle_start = stack.iter().position(|s| s == name).unwrap_or(0);
            let cycle_path: Vec<String> = stack[cycle_start..].to_vec();
            let path_str = cycle_path
                .iter()
                .map(|s| format!("`{s}`"))
                .chain(std::iter::once(format!("`{name}`")))
                .collect::<Vec<_>>()
                .join(" → ");
            return Err(Diagnostic {
                stage: PipelineStage::Resolve,
                span: Span::new(0, 0),
                message: format!("import cycle detected: {path_str}"),
                hint: Some(
                    "Break the cycle by removing an import or restructuring the modules."
                        .into(),
                ),
                rule: None,
                type_stage: None,
                module_name: None,
                origin_span: None,
            });
        }
        _ => {}
    }

    visited.insert(name.to_string(), 1);
    stack.push(name.to_string());

    // Visit dependencies (imports) first — post-order gives callees-first.
    if let Some(module) = resolved.get(name) {
        for dep in &module.imports {
            dfs(dep, resolved, visited, order, stack)?;
        }
    }

    stack.pop();
    visited.insert(name.to_string(), 2);
    order.push(name.to_string());

    Ok(())
}
