//! Core actor scheduling: worker pool, turn processing, lifecycle.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use std::collections::VecDeque;

use wag_interp::{ActorId, Engine, Value};

use crate::builtin_io::{BuiltinCtors, IoConfig};
use crate::host::TurnHost;
use crate::native::{install_builtins, process_native};
use crate::runtime::{
    ActorCell, ActorKind, ActorStatus, Inner, MailItem, ReadyState, Runtime, RuntimeError,
    RuntimeStats, SchedulerConfig, MAX_EXEC_TIMEOUT,
};

impl Runtime {
    pub fn new(program: wag_interp::LoadedProgram, config: SchedulerConfig) -> Self {
        Self::new_with_io(program, config, IoConfig::default())
    }

    pub fn new_with_io(program: wag_interp::LoadedProgram, config: SchedulerConfig, io: IoConfig) -> Self {
        let ctors = BuiltinCtors::from_program(&program).ok();
        let inner = Arc::new(Inner {
            program,
            actors: std::sync::RwLock::new(std::collections::HashMap::new()),
            ready: std::sync::Mutex::new(ReadyState {
                queue: std::collections::VecDeque::new(),
                busy: 0,
                main_done: false,
                fatal: None,
            }),
            cvar: std::sync::Condvar::new(),
            next_id: std::sync::atomic::AtomicU32::new(1),
            stop: std::sync::atomic::AtomicBool::new(false),
            workers: std::sync::Mutex::new(Vec::new()),
            config,
            main_id: std::sync::Mutex::new(None),
            named: std::sync::Mutex::new(std::collections::HashMap::new()),
            io,
            builtin_ctors: std::sync::Mutex::new(ctors),
            streams: std::sync::Mutex::new(std::collections::HashMap::new()),
            net_backlog: std::sync::Mutex::new(std::collections::VecDeque::new()),
            net_port: Arc::new(std::sync::Mutex::new(None)),
            io_stop: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            total_steps: std::sync::atomic::AtomicU64::new(0),
            pinned: std::sync::Mutex::new(std::collections::HashMap::new()),
            pinned_kinds: std::sync::Mutex::new(std::collections::HashMap::new()),
            turn_log: std::sync::Mutex::new(std::collections::HashMap::new()),
            worker_queues: std::sync::Mutex::new(Vec::new()),
        });
        let rt = Runtime { inner };
        // Install native @SysStd / @net cells + the WAG prelude @std actor
        // (D043 two-layer I/O) when the prelude was merged (§5.5.7).
        if let Err(e) = install_builtins(&rt.inner) {
            // Surface later via fatal if needed; tests assert install succeeds.
            let mut ready = rt.inner.ready.lock().unwrap();
            ready.fatal = Some(e.to_string());
        }
        rt
    }

    /// Bound port for `@net` listener once started (test helper).
    pub fn net_port(&self) -> Option<u16> {
        *self.inner.net_port.lock().unwrap()
    }

    pub fn io_config(&self) -> &IoConfig {
        &self.inner.io
    }

    pub fn named_actor(&self, name: &str) -> Option<ActorId> {
        self.inner.named.lock().unwrap().get(name).copied()
    }

    pub fn program(&self) -> &wag_interp::LoadedProgram {
        &self.inner.program
    }


    /// Pin an actor to a specific worker thread index (test helper for guaranteed
    /// cross-thread verification). Subsequent enqueues route to that worker's private queue.
    pub fn pin_actor(&self, id: ActorId, worker_idx: u32) {
        self.inner.pinned.lock().unwrap().insert(id, worker_idx);
    }

    /// Pre-pin all future actors of a given kind to a worker index (test helper).
    /// Applied automatically in `spawn_actor_inner` so actors spawned by `@main`
    /// are pinned before their first turn is processed.
    pub fn pin_kind(&self, kind: &str, worker_idx: u32) {
        self.inner.pinned_kinds.lock().unwrap().insert(kind.to_string(), worker_idx);
    }

    /// Worker indices that processed turns for this actor (test verification).
    /// Empty in drive mode (workers=0) — only populated by the multi-worker pool.
    pub fn turn_threads(&self, id: ActorId) -> Vec<u32> {
        self.inner.turn_log.lock().unwrap().get(&id).cloned().unwrap_or_default()
    }

