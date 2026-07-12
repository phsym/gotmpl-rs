//! The context-aware escaping pass — port of Go's `escape.go`.
//!
//! [`escape`] rewrites a parsed template so that every printing action is
//! wrapped in the escaper(s) required by its surrounding HTML/URL/JS/CSS
//! context, literal text is normalized (`<` in text becomes `&lt;`, comments
//! are elided, `<script>`/`</script>`/`<!--` inside JS literals are neutralized,
//! …), and each cross-context `{{template}}` call is retargeted at a
//! context-specialized ("mangled") clone of the callee. It mirrors Go's
//! `escaper` closely; the differences are limited to Rust's ownership model and
//! are documented below.
//!
//! # Edit strategy (vs. Go's deferred `commit`)
//!
//! Go records edits in node-pointer-keyed maps and applies them in `commit()`
//! because a tree may be walked several times (to find the output context of a
//! recursive template, and to check `{{range}}` loop re-entry) before its
//! context is confirmed. Rust has no stable node pointers and forbids `unsafe`,
//! so instead the [`Escaper`] OWNS a fresh clone of each tree it escapes and
//! mutates it in place during a single **commit** walk. The trial walks Go runs
//! with a throwaway escaper — `computeOutCtx`'s output-context probe and the
//! `{{range}}` re-entry check — run here in **measure** mode: a walk that
//! computes the resulting [`Context`] without touching any node. Escaper
//! selection is a pure function of the context, and context transitions depend
//! only on the literal text plus *the fact* that an action interpolates
//! (not on which escaper was chosen), so measure and commit compute identical
//! contexts and the committed output matches Go's.
//!
//! # Escape-set strategy for [`escape`]
//!
//! Go escapes lazily, per `Execute`/`ExecuteTemplate`. This crate escapes once,
//! eagerly, into an [`EscapeSet`] cached on first execute. The choices:
//!
//! - The **entry** tree (`inner.root_tree()`) is escaped from [`Context::text`].
//!   If it ends in a non-text context this is [`EscapeErrorCode::EndContext`]
//!   and the whole set fails — matching Go's refusal to `Execute` such a
//!   template.
//! - Every **`{{define}}`** is also escaped from text as a *base* version so
//!   [`execute_template`](super::Template::execute_template) can run it. If a
//!   define cannot be escaped from text (it errors, or ends in a non-text
//!   context because it is a fragment only meant to be `{{template}}`-included
//!   mid-tag) its base is *omitted* rather than aborting the whole set — unless
//!   it was already committed as part of the entry's reachable set, in which
//!   case it is retained for the entry's use. Such a define remains reachable
//!   through its context-derived (mangled) clones for `{{template}}` inclusion.
//!   Divergence from Go: a mid-tag-only define is rejected for a *direct*
//!   `execute_template` as [`TemplateError::UndefinedTemplate`] rather than
//!   Go's lazy `ErrEndContext`.
//! - All context-derived clones produced while escaping the entry and the bases
//!   are retained under their mangled names.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write as _;

use crate::error::{EscapeErrorCode, Result, TemplateError};
use crate::parse::{
    ActionNode, BranchNode, CommandNode, Expr, ListNode, Node, PipeNode, Pos, SmolStr,
    TemplateNode, TextNode,
};
use crate::value::ValueFunc;

use super::context::{Attr, Context, Delim, Element, JsCtx, State, UrlPart, write_int_slice};
use super::lex::{delim_ends, index_any, index_byte, index_js_line_terminator};
use super::transition::{special_tag_end, transition};

// The exact `escape.go` funcMap escaper names appended to pipelines.
const HTMLESCAPER: &str = "_html_template_htmlescaper";
const ATTRESCAPER: &str = "_html_template_attrescaper";
const RCDATAESCAPER: &str = "_html_template_rcdataescaper";
const NOSPACEESCAPER: &str = "_html_template_nospaceescaper";
const COMMENTESCAPER: &str = "_html_template_commentescaper";
const HTMLNAMEFILTER: &str = "_html_template_htmlnamefilter";
const CSSESCAPER: &str = "_html_template_cssescaper";
const CSSVALUEFILTER: &str = "_html_template_cssvaluefilter";
const JSVALESCAPER: &str = "_html_template_jsvalescaper";
const JSSTRESCAPER: &str = "_html_template_jsstrescaper";
const JSTMPLLITESCAPER: &str = "_html_template_jstmpllitescaper";
const JSREGEXPESCAPER: &str = "_html_template_jsregexpescaper";
const SRCSETESCAPER: &str = "_html_template_srcsetescaper";
const URLESCAPER: &str = "_html_template_urlescaper";
const URLFILTER: &str = "_html_template_urlfilter";
const URLNORMALIZER: &str = "_html_template_urlnormalizer";

/// The escaped tree set produced by [`escape`] and cached on a
/// [`Template`](super::Template): the entry tree, all named templates (base
/// plus context-derived clones), and the funcmap with the escaper functions
/// merged in.
pub(crate) struct EscapeSet {
    pub(crate) entry: Arc<ListNode>,
    pub(crate) templates: BTreeMap<String, Arc<ListNode>>,
    pub(crate) funcs: Arc<BTreeMap<String, ValueFunc>>,
}

/// A captured escaping error, mirroring Go's `context.err`. Carried on the
/// [`Escaper`] and materialized into a [`TemplateError::Escape`] by the caller
/// (which supplies the template name).
struct EscErr {
    code: EscapeErrorCode,
    line: usize,
    description: String,
}

impl EscErr {
    /// An error with no source line yet; the caller annotates it from the
    /// offending node. Used by the `classify_*` helpers, which lack position
    /// information.
    fn at0(code: EscapeErrorCode, description: String) -> Self {
        EscErr {
            code,
            line: 0,
            description,
        }
    }

    fn into_error(self, name: Option<String>) -> TemplateError {
        TemplateError::Escape {
            code: self.code,
            name,
            line: self.line,
            description: self.description,
        }
    }
}

/// Whether the current walk mutates nodes (`Commit`) or only computes contexts
/// (`Measure`). See the module docs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Measure,
    Commit,
}

/// The branch kind, for `{{if}}`/`{{range}}`/`{{with}}`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    If,
    Range,
    With,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::If => "if",
            Kind::Range => "range",
            Kind::With => "with",
        }
    }
}

/// Per-`{{range}}` collector for the contexts at `{{break}}`/`{{continue}}`
/// actions (Go's `rangeContext`). Each entry pairs the context with the source
/// line for error annotation.
#[derive(Default)]
struct RangeCtx {
    breaks: Vec<(Context, usize)>,
    continues: Vec<(Context, usize)>,
}

/// Collects type inferences and applies the edits needed to make a template
/// set injection-safe. Port of Go's `escaper`.
struct Escaper {
    /// Base source trees to read from: the `{{define}}`s plus the entry tree
    /// under the entry name.
    source: BTreeMap<String, Arc<ListNode>>,
    /// Mangled name → output context. Presence means "escaping has started",
    /// so recursive calls take the fast path.
    output: BTreeMap<String, Context>,
    /// Mangled name → start context used to commit its body.
    start_ctx: BTreeMap<String, Context>,
    /// Recursion guard: mangled names seen during the current walk.
    called: BTreeSet<String>,
    /// Mangled name → fully escaped tree (Go's `derived` plus the escaped
    /// bases). Becomes [`EscapeSet::templates`].
    escaped: BTreeMap<String, ListNode>,
    /// Mangled names whose escaped tree has been committed.
    committed: BTreeSet<String>,
    /// Stack of the enclosing `{{range}}` loop collectors.
    range_stack: Vec<RangeCtx>,
    /// The first error encountered (Go's `context.err`).
    err: Option<EscErr>,
    /// Current walk mode.
    mode: Mode,
}

impl Escaper {
    fn new(source: BTreeMap<String, Arc<ListNode>>) -> Self {
        Escaper {
            source,
            output: BTreeMap::new(),
            start_ctx: BTreeMap::new(),
            called: BTreeSet::new(),
            escaped: BTreeMap::new(),
            committed: BTreeSet::new(),
            range_stack: Vec::new(),
            err: None,
            mode: Mode::Commit,
        }
    }

    // -- node dispatch (Go's escape / escapeList) ---------------------------

    /// Port of Go's `escapeList`: thread the context through a node list,
    /// stopping at a dead (post-`break`/`continue`) context.
    fn escape_list(&mut self, mut c: Context, list: &mut ListNode) -> Context {
        for i in 0..list.nodes.len() {
            c = self.escape_node(c, &mut list.nodes[i]);
            if c.state == State::Dead {
                break;
            }
        }
        c
    }

    /// A measure-mode `escapeList` over a borrowed list: computes the output
    /// context without mutating (walks a throwaway clone).
    fn measure_list(&mut self, c: Context, list: &ListNode) -> Context {
        let mut tmp = list.clone();
        let saved = self.mode;
        self.mode = Mode::Measure;
        let out = self.escape_list(c, &mut tmp);
        self.mode = saved;
        out
    }

