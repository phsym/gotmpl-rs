//! Go-compatible formatting, escaping, and number parsing.
//!
//! Holds the adaptations needed to match Go's `fmt` package and `text/template`
//! output conventions. Used by the built-in template functions
//! ([`print`](crate::funcs), [`printf`](crate::funcs), etc.) and by the lexer
//! for number literal parsing.
//!
//! # What lives here
//!
//! | Category | Functions | Go equivalent |
//! |----------|-----------|---------------|
//! | sprintf | [`sprintf`] | `fmt.Sprintf` |
//! | Sprint spacing | [`needs_space`] | `fmt.Sprint` inter-arg spacing rule |
//! | Quoting | [`go_quote_into`], [`try_backquote_into`] | `strconv.Quote`, `%#q` |
//! | Sci-notation | [`write_normalized_sci`] | Exponent `e+00` format |
//! | `%g` formatting | [`write_g_default`], [`write_g_with_precision`] | `%g` / `%.Ng` |
//! | Integer bases | [`format_int_base_into`] | `%x`, `%o`, `%b` with sign handling |
//! | HTML escape | [`html_escape`] | `template.HTMLEscapeString` |
//! | JS escape | [`js_escape`] | `template.JSEscapeString` |
//! | URL encode | [`url_encode`] | `template.URLQueryEscaper` |
//! | Hex float parse | [`parse_hex_float`] | Hex float literal `0x1.Fp10` |

use alloc::format;
use alloc::string::String;
use core::fmt::Write;

use crate::error::Result;
use crate::value::Value;

// fmt.Sprint spacing
/// Returns `true` when Go's `fmt.Sprint` would insert a space between two
/// adjacent arguments (i.e. neither is a string).
pub(crate) fn needs_space(prev: &Value, next: &Value) -> bool {
    !matches!(prev, Value::String(_)) && !matches!(next, Value::String(_))
}

// sprintf
/// Maximum allowed printf width or precision. Larger values are clamped to
/// this cap to prevent attacker-controlled format strings from triggering
/// multi-gigabyte padding allocations. Also stays below Rust's internal
/// formatter count limit (u16::MAX), which panics on larger widths.
pub(crate) const PRINTF_MAX_LEN: usize = u16::MAX as usize;

fn clamp_fmt_len(n: usize) -> usize {
    if n > PRINTF_MAX_LEN {
        PRINTF_MAX_LEN
    } else {
        n
    }
}

/// Parsed printf format specifier (`flags`, `width`, `precision`).
///
/// Mirrors the subset of Go's `fmt` format grammar that `text/template` uses.
#[derive(Default)]
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
    fn from_flags(chars: &mut core::iter::Peekable<core::str::Chars<'_>>) -> Self {
        let mut spec = FmtSpec::default();
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
        spec
    }

    /// Pad `out[start..]` to the configured width in place.
    ///
    /// Width is measured in Unicode scalars (chars) to match Rust's formatter
    /// semantics and Go parity on the verbs we support. Padding is inserted
    /// only when the already-written content is shorter than `width`.
    ///
    /// `start` must sit on a char boundary in `out`. Callers always invoke
    /// at `out.len()` taken before the verb's content was pushed, which
    /// trivially satisfies that requirement.
    fn pad_in_place(&self, out: &mut String, start: usize, is_numeric: bool) {
        let Some(width) = self.width else {
            return;
        };
        let written = out[start..].chars().count();
        if written >= width {
            return;
        }
        let pad_count = width - written;

        if self.left_align {
            for _ in 0..pad_count {
                out.push(' ');
            }
        } else if self.zero && is_numeric {
            // Zero-pad after a leading '+' or '-' sign so `-042` stays sign-first.
            let sign_off = match out.as_bytes().get(start) {
                Some(&b'-') | Some(&b'+') => 1,
                _ => 0,
            };
            insert_repeated(out, start + sign_off, '0', pad_count);
        } else {
            insert_repeated(out, start, ' ', pad_count);
        }
    }

    /// Write a signed integer with sign flags (`+`, ` `) applied into `out`.
    fn write_signed(&self, out: &mut String, n: i64) {
        if n >= 0 {
            if self.plus {
                out.push('+');
            } else if self.space {
                out.push(' ');
            }
        }
        let _ = write!(out, "{}", n);
    }
}

/// Insert `count` copies of `ch` at byte position `pos` in `out`.
///
/// One allocation for the pad buffer (via `str::repeat`), then a single
/// `insert_str` memmove. Callers pass ASCII characters, so `pos` is
/// guaranteed to sit on a char boundary.
fn insert_repeated(out: &mut String, pos: usize, ch: char, count: usize) {
    let mut buf = [0u8; 4];
    let s: &str = ch.encode_utf8(&mut buf);
    out.insert_str(pos, &s.repeat(count));
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
    let mut out = String::with_capacity(fmt_str.len() + 16 * args.len());
    sprintf_into(&mut out, fmt_str, args)?;
    Ok(out)
}

/// Read a run of ASCII digits as a `usize` (saturating on overflow).
///
/// Returns `None` if no digits were consumed.
fn parse_uint_digits(chars: &mut core::iter::Peekable<core::str::Chars<'_>>) -> Option<usize> {
    let mut n: usize = 0;
    let mut any = false;
    while let Some(c) = chars.next_if(|c| c.is_ascii_digit()) {
        n = n
            .saturating_mul(10)
            .saturating_add((c as u8 - b'0') as usize);
        any = true;
    }
    any.then_some(n)
}

enum ParsedIndex {
    Absent,
    /// 1-based; caller validates `N` against `args.len()`.
    Found(usize),
    Bad,
}