    /// Live actor IDs whose kind name matches (test helper for locating spawned actors).
    pub fn actor_ids_by_kind(&self, kind: &str) -> Vec<ActorId> {
        let actors = self.inner.actors.read().unwrap();
        actors
            .iter()
            .filter(|(_, mx)| {
                let cell = mx.lock().unwrap();
                !cell.dead && cell.kind.name() == kind
            })
            .map(|(id, _)| *id)
            .collect()
    }

    pub fn start_workers(&self) {
        let n = self.inner.config.workers;
        if n == 0 {
            return; // caller-thread / `drive` mode
        }
        let mut handles = self.inner.workers.lock().unwrap();
        if !handles.is_empty() {
            return;
        }
        // Initialize per-worker queues (one private queue per worker for pinned actors).
        {
            let mut wq = self.inner.worker_queues.lock().unwrap();
            wq.clear();
            for _ in 0..n {
                wq.push(std::sync::Mutex::new(VecDeque::new()));
            }
        }
        for i in 0..n {
            let inner = Arc::clone(&self.inner);
            let h = thread::Builder::new()
                .name(format!("wag-worker-{i}"))
                .spawn(move || worker_loop(inner, i as u32))
                .expect("spawn worker");
            handles.push(h);
        }
    }

    /// Process ready actors on the caller thread until idle or `main_done`.
    pub fn drive(&self, max_turns: usize) -> Result<(), RuntimeError> {
        for _ in 0..max_turns {
            let id = {
                let mut ready = self.inner.ready.lock().unwrap();
                if let Some(ref err) = ready.fatal {
                    return Err(RuntimeError::Panic(err.clone()));
                }
                if ready.main_done && ready.queue.is_empty() {
                    return Ok(());
                }
                match ready.queue.pop_front() {
                    Some(id) => {
                        ready.busy += 1;
                        id
                    }
                    None => {
                        ready.busy = 0;
                        return Ok(());
                    }
                }
            };
            let result = process_one(&self.inner, id);
            {
                let mut ready = self.inner.ready.lock().unwrap();
                ready.busy = ready.busy.saturating_sub(1);
            }
            result?;
        }
        Ok(())
    }

    /// Spawn an actor of the given kind with initial state. Delivers `Start` if present.
    pub fn spawn_actor(&self, kind: &str, init_state: Value) -> Result<ActorId, RuntimeError> {
        // Boundary translation from a registry name to an actor role. `"main"`
        // names the entry actor; everything else is a named user/native actor.
        let kind = if kind == "main" {
            ActorKind::Main
        } else {
            ActorKind::Named(kind.to_string())
        };
        spawn_actor_inner(&self.inner, kind, init_state, None)
    }

    /// Spawn `@main` with `None` initial state and the given argv payload,
    /// triggering the desugared `main_boot` function (§5.4, D081 v2).
    pub fn spawn_main(&self, argv: Value) -> Result<ActorId, RuntimeError> {
        let none_ctor = self
            .program()
            .ctor_by_name
            .get("None")
            .copied()
            .ok_or_else(|| RuntimeError::Panic("program has no `None` ctor".into()))?;
        let id = spawn_actor_inner(
            &self.inner,
            ActorKind::Main,
            Value::Data(none_ctor, vec![]),
            Some(argv),
        )?;
        *self.inner.main_id.lock().unwrap() = Some(id);
        Ok(id)
    }

    /// Enqueue a message from an external (test) sender. Uses a synthetic sender id 0.
    pub fn inject(&self, target: ActorId, msg: Value) -> Result<(), RuntimeError> {
        enqueue_msg(&self.inner, ActorId(0), target, msg)
    }