    /// Port of Go's `escape` dispatch.
    fn escape_node(&mut self, c: Context, node: &mut Node) -> Context {
        match node {
            Node::Text(t) => self.escape_text(c, t),
            Node::Action(a) => self.escape_action(c, a),
            Node::If(b) => self.escape_branch(c, b, Kind::If),
            Node::Range(b) => self.escape_branch(c, b, Kind::Range),
            Node::With(b) => self.escape_branch(c, b, Kind::With),
            Node::Template(t) => self.escape_template(c, t),
            Node::List(l) => self.escape_list(c, l),
            // Defines are escaped as their own entries, not inline.
            Node::Define(_) => c,
            Node::Break(pos) => {
                let line = pos.line;
                if let Some(top) = self.range_stack.last_mut() {
                    top.breaks.push((c, line));
                }
                Context::in_state(State::Dead)
            }
            Node::Continue(pos) => {
                let line = pos.line;
                if let Some(top) = self.range_stack.last_mut() {
                    top.continues.push((c, line));
                }
                Context::in_state(State::Dead)
            }
        }
    }

    // -- actions (Go's escapeAction / ensurePipelineContains) ---------------

    /// Port of Go's `escapeAction`.
    fn escape_action(&mut self, c_in: Context, action: &mut ActionNode) -> Context {
        if !action.pipe.decl.is_empty() {
            // A local variable assignment, not an interpolation.
            return c_in;
        }
        let mut c = c_in.nudge();

        // Reject predefined escapers used where they are disallowed.
        let ncmds = action.pipe.commands.len();
        let mut bad_escaper: Option<String> = None;
        for (idx, cmd) in action.pipe.commands.iter().enumerate() {
            if let Some(Expr::Identifier(_, ident)) = cmd.args.first()
                && is_predefined_escaper(ident.as_str())
            {
                let is_last = idx + 1 == ncmds;
                let html_unquoted = c.state == State::Attr
                    && c.delim == Delim::SpaceOrTagEnd
                    && ident.as_str() == "html";
                if !is_last || html_unquoted {
                    bad_escaper = Some(ident.as_str().to_string());
                    break;
                }
            }
        }
        if let Some(ident) = bad_escaper {
            self.set_err(EscErr {
                code: EscapeErrorCode::PredefinedEscaper,
                line: action.pipe.pos.line,
                description: format!(
                    "predefined escaper {} disallowed in template",
                    go_quote(ident.as_bytes())
                ),
            });
            return Context::error();
        }

        let mut s: Vec<&'static str> = Vec::with_capacity(3);
        match c.state {
            State::Error => return c,
            State::Url
            | State::CssDqStr
            | State::CssSqStr
            | State::CssDqUrl
            | State::CssSqUrl
            | State::CssUrl => match c.url_part {
                UrlPart::None => {
                    s.push(URLFILTER);
                    if matches!(c.state, State::CssDqStr | State::CssSqStr) {
                        s.push(CSSESCAPER);
                    } else {
                        s.push(URLNORMALIZER);
                    }
                }
                UrlPart::PreQuery => {
                    if matches!(c.state, State::CssDqStr | State::CssSqStr) {
                        s.push(CSSESCAPER);
                    } else {
                        s.push(URLNORMALIZER);
                    }
                }
                UrlPart::QueryOrFrag => s.push(URLESCAPER),
                UrlPart::Unknown => {
                    self.set_err(EscErr {
                        code: EscapeErrorCode::AmbigContext,
                        line: action.pipe.pos.line,
                        description: String::from(
                            "{{...}} appears in an ambiguous context within a URL",
                        ),
                    });
                    return Context::error();
                }
            },
            State::MetaContent => {} // handled by the delim check below
            State::MetaContentUrl => s.push(URLFILTER),
            State::Js => {
                s.push(JSVALESCAPER);
                // A slash after a value starts a div operator.
                c.js_ctx = JsCtx::DivOp;
            }
            State::JsDqStr | State::JsSqStr => s.push(JSSTRESCAPER),
            State::JsTmplLit => s.push(JSTMPLLITESCAPER),
            State::JsRegexp => s.push(JSREGEXPESCAPER),
            State::Css => s.push(CSSVALUEFILTER),
            State::Text => s.push(HTMLESCAPER),
            State::Rcdata => s.push(RCDATAESCAPER),
            State::Attr => {} // handled by the delim check below
            State::AttrName | State::Tag => {
                c.state = State::AttrName;
                s.push(HTMLNAMEFILTER);
            }
            State::Srcset => s.push(SRCSETESCAPER),
            other => {
                if other.is_comment() {
                    s.push(COMMENTESCAPER);
                } else {
                    // Go panics ("unexpected state"); unreachable for a valid
                    // template because every printing context is covered above.
                    return Context::error();
                }
            }
        }
        match c.delim {
            Delim::None => {}
            Delim::SpaceOrTagEnd => s.push(NOSPACEESCAPER),
            _ => s.push(ATTRESCAPER),
        }
        if self.mode == Mode::Commit {
            ensure_pipeline_contains(&mut action.pipe, &s);
        }
        c
    }

    // -- branches (Go's escapeBranch / joinRange) ---------------------------

    /// Port of Go's `escapeBranch`.
    fn escape_branch(&mut self, c: Context, branch: &mut BranchNode, kind: Kind) -> Context {
        if kind == Kind::Range {
            self.range_stack.push(RangeCtx::default());
        }
        let mut c0 = self.escape_list(c.clone(), &mut branch.body);
        if kind == Kind::Range {
            if c0.state != State::Error {
                c0 = self.join_range_stack(c0, "range");
            }
            self.range_stack.pop();
            if c0.state == State::Error {
                return c0;
            }

            // The "true" branch of a range can run more than once: escaping the
            // body once must produce the same context as escaping it twice.
            self.range_stack.push(RangeCtx::default());
            let c1 = self.measure_list(c0.clone(), &branch.body);
            let joined = if c1.state == State::Error {
                None
            } else {
                Context::join(c0.clone(), c1.clone())
            };
            match joined {
                Some(j) => c0 = j,
                None => {
                    // Go's `escapeBranch` derives this failure from
                    // `join(c0, c1, …)`, which yields `ErrBranchEnd`, then only
                    // prepends "on range loop re-entry: " to the description —
                    // the *code* stays `ErrBranchEnd`. When the second-pass
                    // measurement itself recorded a transition error, `join`
                    // propagates that error's code instead, so honor a recorded
                    // `self.err` first.
                    let (code, inner) = match self.err.take() {
                        Some(e) => (e.code, e.description),
                        None => (
                            EscapeErrorCode::BranchEnd,
                            self.branch_end_desc("range", &c0, &c1),
                        ),
                    };
                    self.set_err(EscErr {
                        code,
                        line: branch.pos.line,
                        description: format!("on range loop re-entry: {inner}"),
                    });
                    self.range_stack.pop();
                    return Context::error();
                }
            }
            if c0.state != State::Error {
                c0 = self.join_range_stack(c0, "range");
            }
            self.range_stack.pop();
            if c0.state == State::Error {
                return c0;
            }
        }

        let c1 = match &mut branch.else_body {
            Some(eb) => self.escape_list(c.clone(), eb),
            None => c.clone(),
        };
        if c0.state == State::Error || c1.state == State::Error {
            // The error is already recorded on self.err.
            return Context::error();
        }
        match Context::join(c0.clone(), c1.clone()) {
            Some(j) => j,
            None => {
                let desc = self.branch_end_desc(kind.name(), &c0, &c1);
                self.set_err(EscErr {
                    code: EscapeErrorCode::BranchEnd,
                    line: branch.pos.line,
                    description: desc,
                });
                Context::error()
            }
        }
    }

    /// Port of Go's `joinRange`: fold the break/continue contexts of the
    /// innermost range into the body context `c0`. Uses the pure
    /// [`Context::join_range`] for the success path and, on failure, replays
    /// the joins to locate the offending action for the annotated error.
    fn join_range_stack(&mut self, c0: Context, node_name: &str) -> Context {
        let (breaks, continues) = match self.range_stack.last() {
            Some(top) => (top.breaks.clone(), top.continues.clone()),
            None => return c0,
        };
        let break_ctxs: Vec<Context> = breaks.iter().map(|(c, _)| c.clone()).collect();
        let continue_ctxs: Vec<Context> = continues.iter().map(|(c, _)| c.clone()).collect();
        if let Some(joined) = Context::join_range(c0.clone(), &break_ctxs, &continue_ctxs) {
            return joined;
        }
        // Replay to find which break/continue broke the join, mirroring Go's
        // per-action error annotation.
        let mut acc = c0;
        for (bc, line) in &breaks {
            match Context::join(acc.clone(), bc.clone()) {
                Some(j) => acc = j,
                None => {
                    let desc = self.branch_end_desc(node_name, &acc, bc);
                    self.set_err(EscErr {
                        code: EscapeErrorCode::BranchEnd,
                        line: *line,
                        description: format!("at range loop break: {desc}"),
                    });
                    return Context::error();
                }
            }
        }
        for (cc, line) in &continues {
            match Context::join(acc.clone(), cc.clone()) {
                Some(j) => acc = j,
                None => {
                    let desc = self.branch_end_desc(node_name, &acc, cc);
                    self.set_err(EscErr {
                        code: EscapeErrorCode::BranchEnd,
                        line: *line,
                        description: format!("at range loop continue: {desc}"),
                    });
                    return Context::error();
                }
            }
        }
        Context::error()
    }

