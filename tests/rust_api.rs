// tests/rust_api.rs
//
// Tests for Rust-specific APIs and implementation details that don't correspond
// to specific Go text/template test cases.
//
// These were originally in go_compat.rs and are separated here to keep
// compatibility tests easier to audit against upstream Go behavior.

#![allow(
    clippy::err_expect,
    clippy::ok_expect,
    reason = "Template lacks Debug (closure-typed FuncMap can't render), so .err().expect(..) / .ok().expect(..) is the cleanest shape for failure-expectation tests"
)]

extern crate alloc;
use alloc::sync::Arc;

use gotmpl::Value;
use gotmpl::{MissingKey, Template, tmap};

// RAII helper for filesystem-backed tests. Builds a per-process, per-label
// directory under `std::env::temp_dir()` so concurrent runs of the same test
// binary don't race on the same path, and removes it on drop so successful
// runs leave nothing behind. Cleanup also fires on panic.
//
// Only the `std`/`glob` filesystem tests use this; gate it so non-`std` test
// builds don't see an unused-item warning (or, with its users gated away, an
// orphan).
#[cfg(feature = "std")]
struct TempDir(std::path::PathBuf);
#[cfg(feature = "std")]
impl TempDir {
    fn new(label: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("gotmpl_test_{label}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}
#[cfg(feature = "std")]
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// Value::Function variant and call builtin
#[test]
fn test_call_function_value() {
    let adder: gotmpl::ValueFunc = Arc::new(|args: &[Value]| {
        let sum: i64 = args.iter().filter_map(|a| a.as_int()).sum();
        Ok(Value::Int(sum))
    });
    let data = tmap! {};
    let result = Template::new("test")
        .func("getAdder", move |_args| Ok(Value::Function(adder.clone())))
        .parse(r#"{{call (getAdder) 3 4}}"#)
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(result, "7");
}

#[test]
fn test_function_value_truthy() {
    let f: gotmpl::ValueFunc = Arc::new(|_| Ok(Value::Int(42)));
    let data = Value::Function(f);
    assert!(data.is_truthy());
}

// missingkey option API
#[test]
fn test_missingkey_error_integration() {
    let data = tmap! { "X" => 1i64 };
    let result = Template::new("test")
        .missing_key(MissingKey::Error)
        .parse("{{.Y}}")
        .unwrap()
        .execute_to_string(&data);
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("no entry for key"));
}

#[test]
fn test_missingkey_zero() {
    let data = tmap! { "X" => 1i64 };
    let result = Template::new("test")
        .missing_key(MissingKey::ZeroValue)
        .parse("{{.Y}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(result, "<no value>");
}

#[test]
fn test_missingkey_from_str() {
    assert_eq!("error".parse::<MissingKey>().unwrap(), MissingKey::Error);
    assert_eq!(
        "invalid".parse::<MissingKey>().unwrap(),
        MissingKey::Invalid
    );
    assert_eq!(
        "default".parse::<MissingKey>().unwrap(),
        MissingKey::Invalid
    );
    assert_eq!("zero".parse::<MissingKey>().unwrap(), MissingKey::ZeroValue);
    assert!("garbage".parse::<MissingKey>().is_err());
}

#[test]
fn test_missingkey_display_roundtrip() {
    for mk in [
        MissingKey::Invalid,
        MissingKey::ZeroValue,
        MissingKey::Error,
    ] {
        let s = mk.to_string();
        assert_eq!(s.parse::<MissingKey>().unwrap(), mk);
    }
}

// Max execution depth (Rust safety guard)
#[test]
fn test_max_exec_depth() {
    // Recursive template should error, not stack overflow
    let result = Template::new("test")
        .parse(r#"{{define "recurse"}}{{template "recurse" .}}{{end}}{{template "recurse" .}}"#)
        .unwrap()
        .execute_to_string(&Value::Nil);
    let err = result.expect_err("expected recursion limit error");
    assert!(
        matches!(err, gotmpl::TemplateError::RecursionLimit),
        "expected RecursionLimit, got {:?}",
        err
    );
}

// Successive parse calls (Go's Parse method semantics)
#[test]
fn test_parse_defines_merge() {
    let tmpl = Template::new("root")
        .parse(r#"{{template "header" .}}body{{template "footer" .}}"#)
        .unwrap()
        .parse(r#"{{define "header"}}<h1>{{.Title}}</h1>{{end}}"#)
        .unwrap()
        .parse(r#"{{define "footer"}}<footer>bye</footer>{{end}}"#)
        .unwrap();

    let data = tmap! { "Title" => "Hello" };
    let result = tmpl.execute_to_string(&data).unwrap();
    assert_eq!(result, "<h1>Hello</h1>body<footer>bye</footer>");
}

#[test]
fn test_parse_define_override() {
    // Later parse overrides earlier define
    let tmpl = Template::new("root")
        .parse(r#"{{define "x"}}first{{end}}{{template "x"}}"#)
        .unwrap()
        .parse(r#"{{define "x"}}second{{end}}"#)
        .unwrap();

    assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "second");
}

#[test]
fn test_parse_syntax_error() {
    let result = Template::new("root")
        .parse("main")
        .unwrap()
        .parse("{{define}}");
    assert!(result.is_err());
}

#[test]
fn test_parse_empty_body_does_not_replace_existing() {
    // Go parity: a body of only whitespace/comments does not overwrite an
    // existing main template body.
    let tmpl = Template::new("root")
        .parse("main body")
        .unwrap()
        .parse(r#"   {{/* just a comment */}}   {{define "x"}}x{{end}}"#)
        .unwrap();
    assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "main body");
}

#[test]
fn test_parse_empty_define_does_not_replace_existing() {
    // Go parity: `associate` refuses to overwrite a non-empty existing
    // define with an empty one.
    let tmpl = Template::new("root")
        .parse(r#"{{template "x"}}"#)
        .unwrap()
        .parse(r#"{{define "x"}}first{{end}}"#)
        .unwrap()
        .parse(r#"{{define "x"}}{{end}}"#)
        .unwrap();
    assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "first");
}

// Clone API
#[test]
fn test_clone_preserves_missingkey_error() {
    let original = Template::new("t")
        .missing_key(MissingKey::Error)
        .parse("{{.X}}")
        .unwrap();

    let cloned = original.clone();

    // Both should error on missing key
    let data = tmap! { "Y" => 1i64 };
    assert!(original.execute_to_string(&data).is_err());
    assert!(cloned.execute_to_string(&data).is_err());
}

#[test]
fn test_clone_preserves_custom_delims() {
    let original = Template::new("t")
        .delims("<<", ">>")
        .parse("<<.X>>")
        .unwrap();

    // Clone should inherit the parsed tree (delims were applied at parse time)
    let data = tmap! { "X" => "hello" };
    let cloned = original.clone();
    assert_eq!(cloned.execute_to_string(&data).unwrap(), "hello");
}

#[test]
fn test_clone_unparsed_template() {
    let t = Template::new("empty");
    let cloned = t.clone();
    // Should not panic; executing should error gracefully
    assert!(cloned.execute_to_string(&Value::Nil).is_err());
}

// ToValue implementations
#[test]
fn test_to_value_integers() {
    use gotmpl::ToValue;

    // Signed integers → Value::Int.
    assert_eq!(42i8.to_value(), Value::Int(42));
    assert_eq!(42i16.to_value(), Value::Int(42));
    assert_eq!(42i32.to_value(), Value::Int(42));
    assert_eq!(42i64.to_value(), Value::Int(42));
    assert_eq!(42isize.to_value(), Value::Int(42));
    // Unsigned integers → Value::Uint (mirrors Go's unsigned reflect kinds).
    assert_eq!(42u8.to_value(), Value::Uint(42));
    assert_eq!(42u16.to_value(), Value::Uint(42));
    assert_eq!(42u32.to_value(), Value::Uint(42));
    assert_eq!(42u64.to_value(), Value::Uint(42));
    assert_eq!(42usize.to_value(), Value::Uint(42));
}

#[test]
fn test_to_value_floats() {
    use gotmpl::ToValue;

    assert_eq!(1.5f64.to_value(), Value::Float(1.5));
    // f32 → f64 conversion
    let f: f32 = 2.5;
    if let Value::Float(v) = f.to_value() {
        assert!((v - 2.5).abs() < 1e-6);
    } else {
        panic!("expected Float");
    }
}

#[test]
fn test_to_value_cow_str() {
    use alloc::borrow::Cow;
    use gotmpl::ToValue;

    let borrowed: Cow<'_, str> = Cow::Borrowed("hello");
    assert_eq!(borrowed.to_value(), Value::String("hello".into()));

    let owned: Cow<'_, str> = Cow::Owned("world".into());
    assert_eq!(owned.to_value(), Value::String("world".into()));
}

#[test]
fn test_to_value_slice_and_array() {
    use gotmpl::ToValue;

    let arr = [1i64, 2, 3];
    assert_eq!(
        arr.to_value(),
        Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)].into())
    );

    let slice: &[i64] = &[4, 5];
    assert_eq!(
        slice.to_value(),
        Value::List(vec![Value::Int(4), Value::Int(5)].into())
    );
}

#[test]
fn test_to_value_vecdeque() {
    use alloc::collections::VecDeque;
    use gotmpl::ToValue;

    let mut vd = VecDeque::new();
    vd.push_back(1i64);
    vd.push_back(2);
    assert_eq!(
        vd.to_value(),
        Value::List(vec![Value::Int(1), Value::Int(2)].into())
    );
}

#[test]
fn test_to_value_linked_list() {
    use alloc::collections::LinkedList;
    use gotmpl::ToValue;

    let mut ll = LinkedList::new();
    ll.push_back("a");
    ll.push_back("b");
    assert_eq!(
        ll.to_value(),
        Value::List(vec![Value::String("a".into()), Value::String("b".into())].into())
    );
}

#[test]
fn test_to_value_btreeset() {
    use alloc::collections::BTreeSet;
    use gotmpl::ToValue;

    let mut s = BTreeSet::new();
    s.insert(3i64);
    s.insert(1);
    s.insert(2);
    // BTreeSet iterates in sorted order
    assert_eq!(
        s.to_value(),
        Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)].into())
    );
}

#[test]
fn test_to_value_btreemap_str_keys() {
    use alloc::collections::BTreeMap;
    use gotmpl::ToValue;

    let mut m = BTreeMap::new();
    m.insert("x", 1i64);
    m.insert("y", 2i64);
    let val = m.to_value();
    assert_eq!(val, tmap! { "x" => 1i64, "y" => 2i64 });
}

#[cfg(feature = "std")]
#[test]
fn test_to_value_hashmap() {
    use gotmpl::ToValue;
    use std::collections::HashMap;

    let mut m = HashMap::new();
    m.insert("a".to_string(), 1i64);
    let val = m.to_value();
    assert_eq!(val, tmap! { "a" => 1i64 });

    let mut m2 = HashMap::new();
    m2.insert("b", 2i64);
    let val2 = m2.to_value();
    assert_eq!(val2, tmap! { "b" => 2i64 });
}

#[cfg(feature = "std")]
#[test]
fn test_to_value_hashset() {
    use gotmpl::ToValue;
    use std::collections::HashSet;

    let mut s = HashSet::new();
    s.insert(3i64);
    s.insert(1);
    s.insert(2);
    // HashSet ToValue sorts for determinism
    assert_eq!(
        s.to_value(),
        Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)].into())
    );
}

// From impls for Value
#[test]
fn test_from_str_and_string() {
    assert_eq!(Value::from("hi"), Value::String("hi".into()));
    assert_eq!(Value::from("hi".to_string()), Value::String("hi".into()));
}

#[test]
fn test_from_vec_of_values() {
    let v: Vec<Value> = vec![Value::Int(1), Value::Int(2)];
    assert_eq!(
        Value::from(v),
        Value::List(vec![Value::Int(1), Value::Int(2)].into())
    );
}

#[test]
fn test_from_btreemap_string_keys() {
    use alloc::collections::BTreeMap;
    let mut m: BTreeMap<String, Value> = BTreeMap::new();
    m.insert("k".to_string(), Value::Int(1));
    let v: Value = m.into();
    assert!(matches!(v, Value::Map(_)));
    assert_eq!(v.field("k"), Some(&Value::Int(1)));
}

#[test]
fn test_from_arc_payloads_are_zero_copy() {
    let s: Arc<str> = Arc::from("hello");
    let s_ptr = Arc::as_ptr(&s);
    let v = Value::from(Arc::clone(&s));
    if let Value::String(ref inner) = v {
        assert!(core::ptr::eq(Arc::as_ptr(inner), s_ptr));
    } else {
        panic!("expected Value::String");
    }

    let list: Arc<[Value]> = Arc::from(vec![Value::Int(1), Value::Int(2)]);
    let list_ptr = Arc::as_ptr(&list);
    let v = Value::from(Arc::clone(&list));
    if let Value::List(ref inner) = v {
        assert!(core::ptr::eq(Arc::as_ptr(inner), list_ptr));
    } else {
        panic!("expected Value::List");
    }

    use alloc::collections::BTreeMap;
    let mut inner_map: BTreeMap<Arc<str>, Value> = BTreeMap::new();
    inner_map.insert("x".into(), Value::Int(1));
    let m: Arc<BTreeMap<Arc<str>, Value>> = Arc::new(inner_map);
    let m_ptr = Arc::as_ptr(&m);
    let v = Value::from(Arc::clone(&m));
    if let Value::Map(ref inner) = v {
        assert!(core::ptr::eq(Arc::as_ptr(inner), m_ptr));
    } else {
        panic!("expected Value::Map");
    }
}

#[test]
fn test_from_entries_roundtrip_via_tmap() {
    let v = tmap! { "a" => 1i64, "b" => 2i64 };
    assert_eq!(v.field("a"), Some(&Value::Int(1)));
    assert_eq!(v.field("b"), Some(&Value::Int(2)));
}

#[test]
fn test_slice_negative_error_echoes_input_value() {
    // Regression: previously `Option<i64> as usize` cast -1 to usize::MAX,
    // so the error message printed a huge unsigned number. The message must
    // show the caller's actual (negative) index so the error is actionable.
    let list = Value::List(Arc::from(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
    let err = list.slice(Some(-1), None).unwrap_err().to_string();
    assert!(err.contains("-1"), "error should mention -1, got: {err}");
    assert!(
        !err.contains("18446744073709551615") && !err.contains("9223372036854775807"),
        "error leaks overflowed integer: {err}"
    );
}

#[test]
fn test_missingkey_error_on_range() {
    // MissingKey::Error must also fire when a missing key is used as the
    // subject of a {{range}}, not only bare `{{.Missing}}`.
    let data = tmap! { "X" => vec![1i64, 2] };
    let result = Template::new("t")
        .missing_key(MissingKey::Error)
        .parse("{{range .Missing}}x{{end}}")
        .unwrap()
        .execute_to_string(&data);
    assert!(result.is_err());
}

#[test]
fn test_missingkey_zero_in_pipeline() {
    // Under MissingKey::ZeroValue a missing key yields nil (which %v renders
    // as "<nil>"), rather than erroring or producing the default "<no value>"
    // sentinel.
    let data = tmap! { "X" => 1i64 };
    let out = Template::new("t")
        .missing_key(MissingKey::ZeroValue)
        .parse("{{.Y | printf \"%v\"}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(out, "<nil>");
}

// MissingKey end-to-end (Phase 4).
//
// `MissingKey::Invalid` (the default) and `MissingKey::ZeroValue` are
// preserved as separate variants for Go-options parity even though both yield
// `Value::Nil` and render as `<no value>`. These tests pin the rendering at
// every variant + at every level of nested field access.

#[test]
fn test_missingkey_invalid_default_renders_no_value() {
    // Go: with no missingkey option (or "default"/"invalid"), `{{.Missing}}`
    // on a present map renders as `<no value>`. Our default is `Invalid`.
    let data = tmap! { "X" => 1i64 };
    let out = Template::new("t")
        .parse("{{.Missing}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(out, "<no value>");
}

#[test]
fn test_missingkey_invalid_explicit_renders_no_value() {
    let data = tmap! { "X" => 1i64 };
    let out = Template::new("t")
        .missing_key(MissingKey::Invalid)
        .parse("{{.Missing}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(out, "<no value>");
}

#[test]
fn test_missingkey_default_string_parses_to_invalid() {
    // Go's "default" is a synonym for the unset/Invalid behavior.
    let mk: MissingKey = "default".parse().unwrap();
    assert_eq!(mk, MissingKey::Invalid);
}

#[test]
fn test_missingkey_error_top_level() {
    let data = tmap! { "X" => 1i64 };
    let err = Template::new("t")
        .missing_key(MissingKey::Error)
        .parse("{{.Missing}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap_err();
    assert!(
        matches!(err, gotmpl::TemplateError::MissingKey { .. }),
        "expected MissingKey, got {err:?}"
    );
}

#[test]
fn test_missingkey_error_nested_first_level() {
    // {{.X.Y}} where .X is missing under Error mode — error fires at .X.
    let data = tmap! { "Z" => 1i64 };
    let err = Template::new("t")
        .missing_key(MissingKey::Error)
        .parse("{{.X.Y}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("no entry for key"), "got: {msg}");
}

#[test]
fn test_missingkey_zero_nested_inner_missing() {
    // {{.X.Y}} where .X is present but .Y is missing under ZeroValue —
    // `.Y` resolves to Nil and renders as `<no value>`.
    let data = tmap! { "X" => tmap! { "Z" => 1i64 } };
    let out = Template::new("t")
        .missing_key(MissingKey::ZeroValue)
        .parse("{{.X.Y}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(out, "<no value>");
}

#[test]
fn test_missingkey_invalid_three_level_miss() {
    // {{.X.Y.Z}} default mode: .X present, .Y missing → Nil, .Z on Nil → Nil.
    // Renders as `<no value>` rather than erroring.
    let data = tmap! { "X" => tmap! { "Other" => 1i64 } };
    let out = Template::new("t")
        .parse("{{.X.Y.Z}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(out, "<no value>");
}

#[test]
fn test_missingkey_error_three_level_miss_stops_at_first_miss() {
    // Same shape as above but Error mode — the error must fire at .Y, not .Z,
    // and the message must name `Y` (the actually-missing key).
    let data = tmap! { "X" => tmap! { "Other" => 1i64 } };
    let err = Template::new("t")
        .missing_key(MissingKey::Error)
        .parse("{{.X.Y.Z}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap_err();
    match err {
        gotmpl::TemplateError::MissingKey { key } => {
            assert_eq!(
                key, "Y",
                "should error on first missing key, not later ones"
            );
        }
        other => panic!("expected MissingKey, got {other:?}"),
    }
}

#[test]
fn test_block_override_with_nested_pipeline() {
    // Block override should work for bodies containing pipelines/control flow,
    // not just bare text.
    let tmpl = Template::new("t")
        .parse(r#"[{{block "b" .}}{{.X | printf "%d"}}{{end}}]"#)
        .unwrap()
        .parse(r#"{{define "b"}}{{range .Items}}{{.}}/{{end}}{{end}}"#)
        .unwrap();
    let data = tmap! { "Items" => vec!["a", "b", "c"] };
    assert_eq!(tmpl.execute_to_string(&data).unwrap(), "[a/b/c/]");
}

#[test]
fn test_slice_full_range_shares_storage() {
    // Slicing a List over its full range should return the same Arc, not allocate.
    let list_val = Value::List(Arc::from(vec![Value::Int(1), Value::Int(2), Value::Int(3)]));
    let list_inner_ptr = match &list_val {
        Value::List(a) => Arc::as_ptr(a),
        _ => unreachable!(),
    };
    let sliced = list_val.slice(None, None).unwrap();
    match &sliced {
        Value::List(a) => assert!(core::ptr::eq(Arc::as_ptr(a), list_inner_ptr)),
        _ => panic!("expected Value::List"),
    }

    // Same for String.
    let str_val = Value::String(Arc::from("hello"));
    let str_inner_ptr = match &str_val {
        Value::String(a) => Arc::as_ptr(a),
        _ => unreachable!(),
    };
    let sliced = str_val.slice(Some(0), Some(5)).unwrap();
    match &sliced {
        Value::String(a) => assert!(core::ptr::eq(Arc::as_ptr(a), str_inner_ptr)),
        _ => panic!("expected Value::String"),
    }
}

// add_parse_tree API
#[test]
fn test_add_parse_tree_to_unparsed() {
    use gotmpl::parse::{ListNode, Node, Pos, TextNode};

    // Should not panic
    let tmpl = Template::new("t").add_parse_tree(
        "greeting",
        ListNode {
            pos: Pos::new(0, 1),
            nodes: vec![Node::Text(TextNode {
                pos: Pos::new(0, 1),
                text: "hello".into(),
            })],
        },
    );

    // The main template was never parsed, so execute should fail
    assert!(tmpl.execute_to_string(&Value::Nil).is_err());

    // But execute_template should work for the added tree
    assert_eq!(
        tmpl.execute_template_to_string("greeting", &Value::Nil)
            .unwrap(),
        "hello"
    );
}

// UTF-8 escape edge cases
//
// Rust `&str` is always valid UTF-8, so template source can never contain
// *raw* invalid UTF-8 bytes. The escapes below produce codepoints that either
// fall outside the Unicode scalar-value range or are otherwise pathological.
// These tests pin the current behavior of the escape-processing path.

#[test]
fn test_utf8_escape_lone_surrogate_is_dropped() {
    // \uD800 is a lone high surrogate. char::from_u32 returns None, and the
    // lexer silently skips it. Surrounding text is preserved.
    let out = Template::new("t")
        .parse(r#"X{{"\uD800"}}Y"#)
        .unwrap()
        .execute_to_string(&Value::Nil)
        .unwrap();
    assert_eq!(out, "XY");
}

#[test]
fn test_utf8_escape_beyond_max_codepoint_is_dropped() {
    // U+110000 is past the last valid Unicode codepoint.
    let out = Template::new("t")
        .parse(r#"A{{"\U00110000"}}B"#)
        .unwrap()
        .execute_to_string(&Value::Nil)
        .unwrap();
    assert_eq!(out, "AB");
}

#[test]
fn test_utf8_escape_noncharacter_preserved() {
    // U+FFFE is a Unicode noncharacter but a valid Rust `char` — it must
    // flow through unchanged.
    let out = Template::new("t")
        .parse("[{{.}}]")
        .unwrap()
        .execute_to_string(&Value::String("\u{FFFE}".into()))
        .unwrap();
    assert_eq!(out, "[\u{FFFE}]");
}

#[test]
fn test_utf8_bom_in_text_is_preserved() {
    // A leading byte-order mark is not stripped from the template body.
    let out = Template::new("t")
        .parse("\u{FEFF}hello")
        .unwrap()
        .execute_to_string(&Value::Nil)
        .unwrap();
    assert_eq!(out, "\u{FEFF}hello");
}

#[test]
fn test_utf8_invalid_hex_escape_is_dropped() {
    // `\xZZ` has non-hex digits → from_str_radix fails → silently dropped.
    let out = Template::new("t")
        .parse(r#"A{{"\xZZ"}}B"#)
        .unwrap()
        .execute_to_string(&Value::Nil)
        .unwrap();
    assert_eq!(out, "AB");
}

// Source-position reporting with UTF-8 prefixes
//
// The lexer tracks positions as character indices into the source (it holds
// the source as `Vec<char>`). Both parse errors (via `Token::line_col`) and
// AST node offsets should report *character* columns/offsets so that a UTF-8
// prefix doesn't skew the reported location.

fn parse_err_line_col(src: &str) -> (usize, usize) {
    use gotmpl::TemplateError;
    match Template::new("t")
        .parse(src)
        .err()
        .expect("expected parse error")
    {
        TemplateError::Parse { line, col, .. } => (line, col),
        other => panic!("expected Parse error, got {other:?}"),
    }
}

#[test]
fn test_parse_error_col_ascii_baseline() {
    // Establish what "correct" looks like with no UTF-8 involved.
    // "{{}}" → empty command; error reported at the `}}` (end of the action).
    let (line, col) = parse_err_line_col("{{}}");
    assert_eq!(line, 1);
    assert_eq!(col, 5);
}

#[test]
fn test_parse_error_col_after_two_byte_utf8() {
    // "é{{}}" — one multi-byte char before the action. The error column must
    // advance by 1 per character, not per UTF-8 byte.
    let (line, col) = parse_err_line_col("é{{}}");
    assert_eq!(line, 1);
    assert_eq!(
        col, 6,
        "column must count characters, not bytes (got {col})"
    );
}

#[test]
fn test_parse_error_col_after_four_byte_utf8() {
    // "🎉{{}}" — 🎉 is a 4-byte UTF-8 sequence. Column should still be 6.
    let (line, col) = parse_err_line_col("🎉{{}}");
    assert_eq!(line, 1);
    assert_eq!(
        col, 6,
        "4-byte UTF-8 char must count as a single column (got {col})"
    );
}

#[test]
fn test_parse_error_col_after_multiple_utf8_chars() {
    // "ééé{{}}" — three 2-byte chars. Column should be 8.
    let (line, col) = parse_err_line_col("ééé{{}}");
    assert_eq!(line, 1);
    assert_eq!(col, 8, "got {col}");
}

#[test]
fn test_parse_error_line_and_col_across_newline_after_utf8() {
    // UTF-8 on line 1, error on line 2 after another UTF-8 char.
    // Line tracking should be unaffected; column on line 2 should count chars.
    let (line, col) = parse_err_line_col("日本語\né{{}}");
    assert_eq!(line, 2);
    assert_eq!(col, 6, "got ({line}, {col})");
}

#[test]
fn test_parse_tree_node_offsets_are_byte_indices() {
    // AST node offsets are documented (see Pos::offset) as byte offsets into
    // the source. For "éé{{.X}}", `é` is 2 bytes so `.` sits at byte 6.
    use gotmpl::parse::{Expr, Node, Parser};

    let src = "éé{{.X}}";
    let (tree, _) = Parser::new(src, "{{", "}}").unwrap().parse().unwrap();

    let action = tree
        .nodes
        .iter()
        .find_map(|n| {
            if let Node::Action(a) = n {
                Some(a)
            } else {
                None
            }
        })
        .expect("expected an Action node");

    let field_expr = &action.pipe.commands[0].args[0];
    assert!(matches!(field_expr, Expr::Field(_, _)));
    assert_eq!(
        field_expr.pos().offset,
        6,
        "Pos::offset must be a byte offset; got {}",
        field_expr.pos().offset
    );
    assert_eq!(field_expr.pos().line, 1);
}

#[test]
fn test_parse_tree_text_node_offset_after_utf8_and_newlines() {
    // Text node after a UTF-8 line should have line=2 and offset pointing to
    // the character index of that text in the source.
    use gotmpl::parse::{Node, Parser};

    let src = "日本語\nhello{{.}}";
    let (tree, _) = Parser::new(src, "{{", "}}").unwrap().parse().unwrap();

    // First Text node holds "日本語\nhello" and starts at char index 0.
    // (Line for a multi-line text token follows the lexer's convention of
    // reporting the line at emit time, so we don't pin it here.)
    let first_text = tree
        .nodes
        .iter()
        .find_map(|n| if let Node::Text(t) = n { Some(t) } else { None })
        .expect("expected a Text node");
    assert_eq!(first_text.pos.offset, 0);
    assert_eq!(&*first_text.text, "日本語\nhello");

    // The Action `{{.}}` comes after "日本語\nhello" which is 9 bytes (3 CJK
    // chars × 3 bytes) + 1 '\n' + "hello" (5) = 15 bytes, then `{{` (2) puts
    // the `.` at byte 17.
    let action = tree
        .nodes
        .iter()
        .find_map(|n| {
            if let Node::Action(a) = n {
                Some(a)
            } else {
                None
            }
        })
        .expect("expected an Action node");
    let dot_expr = &action.pipe.commands[0].args[0];
    assert_eq!(dot_expr.pos().offset, 17, "got {}", dot_expr.pos().offset);
    assert_eq!(dot_expr.pos().line, 2);
}

// Method-style access (Phase 1.1)
//
// Go's text/template dispatches `.Foo` to a struct method named `Foo` when one
// exists, and propagates a `(T, error)` second return as an execution error.
// We have no reflection and no method table, so `.Foo` is just a field lookup
// against the dot value. These tests pin that divergence so it does not
// silently change. Callers that need callable behavior should expose a
// `Value::Function` field and invoke it via the `call` builtin.

#[test]
fn test_method_style_access_on_map_without_key_returns_no_value() {
    // Pin: .Method against a Map without that key follows MissingKey rules;
    // with the default (Invalid), it renders as `<no value>` — there is no
    // method dispatch.
    let data = tmap! { "X" => 1i64 };
    let out = Template::new("t")
        .parse("{{.Method}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(out, "<no value>");
}

#[test]
fn test_method_style_access_on_map_with_error_mode_errors() {
    let data = tmap! { "X" => 1i64 };
    let err = Template::new("t")
        .missing_key(MissingKey::Error)
        .parse("{{.Method}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap_err();
    assert!(
        matches!(err, gotmpl::TemplateError::MissingKey { .. }),
        "expected MissingKey, got {err:?}"
    );
}

#[test]
fn test_method_style_access_on_nil_receiver_is_nil() {
    // Pin: dotted access against an untyped Nil receiver returns Nil (renders
    // as `<no value>`) instead of erroring. Go would error on a typed-nil
    // method call; we do not model typed-nil at all.
    let out = Template::new("t")
        .parse("{{.Method}}")
        .unwrap()
        .execute_to_string(&Value::Nil)
        .unwrap();
    assert_eq!(out, "<no value>");
}

#[test]
fn test_method_style_access_on_non_map_value_errors() {
    // Pin: dotted access against a non-Map, non-Nil value (e.g. an Int) is
    // an execution error. Exact-match keeps the message wording stable; any
    // future Pos prefix or wording tweak will trip this and force an update.
    let err = Template::new("t")
        .parse("{{.Method}}")
        .unwrap()
        .execute_to_string(&Value::Int(7))
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "execution error: can't evaluate field Method in type int",
    );
}

#[test]
fn test_method_substitute_via_call_on_function_field() {
    // Documents the replacement path for Go's `(T, error)` method calls:
    // store a `Value::Function` in the Map and invoke it with `call`. The
    // function's `Err(...)` becomes an execution error, mirroring the
    // method-error semantics described in the TODO.
    let f: gotmpl::ValueFunc = Arc::new(|_args| Err(gotmpl::TemplateError::Exec("boom".into())));
    let data = tmap! { "Method" => Value::Function(f) };
    let err = Template::new("t")
        .parse("{{call .Method}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap_err();
    assert!(err.to_string().contains("boom"), "got: {err}");
}

#[test]
fn test_method_substitute_chained_call_propagates_error() {
    // Pipeline shape: `{{call .First | call .Middle | printf "%s"}}`.
    // `call .First` succeeds and produces `"hello"`; the pipeline appends it
    // as the trailing argument to `call .Middle`, which errors. The `printf`
    // stage must not run, and the original error must surface unchanged —
    // closing the gap that Go's chained `t.Method1.Method2.Method3` covers
    // via reflection (we route it through `call` instead).
    let ok_fn: gotmpl::ValueFunc = Arc::new(|_args| Ok(Value::String("hello".into())));
    let bad_fn: gotmpl::ValueFunc =
        Arc::new(|_args| Err(gotmpl::TemplateError::Exec("middle failed".into())));
    let data = tmap! {
        "First" => Value::Function(ok_fn),
        "Middle" => Value::Function(bad_fn),
    };
    let err = Template::new("t")
        .parse("{{call .First | call .Middle | printf \"%s\"}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap_err();
    assert!(err.to_string().contains("middle failed"), "got: {err}");
}

// Multi-step template cycles (Phase 8.3).
//
// Single self-recursive template depth is already covered by
// `test_max_exec_depth`. These tests cover indirect cycles (A→B→A,
// A→B→C→A) and a guarded self-recursion.

#[test]
fn test_two_step_cycle_aborts_with_recursion_limit() {
    let src = r#"
        {{define "a"}}{{template "b" .}}{{end}}
        {{define "b"}}{{template "a" .}}{{end}}
        {{template "a" .}}
    "#;
    let err = Template::new("root")
        .parse(src)
        .unwrap()
        .execute_to_string(&Value::Nil)
        .err()
        .expect("expected recursion limit");
    assert!(matches!(err, gotmpl::TemplateError::RecursionLimit));
}

#[test]
fn test_three_step_cycle_aborts_with_recursion_limit() {
    let src = r#"
        {{define "a"}}{{template "b" .}}{{end}}
        {{define "b"}}{{template "c" .}}{{end}}
        {{define "c"}}{{template "a" .}}{{end}}
        {{template "a" .}}
    "#;
    let err = Template::new("root")
        .parse(src)
        .unwrap()
        .execute_to_string(&Value::Nil)
        .err()
        .expect("expected recursion limit");
    assert!(matches!(err, gotmpl::TemplateError::RecursionLimit));
}

#[test]
fn test_guarded_self_recursion_terminates() {
    // A self-recursive template with a base case (countdown). Hits the limit
    // only when there's no termination — here it should run cleanly.
    let src = r#"{{define "rec"}}{{if gt . 0}}({{.}}){{template "rec" (sub . 1)}}{{end}}{{end}}{{template "rec" .}}"#;
    let out = Template::new("root")
        .func("sub", |args| {
            let a = args.first().and_then(Value::as_int).unwrap_or(0);
            let b = args.get(1).and_then(Value::as_int).unwrap_or(0);
            Ok(Value::Int(a - b))
        })
        .parse(src)
        .unwrap()
        .execute_to_string(&Value::Int(5))
        .unwrap();
    assert_eq!(out, "(5)(4)(3)(2)(1)");
}

// Multi-file override semantics (Phase 8.2).
//
// Go's `parse_files`/`parse_glob` and successive `parse` calls follow
// "last-defined wins" for `{{define "x"}}` blocks. Pinned across both APIs.

#[cfg(feature = "std")]
#[test]
fn test_parse_files_define_override_last_wins() {
    let dir = TempDir::new("parse_files_override");
    let a = dir.path().join("a.tmpl");
    let b = dir.path().join("b.tmpl");
    std::fs::write(&a, r#"{{define "foo"}}A{{end}}"#).unwrap();
    std::fs::write(&b, r#"{{define "foo"}}B{{end}}"#).unwrap();

    let tmpl = Template::new("root")
        .parse(r#"{{template "foo"}}"#)
        .unwrap()
        .parse_files(&[a.to_str().unwrap(), b.to_str().unwrap()])
        .unwrap();
    assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "B");

    // Reverse order — A now wins.
    let tmpl = Template::new("root")
        .parse(r#"{{template "foo"}}"#)
        .unwrap()
        .parse_files(&[b.to_str().unwrap(), a.to_str().unwrap()])
        .unwrap();
    assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "A");
}

#[test]
fn test_successive_parse_calls_define_override_last_wins() {
    let tmpl = Template::new("root")
        .parse(r#"{{template "foo"}}"#)
        .unwrap()
        .parse(r#"{{define "foo"}}first{{end}}"#)
        .unwrap()
        .parse(r#"{{define "foo"}}second{{end}}"#)
        .unwrap();
    assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "second");
}

#[cfg(feature = "std")]
#[test]
fn test_parse_files_nonexistent_returns_readfile() {
    let err = Template::new("t")
        .parse_files(&["/definitely/not/a/path.tmpl"])
        .err()
        .expect("expected ReadFile");
    assert!(matches!(err, gotmpl::TemplateError::ReadFile { .. }));
}

// Typed-nil vs untyped-nil (Phase 5.3).
//
// Go distinguishes typed-nil (e.g. `(*Foo)(nil)`) from untyped-nil; certain
// comparisons and method-receiver checks fire only on typed-nil. We have no
// type tag on `Value::Nil`, so every nil collapses to the untyped form.
// These tests pin that behavior so future typed-nil work is forced to revisit.

#[test]
fn test_option_none_collapses_to_untyped_nil() {
    use gotmpl::ToValue;
    let n: Option<i64> = None;
    assert_eq!(n.to_value(), Value::Nil);
}

#[test]
fn test_option_none_eq_to_nil_literal() {
    // `Option::<i64>::None` and the template `nil` literal compare equal.
    let data = tmap! { "Maybe" => Option::<i64>::None };
    let out = Template::new("t")
        .parse("{{if eq .Maybe nil}}same{{else}}diff{{end}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(out, "same");
}

#[test]
fn test_option_none_is_falsy_in_if() {
    // `if` against an absent value falls through to else.
    let data = tmap! { "Maybe" => Option::<i64>::None };
    let out = Template::new("t")
        .parse("{{if .Maybe}}yes{{else}}no{{end}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(out, "no");
}

// Unsupported types — graceful rejection (Phase 5.2).
//
// We have no `ToValue` impl for complex numbers, `chrono::DateTime`, raw
// pointers, etc., so the *type system* blocks them at compile time — there
// is no runtime panic to worry about. The supported bridge for custom types
// is to implement `ToValue` (typically returning `Value::String`), or to
// convert at the boundary. These tests document the bridge.

#[test]
fn test_unsupported_type_via_to_value_string_bridge() {
    // A made-up complex-number-ish struct. The template engine sees it as a
    // string via `ToValue`, no special handling required.
    use gotmpl::ToValue;
    struct ComplexLike {
        re: f64,
        im: f64,
    }
    impl ToValue for ComplexLike {
        fn to_value(&self) -> Value {
            Value::String(format!("{}+{}i", self.re, self.im).into())
        }
    }
    let data = tmap! { "Z" => ComplexLike { re: 1.5, im: 2.5 } };
    let out = Template::new("t")
        .parse("{{.Z}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(out, "1.5+2.5i");
}

#[test]
fn test_unsupported_type_struct_into_map_bridge() {
    // Idiomatic substitute for Go's struct-passes-through-reflection: build a
    // `Value::Map` at the boundary. No bespoke ToValue impl needed.
    let data = tmap! {
        "Event" => tmap! {
            "Year" => 2026i64,
            "Month" => 5i64,
            "Day" => 9i64,
        },
    };
    let out = Template::new("t")
        .parse("{{.Event.Year}}-{{.Event.Month}}-{{.Event.Day}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(out, "2026-5-9");
}

// Numeric literal error clarity (Phase 2.3).
//
// Pin error messages for invalid / unsupported numeric forms so changes are
// intentional. `3i` is the lone divergence: Go rejects it at parse (complex
// literals are unsupported in templates); we accept the `3` as a number and
// then trip over the trailing `i` at exec, producing a different but
// unambiguous error.

#[test]
fn test_numeric_imaginary_literal_pinned_divergence() {
    let err = Template::new("t")
        .parse("{{3i}}")
        .ok()
        .expect("parse currently succeeds")
        .execute_to_string(&Value::Nil)
        .err()
        .expect("exec must error on the trailing identifier");
    let msg = err.to_string();
    assert!(
        msg.contains("can't give argument to non-function"),
        "got: {msg}"
    );
}

#[test]
fn test_numeric_incomplete_binary_errors_at_parse() {
    let err = Template::new("t")
        .parse("{{0b}}")
        .err()
        .expect("expected parse error");
    assert!(err.to_string().contains("invalid base-2 number"));
}

#[test]
fn test_numeric_incomplete_hex_errors_at_parse() {
    let err = Template::new("t")
        .parse("{{0x}}")
        .err()
        .expect("expected parse error");
    assert!(err.to_string().contains("invalid hex number"));
}

#[test]
fn test_numeric_hex_overflow_errors_at_parse() {
    // 17 hex digits — overflows i64. Pin the message.
    let err = Template::new("t")
        .parse("{{0xFFFFFFFFFFFFFFFFF}}")
        .err()
        .expect("expected parse error");
    assert!(err.to_string().contains("overflow"));
}

// Deep scope isolation (Phase 1.4).

#[test]
fn test_nested_range_shadowing_restores_outer() {
    // 5-level nesting where every range rebinds `$x`. The post-range value
    // must be the original `99`, with each inner level seeing only its own
    // `$x` — no scope leak in either direction.
    let data = tmap! {
        "L" => alloc::vec![Value::Int(1), Value::Int(2)],
    };
    let src = "{{$x := 99}}\
        {{range $x := $.L}}A:{{$x}};\
            {{range $x := $.L}}B:{{$x}};\
                {{range $x := $.L}}C:{{$x}};\
                    {{range $x := $.L}}D:{{$x}};\
                        {{range $x := $.L}}E:{{$x}};\
                        {{end}}\
                    {{end}}\
                {{end}}\
            {{end}}\
        {{end}}\
        AFTER:{{$x}}";
    let out = Template::new("t")
        .parse(src)
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert!(out.contains("AFTER:99"), "got: {out}");
    // Sanity — innermost level prints E:1 / E:2 alternating.
    assert!(out.contains("E:1") && out.contains("E:2"));
}

#[test]
fn test_with_outer_var_restored_after_inner_range_rebind() {
    // {{with $x := 1}}{{range $x := $.L}}{{end}}{{$x}}{{end}}
    // Outer `$x = 1` must survive the range's rebinding.
    let data = tmap! { "L" => alloc::vec![Value::Int(7), Value::Int(8)] };
    let out = Template::new("t")
        .parse("{{with $x := 1}}{{range $x := $.L}}{{end}}{{$x}}{{end}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(out, "1");
}

#[test]
fn test_template_call_does_not_leak_callee_var_to_caller() {
    // The callee defines `$x` — it must not be visible after the
    // `{{template}}` call returns.
    let src = r#"{{define "child"}}{{$x := 99}}TPL:{{$x}};{{end}}{{$x := 1}}main:{{$x}};{{template "child"}}again:{{$x}};"#;
    let out = Template::new("root")
        .parse(src)
        .unwrap()
        .execute_to_string(&Value::Nil)
        .unwrap();
    assert_eq!(out, "main:1;TPL:99;again:1;");
}

#[test]
fn test_template_call_does_not_see_caller_var() {
    // Mirror: the caller's `$x` must not be visible inside the callee, even
    // when the callee declares no shadow. Go errors on `undefined variable`.
    let src = r#"{{define "child"}}{{$x}}{{end}}{{$x := 1}}{{template "child"}}"#;
    let err = Template::new("root")
        .parse(src)
        .unwrap()
        .execute_to_string(&Value::Nil)
        .err()
        .expect("expected undefined variable error in callee");
    assert!(matches!(err, gotmpl::TemplateError::UndefinedVariable(_)));
}

// Lookup parity — divergence pin (Phase 8.4).
//
// In Go, `t.Lookup("name")` returns a re-executable `*Template`. We return
// `Option<&ListNode>` — just the parsed body. The replacement path for
// "execute looked-up template" is `execute_template_to_string("name", …)`.
// Pinned here so callers porting from Go know what to substitute.

#[test]
fn test_lookup_returns_listnode_not_template() {
    let tmpl = Template::new("root")
        .parse(r#"{{define "child"}}hi {{.X}}{{end}}{{template "child" .}}"#)
        .unwrap();
    // Has a child.
    let node = tmpl.lookup("child").expect("child should be present");
    // The returned value is a parse-tree borrow, not an executable Template:
    // accessing it does not double-clone the function map.
    assert!(!node.nodes.is_empty());
    // Replacement for Go's `Lookup("child").Execute(w, data)`:
    let out = tmpl
        .execute_template_to_string("child", &tmap! { "X" => "X" })
        .unwrap();
    assert_eq!(out, "hi X");
}

#[test]
fn test_lookup_missing_returns_none() {
    let tmpl = Template::new("root").parse("hi").unwrap();
    assert!(tmpl.lookup("nope").is_none());
}

// Error recovery — divergence pin (Phase 9).
//
// Go's parser collects multiple errors per pass; we abort at the first one.
// Pinned here so a future switch to multi-error mode trips this test.

#[test]
fn test_parser_stops_at_first_error_pinned() {
    // Two structurally independent parse errors — second `{{}}` would also
    // fail in isolation. Pin two things structurally:
    //   1. The result is a single `Parse` variant (multi-error mode would
    //      have to introduce a new variant or wrap a Vec — either trips here).
    //   2. The reported position is the *first* `{{}}` (col < 10), not the
    //      second (which would land at col 14). This catches a future change
    //      that swallows-then-reports.
    let err = Template::new("t")
        .parse("a {{}} b {{}} c")
        .err()
        .expect("expected parse error");
    match err {
        gotmpl::TemplateError::Parse { line, col, .. } => {
            assert_eq!(line, 1);
            assert!(
                col < 10,
                "should report the first {{}} location, got col {col}",
            );
        }
        other => panic!("expected single Parse variant, got {other:?}"),
    }
}

// I/O edges (Phase 7).

#[test]
fn test_bom_inside_action_is_lex_error() {
    // BOM (U+FEFF) is not a valid start-of-action character. Pin the error
    // text so any future "lex BOM as space" change surfaces here.
    let err = Template::new("t")
        .parse("{{\u{FEFF}.X}}")
        .err()
        .expect("expected lex error");
    let msg = err.to_string();
    assert!(msg.contains("unexpected character"), "got: {msg}");
}

#[cfg(feature = "std")]
#[test]
fn test_parse_files_non_utf8_errors_gracefully() {
    // TODO.md "Differs from Go" #1 — std::fs::read_to_string enforces UTF-8,
    // so a file with arbitrary bytes (here, `0xFE 0xFF`) errors at read time
    // with `ReadFile { source: InvalidData }`, where Go would accept it
    // (Go strings are byte-slices). Pin the error variant.
    let dir = TempDir::new("non_utf8");
    let path = dir.path().join("bad.tmpl");
    std::fs::write(&path, [0xFEu8, 0xFFu8, b'h', b'i']).unwrap();

    let err = Template::new("bad.tmpl")
        .parse_files(&[path.to_str().unwrap()])
        .err()
        .expect("expected ReadFile error on non-UTF-8 input");
    match err {
        gotmpl::TemplateError::ReadFile { source, .. } => {
            assert_eq!(source.kind(), std::io::ErrorKind::InvalidData);
        }
        other => panic!("expected ReadFile, got {other:?}"),
    }
}

// Custom delimiter edge cases (Phase 3.2 / 6.1).
//
// We do byte-literal delim matching, not regex, so meta-characters like `(?`
// or `[*` are allowed and treated literally. Empty delims and identical
// left/right delims are the trickier cases — pinned here.

#[test]
fn test_delims_regex_metachars_are_literal() {
    // Each pair below is a regex metacharacter sequence — it must be matched
    // verbatim, not as a pattern.
    for (l, r) in [("(?", ")?"), ("[*", "*]"), ("+^", "$+")] {
        let src = format!("hi{}.X{}done", l, r);
        let out = Template::new("t")
            .delims(l, r)
            .parse(&src)
            .unwrap()
            .execute_to_string(&tmap! { "X" => "X" })
            .unwrap();
        assert_eq!(out, "hiXdone", "delims=({l:?},{r:?})");
    }
}

#[test]
fn test_delims_identical_left_and_right_pinned() {
    // Pin the divergence: Go's text/template forbids identical left/right
    // delimiters (the parser can't disambiguate the closing token), but we
    // accept them. This test documents the current behavior so the day we
    // tighten validation, this test trips and gets revisited.
    let out = Template::new("t")
        .delims("##", "##")
        .parse("a ##.X## b")
        .unwrap()
        .execute_to_string(&tmap! { "X" => "X" })
        .unwrap();
    assert_eq!(out, "a X b");
}

#[test]
fn test_delims_empty_left_pinned() {
    // Empty left delim is not silently substituted with `{{` (Go does that)
    // but produces a parse error downstream — pinning the divergence.
    let err = Template::new("t")
        .delims("", "}}")
        .parse("test")
        .err()
        .expect("expected error from empty left delim");
    let msg = err.to_string();
    assert!(
        msg.contains("unclosed action") || msg.contains("delim"),
        "empty left should error somewhere; got: {msg}"
    );
}

#[test]
fn test_delims_empty_right_errors_at_parse() {
    // Empty right delim — also errors. Document the message so changes are
    // intentional.
    let err = Template::new("t")
        .delims("{{", "")
        .parse("hi {{.X}}")
        .err()
        .expect("expected error from empty right delim");
    let msg = err.to_string();
    assert!(
        msg.contains("empty command") || msg.contains("delim"),
        "got: {msg}"
    );
}

// Phase 6 — delimiter mixed-content edges.
//
// When custom delims are in use, the default `{{`/`}}` characters become
// ordinary text — they must render verbatim. Conversely with default delims,
// stray `<<`/`>>` are also plain text. A mid-template delim change is not
// supported (delims are per-parse, not per-action).

#[test]
fn test_default_delims_render_custom_delim_chars_as_text() {
    // Custom delims `<<`/`>>` — `{{` and `}}` in the source should render as-is.
    let out = Template::new("t")
        .delims("<<", ">>")
        .parse("braces {{ here }} <<.X>>")
        .unwrap()
        .execute_to_string(&tmap! { "X" => "X" })
        .unwrap();
    assert_eq!(out, "braces {{ here }} X");
}

#[test]
fn test_custom_delims_text_containing_default_delims() {
    // Inverse direction — text containing `{{x}}` should pass through untouched
    // when delims are `<<`/`>>`.
    let out = Template::new("t")
        .delims("<<", ">>")
        .parse("hello {{x}}")
        .unwrap()
        .execute_to_string(&Value::Nil)
        .unwrap();
    assert_eq!(out, "hello {{x}}");
}

#[test]
fn test_delim_change_mid_template_is_not_supported() {
    // Pin: changing delims mid-template (Go style: not supported via syntax;
    // Delims() is per-parse). Our `Template::delims` takes effect on the
    // *next* `parse`, not retroactively. Calling `.delims()` after `.parse()`
    // must not affect the already-parsed tree.
    let mut tmpl = Template::new("t").parse("{{.X}}").unwrap();
    tmpl = tmpl.delims("<<", ">>"); // late call — must be a no-op for the parsed tree
    let out = tmpl.execute_to_string(&tmap! { "X" => "X" }).unwrap();
    assert_eq!(out, "X");
}

// parse_glob (Phase 8.1).
//
// Counterpart to Go's `(*Template).ParseGlob`. Limitations vs Go are
// documented on the API; tests here exercise the supported surface.

#[cfg(feature = "glob")]
#[test]
fn test_parse_glob_loads_all_matches() {
    let dir = TempDir::new("parse_glob_loads");
    std::fs::write(dir.path().join("a.tmpl"), r#"{{define "a"}}A{{end}}"#).unwrap();
    std::fs::write(dir.path().join("b.tmpl"), r#"{{define "b"}}B{{end}}"#).unwrap();
    // Non-matching extension — must be ignored.
    std::fs::write(dir.path().join("ignored.txt"), "ignored").unwrap();

    let pattern = format!("{}/*.tmpl", dir.path().display());
    let tmpl = Template::new("root")
        .parse(r#"{{template "a" .}}+{{template "b" .}}"#)
        .unwrap()
        .parse_glob(&pattern)
        .unwrap();

    assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "A+B");
}

#[cfg(feature = "glob")]
#[test]
fn test_parse_glob_no_matches_errors() {
    let dir = TempDir::new("parse_glob_empty");

    let pattern = format!("{}/*.tmpl", dir.path().display());
    let result = Template::new("root").parse_glob(&pattern);
    let err = result.err().expect("expected NoFiles error");
    assert!(
        matches!(err, gotmpl::TemplateError::NoFiles),
        "expected NoFiles, got {err:?}"
    );
}

#[cfg(feature = "glob")]
#[test]
fn test_parse_glob_question_mark() {
    // `?` matches a single character.
    let dir = TempDir::new("parse_glob_qmark");
    std::fs::write(dir.path().join("a.t"), r#"{{define "a.t"}}1{{end}}"#).unwrap();
    std::fs::write(dir.path().join("b.t"), r#"{{define "b.t"}}2{{end}}"#).unwrap();
    std::fs::write(dir.path().join("ab.t"), "should not match ?.t").unwrap();

    let pattern = format!("{}/?.t", dir.path().display());
    let tmpl = Template::new("root").parse_glob(&pattern).unwrap();
    // Both single-char-name files become known templates.
    assert!(tmpl.lookup("a.t").is_some());
    assert!(tmpl.lookup("b.t").is_some());
    assert!(tmpl.lookup("ab.t").is_none());
}

#[cfg(feature = "glob")]
#[test]
fn test_parse_glob_wildcard_in_dir_component_works() {
    // The `glob` crate honors wildcards in any path component, matching Go's
    // `filepath.Glob`. Build a layout `<tmp>/inner-{1,2}/*.tmpl` and verify
    // both subdirs are visited.
    let dir = TempDir::new("parse_glob_dir_wildcard");
    for sub in ["inner-1", "inner-2"] {
        let p = dir.path().join(sub);
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("x.tmpl"),
            format!(r#"{{{{define "{sub}"}}}}{sub}{{{{end}}}}"#),
        )
        .unwrap();
    }
    let pattern = format!("{}/inner-*/x.tmpl", dir.path().display());
    let tmpl = Template::new("root").parse_glob(&pattern).unwrap();
    assert!(tmpl.lookup("inner-1").is_some());
    assert!(tmpl.lookup("inner-2").is_some());
}

#[cfg(feature = "glob")]
#[test]
fn test_parse_glob_nonexistent_path_errors_no_files() {
    // The `glob` crate yields no matches for a nonexistent directory rather
    // than surfacing the underlying `ENOENT` — same shape as Go's
    // `filepath.Glob`, which returns an empty slice + nil error in that case.
    let err = Template::new("t")
        .parse_glob("/definitely/not/a/path/*.tmpl")
        .err()
        .expect("expected NoFiles error");
    assert!(
        matches!(err, gotmpl::TemplateError::NoFiles),
        "expected NoFiles, got {err:?}"
    );
}

#[cfg(feature = "glob")]
#[test]
fn test_parse_glob_invalid_pattern_errors() {
    // Unbalanced `[` is rejected by the `glob` crate at compile time.
    let err = Template::new("t")
        .parse_glob("/tmp/[unbalanced")
        .err()
        .expect("expected pattern error");
    match err {
        gotmpl::TemplateError::BadPattern { ref pattern, .. } => {
            assert_eq!(pattern, "/tmp/[unbalanced");
        }
        other => panic!("expected BadPattern, got {other:?}"),
    }
}

#[cfg(feature = "glob")]
#[test]
fn test_parse_glob_double_star_recurses() {
    // `**` is a `glob` crate extension over Go's `filepath.Match`; it matches
    // zero or more path components. Build a nested layout and confirm files
    // at multiple depths are all collected.
    let dir = TempDir::new("parse_glob_recursive");
    std::fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
    std::fs::write(dir.path().join("top.tmpl"), r#"{{define "top"}}T{{end}}"#).unwrap();
    std::fs::write(dir.path().join("a/mid.tmpl"), r#"{{define "mid"}}M{{end}}"#).unwrap();
    std::fs::write(
        dir.path().join("a/b/c/deep.tmpl"),
        r#"{{define "deep"}}D{{end}}"#,
    )
    .unwrap();

    let pattern = format!("{}/**/*.tmpl", dir.path().display());
    let tmpl = Template::new("root").parse_glob(&pattern).unwrap();
    assert!(tmpl.lookup("top").is_some());
    assert!(tmpl.lookup("mid").is_some());
    assert!(tmpl.lookup("deep").is_some());
}

#[cfg(feature = "glob")]
#[test]
fn test_parse_glob_character_class() {
    // `[abc].tmpl` — character class supported via the `glob` crate; Go's
    // `filepath.Match` agrees.
    let dir = TempDir::new("parse_glob_class");
    for name in ["a.tmpl", "b.tmpl", "z.tmpl"] {
        std::fs::write(
            dir.path().join(name),
            format!(r#"{{{{define "{name}"}}}}{name}{{{{end}}}}"#),
        )
        .unwrap();
    }
    let pattern = format!("{}/[ab].tmpl", dir.path().display());
    let tmpl = Template::new("root").parse_glob(&pattern).unwrap();
    assert!(tmpl.lookup("a.tmpl").is_some());
    assert!(tmpl.lookup("b.tmpl").is_some());
    assert!(tmpl.lookup("z.tmpl").is_none());
}

// Runtime error positions — divergence pin (Phase 1.2).
//
// Go's text/template prefixes runtime errors with `template: NAME:LINE:COL:`,
// pointing to the AST node that triggered them. Our `TemplateError` exec
// variants (`MissingKey`, `UndefinedFunction`, `Exec`, …) currently carry no
// `Pos` — see TODO.md line 14. These tests pin the current bare messages so
// that the day Pos propagation lands, the assertions will trip and force an
// explicit update to the new format.

#[test]
fn test_runtime_error_missingkey_lacks_position_info() {
    // `.BadField` on line 3, column 1 — Go would say
    //   template: t:3:3: map has no entry for key "BadField"
    // We currently emit only the bare message. Exact-string assertion: the
    // day Pos propagation lands, this test trips and forces an explicit
    // update to the new format (and TODO.md:14).
    let data = tmap! { "X" => 1i64 };
    let src = "line1\nline2\n{{.BadField}}\nline4";
    let err = Template::new("t")
        .missing_key(MissingKey::Error)
        .parse(src)
        .unwrap()
        .execute_to_string(&data)
        .unwrap_err();
    assert_eq!(err.to_string(), "map has no entry for key: BadField");
}

#[test]
fn test_runtime_error_undefined_function_lacks_position_info() {
    // `{{undefinedFn}}` — no Pos at error time. Exact match so any future
    // `template: t:2:3:` prefix shows up here.
    let err = Template::new("t")
        .parse("a\n{{undefinedFn}}\nb")
        .unwrap()
        .execute_to_string(&Value::Nil)
        .unwrap_err();
    assert_eq!(err.to_string(), "undefined function: undefinedFn");
}

#[test]
fn test_runtime_error_inside_range_lacks_position_info() {
    // Error originating inside a `{{range}}` body — Go locates it inside the
    // body, not at the `range` header. We can't yet distinguish the two.
    // Exact-string assertion pins both the absence of Pos and the message
    // form; either change trips the test. Items must be maps so `.BadField`
    // triggers `MissingKey` (the variant we're pinning) rather than field-on-int.
    let data = tmap! {
        "Items" => alloc::vec![tmap! {}],
    };
    let err = Template::new("t")
        .missing_key(MissingKey::Error)
        .parse("{{range .Items}}\n  {{.BadField}}\n{{end}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap_err();
    assert_eq!(err.to_string(), "map has no entry for key: BadField");
}

// uint family — large unsigned values (Phase 5.1).
//
// `ToValue` maps every unsigned type to `Value::Uint(u64)`, so values above
// i64::MAX keep their true magnitude instead of wrapping to a negative i64.
// (These tests previously pinned the old wrapping divergence; the fix flipped
// them to assert the Go-matching behavior.)

#[test]
fn test_uint_u64_max_renders_unsigned() {
    use gotmpl::ToValue;
    assert_eq!(u64::MAX.to_value(), Value::Uint(u64::MAX));

    let data = tmap! { "U" => u64::MAX };
    let out = Template::new("t")
        .parse("{{.U}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(out, "18446744073709551615");
}

#[test]
fn test_uint_usize_max_to_value() {
    // usize maps through the unsigned family. On 64-bit targets usize::MAX is
    // u64::MAX; on 32-bit targets it is u32::MAX — both are exact as Uint.
    use gotmpl::ToValue;
    assert_eq!(usize::MAX.to_value(), Value::Uint(usize::MAX as u64));
}
