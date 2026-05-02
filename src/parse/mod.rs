//! Parser, lexer, and AST node types for the Go template language.
//!
//! Mirrors Go's `text/template/parse` package:
//!
//! - [`Parser`](crate::parse::Parser): recursive-descent parser that turns
//!   template source into an AST.
//! - [`Node`](crate::parse::Node), [`Expr`](crate::parse::Expr), and
//!   supporting structs: the AST types.
//!
//! Most users go through [`Template::parse`](crate::Template::parse) rather
//! than touching these types directly. The AST types are public for advanced
//! use cases like
//! [`Template::add_parse_tree`](crate::Template::add_parse_tree).

pub(crate) mod lexer;
pub(crate) mod node;
mod parser;

// Re-export AST node types
pub use node::{
    ActionNode, BranchNode, CommandNode, DefineNode, Expr, IfNode, ListNode, Node, Number,
    PipeNode, Pos, RangeNode, TemplateNode, TextNode, WithNode, is_empty_tree,
};

// Re-export the parser
pub use parser::Parser;

/// Short-string type used in AST identifier and name fields
/// ([`Expr::Identifier`], [`Expr::Field`], [`PipeNode::decl`],
/// [`TemplateNode::name`], [`DefineNode::name`], etc.).
///
/// Inlines payloads up to 22 bytes (typical for field/variable/template
/// names) and falls back to a refcounted heap pointer for longer ones.
/// Re-exported so callers building AST nodes by hand don't need to depend
/// on `smol_str` directly.
pub use smol_str::SmolStr;
