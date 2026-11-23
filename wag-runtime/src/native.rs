//! Native built-in actor processing: `@SysStd`, `@SysNet`, `@SysConn` (§5.5.7).

use std::io::Write;
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use wag_interp::{
    ActorId, ActorLiveness, ConstructorId, FnId, Value,
};

use crate::builtin_io::{
    self, field0_actor, field0_list, field_actor, field_int, field_ref, match_data_ctor,
    write_stdout, IoEvent, NativeKind, NativeState,
};
use crate::reply;
use crate::runtime::{ActorCell, ActorKind, ActorStatus, Inner, MailItem, RuntimeError};
use crate::scheduler::{enqueue_msg, enqueue_ready, spawn_actor_inner};

/// Install native `@SysStd` / `@SysNet` cells + the WAG prelude `@std`/`@net` actors.
pub(crate) fn install_builtins(inner: &Arc<Inner>) -> Result<(), RuntimeError> {
    let scripted = inner.io.stdin_lines.clone();
    // Native raw-protocol actors (D043): addressed only by the WAG prelude
    // (`@std` → `@SysStd`; `@net` → `@SysNet`; `@SysConn` is spawned per
    // connection by `@SysNet` and is never named in the registry).
    let sysstd_id = spawn_native(
        inner,
        "SysStd",
        NativeKind::Std,
        NativeState::std(scripted),
    )?;
    let sysfile_id = spawn_native(inner, "SysFile", NativeKind::File, NativeState::file())?;
    let sysnet_id = spawn_native(inner, "SysNet", NativeKind::Net, NativeState::net())?;
    {
        let mut named = inner.named.lock().unwrap();
        named.insert("SysStd".into(), sysstd_id);
        named.insert("SysFile".into(), sysfile_id);
        named.insert("SysNet".into(), sysnet_id);
    }
    // WAG prelude `@std` / `@file` / `@net` actors (D043, D082): ordinary user
    // actors, present only when the prelude was merged into the program.
    // Spawned with the initial state `None` (no pending request). Nothing
    // reaches the raw layer without them.
    for name in ["std", "file", "net"] {
        if inner.program.actor(name).is_some() {
            let none_id = inner.program.opt_none_id();
            let id = spawn_actor_inner(inner, ActorKind::Named(name.to_string()), Value::Data(none_id, vec![]), None)?;
            // WAG prelude actors are immortal (D078 decision 5).
            {
                let actors = inner.actors.read().unwrap();
                if let Some(mx) = actors.get(&id) {
                    mx.lock().unwrap().immortal = true;
                }
            }
            inner.named.lock().unwrap().insert(name.into(), id);
        }
    }
    Ok(())
}

pub(crate) fn spawn_native(
    inner: &Arc<Inner>,
    kind: &str,
    native: NativeKind,
    native_state: NativeState,
) -> Result<ActorId, RuntimeError> {
    let id = ActorId(inner.next_id.fetch_add(1, Ordering::SeqCst));
    let cell = ActorCell {
        id,
        kind: ActorKind::Named(kind.to_string()),
        behavior: FnId(u32::MAX), // never called
        end_ctor: None,
        end: None,
        state: Value::Unit,
        mailbox: std::collections::VecDeque::new(),
        status: ActorStatus::Idle,
        dead: false,
        immortal: true,
        native: Some(native),
        native_state: Some(native_state),
        liveness_weak: None,
        abandon_pending: false,
    };
    {
        let mut actors = inner.actors.write().unwrap();
        actors.insert(id, std::sync::Mutex::new(cell));
    }
    Ok(id)
}

pub(crate) fn enqueue_io(inner: &Arc<Inner>, target: ActorId, ev: IoEvent) -> Result<(), RuntimeError> {
    let actors = inner.actors.read().unwrap();
    let Some(mx) = actors.get(&target) else {
        return Ok(());
    };
    let mut cell = mx.lock().unwrap();
    if cell.dead {
        return Ok(());
    }
    // Validate at send time (not delivery): Io events are only legal for native actors.
    if cell.native.is_none() {
        return Err(RuntimeError::Panic(format!(
            "Io event sent to non-native actor {:?}",
            cell.kind,
        )));
    }
    cell.mailbox.push_back(MailItem::Io(ev));
    enqueue_ready(inner, &mut cell);
    Ok(())
}

