//! Byte-exact ports of Go's `html/template` escaping primitives — the string
//! transforms behind the escaper functions in [`super::escapers`].
//!
//! Ported from Go's `content.go` (`stringify`), `html.go` (`htmlReplacer` +
//! replacement tables, `stripTags`, `htmlNameFilter`), `js.go` (`replace` + JS
//! tables, `jsValEscaper` with a `json.Marshal`-equivalent encoder), `css.go`
//! (`cssEscaper`, `cssValueFilter`, `decodeCSS`), and `url.go` (`urlProcessor`,
//! `isSafeURL`, `srcsetFilterAndEscaper`).
//!
//! Fidelity notes:
//! - Go operates on `string`/`[]byte` that may hold invalid UTF-8; every input
//!   here arrives as a Rust `&str` (always valid UTF-8), so the manual
//!   rune-decoding Go performs collapses to [`str::char_indices`]. The one
//!   observable consequence is that Go's "invalid byte decodes to U+FFFD" cases
//!   cannot occur; passing a literal `'\u{FFFD}'` exercises the same code path.
//! - The small byte-slice and CSS/JS lexer helpers shared with `transition.rs`
//!   (`decode_css`, `is_css_nmchar`, `index_any`, …) live in [`super::lex`].

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;

use crate::value::{SafeKind, Value};

use super::context::{AttrContentType, Context, Delim, Element, State, attr_type};
use super::lex::{
    decode_css, delim_ends, index_any, index_sub, is_css_nmchar, is_css_space, is_hex,
    is_js_ident_part,
};
use super::transition::{special_tag_end, transition};

/// The innocuous placeholder emitted in place of unsafe values (Go's
/// `filterFailsafe`, `escape.go`).
pub(crate) const FILTER_FAILSAFE: &str = "ZgotmplZ";

// ---------------------------------------------------------------------------
// content.go: stringify + the content-type lattice.
// ---------------------------------------------------------------------------

/// The type of content produced by [`stringify`] — Go's `contentType` restricted
/// to the values `stringify`/the escapers actually branch on (the seven trusted
/// kinds plus plain).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentType {
    Plain,
    Css,
    Html,
    HtmlAttr,
    Js,
    JsStr,
    Url,
    Srcset,
}

impl ContentType {
    fn from_safe(kind: SafeKind) -> ContentType {
        match kind {
            SafeKind::Html => ContentType::Html,
            SafeKind::HtmlAttr => ContentType::HtmlAttr,
            SafeKind::Js => ContentType::Js,
            SafeKind::JsStr => ContentType::JsStr,
            SafeKind::Css => ContentType::Css,
            SafeKind::Url => ContentType::Url,
            SafeKind::Srcset => ContentType::Srcset,
        }
    }
}

/// Convert escaper arguments to a string and the type of the content — port of
/// Go's `stringify` (`content.go`).
///
/// A single [`Value::Safe`] carries its trusted content type through; a single
/// plain string is plain; anything else (including a lone [`Value::Nil`], which
/// Go skips) is `fmt.Sprint`-joined as plain text.
pub(crate) fn stringify(args: &[Value]) -> (Cow<'_, str>, ContentType) {
    if args.len() == 1 {
        match &args[0] {
            Value::String(s) => return (Cow::Borrowed(s.as_ref()), ContentType::Plain),
            Value::Safe { kind, s } => {
                return (Cow::Borrowed(s.as_ref()), ContentType::from_safe(*kind));
            }
            _ => {}
        }
    }
    (Cow::Owned(sprint_skipping_nil(args)), ContentType::Plain)
}

/// `fmt.Sprint` over the arguments, skipping untyped `nil` (Go issue 25875): the
/// exact join `stringify` performs for the multi-argument / non-string case.
fn sprint_skipping_nil(args: &[Value]) -> String {
    let mut out = String::new();
    let mut prev: Option<&Value> = None;
    for arg in args {
        if matches!(arg, Value::Nil) {
            continue;
        }
        if let Some(p) = prev
            && crate::go::needs_space(p, arg)
        {
            out.push(' ');
        }
        let _ = write!(out, "{arg}");
        prev = Some(arg);
    }
    out
}

// ---------------------------------------------------------------------------
// html.go: htmlReplacer + tables.
// ---------------------------------------------------------------------------

/// The four replacement tables consulted by [`html_replacer`], corresponding to
/// Go's `htmlReplacementTable`, `htmlNormReplacementTable`,
/// `htmlNospaceReplacementTable`, and `htmlNospaceNormReplacementTable`. The
/// `*Norm` variants drop the `&` entry to avoid over-encoding existing entities.
#[derive(Clone, Copy)]
pub(crate) enum HtmlTable {
    Html,
    HtmlNorm,
    Nospace,
    NospaceNorm,
}

impl HtmlTable {
    fn lookup(self, r: u32) -> Option<&'static str> {
        match self {
            HtmlTable::Html => html_table(r),
            HtmlTable::HtmlNorm => html_norm_table(r),
            HtmlTable::Nospace => nospace_table(r),
            HtmlTable::NospaceNorm => nospace_norm_table(r),
        }
    }
}

fn html_table(r: u32) -> Option<&'static str> {
    Some(match r {
        0x00 => "\u{FFFD}",
        0x22 => "&#34;",
        0x26 => "&amp;",
        0x27 => "&#39;",
        0x2b => "&#43;",
        0x3c => "&lt;",
        0x3e => "&gt;",
        _ => return None,
    })
}

fn html_norm_table(r: u32) -> Option<&'static str> {
    Some(match r {
        0x00 => "\u{FFFD}",
        0x22 => "&#34;",
        0x27 => "&#39;",
        0x2b => "&#43;",
        0x3c => "&lt;",
        0x3e => "&gt;",
        _ => return None,
    })
}

fn nospace_table(r: u32) -> Option<&'static str> {
    Some(match r {
        0x00 => "&#xfffd;",
        0x09 => "&#9;",
        0x0a => "&#10;",
        0x0b => "&#11;",
        0x0c => "&#12;",
        0x0d => "&#13;",
        0x20 => "&#32;",
        0x22 => "&#34;",
        0x26 => "&amp;",
        0x27 => "&#39;",
        0x2b => "&#43;",
        0x3c => "&lt;",
        0x3d => "&#61;",
        0x3e => "&gt;",
        0x60 => "&#96;",
        _ => return None,
    })
}

fn nospace_norm_table(r: u32) -> Option<&'static str> {
    Some(match r {
        0x00 => "&#xfffd;",
        0x09 => "&#9;",
        0x0a => "&#10;",
        0x0b => "&#11;",
        0x0c => "&#12;",
        0x0d => "&#13;",
        0x20 => "&#32;",
        0x22 => "&#34;",
        0x27 => "&#39;",
        0x2b => "&#43;",
        0x3c => "&lt;",
        0x3d => "&#61;",
        0x3e => "&gt;",
        0x60 => "&#96;",
        _ => return None,
    })
}