    fn branch_end_desc(&self, node_name: &str, a: &Context, b: &Context) -> String {
        let mut d = String::new();
        d.push_str("{{");
        d.push_str(node_name);
        d.push_str("}} branches end in different contexts: ");
        d.push_str(&context_string(a));
        d.push_str(", ");
        d.push_str(&context_string(b));
        d
    }

    // -- template calls (Go's escapeTemplate / escapeTree / computeOutCtx) --

    /// Port of Go's `escapeTemplate`.
    fn escape_template(&mut self, c: Context, tmpl: &mut TemplateNode) -> Context {
        let (out, dname) = self.escape_tree(&c, tmpl.name.as_str(), tmpl.pos.line);
        if self.mode == Mode::Commit && dname.as_str() != tmpl.name.as_str() {
            tmpl.name = SmolStr::from(dname.as_str());
        }
        out
    }

    /// Port of Go's `escapeTree`: escape the named template starting in `c` and
    /// return its output context and mangled name.
    fn escape_tree(&mut self, c: &Context, name: &str, line: usize) -> (Context, String) {
        let dname = c.mangle(name);
        self.called.insert(dname.clone());

        if let Some(out) = self.output.get(&dname).cloned() {
            // Already escaped (or in progress). Commit the tree if we are in a
            // commit walk and have not yet materialized it.
            if self.mode == Mode::Commit && !self.committed.contains(&dname) {
                self.commit_tree(name, &dname);
            }
            return (out, dname);
        }

        if !self.source.contains_key(name) {
            self.set_err(EscErr {
                code: EscapeErrorCode::NoSuchTemplate,
                line,
                description: format!("no such template {}", go_quote(name.as_bytes())),
            });
            return (Context::error(), dname);
        }

        match self.compute_out_ctx(c, name, &dname) {
            Ok((assume, flow)) => {
                self.start_ctx.insert(dname.clone(), assume);
                // Cache the real output context (not the trial's assumption) so
                // the commit fast-path and later references flow correctly.
                self.output.insert(dname.clone(), flow.clone());
                if self.mode == Mode::Commit && !self.committed.contains(&dname) {
                    self.commit_tree(name, &dname);
                }
                (flow, dname)
            }
            Err(e) => {
                self.set_err(e);
                (Context::error(), dname)
            }
        }
    }

    /// Port of Go's `computeOutCtx`. Returns `(assumed_start, output)` where
    /// `assumed_start` is the context to commit the body from.
    fn compute_out_ctx(
        &mut self,
        c: &Context,
        name: &str,
        dname: &str,
    ) -> core::result::Result<(Context, Context), EscErr> {
        // Naively assume the output context equals the input context.
        let (c1, ok1, err1) = self.measure_body(c.clone(), name, dname, c.clone());
        if ok1 {
            return Ok((c.clone(), c1));
        }
        // Retry assuming c1 as the output (and start) context.
        let (c2, ok2, _err2) = self.measure_body(c1.clone(), name, dname, c1.clone());
        if ok2 {
            return Ok((c1, c2));
        }
        // If the first probe errored, that error (Go's returned c1.err) is
        // authoritative; otherwise the template has no fixed-point output.
        if c1.state == State::Error
            && let Some(e) = err1
        {
            return Err(e);
        }
        Err(EscErr {
            code: EscapeErrorCode::OutputContext,
            line: 0,
            description: format!("cannot compute output context for template {dname}"),
        })
    }

    /// Port of Go's `escapeTemplateBody` + `escapeListConditionally` (measure
    /// only): walk the body from `start` assuming `output[dname] == assume`,
    /// and report whether the assumption is accurate. No node is mutated; the
    /// inferences are kept only when the assumption holds.
    fn measure_body(
        &mut self,
        start: Context,
        name: &str,
        dname: &str,
        assume: Context,
    ) -> (Context, bool, Option<EscErr>) {
        let mut tmp = match self.source.get(name) {
            Some(t) => (**t).clone(),
            None => return (Context::error(), false, None),
        };
        let saved_output = self.output.clone();
        let saved_called = core::mem::take(&mut self.called);
        let saved_err = self.err.take();
        let saved_mode = self.mode;

        self.output.insert(dname.to_string(), assume.clone());
        self.mode = Mode::Measure;
        let c1 = self.escape_list(start, &mut tmp);
        self.mode = saved_mode;

        let recursed = self.called.contains(dname);
        // A trial must never leak its error into the shared slot: capture it as
        // the return value and always restore the pre-trial error.
        let measured_err = self.err.take();
        self.err = saved_err;
        let ok = if c1.state == State::Error {
            false
        } else if !recursed {
            // Not recursively called: c1 is an accurate output context.
            true
        } else {
            assume == c1
        };

        if ok {
            for k in saved_called {
                self.called.insert(k);
            }
        } else {
            self.output = saved_output;
            self.called = saved_called;
        }
        (c1, ok, measured_err)
    }

    /// Commit (materialize) the escaped tree for `dname`, mutating a fresh
    /// clone of the base template `name` in place.
    fn commit_tree(&mut self, name: &str, dname: &str) {
        self.committed.insert(dname.to_string());
        let start = self.start_ctx.get(dname).cloned().unwrap_or_default();
        let mut tree = match self.source.get(name) {
            Some(t) => (**t).clone(),
            None => return,
        };
        let saved_mode = self.mode;
        self.mode = Mode::Commit;
        let _ = self.escape_list(start, &mut tree);
        self.mode = saved_mode;
        self.escaped.insert(dname.to_string(), tree);
    }

    // -- text (Go's escapeText / contextAfterText) --------------------------

    /// Port of Go's `escapeText`: advance the context over a text node and, in
    /// commit mode, rewrite the node's literal text (escape stray `<`, elide
    /// comments, neutralize special script tags).
    fn escape_text(&mut self, mut c: Context, node: &mut TextNode) -> Context {
        let text = node.text.clone();
        let s = text.as_bytes();
        let mut b = String::new();
        let mut written = 0usize;
        let mut i = 0usize;

        while i != s.len() {
            let (c1, nread) = self.context_after_text(c.clone(), &s[i..]);
            let i1 = i + nread;

            if c.state == State::Text || c.state == State::Rcdata {
                let mut end = i1;
                if c1.state != c.state {
                    let mut j = end;
                    while j > i {
                        j -= 1;
                        if s[j] == b'<' {
                            end = j;
                            break;
                        }
                    }
                }
                let mut j = i;
                while j < end {
                    if s[j] == b'<' && !starts_with_doctype(&s[j..]) {
                        push_bytes(&mut b, &s[written..j]);
                        b.push_str("&lt;");
                        written = j + 1;
                    }
                    j += 1;
                }
            } else if c.state.is_comment() && c.delim == Delim::None {
                match c.state {
                    State::JsBlockCmt => {
                        // A block comment holding a line terminator acts as a
                        // line terminator; otherwise as whitespace.
                        if index_js_line_terminator(&s[written..i1]).is_some() {
                            b.push('\n');
                        } else {
                            b.push(' ');
                        }
                    }
                    State::CssBlockCmt => b.push(' '),
                    _ => {}
                }
                written = i1;
            }

            if c.state != c1.state && c1.state.is_comment() && c1.delim == Delim::None {
                // Preserve the text before the comment opener.
                let mut cs = i1.saturating_sub(2);
                if c1.state == State::HtmlCmt || c1.state == State::JsHtmlOpenCmt {
                    cs = cs.saturating_sub(2); // "<!--" instead of "/*"/"//"
                } else if c1.state == State::JsHtmlCloseCmt {
                    cs = cs.saturating_sub(1); // "-->" instead of "/*"/"//"
                }
                if cs >= written {
                    push_bytes(&mut b, &s[written..cs]);
                }
                written = i1;
            }

            if c.state.is_in_script_literal() && contains_special_script_tag(&s[i..i1]) {
                push_bytes(&mut b, &s[written..i]);
                b.push_str(&escape_special_script_tags(&s[i..i1]));
                written = i1;
            }

            c = c1;
            i = i1;
        }

        if written != 0 && c.state != State::Error {
            if !c.state.is_comment() || c.delim != Delim::None {
                push_bytes(&mut b, &s[written..]);
            }
            if self.mode == Mode::Commit {
                node.text = Arc::from(b.as_str());
            }
        }
        c
    }

    /// Port of Go's `contextAfterText`.
    fn context_after_text(&mut self, c: Context, s: &[u8]) -> (Context, usize) {
        if c.delim == Delim::None {
            let (c1, i) = special_tag_end(c.clone(), s);
            if i == 0 {
                // A special end tag was seen; all preceding content consumed.
                return (c1, 0);
            }
            return self.step(c, &s[..i]);
        }

        // We are at the beginning of an attribute value.
        let ends = delim_ends(c.delim);
        let mut i = index_any(s, ends).unwrap_or(s.len());
        if c.delim == Delim::SpaceOrTagEnd
            && let Some(j) = index_any(&s[..i], b"\"'<=`")
        {
            self.set_err(EscErr {
                code: EscapeErrorCode::BadHtml,
                line: 0,
                description: format!(
                    "{} in unquoted attr: {}",
                    go_quote(&s[j..=j]),
                    go_quote(&s[..i])
                ),
            });
            return (Context::error(), s.len());
        }
        if i == s.len() {
            // Remain inside the attribute; decode entities so non-HTML rules
            // handle token boundaries.
            let decoded = html_unescape(s);
            let mut u: &[u8] = &decoded;
            let mut cc = c;
            while !u.is_empty() {
                let (c1, i1) = self.step(cc.clone(), u);
                cc = c1;
                if i1 == 0 {
                    break;
                }
                u = &u[i1..];
            }
            return (cc, s.len());
        }

        let mut element = c.element;
        // A non-JS "type" attribute inside <script> makes the contents non-JS.
        if c.state == State::Attr
            && c.element == Element::Script
            && c.attr == Attr::ScriptType
            && !is_js_type(&s[..i])
        {
            element = Element::None;
        }
        if c.delim != Delim::SpaceOrTagEnd {
            i += 1; // consume the quote
        }
        (
            Context {
                state: State::Tag,
                element,
                ..Context::default()
            },
            i,
        )
    }

