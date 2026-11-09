//! Public entry points: `prepare_runtime`, `run_main`, `run_main_with_io`.

use std::time::Duration;

use wag_interp::{
    build_prelude_list, LoadedProgram, PreludeListCtors, Value,
};

use crate::builtin_io::IoConfig;
use crate::runtime::{ActorKind, Runtime, RuntimeError, SchedulerConfig};
use crate::scheduler::spawn_actor_inner;

/// Parse + type + desugar + load for runtime tests.
pub fn prepare_runtime(
    src: &str,
) -> Result<(wag_syntax::Program, wag_types::TypedProgram, LoadedProgram), Box<dyn std::error::Error + Send + Sync>>
{
    // D088: always merge the prelude so prelude-declared types/ctors (Option,
    // Vec, List, string builtins) are available to all programs.
    let checked = wag_check::check_with_prelude(src)?;
    let loaded = wag_interp::load_checked(checked.program(), checked.typed(), checked.cell_reuse());
    Ok((checked.program().clone(), checked.typed().clone(), loaded))
}

/// Bootstrap `@main` with argv and run until it terminates (§5.4).
pub fn run_main(
    program: LoadedProgram,
    argv: &[String],
    config: SchedulerConfig,
    timeout: Duration,
) -> Result<(), RuntimeError> {
    run_main_with_io(program, argv, config, timeout, IoConfig::default())
        .map(|_| ())
}

/// Like [`run_main`] with injectable stdin lines / captured stdout.
pub fn run_main_with_io(
    program: LoadedProgram,
    argv: &[String],
    config: SchedulerConfig,
    timeout: Duration,
    io: IoConfig,
) -> Result<Runtime, RuntimeError> {
    let rt = Runtime::new_with_io(program, config, io);
    {
        let ready = rt.inner.ready.lock().unwrap();
        if let Some(ref err) = ready.fatal {
            return Err(RuntimeError::Panic(err.clone()));
        }
    }
    let _main = rt
        .program()
        .actor("main")
        .ok_or_else(|| RuntimeError::Panic("program has no @main".into()))?;
    // Build argv as `List String` (§5.4, D081 v2).
    // The prelude is always merged (foundational); D088 removed the old
    // cons-list fallback, so the prelude List ctors are guaranteed present.
    let ctors = PreludeListCtors::from_program(rt.program())
        .ok_or_else(|| RuntimeError::Panic("prelude List ctors not found".into()))?;
    let elems: Vec<Value> = argv.iter().map(|s| Value::Str(s.clone())).collect();
    let argv_val = build_prelude_list(elems, ctors).map_err(RuntimeError::Vm)?;
    // @main initial state is None (§5.4).
    let none_id = rt.program().opt_none_id();
    let id = spawn_actor_inner(&rt.inner, ActorKind::Main, Value::Data(none_id, vec![]), Some(argv_val))?;
    *rt.inner.main_id.lock().unwrap() = Some(id);
    rt.run_until_main_exits(timeout)?;
    Ok(rt)
}
