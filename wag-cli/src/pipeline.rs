//! Pipeline: compile, load, and run WAG programs (single and multi-module).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use wag_check::{check_modules, CheckedProgram, ModuleView};
use wag_runtime::{
    run_main_with_io, IoConfig, Runtime, SchedulerConfig, MAX_EXEC_TIMEOUT,
};
use wag_syntax::{finish_program, parse_desugared, Expr, ImportBlock, ImportGroup, Item, Pat, Span};
use wag_types::infer_multi_module;
use wag_interp::{load_checked, Engine, LoadedProgram, Value, VmError};

use crate::bind;
use crate::diagnostics::{Diagnostic, PipelineStage};
use crate::resolve;

/// Options for running a program after a successful check.
#[derive(Debug, Clone)]
pub struct RunOptions {
    pub argv: Vec<String>,
    /// When set, feed these lines to `@std` then EOF (deterministic tests / `--stdin-file`).
    pub stdin_lines: Option<Vec<String>>,
    pub timeout: Duration,
    pub scheduler: SchedulerConfig,
    /// When true, also write captured `@std` stdout to the process stdout (CLI default).
    pub emit_stdout: bool,
    /// When true, print dev-mode execution stats (steps + allocations) to stderr.
    pub print_stats: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        RunOptions {
            argv: Vec::new(),
            stdin_lines: None,
            // Hard 10s ceiling (CLAUDE.md). Raising the step/timeout budgets needs permission.
            timeout: MAX_EXEC_TIMEOUT,
            scheduler: SchedulerConfig::default(),
            emit_stdout: false,
            print_stats: false,
        }
    }
}

/// Successful compile through check + load.
#[derive(Debug)]
pub struct Compiled {
    checked: CheckedProgram,
    loaded: LoadedProgram,
}

impl Compiled {
    /// Construct from a checked program — `loaded` is derived here so the pair
    /// can never be mismatched.
    pub fn new(checked: CheckedProgram) -> Self {
        let loaded = load_checked(checked.program(), checked.typed(), checked.cell_reuse());
        Self { checked, loaded }
    }
    pub fn checked(&self) -> &CheckedProgram {
        &self.checked
    }
    pub fn loaded(&self) -> &LoadedProgram {
        &self.loaded
    }
}

/// Outcome of `run` when the program has `@main`.
pub struct RunResult {
    pub runtime: Runtime,
    pub stdout: String,
}

/// Successful pipeline result.
// `Checked(Compiled)` is intentionally larger than `Executed(RunResult)`; boxing the
// larger variant would only add indirection for an internal CLI type.
#[allow(clippy::large_enum_variant)]
pub enum PipelineOk {
    /// No `@main` — structural checks + load only.
    Checked(Compiled),
    /// `@main` ran to completion.
    Executed(RunResult),
}

