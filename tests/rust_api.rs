// tests/rust_api.rs
//
// Tests for Rust-specific APIs and implementation details that don't correspond
// to specific Go text/template test cases.
//
// These were originally in go_compat.rs but moved here to keep that file
// exclusively for tests ported from Go's exec_test.go and multi_test.go.

use go_template_rs::Value;
use go_template_rs::{Template, tmap};

// ─── Value::Function variant and call builtin ─────────────────────────────

#[test]
fn test_call_function_value() {
    use std::sync::Arc;
    let adder: go_template_rs::ValueFunc = Arc::new(|args: &[Value]| {
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
    use std::sync::Arc;
    let f: go_template_rs::ValueFunc = Arc::new(|_| Ok(Value::Int(42)));
    let data = Value::Function(f);
    assert!(data.is_truthy());
}

// ─── missingkey option API ────────────────────────────────────────────────

#[test]
fn test_missingkey_error_integration() {
    let data = tmap! { "X" => 1i64 };
    let result = Template::new("test")
        .option("missingkey=error")
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
        .option("missingkey=zero")
        .parse("{{.Y}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(result, "<nil>");
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
        .option("missingkey=error")
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

// ─── add_parse_tree API ───────────────────────────────────────────────────

#[test]
fn test_add_parse_tree_to_unparsed() {
    use go_template_rs::parse::{ListNode, Node, Pos, TextNode};

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
    let mut buf = Vec::new();
    tmpl.execute_template(&mut buf, "greeting", &Value::Nil)
        .unwrap();
    assert_eq!(String::from_utf8(buf).unwrap(), "hello");
}
