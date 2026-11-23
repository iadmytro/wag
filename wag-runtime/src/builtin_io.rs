//! Native `@SysStd` / `@SysFile` / `@SysNet` / `@SysConn` actors (D043 two-layer I/O,
//! §5.5.7, §4.5).
//!
//! Behaviors are Rust-native (not WAG-interpreted). I/O never blocks a scheduler
//! worker: `@SysStd` uses a dedicated stdin thread (or a scripted line buffer
//! for tests); `@SysNet` uses a listener thread plus one `@SysConn` actor
//! (reader thread) per accepted connection, addressed by an opaque Int token.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use wag_interp::{
    ActorId, ConstructorId, LoadedProgram, Value, VmError,
};

/// Injectable / capturable process I/O for tests.
#[derive(Clone)]
pub struct IoConfig {
    /// When `Some`, `@std` serves these lines then `EndOfInput` (no OS stdin thread).
    pub stdin_lines: Option<Vec<String>>,
    /// Captured stdout bytes from `Write` (and optionally `@net` is separate).
    pub stdout: Arc<Mutex<Vec<u8>>>,
    /// Bind address for `@net` listener (default `127.0.0.1:0`).
    pub net_bind: String,
}

impl Default for IoConfig {
    fn default() -> Self {
        IoConfig {
            stdin_lines: None,
            stdout: Arc::new(Mutex::new(Vec::new())),
            net_bind: "127.0.0.1:0".into(),
        }
    }
}

impl IoConfig {
    pub fn scripted(lines: Vec<String>) -> Self {
        IoConfig {
            stdin_lines: Some(lines),
            ..Default::default()
        }
    }