/// Build the synthetic [`resolve::ModuleSet`] for a raw source string: the
/// built-in `prelude` (if imported) as the sole resolvable import target (D053),
/// then the raw source as the `<source>` entry module. Any other import requires
/// a file-based entry (`wagc run <file>`).
fn source_module_set(src: &str) -> Result<resolve::ModuleSet, Diagnostic> {
    let program = parse_desugared(src)?;

    // Imported modules first (callee order), then the raw source as the entry.
    let mut modules: Vec<resolve::ResolvedModule> = Vec::new();
    if let Some(ref ib) = program.imports {
        for group in &ib.groups {
            if group.module != "prelude" {
                return Err(Diagnostic {
                    stage: PipelineStage::Resolve,
                    span: Span::new(0, 0),
                    message: format!(
                        "cannot resolve module `{}` from raw source",
                        group.module
                    ),
                    hint: Some(
                        "Write the program to a file and use `wagc run <file>`; \
                         only the built-in `prelude` is importable here."
                            .into(),
                    ),
                    rule: None,
                    type_stage: None,
                    module_name: None,
                    origin_span: None,
                });
            }
            if modules.is_empty() {
                let path = resolve::builtin_prelude_path().ok_or_else(|| Diagnostic {
                    stage: PipelineStage::Resolve,
                    span: Span::new(0, 0),
                    message: "module `prelude` not found: expected `prelude/prelude.wag`".into(),
                    hint: None,
                    rule: None,
                    type_stage: None,
                    module_name: None,
                    origin_span: None,
                })?;
                let source = std::fs::read_to_string(&path).map_err(|e| Diagnostic {
                    stage: PipelineStage::Resolve,
                    span: Span::new(0, 0),
                    message: format!("cannot read prelude `{}`: {e}", path.display()),
                    hint: None,
                    rule: None,
                    type_stage: None,
                    module_name: None,
                    origin_span: None,
                })?;
                let pprog = parse_desugared(&source)?;
                modules.push(resolve::ResolvedModule {
                    name: "prelude".into(),
                    path,
                    source,
                    program: pprog,
                    imports: Vec::new(),
                    package: "<builtin>".into(),
                });
            }
        }
    }

    modules.push(resolve::ResolvedModule {
        name: "<source>".into(),
        path: std::path::PathBuf::from("<source>"),
        source: src.to_string(),
        program,
        imports: Vec::new(),
        package: "<source>".into(),
    });

    let entry_index = modules.len() - 1;
    Ok(resolve::ModuleSet { modules, entry_index })
}

/// Parse → type → check (pass 1+2) for a raw source string. Does not load or
/// execute. Delegates to [`compile_multi_module`] over a synthetic module set.
pub fn compile_source(src: &str) -> Result<CheckedProgram, Diagnostic> {
    let module_set = source_module_set(src)?;
    compile_multi_module(&module_set)
}

