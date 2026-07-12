//! Shared byte-slice and CSS/rune lexing helpers used across the html escaping
//! modules (`transition`, `primitives`, `escape`).
//!
//! These are the small `bytes.*` / `utf8.*` / `css.go` helpers Go keeps as
//! package-level functions. They were previously duplicated in `transition.rs`
//! and `primitives.rs`; this module is their single home.

use alloc::borrow::Cow;
use alloc::vec::Vec;

use super::context::Delim;

// ---------------------------------------------------------------------------
// Byte-slice helpers mirroring the Go `bytes` functions used by the machine.
// Every search set passed here is ASCII-only, so byte-level scanning matches
// Go's rune-level `bytes.IndexAny` (a UTF-8 continuation byte is always >= 0x80
// and can never equal an ASCII search byte).
// ---------------------------------------------------------------------------

pub(super) fn index_byte(s: &[u8], b: u8) -> Option<usize> {
    s.iter().position(|&x| x == b)
}

pub(super) fn index_any(s: &[u8], set: &[u8]) -> Option<usize> {
    s.iter().position(|x| set.contains(x))
}

pub(super) fn contains_any(s: &[u8], set: &[u8]) -> bool {
    s.iter().any(|x| set.contains(x))
}

pub(super) fn index_sub(s: &[u8], sub: &[u8]) -> Option<usize> {
    if sub.is_empty() {
        return Some(0);
    }
    if sub.len() > s.len() {
        return None;
    }
    s.windows(sub.len()).position(|w| w == sub)
}

pub(super) fn trim_left<'a>(s: &'a [u8], set: &[u8]) -> &'a [u8] {
    let mut i = 0;
    while i < s.len() && set.contains(&s[i]) {
        i += 1;
    }
    &s[i..]
}

pub(super) fn trim_right<'a>(s: &'a [u8], set: &[u8]) -> &'a [u8] {
    let mut i = s.len();
    while i > 0 && set.contains(&s[i - 1]) {
        i -= 1;
    }
    &s[..i]
}

/// The bytes that end an attribute value of a given delimiter — Go's
/// `delimEnds` (`escape.go`).
pub(super) fn delim_ends(d: Delim) -> &'static [u8] {
    match d {
        Delim::DoubleQuote => b"\"",
        Delim::SingleQuote => b"'",
        Delim::SpaceOrTagEnd => b" \t\n\x0c\r>",
        Delim::None => b"",
    }
}

// ---------------------------------------------------------------------------
// Rune / CSS lexing helpers (Go's utf8.* and css.go helpers).
// ---------------------------------------------------------------------------

pub(super) fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

/// Go's `isCSSSpace` (the `wc` production plus CR).
pub(super) fn is_css_space(c: u8) -> bool {
    matches!(c, b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

/// Go's `isCSSNmchar`: a rune allowed anywhere in a CSS identifier.
pub(super) fn is_css_nmchar(r: char) -> bool {
    let u = r as u32;
    r.is_ascii_alphanumeric()
        || r == '-'
        || r == '_'
        || (0x80..=0xD7FF).contains(&u)
        || (0xE000..=0xFFFD).contains(&u)
        || (0x1_0000..=0x10_FFFF).contains(&u)
}

pub(super) fn is_js_ident_part(b: u8) -> bool {
    b == b'$' || b.is_ascii_digit() || b.is_ascii_uppercase() || b == b'_' || b.is_ascii_lowercase()
}

/// Go's `hexDecode`: decode a short hex sequence ("10" -> 16). Non-hex bytes
/// are ignored (callers only pass hex digits).
pub(super) fn hex_decode(s: &[u8]) -> u32 {
    let mut n: u32 = 0;
    for &c in s {
        n <<= 4;
        if c.is_ascii_digit() {
            n |= u32::from(c - b'0');
        } else if (b'a'..=b'f').contains(&c) {
            n |= u32::from(c - b'a') + 10;
        } else if (b'A'..=b'F').contains(&c) {
            n |= u32::from(c - b'A') + 10;
        }
    }
    n
}

/// Go's `skipCSSSpace`: drop a single leading CSS whitespace char (CRLF counts
/// as one).
pub(super) fn skip_css_space(c: &[u8]) -> &[u8] {
    match c.first() {
        Some(b'\t' | b'\n' | 0x0c | b' ') => &c[1..],
        Some(b'\r') => {
            if c.len() >= 2 && c[1] == b'\n' {
                &c[2..]
            } else {
                &c[1..]
            }
        }
        _ => c,
    }
}

/// Decode a single UTF-8 rune from the front of `p`, mirroring Go's
/// `utf8.DecodeRune` (invalid input yields `U+FFFD` with width 1).
pub(super) fn decode_rune(p: &[u8]) -> (char, usize) {
    match p.first() {
        None => ('\u{FFFD}', 0),
        Some(&b0) if b0 < 0x80 => (b0 as char, 1),
        Some(&b0) => {
            let len = match b0 {
                0xC0..=0xDF => 2,
                0xE0..=0xEF => 3,
                0xF0..=0xF7 => 4,
                _ => return ('\u{FFFD}', 1),
            };
            if p.len() < len {
                return ('\u{FFFD}', 1);
            }
            match core::str::from_utf8(&p[..len]) {
                Ok(valid) => match valid.chars().next() {
                    Some(ch) => (ch, len),
                    None => ('\u{FFFD}', 1),
                },
                Err(_) => ('\u{FFFD}', 1),
            }
        }
    }
}

/// Decode the last UTF-8 rune of `p`, mirroring Go's `utf8.DecodeLastRune`.
pub(super) fn decode_last_rune(p: &[u8]) -> (char, usize) {
    let end = p.len();
    if end == 0 {
        return ('\u{FFFD}', 0);
    }
    if p[end - 1] < 0x80 {
        return (p[end - 1] as char, 1);
    }
    // Scan back up to 4 bytes for the lead byte.
    let mut start = end - 1;
    let lim = end.saturating_sub(4);
    while start > lim {
        if p[start] & 0xC0 != 0x80 {
            break;
        }
        start -= 1;
    }
    let (r, size) = decode_rune(&p[start..]);
    if start + size != end {
        return ('\u{FFFD}', 1);
    }
    (r, size)
}

/// The byte index of the first JS line terminator (`\n`, `\r`, U+2028, U+2029),
/// as Go's `bytes.IndexAny` over that set would report. The only non-ASCII
/// members are the two line separators U+2028 (E2 80 A8) and U+2029 (E2 80 A9).
pub(super) fn index_js_line_terminator(s: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'\n' | b'\r' => return Some(i),
            // U+2028 = E2 80 A8, U+2029 = E2 80 A9.
            0xE2 if i + 3 <= s.len()
                && s[i + 1] == 0x80
                && (s[i + 2] == 0xA8 || s[i + 2] == 0xA9) =>
            {
                return Some(i);
            }
            _ => i += 1,
        }
    }
    None
}

