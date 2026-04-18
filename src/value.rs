//! Dynamic value system for template data.
//!
//! Go's template engine inspects arbitrary types at runtime via reflection.
//! In Rust, we use the [`Value`] enum, similar to `serde_json::Value`.
//!
//! The [`ToValue`](crate::ToValue) trait allows converting Rust types into [`Value`]s, and the
//! [`tmap!`](crate::tmap) macro provides a convenient way to build data maps.
//!
//! # Examples
//!
//! ```
//! use gotmpl::{tmap, ToValue};
//!
//! let data = tmap! {
//!     "Name" => "Alice",
//!     "Age" => 30i64,
//!     "Tags" => vec!["admin".to_string(), "user".to_string()],
//! };
//! ```

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;

use crate::error::Result;

/// Type alias for a callable function stored inside [`Value::Function`].
///
/// This is an `Arc`-wrapped closure so that [`Value`] remains [`Clone`].
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use gotmpl::{Value, ValueFunc};
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
/// Every piece of data flowing through the template engine (dot, variables,
/// function arguments, pipeline results) is a `Value`. This plays the role
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
    /// The nil value, represents absence of data.
    Nil,
    /// A boolean value.
    Bool(bool),
    /// A 64-bit signed integer.
    Int(i64),
    /// A 64-bit floating-point number.
    Float(f64),
    /// A UTF-8 string.
    String(Arc<str>),
    /// An ordered list of values.
    ///
    /// Uses [`Arc<[Value]>`] for cheap cloning and single-allocation storage.
    List(Arc<[Value]>),
    /// A sorted string-keyed map of values.
    ///
    /// Uses [`BTreeMap`] with [`Arc<str>`] keys to ensure deterministic
    /// iteration order (matching Go's sorted map key iteration in templates)
    /// and to let `{{range}}` over a map refcount-bump the key into
    /// [`Value::String`] instead of allocating.
    Map(Arc<BTreeMap<Arc<str>, Value>>),
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
            Value::String(s) => Value::String(Arc::clone(s)),
            Value::List(v) => Value::List(Arc::clone(v)),
            Value::Map(m) => Value::Map(Arc::clone(m)),
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
    /// "key missing", which matters for the `missingkey=error` option.
    ///
    /// In Go, this would use reflection to access struct fields or map keys.
    ///
    /// # Examples
    ///
    /// ```
    /// use gotmpl::tmap;
    /// use gotmpl::Value;
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
                if *i < 0 {
                    return Err(crate::error::TemplateError::Exec(format!(
                        "index out of range: {}",
                        i
                    )));
                }
                let idx = *i as usize;
                if idx >= v.len() {
                    return Err(crate::error::TemplateError::Exec(format!(
                        "index out of range: {}",
                        i
                    )));
                }
                Ok(v[idx].clone())
            }
            (Value::List(_), _) => Err(crate::error::TemplateError::Exec(format!(
                "cannot index list with type {}",
                idx.type_name()
            ))),
            (Value::Map(m), Value::String(k)) => {
                Ok(m.get(k.as_ref()).cloned().unwrap_or(Value::Nil))
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
        // Resolve caller-supplied i64 bounds into usize indices in [0, len].
        // `len as i64` is lossless: Rust caps allocations at isize::MAX, which
        // fits in i64 on every supported target.
        fn resolve(
            kind: &str,
            start: Option<i64>,
            end: Option<i64>,
            len: usize,
        ) -> Result<(usize, usize)> {
            let len_i = len as i64;
            let start = start.unwrap_or(0);
            let end = end.unwrap_or(len_i);
            if start < 0 || end < 0 || start > len_i || end > len_i || start > end {
                return Err(crate::error::TemplateError::Exec(format!(
                    "slice: {kind} index out of range [{start}:{end}] with length {len}"
                )));
            }
            Ok((start as usize, end as usize))
        }
        match self {
            Value::List(v) => {
                let (s, e) = resolve("list", start, end, v.len())?;
                if s == 0 && e == v.len() {
                    return Ok(Value::List(Arc::clone(v)));
                }
                Ok(Value::List(Arc::from(&v[s..e])))
            }
            Value::String(str) => {
                let (s, e) = resolve("string", start, end, str.len())?;
                if !str.is_char_boundary(s) || !str.is_char_boundary(e) {
                    return Err(crate::error::TemplateError::Exec(
                        "slice: index not on UTF-8 character boundary".to_string(),
                    ));
                }
                if s == 0 && e == str.len() {
                    return Ok(Value::String(Arc::clone(str)));
                }
                Ok(Value::String(Arc::from(&str[s..e])))
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
            Value::String(s) => Some(s),
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

    /// Creates a [`Value::Map`] from a fixed-size array of key-value pairs.
    ///
    /// This is the constructor used by the [`tmap!`](crate::tmap) macro.
    #[doc(hidden)]
    pub fn from_entries<const N: usize>(entries: [(String, Value); N]) -> Self {
        Value::Map(Arc::new(
            entries
                .into_iter()
                .map(|(k, v)| (Arc::from(k), v))
                .collect(),
        ))
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
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Map(a), Value::Map(b)) => a == b,
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
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
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
/// use gotmpl::{Value, ToValue};
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

// ─── Primitive scalar impls ──────────────────────────────────────────────

impl ToValue for bool {
    fn to_value(&self) -> Value {
        Value::Bool(*self)
    }
}

macro_rules! impl_to_value_int {
    ($($t:ty),*) => {
        $(impl ToValue for $t {
            fn to_value(&self) -> Value {
                Value::Int(*self as i64)
            }
        })*
    };
}

impl_to_value_int!(i8, i16, i32, i64, u8, u16, u32, u64, isize, usize);

impl ToValue for f32 {
    fn to_value(&self) -> Value {
        Value::Float(*self as f64)
    }
}

impl ToValue for f64 {
    fn to_value(&self) -> Value {
        Value::Float(*self)
    }
}

// ─── String-like impls ──────────────────────────────────────────────────

impl ToValue for str {
    fn to_value(&self) -> Value {
        Value::String(Arc::from(self))
    }
}

impl ToValue for String {
    fn to_value(&self) -> Value {
        Value::String(Arc::from(self.as_str()))
    }
}

impl ToValue for alloc::borrow::Cow<'_, str> {
    fn to_value(&self) -> Value {
        Value::String(Arc::from(self.as_ref()))
    }
}

// ─── Reference / wrapper impls ──────────────────────────────────────────

impl<T: ToValue + ?Sized> ToValue for &T {
    fn to_value(&self) -> Value {
        (*self).to_value()
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

// ─── List-like collection impls ─────────────────────────────────────────

impl<T: ToValue> ToValue for [T] {
    fn to_value(&self) -> Value {
        Value::List(
            self.iter()
                .map(ToValue::to_value)
                .collect::<Vec<_>>()
                .into(),
        )
    }
}

impl<T: ToValue, const N: usize> ToValue for [T; N] {
    fn to_value(&self) -> Value {
        Value::List(
            self.iter()
                .map(ToValue::to_value)
                .collect::<Vec<_>>()
                .into(),
        )
    }
}

impl<T: ToValue> ToValue for Vec<T> {
    fn to_value(&self) -> Value {
        Value::List(
            self.iter()
                .map(ToValue::to_value)
                .collect::<Vec<_>>()
                .into(),
        )
    }
}

impl<T: ToValue> ToValue for alloc::collections::VecDeque<T> {
    fn to_value(&self) -> Value {
        Value::List(
            self.iter()
                .map(ToValue::to_value)
                .collect::<Vec<_>>()
                .into(),
        )
    }
}

impl<T: ToValue> ToValue for alloc::collections::LinkedList<T> {
    fn to_value(&self) -> Value {
        Value::List(
            self.iter()
                .map(ToValue::to_value)
                .collect::<Vec<_>>()
                .into(),
        )
    }
}

impl<T: ToValue> ToValue for alloc::collections::BTreeSet<T> {
    fn to_value(&self) -> Value {
        Value::List(
            self.iter()
                .map(ToValue::to_value)
                .collect::<Vec<_>>()
                .into(),
        )
    }
}

#[cfg(feature = "std")]
impl<T: ToValue> ToValue for std::collections::HashSet<T> {
    fn to_value(&self) -> Value {
        // Collect and sort for deterministic output.
        let mut items: Vec<Value> = self.iter().map(ToValue::to_value).collect();
        items.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        Value::List(items.into())
    }
}

// ─── Map-like collection impls ──────────────────────────────────────────

impl<T: ToValue> ToValue for BTreeMap<String, T> {
    fn to_value(&self) -> Value {
        Value::Map(Arc::new(
            self.iter()
                .map(|(k, v)| (Arc::from(k.as_str()), v.to_value()))
                .collect(),
        ))
    }
}

impl<T: ToValue> ToValue for BTreeMap<&str, T> {
    fn to_value(&self) -> Value {
        Value::Map(Arc::new(
            self.iter()
                .map(|(k, v)| (Arc::from(*k), v.to_value()))
                .collect(),
        ))
    }
}

#[cfg(feature = "std")]
impl<T: ToValue> ToValue for std::collections::HashMap<String, T> {
    fn to_value(&self) -> Value {
        Value::Map(Arc::new(
            self.iter()
                .map(|(k, v)| (Arc::from(k.as_str()), v.to_value()))
                .collect(),
        ))
    }
}

#[cfg(feature = "std")]
impl<T: ToValue> ToValue for std::collections::HashMap<&str, T> {
    fn to_value(&self) -> Value {
        Value::Map(Arc::new(
            self.iter()
                .map(|(k, v)| (Arc::from(*k), v.to_value()))
                .collect(),
        ))
    }
}

// ─── From conversions for common types ─────────────────────────────────

impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::String(Arc::from(s))
    }
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::String(Arc::from(s))
    }
}

impl From<BTreeMap<String, Value>> for Value {
    fn from(m: BTreeMap<String, Value>) -> Self {
        Value::Map(Arc::new(
            m.into_iter().map(|(k, v)| (Arc::from(k), v)).collect(),
        ))
    }
}

impl From<BTreeMap<Arc<str>, Value>> for Value {
    fn from(m: BTreeMap<Arc<str>, Value>) -> Self {
        Value::Map(Arc::new(m))
    }
}

impl From<Vec<Value>> for Value {
    fn from(v: Vec<Value>) -> Self {
        Value::List(v.into())
    }
}

impl From<Arc<str>> for Value {
    fn from(s: Arc<str>) -> Self {
        Value::String(s)
    }
}

impl From<Arc<[Value]>> for Value {
    fn from(v: Arc<[Value]>) -> Self {
        Value::List(v)
    }
}

impl From<Arc<BTreeMap<Arc<str>, Value>>> for Value {
    fn from(m: Arc<BTreeMap<Arc<str>, Value>>) -> Self {
        Value::Map(m)
    }
}

/// Converts a [`std::collections::HashMap<String, Value>`] into a [`Value::Map`].
///
/// Useful when you already have data in a `HashMap` and want to pass it
/// to a template without manually converting to `BTreeMap`.
///
/// # Examples
///
/// ```
/// use std::collections::HashMap;
/// use gotmpl::Value;
///
/// let mut hm = HashMap::new();
/// hm.insert("key".to_string(), Value::Int(42));
/// let val = Value::from(hm);
/// assert!(matches!(val, Value::Map(_)));
/// ```
#[cfg(feature = "std")]
impl From<std::collections::HashMap<String, Value>> for Value {
    fn from(m: std::collections::HashMap<String, Value>) -> Self {
        Value::Map(Arc::new(
            m.into_iter().map(|(k, v)| (Arc::from(k), v)).collect(),
        ))
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
/// use gotmpl::{tmap, ToValue};
/// use gotmpl::Value;
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
        $crate::Value::from_entries([])
    };
    ($($key:expr => $val:expr),+ $(,)?) => {
        $crate::Value::from_entries([
            $(($key.to_string(), $crate::ToValue::to_value(&$val)),)+
        ])
    };
}
