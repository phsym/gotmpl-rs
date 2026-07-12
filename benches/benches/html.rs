//! Criterion benchmarks for `gotmpl::html`, the context-aware auto-escaping
//! template that mirrors Go's `html/template`.
//!
//! The scenarios match `benches/go/html_test.go`, so the Rust and Go numbers
//! line up directly.
//!
//! There are two costs to measure:
//!   * `escape/*` builds a fresh template every iteration and executes it once,
//!     so it pays for both the build and the escaping analysis that each engine
//!     runs lazily on the first execute.
//!   * `exec/*` reuses an already-escaped template across iterations, the way a
//!     server renders the same page many times after parsing it once.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use gotmpl::html::{HTML, Template};
use gotmpl::{ToValue, Value, tmap};

// Single-context templates isolate the per-context escaper cost.
const SRC_TEXT: &str = "<p>{{.}}</p>";
const SRC_ATTR: &str = r#"<div class="{{.}}"></div>"#;
const SRC_URL: &str = r#"<a href="{{.}}">x</a>"#;
const SRC_JS: &str = r#"<script>var x = "{{.}}";</script>"#;

// A realistic page: escapes user data across HTML-text, attribute, URL, and
// JS-string contexts while ranging over a list, plus one trusted-content field
// that bypasses escaping.
const SRC_PAGE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head><title>{{.Title}}</title></head>
<body>
<h1 class="{{.HeadingClass}}">{{.Title}}</h1>
<nav><a href="{{.HomeURL}}">Home</a></nav>
<ul class="posts">
{{- range .Posts}}
  <li data-id="{{.ID}}">
    <a href="{{.URL}}" title="{{.Title}}">{{.Title}}</a>
    <span class="author">{{.Author}}</span>
    <p>{{.Body}}</p>
  </li>
{{- end}}
</ul>
<footer>{{.Footer}}</footer>
<script>var pageId = "{{.PageID}}", count = {{.Count}};</script>
</body>
</html>"#;

// A value with characters that need escaping in every context, so the escapers
// do real work rather than a no-op passthrough.
fn spicy() -> Value {
    r#"a & b < c > "d" 'e' /f/"#.to_value()
}

fn data_page() -> Value {
    let posts: Vec<Value> = (0..20)
        .map(|i| {
            tmap! {
                "ID"     => i as i64,
                "URL"    => format!("/posts/{i}?ref=home&sort=new"),
                "Title"  => format!("Post #{i}: \"tips & tricks\""),
                "Author" => format!("user{i} <team & co>"),
                "Body"   => format!(
                    "A short body for post {i}. Uses <em>markup</em> & \"quotes\" \
                     that must be escaped in HTML text."
                ),
            }
        })
        .collect();
    tmap! {
        "Title"        => "My Blog & Notes",
        "HeadingClass" => "main headline",
        "HomeURL"      => "/home?tab=recent&lang=en",
        "Posts"        => posts,
        "Footer"       => HTML::from("<small>&copy; 2026 <b>Example</b></small>"),
        "PageID"       => "abc-123",
        "Count"        => 20i64,
    }
}

fn bench_escape(c: &mut Criterion) {
    let mut g = c.benchmark_group("html_escape");
    let data = data_page();
    g.bench_function("page", |b| {
        b.iter(|| {
            let out = Template::new("page")
                .parse(black_box(SRC_PAGE))
                .expect("parse page")
                .execute_to_string(black_box(&data))
                .expect("exec page");
            black_box(out);
        });
    });
    g.finish();
}

fn bench_exec(c: &mut Criterion) {
    let mut g = c.benchmark_group("html_exec");

    let mut single = |name: &str, src: &str| {
        let tmpl = Template::new("").parse(src).expect("parse single");
        let data = spicy();
        // Prime the escaper so the timed loop measures the render only.
        tmpl.execute_to_string(&data).expect("prime");
        let mut buf = String::new();
        g.bench_function(name, |b| {
            b.iter(|| {
                buf.clear();
                tmpl.execute_fmt(&mut buf, black_box(&data))
                    .expect("exec single");
            });
        });
    };
    single("text", SRC_TEXT);
    single("attr", SRC_ATTR);
    single("url", SRC_URL);
    single("js", SRC_JS);

    let tmpl_page = Template::new("page").parse(SRC_PAGE).expect("parse page");
    let data = data_page();
    tmpl_page.execute_to_string(&data).expect("prime page");
    let mut buf = String::new();
    g.bench_function("page_20_posts", |b| {
        b.iter(|| {
            buf.clear();
            tmpl_page
                .execute_fmt(&mut buf, black_box(&data))
                .expect("exec page");
        });
    });

    g.finish();
}

criterion_group!(benches, bench_escape, bench_exec);
criterion_main!(benches);
