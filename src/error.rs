//! Error types for the template engine.
//!
//! All fallible operations return [`Result<T>`], an alias for
//! `std::result::Result<T, TemplateError>`. Variants are split by phase
//! (lexing, parsing, execution) so callers can pattern-match for targeted
//! diagnostics.

use alloc::format;
use alloc::string::String;
use thiserror::Error;

/// Shared formatter for source-location errors (parse and lex). With a name,
/// the format matches Go: `template: foo.tmpl:12:5: msg`. Without a name, the
/// name segment is dropped but the rest of the format stays the same.
fn fmt_src_err(name: &Option<String>, line: usize, col: usize, message: &str) -> String {
    match name {
        Some(n) => format!("template: {n}:{line}:{col}: {message}"),
        None => format!("template: {line}:{col}: {message}"),
    }
}

/// Formatter for context-aware escaping errors, mirroring Go's
/// `html/template` `Error.Error()`. Go emits one of three shapes depending on
/// how much position it captured:
/// - `html/template:<name>:<line>:<col>: <description>` for errors that carry a
///   parse node (a `{{...}}` action, branch, or `{{template}}` call), via Go's
///   `Tree.ErrorContext`;
/// - `html/template:<name>:<line>: <description>` when only a line is known;
/// - `html/template:<name>: <description>` when no position is known — which is
///   what Go itself produces for errors raised inside the text/tag transition
///   machine (e.g. `ErrBadHTML`, `ErrEndContext`), since those carry no node.
///
/// Documented divergence: this port reproduces the line but **not** the column.
/// The escaping pass works over parsed trees with the source text no longer in
/// hand, and reconstructing Go's byte column would require both retaining the
/// source and matching Go's exact node-position convention. A `line == 0`
/// therefore renders as the position-less form, faithfully matching Go for the
/// transition-machine errors; the node-carrying errors match Go up to the
/// missing `:<col>`.
#[cfg(feature = "html")]
fn fmt_escape_err(name: &Option<String>, line: usize, description: &str) -> String {
    match (name, line) {
        (Some(n), l) if l != 0 => format!("html/template:{n}:{l}: {description}"),
        (Some(n), _) => format!("html/template:{n}: {description}"),
        (None, l) if l != 0 => format!("html/template::{l}: {description}"),
        (None, _) => format!("html/template: {description}"),
    }
}

/// The code for a context-aware escaping error, mirroring the `ErrorCode`
/// values of Go's `html/template` package (its `error.go`). Available only with
/// the `html` feature.
#[cfg(feature = "html")]
#[cfg_attr(docsrs, doc(cfg(feature = "html")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeErrorCode {
    /// A `{{.}}` appears in an ambiguous context within a URL, e.g.
    /// `<a href="{{if .C}}/foo?a={{else}}/bar/{{end}}{{.X}}">`.
    AmbigContext,
    /// The template produced malformed HTML: a banned rune in a tag or
    /// attribute name, or an unquoted attribute value that cannot be escaped.
    BadHtml,
    /// `{{if}}`, `{{range}}`, or `{{with}}` branches end in different contexts.
    BranchEnd,
    /// The template ended in a non-text context (e.g. an unclosed tag, quote,
    /// or `<script>` element).
    EndContext,
    /// A `{{template}}` action referenced a template that is not defined.
    NoSuchTemplate,
    /// The output context of a (recursively called) template could not be
    /// computed.
    OutputContext,
    /// A JavaScript regexp character set `/foo[a-z/` was left unclosed.
    PartialCharset,
    /// A `{{.}}` interrupted an unfinished escape sequence, e.g. `\{{.X}}`.
    PartialEscape,
    /// A `{{range}}` body re-enters ending in a different context than it began.
    ///
    /// Retained for parity with Go's `ErrRangeLoopReentry` code, but — like Go
    /// — never emitted: this failure surfaces as [`BranchEnd`](Self::BranchEnd)
    /// with an `"on range loop re-entry: …"` description prefix.
    RangeLoopReentry,
    /// A `/` in JavaScript could be a division operator or a regexp start and
    /// the context is ambiguous.
    SlashAmbig,
    /// A predefined escaper (`html`/`urlquery`) was used where it is disallowed.
    PredefinedEscaper,
    /// Deprecated in Go and never emitted: an action inside a JS template
    /// literal (now escaped like any other JS context). Present for parity.
    JsTemplate,
}

