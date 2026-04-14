//! Dynamic value system for template data.
//!
//! Go's template engine inspects arbitrary types at runtime via reflection.
//! In Rust, we use the [`Value`] enum — an approach similar to `serde_json::Value`.
//!
//! The [`ToValue`](crate::ToValue) trait allows converting Rust types into [`Value`]s, and the
//! [`tmap!`](crate::tmap) macro provides a convenient way to build data maps.
//!
//! # Examples
//!
//! ```
//! use go_template_rs::{tmap, ToValue};
//!
//! let data = tmap! {
//!     "Name" => "Alice",
//!     "Age" => 30i64,
//!     "Tags" => vec!["admin".to_string(), "user".to_string()],
//! };
//! ```

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::sync::Arc;

use crate::error::Result;

/// Type alias for a callable function stored inside [`Value::Function`].
///
/// This is an `Arc`-wrapped closure so that [`Value`] remains [`Clone`].
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use go_template_rs::{Value, ValueFunc};
///
/// let add: ValueFunc = Arc::new(|args| {
///     let sum: i64 = args.iter().filter_map(|a| a.as_int()).sum();
///     Ok(Value::Int(sum))
/// });
///
/// let result = add(&[Value::Int(2), Value::Int(3)]).unwrap();
/// assert_eq!(result, Value::Int(5));
/// ```
pub type ValueFunc = Arc<dyn Fn(&[Value]) -> Result<Value> + Send + Sync>;

/// The core dynamic type for template data.
///
/// Every piece of data flowing through the template engine — dot, variables,
/// function arguments, pipeline results — is a `Value`. This plays the role
/// that `reflect.Value` plays in Go's template engine.
///
/// # Truthiness
///
/// [`Value::is_truthy`] follows Go's semantics:
///
/// | Value | Truthy? |
/// |-------|---------|
/// | `Nil` | `false` |
/// | `Bool(false)` | `false` |
/// | `Int(0)` | `false` |
/// | `Float(0.0)` | `false` |
/// | `String("")` | `false` |
/// | Empty `List` or `Map` | `false` |
/// | Everything else | `true` |
///
/// # Display
///
/// The [`Display`](fmt::Display) implementation matches Go's default formatting:
/// - `Nil` → `<nil>`
/// - `List` → `[a b c]`
/// - `Map` → `map[k1:v1 k2:v2]`
/// - `Function` → `<func>`
pub enum Value {
    /// The nil value — represents absence of data.
    Nil,
    /// A boolean value.
    Bool(bool),
    /// A 64-bit signed integer.
    Int(i64),
    /// A 64-bit floating-point number.
    Float(f64),
    /// A UTF-8 string.
    String(String),
    /// An ordered list of values.
    List(Vec<Value>),
    /// A sorted string-keyed map of values.
    ///
    /// Uses [`BTreeMap`] to ensure deterministic iteration order (matching
    /// Go's sorted map key iteration in templates).
    Map(BTreeMap<String, Value>),
    /// A callable function value, invoked via the `call` builtin.
    ///
    /// See [`ValueFunc`] for the expected signature.
    Function(ValueFunc),
}

impl Clone for Value {
    fn clone(&self) -> Self {
        match self {
            Value::Nil => Value::Nil,
            Value::Bool(b) => Value::Bool(*b),
            Value::Int(n) => Value::Int(*n),
            Value::Float(f) => Value::Float(*f),
            Value::String(s) => Value::String(s.clone()),
            Value::List(v) => Value::List(v.clone()),
            Value::Map(m) => Value::Map(m.clone()),
            Value::Function(f) => Value::Function(Arc::clone(f)),
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "Nil"),
            Value::Bool(b) => write!(f, "Bool({b:?})"),
            Value::Int(n) => write!(f, "Int({n:?})"),
            Value::Float(v) => write!(f, "Float({v:?})"),
            Value::String(s) => write!(f, "String({s:?})"),
            Value::List(v) => write!(f, "List({v:?})"),
            Value::Map(m) => write!(f, "Map({m:?})"),
            Value::Function(_) => write!(f, "Function(...)"),
        }
    }
}