/// Multi-module compile: per-module inference with per-module namespaces,
/// then module-aware check (D067) — errors carry their originating module.
///
/// Modules are inferred in DAG order (callees first). Each module gets its own
/// name-resolution env (imports + locals); the prelude is an ordinary module
/// in the set when imported (D053).
pub fn compile_multi_module(
    module_set: &resolve::ModuleSet,
) -> Result<CheckedProgram, Diagnostic> {
    use wag_syntax::Item;
    use wag_types::ModuleInput;

    // 1. Check for cross-module name collisions (same name in two modules).
    bind::check_cross_module_collisions(module_set)?;

    // 2. Import collision detection + namespace building (via bind.rs).
    bind::bind_names(module_set)?;

    // 2. Desugar each module separately and extract import aliases.
    let mut finished_progs: Vec<wag_syntax::Program> = Vec::new();
    for module in &module_set.modules {
        let mut prog = wag_syntax::Program {
            exports: None,
            imports: None,
            items: module.program.items.clone(),
            source: None,
        };
        finish_program(&mut prog);
        // D060: single early desugar of method-body bare names (before
        // inference). Stateless — no cross-module info needed (D059).
        wag_syntax::desugar_method_bare_names(&mut prog);
        // D081: split `@main` into boot/run/end (§5.4.1) before inference.
        wag_syntax::desugar_main(&mut prog);
        finished_progs.push(prog);
    }

    let mut module_inputs: Vec<ModuleInput> = Vec::new();
    for (module, prog) in module_set.modules.iter().zip(finished_progs.into_iter()) {
        // Extract import aliases: (visible_name, source_name).
        let mut import_aliases: Vec<(String, String)> = Vec::new();
        if let Some(ref imp_block) = module.program.imports {
            for group in &imp_block.groups {
                for item in &group.items {
                    match item {
                        wag_syntax::ImportItem::Named {
                            name,
                            is_actor,
                            alias,
                            ..
                        } => {
                            let visible = alias.clone().unwrap_or_else(|| {
                                if *is_actor {
                                    format!("@{}", name)
                                } else {
                                    name.clone()
                                }
                            });
                            let source = if *is_actor {
                                format!("@{}", name)
                            } else {
                                name.clone()
                            };
                            import_aliases.push((visible, source));
                        }
                        wag_syntax::ImportItem::All { span } => {
                            // `..` imports all *exported* names under their canonical keys
                            // (actors `@`-prefixed) — the same keys inference registers.
                            let exported = bind::exported_names(module_set, &group.module, *span)?;
                            for key in exported {
                                import_aliases.push((key.clone(), key));
                            }
                        }
                    }
                }
            }
        }

        // Compute the effective export list for this module.
        let exports: Vec<String> = match &module.program.exports {
            None => Vec::new(),
            Some(eb) => {
                let mut names = Vec::new();
                for item in &eb.items {
                    match item {
                        wag_syntax::ExportItem::Named { name, .. } => {
                            names.push(name.clone());
                        }
                        wag_syntax::ExportItem::All { .. } => {
                            // `export ..` → all local names (canonical keys: actors `@`-prefixed).
                            for i in &module.program.items {
                                match i {
                                    Item::Func(f) => names.push(f.name.clone()),
                                    Item::Actor(a) => names.push(format!("@{}", a.name)),
                                    Item::Constructor(_) => {}
                                    Item::TypeAlias(a) => names.push(a.name.clone()),
                                    Item::ParametricType(p) => names.push(p.name.clone()),
                                    Item::ConstrainedDatatype(c) => names.push(c.name.clone()),
                                    // Methods (D056) are not top-level exportable names.
                                    Item::Method(_) => {}
                                    Item::Class(c) => names.push(c.name.clone()),
                                    Item::Instance(_) => {}
                                }
                            }
                        }
                    }
                }
                names
            }
        };

        module_inputs.push(ModuleInput {
            name: module.name.clone(),
            items: prog.items,
            import_aliases,
            exports,
        });
    }

    // 2b. Resolve cross-module ctor name collisions: rename later modules'
    //     ctors to `{module}_{Ctor}` so the global registry stays unambiguous.
    let ctor_renames = resolve_ctor_collisions(&mut module_inputs);
    // Patch import aliases: source names that were renamed must use the new key.
    for mi in &mut module_inputs {
        for (_visible, source) in mi.import_aliases.iter_mut() {
            if let Some(new_name) = ctor_renames.get(source) {
                *source = new_name.clone();
            }
        }
    }

    // 3. Per-module inference (per-module namespaces).
    // Convert via `From<TypeError>` so the structured causal note (`context`)
    // and message-pattern hints flow through instead of being dropped.
    let typed = infer_multi_module(&module_inputs).map_err(Diagnostic::from)?;

    // 4. Module-aware check (D067): per-module views so every check-stage
    // error carries its originating module. `CheckedProgram.program` is still
    // the merged desugared program for load.
    let views: Vec<ModuleView> = module_inputs
        .iter()
        .map(|m| ModuleView { name: Some(m.name.as_str()), items: &m.items })
        .collect();
    let checked = check_modules(&views, typed)?;
    Ok(checked)
}

/// Multi-module run: resolve → compile → load → execute.
pub fn run_multi_module(
    module_set: &resolve::ModuleSet,
    opts: RunOptions,
) -> Result<PipelineOk, Diagnostic> {
    let checked = compile_multi_module(module_set)?;
    let loaded = load_checked(checked.program(), checked.typed(), checked.cell_reuse());

    if loaded.actor("main").is_some() {
        let io = if let Some(lines) = opts.stdin_lines.clone() {
            IoConfig::scripted(lines)
        } else {
            IoConfig::default()
        };
        let timeout = opts.timeout.min(MAX_EXEC_TIMEOUT);
        let result = run_main_with_io(
            loaded.clone(),
            &opts.argv,
            opts.scheduler,
            timeout,
            io.clone(),
        );
        let stdout = io.take_stdout_string();
        if opts.emit_stdout && !stdout.is_empty() {
            print!("{stdout}");
        }
        match result {
            Ok(rt) => {
                if opts.print_stats {
                    let s = rt.stats();
                    eprintln!(
                        "[wagc] dev stats: steps={} (hard limits: {} steps/call, {}s wall)",
                        s.total_steps,
                        wag_interp::STEP_LIMIT,
                        MAX_EXEC_TIMEOUT.as_secs()
                    );
                }
                Ok(PipelineOk::Executed(RunResult { runtime: rt, stdout }))
            }
            Err(e) => Err(e.into()),
        }
    } else {
        Ok(PipelineOk::Checked(Compiled::new(checked)))
    }
}

