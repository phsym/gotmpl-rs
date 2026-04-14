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
use crate::go;
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
            let mut result = String::new();
            for (i, arg) in args.iter().enumerate() {
                if i > 0 && go::needs_space(&args[i - 1], arg) {
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
            let s: Vec<String> = args.iter().map(|a| format!("{}", a)).collect();
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
            let result = go::sprintf(&fmt_str, &args[1..])?;
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
            Ok(Value::String(go::html_escape(&s)))
        }),
    );

    m.insert(
        "js".into(),
        Arc::new(|args: &[Value]| {
            check_args("js", args, 1)?;
            let s = format!("{}", args[0]);
            Ok(Value::String(go::js_escape(&s)))
        }),
    );

    m.insert(
        "urlquery".into(),
        Arc::new(|args: &[Value]| {
            check_args("urlquery", args, 1)?;
            let s = format!("{}", args[0]);
            Ok(Value::String(go::url_encode(&s)))
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
