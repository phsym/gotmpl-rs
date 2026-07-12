//! serde integration: convert any [`serde::Serialize`] type into a [`Value`].
//!
//! Available only with the `serde` feature. This is the *eager* path: like
//! [`ToValue`](crate::ToValue), serializing walks the whole value and
//! materializes a full [`Value`] tree. The point of it, over a hand-written
//! `ToValue` impl, is that there's no boilerplate: any `#[derive(Serialize)]`
//! type becomes template data.
//!
//! Two entry points, both fallible (serde serialization can fail on
//! unrepresentable map keys or out-of-range 128-bit integers):
//! - the free function [`to_value`], mirroring `serde_json::to_value`, and
//! - the [`ToSerdeValue`] extension trait, giving a `.to_serde_value()` method
//!   on every `Serialize` type.
//!
//! # Field-name casing
//!
//! Go templates access exported PascalCase fields (`{{.Name}}`), but serde
//! derives field names verbatim from the Rust identifiers (`name`). Add
//! `#[serde(rename_all = "PascalCase")]` (or a per-field `#[serde(rename = "...")]`)
//! so the map keys match what the template dots into.
//!
//! # Map keys
//!
//! [`Value::Map`] is string-keyed, so a non-string map key (an integer, `bool`,
//! or `char`) is stringified via its `Display` impl, so `1u32` becomes `"1"`.
//! Because the resulting keys are ordinary strings, `{{range}}` iterates them in
//! **lexical** order, not the key's natural order. This differs from Go, which
//! sorts numeric map keys numerically: a `map[int]V` with keys `1, 2, 10` ranges
//! as `1, 2, 10` in Go but as `1, 10, 2` here. Compound keys (structs, tuples,
//! sequences) cannot be stringified and produce an error.
//!
//! # Examples
//!
//! ```
//! use gotmpl::{to_value, Value};
//! use serde::Serialize;
//!
//! #[derive(Serialize)]
//! #[serde(rename_all = "PascalCase")]
//! struct User { name: String, age: u8 }
//!
//! let v = to_value(&User { name: "Alice".into(), age: 30 }).unwrap();
//! assert_eq!(v.field("Name"), Some(&Value::String("Alice".into())));
//! assert_eq!(v.field("Age"), Some(&Value::Uint(30)));
//! ```

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::ToString;
use alloc::sync::Arc;
use alloc::vec::Vec;

use serde::Serialize;
use serde::ser::{self, Impossible};

use crate::error::{Result, TemplateError};
use crate::value::Value;

/// Convert a [`serde::Serialize`] value into a template [`Value`].
///
/// The eager counterpart to [`ToValue`](crate::ToValue) for serde types.
/// Mirrors `serde_json::to_value`, but borrows its argument (`&T`) rather than
/// consuming it, so the source value stays usable afterwards. See the
/// [module docs](self) for the serde-to-`Value` mapping and the field-name
/// casing note.
///
/// # Errors
///
/// Returns [`TemplateError::Exec`] if the value cannot be represented: a map
/// key that does not serialize to a string, or an `i128`/`u128` outside the
/// `i64`/`u64` range.
pub fn to_value<T: ?Sized + Serialize>(value: &T) -> Result<Value> {
    value.serialize(ValueSerializer)
}

/// Extension trait adding [`to_serde_value`](ToSerdeValue::to_serde_value) to
/// every [`serde::Serialize`] type.
///
/// A thin method-style wrapper over [`to_value`]. Named `to_serde_value` rather
/// than `to_value` so it never collides with [`ToValue::to_value`](crate::ToValue::to_value)
/// on types (like `String` or `i32`) that implement both traits.
///
/// # Examples
///
/// ```
/// use gotmpl::{ToSerdeValue, Value};
///
/// let v = 42u32.to_serde_value().unwrap();
/// assert_eq!(v, Value::Uint(42));
/// ```
pub trait ToSerdeValue {
    /// Convert `self` into a template [`Value`] via its [`serde::Serialize`] impl.
    ///
    /// # Errors
    ///
    /// See [`to_value`].
    fn to_serde_value(&self) -> Result<Value>;
}

