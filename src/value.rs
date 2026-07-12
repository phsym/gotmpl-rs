//! Dynamic value system for template data.
//!
//! Go's template engine inspects arbitrary types at runtime through
//! reflection. Rust has no such reflection, so data is carried through the
//! engine as a [`Value`] enum, similar in spirit to `serde_json::Value`.
//!
//! The [`ToValue`](crate::ToValue) trait converts Rust types into [`Value`]s,
//! and the [`tmap!`](crate::tmap) macro builds data maps.
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

/// Alias for a callable function stored inside [`Value::Function`].
///
/// Wrapped in `Arc` so that [`Value`] remains [`Clone`].
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

/// The dynamic type for template data.
///
/// Dot, variables, function arguments, and pipeline results are all carried
/// as a `Value`. Plays the role that `reflect.Value` plays in Go's template
/// engine.
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
/// | `Uint(0)` | `false` |
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
    /// A 64-bit unsigned integer.
    ///
    /// Produced by [`ToValue`] for every unsigned Rust integer type
    /// (`u8`/`u16`/`u32`/`u64`/`usize`), mirroring Go's distinct unsigned
    /// reflect kinds. Kept separate from [`Value::Int`] so values above
    /// `i64::MAX` (e.g. `u64::MAX`) render and compare as their true unsigned
    /// value instead of wrapping to a negative `i64`.
    Uint(u64),
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

    /// Trusted content that bypasses context-aware escaping in its matching
    /// context. Produced by the [`html`](crate::html) module's safe-content
    /// wrappers ([`HTML`](crate::html::HTML), etc.). Available only with the
    /// `html` feature; the default build's `Value` has no such variant, so this
    /// addition is not a breaking change for `text/template` users.
    #[cfg(feature = "html")]
    #[cfg_attr(docsrs, doc(cfg(feature = "html")))]
    Safe {
        /// Which content type this string is trusted as.
        kind: SafeKind,
        /// The trusted string, emitted verbatim in its matching context.
        s: Arc<str>,
    },
}

/// The content type of a [`Value::Safe`] — the seven trusted-content kinds of
/// Go's `html/template` (`template.HTML`, `template.JS`, …). Available only with
/// the `html` feature.
#[cfg(feature = "html")]
#[cfg_attr(docsrs, doc(cfg(feature = "html")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeKind {
    /// Known-safe HTML markup ([`html::HTML`](crate::html::HTML)).
    Html,
    /// A known-safe HTML attribute fragment ([`html::HTMLAttr`](crate::html::HTMLAttr)).
    HtmlAttr,
    /// Known-safe JavaScript source ([`html::JS`](crate::html::JS)).
    Js,
    /// A known-safe JavaScript string-literal body ([`html::JSStr`](crate::html::JSStr)).
    JsStr,
    /// Known-safe CSS source ([`html::CSS`](crate::html::CSS)).
    Css,
    /// A known-safe URL ([`html::URL`](crate::html::URL)).
    Url,
    /// A known-safe `srcset` value ([`html::Srcset`](crate::html::Srcset)).
    Srcset,
}

impl Clone for Value {
    fn clone(&self) -> Self {
        match self {
            Value::Nil => Value::Nil,
            Value::Bool(b) => Value::Bool(*b),
            Value::Int(n) => Value::Int(*n),
            Value::Uint(n) => Value::Uint(*n),
            Value::Float(f) => Value::Float(*f),
            Value::String(s) => Value::String(Arc::clone(s)),
            Value::List(v) => Value::List(Arc::clone(v)),
            Value::Map(m) => Value::Map(Arc::clone(m)),
            Value::Function(f) => Value::Function(Arc::clone(f)),
            #[cfg(feature = "html")]
            Value::Safe { kind, s } => Value::Safe {
                kind: *kind,
                s: Arc::clone(s),
            },
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => write!(f, "Nil"),
            Value::Bool(b) => write!(f, "Bool({b:?})"),
            Value::Int(n) => write!(f, "Int({n:?})"),
            Value::Uint(n) => write!(f, "Uint({n:?})"),
            Value::Float(v) => write!(f, "Float({v:?})"),
            Value::String(s) => write!(f, "String({s:?})"),
            Value::List(v) => write!(f, "List({v:?})"),
            Value::Map(m) => write!(f, "Map({m:?})"),
            Value::Function(_) => write!(f, "Function(...)"),
            #[cfg(feature = "html")]
            Value::Safe { kind, s } => write!(f, "Safe({kind:?}, {s:?})"),
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
            Value::Uint(n) => *n != 0,
            Value::Float(f) => *f != 0.0,
            Value::String(s) => !s.is_empty(),
            Value::List(v) => !v.is_empty(),
            Value::Map(m) => !m.is_empty(),
            Value::Function(_) => true,
            #[cfg(feature = "html")]
            Value::Safe { s, .. } => !s.is_empty(),
        }
    }

