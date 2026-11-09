# Wag

**Wag** is an experimental, zero-managed-memory language, hosted as a Rust workspace interpreter. It is built around strict tail recursion: every call in tail position runs on a trampoline, and any non-tail cycle in the call graph is a compile error — so recursion runs in bounded memory.

> **Status: experimental.** Syntax, semantics, and the standard library are all subject to change.

## Memory management

No garbage collector, no tracing. One allocator: a single **shared global heap** on which every data value lives as an owned cell.

- **Everything is a function** — no variable/assignment distinction; even `x = expr` is conceptually just `() -> T` (a function taking no arguments that returns the value), so every binding is fixed once made, and state is threaded through arguments.
- **Linear ownership** — every owned value has exactly one binding; using it consumes it, a second use needs an explicit `copy`.
- **Scope-based reclamation** — values are dropped when their binding leaves scope or is consumed; no allocator state is threaded through calls.
- **Borrows** — `&T` parameters are temporary read-only views that end with the call.
- **No null** — absence is always `Option T`.
- **Pointers are contained in data** — pointers exist only as the internal representation of heap-backed values (e.g. `List T`, `Arr T`, `String`); no programmer-facing pointer syntax exists.
- **Actor lifetimes** — the one exception to linearity: `Actor T` handles are copyable and carry an internal reference count; an actor is abandoned when the last handle leaves scope.

## Actor model

Actors are the sole concurrency mechanism and the sole source of side effects — every other function is pure. An actor is a single-threaded message processor with one FIFO mailbox, threading its state through each turn. Messages carry data by ownership transfer and handles by copy. Actors stop when the last handle to them leaves scope. Actor behaviour is fully deterministic.

## Syntax

```wag
import
  prelude
    ..

-- Function: signature, parameters, body
add = Int, Int -> Int: a, b -> a + b

-- Datatype: uppercase constructors, whitespace-separated fields
Shape = Circle Int | Rect Int Int

-- Pattern matching consumes the matched value
area = Shape -> Int: s ->
  match s
    Circle r -> r * r
    Rect w h -> w * h

-- Actor: created by application, addressed with a trailing @
CounterMsg = Increment | Get (Actor ReplyMsg)
ReplyMsg   = CountIs Int

@counter = Int, CounterMsg -> Int: state, msg -> match msg
  Increment -> state + 1
  Get replyTo@ ->
    replyTo@ (CountIs (copy state))
    state

MainMsg = Start (List String) | End Int

@main = @std: Int, MainMsg -> Int: st, msg ->
  match msg
    Start argv ->
      c@ = @counter 0
      c@ (Increment)
      self@ (End 0)
      0
    End code -> ()
```

## Workspace

| Crate | Role |
|-------|------|
| `wag-syntax` | Lexer, layout, Pratt parser, desugaring |
| `wag-types` | Type inference, exhaustiveness |
| `wag-check` | Linearity, field paths, tail cycles |
| `wag-interp` | Values, heap + regions, trampoline |
| `wag-runtime` | Actor scheduler, `@std`/`@net` I/O |
| `wag-cli` | `wagc` binary: parse → type → check → load → run |

## Build & run

```bash
cargo build --workspace
cargo test --workspace
cargo run -p wag-cli --bin wagc -- run path/to/file.wag
```

## License

Apache License 2.0 — see [LICENSE](LICENSE).
