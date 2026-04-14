//! Template execution engine — walks the AST and writes output.
//!
//! The [`Executor`] evaluates a parsed template tree against a [`Value`] data
//! context, writing results to any [`std::io::Write`] destination.
//!
//! This module is used internally by [`Template::execute`](crate::Template::execute);
//! most users don't need to interact with it directly.
//!
//! # Execution model
//!
//! The executor maintains:
//! - **dot** — the current context value (changes inside `range`/`with`)
//! - **`$`** — always refers to the root data passed to [`execute`](Executor::execute)
//! - **variable scopes** — a stack of name→[`Value`] frames, pushed/popped for control blocks
//! - **recursion depth** — prevents stack overflow from recursive `{{template}}` calls

use std::collections::HashMap;
use std::io::Write;

use crate::error::{TemplateError, Result};
use crate::funcs::Func;
use crate::parse::{ListNode, Node, BranchNode, TemplateNode, PipeNode, CommandNode, Expr};
use crate::value::Value;

/// Maximum recursion depth for `{{template}}` calls.
///
/// Go uses 100,000 but Rust's default stack is smaller. 1,000 is safe and
/// still far beyond any reasonable template nesting.
const MAX_EXEC_DEPTH: usize = 1_000;

/// Controls behavior when accessing a missing key on a [`Value::Map`].
///
/// Set via [`Template::option`](crate::Template::option) with
/// `"missingkey=invalid"`, `"missingkey=zero"`, or `"missingkey=error"`.
#[derive(Debug, Clone, Copy, PartialEq)]
#[derive(Default)]
pub enum MissingKey {
    /// Return [`Value::Nil`] for missing keys (the default).
    #[default]
    Invalid,
    /// Return [`Value::Nil`] for missing keys (same as `Invalid`).
    ZeroValue,
    /// Return a [`TemplateError::Exec`] for missing keys.
    Error,
}

/// Internal control-flow signal for `{{break}}` and `{{continue}}`.
///
/// Propagated as a [`TemplateError::ControlFlow`] and caught by the range walker.
/// This type should never appear in errors returned to users.
#[derive(Debug)]
pub enum ControlFlow {
    Break,
    Continue,
}

impl std::fmt::Display for ControlFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlFlow::Break => write!(f, "break"),
            ControlFlow::Continue => write!(f, "continue"),
        }
    }
}

impl std::error::Error for ControlFlow {}

/// Variable scope: a stack of name→value mappings.
/// New scopes are pushed for range/with blocks.
struct VarScope {
    frames: Vec<HashMap<String, Value>>,
}

impl VarScope {
    fn new() -> Self {
        VarScope {
            frames: vec![HashMap::new()],
        }
    }

    fn push(&mut self) {
        self.frames.push(HashMap::new());
    }

    fn pop(&mut self) {
        self.frames.pop();
    }

    fn set(&mut self, name: &str, val: Value) {
        if let Some(frame) = self.frames.last_mut() {
            frame.insert(name.to_string(), val);
        }
    }

    /// Set a variable in any existing scope (for assignment with =)
    fn assign(&mut self, name: &str, val: Value) {
        for frame in self.frames.iter_mut().rev() {
            if frame.contains_key(name) {
                frame.insert(name.to_string(), val);
                return;
            }
        }
        // If not found, set in current scope
        self.set(name, val);
    }

    fn get(&self, name: &str) -> Option<&Value> {
        for frame in self.frames.iter().rev() {
            if let Some(v) = frame.get(name) {
                return Some(v);
            }
        }
        None
    }
}

/// The template execution context.
///
/// Walks the AST produced by the [`Parser`](crate::parse::Parser), evaluating
/// pipelines, resolving variables, calling functions, and writing output.
///
/// Created internally by [`Template::execute`](crate::Template::execute).
pub struct Executor<'a> {
    funcs: &'a HashMap<String, Func>,
    templates: &'a HashMap<String, ListNode>,
    vars: VarScope,
    depth: usize,
    missing_key: MissingKey,
}