pub(crate) fn finish_native_turn(inner: &Arc<Inner>, id: ActorId) -> Result<(), RuntimeError> {
    let actors = inner.actors.read().unwrap();
    if let Some(mx) = actors.get(&id) {
        let mut cell = mx.lock().unwrap();
        cell.requeue_or_idle(inner);
    }
    Ok(())
}

pub(crate) fn process_native(
    inner: &Arc<Inner>,
    id: ActorId,
    kind: NativeKind,
    item: MailItem,
) -> Result<(), RuntimeError> {
    match kind {
        NativeKind::Std => process_std(inner, id, item)?,
        NativeKind::File => process_file(inner, id, item)?,
        NativeKind::Net => process_net(inner, id, item)?,
        NativeKind::Conn => process_conn(inner, id, item)?,
    }
    finish_native_turn(inner, id)
}

fn process_std(
    inner: &Arc<Inner>,
    id: ActorId,
    item: MailItem,
) -> Result<(), RuntimeError> {
    let ctors = inner
        .builtin_ctors
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| RuntimeError::Panic("@SysStd: missing builtin ctors".into()))?;
    let read_line = ctors
        .read_line
        .ok_or_else(|| RuntimeError::Panic("@SysStd: prelude `ReadLine` ctor missing".into()))?;
    let write_line = ctors.write_line.ok_or_else(|| {
        RuntimeError::Panic("@SysStd: prelude `WriteLine` ctor missing".into())
    })?;

    match item {
        MailItem::Msg(msg) => {
            let Some((cid, fields)) = match_data_ctor(&msg) else {
                return Err(RuntimeError::Panic(format!(
                    "@SysStd: expected Data message, got {msg:?}"
                )));
            };
            if cid == read_line {
                // Raw protocol: the requester is the WAG @std actor (its
                // `ReadLine self`), which relays to the user's replyTo.
                let requester = field0_actor(fields)
                    .ok_or_else(|| RuntimeError::Panic("ReadLine field not Actor".into()))?;
                // Newest request wins (§5.5.7).
                let mut deliver_now: Option<IoEvent> = None;
                {
                    let actors = inner.actors.read().unwrap();
                    let mx = actors.get(&id).unwrap();
                    let mut cell = mx.lock().unwrap();
                    if let Some(NativeState::Std {
                        pending,
                        eof,
                        scripted,
                        stdin_thread_started,
                    }) = cell.native_state.as_mut()
                    {
                        *pending = Some(requester);
                        if *eof {
                            deliver_now = Some(IoEvent::StdEof);
                        } else if let Some(q) = scripted.as_mut() {
                            if let Some(line) = q.pop_front() {
                                deliver_now = Some(IoEvent::StdLine(line));
                            } else {
                                *eof = true;
                                deliver_now = Some(IoEvent::StdEof);
                            }
                        } else if !*stdin_thread_started {
                            *stdin_thread_started = true;
                            let inner2 = Arc::clone(inner);
                            let std_id = id;
                            let stop = Arc::clone(&inner.io_stop);
                            builtin_io::start_stdin_thread(stop, move |ev| {
                                let _ = enqueue_io(&inner2, std_id, ev);
                            });
                        }
                    }
                }
                if let Some(ev) = deliver_now {
                    // Deliver through mailbox path (not direct state write).
                    enqueue_io(inner, id, ev)?;
                }
            } else if cid == write_line {
                let val = builtin_io::field_ref(fields, 0)
                    .ok_or_else(|| RuntimeError::Panic("WriteLine missing String".into()))?;
                write_stdout(&inner.io, &inner.program, val).map_err(RuntimeError::from)?;
            } else {
                return Err(RuntimeError::Panic(format!(
                    "@SysStd: unknown message ctor {}",
                    cid.0
                )));
            }
        }
        MailItem::Io(ev) => {
            if !matches!(ev, IoEvent::StdLine(_) | IoEvent::StdEof) {
                return Ok(());
            }
            let pending = {
                let actors = inner.actors.read().unwrap();
                let mx = actors.get(&id).unwrap();
                let mut cell = mx.lock().unwrap();
                if let Some(NativeState::Std {
                    pending, eof, ..
                }) = cell.native_state.as_mut()
                {
                    if matches!(ev, IoEvent::StdEof) {
                        *eof = true;
                    }
                    pending.take()
                } else {
                    None
                }
            };
            let Some(requester) = pending else {
                return Ok(());
            };
            let msg = match ev {
                IoEvent::StdLine(line) => reply::make_chunk(&inner.program, &line),
                IoEvent::StdEof => reply::make_end(&inner.program),
                _ => unreachable!(),
            }
            .map_err(RuntimeError::from)?;
            enqueue_msg(inner, id, requester, msg)?;
        }
        MailItem::Abandon => {} // system actors are immortal; should not receive this
    }
    Ok(())
}

