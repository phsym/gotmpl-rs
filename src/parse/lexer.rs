//! Lexer (scanner) that tokenizes template source text into a stream of tokens.
//!
//! The lexer recognizes two "worlds":
//! - **Outside delimiters**: everything is raw text (`TokenKind::Text`)
//! - **Inside delimiters**: keywords, identifiers, operators, and literals
//!
//! Supports configurable delimiters (default `{{` / `}}`), trim markers
//! (`{{-` / `-}}`), comments (`{{/* ... */}}`), and Go-compatible number
//! literals (hex, octal, binary, underscores).
//!
//! This module is primarily used internally by the [`Parser`](crate::parse::Parser).

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::node::Number;
use crate::error::{Result, TemplateError};

/// Build an integer-literal lex error message that distinguishes an
/// overflow from a syntax error, using `ParseIntError::kind()`.
fn int_parse_msg(kind: &str, err: &core::num::ParseIntError) -> String {
    use core::num::IntErrorKind::*;
    match err.kind() {
        PosOverflow | NegOverflow => format!("{} integer literal overflows i64", kind),
        _ => format!("invalid {} number", kind),
    }
}

fn strip_underscores(raw: &str) -> Cow<'_, str> {
    if raw.contains('_') {
        Cow::Owned(raw.replace('_', ""))
    } else {
        Cow::Borrowed(raw)
    }
}

// Token types
/// The kind of a lexed [`Token`].
#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Structural
    Text,           // raw text outside {{ }}
    LeftDelim,      // {{
    RightDelim,     // }}
    LeftTrimDelim,  // {{- (trim whitespace before)
    RightTrimDelim, // -}} (trim whitespace after)

    // Values
    Dot,        // .
    Field,      // .FieldName
    Variable,   // $var
    Identifier, // function name or keyword
    String,     // "quoted string" or `raw string`
    Number,     // integer or float literal
    Bool,       // true / false
    Nil,        // nil
    Char,       // character literal

    // Operators
    Pipe,       // |
    Comma,      // ,
    Assign,     // =
    Declare,    // :=
    LeftParen,  // (
    RightParen, // )

    // Keywords (distinguished from Identifier after lexing)
    If,
    Else,
    End,
    Range,
    With,
    Define,
    Template,
    Block,
    Break,
    Continue,

    // Special
    Eof,
}

/// A single token produced by the [`Lexer`].
///
/// The `val` field borrows from the source when possible (text, identifiers,
/// field names, keywords, raw strings) and only allocates for values that
/// must be transformed, i.e. quoted strings with escapes. For `Number` and
/// `Char` tokens, `val` always borrows the raw source slice and the parsed
/// numeric value is carried in `num`, so the parser can skip the decimal
/// round-trip.
#[derive(Debug, Clone)]
pub struct Token<'a> {
    /// What kind of token this is.
    pub kind: TokenKind,
    /// The token's string value (literal text, identifier name, number, etc.).
    pub val: Cow<'a, str>,
    /// Pre-parsed numeric value for `Number` / `Char` tokens. `None` for all
    /// other token kinds.
    pub num: Option<Number>,
    /// Byte offset in the original source.
    pub pos: usize,
    /// 1-based line number tracked during scanning.
    pub line: usize,
}

impl Token<'_> {
    /// Compute the 1-based `(line, column)` from the byte offset and the original source.
    ///
    /// `self.pos` is a byte offset. Columns count UTF-8 *characters*, not bytes,
    /// so multi-byte characters advance the column by one.
    pub fn line_col(&self, source: &str) -> (usize, usize) {
        debug_assert!(
            self.pos <= source.len(),
            "token pos {} exceeds source length {}",
            self.pos,
            source.len()
        );
        let end = self.pos.min(source.len());
        debug_assert!(
            source.is_char_boundary(end),
            "token pos {} is not on a UTF-8 char boundary",
            end
        );
        let mut line = 1;
        let mut col = 1;
        for ch in source[..end].chars() {
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }
}

// Lexer
/// Tokenizer for Go template source text.
///
/// Converts a raw template string into a [`Vec<Token>`] via [`tokenize`](Self::tokenize).
/// Supports configurable delimiters, trim markers, comments, and Go-compatible
/// number literals.
///
/// This is used internally by the [`Parser`](crate::parse::Parser); most users
/// interact with [`Template`](crate::Template) directly.
pub struct Lexer<'a> {
    input: &'a str,
    pos: usize,
    start: usize,
    tokens: Vec<Token<'a>>,
    left_delim: &'a str,
    right_delim: &'a str,
    line: usize,
    parse_name: Option<&'a str>,
}

impl<'a> Lexer<'a> {
    /// Create a new lexer for the given input with the specified delimiters.
    pub fn new(input: &'a str, left_delim: &'a str, right_delim: &'a str) -> Self {
        // Heuristic: one token per ~8 bytes of source. Overshoots slightly on
        // ASCII-heavy text, which is fine; it avoids the ~7 reallocations a
        // defaulted Vec would do on a 100-token template.
        let capacity = input.len() / 8 + 8;
        Lexer {
            input,
            pos: 0,
            start: 0,
            tokens: Vec::with_capacity(capacity),
            left_delim,
            right_delim,
            line: 1,
            parse_name: None,
        }
    }