/// Outcome of running a single test file (see [`run_test_file`]).
#[derive(Debug)]
pub enum TestOutcome {
    /// All assertions passed.
    Pass,
    /// An assertion failed; carries the failure message (`assertion failed: …`).
    Fail(String),
}

/// Mirror a test path to its source path by replacing the first `test` path
/// component with `src`. Returns `None` if no `test` component is present.
pub fn mirror_src_path(test_path: &Path) -> Option<PathBuf> {
    let mut replaced = false;
    let comps: Vec<std::ffi::OsString> = test_path
        .components()
        .map(|c| {
            if !replaced && c.as_os_str() == "test" {
                replaced = true;
                std::ffi::OsString::from("src")
            } else {
                c.as_os_str().to_os_string()
            }
        })
        .collect();
    if replaced { Some(comps.iter().collect()) } else { None }
}

/// Compile and run one test file against its mirrored source module.
///
/// The test file's items are merged into the source entry module (same namespace,
/// so a test can call both exported *and* private functions) and its import block
/// is unioned with the source's. The module graph is then re-resolved from the
/// merged entry so any module the *test* imports (but the source did not) enters
/// the DAG in callees-first order, keeping cross-module resolution correct.
/// `test_main` is invoked; a failing `assert` surfaces as [`TestOutcome::Fail`].
pub fn run_test_file(test_path: &Path) -> Result<TestOutcome, Diagnostic> {
    let src_path = mirror_src_path(test_path).ok_or_else(|| test_diag(format!(
        "`{}` is not under a `test/` directory; cannot find mirrored source",
        test_path.display(),
    )))?;
    if !src_path.exists() {
        return Err(test_diag(format!(
            "mirrored source `{}` does not exist (expected for test `{}`)",
            src_path.display(),
            test_path.display(),
        )));
    }

    // Parse the test file and the source entry module. The test's items are merged
    // into the source entry (same namespace → private access) and its import block
    // is unioned with the source's so a test may pull in whatever else it needs.
    let test_source = std::fs::read_to_string(test_path)
        .map_err(|e| test_diag(format!("cannot read test `{}`: {e}", test_path.display())))?;
    let test_prog = parse_desugared(&test_source)?;

    let src_source = std::fs::read_to_string(&src_path)
        .map_err(|e| test_diag(format!("cannot read source `{}`: {e}", src_path.display())))?;
    let mut entry_program = parse_desugared(&src_source)?;
    entry_program.items.extend(test_prog.items);
    // Union the import blocks so tests can import names beyond what the source
    // already imported. Deduplicate by module name (source wins on conflict).
    entry_program.imports = merge_imports(&entry_program.imports, &test_prog.imports);

    // Re-resolve the module graph with the merged entry program so any module the
    // *test* imports (but the source did not) enters the DAG in callees-first order,
    // keeping cross-module resolution correct.
    let module_set = resolve::resolve_modules_with_entry(&src_path, entry_program)?;

    // Compile + load via the standard multi-module pipeline.
    let checked = compile_multi_module(&module_set)?;
    let loaded = load_checked(checked.program(), checked.typed(), checked.cell_reuse());

    // Invoke the nullary test entry point.
    let mut engine = Engine::new(&loaded);
    match engine.call_name("test_main", vec![]) {
        Ok(_) => Ok(TestOutcome::Pass),
        Err(VmError::Panic(msg)) if msg.starts_with("assertion failed") => {
            Ok(TestOutcome::Fail(msg))
        }
        Err(e) => Err(Diagnostic::from(e)),
    }
}