impl Value {
    /// Returns whether this value is "truthy" according to Go's template semantics.
    ///
    /// See the [type-level docs](Value) for the full truthiness table.
    pub fn is_truthy(&self) -> bool {
        match self {
            Value::Nil => false,
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::Float(f) => *f != 0.0,
            Value::String(s) => !s.is_empty(),
            Value::List(v) => !v.is_empty(),
            Value::Map(m) => !m.is_empty(),
            Value::Function(_) => true,
        }
    }

    /// Access a field by name on a [`Value::Map`]. Returns [`Value::Nil`] if the
    /// key is missing or the value is not a map.
    ///
    /// In Go, this would use reflection to access struct fields or map keys.
    ///
    /// # Examples
    ///
    /// ```
    /// use go_template_rs::tmap;
    /// use go_template_rs::Value;
    ///
    /// let data = tmap! { "Name" => "Alice" };
    /// assert_eq!(data.field("Name"), Value::String("Alice".into()));
    /// assert_eq!(data.field("Missing"), Value::Nil);
    /// ```
    pub fn field(&self, name: &str) -> Value {
        match self {
            Value::Map(m) => m.get(name).cloned().unwrap_or(Value::Nil),
            _ => Value::Nil,
        }
    }

    /// Index into a [`Value::List`] (by integer) or [`Value::Map`] (by string).
    ///
    /// Returns [`Value::Nil`] if the index is out of bounds, the key is missing,
    /// or the types don't match. Mirrors Go's `index` builtin.
    pub fn index(&self, idx: &Value) -> Value {
        match (self, idx) {
            (Value::List(v), Value::Int(i)) => {
                let i = *i as usize;
                v.get(i).cloned().unwrap_or(Value::Nil)
            }
            (Value::Map(m), Value::String(k)) => m.get(k.as_str()).cloned().unwrap_or(Value::Nil),
            _ => Value::Nil,
        }
    }

    /// Returns the length of a string, list, or map.
    ///
    /// Returns `None` for types that have no concept of length.
    /// Mirrors Go's `len` builtin.
    pub fn len(&self) -> Option<usize> {
        match self {
            Value::String(s) => Some(s.len()),
            Value::List(v) => Some(v.len()),
            Value::Map(m) => Some(m.len()),
            _ => None,
        }
    }

    /// Returns `Some(true)` if the value has a length and that length is zero.
    ///
    /// Returns `None` for types that have no concept of length.
    pub fn is_empty(&self) -> Option<bool> {
        self.len().map(|n| n == 0)
    }

    /// Extracts a string slice if this is a [`Value::String`].
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    /// Extracts an `i64` if this is a [`Value::Int`], or truncates a [`Value::Float`].
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            Value::Float(f) => Some(*f as i64),
            _ => None,
        }
    }

    /// Extracts an `f64` if this is a [`Value::Float`], or widens a [`Value::Int`].
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(n) => Some(*n as f64),
            _ => None,
        }
    }

    /// Returns `true` if this is a [`Value::Function`].
    pub fn is_function(&self) -> bool {
        matches!(self, Value::Function(_))
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "<nil>"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Int(n) => write!(f, "{n}"),
            Value::Float(v) => write!(f, "{v}"),
            Value::String(s) => write!(f, "{s}"),
            Value::List(v) => {
                write!(f, "[")?;
                for (i, item) in v.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
            Value::Map(m) => {
                write!(f, "map[")?;
                for (i, (k, v)) in m.iter().enumerate() {
                    if i > 0 {
                        write!(f, " ")?;
                    }
                    write!(f, "{k}:{v}")?;
                }
                write!(f, "]")
            }
            Value::Function(_) => write!(f, "<func>"),
        }
    }
}

/// Equality comparison with numeric coercion.
///
/// [`Value::Int`] and [`Value::Float`] are comparable across types
/// (e.g., `Int(1) == Float(1.0)`). All other cross-type comparisons
/// return `false`.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::Int(a), Value::Float(b)) => (*a as f64) == *b,
            (Value::Float(a), Value::Int(b)) => *a == (*b as f64),
            _ => false,
        }
    }
}

/// Ordering comparison with numeric coercion.
///
/// Supports ordering for [`Value::Int`], [`Value::Float`] (including cross-type),
/// and [`Value::String`]. Returns `None` for all other type combinations.
impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::Int(a), Value::Float(b)) => (*a as f64).partial_cmp(b),
            (Value::Float(a), Value::Int(b)) => a.partial_cmp(&(*b as f64)),
            (Value::String(a), Value::String(b)) => a.partial_cmp(b),
            _ => None,
        }
    }
}