/// Execute a range loop body with break/continue handling.
///
/// `iter` yields `(index_value, element_value)` pairs. The macro is
/// necessary because `break` / `continue` are loop-control keywords
/// that cannot live inside a regular function call.
macro_rules! range_loop {
    ($self:expr, $w:expr, $branch:expr, $iter:expr) => {
        for (idx_val, item) in $iter {
            $self.vars.push();
            if $branch.pipe.decl.len() == 1 {
                $self.vars.set(&$branch.pipe.decl[0], item.clone());
            } else if $branch.pipe.decl.len() >= 2 {
                $self.vars.set(&$branch.pipe.decl[0], idx_val);
                $self.vars.set(&$branch.pipe.decl[1], item.clone());
            }
            match $self.walk($w, &$branch.body, &item) {
                Ok(()) => {}
                Err(TemplateError::ControlFlow(ControlFlow::Break)) => {
                    $self.vars.pop();
                    break;
                }
                Err(TemplateError::ControlFlow(ControlFlow::Continue)) => {
                    $self.vars.pop();
                    continue;
                }
                Err(e) => {
                    $self.vars.pop();
                    return Err(e);
                }
            }
            $self.vars.pop();
        }
    };
}

impl<'a> Executor<'a> {
    /// Create a new executor with the given function map and template definitions.
    ///
    /// The `funcs` map should contain all [built-in](crate::funcs::builtins) and
    /// user-defined functions. The `templates` map holds named templates from
    /// `{{define}}` blocks.
    pub fn new(
        funcs: &'a HashMap<String, Func>,
        templates: &'a HashMap<String, ListNode>,
    ) -> Self {
        Executor {
            funcs,
            templates,
            vars: VarScope::new(),
            depth: 0,
            missing_key: MissingKey::default(),
        }
    }

    /// Set the [`MissingKey`] behavior for this executor.
    pub fn set_missing_key(&mut self, mk: MissingKey) {
        self.missing_key = mk;
    }

    /// Execute the template tree with the given data, writing output to `writer`.
    ///
    /// The `dot` value is the initial context (`.`) and is also bound to `$`.
    ///
    /// # Errors
    ///
    /// Returns a [`TemplateError`] on undefined references, type errors,
    /// I/O failures, or exceeding the recursion depth limit.
    pub fn execute<W: Write>(
        &mut self,
        writer: &mut W,
        tree: &ListNode,
        dot: &Value,
    ) -> Result<()> {
        // Set $  to the initial dot value (Go does this)
        self.vars.set("$", dot.clone());
        self.walk(writer, tree, dot)
    }

    // ─── AST walker ──────────────────────────────────────────────────

    fn walk<W: Write>(
        &mut self,
        w: &mut W,
        list: &ListNode,
        dot: &Value,
    ) -> Result<()> {
        for node in &list.nodes {
            self.walk_node(w, node, dot)?;
        }
        Ok(())
    }

    fn walk_node<W: Write>(
        &mut self,
        w: &mut W,
        node: &Node,
        dot: &Value,
    ) -> Result<()> {
        match node {
            Node::Text(text) => {
                w.write_all(text.text.as_bytes())?;
                Ok(())
            }
            Node::Action(action) => {
                let val = self.eval_pipeline(dot, &action.pipe)?;
                // Only print if there are no declarations (side-effect-only)
                if action.pipe.decl.is_empty() {
                    let s = format!("{}", val);
                    w.write_all(s.as_bytes())?;
                }
                Ok(())
            }
            Node::If(branch) => self.walk_if(w, branch, dot),
            Node::Range(branch) => self.walk_range(w, branch, dot),
            Node::With(branch) => self.walk_with(w, branch, dot),
            Node::Template(tmpl) => self.walk_template(w, tmpl, dot),
            Node::Define(_) => Ok(()), // defines are collected at parse time
            Node::List(list) => self.walk(w, list, dot),
            Node::Break(_) => Err(TemplateError::ControlFlow(ControlFlow::Break)),
            Node::Continue(_) => Err(TemplateError::ControlFlow(ControlFlow::Continue)),
        }
    }

    // ─── Control flow ────────────────────────────────────────────────

    fn walk_if<W: Write>(
        &mut self,
        w: &mut W,
        branch: &BranchNode,
        dot: &Value,
    ) -> Result<()> {
        let val = self.eval_pipeline(dot, &branch.pipe)?;
        if val.is_truthy() {
            self.vars.push();
            let result = self.walk(w, &branch.body, dot);
            self.vars.pop();
            result
        } else if let Some(ref else_body) = branch.else_body {
            self.vars.push();
            let result = self.walk(w, else_body, dot);
            self.vars.pop();
            result
        } else {
            Ok(())
        }
    }

