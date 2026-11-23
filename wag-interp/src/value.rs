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
    Bool(bool),
    Unit,
    Fn(FnId),
    Actor(ActorHandle),
    /// Owned composite: product, list node, array, user data type (D081).
    Data(ConstructorId, Vec<Value>),
    /// Single-field composite — no `Vec` wrapper (Data1 optimisation).
    Data1(ConstructorId, Box<Value>),
    /// Partially-applied function / closure — captured args owned inline.
    Closure(FnId, Vec<Value>),
}

impl Value {
    // -- Data helpers --------------------------------------------------------

    /// True if this is an owned composite (`Data` or `Data1`).
    #[inline]
    pub fn is_data(&self) -> bool {
        matches!(self, Value::Data(..) | Value::Data1(..))
    }

    /// Borrow the field slice if this is a `Data`/`Data1` value.
    #[inline]
    pub fn data_fields(&self) -> Option<&[Value]> {
        match self {
            Value::Data(_, fields) => Some(fields),
            Value::Data1(_, field) => Some(std::slice::from_ref(field.as_ref())),
            _ => None,
        }
    }

    /// Borrow the constructor id if this is a `Data`/`Data1` value.
    #[inline]
    pub fn data_ctor(&self) -> Option<ConstructorId> {
        match self {
            Value::Data(ctor, _) => Some(*ctor),
            Value::Data1(ctor, _) => Some(*ctor),
            _ => None,
        }
    }

    /// Consume the value and return its fields (panics if not `Data`/`Data1`).
    #[inline]
    pub fn into_data(self) -> (ConstructorId, Vec<Value>) {
        match self {
            Value::Data(ctor, fields) => (ctor, fields),
            Value::Data1(ctor, field) => (ctor, vec![*field]),
            other => panic!("Value::into_data on non-Data: {other:?}"),
        }
    }