/// `@SysFile` (D082): synchronous file reads. The requester is the WAG `@file`
/// actor (its `ReadFileRaw path self`), which relays to the user's `replyTo`.
/// No pending slot: the reply is sent directly by this handler (§5.5.7).
fn process_file(
    inner: &Arc<Inner>,
    id: ActorId,
    item: MailItem,
) -> Result<(), RuntimeError> {
    let ctors = inner
        .builtin_ctors
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| RuntimeError::Panic("@SysFile: missing builtin ctors".into()))?;
    let read_file = ctors
        .read_file
        .ok_or_else(|| RuntimeError::Panic("@SysFile: prelude `ReadFileRaw` ctor missing".into()))?;

    match item {
        MailItem::Msg(msg) => {
            let Some((cid, fields)) = match_data_ctor(&msg) else {
                return Err(RuntimeError::Panic(format!(
                    "@SysFile: expected Data message, got {msg:?}"
                )));
            };
            if cid != read_file {
                return Err(RuntimeError::Panic(format!(
                    "@SysFile: unknown message ctor {}",
                    cid.0
                )));
            }
            let path_val = builtin_io::field_ref(fields, 0)
                .ok_or_else(|| RuntimeError::Panic("ReadFileRaw missing path".into()))?;
            let path = builtin_io::string_value_to_str(&inner.program, path_val)
                .map_err(RuntimeError::from)?;
            let requester = builtin_io::field_actor(fields, 1)
                .ok_or_else(|| RuntimeError::Panic("ReadFileRaw requester not Actor".into()))?;
            let reply = match std::fs::read(&path) {
                Ok(bytes) => reply::make_file_data(&inner.program, &bytes)
                    .map_err(RuntimeError::from)?,
                Err(e) => reply::make_file_err(&inner.program, &e.to_string())
                    .map_err(RuntimeError::from)?,
            };
            enqueue_msg(inner, id, requester, reply)?;
        }
        MailItem::Io(_) => {} // @SysFile is synchronous; no Io events
        MailItem::Abandon => {} // system actors are immortal; should not receive this
    }
    Ok(())
}

