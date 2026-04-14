//! Built-in template functions, equivalent to Go's `text/template` builtins.
//!
//! In Go, template functions are stored in a `FuncMap` and called via reflection.
//! In Rust, we use boxed closures with the signature [`Func`].
//!
//! All built-in functions are registered automatically when creating a
//! [`Template`](crate::Template). Custom functions can be added via
//! [`Template::func`](crate::Template::func).
//!
//! # Built-in function reference
//!
//! | Category | Functions |
//! |----------|-----------|
//! | Comparison | `eq`, `ne`, `lt`, `le`, `gt`, `ge` |
//! | Logic | `and`, `or`, `not` |
//! | Output | `print`, `printf`, `println` |
//! | Data | `len`, `index`, `slice`, `call` |
//! | Escaping | `html`, `js`, `urlquery` |

use crate::error::{Result, TemplateError};
use crate::value::Value;
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::Arc;

/// The function type used by the template engine.
///
/// All template functions — both built-in and user-defined — share
/// this signature: they receive arguments as a [`Value`] slice and return a
/// [`Result<Value>`](crate::error::Result).
///
/// Uses [`Arc`] so that functions can be shared across cloned templates.
/// Register custom functions via [`Template::func`](crate::Template::func).
pub type Func = Arc<dyn Fn(&[Value]) -> Result<Value> + Send + Sync>;