/// The error type returned by all template operations.
///
/// Variants group by phase: lexing ([`Lex`](Self::Lex)), parsing
/// ([`Parse`](Self::Parse)), execution (several structured variants, plus
/// [`Exec`](Self::Exec) as a catch-all string for rare cases), and I/O
/// ([`Io`](Self::Io), [`ReadFile`](Self::ReadFile)). Prefer matching on the
/// structured variants when available rather than parsing [`Exec`](Self::Exec)
/// strings.
///
/// # Examples
///
/// ```
/// use gotmpl::Template;
///
/// let result = Template::new("t").parse("{{.X");
/// assert!(result.is_err());
/// let err = result.err().unwrap();
/// assert!(err.to_string().contains("unclosed action"));
/// ```
#[derive(Debug, Error)]
pub enum TemplateError {
    /// A syntax error found during parsing, with source location.
    ///
    /// The optional `name` tags the template's origin (e.g. the file name
    /// when parsing via [`parse_files`](crate::Template::parse_files)). It is
    /// prefixed Go-style in the `Display` output:
    /// `template: <name>:<line>:<col>: <message>`.
    #[error("{}", fmt_src_err(name, *line, *col, message))]
    Parse {
        /// Source of the template (e.g. file name) if known.
        name: Option<String>,
        /// 1-based line number in the template source.
        line: usize,
        /// 1-based column number in the template source.
        col: usize,
        /// Human-readable description of the parse error.
        message: String,
    },

    /// An error found during lexical scanning.
    ///
    /// Shares the `Parse` variant's shape and `Display` format. The message
    /// itself describes the lex-specific failure, so no extra preamble is
    /// added.
    #[error("{}", fmt_src_err(name, *line, *col, message))]
    Lex {
        /// Source of the template (e.g. file name) if known.
        name: Option<String>,
        /// 1-based line number in the template source.
        line: usize,
        /// 1-based column number in the template source.
        col: usize,
        /// Human-readable description of the lex error.
        message: String,
    },

    /// A general execution error (type mismatch, invalid operation, etc.).
    ///
    /// Prefer the structured variants below when they apply.
    #[error("execution error: {0}")]
    Exec(String),

    /// An index or slice bound was outside the sequence it addressed.
    #[error("index out of range: {index}")]
    IndexOutOfRange {
        /// The offending index as supplied (may be negative).
        index: i64,
    },

    /// A value had the wrong type for the operation attempted on it.
    #[error("type mismatch: expected {expected}, got {got}")]
    TypeMismatch {
        /// The type name the operation required (e.g. `"int"`, `"list"`).
        expected: &'static str,
        /// The actual type of the offending value.
        got: &'static str,
    },

    /// A required map key was missing and [`MissingKey::Error`](crate::MissingKey::Error) is set.
    #[error("map has no entry for key: {key}")]
    MissingKey {
        /// The key that was looked up.
        key: String,
    },

    /// Executor recursion depth exceeded.
    ///
    /// Triggered by deeply nested `{{template}}` calls or `{{if}}`/`{{with}}`/
    /// `{{range}}` bodies. The limit is internal and not configurable.
    #[error("recursion limit exceeded")]
    RecursionLimit,

    /// The per-execution `{{range}}` iteration budget was exhausted.
    ///
    /// Configurable via [`Template::max_range_iters`](crate::Template::max_range_iters).
    #[error("range iteration budget exhausted")]
    RangeIterLimit,

