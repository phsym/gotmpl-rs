#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc = include_str!("../README.md")]
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::todo,
    clippy::unimplemented,
    clippy::unreachable
)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable
    )
)]

extern crate alloc;

pub(crate) mod error;
pub(crate) mod exec;
pub(crate) mod funcs;
pub(crate) mod go;
/// Parser, lexer, and AST node types for the Go template language.
///
/// This module mirrors Go's `text/template/parse` package. Most users don't
/// need it directly; use [`Template::parse`] instead. The AST types are public
/// for advanced use cases like [`Template::add_parse_tree`].
pub mod parse;
pub(crate) mod value;

// Public re-exports
// All user-facing types are available at the crate root.

pub use error::{Result, TemplateError};
use funcs::builtins;

/// Shared, lazily-constructed builtins map. `Template::new` clones this `Arc`
/// (one atomic op) instead of rebuilding the builtins BTreeMap on every call.
/// Mutation via [`Template::func`] / [`Template::funcs`] goes through
/// `Arc::make_mut`, so registering a custom func triggers a one-time copy and
/// the shared map is never observed in a mutated state.
#[cfg(feature = "std")]
fn shared_builtins() -> Arc<BTreeMap<String, ValueFunc>> {
    use std::sync::LazyLock;
    static BUILTINS: LazyLock<Arc<BTreeMap<String, ValueFunc>>> =
        LazyLock::new(|| Arc::new(builtins()));
    BUILTINS.clone()
}

#[cfg(not(feature = "std"))]
fn shared_builtins() -> Arc<BTreeMap<String, ValueFunc>> {
    Arc::new(builtins())
}

fn col_for_offset(src: &str, offset: usize) -> usize {
    let end = offset.min(src.len());
    let line_start = src[..end].rfind('\n').map_or(0, |i| i + 1);
    src[line_start..end].chars().count() + 1
}
pub use go::{html_escape, js_escape, url_encode};
pub use value::{ToValue, Value, ValueFunc};

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

#[cfg(feature = "std")]
use std::io::Write;

use exec::Executor;
pub use exec::MissingKey;
use parse::{DefineNode, ListNode, Parser};

/// A map from function names to template functions, equivalent to Go's
/// `template.FuncMap`. Pass one to [`Template::funcs`] to register several
/// functions at once.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use gotmpl::{FuncMap, Value};
///
/// let mut fm = FuncMap::new();
/// fm.insert("double".into(), Arc::new(|args: &[Value]| {
///     let n = args.first().and_then(Value::as_int).unwrap_or(0);
///     Ok(Value::Int(n * 2))
/// }));
/// ```
pub type FuncMap = BTreeMap<String, ValueFunc>;

/// A parsed template, equivalent to Go's
/// [`template.Template`](https://pkg.go.dev/text/template#Template).
///
/// Configure, parse, and execute via the builder-style API:
///
/// ```
/// use gotmpl::{Template, MissingKey, tmap};
///
/// let output = Template::new("greet")
///     .delims("<<", ">>")                        // optional: custom delimiters
///     .missing_key(MissingKey::Error)             // optional: error on missing keys
///     .func("shout", |args| {                     // optional: custom functions
///         let s = args.first().map(|v| v.to_string()).unwrap_or_default();
///         Ok(gotmpl::Value::String(s.to_uppercase().into()))
///     })
///     .parse("Hello, << .Name | shout >>!")       // parse template source
///     .unwrap()
///     .execute_to_string(&tmap! { "Name" => "world" })
///     .unwrap();
///
/// assert_eq!(output, "Hello, WORLD!");
/// ```
pub struct Template {
    name: String,
    tree: Option<ListNode>,
    defines: BTreeMap<String, Arc<ListNode>>,
    funcs: Arc<BTreeMap<String, ValueFunc>>,
    left_delim: String,
    right_delim: String,
    missing_key: MissingKey,
    max_range_iters: u64,
}

/// Adapts a [`std::io::Write`] to [`core::fmt::Write`]. Any [`io::Error`](std::io::Error)
/// gets stashed, since [`fmt::Error`](core::fmt::Error) has no payload to carry it.
#[cfg(feature = "std")]
struct IoAdapter<'a, W> {
    inner: &'a mut W,
    error: Option<std::io::Error>,
}

#[cfg(feature = "std")]
impl<'a, W> IoAdapter<'a, W> {
    fn new(inner: &'a mut W) -> Self {
        IoAdapter { inner, error: None }
    }

    fn err_mapper(self) -> impl FnOnce(TemplateError) -> TemplateError {
        move |e| match e {
            error::TemplateError::Write => error::TemplateError::Io(
                self.error
                    .unwrap_or_else(|| std::io::Error::other("write error")),
            ),
            _ => e,
        }
    }
}

#[cfg(feature = "std")]
impl<W: std::io::Write> core::fmt::Write for IoAdapter<'_, W> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.inner.write_all(s.as_bytes()).map_err(|e| {
            self.error = Some(e);
            core::fmt::Error
        })
    }
}

impl Template {
    /// Create a new, empty template with the given name.
    ///
    /// The name is used in error messages and when invoking templates via
    /// `{{template "name"}}`. Built-in functions are registered automatically.
    pub fn new(name: &str) -> Self {
        Template {
            name: name.to_string(),
            tree: None,
            defines: BTreeMap::new(),
            funcs: shared_builtins(),
            left_delim: "{{".to_string(),
            right_delim: "}}".to_string(),
            missing_key: MissingKey::default(),
            max_range_iters: exec::DEFAULT_MAX_RANGE_ITERS,
        }
    }

    /// Set the total number of `{{range}}` iterations allowed per execution
    /// (across all nested ranges). Defaults to 10,000,000. Set to `0` to
    /// disable the cap entirely (at your own risk with untrusted templates).
    #[must_use]
    pub fn max_range_iters(mut self, n: u64) -> Self {
        self.max_range_iters = n;
        self
    }

    /// Set custom action delimiters (default: `"{{"` and `"}}"`).
    ///
    /// Must be called **before** [`parse`](Self::parse).
    ///
    /// # Examples
    ///
    /// ```
    /// use gotmpl::{Template, tmap};
    ///
    /// let result = Template::new("t")
    ///     .delims("<%", "%>")
    ///     .parse("Hello, <%.Name%>!")
    ///     .unwrap()
    ///     .execute_to_string(&tmap! { "Name" => "World" })
    ///     .unwrap();
    /// assert_eq!(result, "Hello, World!");
    /// ```
    #[must_use]
    pub fn delims(mut self, left: &str, right: &str) -> Self {
        self.left_delim = left.to_string();
        self.right_delim = right.to_string();
        self
    }