enum IndexOutcome {
    None,
    Applied,
    /// Marker was malformed/out-of-range; BADINDEX has been written and the
    /// caller must `continue`.
    Bad,
}

/// Try to parse a `[N]` argument-index marker at the current position.
///
/// On a malformed bracket: if a `]` exists later in the format, consume up
/// to and past it; otherwise consume only the `[` so the remaining chars
/// (digits, verb) flow back through the regular parse path — `%[1d` ends
/// up as `%!d(BADINDEX)` rather than a swallowed directive.
fn try_parse_index(chars: &mut core::iter::Peekable<core::str::Chars<'_>>) -> ParsedIndex {
    if chars.peek() != Some(&'[') {
        return ParsedIndex::Absent;
    }
    let mut probe = chars.clone();
    probe.next(); // consume '['
    if let Some(n) = parse_uint_digits(&mut probe) {
        if probe.peek() == Some(&']') {
            probe.next(); // consume ']'
            *chars = probe;
            return ParsedIndex::Found(n);
        }
    }
    chars.next(); // consume '['
    // Only the malformed path needs to know whether a `]` exists later, so
    // we do the scan here rather than up front.
    if chars.clone().any(|c| c == ']') {
        for c in chars.by_ref() {
            if c == ']' {
                break;
            }
        }
    }
    ParsedIndex::Bad
}

/// Parse an optional `[N]` argument-index marker and apply its effect.
///
/// On a valid in-range index, updates `arg_idx`/`reordered` and returns
/// `Applied`. On out-of-range or malformed `[…]`, writes the BADINDEX marker
/// into `out`, sets `reordered` (so a trailing `%!(EXTRA …)` doesn't fire
/// over an unconsumed arg), and returns `Bad` for the caller to `continue`.
fn consume_index(
    chars: &mut core::iter::Peekable<core::str::Chars<'_>>,
    out: &mut String,
    arg_idx: &mut usize,
    reordered: &mut bool,
    args_len: usize,
) -> IndexOutcome {
    match try_parse_index(chars) {
        ParsedIndex::Absent => IndexOutcome::None,
        ParsedIndex::Found(n) if n >= 1 && n <= args_len => {
            *arg_idx = n - 1;
            *reordered = true;
            IndexOutcome::Applied
        }
        ParsedIndex::Found(_) | ParsedIndex::Bad => {
            let verb = skip_to_verb(chars);
            let _ = write!(out, "%!{}(BADINDEX)", verb);
            *reordered = true;
            IndexOutcome::Bad
        }
    }
}

/// Read `args[*arg_idx]` for `*` width / `.*` precision.
///
/// A wrong-type arg is still considered "consumed" (the cursor advances and
/// `Err(())` is returned); a missing arg does not advance the cursor. This
/// asymmetry matches Go's behavior so a trailing `MISSING` marker still
/// fires for the verb when no width arg was actually present.
fn take_int_arg(args: &[Value], arg_idx: &mut usize) -> core::result::Result<i64, ()> {
    match args.get(*arg_idx) {
        Some(&Value::Int(n)) => {
            *arg_idx += 1;
            Ok(n)
        }
        Some(_) => {
            *arg_idx += 1;
            Err(())
        }
        None => Err(()),
    }
}

/// Skip ahead to the verb char (the next non-flag/digit/dot/star/bracket).
/// Used after a BADINDEX to drain the rest of the directive.
fn skip_to_verb(chars: &mut core::iter::Peekable<core::str::Chars<'_>>) -> char {
    let mut in_brackets = false;
    while let Some(&c) = chars.peek() {
        if in_brackets {
            chars.next();
            if c == ']' {
                in_brackets = false;
            }
            continue;
        }
        match c {
            '-' | '+' | ' ' | '#' | '0' | '.' | '*' => {
                chars.next();
            }
            '[' => {
                chars.next();
                in_brackets = true;
            }
            c if c.is_ascii_digit() => {
                chars.next();
            }
            _ => {
                chars.next();
                return c;
            }
        }
    }
    '?'
}