/// Returns a [`HashMap`] containing all built-in template functions.
///
/// This is called automatically by [`Template::new`](crate::Template::new).
/// The returned map can be extended with custom functions via
/// [`Template::func`](crate::Template::func).
///
/// See the module source for the full list of built-in functions.
pub fn builtins() -> HashMap<String, Func> {
    let mut m: HashMap<String, Func> = HashMap::new();

    // ─── Comparison operators ────────────────────────────────────────
    // Go's eq can take 2+ args: eq x y z means x==y || x==z

    m.insert(
        "eq".into(),
        Arc::new(|args: &[Value]| {
            check_min_args("eq", args, 2)?;
            let first = &args[0];
            Ok(Value::Bool(args[1..].iter().any(|a| first == a)))
        }),
    );

    m.insert(
        "ne".into(),
        Arc::new(|args: &[Value]| {
            check_args("ne", args, 2)?;
            Ok(Value::Bool(args[0] != args[1]))
        }),
    );

    m.insert(
        "lt".into(),
        Arc::new(|args: &[Value]| {
            check_args("lt", args, 2)?;
            Ok(Value::Bool(
                args[0].partial_cmp(&args[1]) == Some(std::cmp::Ordering::Less),
            ))
        }),
    );

    m.insert(
        "le".into(),
        Arc::new(|args: &[Value]| {
            check_args("le", args, 2)?;
            Ok(Value::Bool(args[0] <= args[1]))
        }),
    );

    m.insert(
        "gt".into(),
        Arc::new(|args: &[Value]| {
            check_args("gt", args, 2)?;
            Ok(Value::Bool(
                args[0].partial_cmp(&args[1]) == Some(std::cmp::Ordering::Greater),
            ))
        }),
    );

    m.insert(
        "ge".into(),
        Arc::new(|args: &[Value]| {
            check_args("ge", args, 2)?;
            Ok(Value::Bool(args[0] >= args[1]))
        }),
    );

    // ─── Logic ───────────────────────────────────────────────────────
    // and: returns first falsy arg, or last arg if all truthy
    // or:  returns first truthy arg, or last arg if all falsy
    //
    // NOTE: and/or are special-cased in the executor for short-circuit
    // evaluation. These implementations are kept as fallbacks but the
    // executor calls them with pre-evaluated args only when not
    // short-circuiting (which shouldn't happen in normal flow).

    m.insert(
        "and".into(),
        Arc::new(|args: &[Value]| {
            check_min_args("and", args, 1)?;
            for arg in args {
                if !arg.is_truthy() {
                    return Ok(arg.clone());
                }
            }
            Ok(args.last().unwrap().clone())
        }),
    );

    m.insert(
        "or".into(),
        Arc::new(|args: &[Value]| {
            check_min_args("or", args, 1)?;
            for arg in args {
                if arg.is_truthy() {
                    return Ok(arg.clone());
                }
            }
            Ok(args.last().unwrap().clone())
        }),
    );

    m.insert(
        "not".into(),
        Arc::new(|args: &[Value]| {
            check_args("not", args, 1)?;
            Ok(Value::Bool(!args[0].is_truthy()))
        }),
    );

    // ─── Output formatting ──────────────────────────────────────────
    // Go's fmt.Sprint adds spaces between adjacent non-string operands.

    m.insert(
        "print".into(),
        Arc::new(|args: &[Value]| {
            let mut result = std::string::String::new();
            for (i, arg) in args.iter().enumerate() {
                if i > 0 && needs_space(&args[i - 1], arg) {
                    result.push(' ');
                }
                write!(result, "{}", arg).unwrap();
            }
            Ok(Value::String(result))
        }),
    );

    m.insert(
        "println".into(),
        Arc::new(|args: &[Value]| {
            let s: Vec<std::string::String> = args.iter().map(|a| format!("{}", a)).collect();
            Ok(Value::String(format!("{}\n", s.join(" "))))
        }),
    );

    m.insert(
        "printf".into(),
        Arc::new(|args: &[Value]| {
            check_min_args("printf", args, 1)?;
            let fmt_str = match &args[0] {
                Value::String(s) => s.clone(),
                other => {
                    return Err(TemplateError::Exec(format!(
                        "printf: first arg must be string, got {}",
                        other
                    )));
                }
            };
            let result = simple_sprintf(&fmt_str, &args[1..])?;
            Ok(Value::String(result))
        }),
    );

    // ─── Data access ─────────────────────────────────────────────────

    m.insert(
        "len".into(),
        Arc::new(|args: &[Value]| {
            check_args("len", args, 1)?;
            match args[0].len() {
                Some(n) => Ok(Value::Int(n as i64)),
                None => Err(TemplateError::Exec(format!(
                    "len: cannot take length of {}",
                    args[0]
                ))),
            }
        }),
    );

    m.insert(
        "index".into(),
        Arc::new(|args: &[Value]| {
            check_min_args("index", args, 2)?;
            let mut val = args[0].clone();
            for idx in &args[1..] {
                val = val.index(idx)?;
            }
            Ok(val)
        }),
    );

    // slice: Go allows 1-4 args: slice x, slice x i, slice x i j
    m.insert(
        "slice".into(),
        Arc::new(|args: &[Value]| {
            check_min_args("slice", args, 1)?;
            let start = args.get(1).and_then(Value::as_int);
            let end = args.get(2).and_then(Value::as_int);
            args[0].slice(start, end)
        }),
    );

    // call: invoke a function-typed value
    m.insert(
        "call".into(),
        Arc::new(|args: &[Value]| {
            check_min_args("call", args, 1)?;
            match &args[0] {
                Value::Function(f) => f(&args[1..]),
                Value::Nil => Err(TemplateError::Exec("call of nil".into())),
                other => Err(TemplateError::Exec(format!(
                    "call: non-function value of type {}",
                    other.type_name()
                ))),
            }
        }),
    );

    // ─── HTML/JS/URL escaping ────────────────────────────────────────

    m.insert(
        "html".into(),
        Arc::new(|args: &[Value]| {
            check_args("html", args, 1)?;
            let s = format!("{}", args[0]);
            Ok(Value::String(html_escape(&s)))
        }),
    );

    m.insert(
        "js".into(),
        Arc::new(|args: &[Value]| {
            check_args("js", args, 1)?;
            let s = format!("{}", args[0]);
            Ok(Value::String(js_escape(&s)))
        }),
    );

    m.insert(
        "urlquery".into(),
        Arc::new(|args: &[Value]| {
            check_args("urlquery", args, 1)?;
            let s = format!("{}", args[0]);
            Ok(Value::String(url_encode(&s)))
        }),
    );

    m
}

/// Go's fmt.Sprint adds spaces between adjacent non-string operands.
fn needs_space(prev: &Value, next: &Value) -> bool {
    !matches!(prev, Value::String(_)) && !matches!(next, Value::String(_))
}

// ─── Argument validation helpers ─────────────────────────────────────────

fn check_args(name: &str, args: &[Value], expected: usize) -> Result<()> {
    if args.len() != expected {
        return Err(TemplateError::ArgCount {
            name: name.to_string(),
            expected,
            got: args.len(),
        });
    }
    Ok(())
}

fn check_min_args(name: &str, args: &[Value], min: usize) -> Result<()> {
    if args.len() < min {
        return Err(TemplateError::ArgCount {
            name: name.to_string(),
            expected: min,
            got: args.len(),
        });
    }
    Ok(())
}

// ─── sprintf implementation ─────────────────────────────────────────────
// Go's templates use fmt.Sprintf. We implement the common format verbs
// with full support for flags, width, and precision.