    /// Set the behavior for missing map keys.
    ///
    /// # Examples
    ///
    /// ```
    /// use gotmpl::{Template, MissingKey, tmap};
    ///
    /// let result = Template::new("t")
    ///     .missing_key(MissingKey::Error)
    ///     .parse("{{.Y}}")
    ///     .unwrap()
    ///     .execute_to_string(&tmap! { "X" => 1i64 });
    /// assert!(result.is_err());
    /// ```
    #[must_use]
    pub fn missing_key(mut self, mk: MissingKey) -> Self {
        self.missing_key = mk;
        self
    }

    /// Register a custom template function.
    ///
    /// Must be called **before** [`parse`](Self::parse). The function receives
    /// its arguments as a `&[Value]` slice and returns a
    /// [`Result<Value>`](error::Result). It is available inside templates under
    /// the given `name`; registering a name that matches a built-in replaces it.
    ///
    /// # Examples
    ///
    /// ```
    /// use gotmpl::{Template, tmap, Value};
    ///
    /// let result = Template::new("t")
    ///     .func("double", |args| {
    ///         let n = args.first().and_then(Value::as_int).unwrap_or(0);
    ///         Ok(Value::Int(n * 2))
    ///     })
    ///     .parse("{{double 21}}")
    ///     .unwrap()
    ///     .execute_to_string(&tmap!{})
    ///     .unwrap();
    /// assert_eq!(result, "42");
    /// ```
    #[must_use]
    pub fn func(
        mut self,
        name: &str,
        f: impl Fn(&[Value]) -> Result<Value> + Send + Sync + 'static,
    ) -> Self {
        Arc::make_mut(&mut self.funcs).insert(name.to_string(), Arc::new(f));
        self
    }

    /// Register several template functions at once from a [`FuncMap`], the
    /// counterpart to Go's `template.Funcs()`. Must be called **before**
    /// [`parse`](Self::parse).
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use gotmpl::{Template, FuncMap, tmap, Value};
    ///
    /// let mut fm = FuncMap::new();
    /// fm.insert("greet".into(), Arc::new(|args: &[Value]| {
    ///     let name = args.first().map(|v| v.to_string()).unwrap_or_default();
    ///     Ok(Value::String(format!("Hello, {name}!").into()))
    /// }));
    ///
    /// let result = Template::new("t")
    ///     .funcs(fm)
    ///     .parse(r#"{{greet "World"}}"#)
    ///     .unwrap()
    ///     .execute_to_string(&tmap!{})
    ///     .unwrap();
    /// assert_eq!(result, "Hello, World!");
    /// ```
    #[must_use]
    pub fn funcs(mut self, func_map: FuncMap) -> Self {
        Arc::make_mut(&mut self.funcs).extend(func_map);
        self
    }

    /// Parse template source and associate it with this template.
    ///
    /// Mirrors Go's `(*Template).Parse` **method**: named template definitions
    /// (`{{define}}` / `{{block}}`) in `src` are extracted into this template's
    /// definition map, and the top-level content becomes this template's body.
    ///
    /// Successive calls can redefine named templates. A body that reduces to
    /// only whitespace is considered empty and will **not** overwrite an
    /// existing body or define of the same name. That is how `parse` can be
    /// used to add named definitions without clobbering the main template.
    /// Comment-only bodies behave as empty because the lexer strips comments
    /// before parsing. If no body has been set yet, an empty body is still
    /// stored (so executing the template yields `""`).
    ///
    /// Within a single call, redeclaring a name with two non-empty bodies is
    /// an error (Go's `template: multiple definition of template "x"`). Across
    /// calls, the later non-empty body replaces the earlier one.
    ///
    /// Call [`delims`](Self::delims) and [`func`](Self::func) first, so the
    /// parser sees the intended configuration.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError::Lex`] or [`TemplateError::Parse`] if `src`
    /// contains syntax errors, or if two non-empty definitions of the same
    /// name appear in the same parse call.
    pub fn parse(mut self, src: &str) -> Result<Self> {
        // Go parity: `(*Template).Parse` tags errors with the receiver's name
        // (its `parseName`). An empty receiver name drops the `<name>:` segment
        // from the error's display, keeping the rest of the format unchanged.
        let parser =
            Parser::with_name(self.parse_name(), src, &self.left_delim, &self.right_delim)?;
        let (tree, defines) = parser.parse()?;

        // Go's `parse.(*Tree).add` rejects a non-empty top-level that collides
        // with a same-named non-empty `{{define}}` in the same parse call:
        // both would occupy the `treeSet[name]` slot. Detect the same here so
        // the error path matches Go's "multiple definition of template" output.
        Self::check_in_file_basename_collision(&self.name, &tree, &defines, src)?;

        // Mirror Go's `treeSet[t.Name] = topLevelTree` at parse time: pre-populate
        // the name-keyed slot with the non-empty top so `merge_defines` treats it
        // as an existing entry and drops an in-call *empty* same-name `{{define}}`
        // (Go's `parse.(*Tree).add` rule). An empty top never pre-populates, so a
        // prior call's body survives until a later non-empty entry replaces it.
        // A non-empty same-name define in the same call can't reach here: it
        // already errored out of `check_in_file_basename_collision` above.
        if !self.name.is_empty() && !tree.is_empty_tree() {
            self.defines
                .insert(self.name.clone(), Arc::new(tree.clone()));
        }

        if self.tree.is_none() || !tree.is_empty_tree() {
            self.tree = Some(tree);
        }

        self.merge_defines(defines, src)?;

        // Mirror Go's final `AddParseTree` that pairs `t.Tree` with
        // `t.tmpl[t.Name()]`: `self.tree` ends up equal to the entry under
        // `self.defines[self.name]`. No-op for an unnamed receiver. Needed
        // both when a `{{define self.name}}` body replaced the slot via
        // `merge_defines`, and when a prior call's entry needs to propagate
        // into a fresh `self.tree`.
        self.sync_tree_to_own_name();
        Ok(self)
    }

    /// The template's name as a parser tag, or `None` when empty. Matches Go's
    /// `parseName` handling: an unnamed template drops the name segment from
    /// error displays rather than rendering as `template: :<line>:<col>: …`.
    fn parse_name(&self) -> Option<&str> {
        (!self.name.is_empty()).then_some(self.name.as_str())
    }