/// Format directly into `out`. Each verb writes its value at `out.len()`,
/// then pads in place. Supports Go's `[N]` argument indexing and `*` /
/// `.*` dynamic width/precision.
fn sprintf_into(out: &mut String, fmt_str: &str, args: &[Value]) -> Result<()> {
    let mut chars = fmt_str.chars().peekable();
    let mut arg_idx: usize = 0;
    let mut reordered = false;

    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }

        let mut spec = FmtSpec::from_flags(&mut chars);

        // Optional `[N]` after flags (the grammar puts the index *after*
        // flags, not before — so `%[1]-d` parses `-` as the verb, not as
        // a left-align flag).
        let first_index = consume_index(&mut chars, out, &mut arg_idx, &mut reordered, args.len());
        if matches!(first_index, IndexOutcome::Bad) {
            continue;
        }
        let had_index_marker = !matches!(first_index, IndexOutcome::None);

        let mut bad_width = false;
        if chars.peek() == Some(&'*') {
            chars.next();
            // `*` accepts only `int` (float64, string, etc. all yield
            // BADWIDTH) — matched on `Value::Int` directly so floats don't
            // silently truncate via `as_int`.
            match take_int_arg(args, &mut arg_idx) {
                Ok(n) if n >= 0 => spec.width = Some(clamp_fmt_len(n as usize)),
                Ok(n) => {
                    // Negative width → left-align with magnitude as width.
                    // `as usize` may truncate on 32-bit targets for very
                    // large magnitudes, but `clamp_fmt_len` caps the final
                    // width at u16::MAX regardless, so the bound holds.
                    spec.left_align = true;
                    let mag = n.unsigned_abs() as usize;
                    spec.width = Some(clamp_fmt_len(mag));
                }
                Err(()) => bad_width = true,
            }
        } else if let Some(w) = parse_uint_digits(&mut chars) {
            spec.width = Some(clamp_fmt_len(w));
        }

        let mut bad_prec = false;
        if chars.peek() == Some(&'.') {
            chars.next();
            if chars.peek() == Some(&'*') {
                chars.next();
                // Same int-only rule as `*` width; negative precision is bad.
                match take_int_arg(args, &mut arg_idx) {
                    Ok(n) if n >= 0 => spec.precision = Some(clamp_fmt_len(n as usize)),
                    _ => bad_prec = true,
                }
            } else {
                // Empty digits (`%.f`) → precision 0.
                let p = parse_uint_digits(&mut chars).unwrap_or(0);
                spec.precision = Some(clamp_fmt_len(p));
            }
        }

        // Optional `[N]` immediately before the verb. Skipped if a prior
        // `[N]` already fired in this directive, to avoid double-reorder
        // on `%[1][2]d`.
        if !had_index_marker
            && matches!(
                consume_index(&mut chars, out, &mut arg_idx, &mut reordered, args.len()),
                IndexOutcome::Bad
            )
        {
            continue;
        }

        let verb = match chars.next() {
            Some(v) => v,
            None => {
                out.push('%');
                break;
            }
        };

        if verb == '%' {
            out.push('%');
            continue;
        }

        if bad_width {
            out.push_str("%!(BADWIDTH)");
        }
        if bad_prec {
            out.push_str("%!(BADPREC)");
        }

        let arg = if arg_idx < args.len() {
            arg_idx += 1;
            &args[arg_idx - 1]
        } else {
            let _ = write!(out, "%!{}(MISSING)", verb);
            continue;
        };

        let start = out.len();
        match verb {
            's' => match arg {
                Value::String(_) => {
                    match spec.precision {
                        Some(prec) => write_display_truncated(out, arg, prec),
                        None => {
                            let _ = write!(out, "{}", arg);
                        }
                    }
                    spec.pad_in_place(out, start, false);
                }
                _ => write_bad_verb(out, verb, arg),
            },
            'd' => match arg.as_int() {
                Some(n) => {
                    spec.write_signed(out, n);
                    spec.pad_in_place(out, start, true);
                }
                None => write_bad_verb(out, verb, arg),
            },
            'f' => match arg.as_float() {
                Some(f) => {
                    let prec = spec.precision.unwrap_or(6);
                    if f >= 0.0 && !f.is_nan() {
                        if spec.plus {
                            out.push('+');
                        } else if spec.space {
                            out.push(' ');
                        }
                    }
                    let _ = write!(out, "{:.prec$}", f);
                    spec.pad_in_place(out, start, true);
                }
                None => write_bad_verb(out, verb, arg),
            },
            'e' | 'E' => match arg.as_float() {
                Some(f) => {
                    let prec = spec.precision.unwrap_or(6);
                    let raw = if verb == 'e' {
                        format!("{:.prec$e}", f)
                    } else {
                        format!("{:.prec$E}", f)
                    };
                    write_normalized_sci(out, &raw, verb == 'E', false);
                    apply_float_sign_in_place(out, start, f, &spec);
                    spec.pad_in_place(out, start, true);
                }
                None => write_bad_verb(out, verb, arg),
            },
            'g' | 'G' => match arg.as_float() {
                Some(f) => {
                    if f.is_nan() || f.is_infinite() {
                        let _ = write!(out, "{}", f);
                    } else if let Some(prec) = spec.precision {
                        write_g_with_precision(out, f, prec.max(1), verb == 'G');
                    } else {
                        write_g_default(out, f, verb == 'G');
                    };
                    apply_float_sign_in_place(out, start, f, &spec);
                    spec.pad_in_place(out, start, true);
                }
                None => write_bad_verb(out, verb, arg),
            },
            'v' => {
                let _ = write!(out, "{}", arg);
                spec.pad_in_place(out, start, false);
            }
            'U' => match arg {
                Value::Int(n) => {
                    // Go casts the int to uint64 and prints as hex with min 4 digits,
                    // so negative ints wrap (`-1` → `U+FFFFFFFFFFFFFFFF`) and values
                    // outside the Unicode range still format as bare hex.
                    let u = *n as u64;
                    let _ = write!(out, "U+{:04X}", u);
                    if spec.hash {
                        // Only quote when the value is a valid, non-control rune.
                        // Surrogates, > U+10FFFF, and control chars get no quote —
                        // matching Go's `strconv.IsPrint` gate (close enough for
                        // ASCII; full Unicode-graphic parity would need a table).
                        if let Some(c) = u32::try_from(u)
                            .ok()
                            .and_then(char::from_u32)
                            .filter(|c| !c.is_control())
                        {
                            let _ = write!(out, " '{}'", c);
                        }
                    }
                    spec.pad_in_place(out, start, false);
                }
                _ => write_bad_verb(out, verb, arg),
            },
            'q' => match arg {
                Value::String(s) => {
                    if !(spec.hash && try_backquote_into(out, s)) {
                        go_quote_into(out, s);
                    }
                    spec.pad_in_place(out, start, false);
                }
                Value::Int(n) => {
                    // Go's %q on an int emits a single-quoted rune literal.
                    if let Some(c) = u32::try_from(*n).ok().and_then(char::from_u32) {
                        let _ = write!(out, "'{}'", c.escape_default());
                        spec.pad_in_place(out, start, false);
                    } else {
                        write_bad_verb(out, verb, arg);
                    }
                }
                _ => write_bad_verb(out, verb, arg),
            },
            't' => match arg {
                Value::Bool(b) => {
                    out.push_str(if *b { "true" } else { "false" });
                    spec.pad_in_place(out, start, false);
                }
                _ => write_bad_verb(out, verb, arg),
            },
            'x' | 'X' | 'o' | 'b' => match arg {
                Value::String(s) if verb == 'x' || verb == 'X' => {
                    format_string_hex_into(out, s, verb == 'X', &spec);
                    spec.pad_in_place(out, start, false);
                }
                _ => match arg.as_int() {
                    Some(n) => {
                        format_int_base_into(out, n, verb, &spec);
                        // Zero-padding is already baked into the digit string
                        // (Go's scheme: pad digits, then prepend prefix), so
                        // only space-pad the overall field here.
                        spec.pad_in_place(out, start, false);
                    }
                    None => write_bad_verb(out, verb, arg),
                },
            },
            'c' => match arg.as_int() {
                Some(n) => match u32::try_from(n).ok().and_then(char::from_u32) {
                    Some(c) => out.push(c),
                    None => out.push('\u{FFFD}'),
                },
                None => write_bad_verb(out, verb, arg),
            },
            _ => write_bad_verb(out, verb, arg),
        }
    }

    // Trailing %!(EXTRA …) marker for unconsumed args.
    //
    // Go suppresses EXTRA when any [N] indexing was used in the directive —
    // tracking which args got read in that case is too expensive and not
    // unambiguous anyway.
    if !reordered && arg_idx < args.len() {
        out.push_str("%!(EXTRA ");
        for (i, v) in args[arg_idx..].iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            write_bad_arg(out, v);
        }
        out.push(')');
    }

    Ok(())
}

