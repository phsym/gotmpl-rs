//! Template execution engine. Walks the AST and writes output.
//!
//! The [`Executor`] evaluates a parsed template tree against a [`Value`] data
//! context, writing results to any [`core::fmt::Write`] destination.
//!
//! This module is used internally by [`Template::execute`](crate::Template::execute);
//! most users don't need to interact with it directly.
//!
//! # Execution model
//!
//! The executor maintains:
//! - **dot**: the current context value (changes inside `range`/`with`)
//! - **`$`**: always refers to the root data passed to [`execute`](Executor::execute)
//! - **variable scopes**: a stack of name-to-[`Value`] frames, pushed/popped for control blocks
//! - **recursion depth**: prevents stack overflow from recursive `{{template}}` calls

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

#[cfg(feature = "std")]
use std::any::Any;
#[cfg(feature = "std")]
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::error::{Result, TemplateError};
use crate::funcs::Func;
use crate::parse::{BranchNode, CommandNode, Expr, ListNode, Node, Number, PipeNode, TemplateNode};
use crate::value::Value;

/// Maximum total recursion depth across `{{template}}` invocations and
/// nested `{{if}}`/`{{with}}`/`{{range}}` bodies during execution.
///
/// Go uses 100,000; Rust's default thread stack is smaller (2 MB for test
/// threads). 200 gives comfortable headroom on a 2 MB stack and is far
/// beyond any reasonable template nesting.
const MAX_EXEC_DEPTH: usize = 200;

/// Controls behavior when accessing a missing key on a [`Value::Map`].
///
/// Set via [`Template::missing_key`](crate::Template::missing_key).
///
/// Supports parsing from strings via [`FromStr`](core::str::FromStr):
/// `"invalid"`, `"default"`, `"zero"`, and `"error"`.
///
/// ```
/// use gotmpl::MissingKey;
///
/// let mk: MissingKey = "error".parse().unwrap();
/// assert_eq!(mk, MissingKey::Error);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum MissingKey {
    /// Return [`Value::Nil`] for missing keys (the default).
    #[default]
    Invalid,
    /// Return [`Value::Nil`] for missing keys (same as `Invalid`).
    ZeroValue,
    /// Return a [`TemplateError::Exec`] for missing keys.
    Error,
}

impl core::fmt::Display for MissingKey {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MissingKey::Invalid => f.write_str("invalid"),
            MissingKey::ZeroValue => f.write_str("zero"),
            MissingKey::Error => f.write_str("error"),
        }
    }
}

impl core::str::FromStr for MissingKey {
    type Err = TemplateError;

    fn from_str(s: &str) -> core::result::Result<Self, Self::Err> {
        match s {
            "invalid" | "default" => Ok(MissingKey::Invalid),
            "zero" => Ok(MissingKey::ZeroValue),
            "error" => Ok(MissingKey::Error),
            _ => Err(TemplateError::Exec(format!(
                "unrecognized missingkey value: {s:?}"
            ))),
        }
    }
}

// ─── Internal control-flow signaling ────────────────────────────────────
//
// `{{break}}` and `{{continue}}` are not errors, they're control-flow
// signals caught by the range walker. We keep them out of the public
// `TemplateError` enum by using a private error type for the executor's
// internal methods.

/// Private error type that carries either a real [`TemplateError`] or an
/// internal break/continue signal. Only the public [`Executor::execute`]
/// method converts this into a [`TemplateError`] for the caller.
///
/// The `TemplateError` is boxed to keep `ExecSignal` small (one word + tag)
/// so that deeply recursive `walk` calls don't blow the stack.
enum ExecSignal {
    /// A real template error to propagate to the caller.
    Err(Box<TemplateError>),
    /// `{{break}}`: exit the innermost range loop.
    Break,
    /// `{{continue}}`: skip to the next range iteration.
    Continue,
}

impl From<TemplateError> for ExecSignal {
    fn from(e: TemplateError) -> Self {
        ExecSignal::Err(Box::new(e))
    }
}

#[cfg(feature = "std")]
impl From<std::io::Error> for ExecSignal {
    fn from(e: std::io::Error) -> Self {
        ExecSignal::Err(Box::new(TemplateError::Io(e)))
    }
}

impl From<core::fmt::Error> for ExecSignal {
    fn from(_: core::fmt::Error) -> Self {
        ExecSignal::Err(Box::new(TemplateError::Write))
    }
}

/// Internal result alias used by the executor's private methods.
type ExecResult<T> = core::result::Result<T, ExecSignal>;