/// Parsed printf format specifier (flags, width, precision).
struct FmtSpec {
    left_align: bool,
    plus: bool,
    space: bool,
    hash: bool,
    zero: bool,
    width: Option<usize>,
    precision: Option<usize>,
}

impl FmtSpec {
    fn parse(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Self {
        let mut spec = FmtSpec {
            left_align: false,
            plus: false,
            space: false,
            hash: false,
            zero: false,
            width: None,
            precision: None,
        };
        // Flags
        loop {
            match chars.peek() {
                Some('-') => {
                    spec.left_align = true;
                    chars.next();
                }
                Some('+') => {
                    spec.plus = true;
                    chars.next();
                }
                Some(' ') => {
                    spec.space = true;
                    chars.next();
                }
                Some('#') => {
                    spec.hash = true;
                    chars.next();
                }
                Some('0') => {
                    spec.zero = true;
                    chars.next();
                }
                _ => break,
            }
        }
        // Width
        let mut w = std::string::String::new();
        while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
            w.push(chars.next().unwrap());
        }
        if !w.is_empty() {
            spec.width = w.parse().ok();
        }
        // Precision
        if chars.peek() == Some(&'.') {
            chars.next();
            let mut p = std::string::String::new();
            while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.push(chars.next().unwrap());
            }
            spec.precision = Some(if p.is_empty() {
                0
            } else {
                p.parse().unwrap_or(0)
            });
        }
        spec
    }

    /// Pad a formatted string to the configured width.
    fn pad(&self, s: &str, is_numeric: bool) -> std::string::String {
        let char_len = s.chars().count();
        let width = match self.width {
            Some(w) if char_len < w => w,
            _ => return s.to_string(),
        };
        let padding = width - char_len;
        if self.left_align {
            format!("{}{}", s, " ".repeat(padding))
        } else if self.zero && is_numeric {
            // Put zeros after the sign character
            if let Some(rest) = s.strip_prefix('-') {
                format!("-{}{}", "0".repeat(padding), rest)
            } else if let Some(rest) = s.strip_prefix('+') {
                format!("+{}{}", "0".repeat(padding), rest)
            } else {
                format!("{}{}", "0".repeat(padding), s)
            }
        } else {
            format!("{}{}", " ".repeat(padding), s)
        }
    }

    /// Format a signed integer with sign flags applied.
    fn format_signed(&self, n: i64) -> std::string::String {
        if self.plus {
            if n >= 0 {
                format!("+{}", n)
            } else {
                format!("{}", n)
            }
        } else if self.space {
            if n >= 0 {
                format!(" {}", n)
            } else {
                format!("{}", n)
            }
        } else {
            format!("{}", n)
        }
    }
}

