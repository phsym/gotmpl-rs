//! Parity tests for `gotmpl::html::Template` against Go's `html/template`.
//!
//! Mirrors `tests/go_compat.rs` but for the context-aware auto-escaping engine.
//! Every `ok()` case asserts the hardcoded expected output (taken byte-for-byte
//! from Go 1.26.4 `html/template`) and, under the `go-crosscheck` feature,
//! additionally diffs the Rust output against a live Go toolchain.
//!
//! The whole file is gated on the `html` feature, so it is empty otherwise.
#![cfg(feature = "html")]

use gotmpl::html::{CSS, HTML, HTMLAttr, JS, JSStr, Srcset, Template, URL};
use gotmpl::{Value, tmap};

// The crosscheck harness spawns a Go toolchain via `std::process`, so it needs
// `std` in addition to the `go-crosscheck` feature.
#[cfg(all(feature = "go-crosscheck", feature = "std"))]
mod go_crosscheck {
    use gotmpl::{SafeKind, Value};
    use std::io::Write;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::sync::LazyLock;

    /// Path to the compiled Go html helper binary. Built once per test run.
    static GO_BINARY: LazyLock<PathBuf> = LazyLock::new(|| {
        let src =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/testdata/go_html_crosscheck.go");
        let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("go-html-crosscheck");
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

    fn json_escape(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if c < '\x20' => out.push_str(&format!("\\u{:04x}", c as u32)),
                c => out.push(c),
            }
        }
        out
    }

