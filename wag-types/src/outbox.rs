//! Static outbox mapping (D043, spec §5.5.7): actor property → reply
//! constructors the property's outbox delivers.
//!
//! These constructors are **declared in the prelude** (on the actor's reply
//! type, e.g. `StdReply`), not injected into the user program (D035 retired).
//! The table drives two checker rules:
//!
//! 1. A pattern arm for an outbox constructor (qualified with the property's
//!    actor, e.g. `@std.Line`, or bare when the ctor is unique) is legal even
//!    though the ctor does not belong to the scrutinee Msg type.
//! 2. Match exhaustiveness on an actor's Msg type counts the outbox
//!    constructors of that actor's properties as required arms.
//!
//! v1 limitation: the reply *target* is not checked against the outbox type
//! (request ctors carry a flexible `Actor a` field, spec §5.5.7); foreign
//! `replyTo` is accepted and may crash at runtime (same class as D035).

/// Reply constructors delivered by the named actor's outbox.
///
/// Names only (no field types — those live in the prelude declaration or the
/// checker's built-in table).
/// `Std`/`Net` are the WAG prelude actors; `SysStd`/`SysNet` are the native
/// actors they proxy (raw protocol).
pub fn outbox_ctors(actor: &str) -> &'static [&'static str] {
    match actor {
        "std" => &["Line", "EndOfInput"],
        "file" => &["FileContent", "FileError"],
        "net" => &["Connected", "Data", "Closed"],
        "SysStd" => &["Chunk", "StdEnd"],
        "SysFile" => &["FileData", "FileErr"],
        "SysNet" => &["ConnToken", "NetData", "ConnGone"],
        _ => &[],
    }
}

/// True if `ctor` is in the outbox of actor `actor_name`.
pub fn in_outbox(actor_name: &str, ctor: &str) -> bool {
    outbox_ctors(actor_name).contains(&ctor)
}
