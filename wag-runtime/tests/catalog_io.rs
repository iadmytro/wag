//! Milestone 9 — `@std` / `@net` built-in IO actors.
//!
//! **Stdin test harness:** `IoConfig::scripted(lines)` feeds `@std` without an OS
//! stdin thread so tests stay deterministic under `cargo test`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use wag_runtime::{prepare_runtime, IoConfig, Runtime, SchedulerConfig};
use wag_interp::{ConstructorId, Value};

fn fixture(name: &str) -> String {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.push("fixtures");
    p.push(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// The user-facing `@std` is a WAG prelude actor (D043): its reply ctors
/// (`Line`/`EndOfInput`) and the raw `@SysStd` protocol are prelude-declared.


fn cfg_single() -> SchedulerConfig {
    SchedulerConfig { workers: 0 }
}

fn ctor(program: &wag_interp::LoadedProgram, name: &str) -> ConstructorId {
    *program
        .ctor_by_name
        .get(name)
        .unwrap_or_else(|| panic!("missing ctor {name}"))
}

/// End-to-end §8.15 with multi-line scripted stdin.
#[test]
fn fixture_8_15_echo_lines_scripted_stdin() {
    let src = fixture("io/main_std.wag");
    let (_p, _t, loaded) = prepare_runtime(&src).expect("prepare");
    let lines = vec!["hello".into(), "world".into(), "wag".into()];
    let io = IoConfig::scripted(lines.clone());
    let rt = Runtime::new_with_io(loaded.clone(), cfg_single(), io.clone());
    let empty_argv = {
        let ctors = wag_interp::PreludeListCtors::from_program(&loaded).unwrap();
        wag_interp::build_prelude_list(Vec::new(), ctors).unwrap()
    };
    let _main_id = rt.spawn_main(empty_argv).unwrap();
    for _ in 0..200 {
        rt.drive(64).ok();
        let out = io.take_stdout_string();
        if out == lines.concat() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let out = io.take_stdout_string();
    let expected: String = lines.concat();
    assert_eq!(
        out, expected,
        "echoed stdout should match input lines concatenated"
    );
    rt.shutdown();
}

/// Two `RequestLine`s in succession: newest pending requester wins.
#[test]
fn request_line_newest_wins() {
    let src = r#"
MainMsg = Start (List (List Int))

@main = @std: Int, MainMsg -> Int: st, msg ->
  s@ = @std
  match msg
    Start _ ->
      s@ (RequestLine self)
      s@ (RequestLine self)
      0
    Line chars ->
      s@ (Write chars)
      s@ (RequestLine self)
      st
    EndOfInput -> st
"#;
    let (_p, _t, loaded) = prepare_runtime(&src).expect("prepare");
    let io = IoConfig::scripted(vec!["only".into()]);
    let rt = Runtime::new_with_io(loaded.clone(), cfg_single(), io.clone());
    let main_id = rt.spawn_actor("main", Value::Int(0)).unwrap();
    rt.inject(
        main_id,
        {
            let id = ctor(&loaded, "Start");
            let empty_argv = {
                let ctors = wag_interp::PreludeListCtors::from_program(&loaded).unwrap();
                wag_interp::build_prelude_list(Vec::new(), ctors).unwrap()
            };
            Value::Data(id, vec![empty_argv])
        },
    )
    .unwrap();
    rt.run_until_quiescent(Duration::from_millis(500)).ok();
    assert_eq!(io.take_stdout_string(), "only");
    rt.shutdown();
}

/// Structural reply matching is per-`LoadedProgram` (by constructor name).
#[test]
fn structural_reply_line_per_program() {
    let src_a = r#"
MainMsg = Start (List (List Int))
@main = @std: Int, MainMsg -> Int: st, msg ->
  s@ = @std
  match msg
    Start _ ->
      s@ (RequestLine self)
      0
    Line chars ->
      s@ (Write chars)
      s@ (RequestLine self)
      st
    EndOfInput -> st
"#;
    let src_b = r#"
MsgB = Start (List (List Int))
@main = @std: Int, MsgB -> Int: st, msg ->
  s@ = @std
  match msg
    Start _ ->
      s@ (RequestLine self)
      0
    Line chars ->
      s@ (Write chars)
      s@ (RequestLine self)
      st
    EndOfInput -> st
"#;
    let (_p, _t, la) = prepare_runtime(&src_a).unwrap();
    let (_p, _t, lb) = prepare_runtime(&src_b).unwrap();
    let id_a = ctor(&la, "Line");
    let id_b = ctor(&lb, "Line");
    assert_eq!(la.ctors[id_a.0 as usize].name, "Line");
    assert_eq!(lb.ctors[id_b.0 as usize].name, "Line");
    let io = IoConfig::scripted(vec!["x".into()]);
    let rt = Runtime::new_with_io(lb.clone(), cfg_single(), io.clone());
    let main_id = rt.spawn_actor("main", Value::Int(0)).unwrap();
    rt.inject(
        main_id,
        {
            let id = ctor(&lb, "Start");
            let empty_argv = {
                let ctors = wag_interp::PreludeListCtors::from_program(&lb).unwrap();
                wag_interp::build_prelude_list(Vec::new(), ctors).unwrap()
            };
            Value::Data(id, vec![empty_argv])
        },
    )
    .unwrap();
    rt.run_until_quiescent(Duration::from_millis(500)).ok();
    assert_eq!(io.take_stdout_string(), "x");
    rt.shutdown();
    let _ = la;
}

/// `@net` smoke: accept one connection, `Send`/`RequestData` round trip.
#[test]
fn net_smoke_send_recv() {
    let src = r#"
MainMsg = Start (List (List Int))

@server = Int, NetReply -> Int: st, msg ->
  n@ = @net
  s@ = @std
  match msg
    Connected token ->
      n@ (Send (copy token) [65, 66])
      n@ (RequestData (copy token) self)
      0
    Data bytes ->
      s@ (Write (codepointsToString (vecFromList bytes)))
      st
    Closed _ -> st

@main = Int, MainMsg -> Int: st, msg ->
  n@ = @net
  match msg
    Start _ ->
      c@ = @server 0
      n@ (RequestConnection 0 c)
      0
"#;
    let (_p, _t, loaded) = prepare_runtime(&src).expect("prepare");
    let io = IoConfig::scripted(vec![]);
    let rt = Runtime::new_with_io(loaded.clone(), cfg_single(), io.clone());
    let main_id = rt.spawn_actor("main", Value::Int(0)).unwrap();
    rt.inject(
        main_id,
        {
            let id = ctor(&loaded, "Start");
            // Empty argv list.
            let empty_argv = {
                let ctors = wag_interp::PreludeListCtors::from_program(&loaded).unwrap();
                wag_interp::build_prelude_list(Vec::new(), ctors).unwrap()
            };
            Value::Data(id, vec![empty_argv])
        },
    )
    .unwrap();
    rt.run_until_quiescent(Duration::from_millis(100)).ok();

    let mut port = None;
    for _ in 0..200 {
        rt.drive(64).ok();
        port = rt.net_port();
        if port.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let port = port.expect("@net should bind after RequestConnection");
    let mut client = TcpStream::connect(("127.0.0.1", port)).expect("client connect");
    client
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    client
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();

    // Expect "AB" from server Send.
    let mut got = Vec::new();
    for _ in 0..100 {
        rt.drive(64).ok();
        let mut buf = [0u8; 16];
        match client.read(&mut buf) {
            Ok(n) if n > 0 => {
                got.extend_from_slice(&buf[..n]);
                if got.len() >= 2 {
                    break;
                }
            }
            _ => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    assert!(got.len() >= 2, "expected AB from server, got {got:?}");
    assert_eq!(&got[..2], b"AB");

    // Client → server Data path → @std Write.
    client.write_all(b"hi").unwrap();
    let _ = client.flush();
    let mut out = String::new();
    for _ in 0..100 {
        rt.drive(64).ok();
        out = io.take_stdout_string();
        if out.contains('h') {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        out.as_bytes().contains(&b'h'),
        "stdout should capture Data payload, got {out:?}"
    );

    let _ = client.shutdown(std::net::Shutdown::Both);
    rt.shutdown();
}
