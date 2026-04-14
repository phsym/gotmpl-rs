#![doc = include_str!("../README.md")]

pub(crate) mod error;
pub(crate) mod exec;
pub(crate) mod funcs;
/// Parser, lexer, and AST node types for the Go template language.
///
/// This module mirrors Go's `text/template/parse` package. Most users don't
/// need it directly — use [`Template::parse`] instead. The AST types are public
/// for advanced use cases like [`Template::add_parse_tree`].
pub mod parse;
pub(crate) mod value;

// ─── Public re-exports ──────────────────────────────────────────────────
// All user-facing types are available at the crate root.

pub use error::{TemplateError, Result};
pub use funcs::{Func, html_escape, js_escape, url_encode};
use funcs::builtins;
pub use value::{Value, ToValue, ValueFunc};

use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;

use exec::{Executor, MissingKey};
use parse::{ListNode, Parser};

/// A function map mapping names to template functions.
///
/// Equivalent to Go's `template.FuncMap`. Used with [`Template::funcs`] to
/// register multiple functions at once.
///
/// # Examples
///
/// ```
/// use std::sync::Arc;
/// use go_template_rs::{FuncMap, Value};
///
/// let mut fm = FuncMap::new();
/// fm.insert("double".into(), Arc::new(|args: &[Value]| {
///     Ok(Value::Int(args[0].as_int().unwrap_or(0) * 2))
/// }));
/// ```
pub type FuncMap = HashMap<String, Func>;

/// A parsed, ready-to-execute template.
///
/// This is the main entry point of the library, equivalent to Go's
/// [`template.Template`](https://pkg.go.dev/text/template#Template).
///
/// Use the builder-style API to configure, parse, and execute templates:
///
/// ```
/// use go_template_rs::{Template, tmap};
///
/// let output = Template::new("greet")
///     .delims("<<", ">>")                        // optional: custom delimiters
///     .option("missingkey=error")                 // optional: error on missing keys
///     .func("shout", |args| {                     // optional: custom functions
///         let s = format!("{}", args[0]).to_uppercase();
///         Ok(go_template_rs::Value::String(s))
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
    defines: HashMap<String, ListNode>,
    funcs: HashMap<String, Func>,
    left_delim: String,
    right_delim: String,
    missing_key: MissingKey,
}

impl Template {
    /// Create a new, empty template with the given name.
    ///
    /// The name is used in error messages and when invoking templates via
    /// `{{template "name"}}`. All built-in functions are
    /// registered automatically.
    pub fn new(name: &str) -> Self {
        Template {
            name: name.to_string(),
            tree: None,
            defines: HashMap::new(),
            funcs: builtins(),
            left_delim: "{{".to_string(),
            right_delim: "}}".to_string(),
            missing_key: MissingKey::default(),
        }
    }

    /// Set custom action delimiters (default: `"{{"` and `"}}"`).
    ///
    /// Must be called **before** [`parse`](Self::parse).
    ///
    /// # Examples
    ///
    /// ```
    /// use go_template_rs::{Template, tmap};
    ///
    /// let result = Template::new("t")
    ///     .delims("<%", "%>")
    ///     .parse("Hello, <%.Name%>!")
    ///     .unwrap()
    ///     .execute_to_string(&tmap! { "Name" => "World" })
    ///     .unwrap();
    /// assert_eq!(result, "Hello, World!");
    /// ```
    pub fn delims(mut self, left: &str, right: &str) -> Self {
        self.left_delim = left.to_string();
        self.right_delim = right.to_string();
        self
    }