fn process_net(
    inner: &Arc<Inner>,
    id: ActorId,
    item: MailItem,
) -> Result<(), RuntimeError> {
    let ctors = inner
        .builtin_ctors
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| RuntimeError::Panic("@net: missing builtin ctors".into()))?;

    match item {
        MailItem::Msg(msg) => {
            let Some((cid, fields)) = match_data_ctor(&msg) else {
                return Err(RuntimeError::Panic(format!(
                    "@SysNet: expected Data message, got {msg:?}"
                )));
            };
            if cid == ctors.conn_request {
                let port = field_int(fields, 0)
                    .ok_or_else(|| RuntimeError::Panic("ConnRequest port not Int".into()))?;
                let requester = field_actor(fields, 1)
                    .ok_or_else(|| RuntimeError::Panic("ConnRequest replyTo not Actor".into()))?;

                // Start listener once (port 0 → configured bind, typically ephemeral).
                {
                    let actors = inner.actors.read().unwrap();
                    let mx = actors.get(&id).unwrap();
                    let mut cell = mx.lock().unwrap();
                    if let Some(NativeState::Net {
                        pending,
                        listener_started,
                        ..
                    }) = cell.native_state.as_mut()
                    {
                        *pending = Some(requester);
                        if !*listener_started {
                            *listener_started = true;
                            let inner2 = Arc::clone(inner);
                            let net_id = id;
                            let stop = Arc::clone(&inner.io_stop);
                            let port_out = Arc::clone(&inner.net_port);
                            let bind = if port == 0 {
                                inner.io.net_bind.clone()
                            } else {
                                format!("127.0.0.1:{port}")
                            };
                            builtin_io::start_net_listener(bind, stop, port_out, move |ev| {
                                let _ = enqueue_io(&inner2, net_id, ev);
                            });
                        }
                    }
                }

                // If backlog has a stream, promote immediately.
                let stream_opt = {
                    let mut bl = inner.net_backlog.lock().unwrap();
                    bl.pop_front()
                };
                if let Some(stream) = stream_opt {
                    deliver_connected(inner, id, stream)?;
                }
            } else if cid == ctors.data_request {
                let token = field_int(fields, 0)
                    .ok_or_else(|| RuntimeError::Panic("DataRequest token not Int".into()))?;
                let requester = field_actor(fields, 1)
                    .ok_or_else(|| RuntimeError::Panic("DataRequest replyTo not Actor".into()))?;
                forward_to_sysconn(
                    inner, id, ActorId(token as u32), ctors.conn_req_data,
                    vec![Value::Actor(Arc::new(ActorLiveness { id: requester, on_zero: None }))],
                )?;
            } else if cid == ctors.net_send {
                let token = field_int(fields, 0)
                    .ok_or_else(|| RuntimeError::Panic("NetSend token not Int".into()))?;
                let bytes = field_ref(fields, 1)
                    .ok_or_else(|| RuntimeError::Panic("NetSend missing bytes".into()))?;
                forward_to_sysconn(
                    inner, id, ActorId(token as u32), ctors.conn_send,
                    vec![bytes.clone()],
                )?;
            } else if cid == ctors.net_close {
                let token = field_int(fields, 0)
                    .ok_or_else(|| RuntimeError::Panic("NetClose token not Int".into()))?;
                forward_to_sysconn(
                    inner, id, ActorId(token as u32), ctors.conn_close,
                    vec![],
                )?;
            } else {
                return Err(RuntimeError::Panic("@SysNet: unknown message".into()));
            }
        }
        MailItem::Io(IoEvent::NetAccepted(stream)) => {
            // If someone is waiting, connect; else backlog.
            let has_pending = {
                let actors = inner.actors.read().unwrap();
                let mx = actors.get(&id).unwrap();
                let cell = mx.lock().unwrap();
                matches!(
                    cell.native_state.as_ref(),
                    Some(NativeState::Net {
                        pending: Some(_),
                        ..
                    })
                )
            };
            if has_pending {
                deliver_connected(inner, id, stream)?;
            } else {
                inner.net_backlog.lock().unwrap().push_back(stream);
            }
        }
        _ => {}
    }
    Ok(())
}

fn deliver_connected(
    inner: &Arc<Inner>,
    net_id: ActorId,
    stream: TcpStream,
) -> Result<(), RuntimeError> {
    let pending = {
        let actors = inner.actors.read().unwrap();
        let mx = actors.get(&net_id).unwrap();
        let mut cell = mx.lock().unwrap();
        if let Some(NativeState::Net { pending, .. }) = cell.native_state.as_mut() {
            pending.take()
        } else {
            None
        }
    };
    let Some(requester) = pending else {
        inner.net_backlog.lock().unwrap().push_back(stream);
        return Ok(());
    };
    // Token = the `@SysConn` actor id (spec §5.5.7): `@SysNet` routes by
    // token, and `@SysConn` reports itself as the token in `ConnGone`.
    let conn_id = spawn_native(inner, "SysConn", NativeKind::Conn, NativeState::conn())?;
    {
        let mut streams = inner.streams.lock().unwrap();
        streams.insert(conn_id, stream);
    }
    // Publish local port once known.
    if let Some(p) = *inner.net_port.lock().unwrap() {
        let actors = inner.actors.read().unwrap();
        if let Some(mx) = actors.get(&net_id) {
            let mut cell = mx.lock().unwrap();
            if let Some(NativeState::Net { local_port, .. }) = cell.native_state.as_mut() {
                *local_port = Some(p);
            }
        }
    }
    let msg = reply::make_conn_token(&inner.program, conn_id.0 as i64)
        .map_err(RuntimeError::from)?;
    enqueue_msg(inner, net_id, requester, msg)?;
    Ok(())
}

/// Route a raw-protocol message to the `@SysConn` actor named by `token`.
fn forward_to_sysconn(
    inner: &Arc<Inner>,
    sender: ActorId,
    target: ActorId,
    ctor: ConstructorId,
    fields: Vec<Value>,
) -> Result<(), RuntimeError> {
    enqueue_msg(inner, sender, target, Value::data_value(ctor, fields))
}

