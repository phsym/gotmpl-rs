//! Abstract Syntax Tree (AST) node types for the template language.
//!
//! The AST is produced by the [`Parser`](super::Parser) and consumed by
//! the executor. In Go's implementation, nodes use an interface with a tree
//! of concrete types. In Rust, we use enums, the natural way to represent
//! sum types.
//!
//! The top-level enum is [`Node`], with expression-level atoms in [`Expr`].

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

/// Byte-level position in the template source, used for error reporting.
///
/// Carried by every AST node so that execution errors can point back to
/// the originating source location.
#[derive(Debug, Clone, Copy)]
pub struct Pos {
    /// Byte offset from the start of the template source.
    pub offset: usize,
    /// 1-based line number (tracked by the lexer).
    pub line: usize,
}

impl Pos {
    /// Create a new source position.
    pub fn new(offset: usize, line: usize) -> Self {
        Pos { offset, line }
    }
}

/// A top-level AST node produced by the parser.
///
/// Each variant corresponds to a syntactic construct in the Go template language.
/// The executor walks a tree of these nodes to produce output.
#[derive(Debug, Clone)]
pub enum Node {
    /// A sequence of nodes (the body of a template or control-flow branch).
    ///
    /// Corresponds to Go's `parse.ListNode`.
    List(ListNode),

    /// Raw text to emit verbatim (everything outside `{{ }}`).
    Text(TextNode),

    /// A `{{ pipeline }}` action that evaluates a [`PipeNode`] and prints the result.
    ///
    /// If the pipeline contains variable declarations (`$x := ...`), the result
    /// is assigned instead of printed.
    Action(ActionNode),

    /// `{{if pipeline}}...{{end}}`: conditional execution.
    If(IfNode),

    /// `{{range pipeline}}...{{end}}`: iteration over a list, map, or integer.
    Range(RangeNode),

    /// `{{with pipeline}}...{{end}}`: sets dot to the pipeline value if truthy.
    With(WithNode),

    /// `{{template "name" pipeline}}`: invokes a named template.
    Template(TemplateNode),

    /// `{{define "name"}}...{{end}}`: defines a named template.
    ///
    /// Collected at parse time and stored separately from the main AST.
    Define(DefineNode),

    /// `{{break}}`: exits the innermost `{{range}}` loop.
    Break(Pos),

    /// `{{continue}}`: skips to the next iteration of the innermost `{{range}}` loop.
    Continue(Pos),
}

/// A sequence of [`Node`]s, the body of a template or a branch.
#[derive(Debug, Clone)]
pub struct ListNode {
    /// Source position of the first node in the list.
    pub pos: Pos,
    /// The ordered sequence of child nodes.
    pub nodes: Vec<Node>,
}

/// Raw text outside delimiters, emitted verbatim during execution.
#[derive(Debug, Clone)]
pub struct TextNode {
    /// Source position of the text.
    pub pos: Pos,
    /// The literal text content (after any whitespace trimming by the lexer).
    ///
    /// Stored as [`Arc<str>`] so AST clones (e.g., when `{{block}}` duplicates
    /// its body into the template definition map) only bump a refcount.
    pub text: Arc<str>,
}

/// A `{{ pipeline }}` action that evaluates and prints.
///
/// If [`PipeNode::decl`] is non-empty, the result is assigned to those variables
/// instead of being printed.
#[derive(Debug, Clone)]
pub struct ActionNode {
    /// Source position of the opening delimiter.
    pub pos: Pos,
    /// The pipeline to evaluate.
    pub pipe: PipeNode,
}

/// A pipeline: one or more [`CommandNode`]s separated by `|`.
///
/// Optionally declares or assigns variables:
/// - `$x := pipeline`: declare with `:=`
/// - `$x = pipeline`: assign with `=`
/// - `$i, $v := range .Items`: multiple declarations in range
/// - `$i, $v = range .Items`: multiple assignments in range
///
/// When piped, each command's result becomes the **last** argument of the
/// next command (matching Go's convention).
#[derive(Debug, Clone)]
pub struct PipeNode {
    /// Source position.
    pub pos: Pos,
    /// Variable names being declared or assigned (e.g., `["$x"]` or `["$i", "$v"]`).
    ///
    /// Empty when the pipeline has no variable binding.
    pub decl: Vec<Arc<str>>,
    /// The sequence of commands in the pipeline, left to right.
    pub commands: Vec<CommandNode>,
    /// `true` for assignment (`=`), `false` for declaration (`:=`).
    ///
    /// Only meaningful when [`decl`](Self::decl) is non-empty.
    pub is_assign: bool,
}

/// A single command in a pipeline, either a function call or a bare value.
///
/// The first element of [`args`](Self::args) determines the command type:
/// - [`Expr::Identifier`]: a function call; remaining args are its arguments.
/// - Any other [`Expr`]: a bare value (only valid as the first or sole command).
#[derive(Debug, Clone)]
pub struct CommandNode {
    /// Source position.
    pub pos: Pos,
    /// The arguments/operands of this command.
    pub args: Vec<Expr>,
}