    /// A single `transition` step that classifies and captures the specific
    /// error when the transition drops into [`State::Error`].
    fn step(&mut self, c: Context, s: &[u8]) -> (Context, usize) {
        let (c1, n) = transition(c.clone(), s);
        // Only classify a *fresh* transition into the error state; a walk that
        // starts in stateError (e.g. the computeOutCtx retry) carries its error
        // already and must not be re-classified.
        if c.state != State::Error && c1.state == State::Error && self.err.is_none() {
            self.err = Some(classify_transition_error(&c, s));
        }
        (c1, n)
    }

    fn set_err(&mut self, e: EscErr) {
        if self.err.is_none() {
            self.err = Some(e);
        }
    }
}

/// Escape a parsed template into an [`EscapeSet`] ready for execution.
pub(crate) fn escape(inner: &crate::Template) -> Result<EscapeSet> {
    let entry_name = inner.name().to_string();
    let entry_tree = inner.root_tree().cloned().ok_or_else(|| {
        TemplateError::Exec(format!(
            "html/template: {:?} is an incomplete or empty template",
            inner.name()
        ))
    })?;

    let mut source: BTreeMap<String, Arc<ListNode>> = inner.define_map().clone();
    source
        .entry(entry_name.clone())
        .or_insert_with(|| Arc::new(entry_tree.clone()));
    let define_names: Vec<String> = inner.define_map().keys().cloned().collect();

    let mut esc = Escaper::new(source);

    // Escape the entry from the text (start) context; it must end in text.
    let (c, _dname) = esc.escape_tree(&Context::text(), &entry_name, 0);
    if let Some(e) = esc.err.take() {
        return Err(e.into_error(Some(entry_name)));
    }
    if c.state != State::Text {
        return Err(TemplateError::Escape {
            code: EscapeErrorCode::EndContext,
            name: Some(entry_name),
            line: 0,
            description: format!("ends in a non-text context: {}", context_string(&c)),
        });
    }

    // Escape each define from text as a base for execute_template.
    for name in &define_names {
        if name == &entry_name || esc.committed.contains(name) {
            continue;
        }
        esc.err = None;
        let (bc, _) = esc.escape_tree(&Context::text(), name, 0);
        let ok = esc.err.is_none() && bc.state == State::Text;
        esc.err = None;
        if !ok {
            // Omit a base that errors or ends in a non-text context.
            esc.escaped.remove(name);
        }
    }

    let entry = esc
        .escaped
        .get(&entry_name)
        .cloned()
        .map(Arc::new)
        .unwrap_or_else(|| Arc::new(entry_tree));
    let templates: BTreeMap<String, Arc<ListNode>> = esc
        .escaped
        .into_iter()
        .map(|(k, v)| (k, Arc::new(v)))
        .collect();
    let funcs = super::escapers::merge(inner.func_map());
    Ok(EscapeSet {
        entry,
        templates,
        funcs,
    })
}

// ---------------------------------------------------------------------------
// ensurePipelineContains and its dedup tables (Go escape.go:285-425).
// ---------------------------------------------------------------------------

/// Port of Go's `ensurePipelineContains`: idempotently append the escapers in
/// `s` at the end of the pipeline, merging with a trailing predefined escaper.
fn ensure_pipeline_contains(p: &mut PipeNode, s: &[&str]) {
    if s.is_empty() {
        return;
    }
    let mut pipeline_len = p.commands.len();
    let mut s_owned: Vec<String> = s.iter().map(|x| (*x).to_string()).collect();

    if pipeline_len > 0 {
        // The precondition (enforced by escapeAction) is that at most one
        // predefined escaper is present, and only as the last command.
        let esc = match p.commands[pipeline_len - 1].args.first() {
            Some(Expr::Identifier(_, id)) if is_predefined_escaper(id.as_str()) => {
                Some(id.as_str().to_string())
            }
            _ => None,
        };
        if let Some(esc) = esc {
            if p.commands.len() == 1 && p.commands[0].args.len() > 1 {
                // {{ esc arg1 ... argN }} -> {{ _eval_args_ arg1 ... argN | esc }}
                let pos = p.commands[0].args[0].pos();
                p.commands[0].args[0] = Expr::Identifier(pos, SmolStr::from("_eval_args_"));
                let cmd = new_ident_cmd(&esc, p.pos);
                p.commands.push(cmd);
                pipeline_len += 1;
            }
            // If any escaper in s is equivalent to the predefined escaper, use
            // the predefined name and drop the standalone copy.
            let mut dup = false;
            for item in &mut s_owned {
                if esc_fns_eq(&esc, item) {
                    *item = esc.clone();
                    dup = true;
                }
            }
            if dup {
                pipeline_len -= 1;
            }
        }
    }

    let mut new_cmds: Vec<CommandNode> = p.commands[..pipeline_len].to_vec();
    let mut inserted: BTreeSet<String> = BTreeSet::new();
    for cmd in &new_cmds {
        if let Some(Expr::Identifier(_, id)) = cmd.args.first() {
            inserted.insert(normalize_esc_fn(id.as_str()).to_string());
        }
    }
    for name in &s_owned {
        if !inserted.contains(normalize_esc_fn(name)) {
            append_cmd(&mut new_cmds, new_ident_cmd(name, p.pos));
        }
    }
    p.commands = new_cmds;
}

/// Port of Go's `appendCmd`: append `cmd` unless it is redundant with the last.
fn append_cmd(cmds: &mut Vec<CommandNode>, cmd: CommandNode) {
    if let Some(last) = cmds.last()
        && let (Some(Expr::Identifier(_, a)), Some(Expr::Identifier(_, b))) =
            (last.args.first(), cmd.args.first())
        && redundant_funcs(a.as_str(), b.as_str())
    {
        return;
    }
    cmds.push(cmd);
}

fn new_ident_cmd(name: &str, pos: Pos) -> CommandNode {
    CommandNode {
        pos,
        args: alloc::vec![Expr::Identifier(pos, SmolStr::from(name))],
    }
}

fn is_predefined_escaper(name: &str) -> bool {
    name == "html" || name == "urlquery"
}

/// Port of Go's `normalizeEscFn` via the `equivEscapers` table.
fn normalize_esc_fn(e: &str) -> &str {
    match e {
        "_html_template_attrescaper"
        | "_html_template_htmlescaper"
        | "_html_template_rcdataescaper" => "html",
        "_html_template_urlescaper" | "_html_template_urlnormalizer" => "urlquery",
        _ => e,
    }
}

fn esc_fns_eq(a: &str, b: &str) -> bool {
    normalize_esc_fn(a) == normalize_esc_fn(b)
}

