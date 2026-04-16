//! Go-compatible formatting, escaping, and number parsing.
//!
//! This module contains all the adaptations needed to match Go's `fmt` package
//! and `text/template` output conventions from Rust. It is used internally by
//! the built-in template functions ([`print`](crate::funcs), [`printf`](crate::funcs), etc.)
//! and by the lexer for number literal parsing.
//!
//! # What lives here
//!
//! | Category | Functions | Go equivalent |
//! |----------|-----------|---------------|
//! | sprintf | [`sprintf`] | `fmt.Sprintf` |
//! | Sprint spacing | [`needs_space`] | `fmt.Sprint` inter-arg spacing rule |
//! | Quoting | [`go_quote`], [`go_backquote`] | `strconv.Quote`, `%#q` |
//! | Sci-notation | [`go_normalize_sci`] | Exponent `e+00` format |
//! | `%g` formatting | [`format_g_default`], [`format_g_with_precision`] | `%g` / `%.Ng` |
//! | Integer bases | [`format_int_base`] | `%x`, `%o`, `%b` with sign handling |
//! | HTML escape | [`html_escape`] | `template.HTMLEscapeString` |
//! | JS escape | [`js_escape`] | `template.JSEscapeString` |
//! | URL encode | [`url_encode`] | `template.URLQueryEscaper` |
//! | Hex float parse | [`parse_hex_float`] | Hex float literal `0x1.Fp10` |

use alloc::format;
use alloc::string::{String, ToString};
use core::fmt::Write;

use crate::error::Result;
use crate::value::Value;

// ─── fmt.Sprint spacing ─────────────────────────────────────────────────

/// Returns `true` when Go's `fmt.Sprint` would insert a space between two
/// adjacent arguments (i.e. neither is a string).
pub(crate) fn needs_space(prev: &Value, next: &Value) -> bool {
    !matches!(prev, Value::String(_)) && !matches!(next, Value::String(_))
}

// ─── sprintf ────────────────────────────────────────────────────────────