impl<T: Serialize + ?Sized> ToSerdeValue for T {
    fn to_serde_value(&self) -> Result<Value> {
        to_value(self)
    }
}

/// A [`serde::Serializer`] that produces a [`Value`]. Modelled on
/// `serde_json::value::Serializer`.
struct ValueSerializer;

macro_rules! serialize_int {
    ($($method:ident($ty:ty) => $variant:ident;)*) => {
        $(fn $method(self, v: $ty) -> Result<Value> {
            Ok(Value::$variant(v as _))
        })*
    };
}

impl ser::Serializer for ValueSerializer {
    type Ok = Value;
    type Error = TemplateError;

    type SerializeSeq = SerializeVec;
    type SerializeTuple = SerializeVec;
    type SerializeTupleStruct = SerializeVec;
    type SerializeTupleVariant = SerializeTupleVariant;
    type SerializeMap = SerializeMap;
    type SerializeStruct = SerializeMap;
    type SerializeStructVariant = SerializeStructVariant;

    serialize_int! {
        serialize_i8(i8) => Int;
        serialize_i16(i16) => Int;
        serialize_i32(i32) => Int;
        serialize_i64(i64) => Int;
        serialize_u8(u8) => Uint;
        serialize_u16(u16) => Uint;
        serialize_u32(u32) => Uint;
        serialize_u64(u64) => Uint;
    }

    fn serialize_bool(self, v: bool) -> Result<Value> {
        Ok(Value::Bool(v))
    }

    fn serialize_i128(self, v: i128) -> Result<Value> {
        i64::try_from(v).map(Value::Int).map_err(|_| {
            TemplateError::Exec(format!("i128 value {v} out of range for a template int"))
        })
    }

    fn serialize_u128(self, v: u128) -> Result<Value> {
        u64::try_from(v).map(Value::Uint).map_err(|_| {
            TemplateError::Exec(format!("u128 value {v} out of range for a template uint"))
        })
    }

    fn serialize_f32(self, v: f32) -> Result<Value> {
        Ok(Value::Float(v as f64))
    }

    fn serialize_f64(self, v: f64) -> Result<Value> {
        Ok(Value::Float(v))
    }

    fn serialize_char(self, v: char) -> Result<Value> {
        let mut buf = [0u8; 4];
        Ok(Value::String(Arc::from(&*v.encode_utf8(&mut buf))))
    }