fn process_conn(
    inner: &Arc<Inner>,
    id: ActorId,
    item: MailItem,
) -> Result<(), RuntimeError> {
    let ctors = inner
        .builtin_ctors
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| RuntimeError::Panic("@SysConn: missing builtin ctors".into()))?;

    match item {
        MailItem::Msg(msg) => {
            let Some((cid, fields)) = match_data_ctor(&msg) else {
                return Err(RuntimeError::Panic(format!(
                    "@SysConn: expected Data message, got {msg:?}"
                )));
            };
            if cid == ctors.conn_req_data {
                let requester = field0_actor(fields)
                    .ok_or_else(|| RuntimeError::Panic("ConnRequestData field not Actor".into()))?;
                let mut start_reader = false;
                let mut already_closed = false;
                {
                    let actors = inner.actors.read().unwrap();
                    let mx = actors.get(&id).unwrap();
                    let mut cell = mx.lock().unwrap();
                    if let Some(NativeState::Conn {
                        pending,
                        closed,
                        reader_started,
                    }) = cell.native_state.as_mut()
                    {
                        *pending = Some(requester);
                        already_closed = *closed;
                        if !*reader_started && !*closed {
                            *reader_started = true;
                            start_reader = true;
                        }
                    }
                }
                if already_closed {
                    let msg =
                        reply::make_conn_gone(&inner.program, id.0 as i64)
                            .map_err(RuntimeError::from)?;
                    enqueue_msg(inner, id, requester, msg)?;
                } else if start_reader {
                    let stream = {
                        let streams = inner.streams.lock().unwrap();
                        streams
                            .get(&id)
                            .and_then(|s| s.try_clone().ok())
                    };
                    if let Some(stream) = stream {
                        let inner2 = Arc::clone(inner);
                        let conn_id = id;
                        let stop = Arc::clone(&inner.io_stop);
                        builtin_io::start_conn_reader(stream, stop, move |ev| {
                            let _ = enqueue_io(&inner2, conn_id, ev);
                        });
                    }
                }
            } else if cid == ctors.conn_send {
                let list = field0_list(fields)
                    .ok_or_else(|| RuntimeError::Panic("ConnSend missing list".into()))?;
                let s = reply::list_to_string(&inner.program, list)
                    .map_err(RuntimeError::from)?;
                let mut streams = inner.streams.lock().unwrap();
                if let Some(stream) = streams.get_mut(&id) {
                    stream
                        .write_all(s.as_bytes())
                        .map_err(|e| RuntimeError::Panic(format!("conn write: {e}")))?;
                    let _ = stream.flush();
                }
            } else if cid == ctors.conn_close {
                {
                    let actors = inner.actors.read().unwrap();
                    let mx = actors.get(&id).unwrap();
                    let mut cell = mx.lock().unwrap();
                    if let Some(NativeState::Conn { closed, .. }) = cell.native_state.as_mut() {
                        *closed = true;
                    }
                }
                let mut streams = inner.streams.lock().unwrap();
                if let Some(stream) = streams.remove(&id) {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            } else {
                return Err(RuntimeError::Panic("@SysConn: unknown message".into()));
            }
        }
        MailItem::Io(ev) => {
            let pending = {
                let actors = inner.actors.read().unwrap();
                let mx = actors.get(&id).unwrap();
                let mut cell = mx.lock().unwrap();
                if let Some(NativeState::Conn {
                    pending, closed, ..
                }) = cell.native_state.as_mut()
                {
                    if matches!(ev, IoEvent::ConnEof) {
                        *closed = true;
                    }
                    pending.take()
                } else {
                    None
                }
            };
            if let Some(requester) = pending {
                let msg = match ev {
                    IoEvent::ConnBytes(bytes) => {
                        let cps: Vec<i64> = bytes.iter().map(|b| *b as i64).collect();
                        reply::make_net_data(&inner.program, &cps)
                    }
                    IoEvent::ConnEof => reply::make_conn_gone(&inner.program, id.0 as i64),
                    _ => return Ok(()),
                }
                .map_err(RuntimeError::from)?;
                enqueue_msg(inner, id, requester, msg)?;
            }
        }
        _ => {}
    }
    Ok(())
}