    /// Run until `@main` terminates or `timeout`.
    pub fn run_until_main_exits(&self, timeout: Duration) -> Result<(), RuntimeError> {
        let timeout = timeout.min(MAX_EXEC_TIMEOUT); // hard 10s ceiling (CLAUDE.md)
        let start = std::time::Instant::now();
        if self.inner.config.workers == 0 {
            while start.elapsed() <= timeout {
                {
                    let ready = self.inner.ready.lock().unwrap();
                    if let Some(ref err) = ready.fatal {
                        return Err(RuntimeError::Panic(err.clone()));
                    }
                    if ready.main_done {
                        // Drain pending messages (e.g. @std writes) before exiting,
                        // then bulk-teardown all remaining actors (D078 process-exit protocol).
                        drop(ready);
                        self.drain_single_threaded()?;
                        self.bulk_teardown();
                        return Ok(());
                    }
                    if ready.queue.is_empty() {
                        // No work and main not done — still waiting for external inject.
                        drop(ready);
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                }
                self.drive(64)?;
            }
            return Err(RuntimeError::Panic(format!(
                "timeout after {timeout:?} waiting for @main to exit"
            )));
        }

        self.start_workers();
        while start.elapsed() <= timeout {
            let outcome = {
                let ready = self.inner.ready.lock().unwrap();
                ready.fatal.as_ref().map(|err| Err(RuntimeError::Panic(err.clone())))
            };
            if let Some(result) = outcome {
                self.shutdown();
                return result;
            }
            // Check main_done to trigger drain of pending messages.
            let main_done = {
                let ready = self.inner.ready.lock().unwrap();
                ready.main_done
            };
            if main_done {
                self.drain_multi_worker()?;
                self.shutdown();
                self.bulk_teardown();
                return Ok(());
            }
            thread::sleep(Duration::from_millis(1));
        }
        self.shutdown();
        Err(RuntimeError::Panic(format!(
            "timeout after {timeout:?} waiting for @main to exit"
        )))
    }

    /// Single-threaded drain: process remaining ready-queue turns until quiescent.
    fn drain_single_threaded(&self) -> Result<(), RuntimeError> {
        let start = std::time::Instant::now();
        while start.elapsed() <= MAX_EXEC_TIMEOUT {
            let ready = self.inner.ready.lock().unwrap();
            if let Some(ref err) = ready.fatal {
                return Err(RuntimeError::Panic(err.clone()));
            }
            if ready.queue.is_empty() && ready.busy == 0 {
                return Ok(());
            }
            drop(ready);
            self.drive(64)?;
        }
        // Drain timeout — proceed with exit (best-effort).
        Ok(())
    }

    /// Multi-worker drain: wait until all ready queues are empty and no worker is busy.
    fn drain_multi_worker(&self) -> Result<(), RuntimeError> {
        let start = std::time::Instant::now();
        while start.elapsed() <= MAX_EXEC_TIMEOUT {
            let ready = self.inner.ready.lock().unwrap();
            if let Some(ref err) = ready.fatal {
                return Err(RuntimeError::Panic(err.clone()));
            }
            if ready.queue.is_empty() && ready.busy == 0 && Self::worker_queues_empty(&self.inner) {
                return Ok(());
            }
            drop(ready);
            thread::sleep(Duration::from_millis(1));
        }
        // Drain timeout — proceed with exit (best-effort).
        Ok(())
    }

    /// True when all per-worker private queues are empty (pinned-actor routing).
    fn worker_queues_empty(inner: &Inner) -> bool {
        let queues = inner.worker_queues.lock().unwrap();
        queues.iter().all(|q| q.lock().unwrap().is_empty())
    }

    /// Drive until the ready queue drains and no worker is busy (test helper).
    pub fn run_until_quiescent(&self, timeout: Duration) -> Result<(), RuntimeError> {
        let timeout = timeout.min(MAX_EXEC_TIMEOUT); // hard 10s ceiling (CLAUDE.md)
        let start = std::time::Instant::now();
        if self.inner.config.workers == 0 {
            while start.elapsed() <= timeout {
                self.drive(256)?;
                let ready = self.inner.ready.lock().unwrap();
                if let Some(ref err) = ready.fatal {
                    return Err(RuntimeError::Panic(err.clone()));
                }
                if ready.queue.is_empty() && ready.busy == 0 {
                    return Ok(());
                }
            }
            return Err(RuntimeError::Panic(format!(
                "timeout after {timeout:?} waiting for quiescence"
            )));
        }

        self.start_workers();
        while start.elapsed() <= timeout {
            {
                let ready = self.inner.ready.lock().unwrap();
                if let Some(ref err) = ready.fatal {
                    return Err(RuntimeError::Panic(err.clone()));
                }
                if ready.queue.is_empty() && ready.busy == 0 && Self::worker_queues_empty(&self.inner) {
                    return Ok(());
                }
            }
            thread::sleep(Duration::from_millis(1));
        }
        Err(RuntimeError::Panic(format!(
            "timeout after {timeout:?} waiting for quiescence"
        )))
    }

    pub fn shutdown(&self) {
        self.inner.stop.store(true, Ordering::SeqCst);
        self.inner.io_stop.store(true, Ordering::SeqCst);
        self.inner.cvar.notify_all();
        // Take handles then join outside any other runtime lock.
        let handles = {
            let mut g = self.inner.workers.lock().unwrap();
            std::mem::take(&mut *g)
        };
        for h in handles {
            // Worker loops check `stop` at least every 50ms wait_timeout.
            let _ = h.join();
        }
    }

    /// Process-exit: teardown all remaining actors (D078). Called after drain + shutdown.
    fn bulk_teardown(&self) {
        let ids: Vec<ActorId> = {
            let actors = self.inner.actors.read().unwrap();
            actors.keys().copied().collect()
        };
        for id in ids {
            teardown_actor(&self.inner, id);
        }
    }

    /// Aggregated dev-mode execution stats across all turns.
    pub fn stats(&self) -> RuntimeStats {
        RuntimeStats {
            total_steps: self.inner.total_steps.load(Ordering::Relaxed),
        }
    }

    /// Snapshot actor state (test helper). Moves the state out with a `Unit`
    /// placeholder — only called at quiescence points in tests, never mid-run.
    pub fn actor_state(&self, id: ActorId) -> Option<Value> {
        let actors = self.inner.actors.read().unwrap();
        let cell = actors.get(&id)?;
        let mut g = cell.lock().unwrap();
        if g.dead {
            None
        } else {
            Some(std::mem::replace(&mut g.state, Value::Unit))
        }
    }

    pub fn is_dead(&self, id: ActorId) -> bool {
        let actors = self.inner.actors.read().unwrap();
        actors
            .get(&id)
            .map(|c| c.lock().unwrap().dead)
            .unwrap_or(true)
    }

    /// Count live (non-dead, non-immortal) user actors. Test helper for
    /// verifying cascading abandonment (D078/D081).
    pub fn live_actor_count(&self) -> usize {
        let actors = self.inner.actors.read().unwrap();
        actors
            .values()
            .filter(|mx| {
                let c = mx.lock().unwrap();
                !c.dead && !c.immortal
            })
            .count()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) fn spawn_actor_inner(
    inner: &Arc<Inner>,
    kind: ActorKind,
    init_state: Value,
    start_payload: Option<Value>,
) -> Result<ActorId, RuntimeError> {
    let name = kind.name().to_string();
    let entry = inner
        .program
        .actor(&name)
        .ok_or_else(|| RuntimeError::Panic(format!("unknown actor kind `@{name}`")))?
        .clone();

    let id = ActorId(inner.next_id.fetch_add(1, Ordering::SeqCst));
    let immortal = kind.is_main();

    // Create the cell and insert it into the map BEFORE calling `boot`, so
    // that self-sends during `main_boot` (e.g. `self@ (End 0)`) can be
    // delivered to the mailbox (§5.4).
    let cell = ActorCell {
        id,
        kind,
        behavior: entry.behavior,
        end_ctor: entry.end_ctor,
        end: entry.end,
        state: init_state.clone(),
        mailbox: std::collections::VecDeque::new(),
        status: ActorStatus::Idle,
        dead: false,
        immortal,
        native: None,
        native_state: None,
        liveness_weak: None,
        abandon_pending: false,
    };

    {
        let mut actors = inner.actors.write().unwrap();
        actors.insert(id, std::sync::Mutex::new(cell));
    }

    // For `@main`, obtain the initial state by calling the desugared `boot`
    // function (§5.4). `boot: List String -> State` runs the `Start` arm with
    // argv. The cell already exists so self-sends during boot are delivered.
    if immortal {
        if let (Some(boot), Some(argv)) = (entry.boot, start_payload.clone()) {
            let mut engine = Engine::new(&inner.program);
            let mut host = TurnHost {
                inner: Arc::clone(&inner),
                self_id: id,
            };
            let new_state = engine
                .call_with_host(&mut host, id, boot, vec![argv])
                .map_err(|e| RuntimeError::from(e))?;
            let actors = inner.actors.read().unwrap();
            if let Some(mx) = actors.get(&id) {
                let mut c = mx.lock().unwrap();
                c.state = new_state;
            }
        }
    }

    // Apply kind-based pre-pinning (test helper for guaranteed thread separation).
    {
        let pinned_kinds = inner.pinned_kinds.lock().unwrap();
        if let Some(&worker_idx) = pinned_kinds.get(&name) {
            inner.pinned.lock().unwrap().insert(id, worker_idx);
        }
    }

    // Deliver Start immediately if present (§5.5.6). `@main` uses `boot` (§5.4)
    // instead, so skip the Start delivery for it. `kind` is moved into the cell
    // above, so reuse the `immortal` (= `kind.is_main()`) boolean.
    if !immortal {
        if let Some(start_id) = entry.start_ctor {
            let ctor = &inner.program.ctors[start_id.0 as usize];
            match start_payload {
                Some(payload) => {
                    // Start with payload (argv for `@main`): wrap in the constructor.
                    let fields = if ctor.arity == 0 {
                        Vec::new()
                    } else {
                        vec![payload]
                    };
                    let msg = Value::data_value(ctor.id, fields);
                    enqueue_msg(inner, ActorId(0), id, msg)?;
                }
                None => {
                    if ctor.arity == 0 {
                        let msg = Value::Data(ctor.id, Vec::new());
                        enqueue_msg(inner, ActorId(0), id, msg)?;
                    } else {
                        // Start requires payload but none given — skip.
                        return Ok(id);
                    }
                }
            }
        }
    }

    Ok(id)
}

pub(crate) fn enqueue_msg(
    inner: &Arc<Inner>,
    _sender: ActorId,
    target: ActorId,
    msg: Value,
) -> Result<(), RuntimeError> {
    // B2: send is O(1) pointer handoff — just push into the target's mailbox.
    let actors = inner.actors.read().unwrap();
    let Some(tcell) = actors.get(&target) else {
        return Ok(());
    };
    let mut t = tcell.lock().unwrap();
    if t.dead {
        return Ok(());
    }

    t.mailbox.push_back(MailItem::Msg(msg));
    enqueue_ready(inner, &mut t);
    Ok(())
}

pub(crate) fn enqueue_ready(inner: &Inner, cell: &mut ActorCell) {
    if cell.dead || cell.status != ActorStatus::Idle {
        return;
    }
    cell.set_queued(inner);
}

fn worker_loop(inner: Arc<Inner>, idx: u32) {
    loop {
        if inner.stop.load(Ordering::SeqCst) {
            return;
        }
        let id = {
            let mut ready = inner.ready.lock().unwrap();
            loop {
                if inner.stop.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(ref _e) = ready.fatal {
                    return;
                }
                // Check this worker's private queue first (pinned actors).
                let own_id = {
                    let queues = inner.worker_queues.lock().unwrap();
                    queues.get(idx as usize).and_then(|q| q.lock().unwrap().pop_front())
                };
                if let Some(id) = own_id {
                    ready.busy += 1;
                    break id;
                }
                // Fall back to the shared queue (unpinned actors).
                if let Some(id) = ready.queue.pop_front() {
                    ready.busy += 1;
                    break id;
                }
                let (guard, wait_res) = inner
                    .cvar
                    .wait_timeout(ready, Duration::from_millis(50))
                    .unwrap();
                ready = guard;
                if wait_res.timed_out() && inner.stop.load(Ordering::SeqCst) {
                    return;
                }
            }
        };

        // Record which worker processed this turn (test verification).
        {
            let mut log = inner.turn_log.lock().unwrap();
            log.entry(id).or_default().push(idx);
        }

        if let Err(e) = process_one(&inner, id) {
            let mut ready = inner.ready.lock().unwrap();
            ready.fatal = Some(e.to_string());
            ready.busy = ready.busy.saturating_sub(1);
            inner.cvar.notify_all();
            return;
        }

        {
            let mut ready = inner.ready.lock().unwrap();
            ready.busy = ready.busy.saturating_sub(1);
        }
    }
}

fn process_one(inner: &Arc<Inner>, id: ActorId) -> Result<(), RuntimeError> {
    // Take mailbox item and run behavior with a short lock release pattern.
    let (behavior, end_ctor, end_fn, state, item, native) = {
        let actors = inner.actors.read().unwrap();
        let Some(cell_mx) = actors.get(&id) else {
            return Ok(());
        };
        let mut cell = cell_mx.lock().unwrap();
        if cell.dead {
            cell.mark_idle();
            return Ok(());
        }
        cell.mark_running();

        let item = cell.mailbox.pop_front();
        match item {
            None => {
                cell.mark_idle();
                return Ok(());
            }
            Some(MailItem::Abandon) => {
                // Refcount abandonment: last handle dropped (D078).
                drop(cell);
                drop(actors);
                teardown_actor(inner, id);
                return Ok(());
            }
            Some(other) => (
                cell.behavior,
                cell.end_ctor,
                cell.end,
                // Move state out instead of deep-cloning per turn: success paths
                // replace it (or tear the actor down), only a failed turn restores it.
                std::mem::replace(&mut cell.state, Value::Unit),
                other,
                cell.native,
            )
        }
    };

    // Native built-in actors (§5.5.7) never use wag state; drop the moved-out value.
    if let Some(nk) = native {
        let result = process_native(inner, id, nk, item);
        return result;
    }

    let msg = match item {
        MailItem::Msg(m) => m,
        MailItem::Io(_) => {
            return Err(RuntimeError::Panic(
                "Io event delivered to non-native actor".into(),
            ));
        }
        MailItem::Abandon => unreachable!("handled above"),
    };

    // `@main` End arm (§5.4): when the dequeued message matches the `End`
    // constructor, call `end (state, code)` and exit the process with `code`.
    // Guarded on `end_fn.is_some()` so only `@main` (which has the `main_end`
    // FnId) is affected — ordinary actors match `End` in their own `run` body.
    if let Some(end_fn) = end_fn {
        if end_ctor == msg.data_ctor() {
            let code = msg
                .data_fields()
                .and_then(|f| f.first())
                .cloned()
                .unwrap_or(Value::Int(0));
            let mut engine = Engine::new(&inner.program);
            let mut host = TurnHost {
                inner: Arc::clone(inner),
                self_id: id,
            };
            // `end (state, code)` -> Unit; the process exits after the arm returns.
            let _ = engine.call_with_host(&mut host, id, end_fn, vec![state, code]);
            teardown_actor(inner, id);
            return Ok(());
        }
    }

    let mut engine = Engine::new(&inner.program);
    let mut host = TurnHost {
        inner: Arc::clone(inner),
        self_id: id,
    };
    let result = engine.call_with_host(&mut host, id, behavior, vec![state.clone(), msg]);
    // Aggregate dev-mode stats (per-turn steps) for analysis.
    inner.total_steps.fetch_add(engine.steps, Ordering::Relaxed);

    match result {
        Err(e) => {
            let actors = inner.actors.read().unwrap();
            if let Some(mx) = actors.get(&id) {
                let mut cell = mx.lock().unwrap();
                cell.mark_idle();
            }
            return Err(e.into());
        }
        Ok(v) => {
            // Behavior returns State directly (§5.4); no Option unwrapping.
            let new_state = v;
            // Old state is dropped; Rust reclaims recursively.
            drop(state);

            let actors = inner.actors.read().unwrap();
            if let Some(mx) = actors.get(&id) {
                let mut cell = mx.lock().unwrap();
                cell.state = new_state;
                // Post-turn abandonment check: last handle dropped while
                // messages were in-flight; now mailbox is drained (D078).
                if cell.abandon_pending && cell.mailbox.is_empty() {
                    drop(cell);
                    drop(actors);
                    teardown_actor(inner, id);
                } else {
                    cell.requeue_or_idle(inner);
                }
            }
        }
    }
    Ok(())
}

/// Shared teardown for all death paths (abandonment, process exit).
/// Ordering (deadlock-free): mark dead + clear mailbox under cell lock → release cell lock →
/// write-lock actor map and remove(id) → drop the cell with no locks held so cascading Arc
/// drops safely enqueue further abandons. If kind is main, set ready.main_done + notify.
fn teardown_actor(inner: &Inner, id: ActorId) {
    // Phase 1: mark dead under cell lock; take out final state and queued
    // messages so they can be freed from the heap with no locks held (D078).
    let (kind, state, msgs) = {
        let actors = inner.actors.read().unwrap();
        let Some(mx) = actors.get(&id) else { return };
        let mut cell = mx.lock().unwrap();
        if cell.dead {
            return; // already torn down
        }
        cell.dead = true;
        cell.mark_idle();
        let msgs: Vec<Value> = cell.mailbox.drain(..).filter_map(|item| match item {
            MailItem::Msg(v) => Some(v),
            _ => None,
        }).collect();
        (cell.kind.clone(), std::mem::replace(&mut cell.state, Value::Unit), msgs)
    };

    // Phase 2: remove from map. The returned Mutex<ActorCell> is dropped after the
    // write-lock is released, so cascading Arc drops fire on_zero sinks with no
    // locks held.
    let _removed = {
        let mut actors = inner.actors.write().unwrap();
        actors.remove(&id)
    };

    // Phase 3: drop final state + queued messages. Nested actor handles drop
    // here, driving refcounts to zero and enqueuing further Abandon items —
    // cascading abandonment (D078).
    drop(state);
    for m in msgs {
        drop(m);
    }

    if kind.is_main() {
        let mut ready = inner.ready.lock().unwrap();
        ready.main_done = true;
        inner.cvar.notify_all();
    }
}
