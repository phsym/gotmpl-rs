//! Go `html/template`-style context-aware auto-escaping.
//!
//! [`Template`] is a drop-in analog of [`crate::Template`] whose output is
//! contextually auto-escaped: values interpolated into HTML text, tag and
//! attribute names, quoted/unquoted attribute values, URLs, `srcset`,
//! JavaScript, and CSS are each escaped according to the surrounding context,
//! exactly as Go's `html/template` does.
//!
//! Escaping is applied by a static pass (mirroring Go's `escape.go`) that runs
//! lazily on the first `execute*` call — so `{{template "x"}}` can reference
//! templates added incrementally beforehand. Once escaped, the template can no
//! longer be parsed into (matching Go's "cannot Parse after Execute").
//!
//! Trusted content that should bypass escaping is carried by the
//! [`HTML`](crate::html::HTML), [`HTMLAttr`](crate::html::HTMLAttr),
//! [`JS`](crate::html::JS), [`JSStr`](crate::html::JSStr),
//! [`CSS`](crate::html::CSS), [`URL`](crate::html::URL), and
//! [`Srcset`](crate::html::Srcset) wrappers (see [`crate::Value::Safe`]).
//!
//! ```
//! use gotmpl::html::Template;
//!
//! let t = Template::new("page").parse("<p>{{.Msg}}</p>").unwrap();
//! assert_eq!(t.name(), "page");
//! ```

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::error::{Result, TemplateError};
use crate::parse::ListNode;
use crate::value::Value;
use crate::{FuncMap, MissingKey};

mod content;
mod context;
mod escape;
mod escapers;
mod lex;
mod primitives;
mod transition;

pub use content::{CSS, HTML, HTMLAttr, JS, JSStr, Srcset, URL};

use escape::EscapeSet;

/// Lazily-computed cache of the escaped tree set. `OnceLock` under `std` (so
/// [`Template`] stays `Sync`); `OnceCell` under `no_std` (which makes
/// [`Template`] `!Sync`).
#[cfg(feature = "std")]
type EscapeCache = std::sync::OnceLock<EscapeSet>;
#[cfg(not(feature = "std"))]
type EscapeCache = core::cell::OnceCell<EscapeSet>;

/// An auto-escaping template, the analog of Go's
/// [`html/template.Template`](https://pkg.go.dev/html/template#Template).
///
/// Wraps a [`crate::Template`] and applies context-aware escaping to its output.
/// Build and parse exactly like [`crate::Template`]; the escaping pass runs
/// automatically on first execution.
pub struct Template {
    inner: crate::Template,
    escaped: EscapeCache,
}

impl Template {
    /// Create a new, empty auto-escaping template with the given name.
    pub fn new(name: &str) -> Self {
        Template {
            inner: crate::Template::new(name),
            escaped: EscapeCache::new(),
        }
    }

    /// Set the total number of `{{range}}` iterations allowed per execution.
    /// See [`crate::Template::max_range_iters`].
    #[must_use]
    pub fn max_range_iters(mut self, n: u64) -> Self {
        self.inner = self.inner.max_range_iters(n);
        self
    }

    /// Set custom action delimiters. See [`crate::Template::delims`].
    #[must_use]
    pub fn delims(mut self, left: &str, right: &str) -> Self {
        self.inner = self.inner.delims(left, right);
        self
    }

    /// Set the behavior for missing map keys. See [`crate::Template::missing_key`].
    #[must_use]
    pub fn missing_key(mut self, mk: MissingKey) -> Self {
        self.inner = self.inner.missing_key(mk);
        self
    }

    /// Register a custom template function. See [`crate::Template::func`].
    #[must_use]
    pub fn func(
        mut self,
        name: &str,
        f: impl Fn(&[Value]) -> Result<Value> + Send + Sync + 'static,
    ) -> Self {
        self.inner = self.inner.func(name, f);
        self
    }

    /// Register several template functions at once. See [`crate::Template::funcs`].
    #[must_use]
    pub fn funcs(mut self, func_map: FuncMap) -> Self {
        self.inner = self.inner.funcs(func_map);
        self
    }

    /// Parse template source. See [`crate::Template::parse`].
    ///
    /// Returns an error if this template has already been executed (and thus
    /// escaped): `"html/template: cannot Parse after Execute"`.
    pub fn parse(mut self, src: &str) -> Result<Self> {
        self.check_can_parse()?;
        self.inner = self.inner.parse(src)?;
        Ok(self)
    }

    /// Parse templates from files. See [`crate::Template::parse_files`].
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    pub fn parse_files(mut self, filenames: &[&str]) -> Result<Self> {
        self.check_can_parse()?;
        self.inner = self.inner.parse_files(filenames)?;
        Ok(self)
    }

