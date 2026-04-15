# go-template

A faithful Rust reimplementation of Go's [`text/template`](https://pkg.go.dev/text/template) library.

## Overview

`go-template` brings Go's powerful text templating language to Rust. It supports the full
template syntax including pipelines, control flow, custom functions, template composition, and
whitespace trimming — all with Go-compatible semantics.

## Quick start

```rust
use go_template::{Template, tmap};

let data = tmap! { "Name" => "World" };
let result = Template::new("hello")
    .parse("Hello, {{.Name}}!")
    .unwrap()
    .execute_to_string(&data)
    .unwrap();
assert_eq!(result, "Hello, World!");
```

## Template syntax

The template language follows Go's `text/template` specification. Actions are delimited by
`{{` and `}}` (configurable via `.delims()`).

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

Commands can be chained with `|`, where each command's output becomes the last argument
of the next:

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
{{$i, $v := range .List}}  Range with index and value
```

### Template composition

```text
{{define "name"}}...{{end}}        Define a named template
{{template "name" .}}              Invoke a named template
{{block "name" .}}default{{end}}   Define and invoke (with default)
```

### Comments

```text
{{/* This is a comment */}}
{{- /* Trimmed comment */ -}}
```

### Whitespace trimming

Adding `-` inside a delimiter trims adjacent whitespace:

```text
{{- .X}}     Trim whitespace before
{{.X -}}     Trim whitespace after
{{- .X -}}   Trim both sides
```

## Built-in functions

| Function                           | Description                                                                               |
| ---------------------------------- | ----------------------------------------------------------------------------------------- |
| `print`                            | Concatenate args (spaces between non-string adjacent args)                                |
| `printf`                           | Formatted output (`%s`, `%d`, `%f`, `%v`, `%q`, `%x`, `%o`, `%b`, `%e`, `%g`, `%t`, `%c`) |
| `println`                          | Print with spaces between args, trailing newline                                          |
| `len`                              | Length of string, list, or map                                                            |
| `index`                            | Index into list or map: `index .List 0`, `index .Map "key"`                               |
| `slice`                            | Slice a list or string: `slice .List 1 3`                                                 |
| `call`                             | Call a function value: `call .Func arg1 arg2`                                             |
| `eq`, `ne`, `lt`, `le`, `gt`, `ge` | Comparison operators. `eq` supports multi-arg: `eq .X 1 2 3`                              |
| `and`                              | Short-circuit AND, returns first falsy arg or last arg                                    |
| `or`                               | Short-circuit OR, returns first truthy arg or last arg                                    |
| `not`                              | Boolean negation                                                                          |
| `html`                             | HTML-escape a string                                                                      |
| `js`                               | JavaScript-escape a string                                                                |
| `urlquery`                         | URL query-escape a string                                                                 |

## Custom functions

Register functions before parsing:

```rust
use go_template::{Template, tmap};
use go_template::Value;

