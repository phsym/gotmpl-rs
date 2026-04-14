//! Error types for the template engine.
//!
//! All fallible operations in this crate return [`Result<T>`], which is an alias
//! for `std::result::Result<T, TemplateError>`.
//!
//! Errors are split into phases — lexing, parsing, and execution — so callers
//! can match on the variant to provide targeted diagnostics.

use thiserror::Error;

/// The error type returned by all template operations.
///
/// Variants map to the phase where the error originated:
///
/// | Phase | Variants |
/// |-------|----------|
/// | Lexing | [`Lex`](Self::Lex) |
/// | Parsing | [`Parse`](Self::Parse) |
/// | Execution | [`Exec`](Self::Exec), [`UndefinedTemplate`](Self::UndefinedTemplate), [`UndefinedFunction`](Self::UndefinedFunction), [`UndefinedVariable`](Self::UndefinedVariable), [`ArgCount`](Self::ArgCount), [`NotIterable`](Self::NotIterable) |
/// | I/O | [`Io`](Self::Io) |
///
/// # Examples
///
/// ```
/// use go_template::Template;
///
/// let result = Template::new("t").parse("{{.X");
/// assert!(result.is_err());
/// let err = result.err().unwrap();
/// assert!(err.to_string().contains("unclosed action"));
/// ```
#[derive(Debug, Error)]
pub enum TemplateError {
    /// A syntax error found during parsing, with source location.
    #[error("parse error at line {line}, col {col}: {message}")]
    Parse {
        /// 1-based line number in the template source.
        line: usize,
        /// 1-based column number in the template source.
        col: usize,
        /// Human-readable description of the parse error.
        message: String,
    },

    /// An error found during lexical scanning.
    #[error("lex error at position {pos}: {message}")]
    Lex {
        /// Byte offset in the template source where the error occurred.
        pos: usize,
        /// Human-readable description of the lex error.
        message: String,
    },

    /// A general execution error (type mismatch, invalid operation, etc.).
    #[error("execution error: {0}")]
    Exec(String),

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

    /// An I/O error occurred while writing template output.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Alias for `std::result::Result<T, TemplateError>`.
///
/// This is the return type of all fallible operations in this crate.
pub type Result<T> = std::result::Result<T, TemplateError>;