    /// Parse templates matching a glob pattern. See [`crate::Template::parse_glob`].
    #[cfg(feature = "glob")]
    #[cfg_attr(docsrs, doc(cfg(feature = "glob")))]
    pub fn parse_glob(mut self, pattern: &str) -> Result<Self> {
        self.check_can_parse()?;
        self.inner = self.inner.parse_glob(pattern)?;
        Ok(self)
    }

    /// Associate a parsed tree under a name. See [`crate::Template::add_parse_tree`].
    ///
    /// Returns an error if this template has already been executed.
    pub fn add_parse_tree(mut self, name: &str, tree: ListNode) -> Result<Self> {
        self.check_can_parse()?;
        self.inner = self.inner.add_parse_tree(name, tree);
        Ok(self)
    }

    /// Guard mirroring Go's `checkCanParse`: once escaping has run (on first
    /// execute), the template set is frozen.
    fn check_can_parse(&self) -> Result<()> {
        if self.escaped.get().is_some() {
            return Err(TemplateError::Exec(
                "html/template: cannot Parse after Execute".to_string(),
            ));
        }
        Ok(())
    }

    /// Escape the template set (lazily, once) and return the cached result.
    fn escape_set(&self) -> Result<&EscapeSet> {
        if self.escaped.get().is_none() {
            let set = escape::escape(&self.inner)?;
            // A concurrent initializer may win the race; the result is
            // deterministic, so either set is equivalent.
            let _ = self.escaped.set(set);
        }
        match self.escaped.get() {
            Some(set) => Ok(set),
            None => Err(TemplateError::Exec(
                "html/template: escaping did not complete".to_string(),
            )),
        }
    }

    /// Execute the template into a [`core::fmt::Write`], escaping output
    /// contextually. See [`crate::Template::execute_fmt`].
    pub fn execute_fmt<W: core::fmt::Write>(&self, writer: &mut W, data: &Value) -> Result<()> {
        let set = self.escape_set()?;
        self.inner
            .execute_tree_fmt(writer, &set.entry, &set.templates, &set.funcs, data)
    }

    /// Execute the template and return the escaped output as a `String`.
    pub fn execute_to_string(&self, data: &Value) -> Result<String> {
        let mut buf = String::new();
        self.execute_fmt(&mut buf, data)?;
        Ok(buf)
    }

    /// Execute the template into a [`std::io::Write`]. See [`crate::Template::execute`].
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    pub fn execute<W: std::io::Write>(&self, writer: &mut W, data: &Value) -> Result<()> {
        let mut adapter = crate::IoAdapter::new(writer);
        self.execute_fmt(&mut adapter, data)
            .map_err(adapter.err_mapper())
    }

    /// Execute a named sub-template into a [`core::fmt::Write`], escaping output.
    pub fn execute_template_fmt<W: core::fmt::Write>(
        &self,
        writer: &mut W,
        name: &str,
        data: &Value,
    ) -> Result<()> {
        let set = self.escape_set()?;
        let tree = set
            .templates
            .get(name)
            .ok_or_else(|| TemplateError::UndefinedTemplate(name.to_string()))?;
        self.inner
            .execute_tree_fmt(writer, tree, &set.templates, &set.funcs, data)
    }

    /// Execute a named sub-template and return the escaped output as a `String`.
    pub fn execute_template_to_string(&self, name: &str, data: &Value) -> Result<String> {
        let mut buf = String::new();
        self.execute_template_fmt(&mut buf, name, data)?;
        Ok(buf)
    }

    /// Execute a named sub-template into a [`std::io::Write`].
    #[cfg(feature = "std")]
    #[cfg_attr(docsrs, doc(cfg(feature = "std")))]
    pub fn execute_template<W: std::io::Write>(
        &self,
        writer: &mut W,
        name: &str,
        data: &Value,
    ) -> Result<()> {
        let mut adapter = crate::IoAdapter::new(writer);
        self.execute_template_fmt(&mut adapter, name, data)
            .map_err(adapter.err_mapper())
    }

    /// Returns the template name. See [`crate::Template::name`].
    pub fn name(&self) -> &str {
        self.inner.name()
    }

    /// Look up a named template definition. See [`crate::Template::lookup`].
    pub fn lookup(&self, name: &str) -> Option<&ListNode> {
        self.inner.lookup(name)
    }

    /// Returns the names of all defined templates. See [`crate::Template::templates`].
    pub fn templates(&self) -> Vec<&str> {
        self.inner.templates()
    }

    /// Returns a human-readable listing of defined templates.
    /// See [`crate::Template::defined_templates`].
    pub fn defined_templates(&self) -> String {
        self.inner.defined_templates()
    }
}

impl Clone for Template {
    /// Clone the template. The clone starts un-escaped (its escaping runs on
    /// its own first execute), so it may be reconfigured and parsed into.
    fn clone(&self) -> Self {
        Template {
            inner: self.inner.clone(),
            escaped: EscapeCache::new(),
        }
    }
}
