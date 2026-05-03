//! Recursive-descent parser that converts a token stream into an AST.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::SmolStr;
use super::lexer::{Lexer, Token, TokenKind};
use super::node::*;
use crate::error::{Result, TemplateError};

/// Parse a decimal-form number token into a [`Number`].
///
/// The lexer already normalizes hex/octal/binary integer literals to decimal
/// and hex floats to their decimal-float form, so this only has to discriminate
/// between integer and float on the final text.
fn parse_number(s: &str) -> Option<Number> {
    if s.contains('.') || s.contains('e') || s.contains('E') {
        s.parse::<f64>().ok().map(Number::Float)
    } else {
        s.parse::<i64>().ok().map(Number::Int)
    }
}

/// Recursion cap for nested `{{if}}`/`{{with}}`/`{{range}}`/`{{block}}`
/// bodies and parenthesised pipelines, to keep pathological input from
/// blowing the thread stack.
const MAX_PARSE_DEPTH: usize = 100;

/// Recursive-descent parser for Go template source.
///
/// Created from a source string via [`new`](Self::new), then invoked via
/// [`parse`](Self::parse) to produce the AST.
///
/// # Examples
///
/// ```
/// use gotmpl::parse::{Parser, Node};
///
/// let parser = Parser::new("Hello, {{.Name}}!", "{{", "}}").unwrap();
/// let (tree, defines) = parser.parse().unwrap();
/// assert_eq!(tree.nodes.len(), 3); // Text, Action, Text
/// assert!(defines.is_empty());
/// ```
pub struct Parser<'a> {
    tokens: Vec<Token<'a>>,
    pos: usize,
    source: &'a str,
    depth: usize,
    parse_name: Option<&'a str>,
}

impl<'a> Parser<'a> {
    /// Create a new parser for the given template source.
    ///
    /// Runs the lexer internally to produce the token stream.
    ///
    /// # Errors
    ///
    /// Returns a [`TemplateError::Lex`] if
    /// the source contains lexical errors (unterminated strings, invalid characters, etc.).
    pub fn new(source: &'a str, left_delim: &'a str, right_delim: &'a str) -> Result<Self> {
        Self::with_name(None, source, left_delim, right_delim)
    }

    /// Like [`new`](Self::new) but tags the parser with a source name (typically
    /// a file path) that is prefixed onto lex and parse error messages, matching
    /// Go's `template: <name>:<line>:<col>: <message>` format.
    ///
    /// The name is borrowed for the parser's lifetime and only copied into the
    /// error's owned `String` when an error is actually emitted.
    pub fn with_name(
        parse_name: Option<&'a str>,
        source: &'a str,
        left_delim: &'a str,
        right_delim: &'a str,
    ) -> Result<Self> {
        let lexer = Lexer::new(source, left_delim, right_delim).with_name(parse_name);
        let tokens = lexer.tokenize()?;
        Ok(Parser {
            tokens,
            pos: 0,
            source,
            depth: 0,
            parse_name,
        })
    }

    fn err_name(&self) -> Option<String> {
        self.parse_name.map(String::from)
    }

    fn enter(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            return Err(TemplateError::Parse {
                name: self.err_name(),
                line: 0,
                col: 0,
                message: alloc::format!("template nesting depth exceeded {}", MAX_PARSE_DEPTH),
            });
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// Parse the entire template into an AST.
    ///
    /// Returns a tuple of:
    /// - [`ListNode`]: the top-level node sequence (the template body)
    /// - [`Vec<DefineNode>`]: any `{{define "name"}}...{{end}}` blocks found
    ///
    /// # Errors
    ///
    /// Returns a [`TemplateError::Parse`] on
    /// syntax errors (unexpected tokens, unclosed blocks, etc.).
    pub fn parse(mut self) -> Result<(ListNode, Vec<DefineNode>)> {
        let mut defines = Vec::new();
        let list = self.parse_list(&mut defines)?;
        Ok((list, defines))
    }

    // Token navigation
    fn peek(&self) -> &Token<'a> {
        &self.tokens[self.pos]
    }