    /// Look up a field by name on a [`Value::Map`].
    ///
    /// Returns `Some(&value)` when the key exists (the value may itself be
    /// [`Value::Nil`]), and `None` when the key is absent or the receiver is
    /// not a map. This lets callers distinguish "key set to nil" from
    /// "key missing", which matters for the `missingkey=error` option.
    ///
    /// In Go, the equivalent lookup uses reflection on struct fields or map
    /// keys.
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

    /// Index into a [`Value::List`] (by integer), [`Value::Map`] (by string),
    /// or [`Value::String`] (by integer, returning the raw byte).
    ///
    /// Mirrors Go's `index` builtin semantics:
    /// - **List + Int**: returns the element, or an error if out of bounds.
    /// - **Map + String**: returns the value, or [`Value::Nil`] for missing keys.
    /// - **String + Int**: returns the byte at that offset as a [`Value::Int`]
    ///   (Go indexes strings as `[]byte`; mid-codepoint offsets are valid).
    /// - **Nil + anything**: returns an error (`index of untyped nil`).
    /// - **Other combinations**: returns an error (type mismatch).
    ///
    /// # Errors
    ///
    /// Returns an error on out-of-bounds list access, indexing with an
    /// incompatible key type, or indexing a non-indexable value.
    pub fn index(&self, idx: &Value) -> Result<Value> {
        // Resolve an integer index (signed or unsigned) to a usize offset,
        // bounds-checking against `len`. A `Value::Uint` above `i64::MAX`
        // wraps to a negative `i64` here and fails the lower-bound check —
        // correct, since it can never be a valid offset.
        fn check_bounds(idx: &Value, len: usize) -> Result<usize> {
            let i = match idx {
                Value::Int(i) => *i,
                Value::Uint(i) => *i as i64,
                #[allow(
                    clippy::unreachable,
                    reason = "callers guard with a Value::Int | Value::Uint pattern"
                )]
                _ => unreachable!(),
            };
            if i < 0 || (i as usize) >= len {
                Err(crate::error::TemplateError::IndexOutOfRange { index: i })
            } else {
                Ok(i as usize)
            }
        }
        fn bad_index(kind: &str, idx: &Value) -> crate::error::TemplateError {
            crate::error::TemplateError::Exec(format!(
                "cannot index {} with type {}",
                kind,
                idx.type_name()
            ))
        }
        match (self, idx) {
            (Value::List(v), Value::Int(_) | Value::Uint(_)) => {
                Ok(v[check_bounds(idx, v.len())?].clone())
            }
            (Value::List(_), _) => Err(bad_index("list", idx)),
            (Value::Map(m), Value::String(k)) => {
                Ok(m.get(k.as_ref()).cloned().unwrap_or(Value::Nil))
            }
            (Value::Map(_), _) => Err(bad_index("map", idx)),
            // Go indexes strings as `[]byte` — mid-codepoint offsets are
            // valid since Go has no UTF-8 invariant. We surface the byte as
            // a `Value::Int` to keep that semantic without breaking ours.
            (Value::String(s), Value::Int(_) | Value::Uint(_)) => {
                let bytes = s.as_bytes();
                Ok(Value::Int(bytes[check_bounds(idx, bytes.len())?] as i64))
            }
            (Value::String(_), _) => Err(bad_index("string", idx)),
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
            Value::Uint(_) => "uint",
            Value::Float(_) => "float64",
            Value::String(_) => "string",
            Value::List(_) => "list",
            Value::Map(_) => "map",
            Value::Function(_) => "func",
            // Safe content compares and formats as its underlying string, so it
            // reports "string" (matching Go's basicKind for template.HTML etc.).
            #[cfg(feature = "html")]
            Value::Safe { .. } => "string",
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

    /// Extracts an `i64`: returned directly for a [`Value::Int`], reinterpreted
    /// from a [`Value::Uint`] (`u64 as i64`, which wraps to a negative value
    /// above `i64::MAX`), or truncated from a [`Value::Float`].
    ///
    /// The `Uint` path is lossy as a *signed value* above `i64::MAX` — only the
    /// bit pattern survives (`n as i64 as u64 == n`), not the magnitude. Anything
    /// that must see a uint's true magnitude matches [`Value::Uint`] directly
    /// instead: the `%d` / `%x` / `%X` / `%o` / `%b` / `%U` printf verbs and the
    /// index/slice paths all do. The sole remaining uint consumer here is `%c`,
    /// where a value outside the rune range is invalid regardless of sign, so the
    /// wrap is harmless (it maps to U+FFFD either way).
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(n) => Some(*n),
            Value::Uint(n) => Some(*n as i64),
            Value::Float(f) => Some(*f as i64),
            _ => None,
        }
    }

    /// Extracts an `f64` if this is a [`Value::Float`], or widens a
    /// [`Value::Int`] / [`Value::Uint`].
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            Value::Int(n) => Some(*n as f64),
            Value::Uint(n) => Some(*n as f64),
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
            Value::Uint(n) => write!(f, "{n}"),
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
            // Trusted content renders verbatim, exactly like its string.
            #[cfg(feature = "html")]
            Value::Safe { s, .. } => write!(f, "{s}"),
        }
    }
}

