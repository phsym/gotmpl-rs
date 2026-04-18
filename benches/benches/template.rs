//! Criterion benchmarks for the `gotmpl` crate.
//!
//! Mirrors the scenarios in `benches/go/template_test.go`, so Rust and Go
//! numbers can be compared apples-to-apples.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use gotmpl::{Template, ToValue, Value, tmap};

const SRC_SIMPLE: &str = "Hello, {{.Name}}!";
const SRC_PRINTF: &str = r#"{{printf "%s is %d years old" (.Name) (.Age)}}"#;
const SRC_RANGE: &str = "{{range .}}{{.}},{{end}}";
const SRC_COMPLEX: &str = "\
{{- range .Users -}}
{{- if .Active }}{{printf \"%s <%s> (%d pts)\" (.Name) (.Email) (.Score)}}
{{ end -}}
{{- end -}}";

fn data_simple() -> Value {
    tmap! { "Name" => "World" }
}

fn data_printf() -> Value {
    tmap! { "Name" => "Alice", "Age" => 30i64 }
}

fn data_range() -> Value {
    (0i64..100).collect::<Vec<_>>().to_value()
}

fn data_complex() -> Value {
    let users: Vec<Value> = (0..50)
        .map(|i| {
            tmap! {
                "Name"   => format!("user{i}"),
                "Email"  => format!("user{i}@example.com"),
                "Score"  => (i * 7) as i64,
                "Active" => i % 2 == 0,
            }
        })
        .collect();
    tmap! { "Users" => users }
}

fn bench_parse(c: &mut Criterion) {
    let mut g = c.benchmark_group("parse");
    g.bench_function("simple", |b| {
        b.iter(|| {
            Template::new("")
                .parse(black_box(SRC_SIMPLE))
                .expect("parse simple");
        });
    });
    g.bench_function("complex", |b| {
        b.iter(|| {
            Template::new("")
                .parse(black_box(SRC_COMPLEX))
                .expect("parse complex");
        });
    });
    g.finish();
}

fn bench_exec(c: &mut Criterion) {
    let mut g = c.benchmark_group("exec");

    let tmpl_simple = Template::new("").parse(SRC_SIMPLE).expect("parse simple");
    let data = data_simple();
    let mut buf = String::new();
    g.bench_function("simple", |b| {
        b.iter(|| {
            buf.clear();
            tmpl_simple
                .execute_fmt(&mut buf, black_box(&data))
                .expect("exec simple");
        });
    });

    let tmpl_printf = Template::new("").parse(SRC_PRINTF).expect("parse printf");
    let data = data_printf();
    let mut buf = String::new();
    g.bench_function("printf", |b| {
        b.iter(|| {
            buf.clear();
            tmpl_printf
                .execute_fmt(&mut buf, black_box(&data))
                .expect("exec printf");
        });
    });

    let tmpl_range = Template::new("").parse(SRC_RANGE).expect("parse range");
    let data = data_range();
    let mut buf = String::new();
    g.bench_function("range_100", |b| {
        b.iter(|| {
            buf.clear();
            tmpl_range
                .execute_fmt(&mut buf, black_box(&data))
                .expect("exec range");
        });
    });

    let tmpl_complex = Template::new("").parse(SRC_COMPLEX).expect("parse complex");
    let data = data_complex();
    let mut buf = String::new();
    g.bench_function("complex_50_users", |b| {
        b.iter(|| {
            buf.clear();
            tmpl_complex
                .execute_fmt(&mut buf, black_box(&data))
                .expect("exec complex");
        });
    });

    g.finish();
}

criterion_group!(benches, bench_parse, bench_exec);
criterion_main!(benches);