/// Union two import blocks, deduplicating by module name (first wins).
fn merge_imports(a: &Option<ImportBlock>, b: &Option<ImportBlock>) -> Option<ImportBlock> {
    let mut groups: Vec<ImportGroup> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for ib in [a, b].into_iter().flatten() {
        for g in &ib.groups {
            if !seen.contains(&g.module) {
                seen.push(g.module.clone());
                groups.push(g.clone());
            }
        }
    }
    if groups.is_empty() {
        None
    } else {
        let span = a
            .as_ref()
            .and_then(|ib| ib.groups.first())
            .map(|g| g.span)
            .or_else(|| b.as_ref().and_then(|ib| ib.groups.first()).map(|g| g.span))
            .unwrap_or_else(|| Span::new(0, 0));
        Some(ImportBlock { groups, span })
    }
}

/// Build a resolve-stage diagnostic for test-runner bookkeeping errors.
fn test_diag(message: String) -> Diagnostic {
    Diagnostic {
        stage: PipelineStage::Resolve,
        span: Span::new(0, 0),
        message,
        hint: None,
        rule: None,
        type_stage: None,
        module_name: None,
        origin_span: None,
    }
}

/// CLI entry: resolve modules from the entry file, then run the multi-module
/// pipeline.
#[allow(clippy::result_large_err)] // (String, Diagnostic) is ergonomic at call sites
pub fn run_entry_path(
    path: &Path,
    opts: RunOptions,
) -> Result<PipelineOk, (String, Diagnostic)> {
    // Render each error against the source of the module that owns it
    // (`diag.module_name`), never the entry file — a location must not be
    // highlighted inside our module for another module's error. Errors raised
    // before the ModuleSet exists resolve their source by reading the module's
    // own file; once built, look it up via `ModuleSet::source_for`.
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let module_set = resolve::resolve_packages(path).map_err(|d| {
        let src = match &d.module_name {
            Some(name) => std::fs::read_to_string(parent.join(format!("{name}.wag")))
                .unwrap_or_default(),
            None => std::fs::read_to_string(path).unwrap_or_default(),
        };
        (src, d)
    })?;
    run_multi_module(&module_set, opts).map_err(|d| {
        let src = match &d.module_name {
            Some(name) => module_set
                .source_for(name)
                .map(|s| s.to_string())
                .unwrap_or_else(|| module_set.entry().source.clone()),
            None => module_set.entry().source.clone(),
        };
        (src, d)
    })
}

/// Compile and load into a `LoadedProgram` (cell reuse enabled).
pub fn compile_and_load(src: &str) -> Result<Compiled, Diagnostic> {
    let checked = compile_source(src)?;
    Ok(Compiled::new(checked))
}

/// Full pipeline for a raw source string: compile+load, and if `@main` is
/// present, execute it. Delegates to [`run_multi_module`] over a synthetic
/// module set so the single- and multi-module run paths share one code path.
pub fn run_source(src: &str, opts: RunOptions) -> Result<PipelineOk, Diagnostic> {
    let module_set = source_module_set(src)?;
    run_multi_module(&module_set, opts)
}

/// Smoke-execute a named pure function after compile (harness helper).
pub fn call_fn(src: &str, name: &str, args: Vec<Value>) -> Result<Value, Diagnostic> {
    let compiled = compile_and_load(src)?;
    let mut eng = Engine::new(compiled.loaded());
    eng.call_name(name, args).map_err(Diagnostic::from)
}