// ─── ToValue trait ───────────────────────────────────────────────────────

/// Trait for converting Rust types into template [`Value`]s.
///
/// This is the Rust equivalent of Go's ability to pass any type to
/// `template.Execute()`. Blanket implementations are provided for common
/// types; implement this trait for your own types to pass them as template data.
///
/// # Examples
///
/// ```
/// use go_template_rs::{Value, ToValue};
///
/// assert_eq!(42i64.to_value(), Value::Int(42));
/// assert_eq!("hello".to_value(), Value::String("hello".into()));
/// assert_eq!(true.to_value(), Value::Bool(true));
///
/// let none: Option<i64> = None;
/// assert_eq!(none.to_value(), Value::Nil);
/// ```
pub trait ToValue {
    /// Convert this value into a template [`Value`].
    fn to_value(&self) -> Value;
}

impl ToValue for Value {
    fn to_value(&self) -> Value {
        self.clone()
    }
}

impl ToValue for bool {
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }
}

impl ToValue for i64 {
    fn to_value(&self) -> Value {
        Value::Int(*self)
    }
}

impl ToValue for i32 {
    fn to_value(&self) -> Value {
        Value::Int(*self as i64)
    }
}

impl ToValue for f64 {
    fn to_value(&self) -> Value {
        Value::Float(*self)
    }
}

impl ToValue for str {
    fn to_value(&self) -> Value {
        Value::String(self.to_string())
    }
}

impl<T: ToValue + ?Sized> ToValue for &T {
    fn to_value(&self) -> Value {
        (*self).to_value()
    }
}

impl ToValue for String {
    fn to_value(&self) -> Value {
        Value::String(self.clone())
    }
}

impl<T: ToValue> ToValue for Vec<T> {
    fn to_value(&self) -> Value {
        Value::List(self.iter().map(ToValue::to_value).collect())
    }
}

impl<T: ToValue> ToValue for BTreeMap<String, T> {
    fn to_value(&self) -> Value {
        Value::Map(
            self.iter()
                .map(|(k, v)| (k.clone(), v.to_value()))
                .collect(),
        )
    }
}

/// Converts `Some(v)` to `v.to_value()` and `None` to [`Value::Nil`].
impl<T: ToValue> ToValue for Option<T> {
    fn to_value(&self) -> Value {
        match self {
            Some(v) => v.to_value(),
            None => Value::Nil,
        }
    }
}

// ─── From conversions for common collection types ──────────────────────

/// Converts a [`HashMap<String, Value>`] into a [`Value::Map`].
///
/// Useful when you already have data in a `HashMap` and want to pass it
/// to a template without manually converting to `BTreeMap`.
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
/// use go_template_rs::Value;
///
/// let mut hm = HashMap::new();
/// hm.insert("key".to_string(), Value::Int(42));
/// let val = Value::from(hm);
/// assert!(matches!(val, Value::Map(_)));
/// ```
impl From<HashMap<String, Value>> for Value {
    fn from(m: HashMap<String, Value>) -> Self {
        Value::Map(m.into_iter().collect())
    }
}

// ─── Convenience macro for building Value::Map literals ──────────────────

/// Creates a [`Value::Map`] from key-value pairs, similar to Go's map literals.
///
/// Keys are converted to strings via `.to_string()`, and values are converted
/// via [`ToValue::to_value`].
///
/// # Examples
///
/// ```
/// use go_template_rs::{tmap, ToValue};
/// use go_template_rs::Value;
///
/// let data = tmap! {
///     "name" => "Alice",
///     "age" => 30i64,
///     "scores" => vec![95i64, 87, 92],
///     "address" => tmap! { "city" => "Paris" },
/// };
///
/// assert!(matches!(data, Value::Map(_)));
/// ```
#[macro_export]
macro_rules! tmap {
    () => {
        $crate::Value::Map(std::collections::BTreeMap::new())
    };
    ($($key:expr => $val:expr),+ $(,)?) => {
        $crate::Value::Map(std::collections::BTreeMap::from([
            $(($key.to_string(), $crate::ToValue::to_value(&$val)),)+
        ]))
    };
}
