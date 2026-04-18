//! Built-in template functions, equivalent to Go's `text/template` builtins.
//!
//! In Go, template functions are stored in a `FuncMap` and called via reflection.
//! In Rust, we use boxed closures with the signature [`ValueFunc`].
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
//!
//! # Escape-function security notes
//!
//! The escape builtins match Go's `text/template` parity exactly, which is
//! **not** enough for all HTML/JS contexts:
//!
//! - `html` does **not** escape backticks and is only safe inside
//!   double-quoted attribute values or text nodes — never in unquoted
//!   attributes, `<script>` blocks, or inline event handlers.
//! - `js` does **not** escape U+2028 / U+2029 (line / paragraph separator),
//!   which terminate string literals when embedded in `<script>` tags.
//! - `urlquery` percent-encodes for query strings; it is not a replacement
//!   for full URL-construction logic when building paths.
//!
//! For context-aware escaping use a dedicated `html/template`-style crate.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt::Write;

use crate::error::{Result, TemplateError};
use crate::go;
use crate::value::{Value, ValueFunc};

/// Returns a [`BTreeMap`] containing all built-in template functions.
///
/// This is called automatically by [`Template::new`](crate::Template::new).
/// The returned map can be extended with custom functions via
/// [`Template::func`](crate::Template::func).
///
/// See the module source for the full list of built-in functions.
pub fn builtins() -> BTreeMap<String, ValueFunc> {
    let mut m: BTreeMap<String, ValueFunc> = BTreeMap::new();

    // ─── Comparison operators ────────────────────────────────────────
    // Go's eq can take 2+ args: eq x y z means x==y || x==z

    m.insert(
        "eq".into(),
        Arc::new(|args: &[Value]| {
            check_min_args("eq", args, 2)?;
            let first = &args[0];
            for arg in &args[1..] {
                if compare_eq(first, arg)? {
                    return Ok(Value::Bool(true));
                }
            }
            Ok(Value::Bool(false))
        }),
    );

    m.insert(
        "ne".into(),
        Arc::new(|args: &[Value]| {
            check_args("ne", args, 2)?;
            Ok(Value::Bool(!compare_eq(&args[0], &args[1])?))
        }),
    );

    m.insert(
        "lt".into(),
        Arc::new(|args: &[Value]| {
            check_args("lt", args, 2)?;
            Ok(Value::Bool(
                compare_order(&args[0], &args[1])? == Ordering::Less,
            ))
        }),
    );

    m.insert(
        "le".into(),
        Arc::new(|args: &[Value]| {
            check_args("le", args, 2)?;
            let ord = compare_order(&args[0], &args[1])?;
            Ok(Value::Bool(ord == Ordering::Less || ord == Ordering::Equal))
        }),
    );

    m.insert(
        "gt".into(),
        Arc::new(|args: &[Value]| {
            check_args("gt", args, 2)?;
            Ok(Value::Bool(
                compare_order(&args[0], &args[1])? == Ordering::Greater,
            ))
        }),
    );

    m.insert(
        "ge".into(),
        Arc::new(|args: &[Value]| {
            check_args("ge", args, 2)?;
            let ord = compare_order(&args[0], &args[1])?;
            Ok(Value::Bool(
                ord == Ordering::Greater || ord == Ordering::Equal,
            ))
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
            #[allow(
                clippy::unwrap_used,
                reason = "check_min_args guarantees args is non-empty"
            )]
            let last = args.last().unwrap().clone();
            Ok(last)
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
            #[allow(
                clippy::unwrap_used,
                reason = "check_min_args guarantees args is non-empty"
            )]
            let last = args.last().unwrap().clone();
            Ok(last)
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
            let mut result = String::new();
            for (i, arg) in args.iter().enumerate() {
                if i > 0 && go::needs_space(&args[i - 1], arg) {
                    result.push(' ');
                }
                write!(result, "{}", arg).ok();
            }
            Ok(Value::String(Arc::from(result)))
        }),
    );

    m.insert(
        "println".into(),
        Arc::new(|args: &[Value]| {
            let s: Vec<String> = args.iter().map(|a| format!("{}", a)).collect();
            Ok(Value::String(Arc::from(format!("{}\n", s.join(" ")))))
        }),
    );

    m.insert(
        "printf".into(),
        Arc::new(|args: &[Value]| {
            check_min_args("printf", args, 1)?;
            let fmt_str: &str = match &args[0] {
                Value::String(s) => s,
                other => {
                    return Err(TemplateError::Exec(format!(
                        "printf: first arg must be string, got {}",
                        other
                    )));
                }
            };
            let result = go::sprintf(fmt_str, &args[1..])?;
            Ok(Value::String(Arc::from(result)))
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

    // slice: Go allows 1-4 args total:
    // slice x, slice x i, slice x i j, slice x i j k
    m.insert(
        "slice".into(),
        Arc::new(|args: &[Value]| {
            check_min_args("slice", args, 1)?;
            if args.len() > 4 {
                return Err(TemplateError::Exec(format!(
                    "wrong number of args for slice: want 1-4 got {}",
                    args.len()
                )));
            }

            let v = &args[0];
            match args.len() {
                1 => v.slice(None, None),
                2 => {
                    let start = parse_slice_index(&args[1])?;
                    v.slice(Some(start), None)
                }
                3 => {
                    let start = parse_slice_index(&args[1])?;
                    let end = parse_slice_index(&args[2])?;
                    v.slice(Some(start), Some(end))
                }
                4 => {
                    // 3-index slice. We only support this for lists (like slices),
                    // and we validate i <= j <= k <= len.
                    let start = parse_slice_index(&args[1])?;
                    let end = parse_slice_index(&args[2])?;
                    let max = parse_slice_index(&args[3])?;
                    match v {
                        Value::List(list) => {
                            let len = list.len() as i64;
                            if start < 0 || end < 0 || max < 0 {
                                return Err(TemplateError::Exec(format!(
                                    "slice: index out of range [{}:{}:{}]",
                                    start, end, max
                                )));
                            }
                            if start > end {
                                return Err(TemplateError::Exec(format!(
                                    "slice: invalid slice index: {} > {}",
                                    start, end
                                )));
                            }
                            if end > max {
                                return Err(TemplateError::Exec(format!(
                                    "slice: invalid slice index: {} > {}",
                                    end, max
                                )));
                            }
                            if max > len {
                                return Err(TemplateError::Exec(format!(
                                    "slice: index out of range [{}:{}:{}] with length {}",
                                    start, end, max, len
                                )));
                            }
                            v.slice(Some(start), Some(end))
                        }
                        Value::String(_) => Err(TemplateError::Exec(
                            "slice: cannot 3-index slice a string".into(),
                        )),
                        _ => Err(TemplateError::Exec(format!(
                            "slice: cannot slice type {}",
                            v.type_name()
                        ))),
                    }
                }
                #[allow(
                    clippy::unreachable,
                    reason = "args.len() is in 1..=4: bounded by check_min_args and the `> 4` bailout above"
                )]
                _ => unreachable!(),
            }
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
            Ok(Value::String(Arc::from(go::html_escape(&s))))
        }),
    );

    m.insert(
        "js".into(),
        Arc::new(|args: &[Value]| {
            check_args("js", args, 1)?;
            let s = format!("{}", args[0]);
            Ok(Value::String(Arc::from(go::js_escape(&s))))
        }),
    );

    m.insert(
        "urlquery".into(),
        Arc::new(|args: &[Value]| {
            check_args("urlquery", args, 1)?;
            let s = format!("{}", args[0]);
            Ok(Value::String(Arc::from(go::url_encode(&s))))
        }),
    );

    m
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

fn compare_eq(left: &Value, right: &Value) -> Result<bool> {
    match (left, right) {
        (Value::Nil, Value::Nil) => Ok(true),
        (Value::Nil, _) | (_, Value::Nil) => Ok(false),
        (Value::Bool(a), Value::Bool(b)) => Ok(a == b),
        (Value::Int(a), Value::Int(b)) => Ok(a == b),
        (Value::Float(a), Value::Float(b)) => Ok(a == b),
        (Value::String(a), Value::String(b)) => Ok(a == b),
        (Value::List(_), Value::List(_))
        | (Value::Map(_), Value::Map(_))
        | (Value::Function(_), Value::Function(_)) => Err(TemplateError::Exec(format!(
            "non-comparable type {}",
            left.type_name()
        ))),
        _ => Err(TemplateError::Exec(format!(
            "incompatible types for comparison: {} and {}",
            left.type_name(),
            right.type_name()
        ))),
    }
}

fn compare_order(left: &Value, right: &Value) -> Result<Ordering> {
    match (left, right) {
        (Value::Int(a), Value::Int(b)) => Ok(a.cmp(b)),
        (Value::Float(a), Value::Float(b)) => a
            .partial_cmp(b)
            .ok_or_else(|| TemplateError::Exec("invalid type for comparison".into())),
        (Value::String(a), Value::String(b)) => Ok(a.cmp(b)),
        _ if left.type_name() != right.type_name() => Err(TemplateError::Exec(format!(
            "incompatible types for comparison: {} and {}",
            left.type_name(),
            right.type_name()
        ))),
        _ => Err(TemplateError::Exec("invalid type for comparison".into())),
    }
}

fn parse_slice_index(arg: &Value) -> Result<i64> {
    match arg {
        Value::Int(n) => Ok(*n),
        _ => Err(TemplateError::Exec(format!(
            "slice: index must be integer, got {}",
            arg.type_name()
        ))),
    }
}