/// Runes IE rejects in unquoted attributes: `[U+FDD0, U+FDEF]` and
/// `[U+FFF0, U+FFFF]`. When `badRunes` is false these are escaped to `&#x…;`.
fn is_bad_rune(r: u32) -> bool {
    (0xfdd0..=0xfdef).contains(&r) || (0xfff0..=0xffff).contains(&r)
}

/// Return `s` with runes replaced per `table`; when `bad_runes` is true the
/// IE-hostile ranges are passed through unescaped instead of `&#x…;`-encoded.
/// Byte-exact port of Go's `htmlReplacer`.
pub(crate) fn html_replacer(s: &str, table: HtmlTable, bad_runes: bool) -> Cow<'_, str> {
    let mut b = String::new();
    let mut written = 0usize;
    for (i, ch) in s.char_indices() {
        let r = ch as u32;
        if let Some(repl) = table.lookup(r) {
            if written == 0 {
                b.reserve(s.len());
            }
            b.push_str(&s[written..i]);
            b.push_str(repl);
            written = i + ch.len_utf8();
        } else if !bad_runes && is_bad_rune(r) {
            if written == 0 {
                b.reserve(s.len());
            }
            b.push_str(&s[written..i]);
            let _ = write!(b, "&#x{r:x};");
            written = i + ch.len_utf8();
        }
    }
    if written == 0 {
        return Cow::Borrowed(s);
    }
    b.push_str(&s[written..]);
    Cow::Owned(b)
}

// ---------------------------------------------------------------------------
// html.go: stripTags (real, driven by the P3 transition machine).
// ---------------------------------------------------------------------------

/// Take a snippet of HTML and return only its text content — e.g.
/// `<b>&iexcl;Hi!</b> <script>...</script>` → `&iexcl;Hi! `. A faithful port of
/// Go's `stripTags`, driven by the P3 context-transition machine
/// ([`transition`]/[`special_tag_end`]) so that `<div title="1>2">` and
/// `I <3 Ponies!` are not mangled.
pub(crate) fn strip_tags(html: &str) -> Cow<'_, str> {
    let s = html.as_bytes();
    let mut b = String::new();
    let mut c = Context::default();
    let mut i = 0usize;
    let mut all_text = true;
    while i != s.len() {
        if c.delim == Delim::None {
            // Use RCDATA instead of descending into JS or CSS bodies.
            let use_rcdata = c.element != Element::None && !c.state.is_in_tag();
            let (d, nread) = if use_rcdata {
                special_tag_end(c.clone(), &s[i..])
            } else {
                transition(c.clone(), &s[i..])
            };
            let i1 = i + nread;
            if c.state == State::Text || c.state == State::Rcdata {
                // Emit text up to the start of the tag or comment.
                let mut j = i1;
                if d.state != c.state {
                    let mut j1 = i1;
                    while j1 > i {
                        j1 -= 1;
                        if s[j1] == b'<' {
                            j = j1;
                            break;
                        }
                    }
                }
                b.push_str(&html[i..j]);
            } else {
                all_text = false;
            }
            c = d;
            i = i1;
            continue;
        }
        // Inside an attribute value: skip to the delimiter.
        let Some(rel) = index_any(&s[i..], delim_ends(c.delim)) else {
            break;
        };
        let mut i1 = i + rel;
        if c.delim != Delim::SpaceOrTagEnd {
            // Consume the quote.
            i1 += 1;
        }
        c = Context {
            state: State::Tag,
            element: c.element,
            ..Context::default()
        };
        i = i1;
    }
    if all_text {
        return Cow::Borrowed(html);
    }
    if c.state == State::Text || c.state == State::Rcdata {
        b.push_str(&html[i..]);
    }
    Cow::Owned(b)
}

// ---------------------------------------------------------------------------
// html.go: htmlNameFilter.
// ---------------------------------------------------------------------------

/// Accept only valid parts of an HTML attribute/tag name, or a known-safe
/// attribute. Port of Go's `htmlNameFilter`.
pub(crate) fn html_name_filter(s: &str, t: ContentType) -> Cow<'_, str> {
    if t == ContentType::HtmlAttr {
        return Cow::Borrowed(s);
    }
    if s.is_empty() {
        // Avoid violating structure preservation, e.g.
        // `<input checked {{.K}}={{.V}}>`.
        return Cow::Borrowed(FILTER_FAILSAFE);
    }
    let lower = s.to_ascii_lowercase();
    if attr_type(&lower) != AttrContentType::Plain {
        return Cow::Borrowed(FILTER_FAILSAFE);
    }
    for r in lower.chars() {
        if !(r.is_ascii_digit() || r.is_ascii_lowercase()) {
            return Cow::Borrowed(FILTER_FAILSAFE);
        }
    }
    Cow::Owned(lower)
}

// ---------------------------------------------------------------------------
// js.go: replace + JS replacement tables.
// ---------------------------------------------------------------------------

/// Go's `lowUnicodeReplacementTable`: every codepoint in `[0, 0x20)` maps to a
/// non-empty JS escape, so it always wins over the per-context table there.
const LOW_UNICODE: [&str; 0x20] = [
    "\\u0000", "\\u0001", "\\u0002", "\\u0003", "\\u0004", "\\u0005", "\\u0006", "\\u0007",
    "\\u0008", "\\t", "\\n", "\\u000b", "\\f", "\\r", "\\u000e", "\\u000f", "\\u0010", "\\u0011",
    "\\u0012", "\\u0013", "\\u0014", "\\u0015", "\\u0016", "\\u0017", "\\u0018", "\\u0019",
    "\\u001a", "\\u001b", "\\u001c", "\\u001d", "\\u001e", "\\u001f",
];

/// Selects one of the per-context JS replacement tables for [`js_replace`].
#[derive(Clone, Copy)]
pub(crate) enum JsTable {
    Str,
    StrNorm,
    BqStr,
    Regexp,
}

impl JsTable {
    fn lookup(self, r: u32) -> Option<&'static str> {
        match self {
            JsTable::Str => js_str_table(r),
            JsTable::StrNorm => js_str_norm_table(r),
            JsTable::BqStr => js_bq_str_table(r),
            JsTable::Regexp => js_regexp_table(r),
        }
    }
}

fn js_str_table(r: u32) -> Option<&'static str> {
    Some(match r {
        0x22 => "\\u0022",
        0x26 => "\\u0026",
        0x27 => "\\u0027",
        0x2b => "\\u002b",
        0x2f => "\\/",
        0x3c => "\\u003c",
        0x3e => "\\u003e",
        0x5c => "\\\\",
        0x60 => "\\u0060",
        _ => return None,
    })
}