    fn walk_with<W: Write>(
        &mut self,
        w: &mut W,
        branch: &BranchNode,
        dot: &Value,
    ) -> Result<()> {
        let val = self.eval_pipeline(dot, &branch.pipe)?;
        if val.is_truthy() {
            // With sets dot to the pipeline value
            self.vars.push();
            let result = self.walk(w, &branch.body, &val);
            self.vars.pop();
            result
        } else if let Some(ref else_body) = branch.else_body {
            self.walk(w, else_body, dot)
        } else {
            Ok(())
        }
    }

    fn walk_range<W: Write>(
        &mut self,
        w: &mut W,
        branch: &BranchNode,
        dot: &Value,
    ) -> Result<()> {
        let val = self.eval_pipeline(dot, &branch.pipe)?;

        match &val {
            Value::List(items) if items.is_empty() => {
                if let Some(ref else_body) = branch.else_body {
                    self.walk(w, else_body, dot)?;
                }
            }
            Value::List(items) => {
                range_loop!(self, w, branch,
                    items.iter().enumerate().map(|(i, v)| (Value::Int(i as i64), v.clone()))
                );
            }
            Value::Map(map) if map.is_empty() => {
                if let Some(ref else_body) = branch.else_body {
                    self.walk(w, else_body, dot)?;
                }
            }
            Value::Map(map) => {
                range_loop!(self, w, branch,
                    map.iter().map(|(k, v)| (Value::String(k.clone()), v.clone()))
                );
            }
            Value::Int(n) => {
                // Go 1.22+: range over integer
                let count = *n;
                if count <= 0 {
                    if let Some(ref else_body) = branch.else_body {
                        self.walk(w, else_body, dot)?;
                    }
                } else {
                    range_loop!(self, w, branch,
                        (0..count).map(|i| (Value::Int(i), Value::Int(i)))
                    );
                }
            }
            Value::Nil => {
                // Nil is treated as empty
                if let Some(ref else_body) = branch.else_body {
                    self.walk(w, else_body, dot)?;
                }
            }
            other => {
                return Err(TemplateError::NotIterable(format!("{}", other)));
            }
        }

        Ok(())
    }

    fn walk_template<W: Write>(
        &mut self,
        w: &mut W,
        tmpl: &TemplateNode,
        dot: &Value,
    ) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_EXEC_DEPTH {
            return Err(TemplateError::Exec(format!(
                "exceeded maximum template call depth ({})",
                MAX_EXEC_DEPTH
            )));
        }

        let new_dot = if let Some(ref pipe) = tmpl.pipe {
            self.eval_pipeline(dot, pipe)?
        } else {
            dot.clone()
        };

        let tree = self
            .templates
            .get(&tmpl.name)
            .ok_or_else(|| TemplateError::UndefinedTemplate(tmpl.name.clone()))?
            .clone();