    /// Merge one parse call's `{{define}}` bodies into `self.defines`,
    /// matching Go's `parse.(*Tree).add` + `parse.associate` in a single pass:
    ///
    ///   - A second non-empty body for the same name *within this call* is a
    ///     parse error (Go's `add`). An empty body after a non-empty one in
    ///     the same call is silently dropped, preserving the non-empty body.
    ///   - An empty body never overwrites an existing non-empty define in
    ///     `self.defines` (Go's `associate` IsEmptyTree guard). Non-empty
    ///     bodies always replace, which is how across-call redefinition works.
    ///
    /// The `seen_non_empty` set tracks names already written non-empty in this
    /// call. That is what distinguishes "in-call duplicate, error" from
    /// "across-call duplicate, replace": `self.defines` alone can't tell them
    /// apart. `src` is used only for error-position reporting.
    fn merge_defines(&mut self, defines: Vec<DefineNode>, src: &str) -> Result<()> {
        let mut seen_non_empty: alloc::collections::BTreeSet<Arc<str>> =
            alloc::collections::BTreeSet::new();
        for def in defines {
            let new_is_empty = def.body.is_empty_tree();
            if seen_non_empty.contains(&def.name) {
                if new_is_empty {
                    continue;
                }
                return Err(error::TemplateError::Parse {
                    name: self.parse_name().map(String::from),
                    line: def.pos.line,
                    col: col_for_offset(src, def.pos.offset),
                    message: alloc::format!(
                        "multiple definition of template {:?}",
                        def.name.as_ref()
                    ),
                });
            }
            if new_is_empty && self.defines.contains_key(def.name.as_ref()) {
                continue;
            }
            if !new_is_empty {
                seen_non_empty.insert(def.name.clone());
            }
            self.defines
                .insert(def.name.to_string(), Arc::new(def.body));
        }
        Ok(())
    }

    /// Register a parsed body under `name` using Go's `parse.associate` rule.
    /// An empty body does not overwrite an existing non-empty define. Used by
    /// [`parse_files`](Self::parse_files) for the basename registration.
    fn associate_body(&mut self, name: &str, body: ListNode) {
        if body.is_empty_tree() && self.defines.contains_key(name) {
            return;
        }
        self.defines.insert(name.to_string(), Arc::new(body));
    }

    /// Mirror Go's `parse.(*Tree).add` in-call conflict check: if the top-level
    /// tree is non-empty *and* the same parse produced a non-empty
    /// `{{define name}}`, both would claim the same slot in Go's treeSet, and
    /// Go errors with "multiple definition of template". `name` is the slot
    /// the top-level occupies: self.name for `parse`, the basename for
    /// `parse_files`. An empty `name` skips the check (the unnamed-receiver
    /// case, like `Template::new("")`, has no define to collide with).
    fn check_in_file_basename_collision(
        name: &str,
        top: &ListNode,
        defines: &[DefineNode],
        src: &str,
    ) -> Result<()> {
        if name.is_empty() || top.is_empty_tree() {
            return Ok(());
        }
        if let Some(def) = defines
            .iter()
            .find(|d| d.name.as_ref() == name && !d.body.is_empty_tree())
        {
            return Err(error::TemplateError::Parse {
                name: Some(name.to_string()),
                line: def.pos.line,
                col: col_for_offset(src, def.pos.offset),
                message: alloc::format!("multiple definition of template {name:?}"),
            });
        }
        Ok(())
    }

    /// After a parse merge, mirror Go's final `AddParseTree` step that syncs
    /// `t.Tree` with `t.tmpl[t.Name()]`. Needed when a `{{define}}` body has
    /// replaced the top-level entry, so that `execute` and
    /// `execute_template(name)` render the same thing.
    fn sync_tree_to_own_name(&mut self) {
        if self.name.is_empty() {
            return;
        }
        if let Some(entry) = self.defines.get(self.name.as_str()) {
            self.tree = Some((**entry).clone());
        }
    }

    /// Parse one or more template files and associate them with this template.
    ///
    /// Mirrors Go's `(*Template).ParseFiles` **method** (not the package-level
    /// `template.ParseFiles` constructor). Each file is read and parsed;
    /// `{{define}}` blocks are hoisted into this template's definition map, and
    /// the file's basename is registered as an associated template whose body
    /// is the file's top-level content.
    ///
    /// If a file's basename matches this template's own name (as set via
    /// [`new`](Self::new)), that file's content also becomes the receiver's
    /// main tree, so [`execute`](Self::execute) works directly. Otherwise the
    /// main tree is left alone and callers must use
    /// [`execute_template`](Self::execute_template) with the basename. This
    /// matches Go's `name == t.Name()` branch in `parseFiles`.
    ///
    /// For the constructor-style behavior (receiver's name and tree synthesized
    /// from the first file's basename), call
    /// `Template::new(basename).parse_files(&[...])` explicitly, or use the
    /// top-level [`execute_file`] helper for the single-file case.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError::NoFiles`] if `filenames` is empty, or a
    /// read/parse error for the first file that fails.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use gotmpl::Template;
    ///
    /// let tmpl = Template::new("site")
    ///     .parse_files(&["templates/header.html", "templates/footer.html"])
    ///     .unwrap();
    /// ```
    #[cfg(feature = "std")]
    pub fn parse_files(mut self, filenames: &[&str]) -> Result<Self> {
        if filenames.is_empty() {
            return Err(error::TemplateError::NoFiles);
        }

        for filename in filenames {
            let content =
                std::fs::read_to_string(filename).map_err(|e| error::TemplateError::ReadFile {
                    path: filename.to_string(),
                    source: e,
                })?;

            // Register the file's top-level content under its basename
            let basename = std::path::Path::new(filename)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(filename);

            // Tag the parser with the basename so parse/lex errors report
            // the originating file (Go's parseName), matching its
            // `template: foo.tmpl:12:5: …` format.
            let parser = Parser::with_name(
                Some(basename),
                &content,
                &self.left_delim,
                &self.right_delim,
            )?;
            let (tree, defines) = parser.parse()?;

            // Go parity: if the basename matches the receiver's name, the file's
            // top-level content becomes the receiver's own tree (so `Execute` works
            // directly). Otherwise it is only registered as an associated template.
            // Go's (*Template).ParseFiles dispatches tmpl.Parse(s) per file, so
            // define merging follows the same add+associate rules: an in-file
            // duplicate non-empty define errors, and an empty body never
            // overwrites an existing non-empty one.
            Self::check_in_file_basename_collision(basename, &tree, &defines, &content)?;

            if basename == self.name && (self.tree.is_none() || !tree.is_empty_tree()) {
                self.tree = Some(tree.clone());
            }
            // `associate_body` plays the role of the `parse` pre-populate step:
            // with a non-empty top it seeds `self.defines[basename]` so the
            // subsequent `merge_defines` drops an in-file empty same-name
            // `{{define}}` the same way Go's `parse.(*Tree).add` does.
            self.associate_body(basename, tree);
            self.merge_defines(defines, &content)?;

            // When this file targets the receiver's own slot, pair `self.tree`
            // with the final `self.defines[self.name]` entry (Go's
            // `AddParseTree` invariant). Covers both "empty top + non-empty
            // same-name define replaced the slot" and "prior call's body
            // survives a no-op parse".
            if basename == self.name {
                self.sync_tree_to_own_name();
            }
        }
        Ok(self)
    }

