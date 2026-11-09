//! Core runtime types: `Runtime`, `Inner`, actor cells, ready state, config.

use std::collections::{HashMap, VecDeque};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use wag_interp::{ActorId, ConstructorId, FnId, LoadedProgram, Value, VmError};

use crate::builtin_io::{BuiltinCtors, IoConfig, IoEvent, NativeKind, NativeState};

/// Hard wall-clock ceiling for any program run (CLAUDE.md: 10-second hard limit
/// to protect the host). Every public run entry point caps its timeout at this.
pub const MAX_EXEC_TIMEOUT: Duration = Duration::from_secs(10);

/// Aggregated dev-mode execution stats across all actor turns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeStats {
    /// Total WAG function-body evaluations (steps) across all actor turns.
    pub total_steps: u64,
}

/// Fatal runtime failure (distinct from a clean `@main` exit).
#[derive(Debug)]
pub enum RuntimeError {
    Vm(VmError),
    Panic(String),
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuntimeError::Vm(e) => write!(f, "{e}"),
            RuntimeError::Panic(s) => write!(f, "runtime: {s}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl From<VmError> for RuntimeError {
    fn from(e: VmError) -> Self {
        RuntimeError::Vm(e)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ActorStatus {
    Idle,
    Queued,
    Running,
}

/// Role/name of an actor. Replaces the old stringly-typed `kind: String` so that
/// "is this @main?" is a pattern match rather than a string compare, and empty or
/// typo'd kinds are unrepresentable (§4.6).
#[derive(Debug, Clone)]
pub enum ActorKind {
    /// The `@main` entry actor (drives `ready.main_done`).
    Main,
    /// A named WAG or native actor (`@name`). Name is for diagnostics only —
    /// behavior and native-ness are resolved at spawn and stored separately.
    Named(String),
}

impl ActorKind {
    pub(crate) fn is_main(&self) -> bool {
        matches!(self, Self::Main)
    }
    /// Registry name used to look up the actor's behavior (`"main"` for `Main`).
    pub(crate) fn name(&self) -> &str {
        match self {
            Self::Main => "main",
            Self::Named(n) => n,
        }
    }
}

/// One mailbox entry. B2: payload is an O(1) ownership handoff (§4.3.4).
pub(crate) enum MailItem {
    /// Ordinary WAG message.
    Msg(Value),
    /// Refcount abandonment: last handle dropped, actor must stop (D078).
    Abandon,
    /// Built-in I/O completion (never constructed from WAG source).
    Io(IoEvent),
}

pub(crate) struct ActorCell {
    pub id: ActorId,
    /// Role/name of this actor (see `ActorKind`).
    pub kind: ActorKind,
    pub behavior: FnId,
    /// `End: Int` constructor id (for `@main`) used to detect an `End` message
    /// in the delivery loop (§5.4). `None` for every other actor.
    pub end_ctor: Option<ConstructorId>,
    /// `main_end: (State, Int) -> Unit` FnId (for `@main`) — the desugared `End`
    /// arm called on an `End` message before process exit (§5.4). `None` for
    /// every other actor.
    pub end: Option<FnId>,
    pub state: Value,
    pub mailbox: VecDeque<MailItem>,
    pub status: ActorStatus,
    /// True once terminated (None return, abandonment, or process exit).
    pub dead: bool,
    /// Immortal system actors (`@std`, `@file`, `@net`, conn) are exempt from
    /// refcount abandonment — their handles carry no `on_zero` sink (D078 decision 5).
    pub immortal: bool,
    /// Built-in IO actor kind, if any.
    pub native: Option<NativeKind>,
    pub native_state: Option<NativeState>,
    /// Weak reference to the canonical liveness Arc (D078). Lazily created on
    /// first `make_handle` call; all subsequent calls upgrade this Weak to get
    /// a clone. Using Weak ensures the cell itself doesn't keep the refcount
    /// above zero, so abandonment fires when all WAG-side handles are dropped.
    pub liveness_weak: Option<std::sync::Weak<wag_interp::ActorLiveness>>,
    /// Set when the last handle drops but messages are still in-flight.
    /// The actor drains its mailbox; post-turn check triggers teardown once empty.
    pub abandon_pending: bool,
}

impl ActorCell {
    /// Transition into `Running` — a worker calls this right after popping the actor
    /// off the ready queue. A Running actor is, by definition, not in the queue.
    pub(crate) fn mark_running(&mut self) {
        self.status = ActorStatus::Running;
    }

    /// Transition to `Idle` — no pending work (drained mailbox, error, or termination).
    /// An Idle actor is never in the ready queue.
    pub(crate) fn mark_idle(&mut self) {
        self.status = ActorStatus::Idle;
    }

    /// The single transition into `Queued`: set status and push onto the ready queue
    /// (plus wake a worker) as one unit, so "status == Queued" always implies this actor's
    /// id is in a ready queue. Every path that makes an actor runnable goes through here.
    /// Pinned actors (§5.5 test helper) are routed to their designated worker's queue.
    pub(crate) fn set_queued(&mut self, inner: &Inner) {
        self.status = ActorStatus::Queued;
        // Check pinning: route to the designated worker's private queue if available.
        let pinned_to = inner.pinned.lock().unwrap().get(&self.id).copied();
        if let Some(worker_idx) = pinned_to {
            let queues = inner.worker_queues.lock().unwrap();
            if let Some(q) = queues.get(worker_idx as usize) {
                q.lock().unwrap().push_back(self.id);
                drop(queues);
                inner.cvar.notify_all();
                return;
            }
        }
        // Not pinned (or worker pool not started) — use the shared queue.
        let mut ready = inner.ready.lock().unwrap();
        ready.queue.push_back(self.id);
        inner.cvar.notify_one();
    }

    /// After a turn (or native step): requeue if there is still work, else go idle.
    pub(crate) fn requeue_or_idle(&mut self, inner: &Inner) {
        if self.dead || self.mailbox.is_empty() {
            self.mark_idle();
        } else {
            self.set_queued(inner);
        }
    }
}

pub(crate) struct ReadyState {
    pub queue: VecDeque<ActorId>,
    /// Number of workers currently inside a turn (for idle detection).
    pub busy: usize,
    /// Set when `@main` terminates — process should exit.
    pub main_done: bool,
    /// Fatal error from a worker.
    pub fatal: Option<String>,
}

pub(crate) struct Inner {
    pub program: LoadedProgram,
    pub actors: RwLock<HashMap<ActorId, Mutex<ActorCell>>>,
    pub ready: Mutex<ReadyState>,
    pub cvar: Condvar,
    pub next_id: AtomicU32,
    /// Worker stop flag.
    pub stop: AtomicBool,
    pub workers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    pub config: SchedulerConfig,
    /// Actor id of `@main`, if spawned.
    pub main_id: Mutex<Option<ActorId>>,
    /// Live `@Name` bindings (`Std`, `Net`, …).
    pub named: Mutex<HashMap<String, ActorId>>,
    pub io: IoConfig,
    pub builtin_ctors: Mutex<Option<BuiltinCtors>>,
    /// Open TCP streams owned by Conn actors.
    pub streams: Mutex<HashMap<ActorId, TcpStream>>,
    /// Accepted connections waiting for a `RequestConnection` consumer.
    pub net_backlog: Mutex<VecDeque<TcpStream>>,
    pub net_port: Arc<Mutex<Option<u16>>>,
    pub io_stop: Arc<AtomicBool>,
    /// Aggregated dev-mode step count across all actor turns (allocations are
    /// read from the process-global heap in `stats()`).
    pub total_steps: AtomicU64,
    /// Actor → worker-index pinning map (test helper for guaranteed thread separation).
    pub pinned: Mutex<HashMap<ActorId, u32>>,
    /// Kind-name → worker-index pre-pinning (applied automatically at spawn time).
    pub pinned_kinds: Mutex<HashMap<String, u32>>,
    /// Per-actor log of which worker index processed each turn (test verification).
    pub turn_log: Mutex<HashMap<ActorId, Vec<u32>>>,
    /// Per-worker ready queues (populated in `start_workers`; empty in drive mode).
    pub worker_queues: Mutex<Vec<Mutex<VecDeque<ActorId>>>>,
}

#[derive(Clone, Debug, Default)]
pub struct SchedulerConfig {
    /// Defaults to caller-thread `drive` mode (workers=0). A multi-thread pool
    /// is available by setting `workers >= 1`; tests use 0 for determinism.
    pub workers: usize,
}

/// Running (or runable) actor system for one loaded program.
pub struct Runtime {
    pub(crate) inner: Arc<Inner>,
}