    /// Construct a `Data`/`Data1` value from a constructor id and fields.
    /// Uses `Data1` (no `Vec` wrapper) for single-field constructors.
    #[inline]
    pub fn data_value(ctor: ConstructorId, fields: Vec<Value>) -> Value {
        if fields.len() == 1 {
            Value::Data1(ctor, Box::new(fields.into_iter().next().unwrap()))
        } else {
            Value::Data(ctor, fields)
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

    // -- Display -------------------------------------------------------------

    /// Short human-readable description (no heap deref needed).
    pub fn brief_desc(&self) -> String {
        use Value::*;
        match self {
            Int(i) => format!("Int({i})"),
            Bool(b) => format!("Bool({b})"),
            Unit => "Unit".into(),
            Fn(id) => format!("Fn({})", id.0),
            Actor(a) => format!("Actor({})", a.id.0),
            Data(ctor, fields) => {
                format!("Data(ctor={:?}, {}f)", ctor, fields.len())
            }
            Data1(ctor, field) => {
                format!("Data1(ctor={:?}, {})", ctor, field.brief_desc())
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
            (Bool(a), Bool(b)) => a == b,
            (Unit, Unit) => true,
            (Fn(a), Fn(b)) => a == b,
            // Actor: identity by id (not Arc pointer).
            (Actor(a), Actor(b)) => a.id == b.id,
            (Data(ctor_a, fa), Data(ctor_b, fb)) => {
                ctor_a == ctor_b && fa == fb
            }
            (Data1(ctor_a, fa), Data1(ctor_b, fb)) => {
                ctor_a == ctor_b && fa == fb
            }
            // Cross-compatibility: Data(ctor, [x]) == Data1(ctor, x).
            (Data(ctor_a, fa), Data1(ctor_b, fb)) if fa.len() == 1 => {
                ctor_a == ctor_b && &fa[0] == fb.as_ref()
            }
            (Data1(ctor_a, fa), Data(ctor_b, fb)) if fb.len() == 1 => {
                ctor_a == ctor_b && fa.as_ref() == &fb[0]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cid(n: u32) -> ConstructorId {
        ConstructorId(n)
    }

    // -- data_value ----------------------------------------------------------

    #[test]
    fn data_value_zero_fields_is_data() {
        let v = Value::data_value(cid(1), vec![]);
        assert!(matches!(v, Value::Data(c, f) if c == cid(1) && f.is_empty()));
    }

    #[test]
    fn data_value_single_field_is_data1() {
        let v = Value::data_value(cid(1), vec![Value::Int(42)]);
        assert!(matches!(v, Value::Data1(c, f) if c == cid(1) && *f == Value::Int(42)));
    }

    #[test]
    fn data_value_multi_field_is_data() {
        let v = Value::data_value(cid(2), vec![Value::Int(1), Value::Bool(true)]);
        assert!(matches!(v, Value::Data(c, f) if c == cid(2) && f.len() == 2));
    }

    // -- Data helpers on Data1 -----------------------------------------------

    #[test]
    fn data1_helpers() {
        let v = Value::Data1(cid(3), Box::new(Value::Int(7)));
        assert!(v.is_data());
        assert_eq!(v.data_ctor(), Some(cid(3)));
        let fields = v.data_fields().expect("Data1 has fields");
        assert_eq!(fields, std::slice::from_ref(&Value::Int(7)));
        let (c, fs) = Value::Data1(cid(3), Box::new(Value::Int(7))).into_data();
        assert_eq!(c, cid(3));
        assert_eq!(fs, vec![Value::Int(7)]);
    }

    #[test]
    fn data_helpers_on_data() {
        let v = Value::Data(cid(4), vec![Value::Int(1), Value::Int(2)]);
        assert!(v.is_data());
        assert_eq!(v.data_ctor(), Some(cid(4)));
        assert_eq!(v.data_fields().unwrap().len(), 2);
    }

    #[test]
    fn non_data_helpers_return_none() {
        let v = Value::Int(5);
        assert!(!v.is_data());
        assert_eq!(v.data_ctor(), None);
        assert_eq!(v.data_fields(), None);
    }

    // -- PartialEq cross-compatibility ----------------------------------------

    #[test]
    fn data_data1_cross_equal_same_ctor() {
        let a = Value::Data(cid(1), vec![Value::Int(9)]);
        let b = Value::Data1(cid(1), Box::new(Value::Int(9)));
        assert_eq!(a, b);
        assert_eq!(b, a);
    }

    #[test]
    fn data_data1_cross_unequal_different_ctor() {
        let a = Value::Data(cid(1), vec![Value::Int(9)]);
        let b = Value::Data1(cid(2), Box::new(Value::Int(9)));
        assert_ne!(a, b);
    }

    #[test]
    fn data_data1_cross_unequal_different_field() {
        let a = Value::Data(cid(1), vec![Value::Int(9)]);
        let b = Value::Data1(cid(1), Box::new(Value::Int(10)));
        assert_ne!(a, b);
    }

    #[test]
    fn data_multi_field_never_equals_data1() {
        // A 2-field Data must not match a Data1 even with the same ctor.
        let a = Value::Data(cid(1), vec![Value::Int(9), Value::Int(9)]);
        let b = Value::Data1(cid(1), Box::new(Value::Int(9)));
        assert_ne!(a, b);
    }

    #[test]
    fn data1_nested_structural_equality() {
        // Char-shaped values: Data1(ctor, Int) — structural equality through the box.
        let a = Value::Data1(cid(5), Box::new(Value::Int(104)));
        let b = Value::Data1(cid(5), Box::new(Value::Int(104)));
        let c = Value::Data1(cid(5), Box::new(Value::Int(233)));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn data1_vs_data_different_variants_unequal() {
        let a = Value::Data1(cid(1), Box::new(Value::Int(9)));
        let b = Value::Int(9);
        assert_ne!(a, b);
    }

    // -- brief_desc ------------------------------------------------------------

    #[test]
    fn brief_desc_data1() {
        let v = Value::Data1(cid(7), Box::new(Value::Int(42)));
        assert_eq!(v.brief_desc(), "Data1(ctor=ConstructorId(7), Int(42))");
    }

    #[test]
    fn brief_desc_data() {
        let v = Value::Data(cid(7), vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(v.brief_desc(), "Data(ctor=ConstructorId(7), 2f)");
    }
}
