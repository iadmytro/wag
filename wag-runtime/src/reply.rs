//! Structural reply-type matching for the native `@SysStd` / `@SysFile` /
//! `@SysNet` / `@SysConn` actors.
//!
//! Raw-protocol reply constructors (`Chunk`/`StdEnd` for `@SysStd`;
//! `FileData`/`FileErr` for `@SysFile` (D082);
//! `ConnToken`/`NetData`/`ConnGone` for `@SysNet`/`@SysConn`) are resolved
//! by name against the program-wide constructor table — the same escape hatch
//! used for `Start` / `Down` (§5.5.6 / §5.5.8). Names are unique within a
//! program (§3.1.1), so lookup is per-`LoadedProgram`, not a global cache
//! across programs.
//!
//! User-facing reply ctors (`Line`/`EndOfInput`, `Connected`/`Data`/`Closed`)
//! are constructed by the WAG prelude `@std`/`@net` actors, not here (D043
//! two-layer I/O).

use wag_interp::{
    build_prelude_list, read_prelude_list, ConstructorId, LoadedProgram, PreludeListCtors,
    Value, VmError,
};

/// Look up a reply constructor by name in this program's table.
pub fn reply_ctor(program: &LoadedProgram, name: &str) -> Result<ConstructorId, VmError> {
    program.ctor_by_name.get(name).copied().ok_or_else(|| {
        VmError::Panic(format!(
            "structural reply: program has no `{name}` constructor (caller Msg type must declare it)"
        ))
    })
}

/// Build a `List Int` of Unicode scalar values (prelude-declared, D054/D056).
pub fn list_from_codepoints(
    program: &LoadedProgram,
    cps: &[i64],
) -> Result<Value, VmError> {
    let elems: Vec<Value> = cps.iter().map(|&cp| Value::Int(cp)).collect();
    let ctors = PreludeListCtors::from_program(program)
        .ok_or_else(|| VmError::Panic("prelude List ctors not found".into()))?;
    build_prelude_list(elems, ctors)
}

/// Read a `List Int` of codepoints into a Rust `String` (replacement char on bad scalars).
pub fn list_to_string(program: &LoadedProgram, v: &Value) -> Result<String, VmError> {
    let ctors = PreludeListCtors::from_program(program)
        .ok_or_else(|| VmError::Panic("prelude List ctors not found".into()))?;
    let elems = read_prelude_list(v, ctors)?;
    let mut bytes = Vec::new();
    for e in elems {
        match e {
            Value::Int(n) => bytes.push(n as u8),
            _ => return Err(VmError::Panic("list_to_string: non-Int element".into())),
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `Chunk text` — raw `@SysStd` reply (prelude-declared on `SysStdReply`).
pub fn make_chunk(program: &LoadedProgram, line: &str) -> Result<Value, VmError> {
    let ctor = reply_ctor(program, "Chunk")?;
    build_data(program, ctor, vec![Value::Str(line.to_string())])
}

/// Nullary `StdEnd` — raw `@SysStd` reply (prelude-declared on `SysStdReply`).
pub fn make_end(program: &LoadedProgram) -> Result<Value, VmError> {
    let ctor = reply_ctor(program, "StdEnd")?;
    build_data(program, ctor, vec![])
}

/// `FileData text` — raw `@SysFile` file-read reply (prelude-declared on `SysFileReply`, D082).
pub fn make_file_data(program: &LoadedProgram, bytes: &[u8]) -> Result<Value, VmError> {
    let ctor = reply_ctor(program, "FileData")?;
    let s = String::from_utf8_lossy(bytes).into_owned();
    build_data(program, ctor, vec![Value::Str(s)])
}

/// `FileErr msg` — raw `@SysFile` file-read error reply (prelude-declared on `SysFileReply`, D082).
pub fn make_file_err(program: &LoadedProgram, msg: &str) -> Result<Value, VmError> {
    let ctor = reply_ctor(program, "FileErr")?;
    build_data(program, ctor, vec![Value::Str(msg.to_string())])
}

/// `NetData bytes` — raw `@SysConn` reply (built-in on `SysNetReply`).
pub fn make_net_data(
    program: &LoadedProgram,
    bytes_as_codepoints: &[i64],
) -> Result<Value, VmError> {
    let ctor = reply_ctor(program, "NetData")?;
    let bytes = list_from_codepoints(program, bytes_as_codepoints)?;
    build_data(program, ctor, vec![bytes])
}

/// `ConnGone token` — raw `@SysConn` reply (built-in on `SysNetReply`).
pub fn make_conn_gone(program: &LoadedProgram, token: i64) -> Result<Value, VmError> {
    let ctor = reply_ctor(program, "ConnGone")?;
    build_data(program, ctor, vec![Value::Int(token)])
}

/// `ConnToken token` — raw `@SysNet` reply (built-in on `SysNetReply`).
pub fn make_conn_token(program: &LoadedProgram, token: i64) -> Result<Value, VmError> {
    let ctor = reply_ctor(program, "ConnToken")?;
    build_data(program, ctor, vec![Value::Int(token)])
}

fn build_data(
    program: &LoadedProgram,
    ctor: ConstructorId,
    fields: Vec<Value>,
) -> Result<Value, VmError> {
    let entry = program
        .ctors
        .get(ctor.0 as usize)
        .ok_or_else(|| VmError::Panic("bad constructor id".into()))?;
    if entry.arity != fields.len() {
        return Err(VmError::Panic(format!(
            "constructor `{}` arity {} != {}",
            entry.name,
            entry.arity,
            fields.len()
        )));
    }
    Ok(Value::Data(ctor, fields))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Chunk`/`End` are prelude-declared (D043) — the program under test must
    /// include the prelude.
    fn prelude() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../prelude/prelude.wag"
        ))
        .expect("prelude.wag readable")
    }

    #[test]
    fn chunk_end_resolved_per_program() {
        let src = format!(
            "{}\n\n@main = @std: Option (Option Int), StdReply -> Option (Option Int): st, msg -> match msg\n  Line _ -> return st\n  EndOfInput -> return None\n",
            prelude()
        );
        let (_p, _t, loaded) = wag_interp::prepare(&src).unwrap();
        let chunk = make_chunk(&loaded, "hi").unwrap();
        assert!(matches!(&chunk, Value::Data(c, f) if *c == *loaded.ctor_by_name.get("Chunk").unwrap() && f.len() == 1));

        let end = make_end(&loaded).unwrap();
        assert!(matches!(&end, Value::Data(c, f) if *c == *loaded.ctor_by_name.get("StdEnd").unwrap() && f.is_empty()));
    }
}
