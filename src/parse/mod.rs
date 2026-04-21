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
