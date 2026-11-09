//! Automatic actor disposal (D078 refcount abandonment).
//!
//! Verifies that a mortal actor is cleaned up automatically once the last live
//! handle to it is dropped. Uses the spec §8.14 `counter_supervised` fixture:
//! `@supervisor` stores a counter handle in its state; on `Shutdown` it returns
//! `None`, which drops that handle, driving the counter's refcount to zero and
//! triggering abandonment — so both actors are removed from the runtime.

use std::path::PathBuf;
use std::time::Duration;

use wag_interp::Value;
use wag_runtime::{prepare_runtime, Runtime, SchedulerConfig};

fn fixture(name: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.push("fixtures");
    p.push(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Build an arity-0 constructor value (Boot / Bump / Shutdown) as a message.
fn msg(loaded: &wag_interp::LoadedProgram, name: &str) -> Value {
    let ctor = *loaded.ctor_by_name.get(name).unwrap_or_else(|| panic!("{name} ctor exists"));
    Value::Data(ctor, vec![])
}

#[test]
fn last_handle_drop_disposes_actor() {
    let src = fixture("actors/counter_supervised.wag");
    let (_p, _t, loaded) = prepare_runtime(&src).expect("prepare_runtime");

    // Drive mode (workers=0): single-threaded and deterministic. Disposal is a
    // refcount + mailbox concern, not a threading one, so no worker pool is needed.
    let rt = Runtime::new(loaded.clone(), SchedulerConfig { workers: 0 });

    // @supervisor's initial state is `Option SupervisorState` → None.
    let sup_id = rt
        .spawn_actor("supervisor", Value::Data(loaded.opt_none_id(), vec![]))
        .expect("spawn supervisor");

    // --- Phase 1: Boot spawns a counter and stores its handle in state. ---
    rt.inject(sup_id, msg(&loaded, "Boot")).expect("inject Boot");
    rt.run_until_quiescent(Duration::from_secs(5)).expect("quiesce after Boot");

    let counters = rt.actor_ids_by_kind("counter");
    assert_eq!(counters.len(), 1, "Boot should have spawned exactly one @counter");
    // Do NOT read the counter's state here: `actor_state` moves it out (→ Unit),
    // which would break the Increment in Phase 2.

    // --- Phase 2: Bump drives the live counter; confirm it did real work. ---
    rt.inject(sup_id, msg(&loaded, "Bump")).expect("inject Bump");
    rt.run_until_quiescent(Duration::from_secs(5)).expect("quiesce after Bump");

    let c_id = rt.actor_ids_by_kind("counter")[0];
    assert_eq!(
        rt.actor_state(c_id).expect("counter alive"),
        Value::Int(1),
        "counter should have incremented to 1 before disposal"
    );

    // --- Phase 3: Shutdown makes the supervisor return None, dropping the only
    // counter handle. The refcount hits zero → Abandon is enqueued → both actors
    // are torn down and removed from the runtime automatically. ---
    rt.inject(sup_id, msg(&loaded, "Shutdown")).expect("inject Shutdown");
    rt.run_until_quiescent(Duration::from_secs(5)).expect("quiesce after Shutdown");

    assert!(
        rt.actor_ids_by_kind("counter").is_empty(),
        "counter should be auto-disposed once its last handle is dropped"
    );
    // D081/spec: returning None is just a state value — the supervisor stays alive.
    assert_eq!(
        rt.actor_ids_by_kind("supervisor").len(),
        1,
        "supervisor should still be alive (None is a state value, not termination)"
    );
}