fn js_str_norm_table(r: u32) -> Option<&'static str> {
    // Like js_str_table but without the `\` entry (does not over-encode escapes).
    Some(match r {
        0x22 => "\\u0022",
        0x26 => "\\u0026",
        0x27 => "\\u0027",
        0x2b => "\\u002b",
        0x2f => "\\/",
        0x3c => "\\u003c",
        0x3e => "\\u003e",
        0x60 => "\\u0060",
        _ => return None,
    })
}

fn js_bq_str_table(r: u32) -> Option<&'static str> {
    // Like js_str_table plus the JS template-literal specials `$`, `{`, `}`.
    Some(match r {
        0x22 => "\\u0022",
        0x24 => "\\u0024",
        0x26 => "\\u0026",
        0x27 => "\\u0027",
        0x2b => "\\u002b",
        0x2f => "\\/",
        0x3c => "\\u003c",
        0x3e => "\\u003e",
        0x5c => "\\\\",
        0x60 => "\\u0060",
        0x7b => "\\u007b",
        0x7d => "\\u007d",
        _ => return None,
    })
}

fn js_regexp_table(r: u32) -> Option<&'static str> {
    Some(match r {
        0x22 => "\\u0022",
        0x24 => "\\$",
        0x26 => "\\u0026",
        0x27 => "\\u0027",
        0x28 => "\\(",
        0x29 => "\\)",
        0x2a => "\\*",
        0x2b => "\\u002b",
        0x2d => "\\-",
        0x2e => "\\.",
        0x2f => "\\/",
        0x3c => "\\u003c",
        0x3e => "\\u003e",
        0x3f => "\\?",
        0x5b => "\\[",
        0x5c => "\\\\",
        0x5d => "\\]",
        0x5e => "\\^",
        0x7b => "\\{",
        0x7c => "\\|",
        0x7d => "\\}",
        _ => return None,
    })
}

/// Replace each rune of `s` per the low-Unicode table (checked first), then the
/// per-context `table`, then the JSON-in-HTML-hostile U+2028/U+2029. Byte-exact
/// port of Go's `replace`.
pub(crate) fn js_replace(s: &str, table: JsTable) -> Cow<'_, str> {
    let mut b = String::new();
    let mut written = 0usize;
    for (i, ch) in s.char_indices() {
        let r = ch as u32;
        let repl: Option<&str> = if r < 0x20 {
            Some(LOW_UNICODE[r as usize])
        } else if let Some(rep) = table.lookup(r) {
            Some(rep)
        } else if r == 0x2028 {
            Some("\\u2028")
        } else if r == 0x2029 {
            Some("\\u2029")
        } else {
            None
        };
        if let Some(rep) = repl {
            if written == 0 {
                b.reserve(s.len());
            }
            b.push_str(&s[written..i]);
            b.push_str(rep);
            written = i + ch.len_utf8();
        }
    }
    if written == 0 {
        return Cow::Borrowed(s);
    }
    b.push_str(&s[written..]);
    Cow::Owned(b)
}

// ---------------------------------------------------------------------------
// js.go: jsValEscaper + the encoding/json-equivalent marshaler.
// ---------------------------------------------------------------------------

/// Escape inputs to a side-effect-free JS Expression. Port of Go's
/// `jsValEscaper`: `JS`-typed content passes through verbatim, `JSStr`-typed
/// content is wrapped in quotes, and everything else is JSON-encoded (see
/// [`json_marshal`]) and padded so identifiers/numbers cannot run into adjacent
/// tokens.
pub(crate) fn js_val_escaper(args: &[Value]) -> String {
    if args.len() == 1 {
        match &args[0] {
            Value::Safe {
                kind: SafeKind::Js,
                s,
            } => return String::from(s.as_ref()),
            Value::Safe {
                kind: SafeKind::JsStr,
                s,
            } => return format!("\"{s}\""),
            other => return finish_js_val(json_marshal(other)),
        }
    }
    // Multiple (or zero) arguments: fmt.Sprint then JSON-encode the string.
    let joined = crate::go::sprint(args);
    let mut b = String::new();
    json_quote(&joined, &mut b);
    finish_js_val(b)
}

/// The tail of `jsValEscaper`: empty → `" null "`, else surround with spaces
/// when the first/last byte is an identifier part and re-escape any raw
/// U+2028/U+2029 (a defensive no-op, since [`json_marshal`] already escapes
/// them).
fn finish_js_val(b: String) -> String {
    if b.is_empty() {
        return String::from(" null ");
    }
    let bytes = b.as_bytes();
    let pad = is_js_ident_part(bytes[0]) || is_js_ident_part(bytes[bytes.len() - 1]);
    let has_sep = b.contains(['\u{2028}', '\u{2029}']);
    if !pad && !has_sep {
        return b;
    }
    let mut out = String::with_capacity(b.len() + 2);
    if pad {
        out.push(' ');
    }
    for ch in b.chars() {
        match ch {
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    if pad {
        out.push(' ');
    }
    out
}

/// Encode a [`Value`] the way Go's `encoding/json` (with its default HTML
/// escaping) would. Strings are JSON-quoted with `<`/`>`/`&` and U+2028/U+2029
/// escaped; a [`Value::Function`] cannot be marshaled by Go and becomes `null`.
pub(crate) fn json_marshal(v: &Value) -> String {
    let mut out = String::new();
    json_encode(v, &mut out);
    out
}

fn json_encode(v: &Value, out: &mut String) {
    match v {
        Value::Nil => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(n) => {
            let _ = write!(out, "{n}");
        }
        Value::Uint(n) => {
            let _ = write!(out, "{n}");
        }
        Value::Float(f) => out.push_str(&json_float(*f)),
        Value::String(s) => json_quote(s, out),
        Value::Safe { s, .. } => json_quote(s, out),
        Value::List(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_encode(item, out);
            }
            out.push(']');
        }
        Value::Map(m) => {
            out.push('{');
            for (i, (k, val)) in m.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_quote(k, out);
                out.push(':');
                json_encode(val, out);
            }
            out.push('}');
        }
        // Go's json.Marshal cannot encode a func; it returns an error. We emit
        // `null` so the surrounding script stays syntactically valid.
        Value::Function(_) => out.push_str("null"),
    }
}

