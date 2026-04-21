//! Error types for the template engine.
//!
//! All fallible operations in this crate return [`Result<T>`], which is an alias
//! for `std::result::Result<T, TemplateError>`.
//!
//! Errors are split into phases (lexing, parsing, and execution) so callers
//! can match on the variant to provide targeted diagnostics.

use alloc::format;
use alloc::string::String;
use thiserror::Error;

/// Shared formatter for source-location errors (parse + lex). The format is
/// Go-compatible when a name is present (`template: foo.tmpl:12:5: msg`) and
/// drops the name segment when absent, keeping the structure parallel.
fn fmt_src_err(name: &Option<String>, line: usize, col: usize, message: &str) -> String {
    match name {
        Some(n) => format!("template: {n}:{line}:{col}: {message}"),
        None => format!("template: {line}:{col}: {message}"),
    }
}

/// The error type returned by all template operations.
///
/// Variants map to the phase where the error originated:
///
/// | Phase | Variants |
/// |-------|----------|
/// | Lexing | [`Lex`](Self::Lex) |
/// | Parsing | [`Parse`](Self::Parse) |
/// | Execution | many: see below |
/// | I/O | [`Io`](Self::Io), [`ReadFile`](Self::ReadFile) |
///
/// Execution errors include [`Exec`](Self::Exec) as a catch-all string
/// variant for rare cases; prefer pattern-matching on the structured
/// variants where available.
///
/// Marked `#[non_exhaustive]`: future versions may add variants without a
/// major bump.
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
#[non_exhaustive]
pub enum TemplateError {
    /// A syntax error found during parsing, with source location.
    ///
    /// The optional `name` tags the template's origin (e.g. the file name
    /// when parsing via [`parse_files`](crate::Template::parse_files)) and
    /// is prefixed Go-style in the `Display` output
    /// (`template: <name>:<line>:<col>: <message>`).
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
    /// Shares the `Parse` variant's shape and `Display` format — the message
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

    /// An I/O error occurred while writing template output.
    #[cfg(feature = "std")]
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A formatting/write error occurred while writing template output.
    #[error("write error")]
    Write,
}

impl From<core::fmt::Error> for TemplateError {
    fn from(_: core::fmt::Error) -> Self {
        TemplateError::Write
    }
}

/// Alias for `Result<T, TemplateError>`.
///
/// This is the return type of all fallible operations in this crate.
pub type Result<T> = core::result::Result<T, TemplateError>;