    /// Set execution options, matching Go's `template.Option()`.
    ///
    /// # Supported options
    ///
    /// | Option | Behavior |
    /// |--------|----------|
    /// | `"missingkey=invalid"` | Missing map keys return [`Value::Nil`] (default) |
    /// | `"missingkey=zero"` | Same as `invalid` |
    /// | `"missingkey=default"` | Same as `invalid` |
    /// | `"missingkey=error"` | Missing map keys cause a [`TemplateError::Exec`] |
    ///
    /// Unknown options are silently ignored.
    pub fn option(mut self, opt: &str) -> Self {
        match opt {
            "missingkey=invalid" => self.missing_key = MissingKey::Invalid,
            "missingkey=zero" => self.missing_key = MissingKey::ZeroValue,
            "missingkey=default" => self.missing_key = MissingKey::Invalid,
            "missingkey=error" => self.missing_key = MissingKey::Error,
            _ => {} // ignore unknown options like Go does
        }
        self
    }

    /// Register a custom template function.
    ///
    /// Must be called **before** [`parse`](Self::parse). Functions receive their
    /// arguments as a `&[Value]` slice and return a [`Result<Value>`](error::Result).
    ///
    /// The function is available inside templates by the given `name`.
    /// Registering a name that matches a built-in replaces it.
    ///
    /// # Examples
    ///
    /// ```
    /// use go_template_rs::{Template, tmap, Value};
    ///
    /// let result = Template::new("t")
    ///     .func("double", |args| {
    ///         let n = args[0].as_int().unwrap_or(0);
    ///         Ok(Value::Int(n * 2))
    ///     })
    ///     .parse("{{double 21}}")
    ///     .unwrap()
    ///     .execute_to_string(&tmap!{})
    ///     .unwrap();
    /// assert_eq!(result, "42");
    /// ```
    pub fn func(
        mut self,
        name: &str,
        f: impl Fn(&[Value]) -> Result<Value> + Send + Sync + 'static,
    ) -> Self {
        self.funcs.insert(name.to_string(), Arc::new(f));
        self
    }

    /// Register multiple template functions at once from a [`FuncMap`].
    ///
    /// Equivalent to Go's `template.Funcs()`. Must be called **before**
    /// [`parse`](Self::parse).
    ///
    /// # Examples
    ///
    /// ```
    /// use std::sync::Arc;
    /// use go_template_rs::{Template, FuncMap, tmap, Value};
    ///
    /// let mut fm = FuncMap::new();
    /// fm.insert("greet".into(), Arc::new(|args: &[Value]| {
    ///     Ok(Value::String(format!("Hello, {}!", args[0])))
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
    pub fn funcs(mut self, func_map: FuncMap) -> Self {
        self.funcs.extend(func_map);
        self
    }

    /// Parse the template source string.
    ///
    /// This lexes and parses the source, extracting `{{define}}` blocks into the
    /// template's definition map. Must be called after [`delims`](Self::delims)
    /// and [`func`](Self::func).
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError::Lex`] or
    /// [`TemplateError::Parse`] if the source
    /// contains syntax errors.
    pub fn parse(mut self, src: &str) -> Result<Self> {
        let parser = Parser::new(src, &self.left_delim, &self.right_delim)?;
        let (tree, defines) = parser.parse()?;

        self.tree = Some(tree);
        for def in defines {
            self.defines.insert(def.name.clone(), def.body);
        }

        Ok(self)
    }

    /// Parse an additional template string and merge its `{{define}}` blocks.
    ///
    /// This allows building a template set from multiple sources, similar to
    /// Go's `ParseFiles` / `ParseGlob`. Only `{{define}}` blocks from the
    /// additional source are extracted; top-level content is ignored.
    ///
    /// # Errors
    ///
    /// Returns a parse error if the source contains syntax errors.
    pub fn parse_additional(mut self, src: &str) -> Result<Self> {
        let parser = Parser::new(src, &self.left_delim, &self.right_delim)?;
        let (_, defines) = parser.parse()?;

        for def in defines {
            self.defines.insert(def.name.clone(), def.body);
        }

        Ok(self)
    }

    /// Parse template definitions from one or more files and merge them.
    ///
    /// Equivalent to Go's `template.ParseFiles()`. Each file is read and parsed;
    /// `{{define}}` blocks are extracted and added to this template's definition map.
    /// The file's basename (without directory) is also registered as a template name
    /// for the file's top-level content.
    ///
    /// # Errors
    ///
    /// Returns an error if any file cannot be read or contains syntax errors.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use go_template_rs::Template;
    ///
    /// let tmpl = Template::new("site")
    ///     .parse_files(&["templates/header.html", "templates/footer.html"])
    ///     .unwrap();
    /// ```
    pub fn parse_files(mut self, filenames: &[&str]) -> Result<Self> {
        for filename in filenames {
            let content = std::fs::read_to_string(filename)
                .map_err(|e| error::TemplateError::Exec(
                    format!("parse_files: {}: {}", filename, e)
                ))?;

            let parser = Parser::new(&content, &self.left_delim, &self.right_delim)?;
            let (tree, defines) = parser.parse()?;

            // Register the file's top-level content under its basename
            let basename = std::path::Path::new(filename)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(filename);
            self.defines.insert(basename.to_string(), tree);

            for def in defines {
                self.defines.insert(def.name.clone(), def.body);
            }
        }
        Ok(self)
    }