/// Variable scope: a stack of name→value mappings.
/// New scopes are pushed for range/with blocks.
struct VarScope {
    frames: Vec<BTreeMap<String, Value>>,
}

impl VarScope {
    fn new() -> Self {
        VarScope {
            frames: vec![BTreeMap::new()],
        }
    }

    fn push(&mut self) {
        self.frames.push(BTreeMap::new());
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

/// Default per-execution iteration budget across all `{{range}}` loops.
/// Caps attacker-controlled `{{range 10000000000}}` / nested-range DoS.
pub(crate) const DEFAULT_MAX_RANGE_ITERS: u64 = 10_000_000;

/// The template execution context.
///
/// Walks the AST produced by the [`Parser`](crate::parse::Parser), evaluating
/// pipelines, resolving variables, calling functions, and writing output.
///
/// Created internally by [`Template::execute`](crate::Template::execute).
pub struct Executor<'a> {
    funcs: &'a BTreeMap<String, Func>,
    templates: &'a BTreeMap<String, ListNode>,
    vars: VarScope,
    depth: usize,
    missing_key: MissingKey,
    range_iters_remaining: u64,
}

/// Returns a short human-readable name for an expression, used in error messages.
fn expr_name(expr: &Expr) -> &'static str {
    match expr {
        Expr::Dot(_) => "<.>",
        Expr::Field(_, _) => "<field>",
        Expr::Variable(_, _, _) => "<variable>",
        Expr::Identifier(_, _) => "<identifier>",
        Expr::String(_, _) => "<string>",
        Expr::Number(_, _) => "<number>",
        Expr::Bool(_, _) => "<bool>",
        Expr::Nil(_) => "<nil>",
        Expr::Pipe(_, _) => "<pipeline>",
        Expr::Chain(_, _, _) => "<chain>",
    }
}