    /// Add a pre-built parse tree as a named template definition, the
    /// counterpart to Go's `template.AddParseTree()`. Useful for injecting
    /// programmatically built ASTs without running the parser.
    ///
    /// Replaces any existing definition with the same `name`.
    ///
    /// # Examples
    ///
    /// ```
    /// use gotmpl::{Template, tmap};
    /// use gotmpl::parse::{ListNode, Node, TextNode, Pos};
    ///
    /// // Build an AST node by hand
    /// let tree = ListNode {
    ///     pos: Pos::new(0, 1),
    ///     nodes: vec![Node::Text(TextNode {
    ///         pos: Pos::new(0, 1),
    ///         text: "injected".into(),
    ///     })],
    /// };
    ///
    /// let result = Template::new("t")
    ///     .parse(r#"{{template "my_tree"}}"#)
    ///     .unwrap()
    ///     .add_parse_tree("my_tree", tree)
    ///     .execute_to_string(&tmap!{})
    ///     .unwrap();
    /// assert_eq!(result, "injected");
    /// ```
    #[must_use]
    pub fn add_parse_tree(mut self, name: &str, tree: ListNode) -> Self {
        self.defines.insert(name.to_string(), Arc::new(tree));
        self
    }

    /// Execute the template, writing output to the given [`fmt::Write`](core::fmt::Write) destination.
    ///
    /// The `data` argument becomes the initial dot (`.`) value inside the template.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The template has not been [parsed](Self::parse) yet.
    /// - An undefined function, template, or variable is referenced.
    /// - A type error occurs during execution (e.g., ranging over a non-iterable).
    /// - A write error occurs.
    /// - The recursive template call depth exceeds the safety limit.
    /// - The per-execution `{{range}}` iteration budget
    ///   ([`max_range_iters`](Self::max_range_iters)) is exhausted.
    pub fn execute_fmt<W: core::fmt::Write>(&self, writer: &mut W, data: &Value) -> Result<()> {
        let tree = self.tree.as_ref().ok_or_else(|| {
            error::TemplateError::Exec(format!("template {:?} has not been parsed", self.name))
        })?;

        let mut executor = Executor::new(&self.funcs, &self.defines);
        executor.set_missing_key(self.missing_key);
        executor.set_max_range_iters(self.max_range_iters);
        executor.execute(writer, tree, data)
    }

    /// Execute a named sub-template, writing output to a [`fmt::Write`](core::fmt::Write) destination.
    ///
    /// Looks up the named template in the definition map and executes it with
    /// the given data. This is the `fmt::Write` counterpart to Go's
    /// `template.ExecuteTemplate()`.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError::UndefinedTemplate`] if no template with the
    /// given name exists, plus all errors from [`execute_fmt`](Self::execute_fmt)
    /// (including [`TemplateError::RangeIterLimit`] and the recursion-limit error).
    pub fn execute_template_fmt<W: core::fmt::Write>(
        &self,
        writer: &mut W,
        name: &str,
        data: &Value,
    ) -> Result<()> {
        let tree = self
            .defines
            .get(name)
            .ok_or_else(|| error::TemplateError::UndefinedTemplate(name.to_string()))?;

        let mut executor = Executor::new(&self.funcs, &self.defines);
        executor.set_missing_key(self.missing_key);
        executor.set_max_range_iters(self.max_range_iters);
        executor.execute(writer, tree.as_ref(), data)
    }

    /// Execute the template, writing output to the given [`io::Write`](std::io::Write) destination.
    ///
    /// Convenience wrapper around [`execute_fmt`](Self::execute_fmt) for
    /// `std::io::Write` targets (files, sockets, `Vec<u8>`, etc.).
    ///
    /// # Errors
    ///
    /// Same as [`execute_fmt`](Self::execute_fmt), plus I/O errors from the writer.
    #[cfg(feature = "std")]
    pub fn execute<W: Write>(&self, writer: &mut W, data: &Value) -> Result<()> {
        let mut adapter = IoAdapter::new(writer);
        self.execute_fmt(&mut adapter, data)
            .map_err(adapter.err_mapper())
    }

    /// Execute a named sub-template, writing output to an [`io::Write`](std::io::Write) destination.
    ///
    /// # Examples
    ///
    /// ```
    /// use gotmpl::{Template, tmap};
    ///
    /// let tmpl = Template::new("root")
    ///     .parse(r#"{{define "header"}}Header: {{.Title}}{{end}}body"#)
    ///     .unwrap();
    ///
    /// let data = tmap! { "Title" => "Hello" };
    ///
    /// // Execute the main template
    /// assert_eq!(tmpl.execute_to_string(&data).unwrap(), "body");
    ///
    /// // Execute just the "header" sub-template
    /// let mut buf = Vec::new();
    /// tmpl.execute_template(&mut buf, "header", &data).unwrap();
    /// assert_eq!(String::from_utf8(buf).unwrap(), "Header: Hello");
    /// ```
    #[cfg(feature = "std")]
    pub fn execute_template<W: Write>(
        &self,
        writer: &mut W,
        name: &str,
        data: &Value,
    ) -> Result<()> {
        let mut adapter = IoAdapter::new(writer);
        self.execute_template_fmt(&mut adapter, name, data)
            .map_err(adapter.err_mapper())
    }

    /// Execute the template and return the result as a [`String`].
    ///
    /// Convenience wrapper around [`execute_fmt`](Self::execute_fmt) that
    /// collects output into a string.
    ///
    /// # Errors
    ///
    /// Same as [`execute_fmt`](Self::execute_fmt).
    pub fn execute_to_string(&self, data: &Value) -> Result<String> {
        let mut buf = String::new();
        self.execute_fmt(&mut buf, data)?;
        Ok(buf)
    }

    /// Execute a named sub-template and return the result as a [`String`].
    ///
    /// Convenience wrapper around [`execute_template_fmt`](Self::execute_template_fmt).
    pub fn execute_template_to_string(&self, name: &str, data: &Value) -> Result<String> {
        let mut buf = String::new();
        self.execute_template_fmt(&mut buf, name, data)?;
        Ok(buf)
    }

    /// Returns the template name set in [`new`](Self::new).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Look up a named template definition, the counterpart to Go's
    /// `template.Lookup()`. Returns `None` when no template with the given
    /// name has been defined (via `{{define}}`, `{{block}}`, or
    /// [`add_parse_tree`](Self::add_parse_tree)).
    ///
    /// # Examples
    ///
    /// ```
    /// use gotmpl::Template;
    ///
    /// let tmpl = Template::new("t")
    ///     .parse(r#"{{define "header"}}...{{end}}"#)
    ///     .unwrap();
    ///
    /// assert!(tmpl.lookup("header").is_some());
    /// assert!(tmpl.lookup("footer").is_none());
    /// ```
    pub fn lookup(&self, name: &str) -> Option<&ListNode> {
        self.defines.get(name).map(Arc::as_ref)
    }