    /// Add a pre-built parse tree as a named template definition.
    ///
    /// Equivalent to Go's `template.AddParseTree()`. This allows injecting
    /// programmatically constructed ASTs without going through the parser.
    ///
    /// If a definition with the same `name` already exists, it is replaced.
    ///
    /// # Examples
    ///
    /// ```
    /// use go_template_rs::{Template, tmap};
    /// use go_template_rs::parse::{ListNode, Node, TextNode, Pos};
    ///
    /// // Build an AST node by hand
    /// let tree = ListNode {
    ///     pos: Pos::new(0, 1),
    ///     nodes: vec![Node::Text(TextNode {
    ///         pos: Pos::new(0, 1),
    ///         text: "injected".to_string(),
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
    pub fn add_parse_tree(mut self, name: &str, tree: ListNode) -> Self {
        self.defines.insert(name.to_string(), tree);
        self
    }

    /// Execute the template, writing output to the given writer.
    ///
    /// The `data` argument becomes the initial dot (`.`) value inside the template.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The template has not been [parsed](Self::parse) yet.
    /// - An undefined function, template, or variable is referenced.
    /// - A type error occurs during execution (e.g., ranging over a non-iterable).
    /// - An I/O error occurs writing to `writer`.
    /// - The recursive template call depth exceeds the safety limit.
    pub fn execute<W: Write>(&self, writer: &mut W, data: &Value) -> Result<()> {
        let tree = self
            .tree
            .as_ref()
            .ok_or_else(|| error::TemplateError::Exec(
                format!("template {:?} has not been parsed", self.name)
            ))?;

        let mut executor = Executor::new(&self.funcs, &self.defines);
        executor.set_missing_key(self.missing_key);
        executor.execute(writer, tree, data)
    }

    /// Execute a named sub-template (from a `{{define}}` or `{{block}}`).
    ///
    /// Equivalent to Go's `template.ExecuteTemplate()`. Looks up the named
    /// template in the definition map and executes it with the given data.
    ///
    /// # Errors
    ///
    /// Returns [`TemplateError::UndefinedTemplate`]
    /// if no template with the given name exists, plus all errors from
    /// [`execute`](Self::execute).
    ///
    /// # Examples
    ///
    /// ```
    /// use go_template_rs::{Template, tmap};
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
    pub fn execute_template<W: Write>(
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
        executor.execute(writer, tree, data)
    }

    /// Execute the template and return the result as a [`String`].
    ///
    /// Convenience wrapper around [`execute`](Self::execute) that collects
    /// output into a string.
    ///
    /// # Errors
    ///
    /// Same as [`execute`](Self::execute), plus an error if the output is
    /// not valid UTF-8 (should not happen in practice).
    pub fn execute_to_string(&self, data: &Value) -> Result<String> {
        let mut buf = Vec::new();
        self.execute(&mut buf, data)?;
        String::from_utf8(buf)
            .map_err(|e| error::TemplateError::Exec(e.to_string()))
    }