/// JSON-quote `s` with encoding/json's default HTML escaping.
fn json_quote(s: &str, out: &mut String) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => out.push_str("\\u003c"),
            '>' => out.push_str("\\u003e"),
            '&' => out.push_str("\\u0026"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Format `f` the way Go's `encoding/json` does: decimal ('f') shortest form
/// for `|f|` in `[1e-6, 1e21)` (and zero), otherwise exponential ('e') shortest
/// form with Go's exponent conventions.
///
/// CAVEAT: non-finite floats (`NaN`, `±Inf`) make Go's `json.Marshal` fail; we
/// emit `null` instead of Go's error-comment path. The exponential branch is a
/// best-effort match of Go's `strconv` output for typical magnitudes.
fn json_float(f: f64) -> String {
    if !f.is_finite() {
        return String::from("null");
    }
    let abs = f.abs();
    if abs != 0.0 && !(1e-6..1e21).contains(&abs) {
        let mut s = format!("{f:e}");
        // Rust omits the `+` on positive exponents; Go's 'e' form includes it.
        if let Some(e) = s.find('e')
            && !s[e + 1..].starts_with('-')
        {
            s.insert(e + 1, '+');
        }
        s
    } else {
        // Rust's `Display` for f64 is the shortest round-tripping decimal, and
        // renders integers without a trailing `.0` and negative zero as `-0`,
        // matching Go's `strconv.FormatFloat(f, 'f', -1, 64)`.
        format!("{f}")
    }
}

// ---------------------------------------------------------------------------
// css.go: cssEscaper, cssValueFilter, decodeCSS + helpers.
// ---------------------------------------------------------------------------

fn css_table(r: u32) -> Option<&'static str> {
    Some(match r {
        0x00 => "\\0",
        0x09 => "\\9",
        0x0a => "\\a",
        0x0c => "\\c",
        0x0d => "\\d",
        0x22 => "\\22",
        0x26 => "\\26",
        0x27 => "\\27",
        0x28 => "\\28",
        0x29 => "\\29",
        0x2b => "\\2b",
        0x2f => "\\2f",
        0x3a => "\\3a",
        0x3b => "\\3b",
        0x3c => "\\3c",
        0x3e => "\\3e",
        0x5c => "\\\\",
        0x7b => "\\7b",
        0x7d => "\\7d",
        _ => return None,
    })
}

/// Escape HTML and CSS specials using `\<hex>+` escapes. Byte-exact port of Go's
/// `cssEscaper`, including the trailing space appended after a hex escape when
/// the next byte would otherwise extend it.
pub(crate) fn css_escaper(s: &str) -> Cow<'_, str> {
    let bytes = s.as_bytes();
    let mut b = String::new();
    let mut written = 0usize;
    for (i, ch) in s.char_indices() {
        let Some(repl) = css_table(ch as u32) else {
            continue;
        };
        if written == 0 {
            b.reserve(s.len());
        }
        b.push_str(&s[written..i]);
        b.push_str(repl);
        written = i + ch.len_utf8();
        if repl != "\\\\"
            && (written == bytes.len() || is_hex(bytes[written]) || is_css_space(bytes[written]))
        {
            b.push(' ');
        }
    }
    if written == 0 {
        return Cow::Borrowed(s);
    }
    b.push_str(&s[written..]);
    Cow::Owned(b)
}

/// Allow innocuous CSS values (quantities, colors, keywords) and defang unsafe
/// ones. Port of Go's `cssValueFilter`. The returned value is the CSS3-decoded
/// form of the input (Go returns `string(decodeCSS(s))`).
pub(crate) fn css_value_filter(s: &str, t: ContentType) -> Cow<'_, str> {
    if t == ContentType::Css {
        return Cow::Borrowed(s);
    }
    let decoded = decode_css(s.as_bytes());
    let mut id: Vec<u8> = Vec::new();
    {
        let b = decoded.as_ref();
        for (i, &c) in b.iter().enumerate() {
            match c {
                0 | b'"' | b'\'' | b'(' | b')' | b'/' | b';' | b'@' | b'[' | b'\\' | b']'
                | b'`' | b'{' | b'}' | b'<' | b'>' => {
                    return Cow::Borrowed(FILTER_FAILSAFE);
                }
                b'-' => {
                    // Disallow `<!--`, `-->`, and `--` in identifiers.
                    if i != 0 && b[i - 1] == b'-' {
                        return Cow::Borrowed(FILTER_FAILSAFE);
                    }
                }
                _ => {
                    if c < 0x80 && is_css_nmchar(c as char) {
                        id.push(c);
                    }
                }
            }
        }
        id.make_ascii_lowercase();
        if index_sub(&id, b"expression").is_some() || index_sub(&id, b"mozbinding").is_some() {
            return Cow::Borrowed(FILTER_FAILSAFE);
        }
    }
    match decoded {
        Cow::Borrowed(_) => Cow::Borrowed(s),
        Cow::Owned(v) => Cow::Owned(String::from_utf8_lossy(&v).into_owned()),
    }
}

// ---------------------------------------------------------------------------
// url.go: urlProcessor, isSafeURL, srcsetFilterAndEscaper + helpers.
// ---------------------------------------------------------------------------

/// Append to `b` a normalized (or escaped, when `!norm`) URL corresponding to
/// `s`, reporting whether the appended content differs from `s`. Byte-exact port
/// of Go's `processURLOnto`.
fn process_url_onto(s: &str, norm: bool, b: &mut String) -> bool {
    let bytes = s.as_bytes();
    let n = bytes.len();
    b.reserve(n + 16);
    let mut written = 0usize;
    let mut i = 0usize;
    while i < n {
        let c = bytes[i];
        let keep = match c {
            b'!' | b'#' | b'$' | b'&' | b'*' | b'+' | b',' | b'/' | b':' | b';' | b'=' | b'?'
            | b'@' | b'[' | b']' => norm,
            b'-' | b'.' | b'_' | b'~' => true,
            b'%' => norm && i + 2 < n && is_hex(bytes[i + 1]) && is_hex(bytes[i + 2]),
            _ => c.is_ascii_alphanumeric(),
        };
        if !keep {
            // `written < i` implies the pending run is non-empty and made only
            // of kept (hence ASCII) bytes, so both indices are char boundaries;
            // the empty (`written == i`) case — which occurs between the bytes
            // of a multi-byte char being escaped — is skipped, since Rust
            // rejects even an empty str slice at a non-boundary index.
            if written < i {
                b.push_str(&s[written..i]);
            }
            let _ = write!(b, "%{c:02x}");
            written = i + 1;
        }
        i += 1;
    }
    b.push_str(&s[written..]);
    written != 0
}

/// Normalize (`norm`) or escape a URL part. Port of Go's `urlProcessor`:
/// `URL`-typed content is always normalized.
pub(crate) fn url_processor(norm: bool, s: &str, t: ContentType) -> Cow<'_, str> {
    let norm = norm || t == ContentType::Url;
    let mut b = String::new();
    if process_url_onto(s, norm, &mut b) {
        Cow::Owned(b)
    } else {
        Cow::Borrowed(s)
    }
}

/// Whether `s` is a relative URL or uses an http/https/mailto scheme. Port of
/// Go's `isSafeURL`.
pub(crate) fn is_safe_url(s: &str) -> bool {
    if let Some(colon) = s.find(':') {
        let protocol = &s[..colon];
        if !protocol.contains('/')
            && !protocol.eq_ignore_ascii_case("http")
            && !protocol.eq_ignore_ascii_case("https")
            && !protocol.eq_ignore_ascii_case("mailto")
        {
            return false;
        }
    }
    true
}