/// CLI / harness entry: read a file and run the pipeline.
#[allow(clippy::result_large_err)] // (String, Diagnostic) is ergonomic at call sites
pub fn run_path(path: &Path, opts: RunOptions) -> Result<PipelineOk, (String, Diagnostic)> {
    let src = std::fs::read_to_string(path).map_err(|e| {
        (
            String::new(),
            Diagnostic {
                stage: PipelineStage::Parse,
                span: Span::new(0, 0),
                message: format!("cannot read {}: {e}", path.display()),
                hint: None,
                rule: None,
                type_stage: None,
                module_name: None,
                origin_span: None,
            },
        )
    })?;
    run_source(&src, opts).map_err(|d| (src, d))
}

/// Load stdin lines from a file (split on `\n`; drop only a trailing empty line).
pub fn read_stdin_file(path: &Path) -> std::io::Result<Vec<String>> {
    let text = std::fs::read_to_string(path)?;
    let mut lines: Vec<String> = text.lines().map(|s| s.to_string()).collect();
    if text.ends_with('\n') && lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    Ok(lines)
}

pub fn workspace_fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("fixtures")
}

// ---------------------------------------------------------------------------
// Cross-module ctor name collision resolution (§3.2 per-module namespacing)
// ---------------------------------------------------------------------------

/// Detect same-named user ctors across modules and rename the later ones to
/// `{module}_{Ctor}` so the global registry (type inference + VM) stays
/// unambiguous. Returns a map of original → renamed for all renames performed.
fn resolve_ctor_collisions(
    module_inputs: &mut [wag_types::ModuleInput],
) -> BTreeMap<String, String> {
    // Collect ctor names per module (in topological order).
    let mut name_owners: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, mi) in module_inputs.iter().enumerate() {
        for item in &mi.items {
            match item {
                Item::Constructor(c) => {
                    name_owners.entry(c.name.clone()).or_default().push(i);
                }
                Item::ParametricType(pt) => {
                    for c in &pt.ctors {
                        name_owners.entry(c.name.clone()).or_default().push(i);
                    }
                }
                _ => {}
            }
        }
    }

    // Find collisions: names owned by more than one module.
    let mut renames: BTreeMap<String, String> = BTreeMap::new();
    for (name, owners) in &name_owners {
        if owners.len() <= 1 {
            continue;
        }
        // First owner keeps the name; later owners get `{module}_{Ctor}`.
        for &mi_idx in &owners[1..] {
            let module_name = &module_inputs[mi_idx].name;
            let new_name = format!("{module_name}_{name}");
            renames.insert(name.clone(), new_name.clone());
            rename_ctor_in_items(&mut module_inputs[mi_idx].items, name, &new_name);
        }
    }

    renames
}

/// Rename a ctor in all items: declaration + expression/pattern references.
fn rename_ctor_in_items(items: &mut [Item], old: &str, new: &str) {
    for item in items.iter_mut() {
        match item {
            Item::Constructor(c) if c.name == old => {
                c.name = new.to_string();
            }
            Item::ParametricType(pt) => {
                for c in pt.ctors.iter_mut() {
                    if c.name == old {
                        c.name = new.to_string();
                    }
                }
            }
            Item::Func(f) => rename_ctor_in_expr(&mut f.body, old, new),
            Item::Actor(a) => rename_ctor_in_expr(&mut a.body, old, new),
            Item::Method(m) => rename_ctor_in_expr(&mut m.body, old, new),
            Item::Instance(inst) => {
                for m in inst.methods.iter_mut() {
                    rename_ctor_in_expr(&mut m.body, old, new);
                }
            }
            _ => {}
        }
    }
}