    /// Returns the template name set in [`new`](Self::new).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Look up a named template definition.
    ///
    /// Equivalent to Go's `template.Lookup()`. Returns `None` if no template
    /// with the given name has been defined (via `{{define}}`, `{{block}}`,
    /// or [`add_parse_tree`](Self::add_parse_tree)).
    ///
    /// # Examples
    ///
    /// ```
    /// use go_template_rs::Template;
    ///
    /// let tmpl = Template::new("t")
    ///     .parse(r#"{{define "header"}}...{{end}}"#)
    ///     .unwrap();
    ///
    /// assert!(tmpl.lookup("header").is_some());
    /// assert!(tmpl.lookup("footer").is_none());
    /// ```
    pub fn lookup(&self, name: &str) -> Option<&ListNode> {
        self.defines.get(name)
    }

    /// Returns the names of all defined templates.
    ///
    /// Equivalent to Go's `template.Templates()`, but returns names rather
    /// than template objects (since definitions share the parent's function
    /// map and options).
    ///
    /// The names are returned in sorted order for deterministic output.
    ///
    /// # Examples
    ///
    /// ```
    /// use go_template_rs::Template;
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

    /// Returns a human-readable string listing all defined templates.
    ///
    /// Equivalent to Go's `template.DefinedTemplates()`. Useful for error
    /// messages when a template invocation fails.
    ///
    /// # Examples
    ///
    /// ```
    /// use go_template_rs::Template;
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

    /// Create an independent copy of this template.
    ///
    /// Equivalent to Go's `template.Clone()`. The cloned template has its
    /// own copy of all defined templates and shares the same function map
    /// (via `Arc`-wrapped closures) — modifications to one do not affect the other.
    ///
    /// # Examples
    ///
    /// ```
    /// use go_template_rs::{Template, tmap};
    /// use go_template_rs::parse::{ListNode, Node, TextNode, Pos};
    ///
    /// let original = Template::new("t")
    ///     .parse(r#"{{define "x"}}original{{end}}{{template "x"}}"#)
    ///     .unwrap();
    ///
    /// let mut cloned = original.clone_template();
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
    pub fn clone_template(&self) -> Self {
        Template {
            name: self.name.clone(),
            tree: self.tree.clone(),
            defines: self.defines.clone(),
            funcs: self.funcs.clone(),
            left_delim: self.left_delim.clone(),
            right_delim: self.right_delim.clone(),
            missing_key: self.missing_key,
        }
    }
}

// ─── Convenience constructors ────────────────────────────────────────────

/// Parse and execute a template in one shot.
///
/// This is a convenience function for simple cases where you don't need
/// custom functions, delimiters, or options.
///
/// # Examples
///
/// ```
/// use go_template_rs::{execute, tmap};
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
    Template::new("").parse(template_src)?.execute_to_string(data)
}