/// A parsed numeric literal — either an integer or a floating-point value.
///
/// Produced by the parser from `Number` and `Char` tokens. Character
/// literals are stored as `Int` holding the Unicode code point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Number {
    /// An integer literal (decimal, hex, octal, binary, or char code point).
    Int(i64),
    /// A floating-point literal (decimal float or hex float).
    Float(f64),
}

/// An expression, the atomic building blocks of commands.
///
/// Each variant carries a [`Pos`] for error reporting.
#[derive(Debug, Clone)]
pub enum Expr {
    /// The dot cursor (`.`), refers to the current context value.
    Dot(Pos),

    /// Field access on dot: `.Name`, `.User.Email`.
    ///
    /// The vector contains the chain of field names (e.g., `["User", "Email"]`).
    Field(Pos, Vec<Arc<str>>),

    /// Variable access: `$x`, or chained: `$x.Name`.
    ///
    /// Fields: `(pos, variable_name, field_chain)`.
    Variable(Pos, Arc<str>, Vec<Arc<str>>),

    /// A function or method identifier (e.g., `printf`, `len`).
    ///
    /// Resolved to a [`ValueFunc`](crate::ValueFunc) during execution.
    Identifier(Pos, Arc<str>),

    /// A string literal (`"hello"` or `` `raw` ``).
    ///
    /// Stored as [`Arc<str>`] so execution can cheaply clone it into
    /// [`Value::String`](crate::Value::String) via a refcount bump.
    String(Pos, Arc<str>),

    /// A numeric literal, already parsed to an `i64` or `f64`.
    ///
    /// Hex, octal, binary, decimal, and character literals are all normalized
    /// to this form at parse time. The executor consumes the parsed value
    /// directly, avoiding a second `str::parse` on every evaluation.
    Number(Pos, Number),

    /// A boolean literal (`true` or `false`).
    Bool(Pos, bool),

    /// The `nil` literal.
    Nil(Pos),

    /// A parenthesized sub-pipeline used as an argument: `(pipeline)`.
    Pipe(Pos, PipeNode),

    /// Chained field access on an expression result: `(expr).Field` or `func.Field`.
    ///
    /// Fields: `(pos, inner_expression, field_chain)`.
    Chain(Pos, Box<Expr>, Vec<Arc<str>>),
}

impl Expr {
    /// Returns the source position of this expression.
    pub fn pos(&self) -> Pos {
        match self {
            Expr::Dot(p)
            | Expr::Field(p, _)
            | Expr::Variable(p, _, _)
            | Expr::Identifier(p, _)
            | Expr::String(p, _)
            | Expr::Number(p, _)
            | Expr::Bool(p, _)
            | Expr::Nil(p)
            | Expr::Pipe(p, _)
            | Expr::Chain(p, _, _) => *p,
        }
    }
}

/// Shared structure for `{{if}}`, `{{with}}`, and `{{range}}` nodes.
///
/// All three have the same shape: a condition/value pipeline, a body to execute
/// when the condition is truthy (or for each iteration), and an optional else branch.
///
/// The type aliases [`IfNode`], [`WithNode`], and [`RangeNode`] all resolve to this type.
#[derive(Debug, Clone)]
pub struct BranchNode {
    /// Source position of the keyword (`if`, `with`, or `range`).
    pub pos: Pos,
    /// The condition or iteration pipeline.
    ///
    /// For `range`, this pipeline's [`decl`](PipeNode::decl) may contain loop
    /// variable names (`$i`, `$v`).
    pub pipe: PipeNode,
    /// The body executed when the condition is truthy (or for each iteration).
    pub body: ListNode,
    /// The optional `{{else}}` branch.
    pub else_body: Option<ListNode>,
}

/// Type alias for an `{{if}}` node. See [`BranchNode`] for fields.
pub type IfNode = BranchNode;

/// Type alias for a `{{with}}` node. See [`BranchNode`] for fields.
///
/// Unlike `if`, `with` sets dot to the pipeline result inside the body.
pub type WithNode = BranchNode;

/// Type alias for a `{{range}}` node. See [`BranchNode`] for fields.
///
/// The pipeline's [`decl`](PipeNode::decl) may contain one or two variable names
/// for the loop index and value (e.g., `$i, $v := range .Items` or
/// `$i, $v = range .Items` for assignment to existing variables).
pub type RangeNode = BranchNode;

/// A `{{template "name" pipeline}}` invocation node.
#[derive(Debug, Clone)]
pub struct TemplateNode {
    /// Source position of the `template` keyword.
    pub pos: Pos,
    /// The name of the template to invoke (from a `{{define}}` or `{{block}}`).
    pub name: Arc<str>,
    /// Optional pipeline whose result becomes dot inside the invoked template.
    ///
    /// `None` when invoked without arguments: `{{template "name"}}`.
    pub pipe: Option<PipeNode>,
}

/// A `{{define "name"}}...{{end}}` template definition.
///
/// Collected by the parser and stored in the [`Template`](crate::Template)'s
/// definition map, keyed by [`name`](Self::name).
#[derive(Debug, Clone)]
pub struct DefineNode {
    /// Source position of the `define` keyword.
    pub pos: Pos,
    /// The name of the defined template.
    pub name: Arc<str>,
    /// The body of the defined template.
    pub body: ListNode,
}
