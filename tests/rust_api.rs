// tests/rust_api.rs
//
// Tests for Rust-specific APIs and implementation details that don't correspond
// to specific Go text/template test cases.
//
// These were originally in go_compat.rs and are separated here to keep
// compatibility tests easier to audit against upstream Go behavior.

extern crate alloc;
use alloc::sync::Arc;

use gotmpl::Value;
use gotmpl::{MissingKey, Template, tmap};

// ─── Value::Function variant and call builtin ─────────────────────────────

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

// ─── missingkey option API ────────────────────────────────────────────────

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

// ─── Max execution depth (Rust safety guard) ──────────────────────────────

#[test]
fn test_max_exec_depth() {
    // Recursive template should error, not stack overflow
    let result = Template::new("test")
        .parse(r#"{{define "recurse"}}{{template "recurse" .}}{{end}}{{template "recurse" .}}"#)
        .unwrap()
        .execute_to_string(&Value::Nil);
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("maximum template call depth")
    );
}

// ─── parse_additional API ─────────────────────────────────────────────────

#[test]
fn test_parse_additional_defines() {
    let tmpl = Template::new("root")
        .parse(r#"{{template "header" .}}body{{template "footer" .}}"#)
        .unwrap()
        .parse_additional(r#"{{define "header"}}<h1>{{.Title}}</h1>{{end}}"#)
        .unwrap()
        .parse_additional(r#"{{define "footer"}}<footer>bye</footer>{{end}}"#)
        .unwrap();

    let data = tmap! { "Title" => "Hello" };
    let result = tmpl.execute_to_string(&data).unwrap();
    assert_eq!(result, "<h1>Hello</h1>body<footer>bye</footer>");
}

#[test]
fn test_parse_additional_override() {
    // Later parse_additional overrides earlier define
    let tmpl = Template::new("root")
        .parse(r#"{{define "x"}}first{{end}}{{template "x"}}"#)
        .unwrap()
        .parse_additional(r#"{{define "x"}}second{{end}}"#)
        .unwrap();

    assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "second");
}

#[test]
fn test_parse_additional_syntax_error() {
    let result = Template::new("root")
        .parse("main")
        .unwrap()
        .parse_additional("{{define}}");
    assert!(result.is_err());
}

// ─── Clone API ────────────────────────────────────────────────────────────

#[test]
fn test_clone_preserves_missingkey_error() {
    let original = Template::new("t")
        .missing_key(MissingKey::Error)
        .parse("{{.X}}")
        .unwrap();

    let cloned = original.clone_template();

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
    let cloned = original.clone_template();
    assert_eq!(cloned.execute_to_string(&data).unwrap(), "hello");
}

#[test]
fn test_clone_unparsed_template() {
    let t = Template::new("empty");
    let cloned = t.clone_template();
    // Should not panic; executing should error gracefully
    assert!(cloned.execute_to_string(&Value::Nil).is_err());
}

// ─── ToValue implementations ─────────────────────────────────────────────

#[test]
fn test_to_value_integers() {
    use gotmpl::ToValue;

    assert_eq!(42i8.to_value(), Value::Int(42));
    assert_eq!(42i16.to_value(), Value::Int(42));
    assert_eq!(42i32.to_value(), Value::Int(42));
    assert_eq!(42i64.to_value(), Value::Int(42));
    assert_eq!(42u8.to_value(), Value::Int(42));
    assert_eq!(42u16.to_value(), Value::Int(42));
    assert_eq!(42u32.to_value(), Value::Int(42));
    assert_eq!(42u64.to_value(), Value::Int(42));
    assert_eq!(42usize.to_value(), Value::Int(42));
    assert_eq!(42isize.to_value(), Value::Int(42));
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

// ─── From impls for Value ────────────────────────────────────────────────

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

#[test]
fn test_block_override_with_nested_pipeline() {
    // Block override should work for bodies containing pipelines/control flow,
    // not just bare text.
    let tmpl = Template::new("t")
        .parse(r#"[{{block "b" .}}{{.X | printf "%d"}}{{end}}]"#)
        .unwrap()
        .parse_additional(r#"{{define "b"}}{{range .Items}}{{.}}/{{end}}{{end}}"#)
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

// ─── add_parse_tree API ───────────────────────────────────────────────────

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

// ─── UTF-8 escape edge cases ──────────────────────────────────────────────
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

// ─── Source-position reporting with UTF-8 prefixes ────────────────────────
//
// The lexer tracks positions as character indices into the source (it holds
// the source as `Vec<char>`). Both parse errors (via `Token::line_col`) and
// AST node offsets should report *character* columns/offsets so that a UTF-8
// prefix doesn't skew the reported location.

fn parse_err_line_col(src: &str) -> (usize, usize) {
    use gotmpl::TemplateError;
    match Template::new("t").parse(src).err().expect("expected parse error") {
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
        .find_map(|n| if let Node::Action(a) = n { Some(a) } else { None })
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
        .find_map(|n| if let Node::Action(a) = n { Some(a) } else { None })
        .expect("expected an Action node");
    let dot_expr = &action.pipe.commands[0].args[0];
    assert_eq!(dot_expr.pos().offset, 17, "got {}", dot_expr.pos().offset);
    assert_eq!(dot_expr.pos().line, 2);
}