    fn serialize_str(self, v: &str) -> Result<Value> {
        Ok(v.into())
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<Value> {
        // Matches serde_json: a byte slice becomes a list of numbers.
        Ok(v.iter()
            .map(|b| Value::Uint(u64::from(*b)))
            .collect::<Vec<_>>()
            .into())
    }

    fn serialize_none(self) -> Result<Value> {
        Ok(Value::Nil)
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<Value> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Value> {
        Ok(Value::Nil)
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Value> {
        Ok(Value::Nil)
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<Value> {
        Ok(Value::String(Arc::from(variant)))
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Value> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Value> {
        Ok(singleton_map(Arc::from(variant), to_value(value)?))
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<SerializeVec> {
        Ok(SerializeVec {
            vec: Vec::with_capacity(len.unwrap_or(0)),
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<SerializeVec> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(self, _name: &'static str, len: usize) -> Result<SerializeVec> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<SerializeTupleVariant> {
        Ok(SerializeTupleVariant {
            variant: Arc::from(variant),
            vec: Vec::with_capacity(len),
        })
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<SerializeMap> {
        Ok(SerializeMap {
            map: BTreeMap::new(),
            next_key: None,
        })
    }

    fn serialize_struct(self, _name: &'static str, _len: usize) -> Result<SerializeMap> {
        Ok(SerializeMap {
            map: BTreeMap::new(),
            next_key: None,
        })
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<SerializeStructVariant> {
        Ok(SerializeStructVariant {
            variant: Arc::from(variant),
            map: BTreeMap::new(),
        })
    }
}

/// Build a single-entry `Value::Map`, used for serde's externally-tagged
/// enum representations (newtype/tuple/struct variants).
fn singleton_map(key: Arc<str>, value: Value) -> Value {
    let mut map = BTreeMap::new();
    map.insert(key, value);
    map.into()
}

/// Accumulator for sequences and tuples → [`Value::List`].
struct SerializeVec {
    vec: Vec<Value>,
}

impl ser::SerializeSeq for SerializeVec {
    type Ok = Value;
    type Error = TemplateError;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.vec.push(to_value(value)?);
        Ok(())
    }

    fn end(self) -> Result<Value> {
        Ok(self.vec.into())
    }
}

impl ser::SerializeTuple for SerializeVec {
    type Ok = Value;
    type Error = TemplateError;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Value> {
        ser::SerializeSeq::end(self)
    }
}

impl ser::SerializeTupleStruct for SerializeVec {
    type Ok = Value;
    type Error = TemplateError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        ser::SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> Result<Value> {
        ser::SerializeSeq::end(self)
    }
}

/// Accumulator for a tuple enum variant → externally-tagged
/// `{ variant: [elements…] }`.
struct SerializeTupleVariant {
    variant: Arc<str>,
    vec: Vec<Value>,
}

impl ser::SerializeTupleVariant for SerializeTupleVariant {
    type Ok = Value;
    type Error = TemplateError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        self.vec.push(to_value(value)?);
        Ok(())
    }

    fn end(self) -> Result<Value> {
        Ok(singleton_map(self.variant, self.vec.into()))
    }
}

/// Accumulator for maps and structs → [`Value::Map`].
struct SerializeMap {
    map: BTreeMap<Arc<str>, Value>,
    next_key: Option<Arc<str>>,
}

impl ser::SerializeMap for SerializeMap {
    type Ok = Value;
    type Error = TemplateError;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, key: &T) -> Result<()> {
        self.next_key = Some(key.serialize(MapKeySerializer)?);
        Ok(())
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<()> {
        let key = self.next_key.take().ok_or_else(|| {
            <TemplateError as ser::Error>::custom("serialize_value called before serialize_key")
        })?;
        self.map.insert(key, to_value(value)?);
        Ok(())
    }

    fn end(self) -> Result<Value> {
        Ok(self.map.into())
    }
}

impl ser::SerializeStruct for SerializeMap {
    type Ok = Value;
    type Error = TemplateError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        self.map.insert(Arc::from(key), to_value(value)?);
        Ok(())
    }

    fn end(self) -> Result<Value> {
        Ok(self.map.into())
    }
}

/// Accumulator for a struct enum variant → externally-tagged
/// `{ variant: { fields… } }`.
struct SerializeStructVariant {
    variant: Arc<str>,
    map: BTreeMap<Arc<str>, Value>,
}

impl ser::SerializeStructVariant for SerializeStructVariant {
    type Ok = Value;
    type Error = TemplateError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<()> {
        self.map.insert(Arc::from(key), to_value(value)?);
        Ok(())
    }

    fn end(self) -> Result<Value> {
        Ok(singleton_map(self.variant, self.map.into()))
    }
}

/// Serializes a map key into an `Arc<str>`. Accepts strings and stringifies
/// primitive keys (bool, char, integers); rejects everything else. This is the
/// same policy as `serde_json`, since [`Value::Map`] is string-keyed.
struct MapKeySerializer;

fn key_must_be_string() -> TemplateError {
    TemplateError::Exec("map key must serialize to a string or primitive".into())
}

macro_rules! serialize_key_via_display {
    ($($method:ident($ty:ty);)*) => {
        $(fn $method(self, v: $ty) -> Result<Arc<str>> {
            Ok(Arc::from(v.to_string().as_str()))
        })*
    };
}

/// Every serde `serialize_*` form that can't produce a string key rejects with
/// the same error; list them declaratively instead of writing each body out.
macro_rules! reject_key {
    ($($method:ident($($param:ident: $ty:ty),*) -> $ret:ty;)*) => {
        $(fn $method(self, $($param: $ty),*) -> Result<$ret> {
            Err(key_must_be_string())
        })*
    };
}

impl ser::Serializer for MapKeySerializer {
    type Ok = Arc<str>;
    type Error = TemplateError;

    type SerializeSeq = Impossible<Arc<str>, TemplateError>;
    type SerializeTuple = Impossible<Arc<str>, TemplateError>;
    type SerializeTupleStruct = Impossible<Arc<str>, TemplateError>;
    type SerializeTupleVariant = Impossible<Arc<str>, TemplateError>;
    type SerializeMap = Impossible<Arc<str>, TemplateError>;
    type SerializeStruct = Impossible<Arc<str>, TemplateError>;
    type SerializeStructVariant = Impossible<Arc<str>, TemplateError>;

    serialize_key_via_display! {
        serialize_bool(bool);
        serialize_i8(i8);
        serialize_i16(i16);
        serialize_i32(i32);
        serialize_i64(i64);
        serialize_i128(i128);
        serialize_u8(u8);
        serialize_u16(u16);
        serialize_u32(u32);
        serialize_u64(u64);
        serialize_u128(u128);
        serialize_char(char);
    }

    fn serialize_str(self, v: &str) -> Result<Arc<str>> {
        Ok(Arc::from(v))
    }

    // A newtype struct is transparent, so serialize the inner key.
    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Arc<str>> {
        value.serialize(self)
    }

    // A unit variant serializes as its name, which is a valid string key.
    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<Arc<str>> {
        Ok(Arc::from(variant))
    }

    // The two forms carrying a `?Sized + Serialize` payload can't go through
    // the macro (it doesn't take type parameters); the rest do.
    fn serialize_some<T: ?Sized + Serialize>(self, _value: &T) -> Result<Arc<str>> {
        Err(key_must_be_string())
    }
    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<Arc<str>> {
        Err(key_must_be_string())
    }

    reject_key! {
        serialize_f32(_v: f32) -> Arc<str>;
        serialize_f64(_v: f64) -> Arc<str>;
        serialize_bytes(_v: &[u8]) -> Arc<str>;
        serialize_none() -> Arc<str>;
        serialize_unit() -> Arc<str>;
        serialize_unit_struct(_name: &'static str) -> Arc<str>;
        serialize_seq(_len: Option<usize>) -> Self::SerializeSeq;
        serialize_tuple(_len: usize) -> Self::SerializeTuple;
        serialize_tuple_struct(_name: &'static str, _len: usize) -> Self::SerializeTupleStruct;
        serialize_tuple_variant(_name: &'static str, _index: u32, _variant: &'static str, _len: usize) -> Self::SerializeTupleVariant;
        serialize_map(_len: Option<usize>) -> Self::SerializeMap;
        serialize_struct(_name: &'static str, _len: usize) -> Self::SerializeStruct;
        serialize_struct_variant(_name: &'static str, _index: u32, _variant: &'static str, _len: usize) -> Self::SerializeStructVariant;
    }
}

#[cfg(test)]
mod tests {
    use alloc::collections::BTreeMap;
    use alloc::string::{String, ToString};
    use alloc::vec;
    use alloc::vec::Vec;

    use serde::Serialize;

    use super::*;
    use crate::Template;

    /// Build a `Value::Map` from `(key, value)` pairs for terse assertions.
    fn map<const N: usize>(entries: [(&str, Value); N]) -> Value {
        Value::Map(Arc::new(
            entries
                .into_iter()
                .map(|(k, v)| (Arc::from(k), v))
                .collect(),
        ))
    }

    #[test]
    fn scalars() {
        assert_eq!(to_value(&true).unwrap(), Value::Bool(true));
        assert_eq!(to_value(&-7i32).unwrap(), Value::Int(-7));
        assert_eq!(to_value(&7u8).unwrap(), Value::Uint(7));
        assert_eq!(to_value(&1.5f64).unwrap(), Value::Float(1.5));
        assert_eq!(to_value(&'x').unwrap(), Value::String("x".into()));
        assert_eq!(to_value("hi").unwrap(), Value::String("hi".into()));
        assert_eq!(to_value(&Option::<i32>::None).unwrap(), Value::Nil);
        assert_eq!(to_value(&Some(3i32)).unwrap(), Value::Int(3));
        assert_eq!(to_value(&()).unwrap(), Value::Nil);
    }

    #[test]
    fn nested_struct_with_vec_and_option() {
        #[derive(Serialize)]
        #[serde(rename_all = "PascalCase")]
        struct Address {
            city: String,
        }
        #[derive(Serialize)]
        #[serde(rename_all = "PascalCase")]
        struct User {
            name: String,
            age: u8,
            tags: Vec<String>,
            address: Address,
            nickname: Option<String>,
        }

        let v = to_value(&User {
            name: "Alice".into(),
            age: 30,
            tags: vec!["a".into(), "b".into()],
            address: Address {
                city: "Paris".into(),
            },
            nickname: None,
        })
        .unwrap();

        assert_eq!(
            v,
            map([
                ("Name", Value::String("Alice".into())),
                ("Age", Value::Uint(30)),
                (
                    "Tags",
                    Value::List(vec![Value::String("a".into()), Value::String("b".into())].into())
                ),
                ("Address", map([("City", Value::String("Paris".into()))])),
                ("Nickname", Value::Nil),
            ])
        );
    }

    #[test]
    fn enum_representations() {
        #[derive(Serialize)]
        enum E {
            Unit,
            New(i32),
            Tuple(i32, i32),
            Struct { a: i32 },
        }

        assert_eq!(to_value(&E::Unit).unwrap(), Value::String("Unit".into()));
        assert_eq!(to_value(&E::New(5)).unwrap(), map([("New", Value::Int(5))]));
        assert_eq!(
            to_value(&E::Tuple(1, 2)).unwrap(),
            map([(
                "Tuple",
                Value::List(vec![Value::Int(1), Value::Int(2)].into())
            )])
        );
        assert_eq!(
            to_value(&E::Struct { a: 3 }).unwrap(),
            map([("Struct", map([("a", Value::Int(3))]))])
        );
    }

    #[test]
    fn map_keys_are_stringified() {
        let mut m: BTreeMap<u32, &str> = BTreeMap::new();
        m.insert(1, "one");
        m.insert(2, "two");
        assert_eq!(
            to_value(&m).unwrap(),
            map([
                ("1", Value::String("one".into())),
                ("2", Value::String("two".into())),
            ])
        );
    }

    #[test]
    fn bytes_become_list_of_uint() {
        // serde_bytes not pulled in; drive serialize_bytes via a manual impl.
        struct Bytes;
        impl Serialize for Bytes {
            fn serialize<S: serde::Serializer>(
                &self,
                s: S,
            ) -> core::result::Result<S::Ok, S::Error> {
                s.serialize_bytes(&[1u8, 2, 255])
            }
        }
        assert_eq!(
            to_value(&Bytes).unwrap(),
            Value::List(vec![Value::Uint(1), Value::Uint(2), Value::Uint(255)].into())
        );
    }

    #[test]
    fn out_of_range_128bit_errors() {
        assert!(to_value(&i128::MAX).is_err());
        assert!(to_value(&u128::MAX).is_err());
        // In-range 128-bit values still convert.
        assert_eq!(to_value(&5i128).unwrap(), Value::Int(5));
        assert_eq!(to_value(&5u128).unwrap(), Value::Uint(5));
    }

    #[test]
    fn non_string_map_key_errors() {
        // A struct key cannot be stringified.
        #[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
        struct Key {
            a: i32,
        }
        let mut m: BTreeMap<Key, i32> = BTreeMap::new();
        m.insert(Key { a: 1 }, 2);
        assert!(to_value(&m).is_err());
    }

    #[test]
    fn both_surfaces_render_identically() {
        #[derive(Serialize)]
        #[serde(rename_all = "PascalCase")]
        struct User {
            name: String,
            age: u8,
        }
        let user = User {
            name: "Bob".to_string(),
            age: 42,
        };
        let tmpl = Template::new("")
            .parse("Hello {{.Name}}, age {{.Age}}")
            .unwrap();

        let via_fn = tmpl.execute_to_string(&to_value(&user).unwrap()).unwrap();
        let via_method = tmpl
            .execute_to_string(&user.to_serde_value().unwrap())
            .unwrap();

        assert_eq!(via_fn, "Hello Bob, age 42");
        assert_eq!(via_fn, via_method);
    }
}
