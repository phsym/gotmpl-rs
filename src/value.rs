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
//! use go_template::{tmap, ToValue};
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
/// use go_template::{Value, ValueFunc};
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

    /// Look up a field by name on a [`Value::Map`].
    ///
    /// Returns `Some(&value)` when the key exists (the value may itself be
    /// [`Value::Nil`]), and `None` when the key is absent or the receiver is
    /// not a map. This lets callers distinguish "key set to nil" from
    /// "key missing" — important for the `missingkey=error` option.
    ///
    /// In Go, this would use reflection to access struct fields or map keys.
    ///
    /// # Examples
    ///
    /// ```
    /// use go_template::tmap;
    /// use go_template::Value;
    ///
    /// let data = tmap! { "Name" => "Alice", "Empty" => Value::Nil };
    /// assert_eq!(data.field("Name"), Some(&Value::String("Alice".into())));
    /// assert_eq!(data.field("Empty"), Some(&Value::Nil));   // key exists
    /// assert_eq!(data.field("Missing"), None);               // key absent
    /// assert_eq!(Value::Int(1).field("x"), None);            // not a map
    /// ```
    pub fn field(&self, name: &str) -> Option<&Value> {
        match self {
            Value::Map(m) => m.get(name),
            _ => None,
        }
    }

    /// Index into a [`Value::List`] (by integer) or [`Value::Map`] (by string).
    ///
    /// Mirrors Go's `index` builtin semantics:
    /// - **List + Int**: returns the element, or an error if out of bounds.
    /// - **Map + String**: returns the value, or [`Value::Nil`] for missing keys.
    /// - **Nil + anything**: returns an error (`index of untyped nil`).
    /// - **Other combinations**: returns an error (type mismatch).
    ///
    /// # Errors
    ///
    /// Returns an error on out-of-bounds list access, indexing with an
    /// incompatible key type, or indexing a non-indexable value.
    pub fn index(&self, idx: &Value) -> Result<Value> {
        match (self, idx) {
            (Value::List(v), Value::Int(i)) => {
                let i = *i as usize;
                if i >= v.len() {
                    return Err(crate::error::TemplateError::Exec(format!(
                        "index out of range [{}] with length {}",
                        i,
                        v.len()
                    )));
                }
                Ok(v[i].clone())
            }
            (Value::List(_), _) => Err(crate::error::TemplateError::Exec(format!(
                "cannot index list with type {}",
                idx.type_name()
            ))),
            (Value::Map(m), Value::String(k)) => {
                Ok(m.get(k.as_str()).cloned().unwrap_or(Value::Nil))
            }
            (Value::Map(_), _) => Err(crate::error::TemplateError::Exec(format!(
                "cannot index map with type {}",
                idx.type_name()
            ))),
            (Value::Nil, _) => Err(crate::error::TemplateError::Exec(
                "index of untyped nil".into(),
            )),
            _ => Err(crate::error::TemplateError::Exec(format!(
                "cannot index type {}",
                self.type_name()
            ))),
        }
    }

    /// Returns a short type name for use in error messages.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Nil => "nil",
            Value::Bool(_) => "bool",
            Value::Int(_) => "int",
            Value::Float(_) => "float64",
            Value::String(_) => "string",
            Value::List(_) => "list",
            Value::Map(_) => "map",
            Value::Function(_) => "func",
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

    /// Slice a [`Value::List`] or [`Value::String`] by byte range.
    ///
    /// Mirrors Go's `slice` builtin: `slice x`, `slice x i`, `slice x i j`.
    /// Omitted bounds default to `0` (start) and `len` (end).
    ///
    /// # Errors
    ///
    /// Returns an error if the indices are out of range, inverted, on a
    /// non-UTF-8-char boundary (strings), or the value is not sliceable.
    pub fn slice(&self, start: Option<i64>, end: Option<i64>) -> Result<Value> {
        match self {
            Value::List(v) => {
                let start = start.unwrap_or(0) as usize;
                let end = end.map(|n| n as usize).unwrap_or(v.len());
                if start > v.len() || end > v.len() || start > end {
                    return Err(crate::error::TemplateError::Exec(format!(
                        "slice: index out of range [{}:{}] with length {}",
                        start,
                        end,
                        v.len()
                    )));
                }
                Ok(Value::List(v[start..end].to_vec()))
            }
            Value::String(s) => {
                let start = start.unwrap_or(0) as usize;
                let end = end.map(|n| n as usize).unwrap_or(s.len());
                if start > s.len() || end > s.len() || start > end {
                    return Err(crate::error::TemplateError::Exec(format!(
                        "slice: index out of range [{}:{}] with length {}",
                        start,
                        end,
                        s.len()
                    )));
                }
                if !s.is_char_boundary(start) || !s.is_char_boundary(end) {
                    return Err(crate::error::TemplateError::Exec(
                        "slice: index not on UTF-8 character boundary".to_string(),
                    ));
                }
                Ok(Value::String(s[start..end].to_string()))
            }
            _ => Err(crate::error::TemplateError::Exec(format!(
                "slice: cannot slice type {}",
                self.type_name()
            ))),
        }
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

/// Rust-side equality for [`Value`].
///
/// This comparison is type-strict: values must have the same variant to be equal
/// (except `Nil == Nil`).
///
/// Template builtins (`eq`, `ne`) implement Go-compatible comparison error
/// semantics separately in `funcs.rs`.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            _ => false,
        }
    }
}

/// Rust-side partial ordering for [`Value`].
///
/// Supports ordering for same-type numeric/string variants only:
/// [`Value::Int`], [`Value::Float`], [`Value::String`].
/// Returns `None` for all other combinations.
///
/// Template builtins (`lt`, `le`, `gt`, `ge`) implement Go-compatible comparison
/// error semantics separately in `funcs.rs`.
impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
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
/// use go_template::{Value, ToValue};
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
/// use go_template::Value;
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
/// use go_template::{tmap, ToValue};
/// use go_template::Value;
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
