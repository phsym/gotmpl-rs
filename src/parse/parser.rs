//! Recursive-descent parser that converts a token stream into an AST.

use super::lexer::{Lexer, Token, TokenKind};
use super::node::*;
use crate::error::{Result, TemplateError};

/// Recursive-descent parser for Go template source.
///
/// Created from a source string via [`new`](Self::new), then invoked via
/// [`parse`](Self::parse) to produce the AST.
///
/// # Examples
///
/// ```
/// use go_template::parse::{Parser, Node};
///
/// let parser = Parser::new("Hello, {{.Name}}!", "{{", "}}").unwrap();
/// let (tree, defines) = parser.parse().unwrap();
/// assert_eq!(tree.nodes.len(), 3); // Text, Action, Text
/// assert!(defines.is_empty());
/// ```
pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    source: String,
}

impl Parser {
    /// Create a new parser for the given template source.
    ///
    /// Runs the lexer internally to produce the token stream.
    ///
    /// # Errors
    ///
    /// Returns a [`TemplateError::Lex`] if
    /// the source contains lexical errors (unterminated strings, invalid characters, etc.).
    pub fn new(source: &str, left_delim: &str, right_delim: &str) -> Result<Self> {
        let lexer = Lexer::new(source, left_delim, right_delim);
        let tokens = lexer.tokenize()?;
        Ok(Parser {
            tokens,
            pos: 0,
            source: source.to_string(),
        })
    }

    /// Parse the entire template into an AST.
    ///
    /// Returns a tuple of:
    /// - [`ListNode`] — the top-level node sequence (the template body)
    /// - [`Vec<DefineNode>`] — any `{{define "name"}}...{{end}}` blocks found
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

    // ─── Token navigation ────────────────────────────────────────────

    fn peek(&self) -> &Token {
        &self.tokens[self.pos]
    }

    fn next(&mut self) -> &Token {
        let tok = &self.tokens[self.pos];
        self.pos += 1;
        tok
    }

