# gotmpl

[![Test](https://github.com/phsym/gotmpl-rs/actions/workflows/test.yaml/badge.svg)](https://github.com/phsym/gotmpl-rs/actions/workflows/test.yaml)
[![GitHub License](https://img.shields.io/github/license/phsym/gotmpl-rs)](./LICENSE)
[![Crates.io Version](https://img.shields.io/crates/v/gotmpl)](https://crates.io/crates/gotmpl)
[![docs.rs](https://img.shields.io/docsrs/gotmpl)](https://docs.rs/gotmpl)
[![Crates.io MSRV](https://img.shields.io/crates/msrv/gotmpl)](https://crates.io/crates/gotmpl)

A Rust port of Go's [`text/template`](https://pkg.go.dev/text/template).

Supports the full template syntax (pipelines, control flow, custom functions, template
composition, whitespace trimming) with Go compatible output. `no_std` compatible (with `alloc`).

The crate forbids `unsafe` code and denies the panic-family lints (`panic!`, `unwrap`,
`expect`, `unreachable!`, `todo!`, `unimplemented!`).
User-provided functions that panic are caught under `std`; see `no_std` notes below.

## Quick start

```rust
use gotmpl::{Template, tmap};

let data = tmap! { "Name" => "World" };
let result = Template::new("hello")
    .parse("Hello, {{.Name}}!")
    .unwrap()
    .execute_to_string(&data)
    .unwrap();
assert_eq!(result, "Hello, World!");
```

For one-shot renders with no configuration, use `gotmpl::execute` (source
string) or `gotmpl::execute_file` (reads from disk):

```rust
use gotmpl::{execute, tmap};

let result = execute("Hello, {{.Name}}!", &tmap! { "Name" => "World" }).unwrap();
assert_eq!(result, "Hello, World!");
```

## Template syntax

Actions are delimited by `{{` and `}}` (configurable via `.delims()`).

### Data access

```text
{{.}}              Current context (dot)
{{.Name}}          Field access on dot
{{.User.Email}}    Nested field access
{{$}}              Top-level data (root context)
{{$x}}             Variable access
{{$x.Name}}        Field access on variable
```

### Pipelines

```text
{{.Name | printf "%s!"}}
{{"hello" | len | printf "%d chars"}}
```

### Control flow

```text
{{if .Condition}}...{{end}}
{{if .Cond}}...{{else}}...{{end}}
{{if eq .X 1}}...{{else if eq .X 2}}...{{else}}...{{end}}

{{range .Items}}...{{end}}
{{range .Items}}...{{else}}empty{{end}}
{{range $i, $v := .Items}}{{$i}}: {{$v}}{{end}}
{{range .Items}}{{if eq . 3}}{{break}}{{end}}{{.}}{{end}}
{{range .Items}}{{if eq . 3}}{{continue}}{{end}}{{.}}{{end}}
{{range 5}}{{.}} {{end}}

{{with .Value}}...{{end}}
{{with .Value}}...{{else}}fallback{{end}}
```

### Variables

```text
{{$x := .Name}}            Declare variable
{{$x = "new value"}}       Assign to existing variable
{{$i, $v := range .List}}  Range with index and value (declaration)
{{$i, $v = range .List}}   Range with index and value (assignment)
```

### Template composition

```text
{{define "name"}}...{{end}}        Define a named template
{{template "name" .}}              Invoke a named template
{{block "name" .}}default{{end}}   Define and invoke (with default)
```

### Comments and whitespace trimming

```text
{{/* This is a comment */}}
{{- .X}}     Trim whitespace before
{{.X -}}     Trim whitespace after
{{- .X -}}   Trim both sides
```

## Built-in functions

| Function                           | Description                                                  |
| ---------------------------------- | ------------------------------------------------------------ |
| `print`                            | Concatenate args (spaces between non-string adjacent args)   |
| `printf`                           | Formatted output ([see below](#printf-verbs-and-flags))      |
| `println`                          | Print with spaces between args, trailing newline             |
| `len`                              | Length of string, list, or map                               |
| `index`                            | Index into list or map: `index .List 0`, `index .Map "key"`  |
| `slice`                            | Slice a list or string: `slice .List 1 3`                    |
| `call`                             | Call a function value: `call .Func arg1 arg2`                |
| `eq`, `ne`, `lt`, `le`, `gt`, `ge` | Comparison operators. `eq` supports multi-arg: `eq .X 1 2 3` |
| `and`, `or`                        | Short-circuit logic, return the deciding value               |
| `not`                              | Boolean negation                                             |
| `html`, `js`, `urlquery`           | Escape for HTML, JavaScript, URL query                       |

### `printf` verbs and flags

Format strings follow Go's [`fmt`](https://pkg.go.dev/fmt) syntax:
`%[flags][width][.precision]verb`.

| Verb       | Applies to        | Output                                          |
| ---------- | ----------------- | ----------------------------------------------- |
| `%s`       | any               | Default string representation (`Display`)       |
| `%q`       | string, int(rune) | Go-quoted string, or single-quoted rune literal |
| `%v`       | any               | Default formatted value                         |
| `%d`       | int               | Decimal                                         |
| `%b`       | int               | Binary                                          |
| `%o`       | int               | Octal                                           |
| `%x`, `%X` | int, string       | Lower/upper hex (on strings: hex of each byte)  |
| `%c`       | int               | Unicode scalar                                  |
| `%f`       | float             | Decimal, no exponent                            |
| `%e`, `%E` | float             | Scientific notation (lower/upper `e`)           |
| `%g`, `%G` | float             | `%e`/`%E` for large exponents, else `%f`        |
| `%t`       | bool              | `true` / `false`                                |
| `%%`       | —                 | Literal `%`                                     |

Flags: `-` (left-align), `+` (always sign numerics), ` ` (leading space for non-negative
numerics), `#` (alternate form: `0b`/`0o`/`0x`/`0X` prefix), `0` (zero-pad numerics).
Width and `.precision` are both supported. Mismatched verb/argument pairs produce Go's
`%!v(BADVERB)` / `%!v(MISSING)` markers rather than panicking.

## Custom functions

Register functions before parsing:

```rust
use gotmpl::{Template, tmap};
use gotmpl::Value;

let result = Template::new("test")
    .func("upper", |args| {
        match args.first() {
            Some(Value::String(s)) => Ok(Value::String(s.to_uppercase().into())),
            _ => Ok(Value::Nil),
        }
    })
    .parse("{{.Name | upper}}")
    .unwrap()
    .execute_to_string(&tmap! { "Name" => "hello" })
    .unwrap();
assert_eq!(result, "HELLO");
```

## Function values and `call`

`Value::Function` allows passing callable values through templates:

```rust
extern crate alloc;
use alloc::sync::Arc;
use gotmpl::{Template, tmap};
use gotmpl::{Value, ValueFunc};

let adder: ValueFunc = Arc::new(|args| {
    let sum: i64 = args.iter().filter_map(|a| a.as_int()).sum();
    Ok(Value::Int(sum))
});

let result = Template::new("test")
    .func("getAdder", move |_| Ok(Value::Function(adder.clone())))
    .parse("{{call (getAdder) 3 4}}")
    .unwrap()
    .execute_to_string(&tmap!{})
    .unwrap();
assert_eq!(result, "7");
```

## Options

```rust
use gotmpl::{Template, MissingKey};

let tmpl = Template::new("t")
    .missing_key(MissingKey::Error)   // error on missing map keys
    .delims("<<", ">>")              // custom delimiters
    .parse("<< .Name >>")
    .unwrap();
```

`MissingKey` implements `FromStr`, so you can parse from strings (useful for
config files or CLI args):

```rust
use gotmpl::MissingKey;

let mk: MissingKey = "error".parse().unwrap();
```

| `MissingKey` variant | `FromStr` value          | Behavior            |
| -------------------- | ------------------------ | ------------------- |
| `Invalid` (default)  | `"invalid"`, `"default"` | Return `<no value>` |
| `ZeroValue`          | `"zero"`                 | Return `<no value>` |
| `Error`              | `"error"`                | Return an error     |

`ZeroValue` exists for parity with Go's `{{options "missingkey=zero"}}` directive.
Since `Value` is untyped, it behaves the same as `Invalid`; the variant is there so
callers can still opt in to the named option.

## Number literals

Go-compatible number literal syntax:

```text
{{42}}          Decimal
{{3.14}}        Float
{{0xFF}}        Hexadecimal
{{0o77}}        Octal
{{0377}}        Octal (legacy leading zero)
{{0b1010}}      Binary
{{1_000_000}}   Underscore separators
{{'a'}}         Character literal (97)
{{0x1.ep+2}}    Hex float (7.5)
```

## Data model

Template data uses the `Value` enum:

| Variant                               | Rust type     | Go equivalent    |
| ------------------------------------- | ------------- | ---------------- |
| `Nil`                                 | n/a           | `nil`            |
| `Bool(bool)`                          | `bool`        | `bool`           |
| `Int(i64)`                            | `i64`         | `int`            |
| `Float(f64)`                          | `f64`         | `float64`        |
| `String(Arc<str>)`                    | `String`      | `string`         |
| `List(Arc<[Value]>)`                  | `Vec<Value>`  | `[]any`          |
| `Map(Arc<BTreeMap<Arc<str>, Value>>)` | `BTreeMap`    | `map[string]any` |
| `Function(ValueFunc)`                 | `Arc<dyn Fn>` | `func(...)`      |

The `tmap!` macro builds data maps:

```rust
use gotmpl::{tmap, ToValue};

let data = tmap! {
    "Name" => "Alice",
    "Age" => 30i64,
    "Scores" => vec![95i64, 87, 92],
    "Address" => tmap! {
        "City" => "Paris",
    },
};
```

## `no_std` support

The crate works in `no_std` environments (requires `alloc`). Disable the default `std`
feature:

```toml
[dependencies]
gotmpl = { version = "0.3", default-features = false }
```

Without `std`, `execute_fmt` and `execute_to_string` are available. The `io::Write`-based
`execute`/`execute_template` methods and `parse_files` require the `std` feature.
User-defined functions that panic will propagate instead of being caught.

## Differences from Go

Rust has no runtime reflection, so:

- **No struct field access**: use `Value::Map` instead
- **No method calls**: register functions via `.func()`
- **No pointer/interface indirection**: `Value` is always concrete
- **No complex numbers, channels, or `iter.Seq`**
- **NaN comparisons** return an error instead of Go's silently wrong results

## Go cross-check

The test suite can optionally run every template through Go's `text/template` and assert
output parity:

```sh
cargo test --features go-crosscheck
```

A Go helper (`tests/testdata/go_crosscheck.go`) is compiled once per test run. It
reads templates and typed data from stdin as JSON, executes them via Go's
`text/template`, and prints the result.