/// Parsed printf format specifier (`flags`, `width`, `precision`).
///
/// Mirrors the subset of Go's `fmt` format grammar that `text/template` uses.
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
    fn parse(chars: &mut core::iter::Peekable<core::str::Chars<'_>>) -> Self {
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
        let mut w = String::new();
        while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
            w.push(chars.next().unwrap());
        }
        if !w.is_empty() {
            spec.width = w.parse().ok();
        }
        // Precision
        if chars.peek() == Some(&'.') {
            chars.next();
            let mut p = String::new();
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
    fn pad(&self, s: &str, is_numeric: bool) -> String {
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

    /// Format a signed integer with sign flags (`+`, ` `) applied.
    fn format_signed(&self, n: i64) -> String {
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

/// Go-compatible `fmt.Sprintf` implementation.
///
/// Supports the format verbs used by `text/template`:
/// `%s`, `%d`, `%f`, `%e`/`%E`, `%g`/`%G`, `%v`, `%q` (incl. `%#q`),
/// `%t`, `%x`/`%X`, `%o`, `%b`, `%c`, and `%%`.
///
/// Flags: `-` (left-align), `+` (sign), ` ` (space sign), `#` (alternate),
/// `0` (zero-pad). Width and precision (`.N`) are supported.
pub(crate) fn sprintf(fmt_str: &str, args: &[Value]) -> Result<String> {
    let mut result = String::new();
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
                let raw = if verb == 'e' {
                    format!("{:.prec$e}", f)
                } else {
                    format!("{:.prec$E}", f)
                };
                let s = go_normalize_sci(&raw);
                let s = apply_float_sign(s, f, &spec);
                result.push_str(&spec.pad(&s, true));
            }
            'g' | 'G' => {
                let f = arg.as_float().unwrap_or(0.0);
                let s = if f.is_nan() || f.is_infinite() {
                    format!("{}", f)
                } else if let Some(prec) = spec.precision {
                    format_g_with_precision(f, prec.max(1), verb == 'G')
                } else {
                    format_g_default(f, verb == 'G')
                };
                let s = apply_float_sign(s, f, &spec);
                result.push_str(&spec.pad(&s, true));
            }
            'v' => {
                let s = format!("{}", arg);
                result.push_str(&spec.pad(&s, false));
            }
            'q' => {
                let raw = match arg {
                    Value::String(s) => s,
                    _ => "",
                };
                let display;
                let raw = if !matches!(arg, Value::String(_)) {
                    display = format!("{}", arg);
                    display.as_str()
                } else {
                    raw
                };
                let s = if spec.hash {
                    go_backquote(raw).unwrap_or_else(|| go_quote(raw))
                } else {
                    go_quote(raw)
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

// ─── Quoting ────────────────────────────────────────────────────────────

/// Produce a Go-syntax double-quoted string literal (like `strconv.Quote`).
///
/// Escapes backslash, double-quote, newline, tab, carriage-return, bell,
/// backspace, form-feed, vertical-tab, and all control characters.
fn go_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
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

/// Produce a Go-syntax backtick-quoted string (for `%#q`) if possible.
///
/// Returns `None` if the string contains backticks or non-printable characters,
/// in which case the caller should fall back to [`go_quote`].
fn go_backquote(s: &str) -> Option<String> {
    if s.contains('`') {
        return None;
    }
    for ch in s.chars() {
        if ch != '\t' && (ch < ' ' || ch == '\x7F') {
            return None;
        }
    }
    Some(format!("`{}`", s))
}

// ─── Scientific notation normalization ──────────────────────────────────

/// Normalize Rust's scientific notation to Go's format.
///
/// Go always includes an explicit sign and at least 2 digits in the exponent:
/// - `e0` → `e+00`
/// - `e2` → `e+02`
/// - `e-3` → `e-03`
fn go_normalize_sci(s: &str) -> String {
    if let Some(e_pos) = s.find('e').or_else(|| s.find('E')) {
        let (mantissa, exp_part) = s.split_at(e_pos);
        let e_char = &exp_part[..1];
        let exp_str = &exp_part[1..];
        let (sign, digits) = if let Some(d) = exp_str.strip_prefix('-') {
            ("-", d)
        } else if let Some(d) = exp_str.strip_prefix('+') {
            ("+", d)
        } else {
            ("+", exp_str)
        };
        if digits.len() < 2 {
            format!("{}{}{}{:0>2}", mantissa, e_char, sign, digits)
        } else {
            format!("{}{}{}{}", mantissa, e_char, sign, digits)
        }
    } else {
        s.to_string()
    }
}

/// Strip trailing zeros after the decimal point (`"1.50000"` → `"1.5"`).
fn strip_trailing_zeros(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Strip trailing zeros from the mantissa of a scientific notation string.
fn strip_trailing_zeros_sci(s: &str) -> String {
    if let Some(e_pos) = s.find('e').or_else(|| s.find('E')) {
        let mantissa = &s[..e_pos];
        let exp_part = &s[e_pos..];
        format!("{}{}", strip_trailing_zeros(mantissa), exp_part)
    } else {
        strip_trailing_zeros(s)
    }
}

// ─── %g formatting ──────────────────────────────────────────────────────

/// Format a float with Go's `%g` default precision (shortest representation).
///
/// Uses `%e` notation when the exponent is < −4 or ≥ 6, matching Go's
/// `strconv.FormatFloat(f, 'g', -1, 64)`.
fn format_g_default(f: f64, upper: bool) -> String {
    if f == 0.0 {
        return (if f.is_sign_negative() { "-0" } else { "0" }).to_string();
    }
    let exp = f.abs().log10().floor() as i32;
    if !(-4..6).contains(&exp) {
        let raw = format!("{:e}", f);
        let s = go_normalize_sci(&raw);
        if upper { s.replace('e', "E") } else { s }
    } else {
        format!("{}", f)
    }
}

/// Format a float with Go's `%g` and an explicit precision (significant digits).
///
/// Uses `%e` notation when the exponent is < −4 or ≥ `prec`, then strips
/// trailing zeros.
fn format_g_with_precision(f: f64, prec: usize, upper: bool) -> String {
    if f == 0.0 {
        return (if f.is_sign_negative() { "-0" } else { "0" }).to_string();
    }
    let exp = f.abs().log10().floor() as i32;
    if exp < -4 || exp >= prec as i32 {
        let e_prec = prec.saturating_sub(1);
        let raw = format!("{:.prec$e}", f, prec = e_prec);
        let s = go_normalize_sci(&raw);
        let s = strip_trailing_zeros_sci(&s);
        if upper { s.replace('e', "E") } else { s }
    } else {
        let f_prec = if prec as i32 > exp + 1 {
            (prec as i32 - exp - 1) as usize
        } else {
            0
        };
        let raw = format!("{:.prec$}", f, prec = f_prec);
        strip_trailing_zeros(&raw)
    }
}

/// Apply the `+` or space sign flag to a formatted float string.
fn apply_float_sign(s: String, f: f64, spec: &FmtSpec) -> String {
    if spec.plus && f >= 0.0 && !s.starts_with('-') {
        format!("+{}", s)
    } else if spec.space && f >= 0.0 && !s.starts_with('-') {
        format!(" {}", s)
    } else {
        s
    }
}

// ─── Integer base formatting ────────────────────────────────────────────

/// Format a signed integer in a non-decimal base, using Go's conventions
/// (sign is separate from magnitude: `-0xff`, not two's complement).
fn format_int_base(n: i64, base: &str, spec: &FmtSpec) -> String {
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
            "o" => "0",
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

// ─── Escaping functions ─────────────────────────────────────────────────

/// HTML-escape a string, replacing `&`, `<`, `>`, `"`, `'`, and NUL bytes.
///
/// Matches Go's `template.HTMLEscapeString`. NUL bytes are replaced with the
/// Unicode replacement character (U+FFFD). Quotes use numeric entities
/// (`&#34;`, `&#39;`) for brevity, matching Go's choice.
pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
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
pub fn js_escape(s: &str) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\'' => out.push_str("\\'"),
            // Go's JSEscape uses \uXXXX (uppercase hex) for all control
            // characters, including \t, \n, \r (no shorthand escapes).
            '<' => out.push_str("\\u003C"),
            '>' => out.push_str("\\u003E"),
            '&' => out.push_str("\\u0026"),
            '=' => out.push_str("\\u003D"),
            _ if (ch as u32) < 0x20 => {
                write!(out, "\\u{:04X}", ch as u32).unwrap();
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
pub fn url_encode(s: &str) -> String {
    let mut out = String::new();
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

// ─── Hex float parsing ──────────────────────────────────────────────────

/// Parse a hex float literal like `0x1.Fp10` or `-0x1p-2`.
///
/// Used by the lexer to convert Go's hex-float syntax into an `f64`.
pub(crate) fn parse_hex_float(s: &str) -> Option<f64> {
    let negative = s.starts_with('-');
    let s = s.trim_start_matches('+').trim_start_matches('-');
    let s = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X"))?;

    let (mantissa_str, exp_str) = if let Some(p) = s.find(['p', 'P']) {
        (&s[..p], &s[p + 1..])
    } else {
        (s, "0")
    };

    let mantissa = if let Some(dot) = mantissa_str.find('.') {
        let int_part = &mantissa_str[..dot];
        let frac_part = &mantissa_str[dot + 1..];
        let int_val = if int_part.is_empty() {
            0u64
        } else {
            u64::from_str_radix(int_part, 16).ok()?
        };
        let frac_val = if frac_part.is_empty() {
            0u64
        } else {
            u64::from_str_radix(frac_part, 16).ok()?
        };
        let frac_bits = frac_part.len() as u32 * 4;
        int_val as f64 + frac_val as f64 / (1u64 << frac_bits) as f64
    } else {
        u64::from_str_radix(mantissa_str, 16).ok()? as f64
    };

    let exp: i32 = exp_str.parse().ok()?;
    let result = mantissa * (2.0_f64).powi(exp);
    Some(if negative { -result } else { result })
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: run sprintf with Value args and return the String.
    fn sf(fmt: &str, args: &[Value]) -> String {
        sprintf(fmt, args).unwrap()
    }

    // ─── needs_space ────────────────────────────────────────────────

    #[test]
    fn needs_space_two_ints() {
        assert!(needs_space(&Value::Int(1), &Value::Int(2)));
    }

    #[test]
    fn needs_space_two_strings() {
        let a = Value::String("a".into());
        let b = Value::String("b".into());
        assert!(!needs_space(&a, &b));
    }

    #[test]
    fn needs_space_string_int() {
        let s = Value::String("x".into());
        assert!(!needs_space(&s, &Value::Int(1)));
        assert!(!needs_space(&Value::Int(1), &s));
    }

    #[test]
    fn needs_space_bool_bool() {
        assert!(needs_space(&Value::Bool(true), &Value::Bool(false)));
    }

    // ─── go_quote ───────────────────────────────────────────────────

    #[test]
    fn quote_simple() {
        assert_eq!(go_quote("hello"), r#""hello""#);
    }

    #[test]
    fn quote_empty() {
        assert_eq!(go_quote(""), r#""""#);
    }

    #[test]
    fn quote_special_chars() {
        assert_eq!(go_quote("a\"b"), r#""a\"b""#);
        assert_eq!(go_quote("a\\b"), r#""a\\b""#);
        assert_eq!(go_quote("a\nb"), r#""a\nb""#);
        assert_eq!(go_quote("a\tb"), r#""a\tb""#);
        assert_eq!(go_quote("a\rb"), r#""a\rb""#);
    }

    #[test]
    fn quote_bell_backspace_formfeed_vtab() {
        assert_eq!(go_quote("\x07"), r#""\a""#);
        assert_eq!(go_quote("\x08"), r#""\b""#);
        assert_eq!(go_quote("\x0C"), r#""\f""#);
        assert_eq!(go_quote("\x0B"), r#""\v""#);
    }

    #[test]
    fn quote_control_chars() {
        assert_eq!(go_quote("\x01"), r#""\x01""#);
        assert_eq!(go_quote("\x1f"), r#""\x1f""#);
        assert_eq!(go_quote("\x7f"), r#""\x7f""#);
    }

    #[test]
    fn quote_unicode_passthrough() {
        assert_eq!(go_quote("caf\u{00e9}"), "\"caf\u{00e9}\"");
    }

    // ─── go_backquote ───────────────────────────────────────────────

    #[test]
    fn backquote_simple() {
        assert_eq!(go_backquote("hello"), Some("`hello`".into()));
    }

    #[test]
    fn backquote_with_tab() {
        // Tab is allowed in backtick strings
        assert_eq!(go_backquote("a\tb"), Some("`a\tb`".into()));
    }

    #[test]
    fn backquote_rejects_backtick() {
        assert_eq!(go_backquote("hel`lo"), None);
    }

    #[test]
    fn backquote_rejects_control_char() {
        assert_eq!(go_backquote("a\x01b"), None);
        assert_eq!(go_backquote("a\nb"), None);
        assert_eq!(go_backquote("\x7f"), None);
    }

    #[test]
    fn backquote_empty() {
        assert_eq!(go_backquote(""), Some("``".into()));
    }

    // ─── go_normalize_sci ───────────────────────────────────────────

    #[test]
    fn normalize_positive_single_digit_exp() {
        assert_eq!(go_normalize_sci("1.5e0"), "1.5e+00");
        assert_eq!(go_normalize_sci("1.5e2"), "1.5e+02");
        assert_eq!(go_normalize_sci("1.5e9"), "1.5e+09");
    }

    #[test]
    fn normalize_negative_single_digit_exp() {
        assert_eq!(go_normalize_sci("1.5e-3"), "1.5e-03");
        assert_eq!(go_normalize_sci("1.5e-9"), "1.5e-09");
    }

    #[test]
    fn normalize_already_two_digit_exp() {
        assert_eq!(go_normalize_sci("1.5e+10"), "1.5e+10");
        assert_eq!(go_normalize_sci("1.5e-10"), "1.5e-10");
    }

    #[test]
    fn normalize_large_exp() {
        assert_eq!(go_normalize_sci("5e-324"), "5e-324");
        assert_eq!(go_normalize_sci("1e308"), "1e+308");
    }

    #[test]
    fn normalize_uppercase() {
        assert_eq!(go_normalize_sci("1.5E2"), "1.5E+02");
        assert_eq!(go_normalize_sci("1.5E-3"), "1.5E-03");
    }

    #[test]
    fn normalize_no_exponent() {
        assert_eq!(go_normalize_sci("3.14"), "3.14");
    }

    #[test]
    fn normalize_explicit_plus() {
        assert_eq!(go_normalize_sci("1.5e+2"), "1.5e+02");
    }

    // ─── strip_trailing_zeros ───────────────────────────────────────

    #[test]
    fn strip_zeros_basic() {
        assert_eq!(strip_trailing_zeros("1.50000"), "1.5");
        assert_eq!(strip_trailing_zeros("1.0"), "1");
        assert_eq!(strip_trailing_zeros("1.20"), "1.2");
    }

    #[test]
    fn strip_zeros_no_dot() {
        assert_eq!(strip_trailing_zeros("42"), "42");
    }

    #[test]
    fn strip_zeros_no_trailing() {
        assert_eq!(strip_trailing_zeros("1.23"), "1.23");
    }

    #[test]
    fn strip_zeros_all_fractional_zeros() {
        assert_eq!(strip_trailing_zeros("5.000"), "5");
    }

    // ─── strip_trailing_zeros_sci ───────────────────────────────────

    #[test]
    fn strip_zeros_sci_basic() {
        assert_eq!(strip_trailing_zeros_sci("1.50000e+02"), "1.5e+02");
        assert_eq!(strip_trailing_zeros_sci("1.00000e+02"), "1e+02");
    }

    #[test]
    fn strip_zeros_sci_no_trailing() {
        assert_eq!(strip_trailing_zeros_sci("1.23e+02"), "1.23e+02");
    }

    #[test]
    fn strip_zeros_sci_uppercase() {
        assert_eq!(strip_trailing_zeros_sci("1.50000E+02"), "1.5E+02");
    }

    // ─── format_g_default ───────────────────────────────────────────

    #[test]
    fn g_default_zero() {
        assert_eq!(format_g_default(0.0, false), "0");
    }

    #[test]
    fn g_default_negative_zero() {
        assert_eq!(format_g_default(-0.0, false), "-0");
    }

    #[test]
    fn g_default_small() {
        assert_eq!(format_g_default(3.5, false), "3.5");
        assert_eq!(format_g_default(0.5, false), "0.5");
        assert_eq!(format_g_default(100000.0, false), "100000");
    }

    #[test]
    fn g_default_switches_to_sci() {
        // exp >= 6 → sci notation
        assert_eq!(format_g_default(1e6, false), "1e+06");
        assert_eq!(format_g_default(1e7, false), "1e+07");
    }

    #[test]
    fn g_default_very_small_switches_to_sci() {
        // exp < -4 → sci notation
        assert_eq!(format_g_default(0.000035, false), "3.5e-05");
    }

    #[test]
    fn g_default_upper() {
        assert_eq!(format_g_default(1e7, true), "1E+07");
    }

    // ─── format_g_with_precision ────────────────────────────────────

    #[test]
    fn g_prec_basic() {
        assert_eq!(format_g_with_precision(3.5, 4, false), "3.5");
        assert_eq!(format_g_with_precision(1.5, 6, false), "1.5");
    }

    #[test]
    fn g_prec_switches_to_sci() {
        // exp (5) >= prec (4) → sci
        assert_eq!(format_g_with_precision(123456.0, 4, false), "1.235e+05");
    }

    #[test]
    fn g_prec_rounding() {
        assert_eq!(format_g_with_precision(10.0, 2, false), "10");
        assert_eq!(format_g_with_precision(100.0, 2, false), "1e+02");
    }

    #[test]
    fn g_prec_small_number() {
        // exp = -2, prec = 2 → f_prec = 2-(-2)-1 = 3
        assert_eq!(format_g_with_precision(0.0123, 2, false), "0.012");
    }

    #[test]
    fn g_prec_zero() {
        assert_eq!(format_g_with_precision(0.0, 4, false), "0");
    }

    #[test]
    fn g_prec_upper() {
        assert_eq!(format_g_with_precision(1e7, 4, true), "1E+07");
    }

    // ─── format_int_base ────────────────────────────────────────────

    #[test]
    fn int_base_hex() {
        let spec = FmtSpec {
            left_align: false,
            plus: false,
            space: false,
            hash: false,
            zero: false,
            width: None,
            precision: None,
        };
        assert_eq!(format_int_base(255, "x", &spec), "ff");
        assert_eq!(format_int_base(255, "X", &spec), "FF");
        assert_eq!(format_int_base(-255, "x", &spec), "-ff");
    }

    #[test]
    fn int_base_octal() {
        let spec = FmtSpec {
            left_align: false,
            plus: false,
            space: false,
            hash: false,
            zero: false,
            width: None,
            precision: None,
        };
        assert_eq!(format_int_base(8, "o", &spec), "10");
    }

    #[test]
    fn int_base_binary() {
        let spec = FmtSpec {
            left_align: false,
            plus: false,
            space: false,
            hash: false,
            zero: false,
            width: None,
            precision: None,
        };
        assert_eq!(format_int_base(10, "b", &spec), "1010");
    }

    #[test]
    fn int_base_hash_prefix() {
        let spec = FmtSpec {
            left_align: false,
            plus: false,
            space: false,
            hash: true,
            zero: false,
            width: None,
            precision: None,
        };
        assert_eq!(format_int_base(255, "x", &spec), "0xff");
        assert_eq!(format_int_base(255, "X", &spec), "0XFF");
        assert_eq!(format_int_base(8, "o", &spec), "010");
        assert_eq!(format_int_base(10, "b", &spec), "0b1010");
        assert_eq!(format_int_base(-255, "x", &spec), "-0xff");
    }

    // ─── html_escape ────────────────────────────────────────────────

    #[test]
    fn html_basic() {
        assert_eq!(html_escape("<b>hi</b>"), "&lt;b&gt;hi&lt;/b&gt;");
    }

    #[test]
    fn html_ampersand() {
        assert_eq!(html_escape("a&b"), "a&amp;b");
    }

    #[test]
    fn html_quotes() {
        assert_eq!(html_escape(r#"a"b'c"#), "a&#34;b&#39;c");
    }

    #[test]
    fn html_nul() {
        assert_eq!(html_escape("a\0b"), "a\u{FFFD}b");
    }

    #[test]
    fn html_passthrough() {
        assert_eq!(html_escape("hello world"), "hello world");
    }

    #[test]
    fn html_empty() {
        assert_eq!(html_escape(""), "");
    }

    // ─── js_escape ──────────────────────────────────────────────────

    #[test]
    fn js_basic() {
        assert_eq!(js_escape("It'd be nice."), "It\\'d be nice.");
    }

    #[test]
    fn js_backslash_and_quotes() {
        assert_eq!(js_escape(r#"a\b"c"#), r#"a\\b\"c"#);
    }

    #[test]
    fn js_newline_tab() {
        assert_eq!(js_escape("a\nb\tc"), "a\\u000Ab\\u0009c");
    }

    #[test]
    fn js_angle_brackets_ampersand_equals() {
        assert_eq!(js_escape("<b>&="), "\\u003Cb\\u003E\\u0026\\u003D");
    }

    #[test]
    fn js_control_char() {
        assert_eq!(js_escape("\x01"), "\\u0001"); // already uppercase (single digit)
    }

    #[test]
    fn js_carriage_return() {
        assert_eq!(js_escape("\r"), "\\u000D");
    }

    #[test]
    fn js_passthrough() {
        assert_eq!(js_escape("hello"), "hello");
    }

    #[test]
    fn js_empty() {
        assert_eq!(js_escape(""), "");
    }

    // ─── url_encode ─────────────────────────────────────────────────

    #[test]
    fn url_basic() {
        assert_eq!(url_encode("hello world"), "hello%20world");
    }

    #[test]
    fn url_unreserved_passthrough() {
        assert_eq!(url_encode("az-AZ-09-_.~"), "az-AZ-09-_.~");
    }

    #[test]
    fn url_special_chars() {
        assert_eq!(url_encode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn url_slash() {
        assert_eq!(
            url_encode("http://www.example.org/"),
            "http%3A%2F%2Fwww.example.org%2F"
        );
    }

    #[test]
    fn url_unicode() {
        // é = 0xC3 0xA9 in UTF-8
        assert_eq!(url_encode("café"), "caf%C3%A9");
    }

    #[test]
    fn url_empty() {
        assert_eq!(url_encode(""), "");
    }

    // ─── parse_hex_float ────────────────────────────────────────────

    #[test]
    fn hex_float_basic() {
        assert_eq!(parse_hex_float("0x1.ep+2"), Some(7.5));
    }

    #[test]
    fn hex_float_no_frac() {
        assert_eq!(parse_hex_float("0x1p+4"), Some(16.0));
    }

    #[test]
    fn hex_float_negative() {
        assert_eq!(parse_hex_float("-0x1p-2"), Some(-0.25));
    }

    #[test]
    fn hex_float_positive_sign() {
        assert_eq!(parse_hex_float("+0x1.ep+2"), Some(7.5));
    }

    #[test]
    fn hex_float_uppercase() {
        assert_eq!(parse_hex_float("0X1.EP+2"), Some(7.5));
    }

    #[test]
    fn hex_float_no_exponent() {
        assert_eq!(parse_hex_float("0xA"), Some(10.0));
    }

    #[test]
    fn hex_float_zero() {
        assert_eq!(parse_hex_float("0x0p0"), Some(0.0));
    }

    #[test]
    fn hex_float_invalid() {
        assert_eq!(parse_hex_float("not_a_float"), None);
        assert_eq!(parse_hex_float(""), None);
    }

    // ─── sprintf: %s ────────────────────────────────────────────────

    #[test]
    fn sprintf_s_basic() {
        assert_eq!(sf("%s", &[Value::String("hello".into())]), "hello");
    }

    #[test]
    fn sprintf_s_non_string() {
        assert_eq!(sf("%s", &[Value::Int(42)]), "42");
        assert_eq!(sf("%s", &[Value::Bool(true)]), "true");
        assert_eq!(sf("%s", &[Value::Nil]), "<nil>");
    }

    #[test]
    fn sprintf_s_width() {
        assert_eq!(sf("%10s", &[Value::String("hi".into())]), "        hi");
        assert_eq!(sf("%-10s", &[Value::String("hi".into())]), "hi        ");
    }

    #[test]
    fn sprintf_s_precision() {
        assert_eq!(sf("%.3s", &[Value::String("hello".into())]), "hel");
        assert_eq!(sf("%.10s", &[Value::String("hi".into())]), "hi");
    }

    // ─── sprintf: %d ────────────────────────────────────────────────

    #[test]
    fn sprintf_d_basic() {
        assert_eq!(sf("%d", &[Value::Int(42)]), "42");
        assert_eq!(sf("%d", &[Value::Int(-42)]), "-42");
        assert_eq!(sf("%d", &[Value::Int(0)]), "0");
    }

    #[test]
    fn sprintf_d_plus_flag() {
        assert_eq!(sf("%+d", &[Value::Int(42)]), "+42");
        assert_eq!(sf("%+d", &[Value::Int(-42)]), "-42");
    }

    #[test]
    fn sprintf_d_space_flag() {
        assert_eq!(sf("% d", &[Value::Int(42)]), " 42");
        assert_eq!(sf("% d", &[Value::Int(-42)]), "-42");
    }

    #[test]
    fn sprintf_d_width() {
        assert_eq!(sf("%10d", &[Value::Int(42)]), "        42");
        assert_eq!(sf("%-10d", &[Value::Int(42)]), "42        ");
    }

    #[test]
    fn sprintf_d_zero_pad() {
        assert_eq!(sf("%06d", &[Value::Int(42)]), "000042");
        assert_eq!(sf("%06d", &[Value::Int(-42)]), "-00042");
    }

    #[test]
    fn sprintf_d_zero_pad_with_plus() {
        assert_eq!(sf("%+06d", &[Value::Int(42)]), "+00042");
    }

    // ─── sprintf: %f ────────────────────────────────────────────────

    #[test]
    fn sprintf_f_default_precision() {
        assert_eq!(sf("%f", &[Value::Float(1.5)]), "1.500000");
        assert_eq!(sf("%f", &[Value::Float(0.0)]), "0.000000");
    }

    #[test]
    fn sprintf_f_explicit_precision() {
        assert_eq!(sf("%.2f", &[Value::Float(1.5)]), "1.50");
        assert_eq!(sf("%.0f", &[Value::Float(1.5)]), "2"); // rounded
    }

    #[test]
    fn sprintf_f_plus_flag() {
        assert_eq!(sf("%+f", &[Value::Float(1.5)]), "+1.500000");
        assert_eq!(sf("%+f", &[Value::Float(-1.5)]), "-1.500000");
    }

    #[test]
    fn sprintf_f_space_flag() {
        assert_eq!(sf("% f", &[Value::Float(1.5)]), " 1.500000");
        assert_eq!(sf("% f", &[Value::Float(-1.5)]), "-1.500000");
    }

    // ─── sprintf: %e / %E ───────────────────────────────────────────

    #[test]
    fn sprintf_e_default() {
        assert_eq!(sf("%e", &[Value::Float(1.5)]), "1.500000e+00");
        assert_eq!(sf("%e", &[Value::Float(100.0)]), "1.000000e+02");
        assert_eq!(sf("%e", &[Value::Float(0.001)]), "1.000000e-03");
    }

    #[test]
    fn sprintf_e_precision() {
        assert_eq!(sf("%.2e", &[Value::Float(1.5)]), "1.50e+00");
        assert_eq!(sf("%.0e", &[Value::Float(1.5)]), "2e+00");
    }

    #[test]
    fn sprintf_e_uppercase() {
        assert_eq!(sf("%E", &[Value::Float(1.5)]), "1.500000E+00");
    }

    #[test]
    fn sprintf_e_plus_flag() {
        assert_eq!(sf("%+e", &[Value::Float(1.5)]), "+1.500000e+00");
        assert_eq!(sf("%+e", &[Value::Float(-1.5)]), "-1.500000e+00");
    }

    #[test]
    fn sprintf_e_zero() {
        assert_eq!(sf("%e", &[Value::Float(0.0)]), "0.000000e+00");
    }

    // ─── sprintf: %g / %G ───────────────────────────────────────────

    #[test]
    fn sprintf_g_default() {
        assert_eq!(sf("%g", &[Value::Float(3.5)]), "3.5");
        assert_eq!(sf("%g", &[Value::Float(0.0)]), "0");
        assert_eq!(sf("%g", &[Value::Float(1.0)]), "1");
    }

    #[test]
    fn sprintf_g_large_switches_to_sci() {
        assert_eq!(sf("%g", &[Value::Float(1e6)]), "1e+06");
        assert_eq!(sf("%g", &[Value::Float(1e7)]), "1e+07");
    }

    #[test]
    fn sprintf_g_small_switches_to_sci() {
        assert_eq!(sf("%g", &[Value::Float(0.000035)]), "3.5e-05");
    }

    #[test]
    fn sprintf_g_boundary() {
        // exp = 5 → still decimal
        assert_eq!(sf("%g", &[Value::Float(100000.0)]), "100000");
        // exp = 6 → switches to sci
        assert_eq!(sf("%g", &[Value::Float(1000000.0)]), "1e+06");
    }

    #[test]
    fn sprintf_g_precision() {
        assert_eq!(sf("%.4g", &[Value::Float(123456.0)]), "1.235e+05");
        assert_eq!(sf("%.2g", &[Value::Float(10.0)]), "10");
        assert_eq!(sf("%.2g", &[Value::Float(100.0)]), "1e+02");
    }

    #[test]
    fn sprintf_g_uppercase() {
        assert_eq!(sf("%G", &[Value::Float(1e7)]), "1E+07");
    }

    #[test]
    fn sprintf_g_plus_flag() {
        assert_eq!(sf("%+g", &[Value::Float(3.5)]), "+3.5");
        assert_eq!(sf("%+g", &[Value::Float(-3.5)]), "-3.5");
    }

    #[test]
    fn sprintf_g_negative_zero() {
        assert_eq!(sf("%g", &[Value::Float(-0.0)]), "-0");
    }

    // ─── sprintf: %v ────────────────────────────────────────────────

    #[test]
    fn sprintf_v() {
        assert_eq!(sf("%v", &[Value::Int(42)]), "42");
        assert_eq!(sf("%v", &[Value::String("hi".into())]), "hi");
        assert_eq!(sf("%v", &[Value::Bool(true)]), "true");
        assert_eq!(sf("%v", &[Value::Nil]), "<nil>");
    }

    // ─── sprintf: %q ────────────────────────────────────────────────

    #[test]
    fn sprintf_q_basic() {
        assert_eq!(sf("%q", &[Value::String("hello".into())]), r#""hello""#);
    }

    #[test]
    fn sprintf_q_with_special() {
        assert_eq!(sf("%q", &[Value::String("a\nb".into())]), r#""a\nb""#);
    }

    #[test]
    fn sprintf_hash_q_backtick() {
        assert_eq!(sf("%#q", &[Value::String("hello".into())]), "`hello`");
    }

    #[test]
    fn sprintf_hash_q_fallback_on_backtick() {
        assert_eq!(sf("%#q", &[Value::String("hel`lo".into())]), r#""hel`lo""#);
    }

    #[test]
    fn sprintf_hash_q_fallback_on_control() {
        assert_eq!(sf("%#q", &[Value::String("a\nb".into())]), r#""a\nb""#);
    }

    // ─── sprintf: %t ────────────────────────────────────────────────

    #[test]
    fn sprintf_t() {
        assert_eq!(sf("%t", &[Value::Bool(true)]), "true");
        assert_eq!(sf("%t", &[Value::Bool(false)]), "false");
    }

    #[test]
    fn sprintf_t_non_bool() {
        assert_eq!(sf("%t", &[Value::Int(42)]), "42");
    }

    // ─── sprintf: %x / %X / %o / %b ────────────────────────────────

    #[test]
    fn sprintf_x_basic() {
        assert_eq!(sf("%x", &[Value::Int(255)]), "ff");
        assert_eq!(sf("%X", &[Value::Int(255)]), "FF");
    }

    #[test]
    fn sprintf_x_negative() {
        assert_eq!(sf("%x", &[Value::Int(-255)]), "-ff");
    }

    #[test]
    fn sprintf_x_hash() {
        assert_eq!(sf("%#x", &[Value::Int(255)]), "0xff");
        assert_eq!(sf("%#X", &[Value::Int(255)]), "0XFF");
    }

    #[test]
    fn sprintf_x_zero_pad() {
        assert_eq!(sf("%04x", &[Value::Int(127)]), "007f");
    }

    #[test]
    fn sprintf_o() {
        assert_eq!(sf("%o", &[Value::Int(8)]), "10");
        assert_eq!(sf("%#o", &[Value::Int(8)]), "010");
    }

    #[test]
    fn sprintf_b() {
        assert_eq!(sf("%b", &[Value::Int(10)]), "1010");
        assert_eq!(sf("%#b", &[Value::Int(10)]), "0b1010");
    }

    // ─── sprintf: %c ────────────────────────────────────────────────

    #[test]
    fn sprintf_c() {
        assert_eq!(sf("%c", &[Value::Int(65)]), "A");
        assert_eq!(sf("%c", &[Value::Int(0x1F600)]), "\u{1F600}"); // 😀
    }

    // ─── sprintf: %% and mixed ──────────────────────────────────────

    #[test]
    fn sprintf_percent_literal() {
        assert_eq!(sf("100%%", &[]), "100%");
    }

    #[test]
    fn sprintf_multiple_verbs() {
        assert_eq!(
            sf(
                "%d %s %d %s",
                &[
                    Value::Int(1),
                    Value::String("one".into()),
                    Value::Int(2),
                    Value::String("two".into())
                ]
            ),
            "1 one 2 two"
        );
    }

    #[test]
    fn sprintf_missing_arg() {
        assert_eq!(sf("%d %d", &[Value::Int(1)]), "1 %!d(MISSING)");
    }

    #[test]
    fn sprintf_no_args_no_verbs() {
        assert_eq!(sf("hello world", &[]), "hello world");
    }

    #[test]
    fn sprintf_trailing_percent() {
        assert_eq!(sf("test%", &[]), "test%");
    }

    #[test]
    fn sprintf_unknown_verb() {
        assert_eq!(sf("%z", &[Value::Int(1)]), "%z");
    }

    // ─── FmtSpec::pad ───────────────────────────────────────────────

    #[test]
    fn pad_no_width() {
        let spec = FmtSpec {
            left_align: false,
            plus: false,
            space: false,
            hash: false,
            zero: false,
            width: None,
            precision: None,
        };
        assert_eq!(spec.pad("hello", false), "hello");
    }

    #[test]
    fn pad_right_align() {
        let spec = FmtSpec {
            left_align: false,
            plus: false,
            space: false,
            hash: false,
            zero: false,
            width: Some(10),
            precision: None,
        };
        assert_eq!(spec.pad("hi", false), "        hi");
    }

    #[test]
    fn pad_left_align() {
        let spec = FmtSpec {
            left_align: true,
            plus: false,
            space: false,
            hash: false,
            zero: false,
            width: Some(10),
            precision: None,
        };
        assert_eq!(spec.pad("hi", false), "hi        ");
    }

    #[test]
    fn pad_zero_unsigned() {
        let spec = FmtSpec {
            left_align: false,
            plus: false,
            space: false,
            hash: false,
            zero: true,
            width: Some(6),
            precision: None,
        };
        assert_eq!(spec.pad("42", true), "000042");
    }

    #[test]
    fn pad_zero_negative() {
        let spec = FmtSpec {
            left_align: false,
            plus: false,
            space: false,
            hash: false,
            zero: true,
            width: Some(6),
            precision: None,
        };
        assert_eq!(spec.pad("-42", true), "-00042");
    }

    #[test]
    fn pad_zero_positive_sign() {
        let spec = FmtSpec {
            left_align: false,
            plus: false,
            space: false,
            hash: false,
            zero: true,
            width: Some(6),
            precision: None,
        };
        assert_eq!(spec.pad("+42", true), "+00042");
    }

    #[test]
    fn pad_wider_than_needed() {
        let spec = FmtSpec {
            left_align: false,
            plus: false,
            space: false,
            hash: false,
            zero: false,
            width: Some(2),
            precision: None,
        };
        // String already wider, no padding needed
        assert_eq!(spec.pad("hello", false), "hello");
    }

    // ─── FmtSpec::format_signed ─────────────────────────────────────

    #[test]
    fn format_signed_plain() {
        let spec = FmtSpec {
            left_align: false,
            plus: false,
            space: false,
            hash: false,
            zero: false,
            width: None,
            precision: None,
        };
        assert_eq!(spec.format_signed(42), "42");
        assert_eq!(spec.format_signed(-42), "-42");
        assert_eq!(spec.format_signed(0), "0");
    }

    #[test]
    fn format_signed_plus() {
        let spec = FmtSpec {
            left_align: false,
            plus: true,
            space: false,
            hash: false,
            zero: false,
            width: None,
            precision: None,
        };
        assert_eq!(spec.format_signed(42), "+42");
        assert_eq!(spec.format_signed(-42), "-42");
        assert_eq!(spec.format_signed(0), "+0");
    }

    #[test]
    fn format_signed_space() {
        let spec = FmtSpec {
            left_align: false,
            plus: false,
            space: true,
            hash: false,
            zero: false,
            width: None,
            precision: None,
        };
        assert_eq!(spec.format_signed(42), " 42");
        assert_eq!(spec.format_signed(-42), "-42");
        assert_eq!(spec.format_signed(0), " 0");
    }
}