        self.vars.push();
        self.vars.set("$", new_dot.clone());
        let result = self.walk(w, &tree, &new_dot);
        self.vars.pop();
        self.depth -= 1;
        result
    }

    // ─── Pipeline and expression evaluation ──────────────────────────

    fn eval_pipeline(&mut self, dot: &Value, pipe: &PipeNode) -> Result<Value> {
        let mut val = Value::Nil;

        for (i, cmd) in pipe.commands.iter().enumerate() {
            // For piped commands, the previous result becomes the last argument
            let prev = if i > 0 { Some(val.clone()) } else { None };
            val = self.eval_command(dot, cmd, prev)?;
        }

        // Handle variable declarations
        if !pipe.decl.is_empty() {
            for var_name in &pipe.decl {
                if pipe.is_assign {
                    self.vars.assign(var_name, val.clone());
                } else {
                    self.vars.set(var_name, val.clone());
                }
            }
        }

        Ok(val)
    }

    fn eval_command(
        &mut self,
        dot: &Value,
        cmd: &CommandNode,
        piped: Option<Value>,
    ) -> Result<Value> {
        let first = &cmd.args[0];

        // If the first arg is an identifier, it's a function call
        if let Expr::Identifier(_, name) = first {
            // Special-case and/or for short-circuit evaluation
            if name == "and" || name == "or" {
                return self.eval_short_circuit(dot, name, &cmd.args[1..], piped);
            }
            return self.eval_function_call(dot, name, &cmd.args[1..], piped);
        }

        // If the first arg is a Chain with an Identifier inside, it's a function call
        // with field access on the result (e.g., mapOfThree.three)
        if let Expr::Chain(_, inner, fields) = first
            && let Expr::Identifier(_, name) = inner.as_ref() {
                if name == "and" || name == "or" {
                    let mut val = self.eval_short_circuit(dot, name, &cmd.args[1..], piped)?;
                    for field in fields {
                        val = self.field_access(&val, field)?;
                    }
                    return Ok(val);
                }
                let mut val = self.eval_function_call(dot, name, &cmd.args[1..], piped)?;
                for field in fields {
                    val = self.field_access(&val, field)?;
                }
                return Ok(val);
            }

        // Single expression: evaluate it
        if cmd.args.len() == 1 && piped.is_none() {
            return self.eval_expr(dot, first);
        }

        // If we have a piped value but a single non-function arg, that's an error
        // unless it's a field access that acts like an identity
        if piped.is_some() && cmd.args.len() == 1 {
            return self.eval_expr(dot, first);
        }

        self.eval_expr(dot, first)
    }

    /// Short-circuit evaluation for `and` and `or`.
    /// Go special-cases these so arguments are evaluated lazily.
    fn eval_short_circuit(
        &mut self,
        dot: &Value,
        name: &str,
        arg_exprs: &[Expr],
        piped: Option<Value>,
    ) -> Result<Value> {
        // Build the full list of expressions: explicit args + piped value
        // Piped value becomes the last argument
        let is_and = name == "and";

        // We need at least 1 argument (plus maybe piped)
        let total = arg_exprs.len() + if piped.is_some() { 1 } else { 0 };
        if total < 1 {
            return Err(TemplateError::ArgCount {
                name: name.to_string(),
                expected: 1,
                got: 0,
            });
        }

        let mut last = Value::Nil;

        for expr in arg_exprs {
            let val = self.eval_expr(dot, expr)?;
            if is_and && !val.is_truthy() {
                return Ok(val);
            }
            if !is_and && val.is_truthy() {
                return Ok(val);
            }
            last = val;
        }

        // Handle piped value (becomes last argument)
        if let Some(piped_val) = piped {
            if is_and && !piped_val.is_truthy() {
                return Ok(piped_val);
            }
            if !is_and && piped_val.is_truthy() {
                return Ok(piped_val);
            }
            last = piped_val;
        }

        Ok(last)
    }

    fn eval_function_call(
        &mut self,
        dot: &Value,
        name: &str,
        arg_exprs: &[Expr],
        piped: Option<Value>,
    ) -> Result<Value> {
        let func = self
            .funcs
            .get(name)
            .ok_or_else(|| TemplateError::UndefinedFunction(name.to_string()))?;

        let mut args: Vec<Value> = Vec::new();
        for expr in arg_exprs {
            args.push(self.eval_expr(dot, expr)?);
        }

        // Piped value becomes the last argument (Go convention)
        if let Some(piped_val) = piped {
            args.push(piped_val);
        }

        func(&args)
    }

    /// Access a field on a value, respecting missingkey option.
    fn field_access(&self, val: &Value, name: &str) -> Result<Value> {
        match val {
            Value::Map(m) => {
                match m.get(name) {
                    Some(v) => Ok(v.clone()),
                    None => match self.missing_key {
                        MissingKey::Error => Err(TemplateError::Exec(
                            format!("map has no entry for key {:?}", name)
                        )),
                        _ => Ok(Value::Nil),
                    },
                }
            }
            _ => Ok(Value::Nil),
        }
    }

    fn eval_expr(&mut self, dot: &Value, expr: &Expr) -> Result<Value> {
        match expr {
            Expr::Dot(_) => Ok(dot.clone()),

            Expr::Field(_, fields) => {
                let mut val = dot.clone();
                for field in fields {
                    val = self.field_access(&val, field)?;
                }
                Ok(val)
            }

            Expr::Variable(_, name, fields) => {
                let val = self
                    .vars
                    .get(name)
                    .cloned()
                    .ok_or_else(|| TemplateError::UndefinedVariable(name.clone()))?;
                let mut result = val;
                for field in fields {
                    result = self.field_access(&result, field)?;
                }
                Ok(result)
            }

            Expr::String(_, s) => Ok(Value::String(s.clone())),

            Expr::Number(_, s) => {
                if s.contains('.') || s.contains('e') || s.contains('E') {
                    Ok(Value::Float(s.parse::<f64>().map_err(|_| {
                        TemplateError::Exec(format!("invalid float: {}", s))
                    })?))
                } else {
                    Ok(Value::Int(s.parse::<i64>().map_err(|_| {
                        TemplateError::Exec(format!("invalid integer: {}", s))
                    })?))
                }
            }

            Expr::Bool(_, b) => Ok(Value::Bool(*b)),
            Expr::Nil(_) => Ok(Value::Nil),

            Expr::Pipe(_, pipe) => self.eval_pipeline(dot, pipe),

            Expr::Identifier(_, name) => {
                // Bare identifier — could be a zero-arg function call
                if let Some(func) = self.funcs.get(name.as_str()) {
                    return func(&[]);
                }
                Err(TemplateError::UndefinedFunction(name.clone()))
            }

            Expr::Chain(_, inner, fields) => {
                let mut val = self.eval_expr(dot, inner)?;
                for field in fields {
                    val = self.field_access(&val, field)?;
                }
                Ok(val)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::funcs::builtins;
    use crate::parse::Parser;
    use crate::tmap;

    fn exec(template: &str, data: &Value) -> String {
        let parser = Parser::new(template, "{{", "}}").unwrap();
        let (tree, defines) = parser.parse().unwrap();

        let funcs = builtins();
        let mut templates: HashMap<String, ListNode> = HashMap::new();
        for def in &defines {
            templates.insert(def.name.clone(), def.body.clone());
        }

        let mut executor = Executor::new(&funcs, &templates);
        let mut buf = Vec::new();
        executor.execute(&mut buf, &tree, &data).unwrap();
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn test_plain_text() {
        assert_eq!(exec("hello world", &Value::Nil), "hello world");
    }

    #[test]
    fn test_dot() {
        assert_eq!(exec("{{.}}", &Value::String("hi".into())), "hi");
    }

    #[test]
    fn test_field() {
        let data = tmap! { "Name" => "Alice" };
        assert_eq!(exec("Hello, {{.Name}}!", &data), "Hello, Alice!");
    }

    #[test]
    fn test_nested_field() {
        let data = tmap! {
            "User" => tmap! { "Name" => "Bob" },
        };
        assert_eq!(exec("{{.User.Name}}", &data), "Bob");
    }

    #[test]
    fn test_if_true() {
        let data = tmap! { "OK" => true };
        assert_eq!(exec("{{if .OK}}yes{{end}}", &data), "yes");
    }

    #[test]
    fn test_if_false() {
        let data = tmap! { "OK" => false };
        assert_eq!(exec("{{if .OK}}yes{{end}}", &data), "");
    }

    #[test]
    fn test_if_else() {
        let data = tmap! { "OK" => false };
        assert_eq!(exec("{{if .OK}}yes{{else}}no{{end}}", &data), "no");
    }

    #[test]
    fn test_range_list() {
        let data = tmap! {
            "Items" => vec!["a".to_string(), "b".to_string(), "c".to_string()],
        };
        assert_eq!(exec("{{range .Items}}[{{.}}]{{end}}", &data), "[a][b][c]");
    }

    #[test]
    fn test_range_empty_else() {
        let data = tmap! {
            "Items" => Vec::<String>::new(),
        };
        assert_eq!(
            exec("{{range .Items}}{{.}}{{else}}empty{{end}}", &data),
            "empty"
        );
    }

    #[test]
    fn test_with() {
        let data = tmap! {
            "User" => tmap! { "Name" => "Charlie" },
        };
        assert_eq!(
            exec("{{with .User}}Name: {{.Name}}{{end}}", &data),
            "Name: Charlie"
        );
    }

    #[test]
    fn test_pipeline() {
        let data = tmap! { "Name" => "hello" };
        assert_eq!(exec("{{.Name | len}}", &data), "5");
    }

    #[test]
    fn test_printf() {
        let data = tmap! { "N" => 42i64 };
        assert_eq!(exec("{{printf \"num=%d\" .N}}", &data), "num=42");
    }

    #[test]
    fn test_eq() {
        let data = tmap! { "X" => 1i64 };
        assert_eq!(exec("{{if eq .X 1}}yes{{end}}", &data), "yes");
    }

    #[test]
    fn test_variable() {
        let data = tmap! { "Name" => "Dave" };
        assert_eq!(
            exec("{{$x := .Name}}Hello, {{$x}}!", &data),
            "Hello, Dave!"
        );
    }

    #[test]
    fn test_template_call() {
        let data = tmap! { "Name" => "Eve" };
        let result = exec(
            "{{define \"greeting\"}}Hi, {{.Name}}!{{end}}{{template \"greeting\" .}}",
            &data,
        );
        assert_eq!(result, "Hi, Eve!");
    }

    #[test]
    fn test_nested_if() {
        let data = tmap! { "A" => true, "B" => true };
        assert_eq!(
            exec("{{if .A}}{{if .B}}both{{end}}{{end}}", &data),
            "both"
        );
    }

    #[test]
    fn test_html_escape() {
        let data = tmap! { "X" => "<b>bold</b>" };
        assert_eq!(
            exec("{{html .X}}", &data),
            "&lt;b&gt;bold&lt;/b&gt;"
        );
    }

    #[test]
    fn test_trim_both() {
        let data = tmap! { "X" => "hello" };
        assert_eq!(exec("  {{- .X -}}  ", &data), "hello");
    }

    #[test]
    fn test_trim_left() {
        let data = tmap! { "X" => "hello" };
        assert_eq!(exec("  {{- .X }}  ", &data), "hello  ");
    }

    #[test]
    fn test_trim_right() {
        let data = tmap! { "X" => "hello" };
        assert_eq!(exec("  {{ .X -}}  ", &data), "  hello");
    }

    #[test]
    fn test_trim_in_range() {
        let data = tmap! {
            "Items" => vec!["a".to_string(), "b".to_string(), "c".to_string()],
        };
        assert_eq!(
            exec("{{range .Items}}[{{.}}]{{end}}", &data),
            "[a][b][c]"
        );
        assert_eq!(
            exec("{{range .Items -}} {{.}} {{- end}}", &data),
            "abc"
        );
    }

    #[test]
    fn test_trim_newlines_right() {
        let data = tmap! { "X" => "hello" };
        assert_eq!(
            exec("{{.X -}}\n\n  world", &data),
            "helloworld"
        );
    }

    #[test]
    fn test_trim_newlines_left() {
        let data = tmap! { "X" => "hello" };
        assert_eq!(
            exec("world\n\n  {{- .X}}", &data),
            "worldhello"
        );
    }

    #[test]
    fn test_trim_newlines_both() {
        let data = tmap! { "X" => "hello" };
        assert_eq!(
            exec("A \n\t {{- .X -}} \n\t B", &data),
            "AhelloB"
        );
    }

    #[test]
    fn test_trim_multiline_template() {
        let data = tmap! {
            "Items" => vec!["one".to_string(), "two".to_string()],
        };
        assert_eq!(
            exec("Items:{{range .Items}}\n- {{.}}{{end}}", &data),
            "Items:\n- one\n- two"
        );
        assert_eq!(
            exec("Items:\n{{- range .Items}}\n- {{.}}{{end}}", &data),
            "Items:\n- one\n- two"
        );
    }

    #[test]
    fn test_comment() {
        assert_eq!(exec("hello{{/* this is a comment */}} world", &Value::Nil), "hello world");
    }

    #[test]
    fn test_break_in_range() {
        let data = tmap! { "Items" => vec![1i64, 2, 3, 4, 5] };
        assert_eq!(
            exec("{{range .Items}}{{if eq . 3}}{{break}}{{end}}{{.}} {{end}}", &data),
            "1 2 "
        );
    }

    #[test]
    fn test_continue_in_range() {
        let data = tmap! { "Items" => vec![1i64, 2, 3, 4, 5] };
        assert_eq!(
            exec("{{range .Items}}{{if eq . 3}}{{continue}}{{end}}{{.}} {{end}}", &data),
            "1 2 4 5 "
        );
    }
}