    /// Returns the names of all defined templates, in sorted order.
    ///
    /// The counterpart to Go's `template.Templates()`, returning names rather
    /// than template objects since definitions share the parent's function
    /// map and options.
    ///
    /// # Examples
    ///
    /// ```
    /// use gotmpl::Template;
    ///
    /// let tmpl = Template::new("t")
    ///     .parse(r#"{{define "a"}}...{{end}}{{define "b"}}...{{end}}"#)
    ///     .unwrap();
    ///
    /// assert_eq!(tmpl.templates(), vec!["a", "b"]);
    /// ```
    pub fn templates(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.defines.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
    }

    /// Returns a human-readable string listing all defined templates, the
    /// counterpart to Go's `template.DefinedTemplates()`. Useful for error
    /// messages when a template invocation fails.
    ///
    /// # Examples
    ///
    /// ```
    /// use gotmpl::Template;
    ///
    /// let tmpl = Template::new("t")
    ///     .parse(r#"{{define "header"}}...{{end}}{{define "footer"}}...{{end}}"#)
    ///     .unwrap();
    ///
    /// let s = tmpl.defined_templates();
    /// assert!(s.contains("header"));
    /// assert!(s.contains("footer"));
    /// ```
    pub fn defined_templates(&self) -> String {
        if self.defines.is_empty() {
            return String::new();
        }
        let mut names: Vec<&str> = self.defines.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        let quoted: Vec<String> = names.iter().map(|n| format!("{n:?}")).collect();
        format!("; defined templates are: {}", quoted.join(", "))
    }
}

impl Clone for Template {
    /// Create an independent copy of this template, the counterpart to Go's
    /// `template.Clone()`. The cloned template has its own copy of the define
    /// map and shares the function map (via `Arc`-wrapped closures);
    /// modifications to one do not affect the other.
    ///
    /// # Examples
    ///
    /// ```
    /// use gotmpl::{Template, tmap};
    /// use gotmpl::parse::{ListNode, Node, TextNode, Pos};
    ///
    /// let original = Template::new("t")
    ///     .parse(r#"{{define "x"}}original{{end}}{{template "x"}}"#)
    ///     .unwrap();
    ///
    /// let mut cloned = original.clone();
    ///
    /// // Override "x" in the clone
    /// let cloned = cloned.add_parse_tree("x", ListNode {
    ///     pos: Pos::new(0, 1),
    ///     nodes: vec![Node::Text(TextNode {
    ///         pos: Pos::new(0, 1),
    ///         text: "cloned".into(),
    ///     })],
    /// });
    ///
    /// assert_eq!(original.execute_to_string(&tmap!{}).unwrap(), "original");
    /// assert_eq!(cloned.execute_to_string(&tmap!{}).unwrap(), "cloned");
    /// ```
    fn clone(&self) -> Self {
        Self {
            name: self.name.clone(),
            tree: self.tree.clone(),
            defines: self.defines.clone(),
            funcs: self.funcs.clone(),
            left_delim: self.left_delim.clone(),
            right_delim: self.right_delim.clone(),
            missing_key: self.missing_key,
            max_range_iters: self.max_range_iters,
        }
    }
}

// Convenience constructors
/// Parse and execute a template in one shot.
///
/// Convenience for simple cases that don't need custom functions, delimiters,
/// or options.
///
/// # Examples
///
/// ```
/// use gotmpl::{execute, tmap};
///
/// let result = execute("Hello, {{.Name}}!", &tmap! { "Name" => "World" }).unwrap();
/// assert_eq!(result, "Hello, World!");
/// ```
///
/// # Errors
///
/// Returns a parse or execution error if the template is invalid or
/// execution fails.
pub fn execute(template_src: &str, data: &Value) -> Result<String> {
    Template::new("")
        .parse(template_src)?
        .execute_to_string(data)
}

/// Parse a template file and execute it in one shot.
///
/// Convenience wrapper around [`Template::parse_files`] for the common single-file
/// case. The file's basename is used as the template name for execution.
///
/// # Examples
///
/// ```no_run
/// use gotmpl::{execute_file, tmap};
///
/// let result = execute_file("templates/greeting.tmpl", &tmap! { "Name" => "World" }).unwrap();
/// # let _ = result;
/// ```
///
/// # Errors
///
/// Returns an error if the file cannot be read, the template is invalid, or
/// execution fails.
#[cfg(feature = "std")]
pub fn execute_file(filename: &str, data: &Value) -> Result<String> {
    let basename = std::path::Path::new(filename)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(filename);
    Template::new(basename)
        .parse_files(&[filename])?
        .execute_to_string(data)
}