/// Go's `htmlSpaceAndASCIIAlnumBytes` bitset.
const HTML_SPACE_AND_ALNUM: [u8; 16] = [
    0x00, 0x36, 0x00, 0x00, 0x01, 0x00, 0xff, 0x03, 0xfe, 0xff, 0xff, 0x07, 0xfe, 0xff, 0xff, 0x07,
];

fn is_html_space(c: u8) -> bool {
    c <= 0x20 && (HTML_SPACE_AND_ALNUM[(c >> 3) as usize] & (1 << (c & 0x7))) != 0
}

fn is_html_space_or_ascii_alnum(c: u8) -> bool {
    c < 0x80 && (HTML_SPACE_AND_ALNUM[(c >> 3) as usize] & (1 << (c & 0x7))) != 0
}

/// Port of Go's `filterSrcsetElement`: normalize one candidate URL of a srcset
/// (the run `s[left..right]`) and append it — with its surrounding spacing and
/// alnum metadata — to `b`, or emit `#ZgotmplZ` if the URL or metadata is unsafe.
fn filter_srcset_element(s: &str, left: usize, right: usize, b: &mut String) {
    let bytes = s.as_bytes();
    let mut start = left;
    while start < right && is_html_space(bytes[start]) {
        start += 1;
    }
    let end = match bytes[start..right].iter().position(|&c| is_html_space(c)) {
        Some(off) => start + off,
        None => right,
    };
    let url = &s[start..end];
    if is_safe_url(url) {
        let metadata_ok = bytes[end..right]
            .iter()
            .all(|&c| is_html_space_or_ascii_alnum(c));
        if metadata_ok {
            b.push_str(&s[left..start]);
            process_url_onto(url, true, b);
            b.push_str(&s[end..right]);
            return;
        }
    }
    b.push('#');
    b.push_str(FILTER_FAILSAFE);
}

