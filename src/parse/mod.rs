//! Parser, lexer, and AST node types for the Go template language.
//!
//! This module mirrors Go's `text/template/parse` package, grouping together:
//!
//! - [`Parser`](crate::parse::Parser): recursive-descent parser that converts template source into an AST
//! - [`Node`](crate::parse::Node), [`Expr`](crate::parse::Expr), and supporting structs: the AST types
//!
//! Most users interact with [`Template::parse`](crate::Template::parse) rather than
//! using these types directly. The AST types are public for advanced use cases
//! like [`Template::add_parse_tree`](crate::Template::add_parse_tree).

pub(crate) mod lexer;
pub(crate) mod node;
mod parser;

// Re-export AST node types
pub use node::{
    ActionNode, BranchNode, CommandNode, DefineNode, Expr, IfNode, ListNode, Node, Number,
    PipeNode, Pos, RangeNode, TemplateNode, TextNode, WithNode,
};

// Re-export the parser
pub use parser::Parser;