/// Write `arg`'s `Display` output into `out`, truncated to at most
/// `max_chars` Unicode scalars. Avoids the "format then collect" double
/// allocation used by the naive `%.Ns` path.
fn write_display_truncated(out: &mut String, arg: &Value, max_chars: usize) {
    struct Truncate<'a> {
        out: &'a mut String,
        remaining: usize,
    }
    impl core::fmt::Write for Truncate<'_> {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            for ch in s.chars() {
                if self.remaining == 0 {
                    return Ok(());
                }
                self.out.push(ch);
                self.remaining -= 1;
            }
            Ok(())
        }
    }
    let mut w = Truncate {
        out,
        remaining: max_chars,
    };
    let _ = write!(w, "{}", arg);
}

/// Prepend a `+` or space sign to a formatted float at `out[start..]`
/// when the sign flag demands it and the number isn't already negative.
fn apply_float_sign_in_place(out: &mut String, start: usize, f: f64, spec: &FmtSpec) {
    // `f >= 0.0` is false for NaN, matching the semantics of the previous
    // allocating `apply_float_sign` helper (NaN never gets a synthesized sign).
    if f < 0.0 || f.is_nan() {
        return;
    }
    if matches!(out.as_bytes().get(start), Some(&b'-')) {
        return;
    }
    if spec.plus {
        out.insert(start, '+');
    } else if spec.space {
        out.insert(start, ' ');
    }
}

/// Emit Go's `%!verb(type=value)` marker for a type-mismatched or unknown verb.
///
/// Nil is a special case: Go emits `%!verb(<nil>)` without the `nil=` prefix.
fn write_bad_verb(out: &mut String, verb: char, arg: &Value) {
    out.push_str("%!");
    out.push(verb);
    out.push('(');
    write_bad_arg(out, arg);
    out.push(')');
}

/// Emit the `type=value` (or bare `<nil>`) form used inside `%!verb(...)`
/// and `%!(EXTRA …)` markers.
fn write_bad_arg(out: &mut String, arg: &Value) {
    if matches!(arg, Value::Nil) {
        out.push_str("<nil>");
        return;
    }
    let _ = write!(out, "{}=", arg.type_name());
    match arg {
        Value::String(s) => out.push_str(s),
        other => {
            let _ = write!(out, "{}", other);
        }
    }
}

/// `%x` / `%X` on strings: hex-encode each byte into `out`.
///
/// Go's precision on `%.Nx` for strings limits the number of *input bytes*
/// consumed, yielding `2 * N` hex characters, not `N`.
fn format_string_hex_into(out: &mut String, s: &str, upper: bool, spec: &FmtSpec) {
    let bytes = s.as_bytes();
    let take = spec
        .precision
        .map(|p| p.min(bytes.len()))
        .unwrap_or(bytes.len());
    out.reserve(take * 2);
    for b in &bytes[..take] {
        let _ = if upper {
            write!(out, "{:02X}", b)
        } else {
            write!(out, "{:02x}", b)
        };
    }
}