    fn expect(&mut self, kind: TokenKind) -> Result<()> {
        let idx = self.pos;
        self.pos += 1;
        let tok = &self.tokens[idx];
        if tok.kind != kind {
            let tok_kind = tok.kind.clone();
            let tok_val = tok.val.clone();
            let (line, col) = tok.line_col(&self.source);
            return Err(TemplateError::Parse {
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
        let (line, col) = tok.line_col(&self.source);
        TemplateError::Parse {
            line,
            col,
            message: msg.into(),
        }
    }

    // ─── parse_list: the core loop ──────────────────────────────────

    fn parse_list(&mut self, defines: &mut Vec<DefineNode>) -> Result<ListNode> {
        let pos = self.cur_pos();
        let mut nodes = Vec::new();

        loop {
            match self.peek().kind {
                TokenKind::Eof => break,
                TokenKind::Text => {
                    let tok = self.next().clone();
                    nodes.push(Node::Text(TextNode {
                        pos: Pos::new(tok.pos, tok.line),
                        text: tok.val,
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
                    return Err(self.error(format!("unexpected token: {:?}", self.peek().kind)));
                }
            }
        }

        Ok(ListNode { pos, nodes })
    }

    // ─── Control structure parsers ──────────────────────────────────

    fn parse_branch(&mut self, defines: &mut Vec<DefineNode>) -> Result<BranchNode> {
        let pos = self.cur_pos();
        self.next();

        let pipe = self.parse_pipeline(true)?;
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
        Ok(Node::If(self.parse_branch(defines)?))
    }

    fn parse_range(&mut self, defines: &mut Vec<DefineNode>) -> Result<Node> {
        Ok(Node::Range(self.parse_branch(defines)?))
    }

    fn parse_with(&mut self, defines: &mut Vec<DefineNode>) -> Result<Node> {
        Ok(Node::With(self.parse_branch(defines)?))
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
            name: name_tok.val,
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
            name: name_tok.val,
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

        let define = DefineNode {
            pos,
            name: name_tok.val.clone(),
            body: body.clone(),
        };

        let tmpl = Node::Template(TemplateNode {
            pos,
            name: name_tok.val,
            pipe,
        });

        Ok((tmpl, define))
    }

    // ─── Pipeline and command parsing ───────────────────────────────

    fn parse_pipeline(&mut self, allow_decl: bool) -> Result<PipeNode> {
        let pos = self.cur_pos();
        let mut decl = Vec::new();
        let mut is_assign = false;

        if allow_decl && self.peek().kind == TokenKind::Variable {
            let saved = self.pos;
            let mut vars = Vec::new();

            while self.peek().kind == TokenKind::Variable {
                let var_tok = self.next().clone();
                vars.push(var_tok.val);

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
                    let pipe = self.parse_pipeline(false)?;
                    self.expect(TokenKind::RightParen)?;
                    let mut fields = Vec::new();
                    while self.peek().kind == TokenKind::Field {
                        let ftok = self.next().clone();
                        fields.extend(self.parse_field_chain(&ftok.val));
                    }
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
                    let tok = self.next().clone();
                    let mut fields = Vec::new();
                    while self.peek().kind == TokenKind::Field {
                        let ftok = self.next().clone();
                        fields.extend(self.parse_field_chain(&ftok.val));
                    }
                    if fields.is_empty() {
                        args.push(Expr::Dot(Pos::new(tok.pos, tok.line)));
                    } else {
                        args.push(Expr::Field(Pos::new(tok.pos, tok.line), fields));
                    }
                }

                TokenKind::Field => {
                    let tok = self.next().clone();
                    let mut fields = self.parse_field_chain(&tok.val);
                    while self.peek().kind == TokenKind::Field {
                        let ftok = self.next().clone();
                        fields.extend(self.parse_field_chain(&ftok.val));
                    }
                    args.push(Expr::Field(Pos::new(tok.pos, tok.line), fields));
                }

                TokenKind::Variable => {
                    let tok = self.next().clone();
                    let var_name = tok.val.clone();
                    let mut fields = Vec::new();
                    while self.peek().kind == TokenKind::Field {
                        let ftok = self.next().clone();
                        fields.extend(self.parse_field_chain(&ftok.val));
                    }
                    args.push(Expr::Variable(
                        Pos::new(tok.pos, tok.line),
                        var_name,
                        fields,
                    ));
                }

                TokenKind::Identifier => {
                    let tok = self.next().clone();
                    let mut fields = Vec::new();
                    let mut chain_end = tok.pos + tok.val.chars().count();
                    while self.peek().kind == TokenKind::Field && self.peek().pos == chain_end {
                        let ftok = self.next().clone();
                        chain_end = ftok.pos + ftok.val.chars().count();
                        fields.extend(self.parse_field_chain(&ftok.val));
                    }
                    if fields.is_empty() {
                        args.push(Expr::Identifier(Pos::new(tok.pos, tok.line), tok.val));
                    } else {
                        args.push(Expr::Chain(
                            Pos::new(tok.pos, tok.line),
                            Box::new(Expr::Identifier(Pos::new(tok.pos, tok.line), tok.val)),
                            fields,
                        ));
                    }
                }

                TokenKind::String => {
                    let tok = self.next().clone();
                    args.push(Expr::String(Pos::new(tok.pos, tok.line), tok.val));
                }
                TokenKind::Number => {
                    let tok = self.next().clone();
                    args.push(Expr::Number(Pos::new(tok.pos, tok.line), tok.val));
                }
                TokenKind::Bool => {
                    let tok = self.next().clone();
                    args.push(Expr::Bool(Pos::new(tok.pos, tok.line), tok.val == "true"));
                }
                TokenKind::Nil => {
                    let tok = self.next().clone();
                    args.push(Expr::Nil(Pos::new(tok.pos, tok.line)));
                }
                TokenKind::Char => {
                    let tok = self.next().clone();
                    args.push(Expr::Number(Pos::new(tok.pos, tok.line), tok.val));
                }

                _ => break,
            }
        }

        if args.is_empty() {
            return Err(self.error("empty command"));
        }

        Ok(CommandNode { pos, args })
    }

    fn parse_field_chain(&self, field_str: &str) -> Vec<String> {
        field_str
            .split('.')
            .filter(|s| !s.is_empty())
            .map(std::string::ToString::to_string)
            .collect()
    }

    // ─── Delimiter helpers ──────────────────────────────────────────

    fn expect_close_delim(&mut self) -> Result<()> {
        let tok = self.next();
        match tok.kind {
            TokenKind::RightDelim | TokenKind::RightTrimDelim => Ok(()),
            _ => {
                let (line, col) = self.tokens[self.pos - 1].line_col(&self.source);
                Err(TemplateError::Parse {
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
            Node::Text(t) => assert_eq!(t.text, "hello world"),
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
        assert_eq!(defines[0].name, "header");
    }

    #[test]
    fn test_parse_template_call() {
        let (list, _) = parse("{{template \"header\" .}}");
        match &list.nodes[0] {
            Node::Template(t) => {
                assert_eq!(t.name, "header");
                assert!(t.pipe.is_some());
            }
            _ => panic!("expected Template node"),
        }
    }
}