    /// Tag the lexer with a source name (e.g. file path) that is included in
    /// lex error messages. The name is borrowed for the lexer's lifetime and
    /// only copied into a `String` when a lex error is actually emitted.
    pub fn with_name(mut self, name: Option<&'a str>) -> Self {
        self.parse_name = name;
        self
    }

    fn at_left_trim(&self) -> bool {
        self.starts_with(self.left_delim)
            && self.input.as_bytes().get(self.pos + self.left_delim.len()) == Some(&b'-')
    }

    fn at_right_trim(&self) -> bool {
        self.input.as_bytes().get(self.pos) == Some(&b'-')
            && self.input[self.pos + 1..].starts_with(self.right_delim)
    }

    /// Tokenize the entire input and return the token stream.
    pub fn tokenize(mut self) -> Result<Vec<Token<'a>>> {
        self.lex_text()?;
        self.emit(TokenKind::Eof);
        Ok(self.tokens)
    }

    // Helpers
    fn starts_with(&self, prefix: &str) -> bool {
        self.input.as_bytes()[self.pos..].starts_with(prefix.as_bytes())
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    /// Look `n` characters ahead without advancing. O(n) in `n` (scans chars
    /// from `self.pos`), so only use for small lookaheads, not in hot loops.
    fn peek_ahead(&self, n: usize) -> Option<char> {
        self.input[self.pos..].chars().nth(n)
    }

    fn next_char(&mut self) -> Option<char> {
        let ch = self.input[self.pos..].chars().next()?;
        self.pos += ch.len_utf8();
        if ch == '\n' {
            self.line += 1;
        }
        Some(ch)
    }

    /// Back up one character. `self.pos` must be a char boundary and greater
    /// than zero; both hold because every advance path (`next_char`, `skip`)
    /// moves by whole UTF-8 characters starting from 0.
    fn backup(&mut self) {
        debug_assert!(self.pos > 0, "backup with self.pos == 0");
        debug_assert!(
            self.input.is_char_boundary(self.pos),
            "backup from non-boundary offset"
        );
        let bytes = self.input.as_bytes();
        let mut i = self.pos;
        loop {
            i -= 1;
            // Stop at a char boundary: any non-continuation byte.
            if bytes[i] < 0x80 || bytes[i] >= 0xC0 {
                break;
            }
        }
        if bytes[i] == b'\n' {
            self.line -= 1;
        }
        self.pos = i;
    }

    /// Advance `n` bytes, tracking newlines. `n` must land on a char boundary
    /// (callers pass `delim.len()` or byte counts of ASCII sequences).
    fn skip(&mut self, n: usize) {
        let target = self.pos + n;
        while self.pos < target {
            self.next_char();
        }
        debug_assert_eq!(self.pos, target, "skip({}) overshot a char boundary", n);
    }

    fn current_str(&self) -> &'a str {
        debug_assert!(self.input.is_char_boundary(self.start));
        debug_assert!(self.input.is_char_boundary(self.pos));
        &self.input[self.start..self.pos]
    }

    fn emit(&mut self, kind: TokenKind) {
        let val = Cow::Borrowed(self.current_str());
        self.tokens.push(Token {
            kind,
            val,
            num: None,
            pos: self.start,
            line: self.line,
        });
        self.start = self.pos;
    }

    fn emit_val(&mut self, kind: TokenKind, val: Cow<'a, str>) {
        self.tokens.push(Token {
            kind,
            val,
            num: None,
            pos: self.start,
            line: self.line,
        });
        self.start = self.pos;
    }

    fn emit_num(&mut self, kind: TokenKind, num: Number) {
        let val = Cow::Borrowed(self.current_str());
        self.tokens.push(Token {
            kind,
            val,
            num: Some(num),
            pos: self.start,
            line: self.line,
        });
        self.start = self.pos;
    }

    fn ignore(&mut self) {
        self.start = self.pos;
    }

    fn error(&self, msg: impl Into<String>) -> TemplateError {
        // Cold path: error construction. `col` is derived on demand rather
        // than carried through scanning because every `next_char` / `backup`
        // would otherwise need a branch to reset or decrement it. `line` is
        // already tracked because newlines are rare by comparison, and
        // scanning back to the previous newline runs once per error.
        let prefix = &self.input[..self.pos];
        let col = match prefix.rfind('\n') {
            Some(nl) => prefix[nl + 1..].chars().count() + 1,
            None => prefix.chars().count() + 1,
        };
        TemplateError::Lex {
            name: self.parse_name.map(String::from),
            line: self.line,
            col,
            message: msg.into(),
        }
    }

    // State: Scanning text outside delimiters
    fn lex_text(&mut self) -> Result<()> {
        // Track whether the last action ended with a trim marker (-}})
        // so we can trim leading whitespace from the next text.
        let mut trim_leading = false;

        loop {
            if self.pos >= self.input.len() {
                if self.pos > self.start {
                    self.emit_pending_text(trim_leading, false);
                }
                return Ok(());
            }

            // Check for left trim delimiter (e.g. "{{-")
            if self.at_left_trim() {
                if self.pos > self.start {
                    // {{- trims trailing whitespace from preceding text
                    self.emit_pending_text(trim_leading, true);
                }
                self.skip(self.left_delim.len() + 1);
                self.ignore();
                self.emit(TokenKind::LeftTrimDelim);
                trim_leading = self.lex_action_body()?;
                continue;
            }

            // Check for regular left delimiter (e.g. "{{")
            let ld = self.left_delim;
            if self.starts_with(ld) {
                if self.pos > self.start {
                    self.emit_pending_text(trim_leading, false);
                }
                let ld_len = self.left_delim.len();
                self.skip(ld_len);
                self.ignore();
                self.emit(TokenKind::LeftDelim);
                trim_leading = self.lex_action_body()?;
                continue;
            }

            self.next_char();
        }
    }

    /// Emit accumulated text before a delimiter, applying trim as needed.
    ///
    /// `trim_leading`: trim whitespace from the start (previous action had `-}}`).
    /// `trim_trailing`: trim whitespace from the end (current delimiter is `{{-`).
    fn emit_pending_text(&mut self, trim_leading: bool, trim_trailing: bool) {
        let mut slice = self.current_str();
        if trim_leading {
            slice = slice.trim_start();
        }
        if trim_trailing {
            slice = slice.trim_end();
        }
        if !slice.is_empty() {
            self.emit_val(TokenKind::Text, Cow::Borrowed(slice));
        } else {
            self.ignore();
        }
    }

    /// Lex the body of an action after the left delimiter has been emitted.
    /// Handles comments and regular action content.
    /// Returns whether the close delimiter was a trim marker (`-}}`).
    fn lex_action_body(&mut self) -> Result<bool> {
        if let Some(trims) = self.try_lex_comment()? {
            return Ok(trims);
        }
        self.lex_inside()?;
        Ok(self
            .tokens
            .last()
            .is_some_and(|t| t.kind == TokenKind::RightTrimDelim))
    }

    /// Try to lex a comment after the left delimiter has been consumed.
    /// Returns `Ok(Some(close_trims))` if a comment was found and consumed,
    /// where `close_trims` indicates whether trailing whitespace should be trimmed.
    /// Returns `Ok(None)` if not a comment (position is restored).
    fn try_lex_comment(&mut self) -> Result<Option<bool>> {
        let saved_pos = self.pos;
        let saved_start = self.start;
        let saved_line = self.line;

        // Skip whitespace
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.next_char();
            } else {
                break;
            }
        }

        if !self.starts_with("/*") {
            // Not a comment, restore
            self.pos = saved_pos;
            self.start = saved_start;
            self.line = saved_line;
            return Ok(None);
        }

        self.skip(2); // consume /*

        // Scan to closing */
        loop {
            if self.pos >= self.input.len() {
                return Err(self.error("unclosed comment"));
            }
            if self.starts_with("*/") {
                self.skip(2);
                break;
            }
            self.next_char();
        }

        // Skip whitespace after */
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.next_char();
            } else {
                break;
            }
        }

        // Detect whether close has trim marker
        let close_trims;
        if self.at_right_trim() {
            self.skip(self.right_delim.len() + 1);
            close_trims = true;
        } else if self.starts_with(self.right_delim) {
            self.skip(self.right_delim.len());
            close_trims = false;
        } else {
            return Err(self.error("comment not terminated by closing delimiter"));
        }

        self.ignore();

        // Remove the LeftDelim/LeftTrimDelim token that was already emitted.
        // Check if the open was a trim delimiter.
        let open_was_trim = self
            .tokens
            .last()
            .is_some_and(|t| t.kind == TokenKind::LeftTrimDelim);
        self.tokens.pop();

        // The caller (lex_text) needs to know if trailing whitespace should be
        // trimmed. This happens when either open or close had a trim marker.
        Ok(Some(open_was_trim || close_trims))
    }

    // State: Scanning inside {{ ... }}
    fn lex_inside(&mut self) -> Result<()> {
        loop {
            self.skip_whitespace();
            self.ignore();

            if self.pos >= self.input.len() {
                return Err(self.error("unclosed action"));
            }

            // Check for right delimiter (with optional trim)
            if self.at_right_trim() {
                self.skip(self.right_delim.len() + 1);
                self.ignore();
                self.emit(TokenKind::RightTrimDelim);
                return Ok(());
            }

            if self.starts_with(self.right_delim) {
                let rd_len = self.right_delim.len();
                self.skip(rd_len);
                self.ignore();
                self.emit(TokenKind::RightDelim);
                return Ok(());
            }

            let Some(ch) = self.peek() else {
                return Err(self.error("unclosed action"));
            };

            match ch {
                '|' => {
                    self.next_char();
                    self.emit(TokenKind::Pipe);
                }
                ',' => {
                    self.next_char();
                    self.emit(TokenKind::Comma);
                }
                '(' => {
                    self.next_char();
                    self.emit(TokenKind::LeftParen);
                }
                ')' => {
                    self.next_char();
                    self.emit(TokenKind::RightParen);
                }
                ':' => {
                    self.next_char();
                    if self.peek() == Some('=') {
                        self.next_char();
                        self.emit(TokenKind::Declare);
                    } else {
                        return Err(self.error("expected '=' after ':'"));
                    }
                }
                '=' => {
                    self.next_char();
                    self.emit(TokenKind::Assign);
                }
                '"' => self.lex_quoted_string()?,
                '`' => self.lex_raw_string()?,
                '.' => {
                    // Could be just dot, or a field like .Name, or a number like .5
                    self.next_char();
                    if self.peek().is_none_or(|c| !c.is_alphanumeric() && c != '_') {
                        self.emit(TokenKind::Dot);
                    } else if self.peek().is_some_and(|c| c.is_ascii_digit()) {
                        // It's a float starting with .
                        self.backup();
                        self.lex_number()?;
                    } else {
                        // It's a field access: .Name
                        self.lex_field()?;
                    }
                }
                '$' => self.lex_variable()?,
                '-' | '+' => {
                    // Could be a sign for a number, or just a minus/plus
                    if self
                        .peek_ahead(1)
                        .is_some_and(|c| c.is_ascii_digit() || c == '.')
                    {
                        self.lex_number()?;
                    } else {
                        return Err(self.error(format!("unexpected character: {:?}", ch)));
                    }
                }
                '0'..='9' => self.lex_number()?,
                '\'' => self.lex_char_literal()?,
                _ if ch.is_alphabetic() || ch == '_' => self.lex_identifier()?,
                _ => return Err(self.error(format!("unexpected character: {:?}", ch))),
            }
        }
    }

    fn skip_whitespace(&mut self) {
        while let Some(ch) = self.peek() {
            if ch.is_whitespace() {
                self.next_char();
            } else {
                break;
            }
        }
    }

    // Individual token scanners
    fn lex_quoted_string(&mut self) -> Result<()> {
        self.next_char(); // consume opening "
        loop {
            match self.next_char() {
                None => return Err(self.error("unterminated string")),
                Some('\\') => {
                    self.next_char();
                } // skip escaped char
                Some('"') => {
                    let raw = self.current_str();
                    // Strip surrounding quotes for the value
                    let inner = &raw[1..raw.len() - 1];
                    // Only allocate when the literal actually contains escapes.
                    let val = if inner.contains('\\') {
                        Cow::Owned(unescape(inner)?)
                    } else {
                        Cow::Borrowed(inner)
                    };
                    self.emit_val(TokenKind::String, val);
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    fn lex_raw_string(&mut self) -> Result<()> {
        self.next_char(); // consume opening `
        loop {
            match self.next_char() {
                None => return Err(self.error("unterminated raw string")),
                Some('`') => {
                    let raw = self.current_str();
                    let inner = &raw[1..raw.len() - 1];
                    self.emit_val(TokenKind::String, Cow::Borrowed(inner));
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    fn lex_field(&mut self) -> Result<()> {
        // We've already consumed the '.', now consume the identifier part
        while let Some(ch) = self.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                self.next_char();
            } else {
                break;
            }
        }
        self.emit(TokenKind::Field);
        Ok(())
    }

    fn lex_variable(&mut self) -> Result<()> {
        self.next_char(); // consume $
        while let Some(ch) = self.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                self.next_char();
            } else {
                break;
            }
        }
        self.emit(TokenKind::Variable);
        Ok(())
    }

    fn lex_number(&mut self) -> Result<()> {
        // Accept optional sign
        if self.peek() == Some('+') || self.peek() == Some('-') {
            self.next_char();
        }

        // Check for base prefixes: 0x, 0o, 0b
        if self.peek() == Some('0') {
            self.next_char();
            match self.peek() {
                Some('x' | 'X') => {
                    self.next_char();
                    return self.lex_hex_number();
                }
                Some('o' | 'O') => {
                    self.next_char();
                    return self.lex_base_number(8);
                }
                Some('b' | 'B') => {
                    self.next_char();
                    return self.lex_base_number(2);
                }
                _ => {
                    // Check for legacy octal: 0 followed only by [0-7_]
                    // (e.g., 0377 → 255). If a dot, 'e'/'E', or digit 8-9
                    // follows, fall through to decimal instead.
                    let bytes = self.input.as_bytes();
                    let mut look = self.pos;
                    let mut has_octal_digits = false;
                    let mut is_legacy_octal = true;
                    while look < bytes.len() {
                        let b = bytes[look];
                        if (b'0'..=b'7').contains(&b) {
                            has_octal_digits = true;
                            look += 1;
                        } else if b == b'_' {
                            look += 1;
                        } else if matches!(b, b'.' | b'e' | b'E' | b'8' | b'9') {
                            is_legacy_octal = false;
                            break;
                        } else {
                            break;
                        }
                    }
                    if is_legacy_octal && has_octal_digits {
                        while self
                            .peek()
                            .is_some_and(|c| ('0'..='7').contains(&c) || c == '_')
                        {
                            self.next_char();
                        }
                        let raw = self.current_str();
                        let clean = strip_underscores(raw);
                        let (negative, digits) = if let Some(d) = clean.strip_prefix("-0") {
                            (true, d)
                        } else if let Some(d) = clean.strip_prefix("+0") {
                            (false, d)
                        } else if let Some(d) = clean.strip_prefix('0') {
                            (false, d)
                        } else {
                            (false, clean.as_ref())
                        };
                        match i64::from_str_radix(digits, 8) {
                            Ok(n) => {
                                let val = if negative { -n } else { n };
                                self.emit_num(TokenKind::Number, Number::Int(val));
                            }
                            Err(e) => return Err(self.error(int_parse_msg("octal", &e))),
                        }
                        return Ok(());
                    }
                    // Otherwise fall through to lex_decimal_number
                }
            }
        }

        self.lex_decimal_number()
    }

    fn lex_hex_number(&mut self) -> Result<()> {
        let mut has_digits = false;
        while let Some(ch) = self.peek() {
            if ch.is_ascii_hexdigit() {
                has_digits = true;
                self.next_char();
            } else if ch == '_' {
                self.next_char(); // skip digit separator
            } else if ch == '.' || ch == 'p' || ch == 'P' {
                // Hex float: 0x1.Fp10
                return self.lex_hex_float(has_digits);
            } else {
                break;
            }
        }
        if !has_digits {
            return Err(self.error("invalid hex number"));
        }
        // Emit as number but we need to convert to decimal for the value
        let raw = self.current_str();
        let clean = strip_underscores(raw);
        // Parse hex int
        let negative = clean.starts_with('-');
        let hex_str = if negative {
            clean
                .trim_start_matches('-')
                .trim_start_matches('+')
                .trim_start_matches("0x")
                .trim_start_matches("0X")
        } else {
            clean
                .trim_start_matches('+')
                .trim_start_matches("0x")
                .trim_start_matches("0X")
        };
        match i64::from_str_radix(hex_str, 16) {
            Ok(n) => {
                let val = if negative { -n } else { n };
                self.emit_num(TokenKind::Number, Number::Int(val));
            }
            Err(e) => return Err(self.error(int_parse_msg("hex", &e))),
        }
        Ok(())
    }

    fn lex_hex_float(&mut self, _had_digits: bool) -> Result<()> {
        // Consume hex float: digits, dot, hex digits, p/P, optional sign, decimal digits
        if self.peek() == Some('.') {
            self.next_char();
            while let Some(ch) = self.peek() {
                if ch.is_ascii_hexdigit() || ch == '_' {
                    self.next_char();
                } else {
                    break;
                }
            }
        }
        if self.peek() == Some('p') || self.peek() == Some('P') {
            self.next_char();
            if self.peek() == Some('+') || self.peek() == Some('-') {
                self.next_char();
            }
            while let Some(ch) = self.peek() {
                if ch.is_ascii_digit() || ch == '_' {
                    self.next_char();
                } else {
                    break;
                }
            }
        }
        // For hex floats, just emit the raw value. Rust can parse them with special handling
        let raw = self.current_str();
        let clean = strip_underscores(raw);
        // Parse hex float manually: use the format 0xHEX.HEXpEXP
        match crate::go::parse_hex_float(&clean) {
            Some(f) => self.emit_num(TokenKind::Number, Number::Float(f)),
            None => return Err(self.error("invalid hex float")),
        }
        Ok(())
    }

    fn lex_base_number(&mut self, base: u32) -> Result<()> {
        let mut has_digits = false;
        while let Some(ch) = self.peek() {
            let valid = match base {
                2 => ch == '0' || ch == '1',
                8 => ('0'..='7').contains(&ch),
                _ => false,
            };
            if valid {
                has_digits = true;
                self.next_char();
            } else if ch == '_' {
                self.next_char();
            } else {
                break;
            }
        }
        if !has_digits {
            return Err(self.error(format!("invalid base-{} number", base)));
        }
        let raw = self.current_str();
        let clean = strip_underscores(raw);
        let negative = clean.starts_with('-');
        let prefix_len = if negative { 3 } else { 2 }; // skip sign + 0x/0o/0b
        let digits = &clean[prefix_len..];
        let kind = match base {
            2 => "binary",
            8 => "octal",
            _ => "integer",
        };
        match i64::from_str_radix(digits, base) {
            Ok(n) => {
                let val = if negative { -n } else { n };
                self.emit_num(TokenKind::Number, Number::Int(val));
            }
            Err(e) => return Err(self.error(int_parse_msg(kind, &e))),
        }
        Ok(())
    }

    fn lex_decimal_number(&mut self) -> Result<()> {
        let mut has_dot = false;
        let mut has_exp = false;
        let mut has_digits = self
            .input
            .as_bytes()
            .get(self.pos.saturating_sub(1))
            .is_some_and(u8::is_ascii_digit);

        while let Some(ch) = self.peek() {
            if ch.is_ascii_digit() {
                has_digits = true;
                self.next_char();
            } else if ch == '_' {
                // Digit separator
                self.next_char();
            } else if ch == '.' && !has_dot && !has_exp {
                has_dot = true;
                self.next_char();
            } else if (ch == 'e' || ch == 'E') && !has_exp {
                has_exp = true;
                self.next_char();
                if self.peek() == Some('+') || self.peek() == Some('-') {
                    self.next_char();
                }
            } else {
                break;
            }
        }

        if !has_digits {
            return Err(self.error("invalid number"));
        }

        let raw = self.current_str();
        // Strip underscores for parsing; borrow when the literal has none.
        let clean = strip_underscores(raw);
        let num = if has_dot || has_exp {
            clean
                .parse::<f64>()
                .map(Number::Float)
                .map_err(|_| self.error("invalid number"))?
        } else {
            clean
                .parse::<i64>()
                .map(Number::Int)
                .map_err(|e| self.error(int_parse_msg("decimal", &e)))?
        };
        // Emit with underscore-stripped val so existing consumers (error
        // messages, tests) see a clean decimal string; the parser reads `num`.
        self.tokens.push(Token {
            kind: TokenKind::Number,
            val: clean,
            num: Some(num),
            pos: self.start,
            line: self.line,
        });
        self.start = self.pos;
        Ok(())
    }

    fn lex_char_literal(&mut self) -> Result<()> {
        self.next_char(); // consume opening '
        let ch = match self.next_char() {
            Some('\\') => {
                // Escaped character
                match self.next_char() {
                    Some('n') => '\n',
                    Some('t') => '\t',
                    Some('r') => '\r',
                    Some('\\') => '\\',
                    Some('\'') => '\'',
                    Some('0') => '\0',
                    Some('a') => '\x07', // bell
                    Some('b') => '\x08', // backspace
                    Some('f') => '\x0C', // form feed
                    Some('v') => '\x0B', // vertical tab
                    Some('x') => {
                        // \xHH
                        let mut hex = String::new();
                        for _ in 0..2 {
                            match self.next_char() {
                                Some(c) if c.is_ascii_hexdigit() => hex.push(c),
                                _ => return Err(self.error("invalid hex escape in char literal")),
                            }
                        }
                        #[allow(
                            clippy::unwrap_used,
                            reason = "hex is 2 validated ASCII hex digits, always parses"
                        )]
                        let n = u32::from_str_radix(&hex, 16).unwrap();
                        char::from_u32(n).unwrap_or('\0')
                    }
                    Some('u') => {
                        // \uHHHH
                        let mut hex = String::new();
                        for _ in 0..4 {
                            match self.next_char() {
                                Some(c) if c.is_ascii_hexdigit() => hex.push(c),
                                _ => {
                                    return Err(
                                        self.error("invalid unicode escape in char literal")
                                    );
                                }
                            }
                        }
                        #[allow(
                            clippy::unwrap_used,
                            reason = "hex is 4 validated ASCII hex digits, always parses"
                        )]
                        let n = u32::from_str_radix(&hex, 16).unwrap();
                        char::from_u32(n).unwrap_or('\0')
                    }
                    Some('U') => {
                        // \UHHHHHHHH
                        let mut hex = String::new();
                        for _ in 0..8 {
                            match self.next_char() {
                                Some(c) if c.is_ascii_hexdigit() => hex.push(c),
                                _ => {
                                    return Err(
                                        self.error("invalid unicode escape in char literal")
                                    );
                                }
                            }
                        }
                        #[allow(
                            clippy::unwrap_used,
                            reason = "hex is 8 validated ASCII hex digits, always parses"
                        )]
                        let n = u32::from_str_radix(&hex, 16).unwrap();
                        char::from_u32(n).unwrap_or('\0')
                    }
                    Some(c) if c.is_ascii_digit() => {
                        // Octal: \NNN
                        let mut oct = String::new();
                        oct.push(c);
                        for _ in 0..2 {
                            match self.peek() {
                                Some(c) if c.is_ascii_digit() => {
                                    oct.push(c);
                                    self.next_char();
                                }
                                _ => break,
                            }
                        }
                        char::from_u32(u32::from_str_radix(&oct, 8).unwrap_or(0)).unwrap_or('\0')
                    }
                    Some(c) => c,
                    None => return Err(self.error("unterminated character literal")),
                }
            }
            Some(c) => c,
            None => return Err(self.error("unterminated character literal")),
        };

        if self.next_char() != Some('\'') {
            return Err(self.error("unterminated character literal"));
        }

        // Emit as a number (Go treats char constants as their Unicode code point)
        self.emit_num(TokenKind::Char, Number::Int(i64::from(ch as u32)));
        Ok(())
    }

    fn lex_identifier(&mut self) -> Result<()> {
        while let Some(ch) = self.peek() {
            if ch.is_alphanumeric() || ch == '_' {
                self.next_char();
            } else {
                break;
            }
        }
        let kind = match self.current_str() {
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "end" => TokenKind::End,
            "range" => TokenKind::Range,
            "with" => TokenKind::With,
            "define" => TokenKind::Define,
            "template" => TokenKind::Template,
            "block" => TokenKind::Block,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "true" | "false" => TokenKind::Bool,
            "nil" => TokenKind::Nil,
            _ => TokenKind::Identifier,
        };
        self.emit(kind);
        Ok(())
    }
}