/// Produce a Go-syntax double-quoted string literal (like `strconv.Quote`).
fn go_quote(s: &str) -> std::string::String {
    let mut out = std::string::String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0C' => out.push_str("\\f"),
            '\x0B' => out.push_str("\\v"),
            c if (c as u32) < 0x20 || c == '\x7F' => {
                write!(out, "\\x{:02x}", c as u32).unwrap();
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Format a signed integer in a non-decimal base, using Go's conventions
/// (sign is separate from magnitude: -0xff, not two's complement).
fn format_int_base(n: i64, base: &str, spec: &FmtSpec) -> std::string::String {
    let abs = n.unsigned_abs();
    let digits = match base {
        "x" => format!("{:x}", abs),
        "X" => format!("{:X}", abs),
        "o" => format!("{:o}", abs),
        "b" => format!("{:b}", abs),
        _ => unreachable!(),
    };
    let prefix = if spec.hash {
        match base {
            "x" => "0x",
            "X" => "0X",
            "o" => "0o",
            "b" => "0b",
            _ => "",
        }
    } else {
        ""
    };
    if n < 0 {
        format!("-{}{}", prefix, digits)
    } else {
        format!("{}{}", prefix, digits)
    }
}

fn simple_sprintf(fmt_str: &str, args: &[Value]) -> Result<std::string::String> {
    let mut result = std::string::String::new();
    let mut chars = fmt_str.chars().peekable();
    let mut arg_idx = 0;

    while let Some(ch) = chars.next() {
        if ch != '%' {
            result.push(ch);
            continue;
        }

        let spec = FmtSpec::parse(&mut chars);

        let verb = match chars.next() {
            Some(v) => v,
            None => {
                result.push('%');
                break;
            }
        };

        if verb == '%' {
            result.push('%');
            continue;
        }

        // Consume the next argument, or emit MISSING.
        let arg = if arg_idx < args.len() {
            arg_idx += 1;
            &args[arg_idx - 1]
        } else {
            write!(result, "%!{}(MISSING)", verb).unwrap();
            continue;
        };

        match verb {
            's' => {
                let mut s = format!("{}", arg);
                if let Some(prec) = spec.precision {
                    s = s.chars().take(prec).collect();
                }
                result.push_str(&spec.pad(&s, false));
            }
            'd' => {
                let n = arg.as_int().unwrap_or(0);
                let s = spec.format_signed(n);
                result.push_str(&spec.pad(&s, true));
            }
            'f' => {
                let f = arg.as_float().unwrap_or(0.0);
                let prec = spec.precision.unwrap_or(6);
                let s = if spec.plus {
                    if f >= 0.0 {
                        format!("+{:.prec$}", f)
                    } else {
                        format!("{:.prec$}", f)
                    }
                } else if spec.space {
                    if f >= 0.0 {
                        format!(" {:.prec$}", f)
                    } else {
                        format!("{:.prec$}", f)
                    }
                } else {
                    format!("{:.prec$}", f)
                };
                result.push_str(&spec.pad(&s, true));
            }
            'e' | 'E' => {
                let f = arg.as_float().unwrap_or(0.0);
                let prec = spec.precision.unwrap_or(6);
                let s = if verb == 'e' {
                    format!("{:.prec$e}", f)
                } else {
                    format!("{:.prec$E}", f)
                };
                result.push_str(&spec.pad(&s, true));
            }
            'g' | 'G' => {
                let f = arg.as_float().unwrap_or(0.0);
                let s = format!("{}", f);
                result.push_str(&spec.pad(&s, true));
            }
            'v' => {
                let s = format!("{}", arg);
                result.push_str(&spec.pad(&s, false));
            }
            'q' => {
                let s = match arg {
                    Value::String(s) => go_quote(s),
                    other => go_quote(&format!("{}", other)),
                };
                result.push_str(&spec.pad(&s, false));
            }
            't' => {
                let s = match arg {
                    Value::Bool(b) => format!("{}", b),
                    other => format!("{}", other),
                };
                result.push_str(&spec.pad(&s, false));
            }
            'x' | 'X' | 'o' | 'b' => {
                let n = arg.as_int().unwrap_or(0);
                let s = format_int_base(n, &verb.to_string(), &spec);
                result.push_str(&spec.pad(&s, true));
            }
            'c' => {
                let n = arg.as_int().unwrap_or(0);
                if let Some(c) = char::from_u32(n as u32) {
                    result.push(c);
                }
            }
            _ => {
                result.push('%');
                result.push(verb);
            }
        }
    }

    Ok(result)
}

// ─── Escaping functions ──────────────────────────────────────────────────

/// HTML-escape a string, replacing `&`, `<`, `>`, `"`, `'`, and NUL bytes.
///
/// Matches Go's `template.HTMLEscapeString`. NUL bytes are replaced with the
/// Unicode replacement character (U+FFFD). Quotes use numeric entities
/// (`&#34;`, `&#39;`) for brevity, matching Go's choice.
pub fn html_escape(s: &str) -> std::string::String {
    let mut out = std::string::String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\0' => out.push('\u{FFFD}'), // Go replaces NUL with Unicode replacement char
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// JavaScript-escape a string for safe embedding in JS string literals.
///
/// Matches Go's `template.JSEscapeString`. Escapes backslash, quotes,
/// newlines, tabs, angle brackets (`<`, `>`), ampersand, equals sign,
/// and all control characters below U+0020 as `\uXXXX`.
pub fn js_escape(s: &str) -> std::string::String {
    let mut out = std::string::String::new();
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '=' => out.push_str("\\u003d"),
            _ if (ch as u32) < 0x20 => {
                // Control characters
                write!(out, "\\u{:04x}", ch as u32).unwrap();
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Percent-encode a string for use in URL query parameters (RFC 3986).
///
/// Unreserved characters (`A-Z`, `a-z`, `0-9`, `-`, `_`, `.`, `~`) are
/// passed through; everything else is encoded as `%XX`.
pub fn url_encode(s: &str) -> std::string::String {
    let mut out = std::string::String::new();
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                write!(out, "%{:02X}", byte).unwrap();
            }
        }
    }
    out
}