    fn next(&mut self) -> &Token<'a> {
        let tok = &self.tokens[self.pos];
        self.pos += 1;
        tok
    }

    /// Consume the next token and return just its `(pos, line)`. Lets sites
    /// that only need the source span advance the cursor without cloning
    /// the whole `Token`.
    fn next_span(&mut self) -> (usize, usize) {
        let t = self.next();
        (t.pos, t.line)
    }

    fn expect(&mut self, kind: TokenKind) -> Result<()> {
        let idx = self.pos;
        self.pos += 1;
        let tok = &self.tokens[idx];
        if tok.kind != kind {
            let tok_kind = tok.kind.clone();
            let tok_val = tok.val.clone();
            let (line, col) = tok.line_col(self.source);
            return Err(TemplateError::Parse {
                name: self.err_name(),
                line,
                col,
                message: format!("expected {:?}, got {:?} ({:?})", kind, tok_kind, tok_val),
            });
        }
        Ok(())
    }

    fn cur_pos(&self) -> Pos {
        Pos::new(self.peek().pos, self.peek().line)
    }

    fn error(&self, msg: impl Into<String>) -> TemplateError {
        let tok = self.peek();
        let (line, col) = tok.line_col(self.source);
        TemplateError::Parse {
            name: self.err_name(),
            line,
            col,
            message: msg.into(),
        }
    }

    /// Build a `TemplateError::Parse` anchored at an arbitrary token's location.
    /// Used for errors surfaced after a token has already been consumed, where
    /// `self.peek()` no longer points at the offending token.
    fn token_error(&self, tok: &Token<'a>, msg: impl Into<String>) -> TemplateError {
        let (line, col) = tok.line_col(self.source);
        TemplateError::Parse {
            name: self.err_name(),
            line,
            col,
            message: msg.into(),
        }
    }

    // parse_list: the core loop
    fn parse_list(&mut self, defines: &mut Vec<DefineNode>) -> Result<ListNode> {
        self.enter()?;
        let pos = self.cur_pos();
        let mut nodes = Vec::new();

        loop {
            match self.peek().kind {
                TokenKind::Eof => break,
                TokenKind::Text => {
                    let tok = self.next().clone();
                    nodes.push(Node::Text(TextNode {
                        pos: Pos::new(tok.pos, tok.line),
                        text: Arc::from(tok.val.as_ref()),
                    }));
                }
                TokenKind::LeftDelim | TokenKind::LeftTrimDelim => {
                    self.next(); // consume delimiter

                    match self.peek().kind {
                        TokenKind::End => {
                            self.next();
                            self.expect_close_delim()?;
                            break;
                        }
                        TokenKind::Else => {
                            break;
                        }
                        TokenKind::If => {
                            nodes.push(self.parse_if(defines)?);
                        }
                        TokenKind::Range => {
                            nodes.push(self.parse_range(defines)?);
                        }
                        TokenKind::With => {
                            nodes.push(self.parse_with(defines)?);
                        }
                        TokenKind::Define => {
                            let def = self.parse_define(defines)?;
                            defines.push(def);
                        }
                        TokenKind::Template => {
                            nodes.push(self.parse_template_call()?);
                        }
                        TokenKind::Break => {
                            let pos = self.cur_pos();
                            self.next();
                            self.expect_close_delim()?;
                            nodes.push(Node::Break(pos));
                        }
                        TokenKind::Continue => {
                            let pos = self.cur_pos();
                            self.next();
                            self.expect_close_delim()?;
                            nodes.push(Node::Continue(pos));
                        }
                        TokenKind::Block => {
                            let (node, def) = self.parse_block(defines)?;
                            nodes.push(node);
                            defines.push(def);
                        }
                        _ => {
                            let pipe = self.parse_pipeline(true)?;
                            self.expect_close_delim()?;
                            nodes.push(Node::Action(ActionNode {
                                pos: pipe.pos,
                                pipe,
                            }));
                        }
                    }
                }
                _ => {
                    self.leave();
                    return Err(self.error(format!("unexpected token: {:?}", self.peek().kind)));
                }
            }
        }

        self.leave();
        Ok(ListNode { pos, nodes })
    }

    // Control structure parsers
    fn parse_branch(
        &mut self,
        defines: &mut Vec<DefineNode>,
        allow_multi_decl: bool,
    ) -> Result<BranchNode> {
        let pos = self.cur_pos();
        self.next();

        let pipe = self.parse_pipeline_full(true, allow_multi_decl)?;
        self.expect_close_delim()?;

        let body = self.parse_list(defines)?;

        let else_body = if self.pos < self.tokens.len() && self.peek().kind == TokenKind::Else {
            self.next();

            if self.peek().kind == TokenKind::If {
                let inner_if = self.parse_if(defines)?;
                self.expect_close_delim_or_end()?;
                Some(ListNode {
                    pos: self.cur_pos(),
                    nodes: vec![inner_if],
                })
            } else if self.peek().kind == TokenKind::With {
                let inner_with = self.parse_with(defines)?;
                self.expect_close_delim_or_end()?;
                Some(ListNode {
                    pos: self.cur_pos(),
                    nodes: vec![inner_with],
                })
            } else {
                self.expect_close_delim()?;
                let else_list = self.parse_list(defines)?;
                Some(else_list)
            }
        } else {
            None
        };

        Ok(BranchNode {
            pos,
            pipe,
            body,
            else_body,
        })
    }

    fn parse_if(&mut self, defines: &mut Vec<DefineNode>) -> Result<Node> {
        Ok(Node::If(self.parse_branch(defines, false)?))
    }

    fn parse_range(&mut self, defines: &mut Vec<DefineNode>) -> Result<Node> {
        Ok(Node::Range(self.parse_branch(defines, true)?))
    }

    fn parse_with(&mut self, defines: &mut Vec<DefineNode>) -> Result<Node> {
        Ok(Node::With(self.parse_branch(defines, false)?))
    }

    fn parse_define(&mut self, defines: &mut Vec<DefineNode>) -> Result<DefineNode> {
        let pos = self.cur_pos();
        self.next();

        let name_tok = self.next().clone();
        if name_tok.kind != TokenKind::String {
            return Err(self.error("define expects a string name"));
        }
        self.expect_close_delim()?;

        let body = self.parse_list(defines)?;

        Ok(DefineNode {
            pos,
            name: SmolStr::from(name_tok.val.as_ref()),
            body,
        })
    }

    fn parse_template_call(&mut self) -> Result<Node> {
        let pos = self.cur_pos();
        self.next();

        let name_tok = self.next().clone();
        if name_tok.kind != TokenKind::String {
            return Err(self.error("template expects a string name"));
        }

        let pipe = if self.peek().kind != TokenKind::RightDelim
            && self.peek().kind != TokenKind::RightTrimDelim
        {
            Some(self.parse_pipeline(false)?)
        } else {
            None
        };

        self.expect_close_delim()?;

        Ok(Node::Template(TemplateNode {
            pos,
            name: SmolStr::from(name_tok.val.as_ref()),
            pipe,
        }))
    }

    fn parse_block(&mut self, defines: &mut Vec<DefineNode>) -> Result<(Node, DefineNode)> {
        let pos = self.cur_pos();
        self.next();

        let name_tok = self.next().clone();
        if name_tok.kind != TokenKind::String {
            return Err(self.error("block expects a string name"));
        }

        let pipe = if self.peek().kind != TokenKind::RightDelim
            && self.peek().kind != TokenKind::RightTrimDelim
        {
            Some(self.parse_pipeline(false)?)
        } else {
            None
        };

        self.expect_close_delim()?;
        let body = self.parse_list(defines)?;

        let name = SmolStr::from(name_tok.val.as_ref());
        let define = DefineNode {
            pos,
            name: name.clone(),
            body: body.clone(),
        };

        let tmpl = Node::Template(TemplateNode { pos, name, pipe });

        Ok((tmpl, define))
    }

    // Pipeline and command parsing
    fn parse_pipeline(&mut self, allow_decl: bool) -> Result<PipeNode> {
        self.parse_pipeline_full(allow_decl, false)
    }

    fn parse_pipeline_full(
        &mut self,
        allow_decl: bool,
        allow_multi_decl: bool,
    ) -> Result<PipeNode> {
        let pos = self.cur_pos();
        let mut decl = Vec::new();
        let mut is_assign = false;

        if allow_decl && self.peek().kind == TokenKind::Variable {
            let saved = self.pos;
            let mut vars = Vec::new();

            while self.peek().kind == TokenKind::Variable {
                let var_tok = self.next().clone();
                vars.push(SmolStr::from(var_tok.val.as_ref()));

                if self.peek().kind == TokenKind::Comma {
                    self.next();
                }
            }

            if self.peek().kind == TokenKind::Declare {
                self.next();
                decl = vars;
                is_assign = false;
            } else if self.peek().kind == TokenKind::Assign {
                self.next();
                decl = vars;
                is_assign = true;
            } else {
                self.pos = saved;
            }
        }

        if !allow_multi_decl && decl.len() > 1 {
            return Err(self.error(format!(
                "cannot assign {} variables outside a range pipeline",
                decl.len()
            )));
        }

        let mut commands = Vec::new();
        commands.push(self.parse_command()?);

        while self.peek().kind == TokenKind::Pipe {
            self.next();
            commands.push(self.parse_command()?);
        }

        Ok(PipeNode {
            pos,
            decl,
            commands,
            is_assign,
        })
    }

    fn parse_command(&mut self) -> Result<CommandNode> {
        let pos = self.cur_pos();
        let mut args = Vec::new();

        loop {
            match self.peek().kind {
                TokenKind::RightDelim
                | TokenKind::RightTrimDelim
                | TokenKind::Pipe
                | TokenKind::Eof => break,

                TokenKind::End | TokenKind::Else => break,

                TokenKind::LeftParen => {
                    let paren_pos = self.cur_pos();
                    self.next();
                    self.enter()?;
                    let pipe_result = self.parse_pipeline(false);
                    self.leave();
                    let pipe = pipe_result?;
                    self.expect(TokenKind::RightParen)?;
                    let chain_end = self.tokens[self.pos - 1].pos + 1;
                    let mut fields = Vec::new();
                    self.extend_field_chain(chain_end, &mut fields);
                    if fields.is_empty() {
                        args.push(Expr::Pipe(paren_pos, pipe));
                    } else {
                        args.push(Expr::Chain(
                            paren_pos,
                            Box::new(Expr::Pipe(paren_pos, pipe)),
                            fields,
                        ));
                    }
                }

                TokenKind::Dot => {
                    let (tok_pos, tok_line) = self.next_span();
                    let mut fields = Vec::new();
                    self.extend_field_chain(tok_pos + 1, &mut fields);
                    if fields.is_empty() {
                        args.push(Expr::Dot(Pos::new(tok_pos, tok_line)));
                    } else {
                        args.push(Expr::Field(Pos::new(tok_pos, tok_line), fields));
                    }
                }

                TokenKind::Field => {
                    let tok = self.next().clone();
                    let mut fields = Vec::new();
                    Self::push_field_chain(&tok.val, &mut fields);
                    self.extend_field_chain(tok.pos + tok.val.len(), &mut fields);
                    args.push(Expr::Field(Pos::new(tok.pos, tok.line), fields));
                }

                TokenKind::Variable => {
                    let tok = self.next().clone();
                    let mut fields = Vec::new();
                    self.extend_field_chain(tok.pos + tok.val.len(), &mut fields);
                    args.push(Expr::Variable(
                        Pos::new(tok.pos, tok.line),
                        SmolStr::from(tok.val.as_ref()),
                        fields,
                    ));
                }

                TokenKind::Identifier => {
                    let tok = self.next().clone();
                    let mut fields = Vec::new();
                    self.extend_field_chain(tok.pos + tok.val.len(), &mut fields);
                    let ident_pos = Pos::new(tok.pos, tok.line);
                    let name = SmolStr::from(tok.val.as_ref());
                    if fields.is_empty() {
                        args.push(Expr::Identifier(ident_pos, name));
                    } else {
                        args.push(Expr::Chain(
                            ident_pos,
                            Box::new(Expr::Identifier(ident_pos, name)),
                            fields,
                        ));
                    }
                }

                TokenKind::String => {
                    let tok = self.next().clone();
                    args.push(Expr::String(
                        Pos::new(tok.pos, tok.line),
                        Arc::from(tok.val.as_ref()),
                    ));
                }
                TokenKind::Number => {
                    let tok = self.next().clone();
                    let num = match tok.num {
                        Some(n) => n,
                        None => parse_number(&tok.val).ok_or_else(|| {
                            self.token_error(&tok, format!("invalid number: {}", tok.val))
                        })?,
                    };
                    args.push(Expr::Number(Pos::new(tok.pos, tok.line), num));
                }
                TokenKind::Bool => {
                    let t = self.next();
                    args.push(Expr::Bool(Pos::new(t.pos, t.line), t.val == "true"));
                }
                TokenKind::Nil => {
                    let (pos, line) = self.next_span();
                    args.push(Expr::Nil(Pos::new(pos, line)));
                }
                TokenKind::Char => {
                    let tok = self.next().clone();
                    let num = tok.num.ok_or_else(|| {
                        self.token_error(&tok, format!("invalid char literal: {}", tok.val))
                    })?;
                    args.push(Expr::Number(Pos::new(tok.pos, tok.line), num));
                }

                _ => break,
            }
        }

        if args.is_empty() {
            return Err(self.error("empty command"));
        }

        Ok(CommandNode { pos, args })
    }

    fn push_field_chain(field_str: &str, out: &mut Vec<SmolStr>) {
        out.extend(
            field_str
                .split('.')
                .filter(|s| !s.is_empty())
                .map(SmolStr::from),
        );
    }

    /// Consume any `Field` tokens that immediately follow a chain's tail
    /// (no whitespace between them) and append their components to `fields`.
    fn extend_field_chain(&mut self, mut chain_end: usize, fields: &mut Vec<SmolStr>) {
        while self.peek().kind == TokenKind::Field && self.peek().pos == chain_end {
            let ftok = self.next().clone();
            chain_end = ftok.pos + ftok.val.len();
            Self::push_field_chain(&ftok.val, fields);
        }
    }

    // Delimiter helpers
    fn expect_close_delim(&mut self) -> Result<()> {
        let tok = self.next();
        match tok.kind {
            TokenKind::RightDelim | TokenKind::RightTrimDelim => Ok(()),
            _ => {
                let (line, col) = self.tokens[self.pos - 1].line_col(self.source);
                Err(TemplateError::Parse {
                    name: self.err_name(),
                    line,
                    col,
                    message: format!(
                        "expected closing delimiter, got {:?} ({:?})",
                        self.tokens[self.pos - 1].kind,
                        self.tokens[self.pos - 1].val
                    ),
                })
            }
        }
    }

    /// After parsing an `else if` / `else with` chain, the inner branch
    /// has already consumed `{{end}}`. Verify we stopped at a valid position.
    fn expect_close_delim_or_end(&mut self) -> Result<()> {
        // The inner branch consumed its own `{{end}}`.
        // If we're at a right delimiter here, consume it (an outer end was seen).
        match self.peek().kind {
            TokenKind::RightDelim | TokenKind::RightTrimDelim => {
                self.next();
            }
            TokenKind::Eof | TokenKind::LeftDelim | TokenKind::LeftTrimDelim | TokenKind::Text => {
                // Valid: we're past the end of the control block.
            }
            _ => {
                return Err(self.error(format!(
                    "unexpected token after else-if/else-with: {:?}",
                    self.peek().kind
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> (ListNode, Vec<DefineNode>) {
        Parser::new(input, "{{", "}}").unwrap().parse().unwrap()
    }

    #[test]
    fn test_parse_text() {
        let (list, _) = parse("hello world");
        assert_eq!(list.nodes.len(), 1);
        match &list.nodes[0] {
            Node::Text(t) => assert_eq!(&*t.text, "hello world"),
            _ => panic!("expected Text node"),
        }
    }

    #[test]
    fn test_parse_action() {
        let (list, _) = parse("{{.Name}}");
        assert_eq!(list.nodes.len(), 1);
        match &list.nodes[0] {
            Node::Action(a) => {
                assert_eq!(a.pipe.commands.len(), 1);
                assert_eq!(a.pipe.commands[0].args.len(), 1);
            }
            _ => panic!("expected Action node"),
        }
    }

    #[test]
    fn test_parse_if() {
        let (list, _) = parse("{{if .OK}}yes{{end}}");
        assert_eq!(list.nodes.len(), 1);
        match &list.nodes[0] {
            Node::If(branch) => {
                assert_eq!(branch.body.nodes.len(), 1);
                assert!(branch.else_body.is_none());
            }
            _ => panic!("expected If node"),
        }
    }

    #[test]
    fn test_parse_if_else() {
        let (list, _) = parse("{{if .OK}}yes{{else}}no{{end}}");
        match &list.nodes[0] {
            Node::If(branch) => {
                assert!(branch.else_body.is_some());
            }
            _ => panic!("expected If node"),
        }
    }

    #[test]
    fn test_parse_range() {
        let (list, _) = parse("{{range .Items}}{{.}}{{end}}");
        match &list.nodes[0] {
            Node::Range(_) => {}
            _ => panic!("expected Range node"),
        }
    }

    #[test]
    fn test_parse_pipeline() {
        let (list, _) = parse("{{.Name | len}}");
        match &list.nodes[0] {
            Node::Action(a) => {
                assert_eq!(a.pipe.commands.len(), 2);
            }
            _ => panic!("expected Action with pipeline"),
        }
    }

    #[test]
    fn test_parse_define() {
        let (_, defines) = parse("{{define \"header\"}}Header{{end}}");
        assert_eq!(defines.len(), 1);
        assert_eq!(&*defines[0].name, "header");
    }

    #[test]
    fn test_parse_template_call() {
        let (list, _) = parse("{{template \"header\" .}}");
        match &list.nodes[0] {
            Node::Template(t) => {
                assert_eq!(&*t.name, "header");
                assert!(t.pipe.is_some());
            }
            _ => panic!("expected Template node"),
        }
    }

    #[test]
    fn test_parse_number_helper_discriminates_int_and_float() {
        assert_eq!(parse_number("42"), Some(Number::Int(42)));
        assert_eq!(parse_number("-7"), Some(Number::Int(-7)));
        assert_eq!(parse_number("1.5"), Some(Number::Float(1.5)));
        assert_eq!(parse_number("2e3"), Some(Number::Float(2000.0)));
        assert_eq!(parse_number("2E-1"), Some(Number::Float(0.2)));
        assert!(parse_number("not a number").is_none());
    }

    #[test]
    fn test_parse_number_expr_carries_parsed_variant() {
        // Integer literal -> Expr::Number(_, Number::Int(_))
        let (list, _) = parse("{{42}}");
        match &list.nodes[0] {
            Node::Action(a) => match &a.pipe.commands[0].args[0] {
                Expr::Number(_, Number::Int(42)) => {}
                other => panic!("expected Number::Int(42), got {:?}", other),
            },
            _ => panic!("expected Action node"),
        }

        // Float literal -> Expr::Number(_, Number::Float(_))
        let (list, _) = parse("{{2.5}}");
        match &list.nodes[0] {
            Node::Action(a) => match &a.pipe.commands[0].args[0] {
                Expr::Number(_, Number::Float(f)) if (*f - 2.5).abs() < 1e-9 => {}
                other => panic!("expected Number::Float(2.5), got {:?}", other),
            },
            _ => panic!("expected Action node"),
        }

        // Hex literal is normalized to decimal by the lexer, still an Int.
        let (list, _) = parse("{{0xff}}");
        match &list.nodes[0] {
            Node::Action(a) => match &a.pipe.commands[0].args[0] {
                Expr::Number(_, Number::Int(255)) => {}
                other => panic!("expected Number::Int(255), got {:?}", other),
            },
            _ => panic!("expected Action node"),
        }

        // Char literal is emitted as its Unicode code point (also an Int).
        let (list, _) = parse("{{'A'}}");
        match &list.nodes[0] {
            Node::Action(a) => match &a.pipe.commands[0].args[0] {
                Expr::Number(_, Number::Int(65)) => {}
                other => panic!("expected Number::Int(65), got {:?}", other),
            },
            _ => panic!("expected Action node"),
        }
    }
}