    fn safe_kind_tag(kind: SafeKind) -> &'static str {
        match kind {
            SafeKind::Html => "html",
            SafeKind::HtmlAttr => "htmlattr",
            SafeKind::Js => "js",
            SafeKind::JsStr => "jsstr",
            SafeKind::Css => "css",
            SafeKind::Url => "url",
            SafeKind::Srcset => "srcset",
        }
    }

    fn value_to_json(v: &Value) -> Result<String, String> {
        Ok(match v {
            Value::Nil => r#"{"type":"nil"}"#.to_string(),
            Value::Bool(b) => format!(r#"{{"type":"bool","value":{b}}}"#),
            Value::Int(n) => format!(r#"{{"type":"int","value":{n}}}"#),
            Value::Uint(n) => format!(r#"{{"type":"uint","value":{n}}}"#),
            Value::Float(f) => {
                if f.is_infinite() || f.is_nan() {
                    return Err("Value::Float is NaN or infinite".into());
                }
                let s = if f.fract() == 0.0 {
                    format!("{f:.1}")
                } else {
                    format!("{f}")
                };
                format!(r#"{{"type":"float","value":{s}}}"#)
            }
            Value::String(s) => format!(r#"{{"type":"string","value":"{}"}}"#, json_escape(s)),
            Value::Safe { kind, s } => format!(
                r#"{{"type":"safe","kind":"{}","value":"{}"}}"#,
                safe_kind_tag(*kind),
                json_escape(s)
            ),
            Value::List(items) => {
                let encoded: Result<Vec<String>, _> = items.iter().map(value_to_json).collect();
                format!(r#"{{"type":"list","items":[{}]}}"#, encoded?.join(","))
            }
            Value::Map(m) => {
                let mut entries = Vec::new();
                for (k, v) in m.as_ref() {
                    entries.push(format!(r#""{}":{}"#, json_escape(k), value_to_json(v)?));
                }
                format!(r#"{{"type":"map","map":{{{}}}}}"#, entries.join(","))
            }
            Value::Function(_) => return Err("Value::Function cannot be serialized".into()),
        })
    }

    fn payload(template_str: &str, data: &Value) -> String {
        let data_json = value_to_json(data)
            .unwrap_or_else(|reason| panic!("cross-check refused for {template_str:?}: {reason}"));
        format!(
            r#"{{"template":"{}","data":{}}}"#,
            json_escape(template_str),
            data_json
        )
    }

    fn run_go(template_str: &str, data: &Value) -> std::process::Output {
        let mut child = Command::new(GO_BINARY.as_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn go-html-crosscheck binary");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload(template_str, data).as_bytes())
            .expect("failed to write to go stdin");
        child.wait_with_output().expect("go-html-crosscheck failed")
    }

    pub fn check(template_str: &str, data: &Value, rust_result: &str) {
        let output = run_go(template_str, data);
        assert!(
            output.status.success(),
            "Go html crosscheck failed for {:?}:\n{}",
            template_str,
            String::from_utf8_lossy(&output.stderr)
        );
        let go_result = String::from_utf8(output.stdout).expect("non-UTF-8 Go output");
        assert_eq!(
            rust_result, &go_result,
            "Rust/Go html mismatch for template: {:?}\n  Rust: {:?}\n  Go:   {:?}",
            template_str, rust_result, go_result
        );
    }

    pub fn check_fails(template_str: &str, data: &Value) {
        let output = run_go(template_str, data);
        assert!(
            !output.status.success(),
            "Go html crosscheck: {:?} was expected to fail but Go produced {:?}",
            template_str,
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

fn run(input: &str, data: &Value) -> Result<String, String> {
    Template::new("test")
        .parse(input)
        .map_err(|e| e.to_string())?
        .execute_to_string(data)
        .map_err(|e| e.to_string())
}

/// Assert Rust output equals `expected`, then (under `go-crosscheck`) that Go
/// agrees.
fn ok(input: &str, data: &Value, expected: &str) {
    match run(input, data) {
        Ok(result) => {
            assert_eq!(result, expected, "template: {input}");
            #[cfg(all(feature = "go-crosscheck", feature = "std"))]
            go_crosscheck::check(input, data, &result);
        }
        Err(e) => panic!("template {input:?} failed: {e}"),
    }
}

/// Assert Rust rejects the template (escaping error), and Go does too.
fn fail(input: &str, data: &Value) {
    if let Ok(result) = run(input, data) {
        panic!("template {input:?} should have failed but got {result:?}");
    }
    #[cfg(all(feature = "go-crosscheck", feature = "std"))]
    go_crosscheck::check_fails(input, data);
}

fn s(v: &str) -> Value {
    Value::String(v.into())
}

// ---------------------------------------------------------------------------
// HTML text, attribute, RCDATA, and comment contexts.
// ---------------------------------------------------------------------------

#[test]
fn html_text() {
    ok(
        "{{.}}",
        &s("<b> & \"quote\" 'apos'"),
        "&lt;b&gt; &amp; &#34;quote&#34; &#39;apos&#39;",
    );
}

#[test]
fn quoted_attr() {
    ok(
        "<a title=\"{{.}}\">",
        &s("i <3 you & \"x\""),
        "<a title=\"i &lt;3 you &amp; &#34;x&#34;\">",
    );
}

#[test]
fn unquoted_attr() {
    ok("<a title={{.}}>", &s("a b"), "<a title=a&#32;b>");
}

#[test]
fn rcdata_textarea() {
    ok(
        "<textarea>{{.}}</textarea>",
        &s("</textarea><b>"),
        "<textarea>&lt;/textarea&gt;&lt;b&gt;</textarea>",
    );
}

#[test]
fn html_comment_elided() {
    ok(
        "<b>Hi <!-- c -->{{.}}</b>",
        &s("<x>"),
        "<b>Hi &lt;x&gt;</b>",
    );
}

// ---------------------------------------------------------------------------
// URL and srcset contexts.
// ---------------------------------------------------------------------------

#[test]
fn url_unsafe_scheme_filtered() {
    ok(
        "<a href=\"{{.}}\">",
        &s("javascript:alert(1)"),
        "<a href=\"#ZgotmplZ\">",
    );
}

#[test]
fn url_query_escaped() {
    ok(
        "<a href=\"/search?q={{.}}\">",
        &s("1 & 2"),
        "<a href=\"/search?q=1%20%26%202\">",
    );
}

// ---------------------------------------------------------------------------
// JavaScript contexts.
// ---------------------------------------------------------------------------

#[test]
fn js_value_string() {
    ok(
        "<script>var s = {{.}}</script>",
        &s("</script>"),
        "<script>var s = \"\\u003c/script\\u003e\"</script>",
    );
}

#[test]
fn js_value_int_padded() {
    // A numeric JS value is space-padded so it can't merge with adjacent tokens.
    ok(
        "<script>var n = {{.}}</script>",
        &Value::Int(42),
        "<script>var n =  42 </script>",
    );
}

#[test]
fn js_value_map_json() {
    let data = tmap! { "k" => "</script>" };
    ok(
        "<script>var o = {{.}}</script>",
        &data,
        "<script>var o = {\"k\":\"\\u003c/script\\u003e\"}</script>",
    );
}

// ---------------------------------------------------------------------------
// CSS context.
// ---------------------------------------------------------------------------

#[test]
fn css_value_ok() {
    ok(
        "<style>p{color:{{.}}}</style>",
        &s("red"),
        "<style>p{color:red}</style>",
    );
}

#[test]
fn css_value_dangerous_defanged() {
    ok(
        "<style>p{color:{{.}}}</style>",
        &s("expression(alert(1))"),
        "<style>p{color:ZgotmplZ}</style>",
    );
}

// ---------------------------------------------------------------------------
// Control flow.
// ---------------------------------------------------------------------------

#[test]
fn range_escapes_each() {
    let data = Value::List(vec![s("<a>"), s("<b>")].into());
    ok("{{range .}}{{.}}{{end}}", &data, "&lt;a&gt;&lt;b&gt;");
}

#[test]
fn if_in_attr() {
    ok(
        "<a title=\"{{if .}}x{{else}}y{{end}}\">",
        &Value::Bool(true),
        "<a title=\"x\">",
    );
}

#[test]
fn template_invocation_escapes() {
    ok(
        r#"{{define "x"}}<b>{{.}}</b>{{end}}{{template "x" .}}"#,
        &s("<i>"),
        "<b>&lt;i&gt;</b>",
    );
}

// ---------------------------------------------------------------------------
// Trusted-content pass-through (Value::Safe).
// ---------------------------------------------------------------------------

#[test]
fn safe_html_passthrough_in_text() {
    ok("{{.}}", &HTML::from("<b>ok</b>").into(), "<b>ok</b>");
}

#[test]
fn safe_html_stripped_in_attr() {
    // template.HTML in an attribute value is tag-stripped, not passed through.
    ok(
        "<a title=\"{{.}}\">",
        &HTML::from("<b>x</b>").into(),
        "<a title=\"x\">",
    );
}

#[test]
fn safe_url_passthrough_but_normalized() {
    // template.URL bypasses the scheme filter but is still percent-normalized.
    ok(
        "<a href=\"{{.}}\">",
        &URL::from("javascript:ok()").into(),
        "<a href=\"javascript:ok%28%29\">",
    );
}

// ---------------------------------------------------------------------------
// Negative: a Value::Safe of kind X must NOT bypass escaping in context Y.
// This is the property that makes the module XSS-safe rather than merely an
// escaper; a regression in the per-escaper content-type guard would show here.
// Expectations captured from Go 1.26.4.
// ---------------------------------------------------------------------------

#[test]
fn safe_html_does_not_bypass_url_filter() {
    // template.HTML is not URL-trusted, so the scheme filter still fires.
    ok(
        "<a href=\"{{.}}\">",
        &HTML::from("javascript:alert(1)").into(),
        "<a href=\"#ZgotmplZ\">",
    );
}

#[test]
fn safe_url_does_not_bypass_text_escaping() {
    ok("{{.}}", &URL::from("a<b>c").into(), "a&lt;b&gt;c");
}

#[test]
fn safe_js_does_not_bypass_text_escaping() {
    ok("{{.}}", &JS::from("<b>&").into(), "&lt;b&gt;&amp;");
}

#[test]
fn safe_css_does_not_bypass_text_escaping() {
    ok("{{.}}", &CSS::from("a<b").into(), "a&lt;b");
}

#[test]
fn safe_html_does_not_bypass_js_context() {
    // template.HTML in a <script> value is JSON-quoted, not passed through.
    ok(
        "<script>var x = {{.}}</script>",
        &HTML::from("<b>x</b>").into(),
        r#"<script>var x = "\u003cb\u003ex\u003c/b\u003e"</script>"#,
    );
}

// ---------------------------------------------------------------------------
// Positive: each Value::Safe kind passes through verbatim in its own context.
// Expectations captured from Go 1.26.4.
// ---------------------------------------------------------------------------

#[test]
fn safe_js_passthrough_in_script() {
    ok(
        "<script>{{.}}</script>",
        &JS::from("x = y < 1").into(),
        "<script>x = y < 1</script>",
    );
}

#[test]
fn safe_jsstr_passthrough_in_js_string() {
    ok(
        "<script>var s = \"{{.}}\"</script>",
        &JSStr::from(r"a\x3cb").into(),
        r#"<script>var s = "a\x3cb"</script>"#,
    );
}

#[test]
fn safe_css_passthrough_in_style() {
    ok(
        "<style>{{.}}</style>",
        &CSS::from("color: red; width: 2px").into(),
        "<style>color: red; width: 2px</style>",
    );
}

#[test]
fn safe_htmlattr_passthrough_in_tag() {
    ok(
        "<a {{.}}>",
        &HTMLAttr::from("title=\"ok\"").into(),
        "<a title=\"ok\">",
    );
}

#[test]
fn safe_srcset_passthrough_in_srcset() {
    ok(
        "<img srcset=\"{{.}}\">",
        &Srcset::from("a.png 1x, b.png 2x").into(),
        "<img srcset=\"a.png 1x, b.png 2x\">",
    );
}

// ---------------------------------------------------------------------------
// Additional dangerous contexts (plain, untrusted values).
// Expectations captured from Go 1.26.4.
// ---------------------------------------------------------------------------

#[test]
fn srcset_filters_unsafe_url() {
    ok(
        "<img srcset=\"{{.}}\">",
        &s("javascript:alert(1), b.png 2x"),
        "<img srcset=\"#ZgotmplZ, b.png 2x\">",
    );
}

#[test]
fn js_double_quoted_string() {
    ok(
        "<script>var s = \"{{.}}\"</script>",
        &s(r#"he said "hi" </script>"#),
        r#"<script>var s = "he said \u0022hi\u0022 \u003c\/script\u003e"</script>"#,
    );
}

#[test]
fn js_regexp_literal() {
    ok(
        "<script>var r = /{{.}}/</script>",
        &s("a.b"),
        r#"<script>var r = /a\.b/</script>"#,
    );
}

#[test]
fn css_url_filters_unsafe_scheme() {
    ok(
        "<style>a{background:url({{.}})}</style>",
        &s("javascript:alert(1)"),
        "<style>a{background:url(#ZgotmplZ)}</style>",
    );
}

#[test]
fn unquoted_url_attr() {
    ok("<a href={{.}}>", &s("/x?a=1 2"), "<a href=/x?a&#61;1%202>");
}

#[test]
fn attr_name_filtered() {
    // A dynamic attribute name is filtered to the failsafe unless it is
    // template.HTMLAttr-typed (matching Go).
    ok("<a {{.}}=x>", &s("href"), "<a ZgotmplZ=x>");
}

// ---------------------------------------------------------------------------
// Error-path parity: templates Go's escaper rejects, we must reject too.
// ---------------------------------------------------------------------------

#[test]
fn err_ends_in_url_context() {
    fail("<a href=\"{{.}}", &s("x"));
}

#[test]
fn err_ends_in_script_context() {
    fail("<script>var x = 1", &s("x"));
}

#[test]
fn err_branches_end_in_different_contexts() {
    fail(
        "<a {{if .}}href=\"{{else}}title='{{end}}x\">",
        &Value::Bool(true),
    );
}

#[test]
fn err_ambiguous_slash_in_js() {
    fail(
        "<script>{{if true}}var x = 1{{end}}/foo/g{{.}}</script>",
        &s("y"),
    );
}

// ---------------------------------------------------------------------------
// TestTypedContent — the full type × context matrix.
//
// Ported byte-for-byte from Go 1.26.4 `html/template` content_test.go. Each of
// the nine typed values below is run through every context template; the
// `want` strings are the interpolated slice (Go strips the literal prefix and
// suffix around `{{.}}`), so we reassemble the full expected output as
// `prefix + want + suffix`. `ZgotmplZ` / `#ZgotmplZ` marks a value whose
// content type is rejected in that context. Raw strings preserve the literal
// backslashes of JS `\u` escapes verbatim.
// ---------------------------------------------------------------------------

#[test]
fn typed_content_matrix() {
    // Go's `data` slice, in order. Index comments track the value under test.
    let data: [Value; 9] = [
        s(r#"<b> "foo%" O'Reilly &bar;"#), // 0: plain string
        CSS::from(r#"a[href =~ "//example.com"]#foo"#).into(), // 1: template.CSS
        HTML::from("Hello, <b>World</b> &amp;tc!").into(), // 2: template.HTML
        HTMLAttr::from(r#" dir="ltr""#).into(), // 3: template.HTMLAttr
        JS::from(r#"c && alert("Hello, World!");"#).into(), // 4: template.JS
        JSStr::from(r"Hello, World & O'Reilly\u0021").into(), // 5: template.JSStr (literal \u0021)
        URL::from("greeting=H%69,&addressee=(World)").into(), // 6: template.URL
        Srcset::from("greeting=H%69,&addressee=(World) 2x, https://golang.org/favicon.ico 500.5w")
            .into(), // 7: template.Srcset
        URL::from(",foo/,").into(),        // 8: template.URL
    ];

    // (template, [want for each of the nine values]); the `want` is the
    // interpolated slice (literal prefix/suffix stripped). Generated from Go
    // 1.26.4 html/template content_test.go.
    let cases: &[(&str, [&str; 9])] = &[
        (
            r##"<style>{{.}} { color: blue }</style>"##,
            [
                r##"ZgotmplZ"##,
                r##"a[href =~ "//example.com"]#foo"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
            ],
        ),
        (
            r##"<div style="{{.}}">"##,
            [
                r##"ZgotmplZ"##,
                r##"a[href =~ &#34;//example.com&#34;]#foo"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
            ],
        ),
        (
            r##"{{.}}"##,
            [
                r##"&lt;b&gt; &#34;foo%&#34; O&#39;Reilly &amp;bar;"##,
                r##"a[href =~ &#34;//example.com&#34;]#foo"##,
                r##"Hello, <b>World</b> &amp;tc!"##,
                r##" dir=&#34;ltr&#34;"##,
                r##"c &amp;&amp; alert(&#34;Hello, World!&#34;);"##,
                r##"Hello, World &amp; O&#39;Reilly\u0021"##,
                r##"greeting=H%69,&amp;addressee=(World)"##,
                r##"greeting=H%69,&amp;addressee=(World) 2x, https://golang.org/favicon.ico 500.5w"##,
                r##",foo/,"##,
            ],
        ),
        (
            r##"<a{{.}}>"##,
            [
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##" dir="ltr""##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
                r##"ZgotmplZ"##,
            ],
        ),
        (
            r##"<a title={{.}}>"##,
            [
                r##"&lt;b&gt;&#32;&#34;foo%&#34;&#32;O&#39;Reilly&#32;&amp;bar;"##,
                r##"a[href&#32;&#61;~&#32;&#34;//example.com&#34;]#foo"##,
                r##"Hello,&#32;World&#32;&amp;tc!"##,
                r##"&#32;dir&#61;&#34;ltr&#34;"##,
                r##"c&#32;&amp;&amp;&#32;alert(&#34;Hello,&#32;World!&#34;);"##,
                r##"Hello,&#32;World&#32;&amp;&#32;O&#39;Reilly\u0021"##,
                r##"greeting&#61;H%69,&amp;addressee&#61;(World)"##,
                r##"greeting&#61;H%69,&amp;addressee&#61;(World)&#32;2x,&#32;https://golang.org/favicon.ico&#32;500.5w"##,
                r##",foo/,"##,
            ],
        ),
        (
            r##"<a title='{{.}}'>"##,
            [
                r##"&lt;b&gt; &#34;foo%&#34; O&#39;Reilly &amp;bar;"##,
                r##"a[href =~ &#34;//example.com&#34;]#foo"##,
                r##"Hello, World &amp;tc!"##,
                r##" dir=&#34;ltr&#34;"##,
                r##"c &amp;&amp; alert(&#34;Hello, World!&#34;);"##,
                r##"Hello, World &amp; O&#39;Reilly\u0021"##,
                r##"greeting=H%69,&amp;addressee=(World)"##,
                r##"greeting=H%69,&amp;addressee=(World) 2x, https://golang.org/favicon.ico 500.5w"##,
                r##",foo/,"##,
            ],
        ),
        (
            r##"<textarea>{{.}}</textarea>"##,
            [
                r##"&lt;b&gt; &#34;foo%&#34; O&#39;Reilly &amp;bar;"##,
                r##"a[href =~ &#34;//example.com&#34;]#foo"##,
                r##"Hello, &lt;b&gt;World&lt;/b&gt; &amp;tc!"##,
                r##" dir=&#34;ltr&#34;"##,
                r##"c &amp;&amp; alert(&#34;Hello, World!&#34;);"##,
                r##"Hello, World &amp; O&#39;Reilly\u0021"##,
                r##"greeting=H%69,&amp;addressee=(World)"##,
                r##"greeting=H%69,&amp;addressee=(World) 2x, https://golang.org/favicon.ico 500.5w"##,
                r##",foo/,"##,
            ],
        ),
        (
            r##"<script>alert({{.}})</script>"##,
            [
                r##""\u003cb\u003e \"foo%\" O'Reilly \u0026bar;""##,
                r##""a[href =~ \"//example.com\"]#foo""##,
                r##""Hello, \u003cb\u003eWorld\u003c/b\u003e \u0026amp;tc!""##,
                r##"" dir=\"ltr\"""##,
                r##"c && alert("Hello, World!");"##,
                r##""Hello, World & O'Reilly\u0021""##,
                r##""greeting=H%69,\u0026addressee=(World)""##,
                r##""greeting=H%69,\u0026addressee=(World) 2x, https://golang.org/favicon.ico 500.5w""##,
                r##"",foo/,""##,
            ],
        ),
        (
            r##"<button onclick="alert({{.}})">"##,
            [
                r##"&#34;\u003cb\u003e \&#34;foo%\&#34; O&#39;Reilly \u0026bar;&#34;"##,
                r##"&#34;a[href =~ \&#34;//example.com\&#34;]#foo&#34;"##,
                r##"&#34;Hello, \u003cb\u003eWorld\u003c/b\u003e \u0026amp;tc!&#34;"##,
                r##"&#34; dir=\&#34;ltr\&#34;&#34;"##,
                r##"c &amp;&amp; alert(&#34;Hello, World!&#34;);"##,
                r##"&#34;Hello, World &amp; O&#39;Reilly\u0021&#34;"##,
                r##"&#34;greeting=H%69,\u0026addressee=(World)&#34;"##,
                r##"&#34;greeting=H%69,\u0026addressee=(World) 2x, https://golang.org/favicon.ico 500.5w&#34;"##,
                r##"&#34;,foo/,&#34;"##,
            ],
        ),
        (
            r##"<script>alert("{{.}}")</script>"##,
            [
                r##"\u003cb\u003e \u0022foo%\u0022 O\u0027Reilly \u0026bar;"##,
                r##"a[href =~ \u0022\/\/example.com\u0022]#foo"##,
                r##"Hello, \u003cb\u003eWorld\u003c\/b\u003e \u0026amp;tc!"##,
                r##" dir=\u0022ltr\u0022"##,
                r##"c \u0026\u0026 alert(\u0022Hello, World!\u0022);"##,
                r##"Hello, World \u0026 O\u0027Reilly\u0021"##,
                r##"greeting=H%69,\u0026addressee=(World)"##,
                r##"greeting=H%69,\u0026addressee=(World) 2x, https:\/\/golang.org\/favicon.ico 500.5w"##,
                r##",foo\/,"##,
            ],
        ),
        (
            r##"<script type="text/javascript">alert("{{.}}")</script>"##,
            [
                r##"\u003cb\u003e \u0022foo%\u0022 O\u0027Reilly \u0026bar;"##,
                r##"a[href =~ \u0022\/\/example.com\u0022]#foo"##,
                r##"Hello, \u003cb\u003eWorld\u003c\/b\u003e \u0026amp;tc!"##,
                r##" dir=\u0022ltr\u0022"##,
                r##"c \u0026\u0026 alert(\u0022Hello, World!\u0022);"##,
                r##"Hello, World \u0026 O\u0027Reilly\u0021"##,
                r##"greeting=H%69,\u0026addressee=(World)"##,
                r##"greeting=H%69,\u0026addressee=(World) 2x, https:\/\/golang.org\/favicon.ico 500.5w"##,
                r##",foo\/,"##,
            ],
        ),
        (
            r##"<script type="text/javascript">alert({{.}})</script>"##,
            [
                r##""\u003cb\u003e \"foo%\" O'Reilly \u0026bar;""##,
                r##""a[href =~ \"//example.com\"]#foo""##,
                r##""Hello, \u003cb\u003eWorld\u003c/b\u003e \u0026amp;tc!""##,
                r##"" dir=\"ltr\"""##,
                r##"c && alert("Hello, World!");"##,
                r##""Hello, World & O'Reilly\u0021""##,
                r##""greeting=H%69,\u0026addressee=(World)""##,
                r##""greeting=H%69,\u0026addressee=(World) 2x, https://golang.org/favicon.ico 500.5w""##,
                r##"",foo/,""##,
            ],
        ),
        (
            r##"<script type="text/template">{{.}}</script>"##,
            [
                r##"&lt;b&gt; &#34;foo%&#34; O&#39;Reilly &amp;bar;"##,
                r##"a[href =~ &#34;//example.com&#34;]#foo"##,
                r##"Hello, <b>World</b> &amp;tc!"##,
                r##" dir=&#34;ltr&#34;"##,
                r##"c &amp;&amp; alert(&#34;Hello, World!&#34;);"##,
                r##"Hello, World &amp; O&#39;Reilly\u0021"##,
                r##"greeting=H%69,&amp;addressee=(World)"##,
                r##"greeting=H%69,&amp;addressee=(World) 2x, https://golang.org/favicon.ico 500.5w"##,
                r##",foo/,"##,
            ],
        ),
        (
            r##"<button onclick='alert("{{.}}")'>"##,
            [
                r##"\u003cb\u003e \u0022foo%\u0022 O\u0027Reilly \u0026bar;"##,
                r##"a[href =~ \u0022\/\/example.com\u0022]#foo"##,
                r##"Hello, \u003cb\u003eWorld\u003c\/b\u003e \u0026amp;tc!"##,
                r##" dir=\u0022ltr\u0022"##,
                r##"c \u0026\u0026 alert(\u0022Hello, World!\u0022);"##,
                r##"Hello, World \u0026 O\u0027Reilly\u0021"##,
                r##"greeting=H%69,\u0026addressee=(World)"##,
                r##"greeting=H%69,\u0026addressee=(World) 2x, https:\/\/golang.org\/favicon.ico 500.5w"##,
                r##",foo\/,"##,
            ],
        ),
        (
            r##"<a href="?q={{.}}">"##,
            [
                r##"%3cb%3e%20%22foo%25%22%20O%27Reilly%20%26bar%3b"##,
                r##"a%5bhref%20%3d~%20%22%2f%2fexample.com%22%5d%23foo"##,
                r##"Hello%2c%20%3cb%3eWorld%3c%2fb%3e%20%26amp%3btc%21"##,
                r##"%20dir%3d%22ltr%22"##,
                r##"c%20%26%26%20alert%28%22Hello%2c%20World%21%22%29%3b"##,
                r##"Hello%2c%20World%20%26%20O%27Reilly%5cu0021"##,
                r##"greeting=H%69,&amp;addressee=%28World%29"##,
                r##"greeting%3dH%2569%2c%26addressee%3d%28World%29%202x%2c%20https%3a%2f%2fgolang.org%2ffavicon.ico%20500.5w"##,
                r##",foo/,"##,
            ],
        ),
        (
            r##"<style>body { background: url('?img={{.}}') }</style>"##,
            [
                r##"%3cb%3e%20%22foo%25%22%20O%27Reilly%20%26bar%3b"##,
                r##"a%5bhref%20%3d~%20%22%2f%2fexample.com%22%5d%23foo"##,
                r##"Hello%2c%20%3cb%3eWorld%3c%2fb%3e%20%26amp%3btc%21"##,
                r##"%20dir%3d%22ltr%22"##,
                r##"c%20%26%26%20alert%28%22Hello%2c%20World%21%22%29%3b"##,
                r##"Hello%2c%20World%20%26%20O%27Reilly%5cu0021"##,
                r##"greeting=H%69,&addressee=%28World%29"##,
                r##"greeting%3dH%2569%2c%26addressee%3d%28World%29%202x%2c%20https%3a%2f%2fgolang.org%2ffavicon.ico%20500.5w"##,
                r##",foo/,"##,
            ],
        ),
        (
            r##"<img srcset="{{.}}">"##,
            [
                r##"#ZgotmplZ"##,
                r##"#ZgotmplZ"##,
                r##"Hello,#ZgotmplZ"##,
                r##" dir=%22ltr%22"##,
                r##"#ZgotmplZ, World!%22%29;"##,
                r##"Hello,#ZgotmplZ"##,
                r##"greeting=H%69%2c&amp;addressee=%28World%29"##,
                r##"greeting=H%69,&amp;addressee=(World) 2x, https://golang.org/favicon.ico 500.5w"##,
                r##"%2cfoo/%2c"##,
            ],
        ),
        (
            r##"<img srcset={{.}}>"##,
            [
                r##"#ZgotmplZ"##,
                r##"#ZgotmplZ"##,
                r##"Hello,#ZgotmplZ"##,
                r##"&#32;dir&#61;%22ltr%22"##,
                r##"#ZgotmplZ,&#32;World!%22%29;"##,
                r##"Hello,#ZgotmplZ"##,
                r##"greeting&#61;H%69%2c&amp;addressee&#61;%28World%29"##,
                r##"greeting&#61;H%69,&amp;addressee&#61;(World)&#32;2x,&#32;https://golang.org/favicon.ico&#32;500.5w"##,
                r##"%2cfoo/%2c"##,
            ],
        ),
        (
            r##"<img srcset="{{.}} 2x, https://golang.org/ 500.5w">"##,
            [
                r##"#ZgotmplZ"##,
                r##"#ZgotmplZ"##,
                r##"Hello,#ZgotmplZ"##,
                r##" dir=%22ltr%22"##,
                r##"#ZgotmplZ, World!%22%29;"##,
                r##"Hello,#ZgotmplZ"##,
                r##"greeting=H%69%2c&amp;addressee=%28World%29"##,
                r##"greeting=H%69,&amp;addressee=(World) 2x, https://golang.org/favicon.ico 500.5w"##,
                r##"%2cfoo/%2c"##,
            ],
        ),
        (
            r##"<img srcset="http://godoc.org/ {{.}}, https://golang.org/ 500.5w">"##,
            [
                r##"#ZgotmplZ"##,
                r##"#ZgotmplZ"##,
                r##"Hello,#ZgotmplZ"##,
                r##" dir=%22ltr%22"##,
                r##"#ZgotmplZ, World!%22%29;"##,
                r##"Hello,#ZgotmplZ"##,
                r##"greeting=H%69%2c&amp;addressee=%28World%29"##,
                r##"greeting=H%69,&amp;addressee=(World) 2x, https://golang.org/favicon.ico 500.5w"##,
                r##"%2cfoo/%2c"##,
            ],
        ),
        (
            r##"<img srcset="http://godoc.org/?q={{.}} 2x, https://golang.org/ 500.5w">"##,
            [
                r##"#ZgotmplZ"##,
                r##"#ZgotmplZ"##,
                r##"Hello,#ZgotmplZ"##,
                r##" dir=%22ltr%22"##,
                r##"#ZgotmplZ, World!%22%29;"##,
                r##"Hello,#ZgotmplZ"##,
                r##"greeting=H%69%2c&amp;addressee=%28World%29"##,
                r##"greeting=H%69,&amp;addressee=(World) 2x, https://golang.org/favicon.ico 500.5w"##,
                r##"%2cfoo/%2c"##,
            ],
        ),
        (
            r##"<img srcset="http://godoc.org/ 2x, {{.}} 500.5w">"##,
            [
                r##"#ZgotmplZ"##,
                r##"#ZgotmplZ"##,
                r##"Hello,#ZgotmplZ"##,
                r##" dir=%22ltr%22"##,
                r##"#ZgotmplZ, World!%22%29;"##,
                r##"Hello,#ZgotmplZ"##,
                r##"greeting=H%69%2c&amp;addressee=%28World%29"##,
                r##"greeting=H%69,&amp;addressee=(World) 2x, https://golang.org/favicon.ico 500.5w"##,
                r##"%2cfoo/%2c"##,
            ],
        ),
        (
            r##"<img srcset="http://godoc.org/ 2x, https://golang.org/ {{.}}">"##,
            [
                r##"#ZgotmplZ"##,
                r##"#ZgotmplZ"##,
                r##"Hello,#ZgotmplZ"##,
                r##" dir=%22ltr%22"##,
                r##"#ZgotmplZ, World!%22%29;"##,
                r##"Hello,#ZgotmplZ"##,
                r##"greeting=H%69%2c&amp;addressee=%28World%29"##,
                r##"greeting=H%69,&amp;addressee=(World) 2x, https://golang.org/favicon.ico 500.5w"##,
                r##"%2cfoo/%2c"##,
            ],
        ),
    ];

    const MARKER: &str = "{{.}}";
    for (tmpl, wants) in cases {
        let idx = tmpl.find(MARKER).expect("template must contain {{.}}");
        let prefix = &tmpl[..idx];
        let suffix = &tmpl[idx + MARKER.len()..];
        for (i, want) in wants.iter().enumerate() {
            let expected = format!("{prefix}{want}{suffix}");
            ok(tmpl, &data[i], &expected);
        }
    }
}

// ---------------------------------------------------------------------------
// `<script type=...>` MIME-type classification.
//
// A value in a `<script>` body is JS-escaped only when the element's `type`
// denotes JavaScript (Go's `isJSType`); otherwise the element is ordinary
// RCDATA-free text and the value is HTML-escaped. Expectations captured from
// Go 1.26.4 (see js_test.go `TestIsJsMimeType` for the classifier's table).
// ---------------------------------------------------------------------------

#[test]
fn script_type_mime_classification() {
    // JS MIME types: a bare value in the script body is JS-value escaped,
    // so the string is JSON-quoted and `<`/`>` become `\u003c`/`\u003e`.
    for ty in [
        "text/javascript",
        "application/json",
        "application/ld+json",
        "module",
        "text/javascript;version=1.8",
    ] {
        ok(
            &format!(r#"<script type="{ty}">alert({{{{.}}}})</script>"#),
            &s("</script>"),
            &format!(r#"<script type="{ty}">alert("\u003c/script\u003e")</script>"#),
        );
    }
    // Non-JS types: the body is HTML text, so `</script>` is entity-escaped.
    for ty in ["text/template", "text/plain"] {
        ok(
            &format!(r#"<script type="{ty}">alert({{{{.}}}})</script>"#),
            &s("</script>"),
            &format!(r#"<script type="{ty}">alert(&lt;/script&gt;)</script>"#),
        );
    }
}

// ---------------------------------------------------------------------------
// Strings inside a `<script type="application/ld+json">` string literal.
//
// Ported from Go's `TestStringsInScriptsWithJsonContentTypeAreCorrectlyEscaped`
// (issues #33671 / #37634). A JS/JSON content type still routes the value
// through the JS-string escaper, so control characters and HTML-significant
// bytes are `\u`-escaped rather than left raw. Expectations captured from Go
// 1.26.4.
// ---------------------------------------------------------------------------

#[test]
fn json_content_type_script_string() {
    let tmpl = r#"<script type="application/ld+json">"{{.}}"</script>"#;
    // (input, interpolated want). Wants generated from Go 1.26.4; the
    // backslashes are literal (part of the emitted `\uXXXX` / `\t` escapes).
    let cases: &[(&str, &str)] = &[
        ("", r##""##),
        ("\u{FFFD}", "\u{FFFD}"),
        ("\u{0}", r##"\u0000"##),
        ("\u{1F}", r##"\u001f"##),
        ("\t", r##"\t"##),
        ("<>", r##"\u003c\u003e"##),
        ("'\"", r##"\u0027\u0022"##),
        ("ASCII letters", r##"ASCII letters"##),
        (
            "\u{0295}\u{2299}\u{03D6}\u{2299}\u{0294}",
            "\u{0295}\u{2299}\u{03D6}\u{2299}\u{0294}",
        ),
        ("\u{1F355}", "\u{1F355}"),
    ];
    let prefix = r#"<script type="application/ld+json">""#;
    let suffix = r#""</script>"#;
    for &(input, want) in cases {
        ok(tmpl, &s(input), &format!("{prefix}{want}{suffix}"));
    }
}

// ---------------------------------------------------------------------------
// Empty value in an unquoted attribute → `ZgotmplZ`.
//
// Locks in that `html/template`'s nospace escaper fails an empty unquoted
// attribute value shut (an empty `title=` merges with the following token),
// matching Go 1.26.4. Complements the `unquotedEmptyAttributeValuePlaintext`
// row of the ported TestEscape table, which uses a nil (not empty-string)
// value.
// ---------------------------------------------------------------------------

#[test]
fn nospace_empty_unquoted_attr() {
    ok("<p title={{.}}>", &s(""), "<p title=ZgotmplZ>");
    ok("<p title={{.}}>", &s("a b"), "<p title=a&#32;b>");
}

// ---------------------------------------------------------------------------
// Clone isolation (Rust `Template::clone`).
//
// Go's clone_test.go / template_test.go assert that a clone escapes
// independently of its source. The Rust `Clone` impl deliberately resets the
// escape cache so a clone starts un-escaped (see html/mod.rs), which is what
// makes these behaviors hold. These are Rust-API tests (Go's associated-
// template semantics don't map 1:1), so they don't cross-check.
// ---------------------------------------------------------------------------

#[test]
fn clone_escapes_independently_after_source_execute() {
    let base = Template::new("t").parse("<p>{{.}}</p>").unwrap();
    let clone = base.clone();
    // Executing the source freezes (escapes) it.
    assert_eq!(
        base.execute_to_string(&s("<x>")).unwrap(),
        "<p>&lt;x&gt;</p>"
    );
    // The clone still escapes correctly and identically on its own execute.
    assert_eq!(
        clone.execute_to_string(&s("<y>")).unwrap(),
        "<p>&lt;y&gt;</p>"
    );
}

#[test]
fn clone_can_be_parsed_into_after_source_execute() {
    let base = Template::new("t").parse("<p>{{.}}</p>").unwrap();
    let clone = base.clone();
    let _ = base.execute_to_string(&s("x")).unwrap();
    // The source is now frozen and rejects further parsing...
    assert!(base.clone().parse("x").is_ok()); // (a fresh clone can, though)
    // ...while the clone, never executed, accepts more definitions and escapes
    // them in the correct (URL) context.
    let clone = clone
        .parse(r#"{{define "extra"}}<a href="{{.}}">{{end}}"#)
        .unwrap();
    assert_eq!(
        clone
            .execute_template_to_string("extra", &s("javascript:alert(1)"))
            .unwrap(),
        r##"<a href="#ZgotmplZ">"##
    );
}

#[test]
fn clone_preserves_registered_funcs() {
    let base = Template::new("t")
        .func("shout", |args| {
            let v = args.first().and_then(|v| v.as_str()).unwrap_or_default();
            Ok(Value::String(v.to_uppercase().into()))
        })
        .parse("<p>{{shout .}}</p>")
        .unwrap();
    let clone = base.clone();
    assert_eq!(
        clone.execute_to_string(&s("hi <b>")).unwrap(),
        "<p>HI &lt;B&gt;</p>",
    );
}

// ---------------------------------------------------------------------------
// TestEscapeMap — a data field whose name collides with a predefined escaper
// (`html`, `urlquery`) is an ordinary field lookup, not the escaper function
// (Go issue 20323). Expectations captured from Go 1.26.4.
// ---------------------------------------------------------------------------

#[test]
fn escape_map_field_named_like_escaper() {
    let data = tmap! {
        "html" => "<h1>Hi!</h1>",
        "urlquery" => "http://www.foo.com/index.html?title=main",
    };
    ok("{{.html | print}}", &data, "&lt;h1&gt;Hi!&lt;/h1&gt;");
    ok(
        "{{.urlquery | print}}",
        &data,
        "http://www.foo.com/index.html?title=main",
    );
}

// ---------------------------------------------------------------------------
// TestIdempotentExecute — escaping is applied exactly once. Re-executing a
// template (directly, and implicitly through `{{template}}`) must not re-run
// the escaper, which would over-escape `&` to `&amp;amp;`. This guards the
// eager one-shot escape cache.
// ---------------------------------------------------------------------------

#[test]
fn idempotent_execute() {
    let t = Template::new("")
        .parse(r#"{{define "main"}}<body>{{template "hello"}}</body>{{end}}"#)
        .unwrap()
        .parse(r#"{{define "hello"}}Hello, {{"Ladies & Gentlemen!"}}{{end}}"#)
        .unwrap();
    // "hello" produces the same output when executed twice.
    for _ in 0..2 {
        assert_eq!(
            t.execute_template_to_string("hello", &Value::Nil).unwrap(),
            "Hello, Ladies &amp; Gentlemen!",
        );
    }
    // The implicit re-execution of "hello" inside "main" does not re-escape it.
    assert_eq!(
        t.execute_template_to_string("main", &Value::Nil).unwrap(),
        "<body>Hello, Ladies &amp; Gentlemen!</body>",
    );
}

// ---------------------------------------------------------------------------
// TestEscapeErrorsNotIgnorable / TestEscapeSetErrorsNotIgnorable — a template
// that fails to escape returns an error AND emits no output (a partial,
// unescaped prefix must never reach the writer).
// ---------------------------------------------------------------------------

#[test]
fn escape_errors_produce_no_output() {
    // Top-level template that ends mid-tag.
    let t = Template::new("dangerous").parse("<a").unwrap();
    let mut buf = String::new();
    assert!(t.execute_fmt(&mut buf, &Value::Nil).is_err());
    assert!(buf.is_empty(), "emitted output despite failure: {buf:?}");

    // Same, reached through a template set / named execution.
    let t = Template::new("root")
        .parse(r#"{{define "t"}}<a{{end}}"#)
        .unwrap();
    let mut buf = String::new();
    assert!(t.execute_template_fmt(&mut buf, "t", &Value::Nil).is_err());
    assert!(buf.is_empty(), "emitted output despite failure: {buf:?}");
}

// ---------------------------------------------------------------------------
// TestAliasedParseTreeDoesNotOverescape — aliasing a single parse tree under
// two names must not double-escape when both are executed.
// ---------------------------------------------------------------------------

#[test]
fn aliased_parse_tree_does_not_overescape() {
    let t = Template::new("foo").parse("{{.}}").unwrap();
    let tree = t.lookup("foo").expect("foo tree").clone();
    let t = t.add_parse_tree("bar", tree).unwrap();
    let got_foo = t.execute_template_to_string("foo", &s("<baz>")).unwrap();
    let got_bar = t.execute_template_to_string("bar", &s("<baz>")).unwrap();
    assert_eq!(got_foo, "&lt;baz&gt;");
    assert_eq!(got_foo, got_bar);
}