/// Reports whether a [`Value`] is "true" according to Go's template truthiness rules.
///
/// The counterpart to Go's `template.IsTrue()`. The second "ok" slot is dropped
/// because every [`Value`] variant is always meaningful for truthiness.
///
/// # Examples
///
/// ```
/// use gotmpl::{is_true, Value};
///
/// assert!(is_true(&Value::Bool(true)));
/// assert!(!is_true(&Value::Int(0)));
/// assert!(!is_true(&Value::Nil));
/// ```
pub fn is_true(val: &Value) -> bool {
    val.is_truthy()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToValue;
    use alloc::vec;

    #[test]
    fn test_simple_api() {
        let result = execute("Hello, {{.Name}}!", &tmap! { "Name" => "World" }).unwrap();
        assert_eq!(result, "Hello, World!");
    }

    #[test]
    fn test_custom_func() {
        let result = Template::new("test")
            .func("upper", |args| {
                if let Some(Value::String(s)) = args.first() {
                    Ok(Value::String(s.to_uppercase().into()))
                } else {
                    Ok(Value::Nil)
                }
            })
            .parse("{{.Name | upper}}")
            .unwrap()
            .execute_to_string(&tmap! { "Name" => "hello" })
            .unwrap();
        assert_eq!(result, "HELLO");
    }

    #[test]
    fn test_custom_delims() {
        let result = Template::new("test")
            .delims("<%", "%>")
            .parse("Hello, <%.Name%>!")
            .unwrap()
            .execute_to_string(&tmap! { "Name" => "World" })
            .unwrap();
        assert_eq!(result, "Hello, World!");
    }

    #[test]
    fn test_complex_template() {
        let data = tmap! {
            "Title" => "Users",
            "Users" => vec![
                tmap! { "Name" => "Alice", "Age" => 30i64 }.to_value(),
                tmap! { "Name" => "Bob", "Age" => 25i64 }.to_value(),
            ].to_value(),
        };

        let tmpl = r#"# {{.Title}}
{{range .Users}}- {{.Name}} ({{.Age}})
{{end}}"#;

        let result = execute(tmpl, &data).unwrap();
        assert_eq!(result, "# Users\n- Alice (30)\n- Bob (25)\n");
    }

    #[test]
    fn test_template_inheritance() {
        let data = tmap! { "Content" => "Hello!" };
        let result = Template::new("page")
            .parse(r#"{{define "base"}}<html>{{template "body" .}}</html>{{end}}{{define "body"}}<p>{{.Content}}</p>{{end}}{{template "base" .}}"#)
            .unwrap()
            .execute_to_string(&data)
            .unwrap();
        assert_eq!(result, "<html><p>Hello!</p></html>");
    }

    #[test]
    fn test_pipeline_chaining() {
        let data = tmap! {
            "Items" => vec!["a".to_string(), "bb".to_string(), "ccc".to_string()],
        };
        let result = execute("{{.Items | len | printf \"%d items\"}}", &data).unwrap();
        assert_eq!(result, "3 items");
    }

    #[test]
    fn test_comparison() {
        let data = tmap! { "Score" => 85i64 };
        let result = execute("{{if gt .Score 80}}pass{{else}}fail{{end}}", &data).unwrap();
        assert_eq!(result, "pass");
    }

    #[test]
    fn test_range_with_index() {
        let data = tmap! {
            "Items" => vec!["a".to_string(), "b".to_string()],
        };
        let result = execute("{{range $i, $v := .Items}}{{$i}}:{{$v}} {{end}}", &data);
        assert!(result.is_ok());
    }

    #[test]
    fn test_dollar_variable() {
        let data = tmap! {
            "Name" => "outer",
            "Items" => vec!["inner".to_string()],
        };
        let result = execute("{{range .Items}}{{$}} {{.}}{{end}}", &data);
        assert!(result.is_ok());
    }

    #[test]
    fn test_missingkey_error() {
        let data = tmap! { "X" => 1i64 };
        let result = Template::new("test")
            .missing_key(MissingKey::Error)
            .parse("{{.Missing}}")
            .unwrap()
            .execute_to_string(&data);
        assert!(result.is_err());
    }

    #[test]
    fn test_missingkey_default() {
        let data = tmap! { "X" => 1i64 };
        let result = Template::new("test")
            .parse("{{.Missing}}")
            .unwrap()
            .execute_to_string(&data)
            .unwrap();
        assert_eq!(result, "<no value>");
    }

    #[test]
    fn test_execute_template() {
        let tmpl = Template::new("root")
            .parse(r#"{{define "a"}}hello{{end}}{{define "b"}}world{{end}}main"#)
            .unwrap();

        assert_eq!(
            tmpl.execute_template_to_string("a", &Value::Nil).unwrap(),
            "hello"
        );
        assert_eq!(
            tmpl.execute_template_to_string("b", &Value::Nil).unwrap(),
            "world"
        );

        // Main template
        assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "main");
    }

    #[test]
    fn test_execute_template_undefined() {
        let tmpl = Template::new("t").parse("hello").unwrap();
        let err = tmpl.execute_template_to_string("nope", &Value::Nil);
        assert!(err.is_err());
    }

    #[test]
    fn test_lookup() {
        let tmpl = Template::new("t")
            .parse(r#"{{define "x"}}...{{end}}"#)
            .unwrap();
        assert!(tmpl.lookup("x").is_some());
        assert!(tmpl.lookup("y").is_none());
    }

    #[test]
    fn test_templates_list() {
        let tmpl = Template::new("t")
            .parse(r#"{{define "b"}}...{{end}}{{define "a"}}...{{end}}"#)
            .unwrap();
        assert_eq!(tmpl.templates(), vec!["a", "b"]);
    }

    #[test]
    fn test_defined_templates() {
        let tmpl = Template::new("t")
            .parse(r#"{{define "header"}}...{{end}}{{define "footer"}}...{{end}}"#)
            .unwrap();
        let s = tmpl.defined_templates();
        assert!(s.contains("\"header\""));
        assert!(s.contains("\"footer\""));
    }

    #[test]
    fn test_defined_templates_lists_receiver() {
        // Go parity: `Template::new("t").parse("hello")` seats the top-level
        // into `self.defines["t"]` (Go's `treeSet[t.Name] = topLevelTree`), so
        // `defined_templates()` names "t" itself, matching Go's
        // `DefinedTemplates` output `"; defined templates are: \"t\""`.
        let tmpl = Template::new("t").parse("hello").unwrap();
        assert_eq!(tmpl.defined_templates(), r#"; defined templates are: "t""#);
    }

    #[test]
    fn test_clone_template() {
        let original = Template::new("t")
            .parse(r#"{{define "x"}}original{{end}}{{template "x"}}"#)
            .unwrap();

        let cloned = original.clone().add_parse_tree(
            "x",
            ListNode {
                pos: parse::Pos::new(0, 1),
                nodes: vec![parse::Node::Text(parse::TextNode {
                    pos: parse::Pos::new(0, 1),
                    text: "cloned".into(),
                })],
            },
        );

        assert_eq!(original.execute_to_string(&Value::Nil).unwrap(), "original");
        assert_eq!(cloned.execute_to_string(&Value::Nil).unwrap(), "cloned");
    }

    #[test]
    fn test_add_parse_tree() {
        let tmpl = Template::new("t")
            .parse(r#"{{template "injected"}}"#)
            .unwrap()
            .add_parse_tree(
                "injected",
                ListNode {
                    pos: parse::Pos::new(0, 1),
                    nodes: vec![parse::Node::Text(parse::TextNode {
                        pos: parse::Pos::new(0, 1),
                        text: "works".into(),
                    })],
                },
            );
        assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "works");
    }

    #[test]
    fn test_funcs_bulk() {
        let mut fm = FuncMap::new();
        fm.insert(
            "greet".into(),
            Arc::new(|args: &[Value]| Ok(Value::String(format!("Hi, {}!", args[0]).into()))),
        );
        let result = Template::new("t")
            .funcs(fm)
            .parse(r#"{{greet "World"}}"#)
            .unwrap()
            .execute_to_string(&tmap! {})
            .unwrap();
        assert_eq!(result, "Hi, World!");
    }

    #[test]
    #[cfg(feature = "std")]
    fn test_parse_files() {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join("gotmpl_test_parse_files");
        let _ = std::fs::create_dir_all(&dir);

        let header = dir.join("header.html");
        let footer = dir.join("footer.html");
        std::fs::File::create(&header)
            .unwrap()
            .write_all(b"{{define \"header\"}}<h1>{{.Title}}</h1>{{end}}")
            .unwrap();
        std::fs::File::create(&footer)
            .unwrap()
            .write_all(b"{{define \"footer\"}}<footer>bye</footer>{{end}}")
            .unwrap();

        let h = header.to_str().unwrap();
        let f = footer.to_str().unwrap();

        let tmpl = Template::new("page")
            .parse(r#"{{template "header" .}}{{template "footer" .}}"#)
            .unwrap()
            .parse_files(&[h, f])
            .unwrap();

        let data = tmap! { "Title" => "Hello" };
        let result = tmpl.execute_to_string(&data).unwrap();
        assert_eq!(result, "<h1>Hello</h1><footer>bye</footer>");

        // Also verify the file basename is registered
        assert!(tmpl.lookup("header.html").is_some());
        assert!(tmpl.lookup("footer.html").is_some());

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(feature = "std")]
    fn test_parse_files_not_found() {
        let result = Template::new("t").parse_files(&["/nonexistent/file.html"]);
        let err = result.err().unwrap();
        assert!(
            matches!(
                err,
                error::TemplateError::ReadFile { ref path, .. }
                    if path == "/nonexistent/file.html"
            ),
            "expected ReadFile error, got {:?}",
            err
        );
    }

    #[test]
    #[cfg(feature = "std")]
    fn test_execute_file() {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join("gotmpl_test_execute_file");
        let _ = std::fs::create_dir_all(&dir);

        let path = dir.join("greeting.tmpl");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"Hello, {{.Name}}!")
            .unwrap();

        let data = tmap! { "Name" => "World" };
        let result = execute_file(path.to_str().unwrap(), &data).unwrap();
        assert_eq!(result, "Hello, World!");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(feature = "std")]
    fn test_parse_files_basename_matches_receiver() {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join("gotmpl_test_parse_files_basename");
        let _ = std::fs::create_dir_all(&dir);

        let path = dir.join("main.tmpl");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"Hi {{.Name}}")
            .unwrap();

        let tmpl = Template::new("main.tmpl")
            .parse_files(&[path.to_str().unwrap()])
            .unwrap();
        let data = tmap! { "Name" => "there" };
        assert_eq!(tmpl.execute_to_string(&data).unwrap(), "Hi there");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(feature = "std")]
    fn test_parse_files_error_cites_filename() {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join("gotmpl_test_parse_files_error_name");
        let _ = std::fs::create_dir_all(&dir);

        let path = dir.join("broken.tmpl");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"ok\n{{if}}missing pipeline{{end}}")
            .unwrap();

        let err = Template::new("t")
            .parse_files(&[path.to_str().unwrap()])
            .err()
            .unwrap();

        match &err {
            error::TemplateError::Parse { name, line, .. } => {
                assert_eq!(name.as_deref(), Some("broken.tmpl"));
                assert_eq!(*line, 2);
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
        // Display format matches Go's `template: <name>:<line>:<col>: <msg>` prefix.
        let s = err.to_string();
        assert!(
            s.starts_with("template: broken.tmpl:"),
            "unexpected display: {s}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_parse_error_without_name_drops_name_segment() {
        // Template::new("") -> empty name, so parse() leaves the error untagged
        // and the display format omits the name segment: `template: <line>:<col>: …`.
        let err = Template::new("").parse("{{if}}").err().unwrap();
        let s = err.to_string();
        assert!(s.starts_with("template: "), "got: {s}");
        assert!(!s.contains("template: :"), "should drop name segment: {s}");
    }

    #[test]
    fn test_lex_error_without_name_drops_name_segment() {
        // Symmetric to the Parse case: an unnamed template should omit the
        // `<name>:` segment from lex-error display too (shared format).
        let err = Template::new("").parse("{{\"unterminated}}").err().unwrap();
        match &err {
            error::TemplateError::Lex { name, .. } => {
                assert!(name.is_none(), "expected name to be None, got {name:?}");
            }
            other => panic!("expected Lex error, got {other:?}"),
        }
        let s = err.to_string();
        assert!(s.starts_with("template: "), "got: {s}");
        assert!(!s.contains("template: :"), "should drop name segment: {s}");
    }

    #[test]
    fn test_lex_error_carries_line_col_and_shared_format() {
        // Unterminated quoted string → lex error on the second line.
        let err = Template::new("t")
            .parse("ok\n{{\"unterminated}}")
            .err()
            .unwrap();
        match &err {
            error::TemplateError::Lex {
                name,
                line,
                col,
                message,
            } => {
                assert_eq!(name.as_deref(), Some("t"));
                assert_eq!(*line, 2);
                assert!(*col >= 1);
                assert!(!message.is_empty());
            }
            other => panic!("expected Lex error, got {other:?}"),
        }
        // Shares the Parse variant's `template: <name>:<line>:<col>:` prefix.
        assert!(err.to_string().starts_with("template: t:2:"));
    }

    #[test]
    fn test_parse_error_tagged_with_receiver_name() {
        let err = Template::new("greet.tmpl").parse("{{if}}").err().unwrap();
        match &err {
            error::TemplateError::Parse { name, .. } => {
                assert_eq!(name.as_deref(), Some("greet.tmpl"));
            }
            other => panic!("expected Parse error, got {other:?}"),
        }
        assert!(err.to_string().starts_with("template: greet.tmpl:"));
    }

    #[test]
    #[cfg(feature = "std")]
    fn test_parse_files_empty() {
        let result = Template::new("t").parse_files(&[]);
        let err = result.err().unwrap();
        assert!(
            matches!(err, error::TemplateError::NoFiles),
            "expected NoFiles error, got {:?}",
            err
        );
    }

    // Go parity: Two non-empty defines of the same name *within one file* go
    // through a single `Parse` call in Go and are rejected by `parse.(*Tree).add`.
    #[test]
    #[cfg(feature = "std")]
    fn test_parse_files_in_file_duplicate_define_errors() {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join("gotmpl_test_parse_files_dup_in_file");
        let _ = std::fs::create_dir_all(&dir);

        let path = dir.join("dup.tmpl");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(br#"{{define "x"}}first{{end}}{{define "x"}}second{{end}}"#)
            .unwrap();

        let err = Template::new("t")
            .parse_files(&[path.to_str().unwrap()])
            .err()
            .expect("expected multiple-definition error");
        assert!(
            err.to_string()
                .contains(r#"multiple definition of template "x""#),
            "unexpected error message: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Go parity: Each file is a separate Parse call, so a non-empty define in
    // a later file replaces an earlier one via `parse.associate` (across-call
    // rule). No error.
    #[test]
    #[cfg(feature = "std")]
    fn test_parse_files_across_files_non_empty_replaces() {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join("gotmpl_test_parse_files_across_replace");
        let _ = std::fs::create_dir_all(&dir);

        let a = dir.join("a.tmpl");
        let b = dir.join("b.tmpl");
        std::fs::File::create(&a)
            .unwrap()
            .write_all(br#"{{define "x"}}first{{end}}"#)
            .unwrap();
        std::fs::File::create(&b)
            .unwrap()
            .write_all(br#"{{define "x"}}second{{end}}"#)
            .unwrap();

        let tmpl = Template::new("t")
            .parse(r#"{{template "x"}}"#)
            .unwrap()
            .parse_files(&[a.to_str().unwrap(), b.to_str().unwrap()])
            .unwrap();
        assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "second");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Go parity: An empty define in a later file does not overwrite an existing
    // non-empty define (`parse.associate`'s IsEmptyTree guard).
    #[test]
    #[cfg(feature = "std")]
    fn test_parse_files_empty_define_does_not_clobber() {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join("gotmpl_test_parse_files_empty_clobber");
        let _ = std::fs::create_dir_all(&dir);

        let a = dir.join("a.tmpl");
        let b = dir.join("b.tmpl");
        std::fs::File::create(&a)
            .unwrap()
            .write_all(br#"{{define "x"}}content{{end}}"#)
            .unwrap();
        std::fs::File::create(&b)
            .unwrap()
            .write_all(br#"{{define "x"}}{{end}}"#)
            .unwrap();

        let tmpl = Template::new("t")
            .parse(r#"{{template "x"}}"#)
            .unwrap()
            .parse_files(&[a.to_str().unwrap(), b.to_str().unwrap()])
            .unwrap();
        assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "content");

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Go parity: `Template::new("X").parse("{{define \"X\"}}body{{end}}")`
    // makes both `execute` and `execute_template("X")` render the define body.
    // Go's parse.(*Tree).add leaves the non-empty define in the treeSet slot,
    // and AddParseTree then syncs t.Tree with it.
    #[test]
    fn test_parse_define_name_matches_receiver_syncs_tree() {
        let tmpl = Template::new("X")
            .parse(r#"{{define "X"}}body{{end}}"#)
            .unwrap();
        assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "body");
        assert_eq!(
            tmpl.execute_template_to_string("X", &Value::Nil).unwrap(),
            "body"
        );
    }

    // Go parity: a non-empty top-level plus an *empty* same-name `{{define}}`
    // is not a collision. Go's `parse.(*Tree).add` drops the empty define and
    // leaves the non-empty top-level in the treeSet slot. A regression here
    // (empty define clobbering the top-level tree via sync) would render "".
    #[test]
    fn test_parse_toplevel_and_empty_same_name_define_keeps_toplevel() {
        let tmpl = Template::new("X")
            .parse(r#"toplevel{{define "X"}}{{end}}"#)
            .unwrap();
        assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "toplevel");
        assert_eq!(
            tmpl.execute_template_to_string("X", &Value::Nil).unwrap(),
            "toplevel"
        );
    }

    // Go parity: a later `Parse` call whose top-level name matches the receiver
    // replaces the prior body, even when an earlier `Parse` installed a
    // `{{define name}}` of the same name. A regression that unconditionally
    // re-syncs from `self.defines[self.name]` would resurrect the old body.
    #[test]
    fn test_parse_second_call_toplevel_replaces_prior_same_name_define() {
        let tmpl = Template::new("X")
            .parse(r#"{{define "X"}}body{{end}}"#)
            .unwrap()
            .parse("newtop")
            .unwrap();
        assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "newtop");
        assert_eq!(
            tmpl.execute_template_to_string("X", &Value::Nil).unwrap(),
            "newtop"
        );
    }

    // Go parity: `ParseFiles` analogue of the non-empty-top + empty-same-name
    // define case. File "X" contains `toplevel{{define "X"}}{{end}}`, receiver
    // is "X". Go's treeSet["X"] keeps the top-level; the empty define is
    // dropped. A regression would render "".
    #[test]
    #[cfg(feature = "std")]
    fn test_parse_files_toplevel_and_empty_same_name_define_keeps_toplevel() {
        use std::io::Write as _;
        let dir =
            std::env::temp_dir().join("gotmpl_test_parse_files_toplevel_empty_define_keeps_top");
        let _ = std::fs::create_dir_all(&dir);

        let path = dir.join("X");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(br#"toplevel{{define "X"}}{{end}}"#)
            .unwrap();

        let tmpl = Template::new("X")
            .parse_files(&[path.to_str().unwrap()])
            .unwrap();
        assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "toplevel");
        assert_eq!(
            tmpl.execute_template_to_string("X", &Value::Nil).unwrap(),
            "toplevel"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    // Go parity: a non-empty top-level plus a non-empty `{{define name}}` where
    // name matches the receiver is `template: multiple definition of template`.
    #[test]
    fn test_parse_toplevel_and_same_name_define_errors() {
        let err = Template::new("X")
            .parse(r#"toplevel {{define "X"}}body{{end}}"#)
            .err()
            .expect("expected multiple-definition error");
        assert!(
            err.to_string()
                .contains(r#"multiple definition of template "X""#),
            "unexpected error message: {err}"
        );
    }

    // Go parity: `ParseFiles` on a file whose basename matches the receiver
    // and whose only content is `{{define basename}}body{{end}}` renders
    // `body` for both `execute` and `execute_template(basename)`.
    #[test]
    #[cfg(feature = "std")]
    fn test_parse_files_define_name_matches_basename_syncs_tree() {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join("gotmpl_test_parse_files_basename_define_sync");
        let _ = std::fs::create_dir_all(&dir);

        let path = dir.join("X");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(br#"{{define "X"}}body{{end}}"#)
            .unwrap();

        let tmpl = Template::new("X")
            .parse_files(&[path.to_str().unwrap()])
            .unwrap();
        assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "body");
        assert_eq!(
            tmpl.execute_template_to_string("X", &Value::Nil).unwrap(),
            "body"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_html_escape() {
        assert_eq!(html_escape("<b>hi</b>"), "&lt;b&gt;hi&lt;/b&gt;");
        assert_eq!(html_escape("a&b"), "a&amp;b");
    }

    #[test]
    fn test_js_escape() {
        assert_eq!(js_escape("a'b"), "a\\'b");
    }

    #[test]
    fn test_url_encode() {
        assert_eq!(url_encode("hello world"), "hello%20world");
    }

    #[test]
    fn test_is_true() {
        assert!(is_true(&Value::Bool(true)));
        assert!(!is_true(&Value::Bool(false)));
        assert!(!is_true(&Value::Int(0)));
        assert!(is_true(&Value::Int(1)));
        assert!(!is_true(&Value::Nil));
    }
}
