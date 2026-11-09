//! Runtime values (§3.5) — plain owned recursive model (D081).
//!
//! [`Value`] is the only runtime value type. Composites are **owned**
//! (`Data(ctor, Vec<Value>)`); there is no heap arena, no cell index, and no
//! manual free. Rust's ownership + `Drop` reclaims memory recursively.
//!
//! WAG's linearity is a compile-time guarantee (§7 rules 10–11); the
//! interpreter does not track ownership at runtime. Borrows (`&T` params)
//! are erased to plain value copies before the interpreter runs (§4.3.3).

use std::fmt;
use std::sync::Arc;

/// Index into a flat, program-wide function table (§4.6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FnId(pub u32);

/// Index into the load-time constructor table (§3.5 / §4.6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ConstructorId(pub u32);

/// Actor identity (§5.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ActorId(pub u32);

// ============================================================================
// Reserved ConstructorIds for built-in data shapes (high range — never collide
// with user-assigned ids which start at 0).
//
// String and Closure are dedicated `Value` variants (not `Data`), so they have
// no ConstructorId here.
// ============================================================================

/// `(a, b)` pair — fields: `[a, b]`.
pub const CTOR_PAIR2: ConstructorId = ConstructorId(0xFFFF_FFFC);
/// `(a, b, c)` triple — fields: `[a, b, c]`.
pub const CTOR_PAIR3: ConstructorId = ConstructorId(0xFFFF_FFFB);
/// Product of 4–7 elements — fields: `[e0..en]`.
pub const CTOR_PRODUCT: ConstructorId = ConstructorId(0xFFFF_FFFA);
/// `Array T` — fields: `[elem0, elem1, ...]`.
pub const CTOR_ARRAY: ConstructorId = ConstructorId(0xFFFF_FFF8);

// ============================================================================
// Actor liveness (D078)
// ============================================================================

/// Liveness token for an actor handle. Cloning the `Arc` is O(1) and
/// increments the refcount; when the last clone drops, `on_zero` fires (if set),
/// enqueuing an abandonment event to stop the actor. System actors carry no sink.
pub struct ActorLiveness {
    pub id: ActorId,
    /// Called exactly once when the last handle is dropped. `None` for immortal
    /// system actors (`@std`, `@net`).
    pub on_zero: Option<Arc<dyn Fn(ActorId) + Send + Sync>>,
}

impl fmt::Debug for ActorLiveness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ActorLiveness({})", self.id.0)
    }
}

/// When the last `Arc<ActorLiveness>` drops, fire the abandonment sink (D078).
impl Drop for ActorLiveness {
    fn drop(&mut self) {
        if let Some(sink) = &self.on_zero {
            sink(self.id);
        }
    }
}

/// O(1)-clone actor handle.
pub type ActorHandle = Arc<ActorLiveness>;

// ============================================================================
// Value — the only runtime value type (D081: plain owned, no heap)
// ============================================================================

/// A WAG runtime value (§3.5). All composites are **owned** and recursive;
/// there is no indirection through a shared arena. Move = ownership transfer.
/// Drop = recursive reclamation. No manual free, no use-after-free possible.
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    /// A Unicode scalar value (codepoint) — inline (§3.11).
    Char(u32),
    Bool(bool),
    Unit,
    /// UTF-8 string (§3.10) — owned inline.
    Str(String),
    Fn(FnId),
    Actor(ActorHandle),
    /// Owned composite: product, list node, array, user data type (D081).
    Data(ConstructorId, Vec<Value>),
    /// Partially-applied function / closure — captured args owned inline.
    Closure(FnId, Vec<Value>),
}

impl Value {
    // -- Data helpers --------------------------------------------------------

    /// True if this is an owned composite (`Data`).
    #[inline]
    pub fn is_data(&self) -> bool {
        matches!(self, Value::Data(..))
    }

    /// Borrow the field vector if this is a `Data` value.
    #[inline]
    pub fn data_fields(&self) -> Option<&[Value]> {
        match self {
            Value::Data(_, fields) => Some(fields),
            _ => None,
        }
    }

    /// Borrow the constructor id if this is a `Data` value.
    #[inline]
    pub fn data_ctor(&self) -> Option<ConstructorId> {
        match self {
            Value::Data(ctor, _) => Some(*ctor),
            _ => None,
        }
    }

    /// Consume the value and return its fields (panics if not `Data`).
    #[inline]
    pub fn into_data(self) -> (ConstructorId, Vec<Value>) {
        match self {
            Value::Data(ctor, fields) => (ctor, fields),
            other => panic!("Value::into_data on non-Data: {other:?}"),
        }
    }

    // -- Closure helpers -----------------------------------------------------

    /// True if this is a closure.
    #[inline]
    pub fn is_closure(&self) -> bool {
        matches!(self, Value::Closure(..))
    }

    /// Borrow the captured-args vector if this is a `Closure`.
    #[inline]
    pub fn closure_args(&self) -> Option<&[Value]> {
        match self {
            Value::Closure(_, args) => Some(args),
            _ => None,
        }
    }

    // -- String helpers ------------------------------------------------------

    /// Borrow the string content if this is a `Str`.
    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    // -- Display -------------------------------------------------------------

    /// Short human-readable description (no heap deref needed).
    pub fn brief_desc(&self) -> String {
        use Value::*;
        match self {
            Int(i) => format!("Int({i})"),
            Char(c) => format!("Char({c})"),
            Bool(b) => format!("Bool({b})"),
            Unit => "Unit".into(),
            Str(s) => format!("Str(\"{}\")", s.chars().take(20).collect::<String>()),
            Fn(id) => format!("Fn({})", id.0),
            Actor(a) => format!("Actor({})", a.id.0),
            Data(ctor, fields) => {
                format!("Data(ctor={:?}, {}f)", ctor, fields.len())
            }
            Closure(fid, args) => {
                format!("Closure(fn={}, {} captured)", fid.0, args.len())
            }
        }
    }
}

// ============================================================================
// Equality — full structural (no heap deref needed; values are owned)
// ============================================================================

impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        use Value::*;
        match (self, other) {
            (Int(a), Int(b)) => a == b,
            (Char(a), Char(b)) => a == b,
            (Bool(a), Bool(b)) => a == b,
            (Unit, Unit) => true,
            (Str(a), Str(b)) => a == b,
            (Fn(a), Fn(b)) => a == b,
            // Actor: identity by id (not Arc pointer).
            (Actor(a), Actor(b)) => a.id == b.id,
            (Data(ctor_a, fa), Data(ctor_b, fb)) => {
                ctor_a == ctor_b && fa == fb
            }
            (Closure(fa, aa), Closure(fb, ab)) => fa == fb && aa == ab,
            _ => false,
        }
    }
}

impl Eq for Value {}

// Send + Sync are derived automatically: all variants contain only
// Send+Sync types (i64, u32, bool, String, FnId, Arc<ActorLiveness>,
// ConstructorId, Vec<Value>). No unsafe impls needed.