    /// A user-registered template function panicked.
    #[cfg(feature = "std")]
    #[error("function {name} panicked: {message}")]
    FuncPanic {
        /// Name of the function that panicked.
        name: String,
        /// Best-effort description of the panic payload.
        message: String,
    },

    /// A `{{template "name"}}` action referenced a template that was never defined.
    #[error("undefined template: {0}")]
    UndefinedTemplate(String),

    /// A template action referenced a function that is not registered.
    ///
    /// Register custom functions with [`Template::func`](crate::Template::func)
    /// before calling [`parse`](crate::Template::parse).
    #[error("undefined function: {0}")]
    UndefinedFunction(String),

    /// A template action referenced a variable that has not been declared.
    #[error("undefined variable: {0}")]
    UndefinedVariable(String),

    /// A function was called with the wrong number of arguments.
    #[error("wrong number of arguments: {name} expects {expected}, got {got}")]
    ArgCount {
        /// Name of the function that was called.
        name: String,
        /// Minimum number of arguments expected.
        expected: usize,
        /// Actual number of arguments provided.
        got: usize,
    },

    /// A `{{range}}` action was applied to a value that is not iterable
    /// ([`Value::List`](crate::value::Value::List), [`Value::Map`](crate::value::Value::Map),
    /// or [`Value::Int`](crate::value::Value::Int)).
    #[error("cannot range over {0}")]
    NotIterable(String),

    /// Failed to read a template file passed to
    /// [`Template::parse_files`](crate::Template::parse_files).
    #[cfg(feature = "std")]
    #[error("failed to read template file {path}: {source}")]
    ReadFile {
        /// The path that failed to open.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },

    /// [`Template::parse_files`](crate::Template::parse_files) was called
    /// with an empty slice of filenames.
    #[cfg(feature = "std")]
    #[error("no files named in call to parse_files")]
    NoFiles,

    /// A glob pattern passed to
    /// [`Template::parse_glob`](crate::Template::parse_glob) was malformed
    /// (e.g. unbalanced `[`).
    #[cfg(feature = "glob")]
    #[error("invalid pattern {pattern:?} at position {pos}: {msg}")]
    BadPattern {
        /// The offending pattern.
        pattern: String,
        /// Approximate character index into `pattern` where the failure was
        /// detected, as reported by the [`glob`] crate.
        pos: usize,
        /// Static description of the failure, as reported by the [`glob`] crate.
        msg: &'static str,
    },

    /// An I/O error occurred while writing template output.
    #[cfg(feature = "std")]
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A formatting/write error occurred while writing template output.
    #[error("write error")]
    Write,

    /// A context-aware escaping error from the `html` feature's escaping pass.
    ///
    /// Raised while escaping a [`html::Template`](crate::html::Template) (lazily,
    /// on first execute) when the template cannot be safely contextualized —
    /// see [`EscapeErrorCode`] for the specific reason. Mirrors the errors from
    /// Go's `html/template`. The rendered message reproduces Go's line but not
    /// its byte column; a `line` of `0` renders without any position, matching
    /// Go's own output for transition-machine errors.
    #[cfg(feature = "html")]
    #[cfg_attr(docsrs, doc(cfg(feature = "html")))]
    #[error("{}", fmt_escape_err(name, *line, description))]
    Escape {
        /// The specific escaping-failure code.
        code: EscapeErrorCode,
        /// Name of the template being escaped, if known.
        name: Option<String>,
        /// 1-based line number in the template source (`0` when unknown, which
        /// — matching Go — renders without any position). The byte column Go
        /// also reports for node-carrying errors is not tracked.
        line: usize,
        /// Human-readable description of the escaping error.
        description: String,
    },
}

impl From<core::fmt::Error> for TemplateError {
    fn from(_: core::fmt::Error) -> Self {
        TemplateError::Write
    }
}

/// Alias for `Result<T, TemplateError>`, the return type of every fallible
/// operation in this crate.
pub type Result<T> = core::result::Result<T, TemplateError>;