/// Rust-side equality for [`Value`].
///
/// Type-strict: values must have the same variant to compare equal
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
            (Value::Uint(a), Value::Uint(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::String(a), Value::String(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Map(a), Value::Map(b)) => a == b,
            #[cfg(feature = "html")]
            (Value::Safe { kind: k1, s: a }, Value::Safe { kind: k2, s: b }) => k1 == k2 && a == b,
            _ => false,
        }
    }
}

/// Rust-side partial ordering for [`Value`].
///
/// Supports ordering for same-type numeric and string variants only:
/// [`Value::Int`], [`Value::Float`], [`Value::String`]. Every other
/// combination returns `None`.
///
/// Template builtins (`lt`, `le`, `gt`, `ge`) implement Go-compatible
/// comparison error semantics separately in `funcs.rs`.
impl PartialOrd for Value {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a.partial_cmp(b),
            (Value::Uint(a), Value::Uint(b)) => a.partial_cmp(b),
            (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
            (Value::String(a), Value::String(b)) => a.partial_cmp(b),
            #[cfg(feature = "html")]
            (Value::Safe { s: a, .. }, Value::Safe { s: b, .. }) => a.partial_cmp(b),
            _ => None,
        }
    }
}

/// Trait for converting Rust types into template [`Value`]s.
///
/// Standing in for Go's ability to pass any type to `template.Execute()`.
/// Blanket impls cover common types; implement it on your own types to pass
/// them as template data.
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

macro_rules! impl_to_value_uint {
    ($($t:ty),*) => {
        $(impl ToValue for $t {
            fn to_value(&self) -> Value {
                Value::Uint(*self as u64)
            }
        })*
    };
}

// Signed integers map to `Value::Int`; unsigned integers map to `Value::Uint`,
// mirroring Go's distinct signed/unsigned reflect kinds. Keeping the unsigned
// family in `Value::Uint` is what lets `u64::MAX` render and compare as its
// true value instead of wrapping to `Value::Int(-1)`.
impl_to_value_int!(i8, i16, i32, i64, isize);
impl_to_value_uint!(u8, u16, u32, u64, usize);

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

fn list_from_iter<'a, T: ToValue + 'a, I: IntoIterator<Item = &'a T>>(iter: I) -> Value {
    Value::List(
        iter.into_iter()
            .map(ToValue::to_value)
            .collect::<Vec<_>>()
            .into(),
    )
}

fn map_from_iter_str<'a, T: ToValue + 'a, I: IntoIterator<Item = (&'a str, &'a T)>>(
    iter: I,
) -> Value {
    Value::Map(Arc::new(
        iter.into_iter()
            .map(|(k, v)| (Arc::from(k), v.to_value()))
            .collect(),
    ))
}

impl<T: ToValue> ToValue for [T] {
    fn to_value(&self) -> Value {
        list_from_iter(self.iter())
    }
}

impl<T: ToValue, const N: usize> ToValue for [T; N] {
    fn to_value(&self) -> Value {
        list_from_iter(self.iter())
    }
}

impl<T: ToValue> ToValue for Vec<T> {
    fn to_value(&self) -> Value {
        list_from_iter(self.iter())
    }
}

impl<T: ToValue> ToValue for alloc::collections::VecDeque<T> {
    fn to_value(&self) -> Value {
        list_from_iter(self.iter())
    }
}

impl<T: ToValue> ToValue for alloc::collections::LinkedList<T> {
    fn to_value(&self) -> Value {
        list_from_iter(self.iter())
    }
}

impl<T: ToValue> ToValue for alloc::collections::BTreeSet<T> {
    fn to_value(&self) -> Value {
        list_from_iter(self.iter())
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

impl<T: ToValue> ToValue for BTreeMap<String, T> {
    fn to_value(&self) -> Value {
        map_from_iter_str(self.iter().map(|(k, v)| (k.as_str(), v)))
    }
}

impl<T: ToValue> ToValue for BTreeMap<&str, T> {
    fn to_value(&self) -> Value {
        map_from_iter_str(self.iter().map(|(k, v)| (*k, v)))
    }
}

#[cfg(feature = "std")]
impl<T: ToValue> ToValue for std::collections::HashMap<String, T> {
    fn to_value(&self) -> Value {
        map_from_iter_str(self.iter().map(|(k, v)| (k.as_str(), v)))
    }
}

#[cfg(feature = "std")]
impl<T: ToValue> ToValue for std::collections::HashMap<&str, T> {
    fn to_value(&self) -> Value {
        map_from_iter_str(self.iter().map(|(k, v)| (*k, v)))
    }
}

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
/// Useful when data is already in a `HashMap` and should be passed to a
/// template without first converting to `BTreeMap`.
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