/// Port of Go's `redundantFuncs`: `funcMap[b](funcMap[a](x)) == funcMap[a](x)`.
fn redundant_funcs(a: &str, b: &str) -> bool {
    match a {
        "_html_template_commentescaper" => {
            matches!(
                b,
                "_html_template_attrescaper" | "_html_template_htmlescaper"
            )
        }
        "_html_template_cssescaper"
        | "_html_template_jsregexpescaper"
        | "_html_template_jsstrescaper"
        | "_html_template_jstmpllitescaper" => b == "_html_template_attrescaper",
        "_html_template_urlescaper" => b == "_html_template_urlnormalizer",
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Text-editing helpers (special script tags, comments, DOCTYPE, entities).
// ---------------------------------------------------------------------------

/// The three special script openers Go neutralizes inside JS literals:
/// `<script`, `</script`, `<!--` (case-insensitive). Returns whether `s`
/// starts with `<` followed by one of them.
fn special_script_match(s: &[u8]) -> bool {
    if s.first() != Some(&b'<') {
        return false;
    }
    let rest = &s[1..];
    for grp in [
        b"script".as_slice(),
        b"/script".as_slice(),
        b"!--".as_slice(),
    ] {
        if rest.len() >= grp.len() && rest[..grp.len()].eq_ignore_ascii_case(grp) {
            return true;
        }
    }
    false
}

fn contains_special_script_tag(s: &[u8]) -> bool {
    (0..s.len()).any(|k| s[k] == b'<' && special_script_match(&s[k..]))
}

/// Port of Go's `escapeSpecialScriptTags`: replace the `<` of each special
/// opener with the literal `\x3C`.
fn escape_special_script_tags(s: &[u8]) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let mut start = 0;
    let mut k = 0;
    while k < s.len() {
        if s[k] == b'<' && special_script_match(&s[k..]) {
            push_bytes(&mut out, &s[start..k]);
            out.push_str("\\x3C");
            start = k + 1;
        }
        k += 1;
    }
    push_bytes(&mut out, &s[start..]);
    out
}

/// Case-insensitive `<!DOCTYPE` prefix test (Go's `doctypeBytes` check).
fn starts_with_doctype(s: &[u8]) -> bool {
    let d = b"<!DOCTYPE";
    s.len() >= d.len() && s[..d.len()].eq_ignore_ascii_case(d)
}

fn push_bytes(b: &mut String, slice: &[u8]) {
    b.push_str(&String::from_utf8_lossy(slice));
}

/// Go-style `%q` quoting, sufficient for the ASCII error strings we produce.
fn go_quote(s: &[u8]) -> String {
    format!("{:?}", String::from_utf8_lossy(s))
}

/// Format a context like Go's `context.String()`
/// (`{state delim urlPart jsCtx [depth] attr element <nil>}`), used in the
/// `ErrEndContext` / `ErrBranchEnd` messages.
fn context_string(c: &Context) -> String {
    let mut s = String::from("{");
    let _ = write!(s, "{} {} {} {} ", c.state, c.delim, c.url_part, c.js_ctx);
    write_int_slice(&mut s, &c.js_brace_depth);
    let _ = write!(s, " {} {} <nil>}}", c.attr, c.element);
    s
}

// ---------------------------------------------------------------------------
// Error classification for transition-internal stateError results. The Context
// carries no error detail, so we reconstruct the Go code/message from the
// pre-transition state and the input run.
// ---------------------------------------------------------------------------

fn classify_transition_error(c: &Context, s: &[u8]) -> EscErr {
    use EscapeErrorCode as E;
    match c.state {
        State::Js => {
            let from = index_byte(s, b'/').unwrap_or(0);
            EscErr::at0(
                E::SlashAmbig,
                format!(
                    "'/' could start a division or regexp: {}",
                    go_quote(&s[from..])
                ),
            )
        }
        State::JsDqStr | State::JsSqStr | State::JsTmplLit => EscErr::at0(
            E::PartialEscape,
            format!("unfinished escape sequence in JS string: {}", go_quote(s)),
        ),
        State::JsRegexp => {
            if has_open_charset(s) {
                EscErr::at0(
                    E::PartialCharset,
                    format!("unfinished JS regexp charset: {}", go_quote(s)),
                )
            } else {
                EscErr::at0(
                    E::PartialEscape,
                    format!("unfinished escape sequence in JS string: {}", go_quote(s)),
                )
            }
        }
        State::CssDqStr | State::CssSqStr | State::CssDqUrl | State::CssSqUrl | State::CssUrl => {
            EscErr::at0(
                E::PartialEscape,
                format!("unfinished escape sequence in CSS string: {}", go_quote(s)),
            )
        }
        State::Tag => classify_tag_error(s),
        State::AttrName => classify_attr_name_error(s),
        _ => EscErr::at0(E::BadHtml, String::from("bad HTML")),
    }
}

/// Whether the JS regexp run ends inside an unclosed `[...]` character set.
fn has_open_charset(s: &[u8]) -> bool {
    let mut in_cs = false;
    let mut k = 0;
    while k < s.len() {
        match s[k] {
            b'\\' => {
                k += 2;
                continue;
            }
            b'[' => in_cs = true,
            b']' => in_cs = false,
            _ => {}
        }
        k += 1;
    }
    in_cs
}

fn classify_tag_error(s: &[u8]) -> EscErr {
    let i = eat_ws(s, 0);
    if i < s.len() {
        let mut j = i;
        while j < s.len() {
            match s[j] {
                b' ' | b'\t' | b'\n' | 0x0c | b'\r' | b'=' | b'>' => break,
                b'\'' | b'"' | b'<' => {
                    return EscErr::at0(
                        EscapeErrorCode::BadHtml,
                        format!(
                            "{} in attribute name: {}",
                            go_quote(&s[j..=j]),
                            go_quote(s)
                        ),
                    );
                }
                _ => j += 1,
            }
        }
        if i == j {
            return EscErr::at0(
                EscapeErrorCode::BadHtml,
                format!(
                    "expected space, attr name, or end of tag, but got {}",
                    go_quote(&s[i..])
                ),
            );
        }
    }
    EscErr::at0(EscapeErrorCode::BadHtml, String::from("bad HTML in tag"))
}

fn classify_attr_name_error(s: &[u8]) -> EscErr {
    for (j, &b) in s.iter().enumerate() {
        if matches!(b, b'\'' | b'"' | b'<') {
            return EscErr::at0(
                EscapeErrorCode::BadHtml,
                format!("{} in attribute name: {}", go_quote(&s[j..=j]), go_quote(s)),
            );
        }
    }
    EscErr::at0(
        EscapeErrorCode::BadHtml,
        String::from("bad HTML in attribute name"),
    )
}

fn eat_ws(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && matches!(s[i], b' ' | b'\t' | b'\n' | 0x0c | b'\r') {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// HTML entity decoding and JS `type` classification for contextAfterText.
// ---------------------------------------------------------------------------

/// Whether the `<script type>` value denotes JavaScript (Go's `isJSType`).
fn is_js_type(s: &[u8]) -> bool {
    let full = String::from_utf8_lossy(s);
    let before_semi = full.split(';').next().unwrap_or("");
    let t = before_semi.trim().to_ascii_lowercase();
    matches!(
        t.as_str(),
        "" | "application/ecmascript"
            | "application/javascript"
            | "application/json"
            | "application/ld+json"
            | "application/x-ecmascript"
            | "application/x-javascript"
            | "module"
            | "text/ecmascript"
            | "text/javascript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
            | "text/jscript"
            | "text/livescript"
            | "text/x-ecmascript"
            | "text/x-javascript"
    )
}

/// A focused `html.UnescapeString` covering the entities that appear in
/// attribute values driven through `contextAfterText`: the named
/// `quot`/`apos`/`amp`/`lt`/`gt` and numeric `&#d;` / `&#xh;`.
fn parse_entity(s: &[u8]) -> Option<(char, usize)> {
    let semi = s.iter().position(|&b| b == b';')?;
    let body = &s[1..semi];
    let consumed = semi + 1;
    if body.first() == Some(&b'#') {
        let num = &body[1..];
        let val = if matches!(num.first(), Some(&b'x') | Some(&b'X')) {
            u32::from_str_radix(core::str::from_utf8(&num[1..]).ok()?, 16).ok()?
        } else {
            core::str::from_utf8(num).ok()?.parse::<u32>().ok()?
        };
        return char::from_u32(val).map(|c| (c, consumed));
    }
    let name = core::str::from_utf8(body).ok()?;
    let ch = match name {
        "quot" => '"',
        "apos" => '\'',
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        _ => return None,
    };
    Some((ch, consumed))
}

fn html_unescape(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'&'
            && let Some((ch, consumed)) = parse_entity(&s[i..])
        {
            let mut buf = [0u8; 4];
            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            i += consumed;
            continue;
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::TemplateError;
    use crate::html::Template;
    use crate::value::{SafeKind, Value};

    fn sval(s: &str) -> Value {
        Value::String(Arc::from(s))
    }

    fn list(items: Vec<Value>) -> Value {
        Value::List(Arc::from(items))
    }

    /// The TestEscape data record (the subset representable in the Value model).
    fn data() -> Value {
        Value::from_entries([
            ("F".to_string(), Value::Bool(false)),
            ("T".to_string(), Value::Bool(true)),
            ("C".to_string(), sval("<Cincinnati>")),
            ("G".to_string(), sval("<Goodbye>")),
            ("H".to_string(), sval("<Hello>")),
            ("A".to_string(), list(alloc::vec![sval("<a>"), sval("<b>")])),
            ("E".to_string(), list(alloc::vec![])),
            ("N".to_string(), Value::Int(42)),
            ("U".to_string(), Value::Nil),
            ("I".to_string(), sval("${ asd `` }")),
            (
                "W".to_string(),
                Value::Safe {
                    kind: SafeKind::Html,
                    s: Arc::from(
                        r#"&iexcl;<b class="foo">Hello</b>, <textarea>O'World</textarea>!"#,
                    ),
                },
            ),
        ])
    }

    fn run(name: &str, input: &str) -> String {
        Template::new(name)
            .parse(input)
            .expect("parse")
            .execute_to_string(&data())
            .expect("execute")
    }

    // ------------------------------------------------------------------
    // TestEscape (output parity).
    // ------------------------------------------------------------------

    #[test]
    fn escape_output_cases() {
        // (name, input, expected) ported from escape_test.go TestEscape.
        let cases: &[(&str, &str, &str)] = &[
            (
                "if",
                "{{if .T}}Hello{{end}}, {{.C}}!",
                "Hello, &lt;Cincinnati&gt;!",
            ),
            (
                "else",
                "{{if .F}}{{.H}}{{else}}{{.G}}{{end}}!",
                "&lt;Goodbye&gt;!",
            ),
            (
                "overescaping1",
                "Hello, {{.C | html}}!",
                "Hello, &lt;Cincinnati&gt;!",
            ),
            (
                "overescaping2",
                "Hello, {{html .C}}!",
                "Hello, &lt;Cincinnati&gt;!",
            ),
            (
                "overescaping3",
                "{{with .C}}{{$msg := .}}Hello, {{$msg}}!{{end}}",
                "Hello, &lt;Cincinnati&gt;!",
            ),
            (
                "assignment",
                "{{if $x := .H}}{{$x}}{{end}}",
                "&lt;Hello&gt;",
            ),
            ("withBody", "{{with .H}}{{.}}{{end}}", "&lt;Hello&gt;"),
            (
                "withElse",
                "{{with .E}}{{.}}{{else}}{{.H}}{{end}}",
                "&lt;Hello&gt;",
            ),
            (
                "rangeBody",
                "{{range .A}}{{.}}{{end}}",
                "&lt;a&gt;&lt;b&gt;",
            ),
            (
                "rangeElse",
                "{{range .E}}{{.}}{{else}}{{.H}}{{end}}",
                "&lt;Hello&gt;",
            ),
            ("nonStringValue", "{{.T}}", "true"),
            ("untypedNilValue", "{{.U}}", ""),
            (
                "constant",
                r#"<a href="/search?q={{"'a<b'"}}">"#,
                r#"<a href="/search?q=%27a%3cb%27">"#,
            ),
            (
                "multipleAttrs",
                "<a b=1 c={{.H}}>",
                "<a b=1 c=&lt;Hello&gt;>",
            ),
            (
                "urlStartRel",
                r#"<a href='{{"/foo/bar?a=b&c=d"}}'>"#,
                r#"<a href='/foo/bar?a=b&amp;c=d'>"#,
            ),
            (
                "urlStartAbsOk",
                r#"<a href='{{"http://example.com/foo/bar?a=b&c=d"}}'>"#,
                r#"<a href='http://example.com/foo/bar?a=b&amp;c=d'>"#,
            ),
            (
                "pathRelativeURLStart",
                r#"<a href="{{"/javascript:80/foo/bar"}}">"#,
                r#"<a href="/javascript:80/foo/bar">"#,
            ),
            (
                "dangerousURLStart",
                r#"<a href='{{"javascript:alert(%22pwned%22)"}}'>"#,
                r#"<a href='#ZgotmplZ'>"#,
            ),
            (
                "dangerousURLStart2",
                r#"<a href='  {{"javascript:alert(%22pwned%22)"}}'>"#,
                r#"<a href='  #ZgotmplZ'>"#,
            ),
            (
                "urlQuery",
                r#"<a href='/search?q={{.H}}'>"#,
                r#"<a href='/search?q=%3cHello%3e'>"#,
            ),
            (
                "urlFragment",
                r#"<a href='/faq#{{.H}}'>"#,
                r#"<a href='/faq#%3cHello%3e'>"#,
            ),
            (
                "urlBranch",
                r#"<a href="{{if .F}}/foo?a=b{{else}}/bar{{end}}">"#,
                r#"<a href="/bar">"#,
            ),
            (
                "urlBranchConflictMoot",
                r#"<a href="{{if .T}}/foo?a={{else}}/bar#{{end}}{{.C}}">"#,
                r#"<a href="/foo?a=%3cCincinnati%3e">"#,
            ),
            (
                "jsStrValue",
                "<button onclick='alert({{.H}})'>",
                r#"<button onclick='alert(&#34;\u003cHello\u003e&#34;)'>"#,
            ),
            (
                "jsNumericValue",
                "<button onclick='alert({{.N}})'>",
                "<button onclick='alert( 42 )'>",
            ),
            (
                "jsBoolValue",
                "<button onclick='alert({{.T}})'>",
                "<button onclick='alert( true )'>",
            ),
            (
                "jsObjValue",
                "<button onclick='alert({{.A}})'>",
                r#"<button onclick='alert([&#34;\u003ca\u003e&#34;,&#34;\u003cb\u003e&#34;])'>"#,
            ),
            (
                "jsObjValueScript",
                "<script>alert({{.A}})</script>",
                r#"<script>alert(["\u003ca\u003e","\u003cb\u003e"])</script>"#,
            ),
            (
                "jsStr",
                "<button onclick='alert(&quot;{{.H}}&quot;)'>",
                r#"<button onclick='alert(&quot;\u003cHello\u003e&quot;)'>"#,
            ),
            (
                "jsRe",
                r#"<button onclick='alert(/{{"foo+bar"}}/.test(""))'>"#,
                r#"<button onclick='alert(/foo\u002bbar/.test(""))'>"#,
            ),
            (
                "jsReBlank",
                r#"<script>alert(/{{""}}/.test(""));</script>"#,
                r#"<script>alert(/(?:)/.test(""));</script>"#,
            ),
            (
                "jsReAmbigOk",
                r#"<script>{{if true}}var x = 1{{end}}</script>"#,
                r#"<script>var x = 1</script>"#,
            ),
            (
                "styleBidiKeywordPassed",
                r#"<p style="dir: {{"ltr"}}">"#,
                r#"<p style="dir: ltr">"#,
            ),
            (
                "styleExpressionBlocked",
                r#"<p style="width: {{"expression(alert(1337))"}}">"#,
                r#"<p style="width: ZgotmplZ">"#,
            ),
            (
                "styleTagSelectorPassed",
                r#"<style>{{"p"}} { color: pink }</style>"#,
                r#"<style>p { color: pink }</style>"#,
            ),
            (
                "styleURLQueryEncoded",
                r#"<p style="background: url(/img?name={{"O'Reilly Animal(1)<2>.png"}})">"#,
                r#"<p style="background: url(/img?name=O%27Reilly%20Animal%281%29%3c2%3e.png)">"#,
            ),
            (
                "styleURLBadProtocolBlocked",
                r#"<a style="background: url('{{"javascript:alert(1337)"}}')">"#,
                r#"<a style="background: url('#ZgotmplZ')">"#,
            ),
            (
                "styleURLMixedCase",
                r#"<p style="background: URL(#{{.H}})">"#,
                r#"<p style="background: URL(#%3cHello%3e)">"#,
            ),
            (
                "styleURLNotEncodedForHTMLInCdata",
                r#"<style>body { background: url('{{"/search?img=foo&size=icon"}}') }</style>"#,
                r#"<style>body { background: url('/search?img=foo&size=icon') }</style>"#,
            ),
            (
                "HTMLcomment",
                "<b>Hello, <!-- name of world -->{{.C}}</b>",
                "<b>Hello, &lt;Cincinnati&gt;</b>",
            ),
            ("HTMLcommentNotFirst", "<<!-- -->!--", "&lt;!--"),
            ("HTMLnormalization1", "a < b", "a &lt; b"),
            ("HTMLnormalization2", "a << b", "a &lt;&lt; b"),
            ("HTMLnormalization3", "a<<!-- --><!-- -->b", "a&lt;b"),
            (
                "HTMLdoctypeNotNormalized",
                "<!DOCTYPE html>Hello, World!",
                "<!DOCTYPE html>Hello, World!",
            ),
            (
                "HTMLdoctypeNotCaseInsensitive",
                "<!doCtYPE htMl>Hello, World!",
                "<!doCtYPE htMl>Hello, World!",
            ),
            ("NoDoctypeInjection", r#"<!{{"DOCTYPE"}}"#, "&lt;!DOCTYPE"),
            (
                "SplitHTMLcomment",
                "<b>Hello, <!-- name of {{if .T}}city -->{{.C}}{{else}}world -->{{.W}}{{end}}</b>",
                "<b>Hello, &lt;Cincinnati&gt;</b>",
            ),
            (
                "SpecialTagsInScriptStringLiterals",
                r#"<script>var a = "asd < 123 <!-- 456 < fgh <script jkl < 789 </script"</script>"#,
                r#"<script>var a = "asd < 123 \x3C!-- 456 < fgh \x3Cscript jkl < 789 \x3C/script"</script>"#,
            ),
            (
                "SpecialTagsInScriptRegexMixedCase",
                r#"<script>var a = /<!-- <ScripT </ScripT/</script>"#,
                r#"<script>var a = /\x3C!-- \x3CScripT \x3C/ScripT/</script>"#,
            ),
            (
                "HTMLsubstitutionCommentedOut",
                "<p><!-- {{.H}} --></p>",
                "<p></p>",
            ),
            (
                "typedHTMLinText",
                "{{.W}}",
                r#"&iexcl;<b class="foo">Hello</b>, <textarea>O'World</textarea>!"#,
            ),
            (
                "typedHTMLinAttribute",
                r#"<div title="{{.W}}">"#,
                r#"<div title="&iexcl;Hello, O&#39;World!">"#,
            ),
            (
                "typedHTMLinRCDATA",
                "<textarea>{{.W}}</textarea>",
                "<textarea>&iexcl;&lt;b class=&#34;foo&#34;&gt;Hello&lt;/b&gt;, &lt;textarea&gt;O&#39;World&lt;/textarea&gt;!</textarea>",
            ),
            (
                "rangeInTextarea",
                "<textarea>{{range .A}}{{.}}{{end}}</textarea>",
                "<textarea>&lt;a&gt;&lt;b&gt;</textarea>",
            ),
            (
                "NoTagInjection",
                r#"{{"10$"}}<{{"script src,evil.org/pwnd.js"}}..."#,
                "10$&lt;script src,evil.org/pwnd.js...",
            ),
            ("NoCommentInjection", r#"<{{"!--"}}"#, "&lt;!--"),
            (
                "NoRCDATAEndTagInjection",
                r#"<textarea><{{"/textarea "}}...</textarea>"#,
                "<textarea>&lt;/textarea ...</textarea>",
            ),
            (
                "dynamicElementName",
                r#"<h{{3}}><table><t{{"head"}}>...</h{{3}}>"#,
                r#"<h3><table><thead>...</h3>"#,
            ),
            (
                "badDynamicAttributeName1",
                r#"<input {{"onchange"}}="{{"doEvil()"}}">"#,
                r#"<input ZgotmplZ="doEvil()">"#,
            ),
            (
                "dynamicAttributeName",
                r#"<img on{{"load"}}="alert({{"loaded"}})">"#,
                r#"<img onload="alert(&#34;loaded&#34;)">"#,
            ),
            (
                "quotedEmptyAttributeValue",
                "<p name=\"{{.U}}\">",
                "<p name=\"\">",
            ),
            (
                "unquotedEmptyAttributeValuePlaintext",
                "<p name={{.U}}>",
                "<p name=ZgotmplZ>",
            ),
            (
                "JStemplatelitspecials",
                "<script>var a = `{{.I}}`</script>",
                "<script>var a = `\\u0024\\u007b asd \\u0060\\u0060 \\u007d`</script>",
            ),
            (
                "srcsetBadURL",
                r#"<img srcset="{{"/not-an-image#,javascript:alert(1)"}}">"#,
                r#"<img srcset="/not-an-image#,#ZgotmplZ">"#,
            ),
            (
                "metaContentURL",
                r#"<meta http-equiv="refresh" content="asd; url={{"javascript:alert(1)"}}; asd; url={{"vbscript:alert(1)"}}; asd">"#,
                r#"<meta http-equiv="refresh" content="asd; url=#ZgotmplZ; asd; url=#ZgotmplZ; asd">"#,
            ),
            (
                "commentEndsFlushWithStart",
                "<!--{{.}}--><script>/*{{.}}*///{{.}}\n</script><style>/*{{.}}*///{{.}}\n</style><a onclick='/*{{.}}*///{{.}}' style='/*{{.}}*///{{.}}'>",
                "<script> \n</script><style> \n</style><a onclick='/**///' style='/**///'>",
            ),
        ];
        for &(name, input, want) in cases {
            let got = run(name, input);
            assert_eq!(got, want, "case {name}: input {input:?}");
        }
    }

    // ------------------------------------------------------------------
    // TestEscapeSet (cross-template / {{template}} / block).
    // ------------------------------------------------------------------

    fn run_set(source: &str, main: &str, data: &Value) -> String {
        Template::new("root")
            .parse(source)
            .expect("parse")
            .execute_template_to_string(main, data)
            .expect("execute")
    }

    #[test]
    fn escape_set_trivial() {
        assert_eq!(
            run_set(r#"{{define "main"}}{{end}}"#, "main", &Value::Nil),
            ""
        );
    }

    #[test]
    fn escape_set_start_context() {
        let src = r#"{{define "main"}}Hello, {{template "helper"}}!{{end}}{{define "helper"}}{{"<World>"}}{{end}}"#;
        assert_eq!(run_set(src, "main", &Value::Nil), "Hello, &lt;World&gt;!");
    }

    #[test]
    fn escape_set_non_start_context() {
        let src = r#"{{define "main"}}<a onclick='a = {{template "helper"}};'>{{end}}{{define "helper"}}{{"<a>"}}<b{{end}}"#;
        assert_eq!(
            run_set(src, "main", &Value::Nil),
            r#"<a onclick='a = &#34;\u003ca\u003e&#34;<b;'>"#
        );
    }

    #[test]
    fn escape_set_two_contexts() {
        let src = r#"{{define "main"}}<button onclick="title='{{template "helper"}}'; ...">{{template "helper"}}</button>{{end}}{{define "helper"}}{{11}} of {{"<100>"}}{{end}}"#;
        assert_eq!(
            run_set(src, "main", &Value::Nil),
            r#"<button onclick="title='11 of \u003c100\u003e'; ...">11 of &lt;100&gt;</button>"#
        );
    }

    #[test]
    fn escape_set_helper_ends_in_different_context() {
        let src = r#"{{define "main"}}<script>var x={{template "helper"}}/{{"42"}};</script>{{end}}{{define "helper"}}{{126}}{{end}}"#;
        assert_eq!(
            run_set(src, "main", &Value::Nil),
            r#"<script>var x= 126 /"42";</script>"#
        );
    }

    #[test]
    fn escape_set_recursive_main() {
        // A recursive template that ends in its start context.
        let src = r#"{{define "main"}}{{range .Children}}{{template "main" .}}{{else}}{{.X}} {{end}}{{end}}"#;
        let leaf = |x: &str| {
            Value::from_entries([
                ("X".to_string(), sval(x)),
                ("Children".to_string(), list(alloc::vec![])),
            ])
        };
        let sub = Value::from_entries([
            ("X".to_string(), sval("")),
            ("Children".to_string(), list(alloc::vec![leaf("baz")])),
        ]);
        let root = Value::from_entries([
            ("X".to_string(), sval("")),
            (
                "Children".to_string(),
                list(alloc::vec![leaf("foo"), leaf("<bar>"), sub]),
            ),
        ]);
        assert_eq!(run_set(src, "main", &root), "foo &lt;bar&gt; baz ");
    }

    #[test]
    fn block_desugars_and_escapes() {
        // {{block}} defines and immediately invokes; the invocation escapes.
        let t = Template::new("t")
            .parse("<p>{{block \"b\" .}}{{.C}}{{end}}</p>")
            .unwrap();
        assert_eq!(
            t.execute_to_string(&data()).unwrap(),
            "<p>&lt;Cincinnati&gt;</p>"
        );
    }

    // ------------------------------------------------------------------
    // TestErrors (representative error codes).
    // ------------------------------------------------------------------

    fn err_of(input: &str) -> TemplateError {
        Template::new("z")
            .parse(input)
            .expect("parse")
            .execute_to_string(&Value::Nil)
            .expect_err("should fail to escape")
    }

    fn escape_parts(e: &TemplateError) -> (EscapeErrorCode, &str) {
        match e {
            TemplateError::Escape {
                code, description, ..
            } => (*code, description.as_str()),
            other => panic!("expected Escape error, got {other:?}"),
        }
    }

    #[test]
    fn error_no_output_when_valid() {
        // Non-error cases must escape and execute cleanly.
        for input in [
            "{{if .Cond}}<a>{{else}}<b>{{end}}",
            "{{if .Cond}}<a>{{end}}",
            "{{with .Cond}}<div>{{end}}",
            "{{range .Items}}<a>{{end}}",
            "<a href='/foo?{{range .Items}}&{{.K}}={{.V}}{{end}}'>",
            "{{range .Items}}<a{{if .X}}{{end}}>{{break}}{{end}}",
            "<script>var a = `${a+b}`</script>",
        ] {
            let r = Template::new("z")
                .parse(input)
                .unwrap()
                .execute_to_string(&Value::Nil);
            assert!(r.is_ok(), "input {input:?} should escape cleanly: {r:?}");
        }
    }

    #[test]
    fn error_branch_end() {
        let (code, desc) = {
            let e = err_of("{{if .Cond}}<a{{end}}");
            let (c, d) = escape_parts(&e);
            (c, d.to_string())
        };
        assert_eq!(code, EscapeErrorCode::BranchEnd);
        assert!(desc.contains("{{if}} branches"), "desc {desc:?}");
    }

    #[test]
    fn error_end_context() {
        let e = err_of("<a b=1 c={{.H}}");
        let (code, desc) = escape_parts(&e);
        assert_eq!(code, EscapeErrorCode::EndContext);
        assert!(
            desc.contains("ends in a non-text context: {stateAttr delimSpaceOrTagEnd"),
            "desc {desc:?}"
        );
    }

    #[test]
    fn error_end_context_script() {
        let e = err_of("<script>foo();");
        let (code, desc) = escape_parts(&e);
        assert_eq!(code, EscapeErrorCode::EndContext);
        assert!(
            desc.contains("ends in a non-text context: {stateJS"),
            "desc {desc:?}"
        );
    }

    #[test]
    fn error_no_such_template() {
        let e = err_of(r#"{{template "foo"}}"#);
        let (code, desc) = escape_parts(&e);
        assert_eq!(code, EscapeErrorCode::NoSuchTemplate);
        assert!(desc.contains(r#"no such template "foo""#), "desc {desc:?}");
    }

    #[test]
    fn error_ambig_context() {
        let e = err_of(r#"<a href="{{if .F}}/foo?a={{else}}/bar/{{end}}{{.H}}">"#);
        let (code, desc) = escape_parts(&e);
        assert_eq!(code, EscapeErrorCode::AmbigContext);
        assert!(
            desc.contains("appears in an ambiguous context within a URL"),
            "desc {desc:?}"
        );
    }

    #[test]
    fn error_predefined_escaper_not_last() {
        let e = err_of("Hello, {{. | urlquery | print}}!");
        let (code, desc) = escape_parts(&e);
        assert_eq!(code, EscapeErrorCode::PredefinedEscaper);
        assert!(
            desc.contains(r#"predefined escaper "urlquery" disallowed"#),
            "desc {desc:?}"
        );
    }

    #[test]
    fn error_predefined_escaper_html_in_unquoted_attr() {
        let e = err_of("<div class={{. | html}}>Hello<div>");
        let (code, desc) = escape_parts(&e);
        assert_eq!(code, EscapeErrorCode::PredefinedEscaper);
        assert!(
            desc.contains(r#"predefined escaper "html" disallowed"#),
            "desc {desc:?}"
        );
    }

    #[test]
    fn error_bad_html_unquoted_attr() {
        let e = err_of("<input type=button value=onclick=>");
        let (code, desc) = escape_parts(&e);
        assert_eq!(code, EscapeErrorCode::BadHtml);
        assert!(desc.contains("in unquoted attr"), "desc {desc:?}");
    }

    #[test]
    fn error_output_context() {
        let input = concat!(
            r#"<script>reverseList = [{{template "t"}}]</script>"#,
            r#"{{define "t"}}{{if .Tail}}{{template "t" .Tail}}{{end}}{{.Head}}",{{end}}"#
        );
        let e = err_of(input);
        let (code, desc) = escape_parts(&e);
        assert_eq!(code, EscapeErrorCode::OutputContext);
        assert!(
            desc.contains(
                "cannot compute output context for template t$htmltemplate_stateJS_elementScript"
            ),
            "desc {desc:?}"
        );
    }

    #[test]
    fn error_range_loop_reentry() {
        let e = err_of("{{range .Items}}<a{{end}}");
        let (code, desc) = escape_parts(&e);
        // The second-pass re-scan of `<a` records a transition error, so — like
        // Go — that error's code propagates (here BadHtml) rather than a
        // distinct range-reentry code; only the "on range loop re-entry:" text
        // is prepended. The `None` fallback (which now yields BranchEnd, not the
        // vestigial RangeLoopReentry) is not reached by this input.
        assert_eq!(code, EscapeErrorCode::BadHtml);
        assert!(desc.contains("on range loop re-entry:"), "desc {desc:?}");
        assert!(desc.contains("in attribute name"), "desc {desc:?}");
    }

    #[test]
    fn error_range_loop_break() {
        let e = err_of("{{range .Items}}<a{{if .X}}{{break}}{{end}}>{{end}}");
        let (code, desc) = escape_parts(&e);
        assert_eq!(code, EscapeErrorCode::BranchEnd);
        assert!(desc.contains("at range loop break:"), "desc {desc:?}");
    }

    #[test]
    fn error_slash_ambiguous() {
        let e = err_of(r#"<script>{{if false}}var x = 1{{end}}/-{{"1.5"}}/i.test(x)</script>"#);
        let (code, _desc) = escape_parts(&e);
        assert_eq!(code, EscapeErrorCode::SlashAmbig);
    }

    #[test]
    fn error_partial_escape() {
        let e = err_of(r#"<a onclick="alert('Hello \"#);
        let (code, desc) = escape_parts(&e);
        assert_eq!(code, EscapeErrorCode::PartialEscape);
        assert!(desc.contains("unfinished escape sequence"), "desc {desc:?}");
    }

    #[test]
    fn error_partial_charset() {
        let e = err_of(r#"<a onclick="/foo[\]/"#);
        let (code, desc) = escape_parts(&e);
        assert_eq!(code, EscapeErrorCode::PartialCharset);
        assert!(
            desc.contains("unfinished JS regexp charset"),
            "desc {desc:?}"
        );
    }

    #[test]
    fn cannot_parse_after_execute() {
        let t = Template::new("t").parse("<p>{{.C}}</p>").unwrap();
        let _ = t.execute_to_string(&data()).unwrap();
        let err = match t.parse("more") {
            Ok(_) => panic!("expected cannot-Parse-after-Execute error"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("cannot Parse after Execute"));
    }

    // ------------------------------------------------------------------
    // isJSType (`<script type>` classification). Ported from js_test.go
    // TestIsJsMimeType, extended to the full accepted list and the
    // trim/lowercase normalization.
    // ------------------------------------------------------------------

    #[test]
    fn is_js_type_table() {
        // Accepted JS/JSON MIME types (Go's `isJSType` switch; the empty
        // string defaults to JS). Parameters after `;` are discarded, the
        // value is trimmed and lowercased.
        for ty in [
            "",
            "application/ecmascript",
            "application/javascript",
            "application/javascript;version=1.8",
            "application/javascript;version=1.8;foo=bar",
            "application/json",
            "application/ld+json",
            "application/x-ecmascript",
            "application/x-javascript",
            "module",
            "text/ecmascript",
            "text/javascript",
            "text/javascript1.5",
            "text/jscript",
            "text/livescript",
            "text/x-ecmascript",
            "text/x-javascript",
            "  text/javascript  ",
            "TEXT/JAVASCRIPT",
        ] {
            assert!(is_js_type(ty.as_bytes()), "expected JS: {ty:?}");
        }
        // Rejected: a `/` where a `;` parameter separator is expected, and
        // non-script content types.
        for ty in [
            "application/javascript/version=1.8",
            "text/template",
            "text/plain",
            "text/html",
            "application/xml",
        ] {
            assert!(!is_js_type(ty.as_bytes()), "expected non-JS: {ty:?}");
        }
    }

    // ------------------------------------------------------------------
    // ensurePipelineContains: escaper insertion, dedup, and equivalence.
    // A focused port of escape_test.go TestEnsurePipelineContains covering
    // the identifier-command cases; we assert the resulting command
    // identifier chain (None marks a non-identifier command such as `.X`).
    // ------------------------------------------------------------------

    fn field_cmd(name: &str) -> CommandNode {
        CommandNode {
            pos: Pos::new(0, 1),
            args: alloc::vec![Expr::Field(
                Pos::new(0, 1),
                alloc::vec![SmolStr::from(name)]
            )],
        }
    }

    fn pipe(cmds: alloc::vec::Vec<CommandNode>) -> PipeNode {
        PipeNode {
            pos: Pos::new(0, 1),
            decl: alloc::vec::Vec::new(),
            commands: cmds,
            is_assign: false,
        }
    }

    fn idents(p: &PipeNode) -> alloc::vec::Vec<Option<String>> {
        p.commands
            .iter()
            .map(|c| match c.args.first() {
                Some(Expr::Identifier(_, id)) => Some(id.to_string()),
                _ => None,
            })
            .collect()
    }

    fn ident(name: &str) -> Option<String> {
        Some(name.to_string())
    }

    #[test]
    fn ensure_pipeline_contains_cases() {
        // {{.X}} + []  ->  .X  (unchanged).
        let mut p = pipe(alloc::vec![field_cmd("X")]);
        ensure_pipeline_contains(&mut p, &[]);
        assert_eq!(idents(&p), alloc::vec![None]);

        // {{.X}} + [html]  ->  .X | html
        let mut p = pipe(alloc::vec![field_cmd("X")]);
        ensure_pipeline_contains(&mut p, &["html"]);
        assert_eq!(idents(&p), alloc::vec![None, ident("html")]);

        // {{.X | print | urlquery}} + [urlquery]  ->  unchanged (already ends
        // in urlquery).
        let mut p = pipe(alloc::vec![
            field_cmd("X"),
            new_ident_cmd("print", Pos::new(0, 1)),
            new_ident_cmd("urlquery", Pos::new(0, 1)),
        ]);
        ensure_pipeline_contains(&mut p, &["urlquery"]);
        assert_eq!(
            idents(&p),
            alloc::vec![None, ident("print"), ident("urlquery")]
        );

        // {{.X | urlquery}} + [html, urlquery]  ->  .X | html | urlquery
        // (html inserted before the trailing predefined escaper; urlquery not
        // duplicated).
        let mut p = pipe(alloc::vec![
            field_cmd("X"),
            new_ident_cmd("urlquery", Pos::new(0, 1)),
        ]);
        ensure_pipeline_contains(&mut p, &["html", "urlquery"]);
        assert_eq!(
            idents(&p),
            alloc::vec![None, ident("html"), ident("urlquery")]
        );

        // {{.X | urlquery}} + [_html_template_urlfilter, _html_template_urlnormalizer]
        //   ->  .X | _html_template_urlfilter | urlquery
        // (urlnormalizer is equivalent to the predefined urlquery, so it is
        // dropped; urlfilter is inserted before urlquery).
        let mut p = pipe(alloc::vec![
            field_cmd("X"),
            new_ident_cmd("urlquery", Pos::new(0, 1)),
        ]);
        ensure_pipeline_contains(
            &mut p,
            &["_html_template_urlfilter", "_html_template_urlnormalizer"],
        );
        assert_eq!(
            idents(&p),
            alloc::vec![None, ident("_html_template_urlfilter"), ident("urlquery")]
        );

        // {{.X | urlquery}} + [_html_template_urlnormalizer]  ->  unchanged
        // (the sole escaper is equivalent to the existing urlquery).
        let mut p = pipe(alloc::vec![
            field_cmd("X"),
            new_ident_cmd("urlquery", Pos::new(0, 1)),
        ]);
        ensure_pipeline_contains(&mut p, &["_html_template_urlnormalizer"]);
        assert_eq!(idents(&p), alloc::vec![None, ident("urlquery")]);
    }
}