// Quoting
/// Write a Go-syntax double-quoted string literal (like `strconv.Quote`) into `out`.
///
/// Escapes backslash, double-quote, newline, tab, carriage-return, bell,
/// backspace, form-feed, vertical-tab, and all control characters.
fn go_quote_into(out: &mut String, s: &str) {
    out.reserve(s.len() + 2);
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
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Try to write a Go-syntax backtick-quoted string (for `%#q`) into `out`.
///
/// Returns `false` if the string contains backticks or non-printable
/// characters; in that case `out` is left unchanged and the caller should
/// fall back to [`go_quote_into`].
fn try_backquote_into(out: &mut String, s: &str) -> bool {
    if s.contains('`') {
        return false;
    }
    for ch in s.chars() {
        // Mirrors Go's strconv.CanBackquote: reject ASCII control chars
        // (U+0000..U+001F plus DEL) except tab, which is allowed inside
        // backticks. Non-ASCII chars pass through — Rust's &str already
        // guarantees valid UTF-8, which is the other half of Go's rule.
        if ch != '\t' && (ch < ' ' || ch == '\x7F') {
            return false;
        }
    }
    out.reserve(s.len() + 2);
    out.push('`');
    out.push_str(s);
    out.push('`');
    true
}

// Scientific notation normalization
/// Write Rust's scientific notation `raw` into `out` in Go's format.
///
/// Go's format requires:
/// - Explicit `+` sign on positive exponents.
/// - At least 2-digit exponent (`e+02`, not `e+2`).
/// - The exponent character is taken from `upper` (`'E'` if true, else `'e'`),
///   regardless of which case `raw` happens to contain. Callers that already
///   produced uppercase `raw` (e.g. `format!("{:E}", f)`) must pass
///   `upper = true` to preserve case.
/// - When `strip_zeros` is set, trailing zeros (and a dangling `.`) are
///   removed from the mantissa before emission.
///
/// Writes directly into `out`: no intermediate `String` allocation, in
/// contrast to the previous `go_normalize_sci` + `replace` + `strip` chain.
fn write_normalized_sci(out: &mut String, raw: &str, upper: bool, strip_zeros: bool) {
    let e_pos = raw.bytes().position(|b| b == b'e' || b == b'E');
    let Some(e_pos) = e_pos else {
        // No exponent: emit the mantissa-only form, optionally stripped.
        out.push_str(if strip_zeros {
            trim_trailing_zeros_view(raw)
        } else {
            raw
        });
        return;
    };
    let mut mantissa = &raw[..e_pos];
    if strip_zeros && mantissa.contains('.') {
        mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
    }
    out.push_str(mantissa);
    out.push(if upper { 'E' } else { 'e' });

    let exp_str = &raw[e_pos + 1..];
    let (sign, digits) = if let Some(d) = exp_str.strip_prefix('-') {
        ('-', d)
    } else if let Some(d) = exp_str.strip_prefix('+') {
        ('+', d)
    } else {
        ('+', exp_str)
    };
    out.push(sign);
    if digits.len() < 2 {
        out.push('0');
    }
    out.push_str(digits);
}

/// Borrowing view of `s` with trailing zeros (and a dangling `.`) stripped.
/// Zero-allocation alternative to the old `strip_trailing_zeros` String
/// helper.
fn trim_trailing_zeros_view(s: &str) -> &str {
    if !s.contains('.') {
        return s;
    }
    s.trim_end_matches('0').trim_end_matches('.')
}

// %g formatting
/// Decimal exponent of `f` as reported by its shortest scientific form.
///
/// Avoids the `log10().floor()` round-off that misclassifies values right at
/// a power-of-ten boundary (e.g. `999999.999…` rounding up to `1e6`).
fn decimal_exp(f: f64) -> i32 {
    let s = format!("{:e}", f.abs());
    s.find('e')
        .and_then(|i| s[i + 1..].parse::<i32>().ok())
        .unwrap_or(0)
}

/// Write a float into `out` using Go's `%g` default precision (shortest
/// representation). Uses `%e` notation when the exponent is < −4 or ≥ 6,
/// matching Go's `strconv.FormatFloat(f, 'g', -1, 64)`.
fn write_g_default(out: &mut String, f: f64, upper: bool) {
    if f == 0.0 {
        out.push_str(if f.is_sign_negative() { "-0" } else { "0" });
        return;
    }
    let exp = decimal_exp(f);
    if !(-4..6).contains(&exp) {
        let raw = format!("{:e}", f);
        write_normalized_sci(out, &raw, upper, false);
    } else {
        let _ = write!(out, "{}", f);
    }
}

/// Write a float into `out` using Go's `%g` with an explicit precision
/// (significant digits). Uses `%e` notation when the exponent is < −4 or
/// ≥ `prec`, then strips trailing zeros.
fn write_g_with_precision(out: &mut String, f: f64, prec: usize, upper: bool) {
    if f == 0.0 {
        out.push_str(if f.is_sign_negative() { "-0" } else { "0" });
        return;
    }
    let exp = decimal_exp(f);
    if exp < -4 || exp >= prec as i32 {
        let e_prec = prec.saturating_sub(1);
        let raw = format!("{:.prec$e}", f, prec = e_prec);
        write_normalized_sci(out, &raw, upper, true);
    } else {
        let f_prec = if prec as i32 > exp + 1 {
            (prec as i32 - exp - 1) as usize
        } else {
            0
        };
        let raw = format!("{:.prec$}", f, prec = f_prec);
        out.push_str(trim_trailing_zeros_view(&raw));
    }
}

// Integer base formatting
/// Write a signed integer in a non-decimal base into `out`, using Go's
/// conventions (sign separate from magnitude; `#` inserts `0x`/`0X`/`0b`/`0`).
///
/// Zero-padding is applied to the digits themselves — matching Go, whose
/// integer formatter builds the digit string, pads it up to
/// `width − sign_len`, then prepends the `#` prefix. As a result `%#08x` of
/// 255 yields `0x000000ff` (the `0x` sits outside the padded digit count),
/// and `%#08o` of 255 yields `00000377` (Go's formatter omits the bare-`0`
/// octal prefix when the padded digits already start with `0`).
fn format_int_base_into(out: &mut String, n: i64, base: char, spec: &FmtSpec) {
    let abs = n.unsigned_abs();

    let sign = if n < 0 {
        Some('-')
    } else if spec.plus {
        Some('+')
    } else if spec.space {
        Some(' ')
    } else {
        None
    };

    let mut digits = String::new();
    let _ = match base {
        'x' => write!(digits, "{:x}", abs),
        'X' => write!(digits, "{:X}", abs),
        'o' => write!(digits, "{:o}", abs),
        'b' => write!(digits, "{:b}", abs),
        #[allow(
            clippy::unreachable,
            reason = "private helper; callers only pass 'x', 'X', 'o', 'b'"
        )]
        _ => unreachable!(),
    };

    // Go's rule: `0` flag without `-` and without explicit precision turns
    // into "pad digits up to width − sign_len". An explicit precision wins
    // over the zero flag.
    let digit_target = match spec.precision {
        Some(p) => p,
        None if spec.zero && !spec.left_align => spec
            .width
            .map(|w| w.saturating_sub(sign.is_some() as usize))
            .unwrap_or(0),
        None => 0,
    };
    if digits.len() < digit_target {
        let pad = digit_target - digits.len();
        let mut padded = String::with_capacity(digit_target);
        for _ in 0..pad {
            padded.push('0');
        }
        padded.push_str(&digits);
        digits = padded;
    }

    if let Some(s) = sign {
        out.push(s);
    }
    if spec.hash {
        match base {
            'x' => out.push_str("0x"),
            'X' => out.push_str("0X"),
            'b' => out.push_str("0b"),
            'o' => {
                // Go skips the `0` octal prefix when the first digit is
                // already `0` — which happens naturally after zero-padding.
                if !digits.starts_with('0') {
                    out.push('0');
                }
            }
            #[allow(
                clippy::unreachable,
                reason = "private helper; callers only pass 'x', 'X', 'o', 'b'"
            )]
            _ => unreachable!(),
        }
    }
    out.push_str(&digits);
}