/// Reports whether a [`Value`] is "true" according to Go's template truthiness rules.
///
/// Equivalent to Go's `template.IsTrue()`. Returns `(truth, ok)` where `ok`
/// indicates whether the truthiness check is meaningful for this type
/// (always `true` for our [`Value`] enum).
///
/// # Examples
///
/// ```
/// use go_template_rs::{is_true, Value};
///
/// assert_eq!(is_true(&Value::Bool(true)), (true, true));
/// assert_eq!(is_true(&Value::Int(0)), (false, true));
/// assert_eq!(is_true(&Value::Nil), (false, true));
/// ```
pub fn is_true(val: &Value) -> (bool, bool) {
    (val.is_truthy(), true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToValue;

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
                    Ok(Value::String(s.to_uppercase()))
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
        let result = execute(
            "{{if gt .Score 80}}pass{{else}}fail{{end}}",
            &data,
        )
        .unwrap();
        assert_eq!(result, "pass");
    }

    #[test]
    fn test_range_with_index() {
        let data = tmap! {
            "Items" => vec!["a".to_string(), "b".to_string()],
        };
        let result = execute(
            "{{range $i, $v := .Items}}{{$i}}:{{$v}} {{end}}",
            &data,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_dollar_variable() {
        let data = tmap! {
            "Name" => "outer",
            "Items" => vec!["inner".to_string()],
        };
        let result = execute(
            "{{range .Items}}{{$}} {{.}}{{end}}",
            &data,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_missingkey_error() {
        let data = tmap! { "X" => 1i64 };
        let result = Template::new("test")
            .option("missingkey=error")
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
        assert_eq!(result, "<nil>");
    }

    #[test]
    fn test_execute_template() {
        let tmpl = Template::new("root")
            .parse(r#"{{define "a"}}hello{{end}}{{define "b"}}world{{end}}main"#)
            .unwrap();

        let mut buf = Vec::new();
        tmpl.execute_template(&mut buf, "a", &Value::Nil).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "hello");

        let mut buf = Vec::new();
        tmpl.execute_template(&mut buf, "b", &Value::Nil).unwrap();
        assert_eq!(String::from_utf8(buf).unwrap(), "world");

        // Main template
        assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "main");
    }

    #[test]
    fn test_execute_template_undefined() {
        let tmpl = Template::new("t").parse("hello").unwrap();
        let mut buf = Vec::new();
        let err = tmpl.execute_template(&mut buf, "nope", &Value::Nil);
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
    fn test_defined_templates_empty() {
        let tmpl = Template::new("t").parse("hello").unwrap();
        assert_eq!(tmpl.defined_templates(), "");
    }

    #[test]
    fn test_clone_template() {
        let original = Template::new("t")
            .parse(r#"{{define "x"}}original{{end}}{{template "x"}}"#)
            .unwrap();

        let cloned = original.clone_template().add_parse_tree("x", ListNode {
            pos: parse::Pos::new(0, 1),
            nodes: vec![parse::Node::Text(parse::TextNode {
                pos: parse::Pos::new(0, 1),
                text: "cloned".into(),
            })],
        });

        assert_eq!(original.execute_to_string(&Value::Nil).unwrap(), "original");
        assert_eq!(cloned.execute_to_string(&Value::Nil).unwrap(), "cloned");
    }

    #[test]
    fn test_add_parse_tree() {
        let tmpl = Template::new("t")
            .parse(r#"{{template "injected"}}"#)
            .unwrap()
            .add_parse_tree("injected", ListNode {
                pos: parse::Pos::new(0, 1),
                nodes: vec![parse::Node::Text(parse::TextNode {
                    pos: parse::Pos::new(0, 1),
                    text: "works".into(),
                })],
            });
        assert_eq!(tmpl.execute_to_string(&Value::Nil).unwrap(), "works");
    }

    #[test]
    fn test_funcs_bulk() {
        let mut fm = FuncMap::new();
        fm.insert("greet".into(), Arc::new(|args: &[Value]| {
            Ok(Value::String(format!("Hi, {}!", args[0])))
        }));
        let result = Template::new("t")
            .funcs(fm)
            .parse(r#"{{greet "World"}}"#)
            .unwrap()
            .execute_to_string(&tmap!{})
            .unwrap();
        assert_eq!(result, "Hi, World!");
    }

    #[test]
    fn test_parse_files() {
        use std::io::Write as _;
        let dir = std::env::temp_dir().join("go_template_rs_test_parse_files");
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
    fn test_parse_files_not_found() {
        let result = Template::new("t")
            .parse_files(&["/nonexistent/file.html"]);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.to_string().contains("parse_files"));
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
        assert_eq!(is_true(&Value::Bool(true)), (true, true));
        assert_eq!(is_true(&Value::Bool(false)), (false, true));
        assert_eq!(is_true(&Value::Int(0)), (false, true));
        assert_eq!(is_true(&Value::Int(1)), (true, true));
        assert_eq!(is_true(&Value::Nil), (false, true));
    }
}