/// Execute a range loop body with break/continue handling.
///
/// `iter` yields `(index_value, element_value)` pairs. The macro is
/// necessary because `break` / `continue` are loop-control keywords
/// that cannot live inside a regular function call.
macro_rules! range_loop {
    ($self:expr, $w:expr, $branch:expr, $iter:expr) => {
        for (idx_val, item) in $iter {
            if $self.range_iters_remaining == 0 {
                return Err(TemplateError::RangeIterLimit.into());
            }
            $self.range_iters_remaining -= 1;
            $self.vars.push();
            if $branch.pipe.is_assign {
                // Assignment form ($v = range ...): modify existing variables
                // in their original (outer) scope so they persist after the loop.
                if $branch.pipe.decl.len() == 1 {
                    $self.vars.assign(&$branch.pipe.decl[0], item.clone());
                } else if $branch.pipe.decl.len() >= 2 {
                    $self.vars.assign(&$branch.pipe.decl[0], idx_val);
                    $self.vars.assign(&$branch.pipe.decl[1], item.clone());
                }
            } else {
                // Declaration form ($v := range ...): create variables in the
                // per-iteration scope (pushed above).
                if $branch.pipe.decl.len() == 1 {
                    $self.vars.set(&$branch.pipe.decl[0], item.clone());
                } else if $branch.pipe.decl.len() >= 2 {
                    $self.vars.set(&$branch.pipe.decl[0], idx_val);
                    $self.vars.set(&$branch.pipe.decl[1], item.clone());
                }
            }
            match $self.walk($w, &$branch.body, &item) {
                Ok(()) => {}
                Err(ExecSignal::Break) => {
                    $self.vars.pop();
                    break;
                }
                Err(ExecSignal::Continue) => {
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
        funcs: &'a BTreeMap<String, Func>,
        templates: &'a BTreeMap<String, ListNode>,
    ) -> Self {
        Executor {
            funcs,
            templates,
            vars: VarScope::new(),
            depth: 0,
            missing_key: MissingKey::default(),
            range_iters_remaining: DEFAULT_MAX_RANGE_ITERS,
        }
    }

    /// Set the [`MissingKey`] behavior for this executor.
    pub fn set_missing_key(&mut self, mk: MissingKey) {
        self.missing_key = mk;
    }

    /// Set the total number of `{{range}}` iterations allowed for this
    /// execution (across all nested ranges). Zero disables the limit.
    pub fn set_max_range_iters(&mut self, n: u64) {
        self.range_iters_remaining = if n == 0 { u64::MAX } else { n };
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
        // Set $ to the initial dot value (Go does this)
        self.vars.set("$", dot.clone());
        self.walk(writer, tree, dot).map_err(|sig| match sig {
            ExecSignal::Err(e) => *e,
            ExecSignal::Break => TemplateError::Exec("unexpected break outside of range".into()),
            ExecSignal::Continue => {
                TemplateError::Exec("unexpected continue outside of range".into())
            }
        })
    }

    // ─── AST walker ──────────────────────────────────────────────────

    fn walk<W: Write>(&mut self, w: &mut W, list: &ListNode, dot: &Value) -> ExecResult<()> {
        for node in &list.nodes {
            self.walk_node(w, node, dot)?;
        }
        Ok(())
    }

    fn walk_node<W: Write>(&mut self, w: &mut W, node: &Node, dot: &Value) -> ExecResult<()> {
        match node {
            Node::Text(text) => {
                w.write_str(&text.text)?;
                Ok(())
            }
            Node::Action(action) => {
                let val = self.eval_pipeline(dot, &action.pipe)?;
                // Only print if there are no declarations (side-effect-only)
                if action.pipe.decl.is_empty() {
                    // Go's template engine prints "<no value>" for nil/missing
                    // values, distinct from fmt.Sprint(nil) which prints "<nil>".
                    if matches!(val, Value::Nil) {
                        w.write_str("<no value>")?;
                    } else {
                        write!(w, "{}", val)?;
                    }
                }
                Ok(())
            }
            Node::If(branch) => self.walk_if(w, branch, dot),
            Node::Range(branch) => self.walk_range(w, branch, dot),
            Node::With(branch) => self.walk_with(w, branch, dot),
            Node::Template(tmpl) => self.walk_template(w, tmpl, dot),
            Node::Define(_) => Ok(()), // defines are collected at parse time
            Node::List(list) => self.walk(w, list, dot),
            Node::Break(_) => Err(ExecSignal::Break),
            Node::Continue(_) => Err(ExecSignal::Continue),
        }
    }

    // ─── Control flow ────────────────────────────────────────────────

    fn check_depth(&self) -> ExecResult<()> {
        if self.depth > MAX_EXEC_DEPTH {
            return Err(TemplateError::RecursionLimit.into());
        }
        Ok(())
    }

    fn walk_if<W: Write>(&mut self, w: &mut W, branch: &BranchNode, dot: &Value) -> ExecResult<()> {
        self.depth += 1;
        let depth_ok = self.check_depth();
        self.vars.push();
        let result = depth_ok.and_then(|()| match self.eval_pipeline(dot, &branch.pipe) {
            Ok(val) => {
                if val.is_truthy() {
                    self.walk(w, &branch.body, dot)
                } else if let Some(ref else_body) = branch.else_body {
                    self.walk(w, else_body, dot)
                } else {
                    Ok(())
                }
            }
            Err(e) => Err(e),
        });
        self.vars.pop();
        self.depth -= 1;
        result
    }

    fn walk_with<W: Write>(
        &mut self,
        w: &mut W,
        branch: &BranchNode,
        dot: &Value,
    ) -> ExecResult<()> {
        self.depth += 1;
        let depth_ok = self.check_depth();
        self.vars.push();
        let result = depth_ok.and_then(|()| match self.eval_pipeline(dot, &branch.pipe) {
            Ok(val) => {
                if val.is_truthy() {
                    self.walk(w, &branch.body, &val)
                } else if let Some(ref else_body) = branch.else_body {
                    self.walk(w, else_body, dot)
                } else {
                    Ok(())
                }
            }
            Err(e) => Err(e),
        });
        self.vars.pop();
        self.depth -= 1;
        result
    }

    fn walk_range<W: Write>(
        &mut self,
        w: &mut W,
        branch: &BranchNode,
        dot: &Value,
    ) -> ExecResult<()> {
        self.depth += 1;
        let result = self.check_depth().and_then(|()| self.walk_range_inner(w, branch, dot));
        self.depth -= 1;
        result
    }

    fn walk_range_inner<W: Write>(
        &mut self,
        w: &mut W,
        branch: &BranchNode,
        dot: &Value,
    ) -> ExecResult<()> {
        // `range` binds its own per-iteration vars via `range_loop!`; use the
        // decl-free evaluator so pipe.decl names do not leak into the outer
        // scope or get pre-bound to the pipeline's final value.
        let val = self.eval_pipeline_value(dot, &branch.pipe)?;

        match &val {
            Value::List(items) if items.is_empty() => {
                if let Some(ref else_body) = branch.else_body {
                    self.walk(w, else_body, dot)?;
                }
            }
            Value::List(items) => {
                range_loop!(
                    self,
                    w,
                    branch,
                    items
                        .iter()
                        .enumerate()
                        .map(|(i, v)| (Value::Int(i as i64), v.clone()))
                );
            }
            Value::Map(map) if map.is_empty() => {
                if let Some(ref else_body) = branch.else_body {
                    self.walk(w, else_body, dot)?;
                }
            }
            Value::Map(map) => {
                range_loop!(
                    self,
                    w,
                    branch,
                    map.iter()
                        .map(|(k, v)| (Value::String(Arc::clone(k)), v.clone()))
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
                    range_loop!(
                        self,
                        w,
                        branch,
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
                return Err(TemplateError::NotIterable(other.type_name().to_string()).into());
            }
        }

        Ok(())
    }

    fn walk_template<W: Write>(
        &mut self,
        w: &mut W,
        tmpl: &TemplateNode,
        dot: &Value,
    ) -> ExecResult<()> {
        self.depth += 1;
        if self.depth > MAX_EXEC_DEPTH {
            self.depth -= 1;
            return Err(TemplateError::RecursionLimit.into());
        }

        let new_dot = if let Some(ref pipe) = tmpl.pipe {
            self.eval_pipeline(dot, pipe)?
        } else {
            dot.clone()
        };

        let tree = self
            .templates
            .get(tmpl.name.as_ref())
            .ok_or_else(|| TemplateError::UndefinedTemplate(tmpl.name.to_string()))?
            .clone();

        self.vars.push();
        self.vars.set("$", new_dot.clone());
        let result = self.walk(w, &tree, &new_dot);
        self.vars.pop();
        self.depth -= 1;
        result
    }

    // ─── Pipeline and expression evaluation ──────────────────────────

    fn eval_pipeline_value(&mut self, dot: &Value, pipe: &PipeNode) -> ExecResult<Value> {
        let mut val = Value::Nil;
        for (i, cmd) in pipe.commands.iter().enumerate() {
            let prev = if i > 0 { Some(val.clone()) } else { None };
            val = self.eval_command(dot, cmd, prev)?;
        }
        Ok(val)
    }

    fn eval_pipeline(&mut self, dot: &Value, pipe: &PipeNode) -> ExecResult<Value> {
        let val = self.eval_pipeline_value(dot, pipe)?;
        match pipe.decl.len() {
            0 => {}
            1 => {
                let name = &pipe.decl[0];
                if pipe.is_assign {
                    self.vars.assign(name, val.clone());
                } else {
                    self.vars.set(name, val.clone());
                }
            }
            n => {
                return Err(TemplateError::Exec(format!(
                    "cannot assign {} variables outside a range pipeline",
                    n
                ))
                .into());
            }
        }
        Ok(val)
    }

    fn eval_command(
        &mut self,
        dot: &Value,
        cmd: &CommandNode,
        piped: Option<Value>,
    ) -> ExecResult<Value> {
        let first = &cmd.args[0];

        // If the first arg is an identifier, it's a function call
        if let Expr::Identifier(_, name) = first {
            // Special-case and/or for short-circuit evaluation
            if name.as_ref() == "and" || name.as_ref() == "or" {
                return self.eval_short_circuit(dot, name, &cmd.args[1..], piped);
            }
            return self.eval_function_call(dot, name, &cmd.args[1..], piped);
        }

        // If the first arg is a Chain with an Identifier inside, it's a function call
        // with field access on the result (e.g., mapOfThree.three)
        if let Expr::Chain(_, inner, fields) = first
            && let Expr::Identifier(_, name) = inner.as_ref()
        {
            if name.as_ref() == "and" || name.as_ref() == "or" {
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

        // Non-function command: cannot accept piped values or extra arguments.
        // Go errors with "can't give argument to non-function".
        if piped.is_some() || cmd.args.len() > 1 {
            return Err(TemplateError::Exec(format!(
                "can't give argument to non-function {}",
                expr_name(first)
            ))
            .into());
        }

        // Go: nil is not a valid command. It cannot be printed or used
        // as a pipeline value. It may only appear as an argument to a function.
        if matches!(first, Expr::Nil(_)) {
            return Err(TemplateError::Exec("nil is not a command".into()).into());
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
    ) -> ExecResult<Value> {
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
            }
            .into());
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
    ) -> ExecResult<Value> {
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

        self.invoke_func(name, func, &args)
    }

    /// Access a field on a value, respecting missingkey option.
    fn field_access(&self, val: &Value, name: &str) -> ExecResult<Value> {
        match val.field(name) {
            Some(v) => Ok(v.clone()),
            None if matches!(val, Value::Map(_)) => match self.missing_key {
                MissingKey::Error => {
                    Err(TemplateError::Exec(format!("map has no entry for key {:?}", name)).into())
                }
                _ => Ok(Value::Nil),
            },
            None if matches!(val, Value::Nil) => Ok(Value::Nil),
            None => Err(TemplateError::Exec(format!(
                "can't evaluate field {} in type {}",
                name,
                val.type_name()
            ))
            .into()),
        }
    }

    #[cfg(feature = "std")]
    fn invoke_func(&self, name: &str, func: &Func, args: &[Value]) -> ExecResult<Value> {
        match catch_unwind(AssertUnwindSafe(|| func(args))) {
            Ok(result) => result.map_err(ExecSignal::from),
            Err(payload) => Err(TemplateError::FuncPanic {
                name: name.to_string(),
                message: panic_payload_to_string(payload),
            }
            .into()),
        }
    }

    #[cfg(not(feature = "std"))]
    fn invoke_func(&self, _name: &str, func: &Func, args: &[Value]) -> ExecResult<Value> {
        func(args).map_err(ExecSignal::from)
    }

    fn eval_expr(&mut self, dot: &Value, expr: &Expr) -> ExecResult<Value> {
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
                    .ok_or_else(|| TemplateError::UndefinedVariable(name.to_string()))?;
                let mut result = val;
                for field in fields {
                    result = self.field_access(&result, field)?;
                }
                Ok(result)
            }

            Expr::String(_, s) => Ok(Value::String(Arc::clone(s))),

            Expr::Number(_, n) => match *n {
                Number::Int(i) => Ok(Value::Int(i)),
                Number::Float(f) => Ok(Value::Float(f)),
            },

            Expr::Bool(_, b) => Ok(Value::Bool(*b)),
            Expr::Nil(_) => Ok(Value::Nil),

            Expr::Pipe(_, pipe) => self.eval_pipeline(dot, pipe),

            Expr::Identifier(_, name) => {
                // Bare identifier, could be a zero-arg function call
                if let Some(func) = self.funcs.get(name.as_ref()) {
                    return self.invoke_func(name, func, &[]);
                }
                Err(TemplateError::UndefinedFunction(name.to_string()).into())
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

#[cfg(feature = "std")]
fn panic_payload_to_string(payload: Box<dyn Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_string()
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
        let mut templates: BTreeMap<String, ListNode> = BTreeMap::new();
        for def in &defines {
            templates.insert(def.name.to_string(), def.body.clone());
        }

        let mut executor = Executor::new(&funcs, &templates);
        let mut buf = String::new();
        executor.execute(&mut buf, &tree, data).unwrap();
        buf
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
        assert_eq!(exec("{{$x := .Name}}Hello, {{$x}}!", &data), "Hello, Dave!");
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
        assert_eq!(exec("{{if .A}}{{if .B}}both{{end}}{{end}}", &data), "both");
    }

    #[test]
    fn test_html_escape() {
        let data = tmap! { "X" => "<b>bold</b>" };
        assert_eq!(exec("{{html .X}}", &data), "&lt;b&gt;bold&lt;/b&gt;");
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
        assert_eq!(exec("{{range .Items}}[{{.}}]{{end}}", &data), "[a][b][c]");
        assert_eq!(exec("{{range .Items -}} {{.}} {{- end}}", &data), "abc");
    }

    #[test]
    fn test_trim_newlines_right() {
        let data = tmap! { "X" => "hello" };
        assert_eq!(exec("{{.X -}}\n\n  world", &data), "helloworld");
    }

    #[test]
    fn test_trim_newlines_left() {
        let data = tmap! { "X" => "hello" };
        assert_eq!(exec("world\n\n  {{- .X}}", &data), "worldhello");
    }

    #[test]
    fn test_trim_newlines_both() {
        let data = tmap! { "X" => "hello" };
        assert_eq!(exec("A \n\t {{- .X -}} \n\t B", &data), "AhelloB");
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
        assert_eq!(
            exec("hello{{/* this is a comment */}} world", &Value::Nil),
            "hello world"
        );
    }

    #[test]
    fn test_break_in_range() {
        let data = tmap! { "Items" => vec![1i64, 2, 3, 4, 5] };
        assert_eq!(
            exec(
                "{{range .Items}}{{if eq . 3}}{{break}}{{end}}{{.}} {{end}}",
                &data
            ),
            "1 2 "
        );
    }

    #[test]
    fn test_continue_in_range() {
        let data = tmap! { "Items" => vec![1i64, 2, 3, 4, 5] };
        assert_eq!(
            exec(
                "{{range .Items}}{{if eq . 3}}{{continue}}{{end}}{{.}} {{end}}",
                &data
            ),
            "1 2 4 5 "
        );
    }
}