// Escaping functions
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
/// angle brackets (`<`, `>`), ampersand, equals sign, all control
/// characters below U+0020, and the JSON-in-HTML-hostile line/paragraph
/// separators U+2028 / U+2029 — all as `\uXXXX`.
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
            // U+2028 LINE SEPARATOR / U+2029 PARAGRAPH SEPARATOR — valid
            // in JSON but illegal mid-string in JavaScript pre-ES2019.
            // Go escapes both to keep `<script>` payloads safe.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            _ if (ch as u32) < 0x20 => {
                write!(out, "\\u{:04X}", ch as u32).ok();
            }
            _ => out.push(ch),
        }
    }
    out
}

/// Percent-encode a string for use in URL query parameters (form-encoding).
///
/// Matches Go's `template.URLQueryEscaper` / `url.QueryEscape`: unreserved
/// characters (`A-Z`, `a-z`, `0-9`, `-`, `_`, `.`, `~`) pass through, space
/// becomes `+`, and everything else is `%XX`-encoded.
pub fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => {
                write!(out, "%{:02X}", byte).ok();
            }
        }
    }
    out
}

// Hex float parsing
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
        let frac_bits = frac_part.len() as i32 * 4;
        int_val as f64 + frac_val as f64 / (2f64).powi(frac_bits)
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
    use alloc::string::ToString;

    // Helper: run sprintf with Value args and return the String.
    fn sf(fmt: &str, args: &[Value]) -> String {
        sprintf(fmt, args).unwrap()
    }

    // Test wrappers for helpers that write into `&mut String` in production.
    // Kept here (not in the main module) so production code stays free of
    // test-only scaffolding.

    impl FmtSpec {
        fn pad(&self, s: &str, is_numeric: bool) -> String {
            let mut out = String::from(s);
            self.pad_in_place(&mut out, 0, is_numeric);
            out
        }

        fn format_signed(&self, n: i64) -> String {
            let mut out = String::new();
            self.write_signed(&mut out, n);
            out
        }
    }

    fn go_quote(s: &str) -> String {
        let mut out = String::new();
        go_quote_into(&mut out, s);
        out
    }

    fn go_backquote(s: &str) -> Option<String> {
        let mut out = String::new();
        if try_backquote_into(&mut out, s) {
            Some(out)
        } else {
            None
        }
    }

    // `base` is a single-char string (`"x"`, `"X"`, `"o"`, `"b"`) so the
    // existing test call-sites compile unchanged.
    fn format_int_base(n: i64, base: &str, spec: &FmtSpec) -> String {
        let mut out = String::new();
        let ch = base.chars().next().unwrap_or('x');
        format_int_base_into(&mut out, n, ch, spec);
        out
    }

    // String-returning wrappers for the sci-notation and `%g` helpers, kept
    // so the existing test call-sites compile after production switched to
    // `&mut String` writers. They are *not* used in production code.

    fn go_normalize_sci(s: &str) -> String {
        let mut out = String::new();
        // Preserve raw case: pass `upper = true` only if the raw `s` already
        // uses 'E'. The old `go_normalize_sci` was case-preserving.
        let upper = s.bytes().any(|b| b == b'E');
        write_normalized_sci(&mut out, s, upper, false);
        out
    }

    fn strip_trailing_zeros(s: &str) -> String {
        trim_trailing_zeros_view(s).to_string()
    }

    fn strip_trailing_zeros_sci(s: &str) -> String {
        // Old behavior: trim zeros from mantissa, leave exponent verbatim.
        if let Some(e_pos) = s.bytes().position(|b| b == b'e' || b == b'E') {
            let mantissa = trim_trailing_zeros_view(&s[..e_pos]);
            let mut out = String::with_capacity(s.len());
            out.push_str(mantissa);
            out.push_str(&s[e_pos..]);
            out
        } else {
            trim_trailing_zeros_view(s).to_string()
        }
    }

    fn format_g_default(f: f64, upper: bool) -> String {
        let mut out = String::new();
        write_g_default(&mut out, f, upper);
        out
    }

    fn format_g_with_precision(f: f64, prec: usize, upper: bool) -> String {
        let mut out = String::new();
        write_g_with_precision(&mut out, f, prec, upper);
        out
    }

    // needs_space
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

    // go_quote
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

    // go_backquote
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

    // go_normalize_sci
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

    // strip_trailing_zeros
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

    // strip_trailing_zeros_sci
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

    // format_g_default
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

    // format_g_with_precision
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

    // format_int_base
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

    // html_escape
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

    // js_escape
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

    // url_encode
    #[test]
    fn url_basic() {
        // Form-encoding: space → '+', not '%20'.
        assert_eq!(url_encode("hello world"), "hello+world");
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

    // parse_hex_float
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

    // sprintf: %s
    #[test]
    fn sprintf_s_basic() {
        assert_eq!(sf("%s", &[Value::String("hello".into())]), "hello");
    }

    #[test]
    fn sprintf_s_non_string() {
        // Go's `%s` rejects non-string args, emitting `%!s(type=value)`.
        assert_eq!(sf("%s", &[Value::Int(42)]), "%!s(int=42)");
        assert_eq!(sf("%s", &[Value::Bool(true)]), "%!s(bool=true)");
        // Nil: bare `<nil>` (no `nil=` prefix), matching Go.
        assert_eq!(sf("%s", &[Value::Nil]), "%!s(<nil>)");
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

    #[test]
    fn sprintf_s_precision_multibyte() {
        // Precision counts Unicode scalars, not bytes. Each accented char is
        // 2 bytes in UTF-8, so `%.3s` keeps the first 3 chars.
        assert_eq!(sf("%.3s", &[Value::String("café".into())]), "caf");
        assert_eq!(sf("%.4s", &[Value::String("café".into())]), "café");
    }

    #[test]
    fn sprintf_s_left_align_with_truncation() {
        // Truncate first to 3 chars, then left-align in width 6.
        assert_eq!(sf("%-6.3s", &[Value::String("hello".into())]), "hel   ");
    }

    #[test]
    fn sprintf_s_zero_precision() {
        assert_eq!(sf("%.0s", &[Value::String("hello".into())]), "");
    }

    // sprintf: %d
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

    #[test]
    fn sprintf_d_zero_pad_plus_negative() {
        // The plus flag is shadowed by the leading minus on negatives.
        assert_eq!(sf("%+06d", &[Value::Int(-42)]), "-00042");
    }

    #[test]
    fn sprintf_d_left_align_overrides_zero() {
        // Go: '-' wins over '0', so pad with spaces on the right.
        assert_eq!(sf("%-06d", &[Value::Int(42)]), "42    ");
    }

    #[test]
    fn sprintf_d_zero_pad_no_overpad() {
        // Width <= already-written length: no padding inserted.
        assert_eq!(sf("%02d", &[Value::Int(12345)]), "12345");
    }

    // sprintf: %f
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

    // sprintf: %e / %E
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

    #[test]
    fn sprintf_e_space_flag() {
        assert_eq!(sf("% e", &[Value::Float(1.5)]), " 1.500000e+00");
        assert_eq!(sf("% e", &[Value::Float(-1.5)]), "-1.500000e+00");
    }

    #[test]
    fn sprintf_e_width() {
        // Right-align in 16 chars; "1.500000e+00" is 12 → 4 leading spaces.
        assert_eq!(sf("%16e", &[Value::Float(1.5)]), "    1.500000e+00");
    }

    #[test]
    fn sprintf_e_plus_width_negative() {
        // The plus flag does not synthesize a sign on negatives; width pads
        // the whole number (sign included) on the left.
        assert_eq!(sf("%+16e", &[Value::Float(-1.5)]), "   -1.500000e+00");
    }

    // sprintf: %g / %G
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

    #[test]
    fn sprintf_g_nan_no_sign_synthesis() {
        // NaN must never get a `+` or space prepended, even with sign flags.
        assert_eq!(sf("%g", &[Value::Float(f64::NAN)]), "NaN");
        assert_eq!(sf("%+g", &[Value::Float(f64::NAN)]), "NaN");
        assert_eq!(sf("% g", &[Value::Float(f64::NAN)]), "NaN");
    }

    #[test]
    fn sprintf_g_inf_with_sign_flags() {
        // The crate currently passes through Rust's Display for inf (lowercase
        // "inf", no synthesized sign). Sign flags still prepend on the
        // positive case because the leading byte is 'i', not '-'.
        assert_eq!(sf("%g", &[Value::Float(f64::INFINITY)]), "inf");
        assert_eq!(sf("%g", &[Value::Float(f64::NEG_INFINITY)]), "-inf");
        assert_eq!(sf("%+g", &[Value::Float(f64::INFINITY)]), "+inf");
    }

    #[test]
    fn sprintf_g_width_and_sci() {
        // Width pads the entire formatted number, sci notation included.
        assert_eq!(sf("%10g", &[Value::Float(1e7)]), "     1e+07");
    }

    #[test]
    fn sprintf_f_nan() {
        assert_eq!(sf("%f", &[Value::Float(f64::NAN)]), "NaN");
        // Sign flags do NOT prepend on NaN.
        assert_eq!(sf("%+f", &[Value::Float(f64::NAN)]), "NaN");
    }

    #[test]
    fn sprintf_f_inf() {
        assert_eq!(sf("%f", &[Value::Float(f64::INFINITY)]), "inf");
        assert_eq!(sf("%f", &[Value::Float(f64::NEG_INFINITY)]), "-inf");
    }

    // sprintf: %v
    #[test]
    fn sprintf_v() {
        assert_eq!(sf("%v", &[Value::Int(42)]), "42");
        assert_eq!(sf("%v", &[Value::String("hi".into())]), "hi");
        assert_eq!(sf("%v", &[Value::Bool(true)]), "true");
        assert_eq!(sf("%v", &[Value::Nil]), "<nil>");
    }

    #[test]
    fn sprintf_v_width() {
        assert_eq!(sf("%6v", &[Value::Int(42)]), "    42");
        assert_eq!(sf("%-6v", &[Value::String("hi".into())]), "hi    ");
    }

    // sprintf: %q
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

    #[test]
    fn sprintf_q_width() {
        // `"hi"` is 4 chars wide; right-align in width 8 → 4 leading spaces.
        assert_eq!(sf("%8q", &[Value::String("hi".into())]), r#"    "hi""#);
        assert_eq!(sf("%-8q", &[Value::String("hi".into())]), r#""hi"    "#);
    }

    #[test]
    fn sprintf_hash_q_width() {
        // Backtick form: `hi` is 4 chars, pad to width 8.
        assert_eq!(sf("%#8q", &[Value::String("hi".into())]), "    `hi`");
    }

    // sprintf: %t
    #[test]
    fn sprintf_t() {
        assert_eq!(sf("%t", &[Value::Bool(true)]), "true");
        assert_eq!(sf("%t", &[Value::Bool(false)]), "false");
    }

    #[test]
    fn sprintf_t_non_bool_emits_bad_verb() {
        assert_eq!(sf("%t", &[Value::Int(42)]), "%!t(int=42)");
    }

    #[test]
    fn sprintf_t_width() {
        assert_eq!(sf("%6t", &[Value::Bool(true)]), "  true");
        assert_eq!(sf("%-6t", &[Value::Bool(true)]), "true  ");
    }

    // sprintf: %x / %X / %o / %b
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
    fn sprintf_x_left_align() {
        assert_eq!(sf("%-6x", &[Value::Int(255)]), "ff    ");
    }

    #[test]
    fn sprintf_x_hash_zero_pad_negative() {
        // `%#08x` pads the *digit* portion up to width − sign_len (so 7 digits
        // for a negative value), then prepends `0x` and the sign. Matches Go's
        // `fmt.Sprintf("%#08x", -255)`.
        assert_eq!(sf("%#08x", &[Value::Int(-255)]), "-0x00000ff");
    }

    #[test]
    fn sprintf_x_hash_zero_pad_positive() {
        // With `#`, the `0x` prefix is *not* counted against the zero-pad
        // width — Go pads to 8 hex digits, yielding `0x000000ff` (10 chars).
        assert_eq!(sf("%#08x", &[Value::Int(255)]), "0x000000ff");
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

    // sprintf: %c
    #[test]
    fn sprintf_c() {
        assert_eq!(sf("%c", &[Value::Int(65)]), "A");
        assert_eq!(sf("%c", &[Value::Int(0x1F600)]), "\u{1F600}"); // 😀
    }

    // sprintf: %% and mixed
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
    fn sprintf_huge_width_is_clamped() {
        // Without clamping this would try to allocate >100 GB.
        let out = sf("%999999999999d", &[Value::Int(1)]);
        assert!(out.len() <= PRINTF_MAX_LEN + 16, "got len {}", out.len());
    }

    #[test]
    fn sprintf_huge_precision_is_clamped() {
        let out = sf("%.999999999999f", &[Value::Float(1.0)]);
        assert!(out.len() <= PRINTF_MAX_LEN + 16, "got len {}", out.len());
    }

    #[test]
    fn sprintf_unknown_verb_emits_bad_verb() {
        assert_eq!(sf("%z", &[Value::Int(1)]), "%!z(int=1)");
    }

    #[test]
    fn sprintf_d_non_int_emits_bad_verb() {
        assert_eq!(sf("%d", &[Value::String("abc".into())]), "%!d(string=abc)");
    }

    #[test]
    fn sprintf_f_non_numeric_emits_bad_verb() {
        assert_eq!(sf("%f", &[Value::String("abc".into())]), "%!f(string=abc)");
    }

    // sprintf: %x / %X on strings (Go parity: precision limits input bytes)
    #[test]
    fn sprintf_x_string_full() {
        assert_eq!(sf("%x", &[Value::String("abc".into())]), "616263");
    }

    #[test]
    fn sprintf_x_string_precision_limits_input_bytes() {
        // Go: fmt.Sprintf("%.2x", "abc") == "6162" (2 bytes → 4 hex chars).
        assert_eq!(sf("%.2x", &[Value::String("abc".into())]), "6162");
        assert_eq!(sf("%.3x", &[Value::String("abc".into())]), "616263");
        assert_eq!(sf("%.0x", &[Value::String("abc".into())]), "");
    }

    #[test]
    fn sprintf_x_string_uppercase_verb() {
        assert_eq!(sf("%X", &[Value::String("abc".into())]), "616263");
    }

    #[test]
    fn sprintf_x_string_precision_over_length_is_capped() {
        // Precision larger than the string length just emits the whole string.
        assert_eq!(sf("%.99x", &[Value::String("ab".into())]), "6162");
    }

    // FmtSpec::pad
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

    // FmtSpec::format_signed
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
