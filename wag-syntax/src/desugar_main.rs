//! D081: desugar `@main` into three functions (§5.4.1).
//!
//! `@main` is the program's root actor with a special `Msg` type carrying
//! `Start: List String` and `End: Int` constructors (§5.4). Its body is a
//! `match msg { … }` whose arms have distinct signatures:
//!
//! - **boot** (`Start` arm): `List String -> State` — no prior state; `st` is
//!   out of scope. The runtime calls it directly with argv to obtain the
//!   initial state.
//! - **run** (all other arms): `(State, Msg) -> State` — the ordinary actor
//!   shape; this becomes the `@main` behavior.
//! - **end** (`End` arm): `(State, Int) -> Unit` — receives the current state
//!   and an exit code; the process exits when it returns.
//!
//! This pass splits the match into three top-level items: the `@main` actor
//! (run arms only), a `main_boot` function (Start arm), and a `main_end`
//! function (End arm). The type checker ties their state types together via a
//! shared metavar (see `wag-types`); the loader extracts the three function
//! ids for the runtime.
//!
//! The pass is a no-op when `@main`'s body is not a `match` on its message
//! parameter, or when the `Start`/`End` arms are absent — the checker's
//! `@main` constructor requirement (§5.4) surfaces the problem.

use crate::ast::*;

/// Conventional names for the desugared `@main` arms. The type checker and
/// loader key off these exact names.
pub const MAIN_BOOT: &str = "main_boot";
pub const MAIN_END: &str = "main_end";

/// Split `@main` into `main_boot` / `@main` (run) / `main_end`. No-op if the
/// shape does not match (non-match body, or missing `Start`/`End` arms).
pub fn desugar_main(program: &mut Program) {
    let main_idx = match program
        .items
        .iter()
        .position(|i| matches!(i, Item::Actor(a) if a.name == "main"))
    {
        Some(idx) => idx,
        None => return,
    };

    let Item::Actor(main) = &program.items[main_idx] else {
        return;
    };
    // `@main` is `(State, Msg) -> …`: two params, second is the message.
    let [st_name, msg_name] = main.params.as_slice() else {
        return;
    };
    let st_name = st_name.clone();

    // The body must be `match <msg> { … }`.
    let Expr::Match {
        scrutinee,
        arms,
        span,
    } = &main.body
    else {
        return;
    };
    let Expr::Var { name, .. } = scrutinee.as_ref() else {
        return;
    };
    if name != msg_name {
        return;
    }
    // Clone the pieces we need before the mutable borrow below.
    let scrutinee = scrutinee.clone();
    let span = *span;

    // Locate the `Start` and `End` arms by constructor name.
    let mut boot_arm: Option<&MatchArm> = None;
    let mut end_arm: Option<&MatchArm> = None;
    let mut run_arms: Vec<MatchArm> = Vec::new();
    for arm in arms {
        let is_start = matches!(
            &arm.pattern,
            Pat::Ctor { name, .. } if name == "Start"
        );
        let is_end = matches!(&arm.pattern, Pat::Ctor { name, .. } if name == "End");
        if is_start {
            boot_arm = Some(arm);
        } else if is_end {
            end_arm = Some(arm);
        } else {
            run_arms.push(arm.clone());
        }
    }
    // Require both special arms; otherwise leave `@main` untouched (the
    // checker reports the missing constructors).
    let (boot_arm, end_arm) = match (boot_arm, end_arm) {
        (Some(b), Some(e)) => (b, e),
        _ => return,
    };

    // `main_boot <argv> = <Start arm body>`. The bound var comes from the
    // `Start` pattern; a wildcard becomes a fresh (unused) param name.
    let boot_param = bound_var_name(&boot_arm.pattern, "argv");
    // Annotate the parameter with the `Start` constructor's field type so the
    // type checker can resolve field access (e.g. `argv.peek`) on it (§5.4.1).
    let boot_sig = boot_signature(main, program);
    let boot = FuncDef {
        name: MAIN_BOOT.into(),
        properties: vec![],
        signature: boot_sig,
        constraints: vec![],
        params: vec![boot_param],
        body: boot_arm.body.clone(),
        span,
    };

    // `main_end <st> <code> = <End arm body>`. Reuses the actor's state param
    // name so the body's `st` references bind to it.
    let end_code = bound_var_name(&end_arm.pattern, "code");
    let end = FuncDef {
        name: MAIN_END.into(),
        properties: vec![],
        signature: None,
        constraints: vec![],
        params: vec![st_name.clone(), end_code],
        body: end_arm.body.clone(),
        span,
    };

    // Rewrite `@main` to the run match (Start/End arms removed). If no run
    // arms remain, the body is just `st` (identity) — the actor idles until
    // `End` terminates the process.
    let Item::Actor(main) = &mut program.items[main_idx] else {
        return;
    };
    main.body = if run_arms.is_empty() {
        Expr::Var {
            name: st_name.clone(),
            span,
        }
    } else {
        // Append a catch-all arm so the match is exhaustive: the scheduler
        // routes `Start`/`End` to `main_boot`/`main_end`, so this arm is
        // unreachable at runtime but satisfies the exhaustiveness check.
        let mut arms = run_arms;
        arms.push(MatchArm {
            pattern: Pat::Wildcard { span },
            body: Expr::Var {
                name: st_name.clone(),
                span,
            },
        });
        Expr::Match {
            scrutinee: scrutinee.clone(),
            arms,
            span,
        }
    };

    // Insert the two functions immediately after `@main`.
    program.items.insert(main_idx + 1, Item::Func(end));
    program.items.insert(main_idx + 1, Item::Func(boot));
}

/// The bound variable of a `Start`/`End` arm pattern, or a fresh name when the
/// pattern is a wildcard (the body then does not reference it).
fn bound_var_name(pat: &Pat, fresh: &str) -> String {
    match pat {
        Pat::Ctor { args, .. } => match args.first() {
            Some(Pat::Var { name, .. }) => name.clone(),
            _ => fresh.to_string(),
        },
        _ => fresh.to_string(),
    }
}

/// Build a `main_boot` signature from the `@main` actor's msg/state types and
/// the `Start` constructor's field type. Returns `None` when the info is
/// unavailable (the type checker then infers from usage).
fn boot_signature(main: &ActorDef, program: &Program) -> Option<Type> {
    // Extract msg type (2nd param) and state type (1st param) from the actor sig.
    let Type::Func(params, _ret) = main.signature.as_ref()? else {
        return None;
    };
    let [state_ty, msg_ty] = params.as_slice() else {
        return None;
    };
    // Find the `Start` constructor whose result type matches the msg type.
    // Constructors live inside `ParametricType` items (or standalone `Constructor`
    // items for single-ctor types).
    let start_field = program.items.iter().find_map(|item| match item {
        Item::ParametricType(pt) => pt.ctors.iter().find_map(|c| {
            if c.name == "Start" && &c.result_type == msg_ty {
                c.fields.first().map(|f| f.ty.clone())
            } else {
                None
            }
        }),
        Item::Constructor(c) => {
            if c.name == "Start" && &c.result_type == msg_ty {
                c.fields.first().map(|f| f.ty.clone())
            } else {
                None
            }
        }
        _ => None,
    })?;
    if std::env::var_os("WAG_DEBUG").is_some() {
        eprintln!(
            "[desugar_main] boot_signature: msg_ty={:?} state_ty={:?} start_field={:?}",
            msg_ty, state_ty, start_field
        );
    }
    Some(Type::Func(vec![start_field], Box::new(state_ty.clone())))
}