let result = Template::new("test")
    .func("upper", |args| {
        match args.first() {
            Some(Value::String(s)) => Ok(Value::String(s.to_uppercase())),
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

The `Value::Function` variant allows passing callable values through templates:

```rust
use std::sync::Arc;
use go_template::{Template, tmap};
use go_template::{Value, ValueFunc};

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
use go_template::Template;

let tmpl = Template::new("t")
    .option("missingkey=error")   // error on missing map keys
    .delims("<<", ">>")           // custom delimiters
    .parse("<< .Name >>")
    .unwrap();
```

### Missing key behavior

| Option               | Behavior                 |
| -------------------- | ------------------------ |
| `missingkey=invalid` | Return `<nil>` (default) |
| `missingkey=zero`    | Return `<nil>`           |
| `missingkey=error`   | Return an error          |

## Number literals

Go-compatible number literal syntax is supported in templates:

```text
{{42}}          Decimal
{{3.14}}        Float
{{0xFF}}        Hexadecimal
{{0o77}}        Octal (explicit prefix)
{{0377}}        Octal (legacy, leading zero)
{{0b1010}}      Binary
{{1_000_000}}   Underscore separators
{{'a'}}         Character literal (emits code point: 97)
{{0x1.ep+2}}    Hex float literal (7.5)
```

## Data model

Template data uses the `Value` enum:

| Variant                        | Rust type     | Go equivalent    |
| ------------------------------ | ------------- | ---------------- |
| `Nil`                          | —             | `nil`            |
| `Bool(bool)`                   | `bool`        | `bool`           |
| `Int(i64)`                     | `i64`         | `int`            |
| `Float(f64)`                   | `f64`         | `float64`        |
| `String(String)`               | `String`      | `string`         |
| `List(Vec<Value>)`             | `Vec<Value>`  | `[]any`          |
| `Map(BTreeMap<String, Value>)` | `BTreeMap`    | `map[string]any` |
| `Function(ValueFunc)`          | `Arc<dyn Fn>` | `func(...)`      |

The `tmap!` macro provides a convenient way to build data maps:

```rust
use go_template::{tmap, ToValue};

let data = tmap! {
    "Name" => "Alice",
    "Age" => 30i64,
    "Scores" => vec![95i64, 87, 92],
    "Address" => tmap! {
        "City" => "Paris",
    },
};
```

## Go interoperability

All Go-specific formatting and escaping adaptations live in the [`go`](src/go.rs) module.
This isolates the Rust↔Go translation layer from the template engine logic.

### Implemented adaptations

| Adaptation                                                                                  | Go equivalent               | Status |
| ------------------------------------------------------------------------------------------- | --------------------------- | ------ |
| `sprintf` — full `fmt.Sprintf` with verbs `%s %d %f %e %E %g %G %v %q %t %x %X %o %b %c %%` | `fmt.Sprintf`               | Done   |
| `%#q` backtick quoting                                                                      | `fmt.Sprintf("%#q", ...)`   | Done   |
| `%e`/`%E` exponent normalization (always `e+00` not `e0`)                                   | `fmt.Sprintf("%e", ...)`    | Done   |
| `%g`/`%G` with precision, sci-notation threshold, trailing-zero stripping                   | `fmt.Sprintf("%g", ...)`    | Done   |
| Printf flags: `-` `+` ` ` `#` `0`, width, `.precision`                                      | `fmt` flag grammar          | Done   |
| `print` inter-arg spacing (spaces between non-string adjacent operands)                     | `fmt.Sprint`                | Done   |
| `go_quote` — double-quoted string literal with Go escape sequences                          | `strconv.Quote`             | Done   |
| Integer base formatting with sign-before-prefix convention (`-0xff`)                        | `fmt.Sprintf("%x", ...)`    | Done   |
| HTML escaping (`&`, `<`, `>`, `"`, `'`, NUL → U+FFFD)                                       | `template.HTMLEscapeString` | Done   |
| JS escaping (backslash, quotes, `<>`, `&`, `=`, control chars as `\uXXXX`)                  | `template.JSEscapeString`   | Done   |
| URL percent-encoding (RFC 3986 unreserved passthrough)                                      | `template.URLQueryEscaper`  | Done   |
| Hex float literal parsing (`0x1.Fp10`)                                                      | Go hex float syntax         | Done   |
| Legacy octal number literals (`0377` → 255)                                                 | Go legacy octal             | Done   |
| `nil` is not a command (bare `{{nil}}` errors)                                              | Go exec semantics           | Done   |
| Truthiness semantics (nil, 0, "", empty collections are falsy)                              | `template.IsTrue`           | Done   |
| `and`/`or` short-circuit evaluation returning the deciding value                            | Go template semantics       | Done   |
| `break`/`continue` in `range` loops                                                         | Go 1.18+                    | Done   |
| `range` over integer (`range 5`)                                                            | Go 1.22+                    | Done   |
| `block` (define + invoke)                                                                   | Go template semantics       | Done   |
| `else if` / `else with` chains                                                              | Go template semantics       | Done   |
| Whitespace trimming (`{{-` / `-}}`)                                                         | Go template semantics       | Done   |
| `$` rebinding inside `{{template}}` calls                                                   | Go template semantics       | Done   |
| Deterministic map iteration (sorted keys via `BTreeMap`)                                    | Go sorted map range         | Done   |

### Not yet implemented

| Feature                                          | Go equivalent             | Reason                                                                                           |
| ------------------------------------------------ | ------------------------- | ------------------------------------------------------------------------------------------------ |
| `<no value>` for missing map keys                | `reflect.Value{}` display | Rust uses `Value::Nil` → `<nil>` instead; Go's `<no value>` depends on `reflect.Value.IsValid()` |
| `missingkey=zero` returning typed zero values    | `reflect.Zero(type)`      | Without reflection, zero-value depends on the map's value type; we return `Nil`                  |
| `index` on missing map key returns typed zero    | `index .Map "k"`          | `Value::Map` is dynamically typed, so missing keys return `Nil` (`<nil>`) instead of typed zero  |
| `%T` format verb (type name)                     | `fmt.Sprintf("%T", ...)`  | Go-specific type name formatting                                                                 |
| `%p` format verb (pointer)                       | `fmt.Sprintf("%p", ...)`  | No pointers in `Value`                                                                           |
| `%w` format verb (error wrapping)                | `fmt.Errorf("%w", ...)`   | Not applicable to templates                                                                      |
| `%#v` Go-syntax value representation             | `fmt.Sprintf("%#v", ...)` | Would need Go-style printing of `Value`                                                          |
| `%U` Unicode format (`U+0041`)                   | `fmt.Sprintf("%U", ...)`  | Rarely used in templates                                                                         |
| Function name validation (reject `"a-b"`, `"2"`) | Go template parser        | Names are validated syntactically but not with Go's exact character rules                        |
| `{{range $i = .List}}` assignment form           | Go range assignment       | Only `:=` declaration form is supported                                                          |

## Differences from Go

Since Rust has no runtime reflection, some Go features are not applicable:

- **No struct field access** — use `Value::Map` instead of structs
- **No method calls** — register functions via `.func()` instead
- **No pointer/interface indirection** — `Value` is always concrete
- **No complex numbers or channels** — not in the `Value` enum
- **No `iter.Seq` / `iter.Seq2`** — use `Value::List` or `Value::Map`
- **Missing map keys print `<nil>`** — Go prints `<no value>` for missing keys (via
  `reflect.Value`); this library uses `Value::Nil` which displays as `<nil>`
- **NaN comparisons error instead of returning wrong results** — Go's `gt`/`ge`
  are implemented as `!le`/`!lt`, so `gt NaN NaN` returns `true` (an IEEE 754
  violation). This library returns an error for unorderable float comparisons,
  which is strictly more correct