    pub fn take_stdout_string(&self) -> String {
        let g = self.stdout.lock().unwrap();
        String::from_utf8_lossy(&g).into_owned()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeKind {
    Std,
    File,
    Net,
    Conn,
}

/// Per-cell native state (not encoded in WAG `Value` state).
pub enum NativeState {
    Std {
        pending: Option<ActorId>,
        eof: bool,
        /// Remaining scripted lines (test mode).
        scripted: Option<VecDeque<String>>,
        stdin_thread_started: bool,
    },
    /// `@SysFile` is synchronous (D082): no pending slot, no threads.
    File,
    Net {
        pending: Option<ActorId>,
        listener_started: bool,
        local_port: Option<u16>,
    },
    Conn {
        pending: Option<ActorId>,
        closed: bool,
        reader_started: bool,
    },
}

impl NativeState {
    pub fn std(scripted: Option<Vec<String>>) -> Self {
        NativeState::Std {
            pending: None,
            eof: false,
            scripted: scripted.map(VecDeque::from),
            stdin_thread_started: false,
        }
    }

    pub fn file() -> Self {
        NativeState::File
    }

    pub fn net() -> Self {
        NativeState::Net {
            pending: None,
            listener_started: false,
            local_port: None,
        }
    }

    pub fn conn() -> Self {
        NativeState::Conn {
            pending: None,
            closed: false,
            reader_started: false,
        }
    }
}

/// Internal mailbox payloads for built-in actors (never appear in WAG source).
pub enum IoEvent {
    StdLine(String),
    StdEof,
    /// Fresh accepted stream — moved into runtime stream table under a temp key then Conn.
    NetAccepted(TcpStream),
    ConnBytes(Vec<u8>),
    ConnEof,
}

#[derive(Clone)]
pub struct BuiltinCtors {
    /// Raw `@SysStd` protocol ctors (D043). Prelude-declared, so `None` when
    /// the prelude is not merged — in which case nothing can reach `@SysStd`
    /// and these are never used.
    pub read_line: Option<ConstructorId>,
    pub write_line: Option<ConstructorId>,
    /// Raw `@SysFile` protocol ctor (D082). Prelude-declared, so `None` when
    /// the prelude is not merged — in which case nothing can reach `@SysFile`.
    pub read_file: Option<ConstructorId>,
    /// `@SysNet` raw protocol ctors (D043) — checker built-ins, always present.
    pub conn_request: ConstructorId,
    pub data_request: ConstructorId,
    pub net_send: ConstructorId,
    pub net_close: ConstructorId,
    /// `@SysConn` inbox ctors — checker built-ins, always present.
    pub conn_req_data: ConstructorId,
    pub conn_send: ConstructorId,
    pub conn_close: ConstructorId,
}

impl BuiltinCtors {
    pub fn from_program(p: &LoadedProgram) -> Result<Self, VmError> {
        let get = |n: &str| {
            p.ctor_by_name
                .get(n)
                .copied()
                .ok_or_else(|| VmError::Panic(format!("missing built-in ctor `{n}`")))
        };
        Ok(BuiltinCtors {
            read_line: p.ctor_by_name.get("ReadLine").copied(),
            write_line: p.ctor_by_name.get("WriteLine").copied(),
            read_file: p.ctor_by_name.get("ReadFileRaw").copied(),
            conn_request: get("ConnRequest")?,
            data_request: get("DataRequest")?,
            net_send: get("NetSend")?,
            net_close: get("NetClose")?,
            conn_req_data: get("ConnRequestData")?,
            conn_send: get("ConnSend")?,
            conn_close: get("ConnClose")?,
        })
    }
}

/// Resolve a constructor-shaped message (Data value).
pub fn match_data_ctor<'a>(msg: &'a Value) -> Option<(ConstructorId, &'a [Value])> {
    match msg {
        Value::Data(ctor, fields) => Some((*ctor, fields.as_slice())),
        Value::Data1(ctor, field) => Some((*ctor, std::slice::from_ref(field.as_ref()))),
        _ => None,
    }
}

/// Write a `String` payload to the captured stdout buffer.
pub fn write_stdout(io: &IoConfig, program: &LoadedProgram, val: &Value) -> Result<(), VmError> {
    let s = program.value_to_string(val)
        .ok_or_else(|| VmError::Panic("Write payload is not a String".into()))?;
    let mut out = io.stdout.lock().unwrap();
    out.write_all(s.as_bytes())
        .map_err(|e| VmError::Panic(format!("stdout write: {e}")))?;
    Ok(())
}

pub fn start_stdin_thread(
    stop: Arc<AtomicBool>,
    deliver: impl Fn(IoEvent) + Send + 'static,
) {
    thread::Builder::new()
        .name("wag-stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut locked = stdin.lock();
            let mut buf = String::new();
            loop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                buf.clear();
                match locked.read_line(&mut buf) {
                    Ok(0) => {
                        deliver(IoEvent::StdEof);
                        break;
                    }
                    Ok(_) => {
                        let line = buf.trim_end_matches(&['\r', '\n'][..]).to_string();
                        deliver(IoEvent::StdLine(line));
                    }
                    Err(_) => {
                        deliver(IoEvent::StdEof);
                        break;
                    }
                }
            }
        })
        .expect("spawn wag-stdin");
}

pub fn start_net_listener(
    bind: String,
    stop: Arc<AtomicBool>,
    port_out: Arc<Mutex<Option<u16>>>,
    deliver: impl Fn(IoEvent) + Send + 'static,
) {
    thread::Builder::new()
        .name("wag-net-listen".into())
        .spawn(move || {
            let listener = match TcpListener::bind(&bind) {
                Ok(l) => l,
                Err(e) => {
                    eprintln!("wag @net bind {bind}: {e}");
                    return;
                }
            };
            if let Ok(addr) = listener.local_addr() {
                *port_out.lock().unwrap() = Some(addr.port());
            }
            let _ = listener.set_nonblocking(true);
            while !stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        deliver(IoEvent::NetAccepted(stream));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        })
        .expect("spawn wag-net-listen");
}