/// Walk an expression tree, renaming `Upper` atoms that match `old`.
fn rename_ctor_in_expr(e: &mut Expr, old: &str, new: &str) {
    match e {
        Expr::Upper { name, .. } if name == old => {
            *name = new.to_string();
        }
        Expr::Apply { fun, args, .. } => {
            rename_ctor_in_expr(fun, old, new);
            for a in args.iter_mut() {
                rename_ctor_in_expr(a, old, new);
            }
        }
        Expr::BinOp { lhs, rhs, .. } => {
            rename_ctor_in_expr(lhs, old, new);
            rename_ctor_in_expr(rhs, old, new);
        }
        Expr::Field { base, .. } => rename_ctor_in_expr(base, old, new),
        Expr::RecordUpdate { receiver, clauses, .. } => {
            rename_ctor_in_expr(receiver, old, new);
            for c in clauses.iter_mut() {
                match c {
                    wag_syntax::UpdateClause::Leaf { value, .. } => {
                        rename_ctor_in_expr(value, old, new);
                    }
                    wag_syntax::UpdateClause::Group { clauses, .. } => {
                        for sc in clauses.iter_mut() {
                            if let wag_syntax::UpdateClause::Leaf { value, .. } = sc {
                                rename_ctor_in_expr(value, old, new);
                            }
                        }
                    }
                }
            }
        }
        Expr::SetField { receiver, value, .. } => {
            rename_ctor_in_expr(receiver, old, new);
            rename_ctor_in_expr(value, old, new);
        }
        Expr::RegionWrite { receiver, value, .. } => {
            rename_ctor_in_expr(receiver, old, new);
            rename_ctor_in_expr(value, old, new);
        }
        Expr::Borrow { inner, .. } => rename_ctor_in_expr(inner, old, new),
        Expr::Bind { value, body, .. } => {
            rename_ctor_in_expr(value, old, new);
            rename_ctor_in_expr(body, old, new);
        }
        Expr::Seq { first, second, .. } => {
            rename_ctor_in_expr(first, old, new);
            rename_ctor_in_expr(second, old, new);
        }
        Expr::If { arms, .. } => {
            for arm in arms.iter_mut() {
                if let Some(cond) = &mut arm.condition {
                    rename_ctor_in_expr(cond, old, new);
                }
                rename_ctor_in_expr(&mut arm.body, old, new);
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            rename_ctor_in_expr(scrutinee, old, new);
            for arm in arms.iter_mut() {
                rename_ctor_in_pat(&mut arm.pattern, old, new);
                rename_ctor_in_expr(&mut arm.body, old, new);
            }
        }
        Expr::Do { stmts, .. } => {
            for s in stmts.iter_mut() {
                match s {
                    wag_syntax::DoStmt::Bind { expr, .. } => {
                        rename_ctor_in_expr(expr, old, new);
                    }
                    wag_syntax::DoStmt::Return(e) => rename_ctor_in_expr(e, old, new),
                }
            }
        }
        Expr::ListLit { elems, .. } => {
            for el in elems.iter_mut() {
                rename_ctor_in_expr(el, old, new);
            }
        }
        Expr::Lambda { body, .. } => rename_ctor_in_expr(body, old, new),
        Expr::Comprehension { generators, accs, guards, result, .. } => {
            for g in generators.iter_mut() {
                for s in g.sources.iter_mut() {
                    rename_ctor_in_expr(s, old, new);
                }
            }
            for a in accs.iter_mut() {
                rename_ctor_in_expr(&mut a.init, old, new);
            }
            for g in guards.iter_mut() {
                rename_ctor_in_expr(g, old, new);
            }
            rename_ctor_in_expr(result, old, new);
        }
        Expr::Product { elems, .. } => {
            for el in elems.iter_mut() {
                rename_ctor_in_expr(el, old, new);
            }
        }
        _ => {}
    }
}

/// Walk a pattern tree, renaming `Ctor` patterns that match `old`.
fn rename_ctor_in_pat(p: &mut Pat, old: &str, new: &str) {
    match p {
        Pat::Ctor { name, args, .. } => {
            if name == old {
                *name = new.to_string();
            }
            for a in args.iter_mut() {
                rename_ctor_in_pat(a, old, new);
            }
        }
        Pat::Product { elems, .. } => {
            for el in elems.iter_mut() {
                rename_ctor_in_pat(el, old, new);
            }
        }
        _ => {}
    }
}