pub(super) fn push_rune(b: &mut Vec<u8>, r: u32) {
    // Go's utf8.EncodeRune encodes an invalid rune as U+FFFD.
    let ch = char::from_u32(r).unwrap_or('\u{FFFD}');
    let mut buf = [0u8; 4];
    b.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
}

/// Go's `decodeCSS`: decode CSS3 escapes. Borrows the input unchanged when it
/// contains no `\`.
pub(super) fn decode_css(s: &[u8]) -> Cow<'_, [u8]> {
    if index_byte(s, b'\\').is_none() {
        return Cow::Borrowed(s);
    }
    let mut b: Vec<u8> = Vec::with_capacity(s.len());
    let mut s = s;
    while !s.is_empty() {
        let i = index_byte(s, b'\\').unwrap_or(s.len());
        b.extend_from_slice(&s[..i]);
        s = &s[i..];
        if s.len() < 2 {
            break;
        }
        if is_hex(s[1]) {
            // unicode ::= '\' [0-9a-fA-F]{1,6} wc?
            let mut j = 2;
            while j < s.len() && j < 7 && is_hex(s[j]) {
                j += 1;
            }
            let mut r = hex_decode(&s[1..j]);
            if r > 0x10_FFFF {
                r /= 16;
                j -= 1;
            }
            push_rune(&mut b, r);
            // The optional space after a hex sequence is consumed.
            s = skip_css_space(&s[j..]);
        } else {
            // `\\` decodes to `\` and `\"` to `"`.
            let (_, n) = decode_rune(&s[1..]);
            b.extend_from_slice(&s[1..1 + n]);
            s = &s[1 + n..];
        }
    }
    Cow::Owned(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- css.go: isCSSNmchar --------------------------------------------

    #[test]
    fn is_css_nmchar_cases() {
        // Ported from css_test.go TestIsCSSNmchar. Go's surrogate (0xd800,
        // 0xdc00) and out-of-range (0x110000) false-cases cannot be expressed
        // as a Rust `char` (the type forbids them), so the type system already
        // guarantees those inputs never reach this function; the remaining
        // rows are ported verbatim.
        let cases: &[(char, bool)] = &[
            ('\u{0}', false),
            ('0', true),
            ('9', true),
            ('A', true),
            ('Z', true),
            ('a', true),
            ('z', true),
            ('_', true),
            ('-', true),
            (':', false),
            (';', false),
            (' ', false),
            ('\u{7f}', false),
            ('\u{80}', true),
            ('\u{1234}', true),
            ('\u{fffe}', false),
            ('\u{10000}', true),
        ];
        for &(r, want) in cases {
            assert_eq!(is_css_nmchar(r), want, "is_css_nmchar({r:?})");
        }
    }

    // ---- css.go: skipCSSSpace -------------------------------------------

    #[test]
    fn skip_css_space_cases() {
        // Ported from css_test.go TestSkipCSSSpace. Only a single leading CSS
        // space is removed; an escaped space (`\20`) is left intact.
        let cases: &[(&str, &str)] = &[
            ("", ""),
            ("foo", "foo"),
            ("\n", ""),
            ("\r\n", ""),
            ("\r", ""),
            ("\t", ""),
            (" ", ""),
            ("\u{0c}", ""),
            (" foo", "foo"),
            ("  foo", " foo"),
            (r"\20", r"\20"),
        ];
        for &(input, want) in cases {
            let got = skip_css_space(input.as_bytes());
            assert_eq!(
                core::str::from_utf8(got).unwrap(),
                want,
                "skip_css_space({input:?})"
            );
        }
    }
}