pub fn start_conn_reader(
    stream: TcpStream,
    stop: Arc<AtomicBool>,
    deliver: impl Fn(IoEvent) + Send + 'static,
) {
    thread::Builder::new()
        .name("wag-net-conn".into())
        .spawn(move || {
            let mut reader = BufReader::new(stream.try_clone().unwrap_or_else(|_| {
                stream.try_clone().expect("tcp clone")
            }));
            let mut buf = [0u8; 4096];
            loop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                match reader.read(&mut buf) {
                    Ok(0) => {
                        deliver(IoEvent::ConnEof);
                        break;
                    }
                    Ok(n) => {
                        deliver(IoEvent::ConnBytes(buf[..n].to_vec()));
                    }
                    Err(_) => {
                        deliver(IoEvent::ConnEof);
                        break;
                    }
                }
            }
        })
        .expect("spawn wag-net-conn");
}

/// Decode field payloads from a Data message (raw protocol ctors are
/// positional: Int port/token first, then Actor reply target or List bytes).
pub fn field0_actor(fields: &[Value]) -> Option<ActorId> {
    match fields.first() {
        Some(Value::Actor(liveness)) => Some(liveness.id),
        _ => None,
    }
}

pub fn field0_list(fields: &[Value]) -> Option<&Value> {
    fields.first()
}

pub fn field_int(fields: &[Value], i: usize) -> Option<i64> {
    match fields.get(i) {
        Some(Value::Int(n)) => Some(*n),
        _ => None,
    }
}

pub fn field_actor(fields: &[Value], i: usize) -> Option<ActorId> {
    match fields.get(i) {
        Some(Value::Actor(liveness)) => Some(liveness.id),
        _ => None,
    }
}

pub fn field_ref(fields: &[Value], i: usize) -> Option<&Value> {
    fields.get(i)
}

/// Extract a `String` value into a Rust `String`.
pub fn string_value_to_str(program: &LoadedProgram, v: &Value) -> Result<String, VmError> {
    program.value_to_string(v)
        .ok_or_else(|| VmError::Panic("expected String value".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wag_interp::{CtorEntry, CtorKind, ConstructorId, LoadedProgram};
    use std::collections::HashMap;

    fn test_program() -> LoadedProgram {
        let ctors = vec![
            CtorEntry { name: "String".into(), id: ConstructorId(0), type_name: "String".into(), arity: 1, pointer_free: false, kind: CtorKind::User, field_is_ref: vec![], resident: false, field_names: vec![], constrained_body: None },
            CtorEntry { name: "VBuf".into(), id: ConstructorId(1), type_name: "Vec".into(), arity: 3, pointer_free: false, kind: CtorKind::User, field_is_ref: vec![], resident: false, field_names: vec![], constrained_body: None },
            CtorEntry { name: "Some".into(), id: ConstructorId(2), type_name: "Option".into(), arity: 1, pointer_free: false, kind: CtorKind::User, field_is_ref: vec![], resident: false, field_names: vec![], constrained_body: None },
            CtorEntry { name: "None".into(), id: ConstructorId(3), type_name: "Option".into(), arity: 0, pointer_free: true, kind: CtorKind::User, field_is_ref: vec![], resident: false, field_names: vec![], constrained_body: None },
        ];
        let ctor_by_name: HashMap<String, ConstructorId> = ctors.iter().map(|c| (c.name.clone(), c.id)).collect();
        LoadedProgram {
            functions: vec![],
            fn_by_name: HashMap::new(),
            fn_overloads: HashMap::new(),
            ctors,
            ctor_by_name,
            method_fns: HashMap::new(),
            actors: HashMap::new(),
            cell_reuse: false,
        }
    }

    /// `Write` must land in the capture buffer exactly once with exact content.
    #[test]
    fn write_stdout_captures_payload_once() {
        let io = IoConfig::default();
        let prog = test_program();
        write_stdout(&io, &prog, &prog.make_string("hi")).unwrap();
        assert_eq!(io.take_stdout_string(), "hi");

        // a second Write appends, still no duplication
        write_stdout(&io, &prog, &prog.make_string("hi")).unwrap();
        assert_eq!(io.take_stdout_string(), "hihi");
    }

    /// Non-`String` payloads are rejected, not silently dropped.
    #[test]
    fn write_stdout_rejects_non_string() {
        let io = IoConfig::default();
        let prog = test_program();
        assert!(write_stdout(&io, &prog, &Value::Int(1)).is_err());
    }
}