// String escape processing
fn unescape(s: &str) -> Result<String> {
    let mut result = String::new();
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.next() {
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some('r') => result.push('\r'),
                Some('\\') => result.push('\\'),
                Some('"') => result.push('"'),
                Some('\'') => result.push('\''),
                Some('0') => result.push('\0'),
                Some('a') => result.push('\x07'),
                Some('b') => result.push('\x08'),
                Some('f') => result.push('\x0C'),
                Some('v') => result.push('\x0B'),
                Some('x') => {
                    let hex: String = chars.by_ref().take(2).collect();
                    if let Ok(n) = u32::from_str_radix(&hex, 16)
                        && let Some(c) = char::from_u32(n)
                    {
                        result.push(c);
                    }
                }
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(n) = u32::from_str_radix(&hex, 16)
                        && let Some(c) = char::from_u32(n)
                    {
                        result.push(c);
                    }
                }
                Some('U') => {
                    let hex: String = chars.by_ref().take(8).collect();
                    if let Ok(n) = u32::from_str_radix(&hex, 16)
                        && let Some(c) = char::from_u32(n)
                    {
                        result.push(c);
                    }
                }
                Some(c) => {
                    result.push('\\');
                    result.push(c);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(ch);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn lex(input: &str) -> Vec<Token<'_>> {
        Lexer::new(input, "{{", "}}").tokenize().unwrap()
    }

    fn kinds(tokens: &[Token<'_>]) -> Vec<TokenKind> {
        tokens.iter().map(|t| t.kind.clone()).collect()
    }

    #[test]
    fn test_plain_text() {
        let tokens = lex("hello world");
        assert_eq!(kinds(&tokens), vec![TokenKind::Text, TokenKind::Eof]);
        assert_eq!(tokens[0].val, "hello world");
    }

    #[test]
    fn test_simple_action() {
        let tokens = lex("{{.Name}}");
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::LeftDelim,
                TokenKind::Field,
                TokenKind::RightDelim,
                TokenKind::Eof,
            ]
        );
        assert_eq!(tokens[1].val, ".Name");
    }

    #[test]
    fn test_text_and_action() {
        let tokens = lex("Hello, {{.Name}}!");
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::Text,
                TokenKind::LeftDelim,
                TokenKind::Field,
                TokenKind::RightDelim,
                TokenKind::Text,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_pipeline() {
        let tokens = lex("{{.Name | printf \"%s\"}}");
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::LeftDelim,
                TokenKind::Field,
                TokenKind::Pipe,
                TokenKind::Identifier,
                TokenKind::String,
                TokenKind::RightDelim,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_if_else_end() {
        let tokens = lex("{{if .OK}}yes{{else}}no{{end}}");
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::LeftDelim,
                TokenKind::If,
                TokenKind::Field,
                TokenKind::RightDelim,
                TokenKind::Text,
                TokenKind::LeftDelim,
                TokenKind::Else,
                TokenKind::RightDelim,
                TokenKind::Text,
                TokenKind::LeftDelim,
                TokenKind::End,
                TokenKind::RightDelim,
                TokenKind::Eof,
            ]
        );
    }

    #[test]
    fn test_trim_whitespace() {
        let tokens = lex("  {{- .X -}}  ");
        // Left trim: preceding whitespace should be removed (no Text token before)
        // Right trim: following whitespace should be removed (no Text token after)
        assert!(tokens.iter().any(|t| t.kind == TokenKind::LeftTrimDelim));
        assert!(tokens.iter().any(|t| t.kind == TokenKind::RightTrimDelim));
        // No text tokens should remain (whitespace on both sides trimmed)
        assert!(!tokens.iter().any(|t| t.kind == TokenKind::Text));
    }

    #[test]
    fn test_left_trim_only() {
        let tokens = lex("  hello  {{- .X }}  ");
        // "  hello" should have trailing whitespace trimmed to "  hello"
        let text_tokens: Vec<&Token> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Text)
            .collect();
        assert_eq!(text_tokens.len(), 2);
        assert_eq!(text_tokens[0].val, "  hello"); // trailing spaces trimmed
        assert_eq!(text_tokens[1].val, "  "); // following text NOT trimmed
    }

    #[test]
    fn test_right_trim_only() {
        let tokens = lex("  {{.X -}}  hello  ");
        // Following "  hello  " should have leading whitespace trimmed
        let text_tokens: Vec<&Token> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Text)
            .collect();
        assert_eq!(text_tokens.len(), 2);
        assert_eq!(text_tokens[0].val, "  "); // preceding text NOT trimmed
        assert_eq!(text_tokens[1].val, "hello  "); // leading spaces trimmed
    }

    #[test]
    fn test_variable_and_declare() {
        let tokens = lex("{{$x := .Name}}");
        assert_eq!(
            kinds(&tokens),
            vec![
                TokenKind::LeftDelim,
                TokenKind::Variable,
                TokenKind::Declare,
                TokenKind::Field,
                TokenKind::RightDelim,
                TokenKind::Eof,
            ]
        );
        assert_eq!(tokens[1].val, "$x");
    }

    #[test]
    fn test_numbers() {
        let tokens = lex("{{42}}");
        assert_eq!(tokens[1].kind, TokenKind::Number);
        assert_eq!(tokens[1].val, "42");
        assert_eq!(tokens[1].num, Some(Number::Int(42)));

        let tokens = lex("{{2.5}}");
        assert_eq!(tokens[1].kind, TokenKind::Number);
        assert_eq!(tokens[1].val, "2.5");
        assert!(matches!(tokens[1].num, Some(Number::Float(f)) if (f - 2.5).abs() < 1e-9));
    }

    #[test]
    fn test_hex_number() {
        let tokens = lex("{{0xFF}}");
        assert_eq!(tokens[1].kind, TokenKind::Number);
        assert_eq!(tokens[1].num, Some(Number::Int(255)));
    }

    #[test]
    fn test_octal_number() {
        let tokens = lex("{{0o77}}");
        assert_eq!(tokens[1].kind, TokenKind::Number);
        assert_eq!(tokens[1].num, Some(Number::Int(63)));
    }

    #[test]
    fn test_binary_number() {
        let tokens = lex("{{0b1010}}");
        assert_eq!(tokens[1].kind, TokenKind::Number);
        assert_eq!(tokens[1].num, Some(Number::Int(10)));
    }

    #[test]
    fn test_underscore_separator() {
        let tokens = lex("{{1_000_000}}");
        assert_eq!(tokens[1].kind, TokenKind::Number);
        assert_eq!(tokens[1].val, "1000000");
        assert_eq!(tokens[1].num, Some(Number::Int(1_000_000)));
    }

    #[test]
    fn test_hex_float_prefilled_num() {
        let tokens = lex("{{0x1.ep+2}}");
        assert_eq!(tokens[1].kind, TokenKind::Number);
        assert!(matches!(tokens[1].num, Some(Number::Float(f)) if (f - 7.5).abs() < 1e-9));
    }

    #[test]
    fn test_decimal_float_underscore_prefilled_num() {
        let tokens = lex("{{1_234.5}}");
        assert_eq!(tokens[1].kind, TokenKind::Number);
        // val is the underscore-stripped string; num carries the parsed float.
        assert_eq!(tokens[1].val, "1234.5");
        assert!(matches!(tokens[1].num, Some(Number::Float(f)) if (f - 1234.5).abs() < 1e-9));
    }

    #[test]
    fn test_non_numeric_tokens_have_no_num() {
        let tokens = lex("{{ .Field }}");
        for tok in &tokens {
            if !matches!(tok.kind, TokenKind::Number | TokenKind::Char) {
                assert!(tok.num.is_none(), "{:?} should not carry num", tok.kind);
            }
        }
    }

    #[test]
    fn test_comment() {
        let tokens = lex("hello{{/* a comment */}}world");
        // Comment should be completely removed; only text remains
        let text_tokens: Vec<&Token> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Text)
            .collect();
        assert_eq!(text_tokens.len(), 2);
        assert_eq!(text_tokens[0].val, "hello");
        assert_eq!(text_tokens[1].val, "world");
    }

    #[test]
    fn test_comment_with_trim() {
        let tokens = lex("hello  {{- /* a comment */ -}}  world");
        let text_tokens: Vec<&Token> = tokens
            .iter()
            .filter(|t| t.kind == TokenKind::Text)
            .collect();
        assert_eq!(text_tokens.len(), 2);
        assert_eq!(text_tokens[0].val, "hello");
        assert_eq!(text_tokens[1].val, "world");
    }

    #[test]
    fn test_break_continue_keywords() {
        let tokens = lex("{{break}}");
        assert_eq!(tokens[1].kind, TokenKind::Break);

        let tokens = lex("{{continue}}");
        assert_eq!(tokens[1].kind, TokenKind::Continue);
    }

    #[test]
    fn test_char_literal_escape() {
        let tokens = lex("{{'\\n'}}");
        assert_eq!(tokens[1].kind, TokenKind::Char);
        // '\n' is the char value of newline = 10
        assert_eq!(tokens[1].num, Some(Number::Int(10)));

        let tokens = lex("{{'a'}}");
        assert_eq!(tokens[1].kind, TokenKind::Char);
        assert_eq!(tokens[1].num, Some(Number::Int(97)));
    }
}