/// Filter and normalize a `srcset` value (comma-separated URLs with metadata).
/// Port of Go's `srcsetFilterAndEscaper`.
pub(crate) fn srcset_filter(s: &str, t: ContentType) -> Cow<'_, str> {
    match t {
        ContentType::Srcset => Cow::Borrowed(s),
        ContentType::Url => {
            // Normalizing strips the HTML whitespace separating URL and
            // metadata; commas additionally separate sources.
            let processed = url_processor(true, s, ContentType::Plain);
            if processed.as_bytes().contains(&b',') {
                Cow::Owned(processed.replace(',', "%2c"))
            } else {
                processed
            }
        }
        _ => {
            let bytes = s.as_bytes();
            let mut b = String::new();
            let mut written = 0usize;
            for (i, &byte) in bytes.iter().enumerate() {
                if byte == b',' {
                    filter_srcset_element(s, written, i, &mut b);
                    b.push(',');
                    written = i + 1;
                }
            }
            filter_srcset_element(s, written, bytes.len(), &mut b);
            Cow::Owned(b)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::sync::Arc;
    use alloc::vec;

    // Convenience: run an html_replacer table and own the result.
    fn hr(s: &str, table: HtmlTable, bad: bool) -> String {
        html_replacer(s, table, bad).into_owned()
    }

    // ---- content.go: stringify ----------------------------------------

    #[test]
    fn stringify_single_string_is_plain() {
        let args = [Value::String(Arc::from("hi"))];
        let (s, t) = stringify(&args);
        assert_eq!(s, "hi");
        assert_eq!(t, ContentType::Plain);
    }

    #[test]
    fn stringify_single_safe_carries_type() {
        let args = [Value::Safe {
            kind: SafeKind::Html,
            s: Arc::from("<b>"),
        }];
        let (s, t) = stringify(&args);
        assert_eq!(s, "<b>");
        assert_eq!(t, ContentType::Html);
    }

    #[test]
    fn stringify_lone_nil_is_empty() {
        let (s, t) = stringify(&[Value::Nil]);
        assert_eq!(s, "");
        assert_eq!(t, ContentType::Plain);
    }

    #[test]
    fn stringify_single_int() {
        let (s, _) = stringify(&[Value::Int(42)]);
        assert_eq!(s, "42");
    }

    #[test]
    fn stringify_multi_skips_nil_and_spaces() {
        // Two non-strings get a space; nil is dropped.
        let (s, t) = stringify(&[Value::Int(1), Value::Nil, Value::Int(2)]);
        assert_eq!(s, "1 2");
        assert_eq!(t, ContentType::Plain);
    }

    // ---- html.go: htmlReplacer ----------------------------------------

    #[test]
    fn html_replacer_basic() {
        assert_eq!(
            hr("<b> \"foo%\" O'Reilly &bar;", HtmlTable::Html, true),
            "&lt;b&gt; &#34;foo%&#34; O&#39;Reilly &amp;bar;"
        );
    }

    #[test]
    fn html_replacer_norm_keeps_amp() {
        assert_eq!(hr("a&amp;b", HtmlTable::HtmlNorm, true), "a&amp;b");
        assert_eq!(hr("a&amp;b", HtmlTable::Html, true), "a&amp;amp;b");
    }

    #[test]
    fn html_replacer_bad_runes_flag() {
        // U+FDEC is in the IE-hostile range: escaped when bad_runes=false,
        // passed through when bad_runes=true.
        assert_eq!(hr("x\u{fdec}y", HtmlTable::Nospace, false), "x&#xfdec;y");
        assert_eq!(hr("x\u{fdec}y", HtmlTable::Html, true), "x\u{fdec}y");
    }

    #[test]
    fn html_nospace_escaper_table() {
        // Adapted from html_test.go TestHTMLNospaceEscaper. The two invalid
        // bytes Go feeds (\x00 as NUL, \x96) are represented by '\0' and
        // U+FFFD, which drive the same replacement paths in valid UTF-8.
        let input = concat!(
            "\x00\x01\x02\x03\x04\x05\x06\x07\x08\t\n\x0b\x0c\r\x0e\x0f",
            "\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f",
            " !\"#$%&'()*+,-./",
            "0123456789:;<=>?",
            "@ABCDEFGHIJKLMNO",
            "PQRSTUVWXYZ[\\]^_",
            "`abcdefghijklmno",
            "pqrstuvwxyz{|}~\x7f",
            "\u{00A0}\u{0100}\u{2028}\u{2029}\u{feff}\u{fdec}\u{1D11E}",
            "erroneous\u{FFFD}0"
        );
        let want = concat!(
            "&#xfffd;\x01\x02\x03\x04\x05\x06\x07",
            "\x08&#9;&#10;&#11;&#12;&#13;\x0e\x0f",
            "\x10\x11\x12\x13\x14\x15\x16\x17",
            "\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f",
            "&#32;!&#34;#$%&amp;&#39;()*&#43;,-./",
            "0123456789:;&lt;&#61;&gt;?",
            "@ABCDEFGHIJKLMNO",
            "PQRSTUVWXYZ[\\]^_",
            "&#96;abcdefghijklmno",
            "pqrstuvwxyz{|}~\x7f",
            "\u{00A0}\u{0100}\u{2028}\u{2029}\u{feff}&#xfdec;\u{1D11E}",
            "erroneous&#xfffd;0"
        );
        assert_eq!(hr(input, HtmlTable::Nospace, false), want);
    }

    // ---- html.go: stripTags -------------------------------------------

    #[test]
    fn strip_tags_cases() {
        // Ported from html_test.go TestStripTags.
        let cases: &[(&str, &str)] = &[
            ("", ""),
            ("Hello, World!", "Hello, World!"),
            ("foo&amp;bar", "foo&amp;bar"),
            (
                "Hello <a href=\"www.example.com/\">World</a>!",
                "Hello World!",
            ),
            ("Foo <textarea>Bar</textarea> Baz", "Foo Bar Baz"),
            ("Foo <!-- Bar --> Baz", "Foo  Baz"),
            ("<", "<"),
            ("foo < bar", "foo < bar"),
            (
                "Foo<script type=\"text/javascript\">alert(1337)</script>Bar",
                "FooBar",
            ),
            ("Foo<div title=\"1>2\">Bar", "FooBar"),
            ("I <3 Ponies!", "I <3 Ponies!"),
            ("<script>foo()</script>", ""),
        ];
        for &(input, want) in cases {
            assert_eq!(strip_tags(input), want, "strip_tags({input:?})");
        }
    }

    // ---- html.go: htmlNameFilter --------------------------------------

    #[test]
    fn html_name_filter_basic() {
        // Known-safe HTMLAttr passes verbatim.
        assert_eq!(
            html_name_filter(" dir=\"ltr\"", ContentType::HtmlAttr),
            " dir=\"ltr\""
        );
        // Empty -> failsafe.
        assert_eq!(html_name_filter("", ContentType::Plain), FILTER_FAILSAFE);
        // A plain lowercased attr name that is not a known typed attribute.
        assert_eq!(html_name_filter("foo", ContentType::Plain), "foo");
        // A known non-plain attribute name -> failsafe (e.g. href is a URL).
        assert_eq!(
            html_name_filter("href", ContentType::Plain),
            FILTER_FAILSAFE
        );
        // Non [0-9a-z] chars -> failsafe.
        assert_eq!(html_name_filter("a b", ContentType::Plain), FILTER_FAILSAFE);
        assert_eq!(html_name_filter("on", ContentType::Plain), FILTER_FAILSAFE);
    }

    // ---- js.go: jsStrEscaper / jsRegexpEscaper via js_replace ---------

    #[test]
    fn js_str_escaper_cases() {
        let cases: &[(&str, &str)] = &[
            ("", ""),
            ("foo", "foo"),
            ("\u{0000}", "\\u0000"),
            ("\t", "\\t"),
            ("\n", "\\n"),
            ("\r", "\\r"),
            ("\u{2028}", "\\u2028"),
            ("\u{2029}", "\\u2029"),
            ("\\", "\\\\"),
            ("\\n", "\\\\n"),
            ("foo\r\nbar", "foo\\r\\nbar"),
            ("\"", "\\u0022"),
            ("'", "\\u0027"),
            ("&amp;", "\\u0026amp;"),
            ("</script>", "\\u003c\\/script\\u003e"),
            ("<![CDATA[", "\\u003c![CDATA["),
            ("]]>", "]]\\u003e"),
            ("<!--", "\\u003c!--"),
            ("-->", "--\\u003e"),
        ];
        for &(input, want) in cases {
            assert_eq!(js_replace(input, JsTable::Str), want, "jsStr({input:?})");
        }
    }

    #[test]
    fn js_regexp_escaper_cases() {
        let cases: &[(&str, &str)] = &[
            ("foo", "foo"),
            ("\t", "\\t"),
            ("\"", "\\u0022"),
            ("</script>", "\\u003c\\/script\\u003e"),
            ("<![CDATA[", "\\u003c!\\[CDATA\\["),
            ("]]>", "\\]\\]\\u003e"),
            ("<!--", "\\u003c!\\-\\-"),
            ("-->", "\\-\\-\\u003e"),
            ("*", "\\*"),
            ("+", "\\u002b"),
            ("?", "\\?"),
            ("[](){}", "\\[\\]\\(\\)\\{\\}"),
            ("$foo|x.y", "\\$foo\\|x\\.y"),
            ("x^y", "x\\^y"),
        ];
        for &(input, want) in cases {
            assert_eq!(js_replace(input, JsTable::Regexp), want, "jsRe({input:?})");
        }
    }

    #[test]
    fn js_str_escaper_full_ascii_and_high() {
        // From js_test.go TestEscapersOnLower7AndSelectHighCodepoints.
        let input = concat!(
            "\x00\x01\x02\x03\x04\x05\x06\x07\x08\t\n\x0b\x0c\r\x0e\x0f",
            "\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f",
            " !\"#$%&'()*+,-./",
            "0123456789:;<=>?",
            "@ABCDEFGHIJKLMNO",
            "PQRSTUVWXYZ[\\]^_",
            "`abcdefghijklmno",
            "pqrstuvwxyz{|}~\x7f",
            "\u{00A0}\u{0100}\u{2028}\u{2029}\u{feff}\u{1D11E}"
        );
        let want = concat!(
            "\\u0000\\u0001\\u0002\\u0003\\u0004\\u0005\\u0006\\u0007",
            "\\u0008\\t\\n\\u000b\\f\\r\\u000e\\u000f",
            "\\u0010\\u0011\\u0012\\u0013\\u0014\\u0015\\u0016\\u0017",
            "\\u0018\\u0019\\u001a\\u001b\\u001c\\u001d\\u001e\\u001f",
            " !\\u0022#$%\\u0026\\u0027()*\\u002b,-.\\/",
            "0123456789:;\\u003c=\\u003e?",
            "@ABCDEFGHIJKLMNO",
            "PQRSTUVWXYZ[\\\\]^_",
            "\\u0060abcdefghijklmno",
            "pqrstuvwxyz{|}~\x7f",
            "\u{00A0}\u{0100}\\u2028\\u2029\u{feff}\u{1D11E}"
        );
        assert_eq!(js_replace(input, JsTable::Str), want);
    }

    // ---- js.go: jsValEscaper ------------------------------------------

    #[test]
    fn js_val_escaper_cases() {
        assert_eq!(js_val_escaper(&[Value::Int(42)]), " 42 ");
        assert_eq!(js_val_escaper(&[Value::Uint(42)]), " 42 ");
        assert_eq!(js_val_escaper(&[Value::Int(-42)]), " -42 ");
        assert_eq!(
            js_val_escaper(&[Value::Uint(1u64 << 53)]),
            " 9007199254740992 "
        );
        assert_eq!(js_val_escaper(&[Value::Float(1.0)]), " 1 ");
        assert_eq!(js_val_escaper(&[Value::Float(-1.0)]), " -1 ");
        assert_eq!(js_val_escaper(&[Value::Float(0.5)]), " 0.5 ");
        assert_eq!(js_val_escaper(&[Value::Float(0.00390625)]), " 0.00390625 ");
        assert_eq!(js_val_escaper(&[Value::Float(0.0)]), " 0 ");
        assert_eq!(js_val_escaper(&[Value::Float(-0.0)]), " -0 ");
        assert_eq!(js_val_escaper(&[Value::String(Arc::from(""))]), "\"\"");
        assert_eq!(
            js_val_escaper(&[Value::String(Arc::from("foo"))]),
            "\"foo\""
        );
        assert_eq!(
            js_val_escaper(&[Value::String(Arc::from("\r\n\u{2028}\u{2029}"))]),
            "\"\\r\\n\\u2028\\u2029\""
        );
        assert_eq!(
            js_val_escaper(&[Value::String(Arc::from("\t\x0b"))]),
            "\"\\t\\u000b\""
        );
        assert_eq!(js_val_escaper(&[Value::Nil]), " null ");
        assert_eq!(
            js_val_escaper(&[Value::String(Arc::from("<!--"))]),
            "\"\\u003c!--\""
        );
        assert_eq!(
            js_val_escaper(&[Value::String(Arc::from("</script"))]),
            "\"\\u003c/script\""
        );
    }

    #[test]
    fn json_float_documented_divergences() {
        // Values inside [1e-6, 1e21) render as shortest decimal, matching Go.
        assert_eq!(json_marshal(&Value::Float(0.0)), "0");
        assert_eq!(json_marshal(&Value::Float(-0.0)), "-0");
        assert_eq!(json_marshal(&Value::Float(123.0)), "123");
        assert_eq!(json_marshal(&Value::Float(1e-6)), "0.000001");
        // Large magnitudes match Go's 'e' form (sign inserted, exponent >= 2
        // digits already).
        assert_eq!(json_marshal(&Value::Float(1e21)), "1e+21");
        assert_eq!(json_marshal(&Value::Float(1e100)), "1e+100");
        // Documented divergence: for a single-digit exponent Go's strconv pads
        // to two digits ("1e-07"); Rust's `{:e}` does not. Both are valid JS
        // numbers, so this is a formatting-fidelity gap, not a safety one.
        assert_eq!(json_marshal(&Value::Float(1e-7)), "1e-7"); // Go: "1e-07"
        assert_eq!(json_marshal(&Value::Float(5e-8)), "5e-8"); // Go: "5e-08"
        // Documented divergence: Go's json.Marshal errors on non-finite floats;
        // we emit `null` so the surrounding script stays syntactically valid.
        assert_eq!(json_marshal(&Value::Float(f64::NAN)), "null");
        assert_eq!(json_marshal(&Value::Float(f64::INFINITY)), "null");
        assert_eq!(json_marshal(&Value::Float(f64::NEG_INFINITY)), "null");
    }

    #[test]
    fn json_marshal_func_is_null() {
        // Documented divergence: Go's json.Marshal cannot encode a func (it
        // errors); we emit `null` to keep the script valid.
        let f: crate::value::ValueFunc = Arc::new(|_| Ok(Value::Nil));
        assert_eq!(json_marshal(&Value::Function(f)), "null");
    }

    #[test]
    fn js_val_escaper_structured() {
        let list = Value::List(Arc::from(vec![
            Value::Int(42),
            Value::String(Arc::from("foo")),
            Value::Nil,
        ]));
        assert_eq!(js_val_escaper(&[list]), "[42,\"foo\",null]");

        let list2 = Value::List(Arc::from(vec![
            Value::String(Arc::from("<!--")),
            Value::String(Arc::from("</script>")),
            Value::String(Arc::from("-->")),
        ]));
        assert_eq!(
            js_val_escaper(&[list2]),
            "[\"\\u003c!--\",\"\\u003c/script\\u003e\",\"--\\u003e\"]"
        );

        assert_eq!(js_val_escaper(&[Value::List(Arc::from(vec![]))]), "[]");
    }

    #[test]
    fn js_val_escaper_map_sorted() {
        let mut m = alloc::collections::BTreeMap::new();
        m.insert(Arc::from("X"), Value::Int(1));
        m.insert(Arc::from("Y"), Value::Int(2));
        let map = Value::Map(Arc::new(m));
        assert_eq!(js_val_escaper(&[map]), "{\"X\":1,\"Y\":2}");
    }

    #[test]
    fn js_val_escaper_safe_passthrough() {
        assert_eq!(
            js_val_escaper(&[Value::Safe {
                kind: SafeKind::Js,
                s: Arc::from("c && alert(\"Hi\");"),
            }]),
            "c && alert(\"Hi\");"
        );
        assert_eq!(
            js_val_escaper(&[Value::Safe {
                kind: SafeKind::JsStr,
                s: Arc::from("Hello \\u0021"),
            }]),
            "\"Hello \\u0021\""
        );
    }

    // ---- css.go: cssEscaper / decodeCSS / cssValueFilter --------------

    #[test]
    fn css_escaper_full() {
        // From css_test.go TestCSSEscaper.
        let input = concat!(
            "\x00\x01\x02\x03\x04\x05\x06\x07\x08\t\n\x0b\x0c\r\x0e\x0f",
            "\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f",
            " !\"#$%&'()*+,-./",
            "0123456789:;<=>?",
            "@ABCDEFGHIJKLMNO",
            "PQRSTUVWXYZ[\\]^_",
            "`abcdefghijklmno",
            "pqrstuvwxyz{|}~\x7f",
            "\u{00A0}\u{0100}\u{2028}\u{2029}\u{feff}\u{1D11E}"
        );
        let want = concat!(
            "\\0\x01\x02\x03\x04\x05\x06\x07",
            "\x08\\9 \\a\x0b\\c \\d\x0e\x0f",
            "\x10\x11\x12\x13\x14\x15\x16\x17",
            "\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f",
            " !\\22#$%\\26\\27\\28\\29*\\2b,-.\\2f ",
            "0123456789\\3a\\3b\\3c=\\3e?",
            "@ABCDEFGHIJKLMNO",
            "PQRSTUVWXYZ[\\\\]^_",
            "`abcdefghijklmno",
            "pqrstuvwxyz\\7b|\\7d~\x7f",
            "\u{00A0}\u{0100}\u{2028}\u{2029}\u{feff}\u{1D11E}"
        );
        assert_eq!(css_escaper(input), want);
    }

    #[test]
    fn decode_css_cases() {
        let cases: &[(&str, &str)] = &[
            ("", ""),
            ("foo", "foo"),
            ("foo\\", "foo"),
            ("foo\\\\", "foo\\"),
            ("\\", ""),
            ("\\A", "\n"),
            ("\\a", "\n"),
            ("\\0a", "\n"),
            ("\\00000a", "\n"),
            ("\\000000a", "\u{0000}a"),
            ("\\1234 5", "\u{1234}5"),
            ("\\12345", "\u{12345}"),
            ("\\\\", "\\"),
            ("\\.", "."),
        ];
        for &(input, want) in cases {
            let got = decode_css(input.as_bytes());
            assert_eq!(
                core::str::from_utf8(&got).unwrap(),
                want,
                "decode({input:?})"
            );
        }
    }

    #[test]
    fn css_value_filter_cases() {
        let ok: &[&str] = &[
            "",
            "foo",
            "0",
            "0px",
            "-5px",
            "1.25in",
            "+.33em",
            "100%",
            "12.5%",
            ".foo",
            "#bar",
            "corner-radius",
            "-moz-corner-radius",
            "#000",
            "#48f",
            "#123456",
            "color: red",
        ];
        for &s in ok {
            assert_eq!(css_value_filter(s, ContentType::Plain), s, "ok({s:?})");
        }
        let bad: &[&str] = &[
            "<!--",
            "-->",
            "<![CDATA[",
            "]]>",
            "</style",
            "\"",
            "'",
            "`",
            "\x00",
            "/* foo */",
            "//",
            "[href=~",
            "expression(alert(1337))",
            "-expression(alert(1337))",
            "expression",
            "Expression",
            "EXPRESSION",
            "-moz-binding",
            "@import url evil.css",
            "<",
            ">",
        ];
        for &s in bad {
            assert_eq!(
                css_value_filter(s, ContentType::Plain),
                FILTER_FAILSAFE,
                "bad({s:?})"
            );
        }
        // Escaped-then-decoded value that survives filtering.
        assert_eq!(
            css_value_filter("-expre\\0000073sion", ContentType::Plain),
            "-expre\x073sion"
        );
        // CSS-typed content passes through verbatim.
        assert_eq!(
            css_value_filter("a[href =~ \"//example.com\"]#foo", ContentType::Css),
            "a[href =~ \"//example.com\"]#foo"
        );
    }

    // ---- url.go: urlProcessor / isSafeURL / srcset --------------------

    #[test]
    fn url_normalizer_cases() {
        let cases: &[(&str, &str)] = &[
            ("", ""),
            (
                "http://example.com:80/foo/bar?q=foo%20&bar=x+y#frag",
                "http://example.com:80/foo/bar?q=foo%20&bar=x+y#frag",
            ),
            (" ", "%20"),
            ("%7c", "%7c"),
            ("%7C", "%7C"),
            ("%2", "%252"),
            ("%", "%25"),
            ("%z", "%25z"),
            ("/foo|bar/%5c\u{1234}", "/foo%7cbar/%5c%e1%88%b4"),
        ];
        for &(input, want) in cases {
            assert_eq!(
                url_processor(true, input, ContentType::Plain),
                want,
                "norm({input:?})"
            );
            // Idempotent.
            assert_eq!(url_processor(true, want, ContentType::Plain), want);
        }
    }

    #[test]
    fn url_filters_full() {
        // From url_test.go TestURLFilters.
        let input = concat!(
            "\x00\x01\x02\x03\x04\x05\x06\x07\x08\t\n\x0b\x0c\r\x0e\x0f",
            "\x10\x11\x12\x13\x14\x15\x16\x17\x18\x19\x1a\x1b\x1c\x1d\x1e\x1f",
            " !\"#$%&'()*+,-./",
            "0123456789:;<=>?",
            "@ABCDEFGHIJKLMNO",
            "PQRSTUVWXYZ[\\]^_",
            "`abcdefghijklmno",
            "pqrstuvwxyz{|}~\x7f",
            "\u{00A0}\u{0100}\u{2028}\u{2029}\u{feff}\u{1D11E}"
        );
        let want_escape = concat!(
            "%00%01%02%03%04%05%06%07%08%09%0a%0b%0c%0d%0e%0f",
            "%10%11%12%13%14%15%16%17%18%19%1a%1b%1c%1d%1e%1f",
            "%20%21%22%23%24%25%26%27%28%29%2a%2b%2c-.%2f",
            "0123456789%3a%3b%3c%3d%3e%3f",
            "%40ABCDEFGHIJKLMNO",
            "PQRSTUVWXYZ%5b%5c%5d%5e_",
            "%60abcdefghijklmno",
            "pqrstuvwxyz%7b%7c%7d~%7f",
            "%c2%a0%c4%80%e2%80%a8%e2%80%a9%ef%bb%bf%f0%9d%84%9e"
        );
        let want_norm = concat!(
            "%00%01%02%03%04%05%06%07%08%09%0a%0b%0c%0d%0e%0f",
            "%10%11%12%13%14%15%16%17%18%19%1a%1b%1c%1d%1e%1f",
            "%20!%22#$%25&%27%28%29*+,-./",
            "0123456789:;%3c=%3e?",
            "@ABCDEFGHIJKLMNO",
            "PQRSTUVWXYZ[%5c]%5e_",
            "%60abcdefghijklmno",
            "pqrstuvwxyz%7b%7c%7d~%7f",
            "%c2%a0%c4%80%e2%80%a8%e2%80%a9%ef%bb%bf%f0%9d%84%9e"
        );
        assert_eq!(url_processor(false, input, ContentType::Plain), want_escape);
        assert_eq!(url_processor(true, input, ContentType::Plain), want_norm);
    }

    #[test]
    fn is_safe_url_cases() {
        assert!(is_safe_url("http://example.com"));
        assert!(is_safe_url("https://example.com"));
        assert!(is_safe_url("HTTP://example.com"));
        assert!(is_safe_url("mailto:a@b.com"));
        assert!(is_safe_url("/relative/path"));
        assert!(is_safe_url("?q=1"));
        assert!(is_safe_url("foo/bar:baz")); // slash before colon -> not a scheme
        assert!(!is_safe_url("javascript:alert(1)"));
        assert!(!is_safe_url("data:text/html,x"));
    }

    #[test]
    fn srcset_filter_cases() {
        let cases: &[(&str, &str)] = &[
            ("http://example.com/img.png", "http://example.com/img.png"),
            (" /img.png 200w", " /img.png 200w"),
            ("javascript:alert(1) 200w", "#ZgotmplZ"),
            ("foo.png, bar.png", "foo.png, bar.png"),
            ("javascript:alert(1), /foo.png", "#ZgotmplZ, /foo.png"),
            ("/bogus#, javascript:alert(1)", "/bogus#,#ZgotmplZ"),
        ];
        for &(input, want) in cases {
            assert_eq!(
                srcset_filter(input, ContentType::Plain),
                want,
                "srcset({input:?})"
            );
        }
    }
}
