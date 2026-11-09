//! Cross-thread Ref data-passing test with guaranteed thread separation.
//!
//! Verifies that owned data values can be sent between actors running on
//! different OS threads, and that the receiver correctly reads nested fields —
//! empirically validating the move-based value model under concurrency.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use wag_interp::{ActorLiveness, Value};
use wag_runtime::{prepare_runtime, Runtime, SchedulerConfig};

fn fixture(name: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.push("fixtures");
    p.push(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

#[test]
fn cross_thread_ref_data_passing() {
    let src = fixture("actors/cross_thread_ref.wag");
    let (_p, _t, loaded) = prepare_runtime(&src).expect("prepare_runtime");

    // Two workers → two OS threads.
    let rt = Runtime::new(loaded.clone(), SchedulerConfig { workers: 2 });

    // Pre-pin actor kinds to specific workers BEFORE any turns are processed.
    // This guarantees sender always runs on worker 0, receiver on worker 1.
    rt.pin_kind("sender", 0);
    rt.pin_kind("receiver", 1);

    // Spawn the receiver first so we can hand its handle to the sender. Both start
    // with an Int state (their behavior signatures are `Int, Msg -> Option Int`).
    let r_id = rt.spawn_actor("receiver", Value::Int(0)).expect("spawn receiver");
    let s_id = rt.spawn_actor("sender", Value::Int(0)).expect("spawn sender");

    // Build a handle to the receiver so the sender can send it a message. No `on_zero`
    // sink: this is a test-only handle; D078 abandonment semantics are not under test
    // (actors spawned from Rust persist in `inner.actors`).
    let recv_handle = Arc::new(ActorLiveness { id: r_id, on_zero: None });

    // Wrap the handle in a `Go` value and inject it into the sender's mailbox. The
    // sender then builds the cross-thread data value itself and sends it over —
    // this is the actual ownership handoff the test validates.
    let go_ctor = *loaded.ctor_by_name.get("Go").expect("Go ctor exists");
    rt.inject(s_id, Value::Data(go_ctor, vec![Value::Actor(recv_handle)])).expect("inject Go");

    // Run until all actors quiesce (no pending messages, no busy workers).
    rt.run_until_quiescent(Duration::from_secs(5))
        .expect("run_until_quiescent");

    // --- Verify data integrity: receiver stored the int from the Data cell. ---
    let receiver_ids = rt.actor_ids_by_kind("receiver");
    assert_eq!(receiver_ids.len(), 1, "expected exactly one @receiver actor");
    let r_id = receiver_ids[0];
    let state = rt.actor_state(r_id).expect("receiver alive");
    assert_eq!(
        state,
        Value::Int(42),
        "receiver state should be the int field from the cross-thread Data Ref"
    );

    // --- Verify thread separation: each actor ran exclusively on its pinned worker. ---
    let sender_ids = rt.actor_ids_by_kind("sender");
    assert_eq!(sender_ids.len(), 1, "expected exactly one @sender actor");
    let s_id = sender_ids[0];

    let sender_threads = rt.turn_threads(s_id);
    assert!(!sender_threads.is_empty(), "sender should have processed at least one turn");
    assert!(
        sender_threads.iter().all(|&t| t == 0),
        "sender turns ran on workers {:?}, expected all on worker 0",
        sender_threads
    );

    let receiver_threads = rt.turn_threads(r_id);
    assert!(!receiver_threads.is_empty(), "receiver should have processed at least one turn");
    assert!(
        receiver_threads.iter().all(|&t| t == 1),
        "receiver turns ran on workers {:?}, expected all on worker 1",
        receiver_threads
    );

    // Confirm the two actors were on genuinely different threads.
    assert_ne!(0u32, 1u32); // trivially true; documents intent.
}
