extern crate alloc;
// tests/go_compat.rs
//
// Test cases ported from Go's text/template/exec_test.go and multi_test.go.
//
// We skip test cases that depend on:
//   - Go's reflect (struct methods, interfaces, pointers, type assertions)
//   - Go's complex number type
//   - Go's channel type
//   - Go's unsafe.Pointer
//   - Go's iter.Seq / iter.Seq2
//   - Go's specific fmt.Sprintf formatting (e.g. %T for type names)
//
// Additional Rust-only edge-case tests may appear here when needed to guard
// behavior parity with upstream Go tests.

use gotmpl::Value;
use gotmpl::{Template, tmap};

// Go cross-check
//
// When the `go-crosscheck` feature is enabled, every call to `ok()` also
// executes the same template through Go's text/template and asserts that
// both implementations produce the same output.
//
// Usage:  cargo test --features go-crosscheck

#[cfg(feature = "go-crosscheck")]
mod go_crosscheck {
    use super::Value;
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::LazyLock;

    /// Path to the compiled Go helper binary. Built exactly once per test run.
    static GO_BINARY: LazyLock<PathBuf> = LazyLock::new(|| {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/testdata/go_crosscheck.go");
        let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("go-crosscheck");
        let output = Command::new("go")
            .args(["build", "-o", bin.to_str().unwrap(), src.to_str().unwrap()])
            .output()
            .expect("failed to run `go build` — is Go installed?");
        assert!(
            output.status.success(),
            "go build failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        bin
    });

    /// Escape a string for embedding in JSON.
    fn json_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c < '\x20' => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                }
                c => out.push(c),
            }
        }
        out
    }

    /// Encode a `Value` as our typed-JSON protocol.
    /// Returns `None` for `Value::Function` (cannot be serialized).
    fn value_to_json(v: &Value) -> Option<String> {
        Some(match v {
            Value::Nil => r#"{"type":"nil"}"#.to_string(),
            Value::Bool(b) => format!(r#"{{"type":"bool","value":{b}}}"#),
            Value::Int(n) => format!(r#"{{"type":"int","value":{n}}}"#),
            Value::Float(f) => {
                // JSON doesn't support inf/nan; bail out.
                if f.is_infinite() || f.is_nan() {
                    return None;
                }
                // Ensure there's always a decimal point so Go reads float64.
                let s = if f.fract() == 0.0 && !f.is_nan() {
                    format!("{f:.1}")
                } else {
                    format!("{f}")
                };
                format!(r#"{{"type":"float","value":{s}}}"#)
            }
            Value::String(s) => {
                format!(r#"{{"type":"string","value":"{}"}}"#, json_escape(s))
            }
            Value::List(items) => {
                let encoded: Option<Vec<String>> = items.iter().map(value_to_json).collect();
                let encoded = encoded?;
                format!(r#"{{"type":"list","items":[{}]}}"#, encoded.join(","))
            }
            Value::Map(m) => {
                let mut entries = Vec::new();
                for (k, v) in m.as_ref() {
                    let v_json = value_to_json(v)?;
                    entries.push(format!(r#""{}":{}"#, json_escape(k), v_json));
                }
                format!(r#"{{"type":"map","map":{{{}}}}}"#, entries.join(","))
            }
            Value::Function(_) => return None,
        })
    }

    /// Run the same template+data through Go's text/template and assert the
    /// output matches `rust_result`.
    pub fn check(template_str: &str, data: &Value, rust_result: &str) {
        let data_json = match value_to_json(data) {
            Some(j) => j,
            None => return, // skip un-serializable data (e.g. Function values)
        };
        let payload = format!(
            r#"{{"template":"{}","data":{}}}"#,
            json_escape(template_str),
            data_json,
        );

        let mut child = Command::new(GO_BINARY.as_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn go-crosscheck binary");

        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .expect("failed to write to go-crosscheck stdin");

        let output = child.wait_with_output().expect("go-crosscheck failed");

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            panic!(
                "Go crosscheck failed for template {:?}:\n{}",
                template_str, stderr
            );
        }

        let go_result =
            String::from_utf8(output.stdout).expect("go-crosscheck produced non-UTF-8 output");

        assert_eq!(
            rust_result, &go_result,
            "Rust/Go output mismatch for template: {:?}\n  Rust: {:?}\n  Go:   {:?}",
            template_str, rust_result, go_result,
        );
    }
}

// Helper
fn run(input: &str, data: &Value) -> core::result::Result<String, String> {
    Template::new("test")
        .func("add", |args| {
            let sum: i64 = args.iter().filter_map(|a| a.as_int()).sum();
            Ok(Value::Int(sum))
        })
        .func("echo", |args| {
            Ok(args.first().cloned().unwrap_or(Value::Nil))
        })
        .func("oneArg", |args| match args.first() {
            Some(Value::String(s)) => Ok(Value::String(format!("oneArg={}", s).into())),
            _ => Err(gotmpl::TemplateError::Exec(
                "oneArg requires a string".into(),
            )),
        })
        .func("twoArgs", |args| match (args.first(), args.get(1)) {
            (Some(Value::String(a)), Some(Value::String(b))) => {
                Ok(Value::String(format!("twoArgs={}{}", a, b).into()))
            }
            _ => Err(gotmpl::TemplateError::Exec(
                "twoArgs requires two strings".into(),
            )),
        })
        .func("zeroArgs", |_args| Ok(Value::String("zeroArgs".into())))
        .func("count", |args| {
            let n = args.first().and_then(|a| a.as_int()).unwrap_or(0);
            let items: Vec<Value> = (0..n)
                .map(|i| {
                    let c = "abcdefghijklmnop".chars().nth(i as usize).unwrap_or('?');
                    Value::String(c.to_string().into())
                })
                .collect();
            Ok(Value::List(items.into()))
        })
        .func("makemap", |args| {
            let mut m: alloc::collections::BTreeMap<String, Value> =
                alloc::collections::BTreeMap::new();
            let strs: Vec<String> = args.iter().map(|a| format!("{}", a)).collect();
            for chunk in strs.chunks(2) {
                if chunk.len() == 2 {
                    m.insert(chunk[0].clone(), Value::String(chunk[1].as_str().into()));
                }
            }
            Ok(m.into())
        })
        .func("mapOfThree", |_args| {
            let mut m: alloc::collections::BTreeMap<String, Value> =
                alloc::collections::BTreeMap::new();
            m.insert("three".to_string(), Value::Int(3));
            Ok(m.into())
        })
        .parse(input)
        .map_err(|e| e.to_string())?
        .execute_to_string(data)
        .map_err(|e| e.to_string())
}

fn ok(input: &str, data: &Value, expected: &str) {
    match run(input, data) {
        Ok(result) => {
            assert_eq!(result, expected, "template: {}", input);
            #[cfg(feature = "go-crosscheck")]
            go_crosscheck::check(input, data, &result);
        }
        Err(e) => panic!("template {:?} failed: {}", input, e),
    }
}

#[allow(dead_code)]
fn fail(input: &str, data: &Value) {
    if let Ok(result) = run(input, data) {
        panic!(
            "template {:?} should have failed but got {:?}",
            input, result
        );
    }
}

// Trivial cases
#[test]
fn test_empty() {
    ok("", &Value::Nil, "");
}

#[test]
fn test_text() {
    ok("some text", &Value::Nil, "some text");
}

// Fields of maps
#[test]
fn test_field_x() {
    let data = tmap! { "X" => "x" };
    ok("-{{.X}}-", &data, "-x-");
}

#[test]
fn test_nested_field_u_v() {
    let data = tmap! { "U" => tmap! { "V" => "v" } };
    ok("-{{.U.V}}-", &data, "-v-");
}

#[test]
fn test_map_one() {
    let data = tmap! {
        "MSI" => tmap! { "one" => 1i64, "two" => 2i64, "three" => 3i64 }
    };
    ok("{{.MSI.one}}", &data, "1");
}

#[test]
fn test_map_two() {
    let data = tmap! {
        "MSI" => tmap! { "one" => 1i64, "two" => 2i64, "three" => 3i64 }
    };
    ok("{{.MSI.two}}", &data, "2");
}

// Dot of various types
#[test]
fn test_dot_int() {
    ok("<{{.}}>", &Value::Int(13), "<13>");
}

#[test]
fn test_dot_float() {
    ok("<{{.}}>", &Value::Float(15.1), "<15.1>");
}

#[test]
fn test_dot_bool() {
    ok("<{{.}}>", &Value::Bool(true), "<true>");
}

#[test]
fn test_dot_string() {
    ok("<{{.}}>", &Value::String("hello".into()), "<hello>");
}

#[test]
fn test_dot_list() {
    let data = Value::List(vec![Value::Int(-1), Value::Int(-2), Value::Int(-3)].into());
    ok("<{{.}}>", &data, "<[-1 -2 -3]>");
}

// Variables
#[test]
fn test_dollar_int() {
    ok("{{$}}", &Value::Int(123), "123");
}

#[test]
fn test_dollar_field_i() {
    let data = tmap! { "I" => 17i64 };
    ok("{{$.I}}", &data, "17");
}

#[test]
fn test_dollar_nested_field() {
    let data = tmap! { "U" => tmap! { "V" => "v" } };
    ok("{{$.U.V}}", &data, "v");
}

#[test]
fn test_declare_in_action() {
    let data = tmap! { "U" => tmap! { "V" => "v" } };
    ok("{{$x := $.U.V}}{{$x}}", &data, "v");
}

#[test]
fn test_simple_assignment() {
    let data = tmap! {};
    ok("{{$x := 2}}{{$x = 3}}{{$x}}", &data, "3");
}

// If
#[test]
fn test_if_true() {
    ok("{{if true}}TRUE{{end}}", &Value::Nil, "TRUE");
}

#[test]
fn test_if_false() {
    ok("{{if false}}TRUE{{else}}FALSE{{end}}", &Value::Nil, "FALSE");
}

#[test]
fn test_if_1() {
    ok(
        "{{if 1}}NON-ZERO{{else}}ZERO{{end}}",
        &Value::Nil,
        "NON-ZERO",
    );
}

#[test]
fn test_if_0() {
    ok("{{if 0}}NON-ZERO{{else}}ZERO{{end}}", &Value::Nil, "ZERO");
}

#[test]
fn test_if_1_5() {
    ok(
        "{{if 1.5}}NON-ZERO{{else}}ZERO{{end}}",
        &Value::Nil,
        "NON-ZERO",
    );
}

#[test]
fn test_if_empty_string() {
    ok(
        "{{if ``}}NON-EMPTY{{else}}EMPTY{{end}}",
        &Value::Nil,
        "EMPTY",
    );
}

#[test]
fn test_if_notempty_string() {
    ok(
        "{{if `notempty`}}NON-EMPTY{{else}}EMPTY{{end}}",
        &Value::Nil,
        "NON-EMPTY",
    );
}

#[test]
fn test_if_empty_slice() {
    let data = tmap! { "SIEmpty" => Vec::<i64>::new() };
    ok(
        "{{if .SIEmpty}}NON-EMPTY{{else}}EMPTY{{end}}",
        &data,
        "EMPTY",
    );
}

#[test]
fn test_if_slice() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok(
        "{{if .SI}}NON-EMPTY{{else}}EMPTY{{end}}",
        &data,
        "NON-EMPTY",
    );
}

#[test]
fn test_if_empty_map() {
    let data = tmap! {
        "MSIEmpty" => alloc::collections::BTreeMap::<String, i64>::new()
    };
    ok(
        "{{if .MSIEmpty}}NON-EMPTY{{else}}EMPTY{{end}}",
        &data,
        "EMPTY",
    );
}

#[test]
fn test_if_map() {
    let data = tmap! {
        "MSI" => tmap! { "one" => 1i64 }
    };
    ok(
        "{{if .MSI}}NON-EMPTY{{else}}EMPTY{{end}}",
        &data,
        "NON-EMPTY",
    );
}

#[test]
fn test_if_dollar_x_with_dollar_y() {
    let data = tmap! { "I" => 17i64 };
    ok(
        "{{if $x := true}}{{with $y := .I}}{{$x}},{{$y}}{{end}}{{end}}",
        &data,
        "true,17",
    );
}

#[test]
fn test_if_else_if() {
    ok(
        "{{if false}}FALSE{{else if true}}TRUE{{end}}",
        &Value::Nil,
        "TRUE",
    );
}

#[test]
fn test_if_else_chain() {
    ok(
        "{{if eq 1 3}}1{{else if eq 2 3}}2{{else if eq 3 3}}3{{end}}",
        &Value::Nil,
        "3",
    );
}

// Print / Printf / Println
#[test]
fn test_print() {
    ok(r#"{{print "hello, print"}}"#, &Value::Nil, "hello, print");
}

#[test]
fn test_printf_string() {
    ok(r#"{{printf "%s" "hello"}}"#, &Value::Nil, "hello");
}

#[test]
fn test_printf_int() {
    ok(r#"{{printf "%d" 127}}"#, &Value::Nil, "127");
}

#[test]
fn test_printf_field() {
    let data = tmap! { "U" => tmap! { "V" => "v" } };
    ok(r#"{{printf "%s" .U.V}}"#, &data, "v");
}

#[test]
fn test_printf_dot() {
    let data = tmap! { "I" => 17i64 };
    ok(r#"{{with .I}}{{printf "%d" .}}{{end}}"#, &data, "17");
}

#[test]
fn test_printf_var() {
    let data = tmap! { "I" => 17i64 };
    ok(r#"{{with $x := .I}}{{printf "%d" $x}}{{end}}"#, &data, "17");
}

#[test]
fn test_printf_multiple_fields() {
    let data = tmap! { "Name" => "Alice", "Age" => 30i64 };
    ok(
        r#"{{printf "%s is %d years old" .Name .Age}}"#,
        &data,
        "Alice is 30 years old",
    );
}

#[test]
fn test_printf_multiple_vars() {
    let data = tmap! { "Name" => "Alice", "Age" => 30i64 };
    ok(
        r#"{{$n := .Name}}{{$a := .Age}}{{printf "%s is %d" $n $a}}"#,
        &data,
        "Alice is 30",
    );
}

#[test]
fn test_printf_multiple_dots() {
    ok(r#"{{printf "%v %v" . .}}"#, &Value::Int(7), "7 7");
}

#[test]
fn test_printf_multiple_paren_fields() {
    let data = tmap! { "Name" => "Alice", "Age" => 30i64 };
    ok(
        r#"{{printf "%s is %d" (.Name) (.Age)}}"#,
        &data,
        "Alice is 30",
    );
}

#[test]
fn test_printf_three_fields() {
    let data = tmap! { "A" => "x", "B" => "y", "C" => "z" };
    ok(r#"{{printf "%s-%s-%s" .A .B .C}}"#, &data, "x-y-z");
}

#[test]
fn test_printf_var_then_field() {
    let data = tmap! { "Name" => "Alice", "Age" => 30i64 };
    ok(
        r#"{{$n := .Name}}{{printf "%s is %d" $n .Age}}"#,
        &data,
        "Alice is 30",
    );
}

#[test]
fn test_printf_paren_then_field() {
    let data = tmap! { "Name" => "Alice", "Age" => 30i64 };
    ok(
        r#"{{printf "%s is %d" (.Name) .Age}}"#,
        &data,
        "Alice is 30",
    );
}

#[test]
fn test_printf_paren_chained_field_then_field() {
    let data = tmap! {
        "U" => tmap! { "V" => "inner" },
        "Other" => "outer"
    };
    ok(r#"{{printf "%s-%s" (.U).V .Other}}"#, &data, "inner-outer");
}

#[test]
fn test_adjacent_field_chain_still_works() {
    let data = tmap! {
        "U" => tmap! { "V" => tmap! { "W" => "deep" } }
    };
    ok(r#"{{printf "%s" .U.V.W}}"#, &data, "deep");
}

// HTML
#[test]
fn test_html_escape() {
    ok(
        r#"{{html "<script>alert(\"XSS\");</script>"}}"#,
        &Value::Nil,
        "&lt;script&gt;alert(&#34;XSS&#34;);&lt;/script&gt;",
    );
}

#[test]
fn test_html_pipeline() {
    ok(
        r#"{{printf "<script>alert(\"XSS\");</script>" | html}}"#,
        &Value::Nil,
        "&lt;script&gt;alert(&#34;XSS&#34;);&lt;/script&gt;",
    );
}

// JavaScript
#[test]
fn test_js_escape() {
    ok(
        r#"{{js .}}"#,
        &Value::String("It'd be nice.".into()),
        r"It\'d be nice.",
    );
}

// URL query
#[test]
fn test_urlquery() {
    ok(
        r#"{{"http://www.example.org/" | urlquery}}"#,
        &Value::Nil,
        "http%3A%2F%2Fwww.example.org%2F",
    );
}

// Booleans: not, and, or
#[test]
fn test_not() {
    ok("{{not true}} {{not false}}", &Value::Nil, "false true");
}

#[test]
fn test_and() {
    ok(
        "{{and false 0}} {{and 1 0}} {{and 0 true}} {{and 1 1}}",
        &Value::Nil,
        "false 0 0 1",
    );
}

#[test]
fn test_or() {
    ok(
        "{{or 0 0}} {{or 1 0}} {{or 0 true}} {{or 1 1}}",
        &Value::Nil,
        "0 1 true 1",
    );
}

#[test]
fn test_and_pipe_true() {
    ok("{{1 | and 1}}", &Value::Nil, "1");
}

#[test]
fn test_and_pipe_false() {
    ok("{{0 | and 1}}", &Value::Nil, "0");
}

#[test]
fn test_or_pipe_true() {
    ok("{{1 | or 0}}", &Value::Nil, "1");
}

#[test]
fn test_or_pipe_false() {
    ok("{{0 | or 0}}", &Value::Nil, "0");
}

#[test]
fn test_boolean_if() {
    ok(
        r#"{{if and true 1 `hi`}}TRUE{{else}}FALSE{{end}}"#,
        &Value::Nil,
        "TRUE",
    );
}

#[test]
fn test_boolean_if_not() {
    ok(
        r#"{{if and true 1 `hi` | not}}TRUE{{else}}FALSE{{end}}"#,
        &Value::Nil,
        "FALSE",
    );
}

#[test]
fn test_boolean_if_pipe() {
    ok(
        "{{if true | not | and 1}}TRUE{{else}}FALSE{{end}}",
        &Value::Nil,
        "FALSE",
    );
}

// Indexing
#[test]
fn test_slice_index_0() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok("{{index .SI 0}}", &data, "3");
}

#[test]
fn test_slice_index_1() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok("{{index .SI 1}}", &data, "4");
}

#[test]
fn test_map_index() {
    let data = tmap! {
        "MSI" => tmap! { "one" => 1i64, "two" => 2i64 }
    };
    ok(r#"{{index .MSI "one"}}"#, &data, "1");
    ok(r#"{{index .MSI "two"}}"#, &data, "2");
}

#[test]
fn test_index_nil_fails() {
    fail("{{index nil 1}}", &Value::Nil);
}

// Slicing
#[test]
fn test_slice_1() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok("{{slice .SI 1}}", &data, "[4 5]");
}

#[test]
fn test_slice_1_2() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok("{{slice .SI 1 2}}", &data, "[4]");
}

#[test]
fn test_string_slice() {
    let data = tmap! { "S" => "xyz" };
    ok("{{slice .S 0 1}}", &data, "x");
    ok("{{slice .S 1}}", &data, "yz");
    ok("{{slice .S 1 2}}", &data, "y");
}

#[test]
fn test_slice_three_index_list() {
    let data = tmap! { "SI" => vec![1i64, 2, 3] };
    ok("{{slice .SI 1 3 3}}", &data, "[2 3]");
}

#[test]
fn test_slice_three_index_negative_fails() {
    let data = tmap! { "SI" => vec![1i64, 2, 3] };
    fail("{{slice .SI 1 2 -1}}", &data);
}

#[test]
fn test_slice_three_index_inverted_fails() {
    let data = tmap! { "SI" => vec![1i64, 2, 3] };
    fail("{{slice .SI 2 2 1}}", &data);
}

#[test]
fn test_slice_three_index_string_fails() {
    let data = tmap! { "S" => "xyz" };
    fail("{{slice .S 1 2 2}}", &data);
}

#[test]
fn test_slice_negative_start_fails() {
    let data = tmap! { "SI" => vec![1i64, 2, 3] };
    fail("{{slice .SI -1}}", &data);
    fail("{{slice .SI -1 2}}", &data);
}

#[test]
fn test_slice_negative_end_fails() {
    let data = tmap! { "SI" => vec![1i64, 2, 3] };
    fail("{{slice .SI 0 -1}}", &data);
}

#[test]
fn test_slice_out_of_bounds_fails() {
    let data = tmap! { "SI" => vec![1i64, 2, 3] };
    fail("{{slice .SI 0 10}}", &data);
    fail("{{slice .SI 5 6}}", &data);
}

#[test]
fn test_slice_start_gt_end_fails() {
    let data = tmap! { "SI" => vec![1i64, 2, 3] };
    fail("{{slice .SI 2 1}}", &data);
}

#[test]
fn test_string_slice_mid_char_utf8_fails() {
    // "é" is 2 bytes in UTF-8 (0xC3 0xA9); slicing at byte 1 is not on a
    // character boundary and must error, not panic.
    let data = tmap! { "S" => "café" };
    fail("{{slice .S 0 4}}", &data); // 4 is inside 'é'
}

// Len
#[test]
fn test_len_slice() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok("{{len .SI}}", &data, "3");
}

#[test]
fn test_len_map() {
    let data = tmap! {
        "MSI" => tmap! { "one" => 1i64, "two" => 2i64, "three" => 3i64 }
    };
    ok("{{len .MSI}}", &data, "3");
}

// With
#[test]
fn test_with_true() {
    ok("{{with true}}{{.}}{{end}}", &Value::Nil, "true");
}

#[test]
fn test_with_false() {
    ok(
        "{{with false}}{{.}}{{else}}FALSE{{end}}",
        &Value::Nil,
        "FALSE",
    );
}

#[test]
fn test_with_1() {
    ok("{{with 1}}{{.}}{{else}}ZERO{{end}}", &Value::Nil, "1");
}

#[test]
fn test_with_0() {
    ok("{{with 0}}{{.}}{{else}}ZERO{{end}}", &Value::Nil, "ZERO");
}

#[test]
fn test_with_1_5() {
    ok("{{with 1.5}}{{.}}{{else}}ZERO{{end}}", &Value::Nil, "1.5");
}

#[test]
fn test_with_empty_string() {
    ok("{{with ``}}{{.}}{{else}}EMPTY{{end}}", &Value::Nil, "EMPTY");
}

#[test]
fn test_with_string() {
    ok(
        "{{with `notempty`}}{{.}}{{else}}EMPTY{{end}}",
        &Value::Nil,
        "notempty",
    );
}

#[test]
fn test_with_empty_slice() {
    let data = tmap! { "SIEmpty" => Vec::<i64>::new() };
    ok("{{with .SIEmpty}}{{.}}{{else}}EMPTY{{end}}", &data, "EMPTY");
}

#[test]
fn test_with_slice() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok("{{with .SI}}{{.}}{{else}}EMPTY{{end}}", &data, "[3 4 5]");
}

#[test]
fn test_with_dollar_x_int() {
    let data = tmap! { "I" => 17i64 };
    ok("{{with $x := .I}}{{$x}}{{end}}", &data, "17");
}

#[test]
fn test_with_variable_and_action() {
    let data = tmap! { "U" => tmap! { "V" => "v" } };
    ok("{{with $x := $}}{{$y := $.U.V}}{{$y}}{{end}}", &data, "v");
}

#[test]
fn test_with_else_with() {
    ok(
        "{{with 0}}{{.}}{{else with true}}{{.}}{{end}}",
        &Value::Nil,
        "true",
    );
}

#[test]
fn test_with_else_with_chain() {
    ok(
        "{{with 0}}{{.}}{{else with false}}{{.}}{{else with `notempty`}}{{.}}{{end}}",
        &Value::Nil,
        "notempty",
    );
}

// Range
#[test]
fn test_range_list() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok("{{range .SI}}-{{.}}-{{end}}", &data, "-3--4--5-");
}

#[test]
fn test_range_empty_no_else() {
    let data = tmap! { "SIEmpty" => Vec::<i64>::new() };
    ok("{{range .SIEmpty}}-{{.}}-{{end}}", &data, "");
}

#[test]
fn test_range_list_else() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok(
        "{{range .SI}}-{{.}}-{{else}}EMPTY{{end}}",
        &data,
        "-3--4--5-",
    );
}

#[test]
fn test_range_empty_else() {
    let data = tmap! { "SIEmpty" => Vec::<i64>::new() };
    ok(
        "{{range .SIEmpty}}-{{.}}-{{else}}EMPTY{{end}}",
        &data,
        "EMPTY",
    );
}

#[test]
fn test_range_dollar_x() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok("{{range $x := .SI}}<{{$x}}>{{end}}", &data, "<3><4><5>");
}

#[test]
fn test_range_dollar_x_dollar_y() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok(
        "{{range $x, $y := .SI}}<{{$x}}={{$y}}>{{end}}",
        &data,
        "<0=3><1=4><2=5>",
    );
}

#[test]
fn test_range_map_dollar_x_dollar_y() {
    let data = tmap! {
        "MSIone" => tmap! { "one" => 1i64 }
    };
    ok(
        "{{range $x, $y := .MSIone}}<{{$x}}={{$y}}>{{end}}",
        &data,
        "<one=1>",
    );
}

#[test]
fn test_range_count() {
    ok(
        r#"{{range $i, $x := count 5}}[{{$i}}]{{$x}}{{end}}"#,
        &Value::Nil,
        "[0]a[1]b[2]c[3]d[4]e",
    );
}

#[test]
fn test_range_nil_count() {
    ok(
        r#"{{range $i, $x := count 0}}{{else}}empty{{end}}"#,
        &Value::Nil,
        "empty",
    );
}

// Pipelines
#[test]
fn test_pipeline_printf() {
    ok(r#"{{"output" | printf "%s"}}"#, &Value::Nil, "output");
}

#[test]
fn test_pipeline_chain() {
    ok(
        r#"{{"output" | printf "%s" | printf "%s"}}"#,
        &Value::Nil,
        "output",
    );
}

// Parenthesized expressions
#[test]
fn test_parens_in_pipeline() {
    ok(
        r#"{{printf "%d %d %d" (1) (2 | add 3) (add 4 (add 5 6))}}"#,
        &Value::Nil,
        "1 5 15",
    );
}

// Comparison operators
#[test]
fn test_eq_true() {
    ok("{{eq true true}}", &Value::Nil, "true");
}

#[test]
fn test_eq_false() {
    ok("{{eq true false}}", &Value::Nil, "false");
}

#[test]
fn test_eq_float() {
    ok("{{eq 1.5 1.5}}", &Value::Nil, "true");
    ok("{{eq 1.5 2.5}}", &Value::Nil, "false");
}

#[test]
fn test_eq_int() {
    ok("{{eq 1 1}}", &Value::Nil, "true");
    ok("{{eq 1 2}}", &Value::Nil, "false");
}

#[test]
fn test_eq_string() {
    ok(r#"{{eq "xy" "xy"}}"#, &Value::Nil, "true");
    ok(r#"{{eq "xy" "xyz"}}"#, &Value::Nil, "false");
}

#[test]
fn test_eq_multi_arg() {
    // eq with multiple args: eq 3 4 5 6 3 => true (3==3)
    ok("{{eq 3 4 5 6 3}}", &Value::Nil, "true");
    ok("{{eq 3 4 5 6 7}}", &Value::Nil, "false");
}

#[test]
fn test_ne() {
    ok("{{ne true true}}", &Value::Nil, "false");
    ok("{{ne true false}}", &Value::Nil, "true");
    ok("{{ne 1.5 1.5}}", &Value::Nil, "false");
    ok("{{ne 1.5 2.5}}", &Value::Nil, "true");
    ok("{{ne 1 1}}", &Value::Nil, "false");
    ok("{{ne 1 2}}", &Value::Nil, "true");
    ok(r#"{{ne "xy" "xy"}}"#, &Value::Nil, "false");
    ok(r#"{{ne "xy" "xyz"}}"#, &Value::Nil, "true");
}

#[test]
fn test_lt() {
    ok("{{lt 1.5 1.5}}", &Value::Nil, "false");
    ok("{{lt 1.5 2.5}}", &Value::Nil, "true");
    ok("{{lt 1 1}}", &Value::Nil, "false");
    ok("{{lt 1 2}}", &Value::Nil, "true");
    ok(r#"{{lt "xy" "xy"}}"#, &Value::Nil, "false");
    ok(r#"{{lt "xy" "xyz"}}"#, &Value::Nil, "true");
}

#[test]
fn test_le() {
    ok("{{le 1.5 1.5}}", &Value::Nil, "true");
    ok("{{le 1.5 2.5}}", &Value::Nil, "true");
    ok("{{le 2.5 1.5}}", &Value::Nil, "false");
    ok("{{le 1 1}}", &Value::Nil, "true");
    ok("{{le 1 2}}", &Value::Nil, "true");
    ok("{{le 2 1}}", &Value::Nil, "false");
    ok(r#"{{le "xy" "xy"}}"#, &Value::Nil, "true");
    ok(r#"{{le "xy" "xyz"}}"#, &Value::Nil, "true");
    ok(r#"{{le "xyz" "xy"}}"#, &Value::Nil, "false");
}

#[test]
fn test_gt() {
    ok("{{gt 1.5 1.5}}", &Value::Nil, "false");
    ok("{{gt 1.5 2.5}}", &Value::Nil, "false");
    ok("{{gt 1 1}}", &Value::Nil, "false");
    ok("{{gt 2 1}}", &Value::Nil, "true");
    ok("{{gt 1 2}}", &Value::Nil, "false");
    ok(r#"{{gt "xy" "xy"}}"#, &Value::Nil, "false");
    ok(r#"{{gt "xy" "xyz"}}"#, &Value::Nil, "false");
}

#[test]
fn test_ge() {
    ok("{{ge 1.5 1.5}}", &Value::Nil, "true");
    ok("{{ge 1.5 2.5}}", &Value::Nil, "false");
    ok("{{ge 2.5 1.5}}", &Value::Nil, "true");
    ok("{{ge 1 1}}", &Value::Nil, "true");
    ok("{{ge 1 2}}", &Value::Nil, "false");
    ok("{{ge 2 1}}", &Value::Nil, "true");
    ok(r#"{{ge "xy" "xy"}}"#, &Value::Nil, "true");
    ok(r#"{{ge "xy" "xyz"}}"#, &Value::Nil, "false");
    ok(r#"{{ge "xyz" "xy"}}"#, &Value::Nil, "true");
}

// Or as if
#[test]
fn test_or_as_if_true() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok(r#"{{or .SI "slice is empty"}}"#, &data, "[3 4 5]");
}

#[test]
fn test_or_as_if_false() {
    let data = tmap! { "SIEmpty" => Vec::<i64>::new() };
    ok(
        r#"{{or .SIEmpty "slice is empty"}}"#,
        &data,
        "slice is empty",
    );
}

// Nested assignment
#[test]
fn test_nested_assignment() {
    ok(
        "{{$x := 2}}{{if true}}{{$x = 3}}{{end}}{{$x}}",
        &Value::Nil,
        "3",
    );
}

// Define and template
#[test]
fn test_define_and_template() {
    let data = Value::String("hello".into());
    ok(
        r#"{{define "greeting"}}Hello, {{.}}!{{end}}{{template "greeting" .}}"#,
        &data,
        "Hello, hello!",
    );
}

// Block
#[test]
fn test_block() {
    let data = Value::String("hello".into());
    ok(
        r#"a({{block "inner" .}}bar({{.}})baz{{end}})b"#,
        &data,
        "a(bar(hello)baz)b",
    );
}

// Custom delimiters
#[test]
fn test_delims() {
    let data = tmap! { "Str" => "Hello, world" };
    let result = Template::new("delims")
        .delims("<<", ">>")
        .parse("<<.Str>>")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(result, "Hello, world");
}

#[test]
fn test_delims_pipe() {
    let data = tmap! { "Str" => "Hello, world" };
    let result = Template::new("delims")
        .delims("|", "|")
        .parse("|.Str|")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(result, "Hello, world");
}

// Trim markers
#[test]
fn test_trim_left() {
    ok("  {{- .}}", &Value::String("hello".into()), "hello");
}

#[test]
fn test_trim_right() {
    ok("{{. -}}  ", &Value::String("hello".into()), "hello");
}

#[test]
fn test_trim_both() {
    ok("  {{- . -}}  ", &Value::String("hello".into()), "hello");
}

#[test]
fn test_trim_newlines() {
    ok(
        "A \n\t {{- . -}} \n\t B",
        &Value::String("hello".into()),
        "AhelloB",
    );
}

// Bug fixes (from Go's exec_test.go)
#[test]
fn test_bug4_nil_in_if() {
    // Nil interface values in if.
    let data = tmap! { "Empty0" => Value::Nil };
    ok("{{if .Empty0}}non-nil{{else}}nil{{end}}", &data, "nil");
}

#[test]
fn test_bug9_lowercase_map_key() {
    // A bug broke map lookups for lower-case names.
    let mut m: alloc::collections::BTreeMap<String, Value> = alloc::collections::BTreeMap::new();
    m.insert("cause".to_string(), Value::String("neglect".into()));
    ok("{{.cause}}", &m.into(), "neglect");
}

// Pipelines with print functions
#[test]
fn test_final_for_printf() {
    // "x" | printf should work (piped arg becomes the format arg)
    ok(r#"{{"x" | printf "%s"}}"#, &Value::Nil, "x");
}

#[test]
fn test_twoargs_pipe() {
    ok(
        r#"{{"aaa" | twoArgs "bbb"}}"#,
        &Value::Nil,
        "twoArgs=bbbaaa",
    );
}

#[test]
fn test_onearg_pipe() {
    ok(r#"{{"aaa" | oneArg}}"#, &Value::Nil, "oneArg=aaa");
}

// Makemap function
#[test]
fn test_makemap() {
    ok(
        r#"{{(makemap "up" "down" "left" "right").left}}"#,
        &Value::Nil,
        "right",
    );
}

// MapOfThree
#[test]
fn test_map_of_three() {
    ok("{{mapOfThree.three}}", &Value::Nil, "3");
}

// Complex nested templates
#[test]
fn test_nested_define_and_template() {
    let data = tmap! { "Name" => "World" };
    ok(
        r#"{{define "base"}}<html>{{template "body" .}}</html>{{end}}{{define "body"}}<p>{{.Name}}</p>{{end}}{{template "base" .}}"#,
        &data,
        "<html><p>World</p></html>",
    );
}

// Range with map, checking that dot is set correctly
#[test]
fn test_range_map_values() {
    let data = tmap! {
        "MSIone" => tmap! { "one" => 1i64 }
    };
    ok("{{range .MSIone}}-{{.}}-{{end}}", &data, "-1-");
}

// Pipeline chaining with len
#[test]
fn test_pipeline_len() {
    let data = tmap! { "Items" => vec!["a".to_string(), "bb".to_string(), "ccc".to_string()] };
    ok(r#"{{.Items | len | printf "%d items"}}"#, &data, "3 items");
}

// Nested if
#[test]
fn test_nested_if() {
    let data = tmap! { "A" => true, "B" => true };
    ok("{{if .A}}{{if .B}}both{{end}}{{end}}", &data, "both");
}

// Dollar in range
#[test]
fn test_dollar_in_range() {
    let data = tmap! {
        "Name" => "outer",
        "Items" => vec!["inner".to_string()],
    };
    // $ refers to the top-level data
    ok(
        "{{range .Items}}{{$.Name}}:{{.}}{{end}}",
        &data,
        "outer:inner",
    );
}

// Deep nesting
#[test]
fn test_deeply_nested_field() {
    let data = tmap! {
        "A" => tmap! {
            "B" => tmap! {
                "C" => tmap! {
                    "D" => "deep"
                }
            }
        }
    };
    ok("{{.A.B.C.D}}", &data, "deep");
}

// Declare in range
#[test]
fn test_declare_in_range() {
    let data = tmap! { "PSI" => vec![21i64, 22, 23] };
    ok(
        "{{range $x := .PSI}}<{{$foo := $x}}{{$x}}>{{end}}",
        &data,
        "<21><22><23>",
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// NEW TESTS: covering all fixes from the review
// ═══════════════════════════════════════════════════════════════════════════

// Fix #1: print spacing (Go's fmt.Sprint behavior)
#[test]
fn test_print_two_ints_space() {
    // Go's fmt.Sprint adds spaces between non-string adjacent operands
    ok("{{print 1 2}}", &Value::Nil, "1 2");
}

#[test]
fn test_print_string_int_no_space() {
    // String adjacent to non-string: no space
    ok(r#"{{print "x" 1}}"#, &Value::Nil, "x1");
}

#[test]
fn test_print_int_string_no_space() {
    ok(r#"{{print 1 "x"}}"#, &Value::Nil, "1x");
}

#[test]
fn test_print_strings_no_space() {
    ok(r#"{{print "a" "b"}}"#, &Value::Nil, "ab");
}

#[test]
fn test_print_multi_ints() {
    ok("{{print 1 2 3}}", &Value::Nil, "1 2 3");
}

// Fix #3: printf %f with 6 decimal places
#[test]
fn test_printf_float_default() {
    ok(r#"{{printf "%f" 1.5}}"#, &Value::Nil, "1.500000");
}

#[test]
fn test_printf_float_precision() {
    ok(r#"{{printf "%.2f" 1.5}}"#, &Value::Nil, "1.50");
}

#[test]
fn test_printf_float_zero() {
    ok(r#"{{printf "%f" 0.0}}"#, &Value::Nil, "0.000000");
}

// Fix #4: call builtin
#[test]
fn test_call_nil_errors() {
    let data = tmap! { "F" => Value::Nil };
    let result = Template::new("test")
        .parse("{{call .F}}")
        .unwrap()
        .execute_to_string(&data);
    assert!(result.is_err());
}

// Fix #5: JS escape includes =
#[test]
fn test_js_escape_equals() {
    ok(r#"{{js "a=b"}}"#, &Value::Nil, r"a\u003Db");
}

#[test]
fn test_js_escape_control_chars() {
    ok(r#"{{js "\t\n"}}"#, &Value::Nil, r"\u0009\u000A");
}

// Fix #6: HTML escape NUL byte
#[test]
fn test_html_nul_byte() {
    let data = Value::String("a\0b".into());
    ok("{{html .}}", &data, "a\u{FFFD}b");
}

// Fix #7: missingkey option
#[test]
fn test_missingkey_default_returns_nil() {
    let data = tmap! { "X" => 1i64 };
    ok("{{.Y}}", &data, "<no value>");
}

// Fix #10: and/or short-circuit
#[test]
fn test_or_short_circuit_basic() {
    // or should return first truthy value without evaluating the rest
    ok(r#"{{or 1 0}}"#, &Value::Nil, "1");
}

#[test]
fn test_and_short_circuit_basic() {
    // and should return first falsy value without evaluating the rest
    ok(r#"{{and 0 1}}"#, &Value::Nil, "0");
}

#[test]
fn test_or_all_falsy() {
    ok(r#"{{or 0 false ""}}"#, &Value::Nil, "");
}

#[test]
fn test_and_all_truthy() {
    ok(r#"{{and 1 true "x"}}"#, &Value::Nil, "x");
}

// Fix #11: comments
#[test]
fn test_comment_simple() {
    ok("hello{{/* comment */}} world", &Value::Nil, "hello world");
}

#[test]
fn test_comment_only() {
    ok("{{/* nothing here */}}", &Value::Nil, "");
}

#[test]
fn test_comment_multiline() {
    ok("A{{/* multi\nline\ncomment */}}B", &Value::Nil, "AB");
}

#[test]
fn test_comment_trim_left() {
    ok(
        "hello  {{- /* comment */ -}}  world",
        &Value::Nil,
        "helloworld",
    );
}

#[test]
fn test_comment_between_actions() {
    let data = tmap! { "X" => "a", "Y" => "b" };
    ok("{{.X}}{{/* sep */}}{{.Y}}", &data, "ab");
}

// Fix #12: break/continue in range
#[test]
fn test_range_break() {
    let data = tmap! { "SI" => vec![1i64, 2, 3, 4, 5] };
    ok(
        "{{range .SI}}{{if eq . 3}}{{break}}{{end}}{{.}} {{end}}",
        &data,
        "1 2 ",
    );
}

#[test]
fn test_range_continue() {
    let data = tmap! { "SI" => vec![1i64, 2, 3, 4, 5] };
    ok(
        "{{range .SI}}{{if eq . 3}}{{continue}}{{end}}{{.}} {{end}}",
        &data,
        "1 2 4 5 ",
    );
}

#[test]
fn test_range_break_else() {
    // break should not trigger the else branch
    let data = tmap! { "SI" => vec![1i64, 2, 3] };
    ok(
        "{{range .SI}}{{if eq . 2}}{{break}}{{end}}{{.}} {{else}}empty{{end}}",
        &data,
        "1 ",
    );
}

#[test]
fn test_range_continue_else() {
    let data = tmap! { "SI" => vec![1i64, 2, 3] };
    ok(
        "{{range .SI}}{{if eq . 2}}{{continue}}{{end}}{{.}} {{else}}empty{{end}}",
        &data,
        "1 3 ",
    );
}

#[test]
fn test_range_break_in_map() {
    // BTreeMap iteration is sorted, so we know the order
    let data = tmap! {
        "M" => tmap! { "a" => 1i64, "b" => 2i64, "c" => 3i64 }
    };
    let result = run(
        "{{range $k, $v := .M}}{{if eq $k \"b\"}}{{break}}{{end}}{{$k}}={{$v}} {{end}}",
        &data,
    )
    .unwrap();
    assert_eq!(result, "a=1 ");
}

// Fix #13: hex/octal/binary number literals
#[test]
fn test_hex_literal() {
    ok("{{0xFF}}", &Value::Nil, "255");
}

#[test]
fn test_hex_literal_upper() {
    ok("{{0XFF}}", &Value::Nil, "255");
}

#[test]
fn test_octal_literal() {
    ok("{{0o77}}", &Value::Nil, "63");
}

#[test]
fn test_octal_literal_upper() {
    ok("{{0O77}}", &Value::Nil, "63");
}

#[test]
fn test_binary_literal() {
    ok("{{0b1010}}", &Value::Nil, "10");
}

#[test]
fn test_binary_literal_upper() {
    ok("{{0B1010}}", &Value::Nil, "10");
}

#[test]
fn test_underscore_separator() {
    ok("{{1_000_000}}", &Value::Nil, "1000000");
}

#[test]
fn test_decimal_underscore() {
    ok("{{1_000}}", &Value::Nil, "1000");
}

// Fix #14: char literal with escapes
#[test]
fn test_char_literal() {
    // 'a' = 97
    ok("{{'a'}}", &Value::Nil, "97");
}

#[test]
fn test_char_literal_newline() {
    // '\n' = 10
    ok("{{printf \"%d\" '\\n'}}", &Value::Nil, "10");
}

// Additional Go test coverage: range over nil
#[test]
fn test_range_nil() {
    ok("{{range .X}}{{.}}{{else}}empty{{end}}", &tmap! {}, "empty");
}

// Additional Go test coverage: range over integer
#[test]
fn test_range_int() {
    ok("{{range $i := 5}}{{$i}} {{end}}", &Value::Nil, "0 1 2 3 4 ");
}

#[test]
fn test_range_int_zero() {
    ok("{{range 0}}x{{else}}empty{{end}}", &Value::Nil, "empty");
}

// Additional Go test coverage: nil pipeline
#[test]
fn test_nil_pipeline() {
    let data = tmap! {};
    ok("{{$x := .Missing}}{{$x}}", &data, "<no value>");
}

// Additional Go test coverage: template with no args
#[test]
fn test_template_no_args() {
    ok(
        r#"{{define "hi"}}hello{{end}}{{template "hi"}}"#,
        &Value::Nil,
        "hello",
    );
}

// Additional Go test coverage: empty define
#[test]
fn test_empty_define() {
    ok(
        r#"{{define "empty"}}{{end}}[{{template "empty"}}]"#,
        &Value::Nil,
        "[]",
    );
}

// Additional Go test coverage: nested variable scope isolation
#[test]
fn test_variable_scope_isolation() {
    // $x declared inside if should not leak out
    ok(
        "{{$x := 1}}{{if true}}{{$x := 2}}{{$x}}{{end}}{{$x}}",
        &Value::Nil,
        "21",
    );
}

// Additional Go test coverage: println
#[test]
fn test_println() {
    ok(
        r#"{{println "hello" "world"}}"#,
        &Value::Nil,
        "hello world\n",
    );
}

#[test]
fn test_println_empty() {
    ok("{{println}}", &Value::Nil, "\n");
}

// Additional Go test coverage: printf additional verbs
#[test]
fn test_printf_v() {
    ok(r#"{{printf "%v" 42}}"#, &Value::Nil, "42");
}

#[test]
fn test_printf_q() {
    ok(r#"{{printf "%q" "hello"}}"#, &Value::Nil, r#""hello""#);
}

#[test]
fn test_printf_x() {
    ok(r#"{{printf "%x" 255}}"#, &Value::Nil, "ff");
}

#[test]
fn test_printf_o() {
    ok(r#"{{printf "%o" 8}}"#, &Value::Nil, "10");
}

// Coverage for the in-place sprintf rewrite. These templates exercise the
// hot paths (`pad_in_place`, `apply_float_sign_in_place`, `write_signed`,
// `write_display_truncated`, `format_int_base_into`, the `%q` writers)
// whose semantics must stay byte-for-byte identical to Go's `fmt.Sprintf`.
// Each case is covered by `ok()` so it's automatically cross-checked when
// `--features go-crosscheck` is enabled.
#[test]
fn test_printf_d_plus_zero_pad_negative() {
    ok(r#"{{printf "%+06d" -42}}"#, &Value::Nil, "-00042");
}

#[test]
fn test_printf_d_zero_pad_positive() {
    ok(r#"{{printf "%06d" 42}}"#, &Value::Nil, "000042");
}

#[test]
fn test_printf_d_zero_pad_with_plus_positive() {
    ok(r#"{{printf "%+06d" 42}}"#, &Value::Nil, "+00042");
}

#[test]
fn test_printf_d_space_sign() {
    ok(r#"{{printf "% d" 42}}"#, &Value::Nil, " 42");
}

#[test]
fn test_printf_d_space_sign_negative() {
    ok(r#"{{printf "% d" -42}}"#, &Value::Nil, "-42");
}

#[test]
fn test_printf_d_left_align() {
    ok(r#"{{printf "[%-6d]" 42}}"#, &Value::Nil, "[42    ]");
}

#[test]
fn test_printf_s_left_align() {
    ok(r#"{{printf "[%-6s]" "hi"}}"#, &Value::Nil, "[hi    ]");
}

#[test]
fn test_printf_s_right_align() {
    ok(r#"{{printf "[%6s]" "hi"}}"#, &Value::Nil, "[    hi]");
}

#[test]
fn test_printf_s_precision_truncate() {
    ok(r#"{{printf "%.3s" "hello"}}"#, &Value::Nil, "hel");
}

#[test]
fn test_printf_s_precision_multibyte() {
    // Precision counts Unicode scalars, not bytes; "café" → "caf".
    ok(r#"{{printf "%.3s" "café"}}"#, &Value::Nil, "caf");
}

#[test]
fn test_printf_s_left_align_with_precision() {
    ok(r#"{{printf "[%-6.3s]" "hello"}}"#, &Value::Nil, "[hel   ]");
}

#[test]
fn test_printf_f_plus_negative() {
    ok(r#"{{printf "%+f" -1.5}}"#, &Value::Nil, "-1.500000");
}

#[test]
fn test_printf_f_space_sign() {
    ok(r#"{{printf "% f" 1.5}}"#, &Value::Nil, " 1.500000");
}

#[test]
fn test_printf_f_precision_zero_rounds() {
    ok(r#"{{printf "%.0f" 1.5}}"#, &Value::Nil, "2");
}

#[test]
fn test_printf_e_width() {
    ok(
        r#"{{printf "[%16e]" 1.5}}"#,
        &Value::Nil,
        "[    1.500000e+00]",
    );
}

#[test]
fn test_printf_e_plus_width_negative() {
    ok(
        r#"{{printf "[%+16e]" -1.5}}"#,
        &Value::Nil,
        "[   -1.500000e+00]",
    );
}

#[test]
fn test_printf_g_plus_negative() {
    ok(r#"{{printf "%+g" -3.5}}"#, &Value::Nil, "-3.5");
}

#[test]
fn test_printf_g_width() {
    ok(r#"{{printf "[%10g]" 1e7}}"#, &Value::Nil, "[     1e+07]");
}

// Coverage for the in-place `write_g_default` / `write_g_with_precision` /
// `write_normalized_sci` rewrite. Each test exercises a distinct branch of
// the new helpers and is cross-checked against Go via `ok()`.

#[test]
fn test_printf_g_negative_decimal() {
    ok(r#"{{printf "%g" -3.5}}"#, &Value::Nil, "-3.5");
}

#[test]
fn test_printf_g_negative_sci() {
    ok(r#"{{printf "%g" -1e7}}"#, &Value::Nil, "-1e+07");
}

#[test]
fn test_printf_g_negative_zero() {
    ok(r#"{{printf "%g" -0.0}}"#, &Value::Nil, "-0");
}

#[test]
fn test_printf_g_boundary_decimal() {
    // exp = 5 — stays in decimal branch.
    ok(r#"{{printf "%g" 100000.0}}"#, &Value::Nil, "100000");
}

#[test]
fn test_printf_g_boundary_sci() {
    // exp = 6 — switches to sci branch.
    ok(r#"{{printf "%g" 1000000.0}}"#, &Value::Nil, "1e+06");
}

#[test]
fn test_printf_g_uppercase_default() {
    ok(r#"{{printf "%G" 1e7}}"#, &Value::Nil, "1E+07");
}

#[test]
fn test_printf_g_uppercase_with_precision() {
    ok(r#"{{printf "%.4G" 123456.0}}"#, &Value::Nil, "1.235E+05");
}

#[test]
fn test_printf_g_precision_decimal_branch() {
    // exp = 1, prec = 2 → decimal branch (exp < prec).
    ok(r#"{{printf "%.2g" 10.0}}"#, &Value::Nil, "10");
}

#[test]
fn test_printf_g_precision_decimal_strip_zeros() {
    // exp = 0, prec = 6, f = 1.5 → decimal branch with trailing-zero strip.
    ok(r#"{{printf "%.6g" 1.5}}"#, &Value::Nil, "1.5");
}

#[test]
fn test_printf_g_precision_decimal_strip_to_int() {
    // f_prec = 5 → "1.00000" → trim → "1".
    ok(r#"{{printf "%.6g" 1.0}}"#, &Value::Nil, "1");
}

#[test]
fn test_printf_g_precision_sci_strip_zeros() {
    // exp = 5 >= prec = 4 → sci branch. Mantissa after strip becomes "1".
    ok(r#"{{printf "%.4g" 100000.0}}"#, &Value::Nil, "1e+05");
}

#[test]
fn test_printf_g_precision_sci_negative() {
    ok(r#"{{printf "%.4g" -123456.0}}"#, &Value::Nil, "-1.235e+05");
}

#[test]
fn test_printf_g_precision_zero() {
    ok(r#"{{printf "%.4g" 0.0}}"#, &Value::Nil, "0");
}

#[test]
fn test_printf_g_precision_small_exponent() {
    // exp = -2, prec = 2 → decimal branch with f_prec = 3 → "0.012".
    ok(r#"{{printf "%.2g" 0.0123}}"#, &Value::Nil, "0.012");
}

// %E uppercase coverage (write_normalized_sci with upper=true on a raw
// string that already contains 'E').
#[test]
fn test_printf_e_uppercase_default() {
    ok(r#"{{printf "%E" 1.5}}"#, &Value::Nil, "1.500000E+00");
}

#[test]
fn test_printf_e_uppercase_negative() {
    ok(r#"{{printf "%E" -1.5}}"#, &Value::Nil, "-1.500000E+00");
}

#[test]
fn test_printf_e_negative_zero() {
    ok(r#"{{printf "%e" -0.0}}"#, &Value::Nil, "-0.000000e+00");
}

#[test]
fn test_printf_e_precision_two() {
    ok(r#"{{printf "%.2e" 1.5}}"#, &Value::Nil, "1.50e+00");
}

#[test]
fn test_printf_e_precision_zero_rounds() {
    ok(r#"{{printf "%.0e" 1.5}}"#, &Value::Nil, "2e+00");
}

#[test]
fn test_printf_e_large_exponent() {
    // Two-digit exponent — exercises the no-pad path of write_normalized_sci.
    ok(r#"{{printf "%e" 1.5e100}}"#, &Value::Nil, "1.500000e+100");
}

#[test]
fn test_printf_e_negative_exponent() {
    ok(r#"{{printf "%e" 1.5e-100}}"#, &Value::Nil, "1.500000e-100");
}

#[test]
fn test_printf_q_width() {
    ok(r#"{{printf "[%8q]" "hi"}}"#, &Value::Nil, r#"[    "hi"]"#);
}

#[test]
fn test_printf_q_left_align() {
    ok(r#"{{printf "[%-8q]" "hi"}}"#, &Value::Nil, r#"["hi"    ]"#);
}

#[test]
fn test_printf_hash_q_backtick() {
    ok(r#"{{printf "%#q" "hello"}}"#, &Value::Nil, "`hello`");
}

#[test]
fn test_printf_hash_q_fallback_on_control() {
    ok(r#"{{printf "%#q" "a\nb"}}"#, &Value::Nil, r#""a\nb""#);
}

#[test]
fn test_printf_q_escapes_control_chars() {
    ok(r#"{{printf "%q" "a\tb"}}"#, &Value::Nil, r#""a\tb""#);
}

#[test]
fn test_printf_x_negative_with_hash() {
    ok(r#"{{printf "%#x" -255}}"#, &Value::Nil, "-0xff");
}

#[test]
fn test_printf_b_with_hash() {
    ok(r#"{{printf "%#b" 10}}"#, &Value::Nil, "0b1010");
}

#[test]
fn test_printf_o_with_hash() {
    ok(r#"{{printf "%#o" 8}}"#, &Value::Nil, "010");
}

#[test]
fn test_printf_x_string_precision_limits_input_bytes() {
    // Go: precision on `%.Nx` for strings limits *input bytes*, yielding
    // 2*N hex chars — not N hex chars.
    ok(r#"{{printf "%.2x" "abc"}}"#, &Value::Nil, "6162");
}

#[test]
fn test_printf_t_width() {
    ok(r#"{{printf "[%6t]" true}}"#, &Value::Nil, "[  true]");
}

#[test]
fn test_printf_v_width() {
    ok(r#"{{printf "[%6v]" 42}}"#, &Value::Nil, "[    42]");
}

#[test]
fn test_printf_c_unicode() {
    ok(r#"{{printf "%c" 65}}"#, &Value::Nil, "A");
}

#[test]
fn test_printf_multiple_verbs_mixed_text() {
    // Exercises the per-verb in-place writes interleaved with literal bytes.
    ok(
        r#"{{printf "id=%05d name=%s ratio=%.2f" 7 "alice" 0.137}}"#,
        &Value::Nil,
        "id=00007 name=alice ratio=0.14",
    );
}

#[test]
fn test_printf_percent_literal_between_verbs() {
    ok(
        r#"{{printf "%d%% of %s" 50 "users"}}"#,
        &Value::Nil,
        "50% of users",
    );
}

// Additional Go test coverage: eq with nil
#[test]
fn test_eq_nil() {
    ok("{{eq nil nil}}", &Value::Nil, "true");
}

#[test]
fn test_ne_nil() {
    ok("{{ne nil 1}}", &Value::Nil, "true");
}

// Additional Go test coverage: variable assignment in nested scope
#[test]
fn test_nested_assignment_in_range() {
    ok(
        "{{$x := 0}}{{range .SI}}{{$x = .}}{{end}}{{$x}}",
        &tmap! { "SI" => vec![1i64, 2, 3] },
        "3",
    );
}

// Range assignment form ($v = range ...)
#[test]
fn test_range_assign_single_var() {
    // $v is declared before the range; the range assignment form modifies it
    // in the outer scope. After the loop, $v holds the last element's value.
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok(
        "{{$v := 0}}{{range $v = .SI}}{{$v}}{{end}} {{$v}}",
        &data,
        "345 5",
    );
}

#[test]
fn test_range_assign_two_vars() {
    // Both $i and $v are declared before the range; the assignment form
    // modifies them in the outer scope. After the loop they hold the last
    // iteration's index and value.
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok(
        "{{$i := 0}}{{$v := 0}}{{range $i, $v = .SI}}{{$i}}:{{$v}} {{end}}{{$i}} {{$v}}",
        &data,
        "0:3 1:4 2:5 2 5",
    );
}

#[test]
fn test_range_assign_map() {
    // Assignment form over a map: $k and $v are modified in the outer scope.
    let data = tmap! { "MSI" => tmap! { "one" => 1i64, "two" => 2i64 } };
    ok(
        r#"{{$k := ""}}{{$v := 0}}{{range $k, $v = .MSI}}{{$k}}={{$v}} {{end}}{{$k}} {{$v}}"#,
        &data,
        // BTreeMap iteration is sorted: "one" < "two"
        "one=1 two=2 two 2",
    );
}

#[test]
fn test_range_assign_single_var_in_body() {
    // The assigned variable is readable both in the body and after the range.
    let data = tmap! { "SI" => vec![10i64, 20] };
    ok(
        "{{$x := 0}}{{range $x = .SI}}<{{$x}}>{{end}}after={{$x}}",
        &data,
        "<10><20>after=20",
    );
}

// Additional Go test coverage: else if with variable
#[test]
fn test_else_if_chain_complex() {
    ok(
        "{{if eq 1 2}}A{{else if eq 2 2}}B{{else}}C{{end}}",
        &Value::Nil,
        "B",
    );
}

// Additional: slice with 1 arg (whole slice)
#[test]
fn test_slice_whole() {
    let data = tmap! { "SI" => vec![1i64, 2, 3] };
    ok("{{slice .SI}}", &data, "[1 2 3]");
}

// Additional Go test coverage: custom delimiters with Unicode
#[test]
fn test_delims_angle_brackets() {
    let data = tmap! { "X" => "hello" };
    let result = Template::new("test")
        .delims("<<", ">>")
        .parse("<<.X>>")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(result, "hello");
}

#[test]
fn test_delims_unicode() {
    let data = tmap! { "X" => "hello" };
    let result = Template::new("test")
        .delims("[[", "]]")
        .parse("[[.X]]")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(result, "hello");
}

// Additional Go test coverage: JS escaping comprehensive
#[test]
fn test_js_escape_backslash() {
    ok(r#"{{js "a\\b"}}"#, &Value::Nil, r"a\\b");
}

#[test]
fn test_js_escape_quotes() {
    ok(r#"{{js "a\"b"}}"#, &Value::Nil, r#"a\"b"#);
}

#[test]
fn test_js_escape_angle_brackets() {
    ok(r#"{{js "<b>"}}"#, &Value::Nil, r"\u003Cb\u003E");
}

#[test]
fn test_js_escape_ampersand() {
    ok(r#"{{js "a&b"}}"#, &Value::Nil, r"a\u0026b");
}

// Additional Go test coverage: incompatible comparisons error
#[test]
fn test_eq_int_float() {
    fail("{{eq 1 1.0}}", &Value::Nil);
}

#[test]
fn test_lt_int_float() {
    fail("{{lt 1 1.5}}", &Value::Nil);
}

#[test]
fn test_gt_float_int() {
    fail("{{gt 2.5 2}}", &Value::Nil);
}

// Additional Go test coverage: index errors
#[test]
fn test_index_out_of_range() {
    // Go returns a template execution error on out-of-bounds list index.
    let data = tmap! { "SI" => vec![1i64, 2, 3] };
    fail("{{index .SI 99}}", &data);
}

// Additional Go test coverage: len of string
#[test]
fn test_len_string() {
    ok(r#"{{len "hello"}}"#, &Value::Nil, "5");
}

// Error cases
#[test]
fn test_len_of_int_fails() {
    fail("{{len 42}}", &Value::Nil);
}

#[test]
fn test_undefined_function_fails() {
    fail("{{noSuchFunc}}", &Value::Nil);
}

#[test]
fn test_undefined_template_fails() {
    fail(r#"{{template "nope"}}"#, &Value::Nil);
}

#[test]
fn test_field_on_non_map_fails() {
    fail("{{.X}}", &Value::Int(1));
}

#[test]
#[cfg(feature = "std")]
fn test_function_panic_is_exec_error() {
    let result = std::panic::catch_unwind(|| {
        Template::new("test")
            .func("boom", |_| panic!("boom panic"))
            .parse("{{boom}}")
            .unwrap()
            .execute_to_string(&Value::Nil)
    });

    match result {
        Ok(exec_result) => {
            let err = exec_result.expect_err("expected execution error");
            let s = err.to_string();
            assert!(s.contains("boom") && s.contains("panicked"), "got: {s}");
            assert!(s.contains("boom panic"), "got: {s}");
        }
        Err(_) => panic!("panic escaped template execution"),
    }
}

#[test]
fn test_unclosed_action_fails() {
    let result = Template::new("test").parse("{{.X");
    assert!(result.is_err());
}

#[test]
fn test_range_over_string_fails() {
    fail("{{range .S}}{{.}}{{end}}", &tmap! { "S" => "hello" });
}

#[test]
fn test_range_over_bool_fails() {
    fail("{{range .B}}{{.}}{{end}}", &tmap! { "B" => true });
}

// ═══════════════════════════════════════════════════════════════════════════
// Additional tests ported from Go's exec_test.go and multi_test.go
// ═══════════════════════════════════════════════════════════════════════════

// Go exec_test.go: trivial cases
#[test]
fn test_nil_action() {
    // Go: nil is not a command — bare {{nil}} errors
    fail("{{nil}}", &Value::Nil);
}

// Go exec_test.go: ideal constants
#[test]
fn test_ideal_int() {
    ok("{{3}}", &Value::Nil, "3");
}

#[test]
fn test_ideal_float() {
    ok("{{1.5}}", &Value::Nil, "1.5");
}

#[test]
fn test_ideal_exp_float() {
    ok("{{1e1}}", &Value::Nil, "10");
}

// Go exec_test.go: map field access with NO key
#[test]
fn test_map_no_key() {
    // Accessing a missing key returns nil (default missingkey behavior)
    let data = tmap! { "MSI" => tmap! { "one" => 1i64 } };
    ok("{{.MSI.NO}}", &data, "<no value>");
}

// Go exec_test.go: dot of various types (extended)
#[test]
fn test_dot_nil() {
    ok("<{{.}}>", &Value::Nil, "<<no value>>");
}

#[test]
fn test_dot_map() {
    // BTreeMap iteration is sorted, so output is deterministic
    let data = tmap! { "a" => 1i64, "b" => 2i64 };
    ok("<{{.}}>", &data, "<map[a:1 b:2]>");
}

// Go exec_test.go: pipeline func
#[test]
fn test_pipeline_func() {
    let data = tmap! { "X" => "xyz" };
    ok("{{.X | printf \"%s\"}}", &data, "xyz");
}

// Go exec_test.go: more if cases
#[test]
fn test_if_nil() {
    // Go: nil is not a command (even in if/with/range condition)
    fail("{{if nil}}TRUE{{else}}FALSE{{end}}", &Value::Nil);
}

#[test]
fn test_if_0_0() {
    ok("{{if 0.0}}TRUE{{else}}FALSE{{end}}", &Value::Nil, "FALSE");
}

#[test]
fn test_if_map_unset() {
    let data = tmap! { "MSI" => tmap! { "one" => 1i64 } };
    ok("{{if .MSI.NO}}TRUE{{else}}FALSE{{end}}", &data, "FALSE");
}

#[test]
fn test_if_map_not_unset() {
    let data = tmap! { "MSI" => tmap! { "one" => 1i64 } };
    ok("{{if .MSI.one}}TRUE{{else}}FALSE{{end}}", &data, "TRUE");
}

#[test]
fn test_if_dollar_x_with_dollar_x_int() {
    // Nested variable shadowing: outer $x is true, inner $x is .I
    let data = tmap! { "I" => 17i64 };
    ok(
        "{{if $x := true}}{{with $x := .I}}{{$x}}{{end}}{{end}}",
        &data,
        "17",
    );
}

// Go exec_test.go: print with numbers
#[test]
fn test_print_123() {
    ok("{{print 123}}", &Value::Nil, "123");
}

#[test]
fn test_print_nil() {
    ok("{{print nil}}", &Value::Nil, "<nil>");
}

// Go exec_test.go: printf lots
#[test]
fn test_printf_lots() {
    ok(
        r#"{{printf "%d %s %d %s" 1 "one" 2 "two"}}"#,
        &Value::Nil,
        "1 one 2 two",
    );
}

// Go exec_test.go: html edge cases
#[test]
fn test_html_ps() {
    let data = tmap! { "PS" => "<p>hi</p>" };
    ok("{{html .PS}}", &data, "&lt;p&gt;hi&lt;/p&gt;");
}

// Go exec_test.go: or / and short-circuit with pipes
#[test]
fn test_or_short_circuit() {
    ok("{{or 0 1 2}}", &Value::Nil, "1");
}

#[test]
fn test_and_short_circuit() {
    ok("{{and 1 0 2}}", &Value::Nil, "0");
}

#[test]
fn test_or_short_circuit2() {
    ok("{{or 0 0 3}}", &Value::Nil, "3");
}

#[test]
fn test_and_short_circuit2() {
    ok("{{and 1 1 0}}", &Value::Nil, "0");
}

// Go exec_test.go: double index
#[test]
fn test_double_index() {
    // index .SI 0 means .SI[0]
    // double index: index of nested structure
    let data = tmap! {
        "Nested" => vec![
            vec![10i64, 20].into_iter().map(Value::Int).collect::<Vec<_>>(),
            vec![30i64, 40].into_iter().map(Value::Int).collect::<Vec<_>>(),
        ].into_iter().map(|v| Value::List(v.into())).collect::<Vec<_>>(),
    };
    ok("{{index .Nested 1 0}}", &data, "30");
}

// Go exec_test.go: with $x struct.U.V
#[test]
fn test_with_dollar_x_nested() {
    let data = tmap! { "U" => tmap! { "V" => "v" } };
    ok("{{with $x := .U.V}}{{$x}}{{end}}", &data, "v");
}

// Go exec_test.go: range with $x PSI
#[test]
fn test_range_dollar_x_psi() {
    let data = tmap! { "PSI" => vec![21i64, 22, 23] };
    ok("{{range $x := .PSI}}<{{$x}}>{{end}}", &data, "<21><22><23>");
}

// Go exec_test.go: range bool
#[test]
fn test_range_bool_list() {
    let data = tmap! {
        "SB" => vec![true, false, true]
            .into_iter().map(Value::Bool).collect::<Vec<_>>()
    };
    ok("{{range .SB}}-{{.}}-{{end}}", &data, "-true--false--true-");
}

// Go exec_test.go: range map (all keys)
#[test]
fn test_range_map_full() {
    let data = tmap! {
        "MSI" => tmap! { "one" => 1i64, "two" => 2i64, "three" => 3i64 }
    };
    // BTreeMap is sorted: one=1, three=3, two=2
    ok(
        "{{range $k, $v := .MSI}}{{$k}}={{$v}} {{end}}",
        &data,
        "one=1 three=3 two=2 ",
    );
}

#[test]
fn test_range_empty_map_no_else() {
    let data = tmap! {
        "MSIEmpty" => alloc::collections::BTreeMap::<String, i64>::new()
    };
    ok("{{range .MSIEmpty}}-{{.}}-{{end}}", &data, "");
}

#[test]
fn test_range_empty_map_else() {
    let data = tmap! {
        "MSIEmpty" => alloc::collections::BTreeMap::<String, i64>::new()
    };
    ok(
        "{{range .MSIEmpty}}-{{.}}-{{else}}empty{{end}}",
        &data,
        "empty",
    );
}

// Go exec_test.go: range int variants
#[test]
fn test_range_int_5() {
    ok("{{range 5}}-{{.}}-{{end}}", &Value::Nil, "-0--1--2--3--4-");
}

#[test]
fn test_range_int_with_index() {
    ok("{{range $i := 3}}[{{$i}}]{{end}}", &Value::Nil, "[0][1][2]");
}

#[test]
fn test_range_int_negative() {
    // Negative count = empty
    ok("{{range -1}}x{{else}}empty{{end}}", &Value::Nil, "empty");
}

// Go exec_test.go: nested assignment changes last declaration
#[test]
fn test_nested_assignment_changes_last_decl() {
    ok(
        "{{$x := 1}}{{if true}}{{$x = 2}}{{end}}{{$x}}",
        &Value::Nil,
        "2",
    );
}

// Go exec_test.go: more comparison tests
#[test]
fn test_eq_nil_nil() {
    ok("{{eq nil nil}}", &Value::Nil, "true");
}

#[test]
fn test_eq_nil_non_nil() {
    ok("{{eq nil 1}}", &Value::Nil, "false");
}

#[test]
fn test_ne_nil_non_nil() {
    ok("{{ne nil 1}}", &Value::Nil, "true");
}

#[test]
fn test_eq_multi_arg_first() {
    // eq 1 2 3 1 → true (1 == 1)
    ok("{{eq 1 2 3 1}}", &Value::Nil, "true");
}

#[test]
fn test_eq_multi_arg_none() {
    ok("{{eq 1 2 3 4}}", &Value::Nil, "false");
}

#[test]
fn test_eq_mixed_int_float() {
    fail("{{eq 1 1.0}}", &Value::Nil);
    fail("{{eq 2 2.0}}", &Value::Nil);
    fail("{{eq 1 1.1}}", &Value::Nil);
}

#[test]
fn test_lt_mixed_int_float() {
    fail("{{lt 1 1.5}}", &Value::Nil);
    fail("{{lt 2 1.5}}", &Value::Nil);
}

#[test]
fn test_ge_mixed_float_int() {
    fail("{{ge 2.0 2}}", &Value::Nil);
    fail("{{ge 1.5 2}}", &Value::Nil);
}

// Go exec_test.go: comparison in pipelines
#[test]
fn test_eq_in_if() {
    ok("{{if eq 1 1}}yes{{else}}no{{end}}", &Value::Nil, "yes");
}

#[test]
fn test_ne_in_if() {
    ok(
        "{{if ne 1 2}}different{{else}}same{{end}}",
        &Value::Nil,
        "different",
    );
}

// Go exec_test.go: cute examples (or as if)
#[test]
fn test_or_as_if_true_inline() {
    ok(r#"{{or .X "default"}}"#, &tmap! { "X" => "value" }, "value");
}

#[test]
fn test_or_as_if_false_inline() {
    ok(r#"{{or .X "default"}}"#, &tmap! { "X" => "" }, "default");
}

// Go exec_test.go: more with cases
#[test]
fn test_with_empty_map() {
    let data = tmap! {
        "MSIEmpty" => alloc::collections::BTreeMap::<String, i64>::new()
    };
    ok(
        "{{with .MSIEmpty}}non-empty{{else}}empty{{end}}",
        &data,
        "empty",
    );
}

#[test]
fn test_with_map() {
    let data = tmap! {
        "MSI" => tmap! { "one" => 1i64 }
    };
    ok("{{with .MSI}}{{.one}}{{else}}empty{{end}}", &data, "1");
}

// Go exec_test.go: numbers (literal formats)
#[test]
fn test_decimal() {
    ok("{{print 1234}}", &Value::Nil, "1234");
}

#[test]
fn test_decimal_underscore_print() {
    ok("{{print 1_234}}", &Value::Nil, "1234");
}

#[test]
fn test_binary() {
    ok("{{printf \"%d\" 0b101}}", &Value::Nil, "5");
}

#[test]
fn test_binary_underscore() {
    ok("{{printf \"%d\" 0b_1_0_1}}", &Value::Nil, "5");
}

#[test]
fn test_octal() {
    ok("{{printf \"%d\" 0o377}}", &Value::Nil, "255");
}

#[test]
fn test_octal_underscore() {
    ok("{{printf \"%d\" 0o3_7_7}}", &Value::Nil, "255");
}

#[test]
fn test_hex() {
    ok("{{printf \"%d\" 0xdead}}", &Value::Nil, "57005");
}

#[test]
fn test_hex_underscore() {
    ok("{{printf \"%d\" 0xde_ad}}", &Value::Nil, "57005");
}

#[test]
fn test_float() {
    ok("{{print 1.5}}", &Value::Nil, "1.5");
}

#[test]
fn test_float_underscore() {
    ok("{{print 1_0.2_5}}", &Value::Nil, "10.25");
}

// Go multi_test.go: invoke template with different dot types
#[test]
fn test_invoke_dot_int() {
    let data = tmap! { "I" => 17i64 };
    ok(
        r#"{{define "dot"}}{{.}}{{end}}{{template "dot" .I}}"#,
        &data,
        "17",
    );
}

#[test]
fn test_invoke_dot_list() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok(
        r#"{{define "dot"}}{{.}}{{end}}{{template "dot" .SI}}"#,
        &data,
        "[3 4 5]",
    );
}

#[test]
fn test_invoke_nested_int() {
    let data = tmap! { "I" => 17i64 };
    ok(
        r#"{{define "inner"}}{{.}}{{end}}{{define "outer"}}[{{template "inner" .}}]{{end}}{{template "outer" .I}}"#,
        &data,
        "[17]",
    );
}

// Go multi_test.go: template with no args
#[test]
fn test_invoke_no_args() {
    ok(
        r#"{{define "x"}}hello{{end}}{{template "x"}}"#,
        &Value::Nil,
        "hello",
    );
}

// Go multi_test.go: testFunc
#[test]
fn test_one_arg_literal() {
    ok(r#"{{oneArg "joe"}}"#, &Value::Nil, "oneArg=joe");
}

#[test]
fn test_one_arg_dot() {
    ok(
        r#"{{oneArg .}}"#,
        &Value::String("joe".into()),
        "oneArg=joe",
    );
}

// Go multi_test.go: TestRedefinition — across *separate* Parse calls,
// a later non-empty definition replaces the earlier one without error.
// (Two non-empty defines in a single parse call would error; see
// `test_redefinition_within_single_parse_errors`.)
#[test]
fn test_redefinition() {
    let tmpl = Template::new("test")
        .parse(r#"{{define "x"}}first{{end}}{{template "x"}}"#)
        .unwrap()
        .parse(r#"{{define "x"}}second{{end}}"#)
        .unwrap();
    let result = tmpl.execute_to_string(&Value::Nil).unwrap();
    assert_eq!(result, "second");
}

// Go multi_test.go: empty template
#[test]
fn test_execute_empty_template() {
    let tmpl = Template::new("empty").parse("").unwrap();
    assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "");
}

// Go exec_test.go: TestMessageForExecuteEmpty
#[test]
fn test_message_for_unparsed_template() {
    let tmpl = Template::new("unparsed");
    let result = tmpl.execute_to_string(&Value::Nil);
    assert!(result.is_err());
    assert!(
        result
            .err()
            .unwrap()
            .to_string()
            .contains("has not been parsed")
    );
}

// Go exec_test.go: TestBlock with override
#[test]
fn test_block_override() {
    use gotmpl::parse::{ListNode, Node, Pos, TextNode};

    let tmpl = Template::new("page")
        .parse(r#"{{block "content" .}}default content{{end}}"#)
        .unwrap();

    // Without override
    assert_eq!(
        tmpl.execute_to_string(&Value::Nil).unwrap(),
        "default content",
    );

    // Override via clone + add_parse_tree
    let overridden = tmpl.clone().add_parse_tree(
        "content",
        ListNode {
            pos: Pos::new(0, 1),
            nodes: vec![Node::Text(TextNode {
                pos: Pos::new(0, 1),
                text: "custom content".into(),
            })],
        },
    );
    assert_eq!(
        overridden.execute_to_string(&Value::Nil).unwrap(),
        "custom content",
    );
}

// Go exec_test.go: ExecuteTemplate
#[test]
fn test_execute_template_with_data() {
    let tmpl = Template::new("root")
        .parse(r#"{{define "greet"}}Hello, {{.Name}}!{{end}}main"#)
        .unwrap();

    let data = tmap! { "Name" => "Alice" };
    assert_eq!(
        tmpl.execute_template_to_string("greet", &data).unwrap(),
        "Hello, Alice!"
    );
}

#[test]
fn test_execute_template_undefined_fails() {
    let tmpl = Template::new("t").parse("hello").unwrap();
    let err = tmpl.execute_template_to_string("no_such_template", &Value::Nil);
    assert!(err.is_err());
}

// Go exec_test.go: Lookup / Templates / DefinedTemplates
#[test]
fn test_lookup_defined() {
    let tmpl = Template::new("t")
        .parse(r#"{{define "a"}}A{{end}}{{define "b"}}B{{end}}"#)
        .unwrap();
    assert!(tmpl.lookup("a").is_some());
    assert!(tmpl.lookup("b").is_some());
    assert!(tmpl.lookup("c").is_none());
}

#[test]
fn test_templates_sorted() {
    let tmpl = Template::new("t")
        .parse(r#"{{define "z"}}{{end}}{{define "a"}}{{end}}{{define "m"}}{{end}}"#)
        .unwrap();
    assert_eq!(tmpl.templates(), vec!["a", "m", "z"]);
}

#[test]
fn test_defined_templates_string() {
    let tmpl = Template::new("t")
        .parse(r#"{{define "alpha"}}{{end}}{{define "beta"}}{{end}}"#)
        .unwrap();
    let s = tmpl.defined_templates();
    assert!(s.contains("\"alpha\""));
    assert!(s.contains("\"beta\""));
    assert!(s.starts_with("; defined templates are:"));
}

// Go exec_test.go: Clone independence
#[test]
fn test_clone_independence() {
    use gotmpl::parse::{ListNode, Node, Pos, TextNode};

    let original = Template::new("t")
        .parse(r#"{{define "x"}}orig{{end}}{{template "x"}}"#)
        .unwrap();

    let clone1 = original.clone().add_parse_tree(
        "x",
        ListNode {
            pos: Pos::new(0, 1),
            nodes: vec![Node::Text(TextNode {
                pos: Pos::new(0, 1),
                text: "clone1".into(),
            })],
        },
    );

    let clone2 = original.clone().add_parse_tree(
        "x",
        ListNode {
            pos: Pos::new(0, 1),
            nodes: vec![Node::Text(TextNode {
                pos: Pos::new(0, 1),
                text: "clone2".into(),
            })],
        },
    );

    assert_eq!(original.execute_to_string(&Value::Nil).unwrap(), "orig");
    assert_eq!(clone1.execute_to_string(&Value::Nil).unwrap(), "clone1");
    assert_eq!(clone2.execute_to_string(&Value::Nil).unwrap(), "clone2");
}

// Go exec_test.go: comments in various positions
#[test]
fn test_comment_standalone() {
    ok("{{/* only comment */}}", &Value::Nil, "");
}

#[test]
fn test_comment_before_action() {
    ok("{{/* c */}}{{.}}", &Value::String("hi".into()), "hi");
}

#[test]
fn test_comment_after_action() {
    ok("{{.}}{{/* c */}}", &Value::String("hi".into()), "hi");
}

#[test]
fn test_comment_in_if() {
    ok("{{if true}}{{/* c */}}yes{{end}}", &Value::Nil, "yes");
}

#[test]
fn test_comment_trim_surrounding_whitespace() {
    // Comment with both trim markers should eat surrounding whitespace
    ok("a  {{- /* comment */ -}}  b", &Value::Nil, "ab");
}

// Go exec_test.go: parenthesized expressions (extended)
#[test]
fn test_parens_dollar_in_paren() {
    let data = tmap! { "X" => "x" };
    ok("{{($).X}}", &data, "x");
}

#[test]
fn test_parens_spaces_and_args() {
    ok(r#"{{printf "%d %d" ( 1 ) ( 2 )}}"#, &Value::Nil, "1 2");
}

// Go exec_test.go: break/continue with else
#[test]
fn test_range_int_break_else() {
    ok(
        "{{range $i := 5}}{{if eq $i 3}}{{break}}{{end}}{{$i}} {{else}}empty{{end}}",
        &Value::Nil,
        "0 1 2 ",
    );
}

#[test]
fn test_range_int_continue_else() {
    ok(
        "{{range $i := 5}}{{if eq $i 2}}{{continue}}{{end}}{{$i}} {{else}}empty{{end}}",
        &Value::Nil,
        "0 1 3 4 ",
    );
}

// Go exec_test.go: call builtin (extended)
#[test]
fn test_call_with_args() {
    use alloc::sync::Arc;
    let data = tmap! {};
    let result = Template::new("test")
        .func("getfn", |_| {
            let f: gotmpl::ValueFunc = Arc::new(|args| {
                let a = args[0].as_int().unwrap_or(0);
                let b = args[1].as_int().unwrap_or(0);
                Ok(Value::Int(a + b))
            });
            Ok(Value::Function(f))
        })
        .parse("{{call (getfn) 10 20}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(result, "30");
}

#[test]
fn test_call_non_function_fails() {
    fail("{{call 42}}", &Value::Nil);
}

// Go exec_test.go: len of nothing (error)
#[test]
fn test_len_of_nil_fails() {
    fail("{{len nil}}", &Value::Nil);
}

#[test]
fn test_len_of_int_fails2() {
    fail("{{len 42}}", &Value::Nil);
}

// Go exec_test.go: slice edge cases
#[test]
fn test_slice_out_of_range() {
    let data = tmap! { "SI" => vec![1i64, 2, 3] };
    fail("{{slice .SI 5}}", &data);
}

#[test]
fn test_slice_inverted_range() {
    let data = tmap! { "SI" => vec![1i64, 2, 3] };
    fail("{{slice .SI 2 1}}", &data);
}

// Go exec_test.go: deeply nested template calls
#[test]
fn test_template_chain() {
    ok(
        r#"{{define "a"}}[{{template "b" .}}]{{end}}{{define "b"}}({{.}}){{end}}{{template "a" "hi"}}"#,
        &Value::Nil,
        "[(hi)]",
    );
}

// Go exec_test.go: printf %% escape
#[test]
fn test_printf_percent_escape() {
    ok(r#"{{printf "100%%"}}"#, &Value::Nil, "100%");
}

// Go exec_test.go: index of map with string key from variable
#[test]
fn test_index_map_with_variable_key() {
    let data = tmap! {
        "M" => tmap! { "k" => "found" }
    };
    ok(r#"{{$key := "k"}}{{index .M $key}}"#, &data, "found");
}

// Go exec_test.go: $ in range points to root data
#[test]
fn test_dollar_in_range_points_to_root() {
    let data = tmap! {
        "Title" => "root",
        "Items" => vec!["a".to_string(), "b".to_string()],
    };
    ok(
        "{{range .Items}}{{$.Title}}:{{.}} {{end}}",
        &data,
        "root:a root:b ",
    );
}

// Go exec_test.go: multiline templates
#[test]
fn test_multiline_template() {
    let tmpl = "line1\n{{if true}}line2\n{{end}}line3";
    ok(tmpl, &Value::Nil, "line1\nline2\nline3");
}

// Go exec_test.go: empty range with declared variable
#[test]
fn test_range_empty_with_decl() {
    let data = tmap! { "SIEmpty" => Vec::<i64>::new() };
    ok(
        "{{range $x := .SIEmpty}}{{$x}}{{else}}empty{{end}}",
        &data,
        "empty",
    );
}

// Go exec_test.go: with as if (non-nil, non-zero check)
#[test]
fn test_with_as_default() {
    // Common Go pattern: {{with .Subtitle}}{{.}}{{else}}No subtitle{{end}}
    let data = tmap! { "Subtitle" => "" };
    ok(
        r#"{{with .Subtitle}}{{.}}{{else}}No subtitle{{end}}"#,
        &data,
        "No subtitle",
    );
}

#[test]
fn test_with_as_default_with_value() {
    let data = tmap! { "Subtitle" => "My Subtitle" };
    ok(
        r#"{{with .Subtitle}}{{.}}{{else}}No subtitle{{end}}"#,
        &data,
        "My Subtitle",
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// Additional tests from review: successive parse, block override, printf
// flags, clone options, edge cases, piped-value-into-non-function
// ═══════════════════════════════════════════════════════════════════════════

// Block override determinism (Go's TestIssue19294)
#[test]
fn test_block_override_determinism() {
    // Run multiple times to catch any non-determinism in block override
    for _ in 0..50 {
        let tmpl = Template::new("page")
            .parse(r#"{{block "style" .}}default{{end}}"#)
            .unwrap()
            .parse(r#"{{define "style"}}custom{{end}}"#)
            .unwrap();
        assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "custom");
    }
}

// Multiple non-empty defines of the same name within a single parse call
// are rejected with `multiple definition of template "x"`, matching Go's
// `parse.(*Tree).add`. (Redefinition across *separate* parse calls is
// allowed and exercised by `test_parse_define_override` in rust_api.rs.)
#[test]
fn test_redefinition_within_single_parse_errors() {
    let err = Template::new("t")
        .parse(
            r#"{{define "x"}}first{{end}}{{define "x"}}second{{end}}{{define "x"}}third{{end}}{{template "x"}}"#,
        )
        .err()
        .expect("expected multiple-definition error");
    let msg = err.to_string();
    assert!(
        msg.contains(r#"multiple definition of template "x""#),
        "unexpected error message: {msg}"
    );
}

// Minimal two-define case: Go rejects this at parse time with
// `template: t:1: template: multiple definition of template "x"`, so Rust
// must too (no execution should happen).
#[test]
fn test_two_non_empty_defines_same_parse_errors() {
    let err = Template::new("t")
        .parse(r#"{{define "x"}}first{{end}}{{define "x"}}second{{end}}{{template "x"}}"#)
        .err()
        .expect("expected multiple-definition error");
    let msg = err.to_string();
    assert!(
        msg.contains(r#"multiple definition of template "x""#),
        "unexpected error message: {msg}"
    );
}

// Empty template definitions: Go's `parse.associate` refuses to overwrite
// a non-empty define with an empty one (same IsEmptyTree rule as the main
// body). Routed through `ok()` so go-crosscheck verifies the Rust output
// matches `text/template`.
#[test]
fn test_empty_define_does_not_clobber_existing() {
    ok(
        r#"{{define "x"}}content{{end}}{{define "x"}}{{end}}[{{template "x"}}]"#,
        &Value::Nil,
        "[content]",
    );
}

// Piped value into non-function errors (Go behavior)
#[test]
fn test_pipe_into_non_function_fails() {
    // Piping into a field access should fail
    let data = tmap! { "X" => "hello" };
    fail("{{true | .X}}", &data);
}

#[test]
fn test_pipe_into_literal_fails() {
    fail(r#"{{"x" | "y"}}"#, &Value::Nil);
}

#[test]
fn test_pipe_into_number_fails() {
    fail("{{1 | 2}}", &Value::Nil);
}

#[test]
fn test_pipe_into_dot_fails() {
    fail("{{1 | .}}", &Value::String("x".into()));
}

// Printf width and flag tests
#[test]
fn test_printf_width_right_aligned() {
    ok(r#"{{printf "%10d" 42}}"#, &Value::Nil, "        42");
}

#[test]
fn test_printf_width_left_aligned() {
    ok(r#"{{printf "%-10d" 42}}"#, &Value::Nil, "42        ");
}

#[test]
fn test_printf_plus_flag() {
    ok(r#"{{printf "%+d" 42}}"#, &Value::Nil, "+42");
    ok(r#"{{printf "%+d" -42}}"#, &Value::Nil, "-42");
}

#[test]
fn test_printf_space_flag() {
    ok(r#"{{printf "% d" 42}}"#, &Value::Nil, " 42");
    ok(r#"{{printf "% d" -42}}"#, &Value::Nil, "-42");
}

#[test]
fn test_printf_zero_pad() {
    ok(r#"{{printf "%06d" 42}}"#, &Value::Nil, "000042");
    ok(r#"{{printf "%06d" -42}}"#, &Value::Nil, "-00042");
}

#[test]
fn test_printf_hash_hex() {
    ok(r#"{{printf "%#x" 255}}"#, &Value::Nil, "0xff");
    ok(r#"{{printf "%#X" 255}}"#, &Value::Nil, "0XFF");
}

#[test]
fn test_printf_hash_octal() {
    ok(r#"{{printf "%#o" 8}}"#, &Value::Nil, "010");
}

#[test]
fn test_printf_string_width() {
    ok(r#"{{printf "%-10s" "hi"}}"#, &Value::Nil, "hi        ");
    ok(r#"{{printf "%10s" "hi"}}"#, &Value::Nil, "        hi");
}

#[test]
fn test_printf_string_precision() {
    // Precision truncates string to N chars
    ok(r#"{{printf "%.3s" "hello"}}"#, &Value::Nil, "hel");
}

#[test]
fn test_printf_missing_arg() {
    // Too few args for format: Go produces %!d(MISSING)
    ok(r#"{{printf "%d"}}"#, &Value::Nil, "%!d(MISSING)");
}

#[test]
fn test_printf_e_verb() {
    ok(r#"{{printf "%e" 1.5}}"#, &Value::Nil, "1.500000e+00");
}

#[test]
fn test_printf_t_verb() {
    ok(r#"{{printf "%t" true}}"#, &Value::Nil, "true");
    ok(r#"{{printf "%t" false}}"#, &Value::Nil, "false");
}

#[test]
fn test_printf_b_verb() {
    ok(r#"{{printf "%b" 10}}"#, &Value::Nil, "1010");
}

#[test]
fn test_printf_c_verb() {
    ok(r#"{{printf "%c" 65}}"#, &Value::Nil, "A");
}

// Go bug7: $ access inside with
#[test]
fn test_dollar_inside_with() {
    let data = tmap! { "I" => 17i64, "X" => "x" };
    ok("{{with $c := .}}{{$.I}}{{end}}", &data, "17");
}

#[test]
fn test_dollar_field_inside_with() {
    let data = tmap! { "I" => 17i64, "X" => "x" };
    ok("{{with .X}}{{$.I}}-{{.}}{{end}}", &data, "17-x");
}

#[test]
fn test_dollar_inside_range() {
    let data = tmap! {
        "I" => 17i64,
        "SI" => vec![1i64, 2, 3],
    };
    ok(
        "{{range .SI}}{{$.I}}-{{.}} {{end}}",
        &data,
        "17-1 17-2 17-3 ",
    );
}

// nil as function arg (Go exec_test.go: "nil call arg")
#[test]
fn test_nil_as_function_arg() {
    ok("{{print nil}}", &Value::Nil, "<nil>");
}

// Scope isolation: variable declared in with pipeline
#[test]
fn test_with_pipeline_var_scope() {
    // Variable from with's else branch should not leak
    ok(
        "{{with 0}}{{.}}{{else}}{{$x := 42}}{{$x}}{{end}}",
        &Value::Nil,
        "42",
    );
}

// Range over empty with declared variable
#[test]
fn test_range_empty_with_two_vars() {
    let data = tmap! { "SIEmpty" => Vec::<i64>::new() };
    ok(
        "{{range $i, $v := .SIEmpty}}{{$i}}:{{$v}}{{else}}empty{{end}}",
        &data,
        "empty",
    );
}

// Nested template $ rebinding
#[test]
fn test_template_dollar_rebinding() {
    // Inside a called template, $ should refer to the dot passed to that template,
    // not the root data.
    let data = tmap! { "I" => 17i64 };
    ok(
        r#"{{define "inner"}}{{$}}{{end}}{{template "inner" .I}}"#,
        &data,
        "17",
    );
}

// Parse error: missing end in define
#[test]
fn test_parse_error_unclosed_action() {
    // Unclosed action should fail at lex time
    let result = Template::new("t").parse("{{define \"foo\"");
    assert!(result.is_err());
}

#[test]
fn test_parse_error_malformed_define_name() {
    let result = Template::new("t").parse("{{define 42}}text{{end}}");
    assert!(result.is_err());
}

// Comparison edge cases
#[test]
fn test_lt_uncomparable_types() {
    // Go: bools are unordered for lt/le/gt/ge.
    fail("{{lt true false}}", &Value::Nil);
}

#[test]
fn test_eq_list_list() {
    // Go: slices/lists are not comparable.
    let data = tmap! {
        "A" => vec![1i64],
        "B" => vec![1i64],
    };
    fail("{{$a := .A}}{{$b := .B}}{{eq $a $b}}", &data);
}

// ═══════════════════════════════════════════════════════════════════════════
// Additional missing Go tests
// ═══════════════════════════════════════════════════════════════════════════

// Legacy octal (Go compat: 0377 = 255)
#[test]
fn test_legacy_octal() {
    ok("{{0377}}", &Value::Nil, "255");
}

#[test]
fn test_legacy_octal_underscore() {
    ok("{{0_3_7_7}}", &Value::Nil, "255");
}

// Non-function with args (Go bug7a/b/c)
#[test]
fn test_non_function_with_args() {
    // {{3 2}} — can't give argument to non-function <number>
    fail("{{3 2}}", &Value::Nil);
}

#[test]
fn test_variable_not_callable() {
    // {{$x 2}} — can't give argument to non-function <variable>
    fail("{{$x := 1}}{{$x 2}}", &Value::Nil);
}

#[test]
fn test_variable_piped_not_callable() {
    // {{3 | $x}} — can't give argument to non-function <variable>
    fail("{{$x := 1}}{{3 | $x}}", &Value::Nil);
}

// Parenthesized non-function (Go Issue 31810)
#[test]
fn test_parenthesized_non_function_ok() {
    // {{(1)}} — ok, just returns 1
    ok("{{(1)}}", &Value::Nil, "1");
}

#[test]
fn test_parenthesized_non_function_with_args_fails() {
    // {{(1) 2}} — can't give argument to non-function
    fail("{{(1) 2}}", &Value::Nil);
}

// Nested variable scoping (Go exec_test.go)
#[test]
fn test_nested_var_scoping_deep() {
    // Inner $x := 2 shadows outer $x := 1. $x = 3 modifies the shadow,
    // not the original. After both scopes pop, outer $x is still 1.
    ok(
        "{{$x := 1}}{{if true}}{{$x := 2}}{{if true}}{{$x = 3}}{{end}}{{end}}{{$x}}",
        &Value::Nil,
        "1",
    );
}

// Range break — text after break not reached (Go exec_test.go)
#[test]
fn test_range_break_not_reached() {
    let data = tmap! { "SI" => vec![3i64, 4, 5] };
    ok(
        "{{range .SI}}-{{.}}-{{break}}NOTREACHED{{else}}EMPTY{{end}}",
        &data,
        "-3-",
    );
}

// Printf %g verb (Go exec_test.go)
#[test]
fn test_printf_g_verb() {
    ok(r#"{{printf "%g" 3.5}}"#, &Value::Nil, "3.5");
}

#[test]
fn test_printf_g_large() {
    ok(r#"{{printf "%g" 1e7}}"#, &Value::Nil, "1e+07");
}

#[test]
fn test_printf_g_small() {
    ok(r#"{{printf "%g" 0.000035}}"#, &Value::Nil, "3.5e-05");
}

#[test]
fn test_printf_g_precision() {
    ok(r#"{{printf "%.4g" 123456.0}}"#, &Value::Nil, "1.235e+05");
}

// Printf %#q verb (Go exec_test.go)
#[test]
fn test_printf_hash_q() {
    ok(r#"{{printf "%#q" "hello"}}"#, &Value::Nil, "`hello`");
}

#[test]
fn test_printf_hash_q_with_backtick() {
    // Contains backtick — falls back to double-quoted
    ok(r#"{{printf "%#q" "hel`lo"}}"#, &Value::Nil, r#""hel`lo""#);
}

// Printf %04x (Go exec_test.go: "printf int")
#[test]
fn test_printf_zero_pad_hex() {
    ok(r#"{{printf "%04x" 127}}"#, &Value::Nil, "007f");
}

// Printf pipe to printf (Go bug16k)
#[test]
fn test_pipe_to_printf() {
    ok(r#"{{"aaa"|printf}}"#, &Value::Nil, "aaa");
}

// Hex float literal (Go exec_test.go)
#[test]
fn test_hex_float_literal() {
    ok("{{printf \"%g\" 0x1.ep+2}}", &Value::Nil, "7.5");
}

// Nil as argument to function is ok (Go exec_test.go)
#[test]
fn test_nil_as_eq_arg() {
    // nil as function argument is fine (not as command)
    ok("{{eq nil nil}}", &Value::Nil, "true");
    ok("{{ne nil 1}}", &Value::Nil, "true");
}

// Go bug14: nil field access
#[test]
fn test_nil_field_access() {
    // Accessing a field on nil returns nil (not an error in our model)
    ok("{{$x := .Missing}}{{$x}}", &tmap! {}, "<no value>");
}

// Go bug10: mapOfThree in parenthesized form
#[test]
fn test_map_of_three_parens() {
    ok("{{(mapOfThree).three}}", &Value::Nil, "3");
}

// UTF-8 execution
#[test]
fn test_utf8_text_preserved_verbatim() {
    ok(
        "Bonjour, ça va? — 日本語 🎉",
        &Value::Nil,
        "Bonjour, ça va? — 日本語 🎉",
    );
}

#[test]
fn test_utf8_string_literal() {
    ok(r#"{{"café"}}"#, &Value::Nil, "café");
}

#[test]
fn test_utf8_raw_string_literal() {
    ok("{{`日本語`}}", &Value::Nil, "日本語");
}

#[test]
fn test_utf8_emoji_in_data() {
    let data = tmap! { "S" => "party 🎉 time" };
    ok("{{.S}}", &data, "party 🎉 time");
}

#[test]
fn test_utf8_data_string() {
    let data = tmap! { "Name" => "José" };
    ok("Hello, {{.Name}}!", &data, "Hello, José!");
}

#[test]
fn test_utf8_field_name() {
    // Go's text/template allows unicode-letter identifiers; our lexer matches
    // via `char::is_alphabetic`, which likewise accepts letters like 'é'.
    let data = tmap! { "Café" => "yum" };
    ok("{{.Café}}", &data, "yum");
}

#[test]
fn test_utf8_cjk_field_name() {
    let data = tmap! { "名前" => "太郎" };
    ok("{{.名前}}", &data, "太郎");
}

#[test]
fn test_utf8_variable_name() {
    let data = tmap! { "X" => "hi" };
    ok("{{$café := .X}}{{$café}}", &data, "hi");
}

#[test]
fn test_utf8_index_with_utf8_key() {
    let data = tmap! { "m" => tmap! { "café" => "yum" } };
    ok(r#"{{index .m "café"}}"#, &data, "yum");
}

#[test]
fn test_utf8_printf_string_and_int() {
    ok(
        r#"{{printf "%s = %d" "café" 42}}"#,
        &Value::Nil,
        "café = 42",
    );
}

#[test]
fn test_utf8_len_returns_byte_length() {
    // Go's `len` on a string returns byte length (UTF-8 bytes), not rune count.
    // "café" = 'c'(1) + 'a'(1) + 'f'(1) + 'é'(2) = 5 bytes.
    let data = tmap! { "S" => "café" };
    ok("{{len .S}}", &data, "5");
}

#[test]
fn test_utf8_len_emoji() {
    // 🎉 = U+1F389, encoded as 4 bytes in UTF-8.
    let data = tmap! { "S" => "🎉" };
    ok("{{len .S}}", &data, "4");
}

#[test]
fn test_utf8_range_over_map_utf8_keys() {
    // Map iteration order is BTreeMap order (lexicographic on UTF-8 bytes).
    // 'α' (U+03B1) sorts before 'β' (U+03B2).
    let data = tmap! { "m" => tmap! { "α" => "alpha", "β" => "beta" } };
    ok(
        "{{range $k, $v := .m}}{{$k}}={{$v}};{{end}}",
        &data,
        "α=alpha;β=beta;",
    );
}

#[test]
fn test_utf8_unicode_escape_bmp() {
    // \u00e9 → é, \u65e5\u672c\u8a9e → 日本語
    ok(r#"{{"\u00e9"}}"#, &Value::Nil, "é");
    ok(r#"{{"\u65e5\u672c\u8a9e"}}"#, &Value::Nil, "日本語");
}

#[test]
fn test_utf8_unicode_escape_supplementary() {
    // \U0001F600 → 😀 (outside the BMP; requires the 8-digit \U form).
    ok(r#"{{"\U0001F600"}}"#, &Value::Nil, "😀");
}

#[test]
fn test_utf8_multiline_with_nonascii() {
    // Ensure lines with UTF-8 don't disturb line/newline tracking in the body.
    ok("é\n{{.}}\n日", &Value::String("ok".into()), "é\nok\n日");
}

#[test]
fn test_utf8_combining_marks_preserved() {
    // 'é' as U+0065 ('e') + U+0301 (combining acute). Two chars, same visual.
    let composed = "caf\u{00e9}"; // precomposed é
    let decomposed = "cafe\u{0301}"; // e + combining acute
    let data = tmap! { "A" => composed, "B" => decomposed };
    // Both are preserved verbatim; no normalisation is applied.
    ok("{{.A}}|{{.B}}", &data, &format!("{composed}|{decomposed}"));
}

#[test]
fn test_utf8_custom_delimiters_preserve_unicode() {
    let data = tmap! { "X" => "日本語" };
    let result = Template::new("test")
        .delims("<<", ">>")
        .parse("[<<.X>>]")
        .unwrap()
        .execute_to_string(&data)
        .unwrap();
    assert_eq!(result, "[日本語]");
}

// Variable scoping
#[test]
fn test_if_decl_does_not_leak_past_end() {
    // Per Go spec, a variable declared in a control pipeline is scoped to
    // the block. Referencing it after `{{end}}` must fail.
    fail("{{if $x := true}}y{{end}}{{$x}}", &Value::Nil);
}

#[test]
fn test_with_decl_does_not_leak_past_end() {
    let data = tmap! { "A" => 1i64 };
    fail("{{with $x := .A}}y{{end}}{{$x}}", &data);
}

#[test]
fn test_range_decl_does_not_leak_past_end() {
    let data = tmap! { "L" => vec![1i64, 2, 3] };
    fail("{{range $i, $v := .L}}{{end}}{{$i}}", &data);
}

#[test]
fn test_multi_var_decl_outside_range_errors() {
    // Only `range` permits multiple declaration variables.
    fail("{{$a, $b := 5}}", &Value::Nil);
}

// Integer literals above i64 range are rejected by the lexer. Go rejects
// the same literals too (at exec time with "overflows int"), so no parity
// regression here.
#[test]
fn test_hex_literal_above_i64_max_rejected() {
    let err = match Template::new("t").parse("{{0xFFFFFFFFFFFFFFFF}}") {
        Ok(_) => panic!("literal above i64::MAX should have failed to parse"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("overflows"),
        "expected overflow error, got: {err}"
    );
}

// DoS guards
#[test]
fn test_deeply_nested_if_rejected_not_panic() {
    let mut src = String::new();
    let n = 150; // exceeds MAX_PARSE_DEPTH (100)
    for _ in 0..n {
        src.push_str("{{if 1}}");
    }
    src.push('x');
    for _ in 0..n {
        src.push_str("{{end}}");
    }
    // Must return a parse error, not stack-overflow.
    let err = Template::new("t").parse(&src).err();
    assert!(
        err.as_ref()
            .is_some_and(|e| e.to_string().contains("nesting depth")),
        "expected depth-limit error, got {:?}",
        err
    );
}

#[test]
fn test_printf_huge_width_terminates() {
    // u16-clamped width must not cause a panic or multi-GB allocation.
    let r = Template::new("t")
        .parse(r#"{{printf "%9999999999d" 0}}"#)
        .and_then(|t| t.execute_to_string(&Value::Nil));
    assert!(r.is_ok());
}

#[test]
fn test_deeply_nested_parens_rejected_not_panic() {
    // Paren nesting in a pipeline recurses through parse_command/parse_pipeline,
    // so the depth guard must apply there too (not just to parse_list).
    let n = 200; // exceeds MAX_PARSE_DEPTH (100)
    let mut src = String::from("{{");
    for _ in 0..n {
        src.push('(');
    }
    src.push('1');
    for _ in 0..n {
        src.push(')');
    }
    src.push_str("}}");
    let err = Template::new("t").parse(&src).err();
    assert!(
        err.as_ref()
            .is_some_and(|e| e.to_string().contains("nesting depth")),
        "expected depth-limit error, got {:?}",
        err
    );
}

#[test]
fn test_huge_range_rejected() {
    // Billion-iteration range must fail fast under the iteration cap.
    let r = Template::new("t")
        .max_range_iters(1_000)
        .parse("{{range 1000000000}}.{{end}}")
        .and_then(|t| t.execute_to_string(&Value::Nil));
    assert!(
        r.as_ref()
            .is_err_and(|e| e.to_string().contains("range iteration budget")),
        "expected range-budget error, got {:?}",
        r
    );
}
