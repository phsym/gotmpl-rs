//! The escaping context and its combinators — port of Go's `context.go`.
//!
//! A [`Context`] is the state carried by the escaping scanner across a
//! template's literal text: the HTML/URL/JS/CSS [`State`], the attribute-value
//! [`Delim`], the [`UrlPart`], the JS [`JsCtx`]/brace depth, the [`Attr`] kind,
//! and the enclosing [`Element`]. This is a lightweight *scanner over template
//! text* (not an HTML parser over rendered output).
//!
//! This is a faithful port of Go's `context` struct plus the pure
//! context algebra (`nudge`, `join`, `joinRange`) that Go keeps in `escape.go`,
//! and `mangle` (derived-template naming). It also hosts [`attr_type`], Go's
//! `attrType` attribute-name classifier (`attr.go`), which both the transition
//! machine and the escapers need.
//!
//! Fidelity notes vs. Go:
//! - Go's `context` carries an `err *Error` and a `parse.Node`; neither is
//!   represented here. An error is encoded purely as [`State::Error`]; the
//!   specific `Err*` code and message are reconstructed by the caller.
//! - Go's `jsBraceDepth` is a `[]int` that distinguishes `nil` from an empty
//!   slice; [`Context::js_brace_depth`] is a [`Vec`], so `mangle` keys off
//!   *non-empty* (as the spec dictates) rather than *non-nil*. The two differ
//!   only for a context whose brace stack was just emptied — an internal
//!   derived-name detail with no observable effect on escaping.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write as _};

/// A high-level HTML/JS/CSS parser state. Port of Go's `state` (`context.go`),
/// following the order of `state_string.go`. Discriminant values are never used
/// (dispatch is by `match`, and escaper selection by name), so the two
/// `MetaContent*` variants can sit next to the other meta states without having
/// to preserve Go's exact `iota` numbering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum State {
    /// Parsed character data, outside any tag/comment/special body.
    #[default]
    Text,
    /// Before an attribute or the end of a tag.
    Tag,
    /// Inside an attribute name.
    AttrName,
    /// After an attribute name but before any `=`.
    AfterName,
    /// After `=` but before the value.
    BeforeValue,
    /// Inside an `<!-- HTML comment -->`.
    HtmlCmt,
    /// Inside an RCDATA element (`<textarea>` or `<title>`).
    Rcdata,
    /// Inside a text-valued HTML attribute.
    Attr,
    /// Inside a URL-valued HTML attribute.
    Url,
    /// Inside an HTML `srcset` attribute.
    Srcset,
    /// Inside an event handler or `<script>` element.
    Js,
    /// Inside a JS double-quoted string.
    JsDqStr,
    /// Inside a JS single-quoted string.
    JsSqStr,
    /// Inside a JS back-quoted template literal.
    JsTmplLit,
    /// Inside a JS regexp literal.
    JsRegexp,
    /// Inside a JS `/* block comment */`.
    JsBlockCmt,
    /// Inside a JS `// line comment`.
    JsLineCmt,
    /// Inside a JS `<!--` HTML-like open comment.
    JsHtmlOpenCmt,
    /// Inside a JS `-->` HTML-like close comment.
    JsHtmlCloseCmt,
    /// Inside a `<style>` element or `style` attribute.
    Css,
    /// Inside a CSS double-quoted string.
    CssDqStr,
    /// Inside a CSS single-quoted string.
    CssSqStr,
    /// Inside a CSS double-quoted `url("...")`.
    CssDqUrl,
    /// Inside a CSS single-quoted `url('...')`.
    CssSqUrl,
    /// Inside a CSS unquoted `url(...)`.
    CssUrl,
    /// Inside a CSS `/* block comment */`.
    CssBlockCmt,
    /// Inside a CSS `// line comment`.
    CssLineCmt,
    /// Infectious error state outside any valid HTML/CSS/JS construct.
    Error,
    /// Inside an HTML `<meta>` element `content` attribute.
    MetaContent,
    /// Inside a `url=` clause of a `<meta>` `content` attribute.
    MetaContentUrl,
    /// Unreachable code after a `{{break}}` or `{{continue}}`.
    Dead,
}

impl State {
    /// Go's `isComment`: states whose content is authored for maintainers and
    /// stripped from output.
    pub(crate) fn is_comment(self) -> bool {
        matches!(
            self,
            State::HtmlCmt
                | State::JsBlockCmt
                | State::JsLineCmt
                | State::JsHtmlOpenCmt
                | State::JsHtmlCloseCmt
                | State::CssBlockCmt
                | State::CssLineCmt
        )
    }

    /// Go's `isInTag`: states occurring solely inside an HTML tag.
    pub(crate) fn is_in_tag(self) -> bool {
        matches!(
            self,
            State::Tag | State::AttrName | State::AfterName | State::BeforeValue | State::Attr
        )
    }

    /// Go's `isInScriptLiteral`: literal states inside a `<script>` where
    /// `<!--`, `<script`, and `</script` need special escaping.
    pub(crate) fn is_in_script_literal(self) -> bool {
        matches!(
            self,
            State::JsDqStr | State::JsSqStr | State::JsTmplLit | State::JsRegexp
        )
    }
}

impl fmt::Display for State {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            State::Text => "stateText",
            State::Tag => "stateTag",
            State::AttrName => "stateAttrName",
            State::AfterName => "stateAfterName",
            State::BeforeValue => "stateBeforeValue",
            State::HtmlCmt => "stateHTMLCmt",
            State::Rcdata => "stateRCDATA",
            State::Attr => "stateAttr",
            State::Url => "stateURL",
            State::Srcset => "stateSrcset",
            State::Js => "stateJS",
            State::JsDqStr => "stateJSDqStr",
            State::JsSqStr => "stateJSSqStr",
            State::JsTmplLit => "stateJSTmplLit",
            State::JsRegexp => "stateJSRegexp",
            State::JsBlockCmt => "stateJSBlockCmt",
            State::JsLineCmt => "stateJSLineCmt",
            State::JsHtmlOpenCmt => "stateJSHTMLOpenCmt",
            State::JsHtmlCloseCmt => "stateJSHTMLCloseCmt",
            State::Css => "stateCSS",
            State::CssDqStr => "stateCSSDqStr",
            State::CssSqStr => "stateCSSSqStr",
            State::CssDqUrl => "stateCSSDqURL",
            State::CssSqUrl => "stateCSSSqURL",
            State::CssUrl => "stateCSSURL",
            State::CssBlockCmt => "stateCSSBlockCmt",
            State::CssLineCmt => "stateCSSLineCmt",
            State::Error => "stateError",
            State::MetaContent => "stateMetaContent",
            State::MetaContentUrl => "stateMetaContentURL",
            State::Dead => "stateDead",
        })
    }
}

/// The delimiter that ends the current HTML attribute. Port of Go's `delim`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Delim {
    /// Outside any attribute.
    #[default]
    None,
    /// A double quote (`"`) closes the attribute.
    DoubleQuote,
    /// A single quote (`'`) closes the attribute.
    SingleQuote,
    /// A space or `>` closes the attribute.
    SpaceOrTagEnd,
}

impl fmt::Display for Delim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Delim::None => "delimNone",
            Delim::DoubleQuote => "delimDoubleQuote",
            Delim::SingleQuote => "delimSingleQuote",
            Delim::SpaceOrTagEnd => "delimSpaceOrTagEnd",
        })
    }
}

/// A part in an RFC 3986 hierarchical URL. Port of Go's `urlPart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum UrlPart {
    /// Not in a URL, or possibly at its very start.
    #[default]
    None,
    /// In the scheme, authority, or path (before any `?`).
    PreQuery,
    /// In the query or fragment.
    QueryOrFrag,
    /// Ambiguous, due to joining contexts before and after the query
    /// separator.
    Unknown,
}

impl fmt::Display for UrlPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            UrlPart::None => "urlPartNone",
            UrlPart::PreQuery => "urlPartPreQuery",
            UrlPart::QueryOrFrag => "urlPartQueryOrFrag",
            UrlPart::Unknown => "urlPartUnknown",
        })
    }
}

/// Whether a `/` starts a regexp literal or a division operator. Port of Go's
/// `jsCtx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum JsCtx {
    /// A `/` would start a regexp literal.
    #[default]
    Regexp,
    /// A `/` would start a division operator.
    DivOp,
    /// A `/` is ambiguous due to context joining.
    Unknown,
}

impl fmt::Display for JsCtx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            JsCtx::Regexp => "jsCtxRegexp",
            JsCtx::DivOp => "jsCtxDivOp",
            JsCtx::Unknown => "jsCtxUnknown",
        })
    }
}

/// The current HTML attribute, from `stateAttrName` until the tag/text ends.
/// Port of Go's `attr`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Attr {
    /// A normal attribute, or no attribute.
    #[default]
    None,
    /// An event-handler attribute (`onclick`, ...).
    Script,
    /// The `type` attribute of a `<script>` element.
    ScriptType,
    /// The `style` attribute (CSS-valued).
    Style,
    /// A URL-valued attribute.
    Url,
    /// A `srcset` attribute.
    Srcset,
    /// The `content` attribute of a `<meta>` element.
    MetaContent,
}

impl fmt::Display for Attr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Attr::None => "attrNone",
            Attr::Script => "attrScript",
            Attr::ScriptType => "attrScriptType",
            Attr::Style => "attrStyle",
            Attr::Url => "attrURL",
            Attr::Srcset => "attrSrcset",
            Attr::MetaContent => "attrMetaContent",
        })
    }
}

/// The enclosing HTML element when inside a start tag or special body. Port of
/// Go's `element`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Element {
    /// Outside a special tag or special element body.
    #[default]
    None,
    /// The raw-text `<script>` element.
    Script,
    /// The raw-text `<style>` element.
    Style,
    /// The RCDATA `<textarea>` element.
    Textarea,
    /// The RCDATA `<title>` element.
    Title,
    /// The `<meta>` element.
    Meta,
}

impl fmt::Display for Element {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Element::None => "elementNone",
            Element::Script => "elementScript",
            Element::Style => "elementStyle",
            Element::Textarea => "elementTextarea",
            Element::Title => "elementTitle",
            Element::Meta => "elementMeta",
        })
    }
}

/// The state the HTML parser must be in when it reaches the template text
/// produced by evaluating a particular node. Port of Go's `context` struct.
///
/// The [`Default`] (all-zero) value is the start context of a template that
/// produces an HTML fragment — equivalently [`Context::text`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Context {
    pub(crate) state: State,
    pub(crate) delim: Delim,
    pub(crate) url_part: UrlPart,
    pub(crate) js_ctx: JsCtx,
    /// For each open JS template-literal interpolation, the current depth of
    /// `{` braces seen. Used to decide whether the next `}` closes the
    /// interpolation. Go's `jsBraceDepth []int`.
    pub(crate) js_brace_depth: Vec<i32>,
    pub(crate) attr: Attr,
    pub(crate) element: Element,
}

/// Render a `&[i32]` the way Go's `fmt "%v"` formats a `[]int`, e.g. `[2 0]`.
pub(crate) fn write_int_slice(out: &mut String, v: &[i32]) {
    out.push('[');
    for (i, d) in v.iter().enumerate() {
        if i != 0 {
            out.push(' ');
        }
        let _ = write!(out, "{d}");
    }
    out.push(']');
}

impl Context {
    /// The start context (Go's zero `context{}`, i.e. `stateText`).
    pub(crate) fn text() -> Context {
        Context::default()
    }

    /// A fresh context in the given state with every other field at its zero
    /// value — Go's `context{state: s}` struct literal.
    pub(crate) fn in_state(state: State) -> Context {
        Context {
            state,
            ..Context::default()
        }
    }

    /// The error context — Go's `context{state: stateError, err: ...}`. The
    /// error detail is not carried; only the [`State::Error`] marker is.
    pub(crate) fn error() -> Context {
        Context::in_state(State::Error)
    }

    /// Produce an identifier that includes a suffix distinguishing it from
    /// template names mangled with different contexts. Exact port of Go's
    /// `context.mangle`.
    pub(crate) fn mangle(&self, template_name: &str) -> String {
        // The mangled name for the default context is the input name.
        if self.state == State::Text {
            return String::from(template_name);
        }
        let mut s = String::from(template_name);
        // write! into a String is infallible; the Result is intentionally
        // discarded throughout.
        let _ = write!(s, "$htmltemplate_{}", self.state);
        if self.delim != Delim::None {
            let _ = write!(s, "_{}", self.delim);
        }
        if self.url_part != UrlPart::None {
            let _ = write!(s, "_{}", self.url_part);
        }
        if self.js_ctx != JsCtx::Regexp {
            let _ = write!(s, "_{}", self.js_ctx);
        }
        if !self.js_brace_depth.is_empty() {
            s.push_str("_jsBraceDepth(");
            write_int_slice(&mut s, &self.js_brace_depth);
            s.push(')');
        }
        if self.attr != Attr::None {
            let _ = write!(s, "_{}", self.attr);
        }
        if self.element != Element::None {
            let _ = write!(s, "_{}", self.element);
        }
        s
    }

    /// The context that results from following empty-string transitions from
    /// this one. Exact port of Go's `nudge` (`escape.go`).
    ///
    /// For example `<a href=` ends in `{BeforeValue, attrURL}`, but the action
    /// in `<a href={{.}}` behaves as the first value character would: nudging
    /// yields `{Url, delimSpaceOrTagEnd}`.
    pub(crate) fn nudge(mut self) -> Context {
        match self.state {
            // In `<foo {{.}}`, the action should emit an attribute.
            State::Tag => self.state = State::AttrName,
            // In `<foo bar={{.}}`, the action is an undelimited value.
            State::BeforeValue => {
                self.state = attr_start_state(self.attr);
                self.delim = Delim::SpaceOrTagEnd;
                self.attr = Attr::None;
            }
            // In `<foo bar {{.}}`, the action is an attribute name.
            State::AfterName => {
                self.state = State::AttrName;
                self.attr = Attr::None;
            }
            _ => {}
        }
        self
    }

    /// Join the two output contexts of a branch template node (`if`/`range`/
    /// `with`). Exact port of Go's `join`.
    ///
    /// Returns [`None`] for the un-joinable case — every case where Go yields a
    /// `stateError` context, including an error on either input. Otherwise the
    /// returned context is never [`State::Error`].
    pub(crate) fn join(a: Context, b: Context) -> Option<Context> {
        if a.state == State::Error {
            return None;
        }
        if b.state == State::Error {
            return None;
        }
        if a.state == State::Dead {
            return Some(b);
        }
        if b.state == State::Dead {
            return Some(a);
        }
        if a == b {
            return Some(a);
        }

        let mut c = a.clone();
        c.url_part = b.url_part;
        if c == b {
            // The contexts differ only by urlPart.
            c.url_part = UrlPart::Unknown;
            return Some(c);
        }

        let mut c = a.clone();
        c.js_ctx = b.js_ctx;
        if c == b {
            // The contexts differ only by jsCtx.
            c.js_ctx = JsCtx::Unknown;
            return Some(c);
        }

        // Allow a nudged context to join with an unnudged one, so that e.g.
        //   <p title={{if .C}}{{.}}{{end}}
        // ends in an unquoted value state even though the else branch ends in
        // stateBeforeValue.
        let na = a.clone().nudge();
        let nb = b.clone().nudge();
        if !(na == a && nb == b)
            && let Some(e) = Context::join(na, nb)
        {
            return Some(e);
        }

        None
    }

    /// Merge break/continue contexts into a range-loop body context. Port of
    /// Go's `joinRange` (minus the error line/description annotation, which
    /// depends on the parse node the [`Context`] no longer carries).
    ///
    /// Returns [`None`] if any join fails.
    pub(crate) fn join_range(
        mut c0: Context,
        breaks: &[Context],
        continues: &[Context],
    ) -> Option<Context> {
        for c in breaks {
            c0 = Context::join(c0, c.clone())?;
        }
        for c in continues {
            c0 = Context::join(c0, c.clone())?;
        }
        Some(c0)
    }
}

/// Go's `attrStartStates`: the state entered at the start of an attribute value
/// of the given [`Attr`] kind. Shared by [`Context::nudge`] and the transition
/// machine's `tBeforeValue`.
pub(crate) fn attr_start_state(a: Attr) -> State {
    match a {
        Attr::None => State::Attr,
        Attr::Script => State::Js,
        Attr::ScriptType => State::Attr,
        Attr::Style => State::Css,
        Attr::Url => State::Url,
        Attr::Srcset => State::Srcset,
        Attr::MetaContent => State::MetaContent,
    }
}

/// A conservative upper bound on the authority of an attribute's value,
/// covering the results Go's `attrType` can return. Port of the relevant subset
/// of Go's `contentType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttrContentType {
    /// Plain text with no special interpretation.
    Plain,
    /// A CSS value.
    Css,
    /// An HTML fragment.
    Html,
    /// JavaScript source.
    Js,
    /// A URL.
    Url,
    /// A `srcset` value.
    Srcset,
    /// A value that affects encoding, credentials, or interpretation of other
    /// content and so must not be interpolated freely.
    Unsafe,
}

/// A conservative (upper-bound on authority) guess at the type of the
/// lowercase-named attribute. Exact port of Go's `attrType`.
///
/// The `name` is expected to already be lowercased, matching Go's contract.
pub(crate) fn attr_type(name: &str) -> AttrContentType {
    // Strip a "data-" prefix so the custom-attribute heuristics apply widely
    // (this also treats data-action as a URL, below). Otherwise, split on the
    // first ':' and either resolve xmlns: to a URL or strip the namespace
    // (so svg:href and xlink:href are treated as href).
    let name = if let Some(stripped) = name.strip_prefix("data-") {
        stripped
    } else if let Some(colon) = name.find(':') {
        if &name[..colon] == "xmlns" {
            return AttrContentType::Url;
        }
        &name[colon + 1..]
    } else {
        name
    };

    if let Some(t) = attr_type_map(name) {
        return t;
    }
    // Partial event-handler names are script.
    if name.starts_with("on") {
        return AttrContentType::Js;
    }
    // Heuristics to prevent "javascript:..." injection in custom data
    // attributes and custom attributes like g:tweetUrl.
    if name.contains("src") || name.contains("uri") || name.contains("url") {
        return AttrContentType::Url;
    }
    AttrContentType::Plain
}

/// Go's `attrTypeMap`: the value classification of the given attribute name, or
/// [`None`] if the name is not a known HTML/HTML4 attribute.
fn attr_type_map(name: &str) -> Option<AttrContentType> {
    use AttrContentType::{Css, Html, Plain, Srcset, Unsafe, Url};
    Some(match name {
        "accept" => Plain,
        "accept-charset" => Unsafe,
        "action" => Url,
        "alt" => Plain,
        "archive" => Url,
        "async" => Unsafe,
        "autocomplete" => Plain,
        "autofocus" => Plain,
        "autoplay" => Plain,
        "background" => Url,
        "border" => Plain,
        "checked" => Plain,
        "cite" => Url,
        "challenge" => Unsafe,
        "charset" => Unsafe,
        "class" => Plain,
        "classid" => Url,
        "codebase" => Url,
        "cols" => Plain,
        "colspan" => Plain,
        "content" => Unsafe,
        "contenteditable" => Plain,
        "contextmenu" => Plain,
        "controls" => Plain,
        "coords" => Plain,
        "crossorigin" => Unsafe,
        "data" => Url,
        "datetime" => Plain,
        "default" => Plain,
        "defer" => Unsafe,
        "dir" => Plain,
        "dirname" => Plain,
        "disabled" => Plain,
        "draggable" => Plain,
        "dropzone" => Plain,
        "enctype" => Unsafe,
        "for" => Plain,
        "form" => Unsafe,
        "formaction" => Url,
        "formenctype" => Unsafe,
        "formmethod" => Unsafe,
        "formnovalidate" => Unsafe,
        "formtarget" => Plain,
        "headers" => Plain,
        "height" => Plain,
        "hidden" => Plain,
        "high" => Plain,
        "href" => Url,
        "hreflang" => Plain,
        "http-equiv" => Unsafe,
        "icon" => Url,
        "id" => Plain,
        "ismap" => Plain,
        "keytype" => Unsafe,
        "kind" => Plain,
        "label" => Plain,
        "lang" => Plain,
        "language" => Unsafe,
        "list" => Plain,
        "longdesc" => Url,
        "loop" => Plain,
        "low" => Plain,
        "manifest" => Url,
        "max" => Plain,
        "maxlength" => Plain,
        "media" => Plain,
        "mediagroup" => Plain,
        "method" => Unsafe,
        "min" => Plain,
        "multiple" => Plain,
        "name" => Plain,
        "novalidate" => Unsafe,
        "open" => Plain,
        "optimum" => Plain,
        "pattern" => Unsafe,
        "placeholder" => Plain,
        "poster" => Url,
        "profile" => Url,
        "preload" => Plain,
        "pubdate" => Plain,
        "radiogroup" => Plain,
        "readonly" => Plain,
        "rel" => Unsafe,
        "required" => Plain,
        "reversed" => Plain,
        "rows" => Plain,
        "rowspan" => Plain,
        "sandbox" => Unsafe,
        "spellcheck" => Plain,
        "scope" => Plain,
        "scoped" => Plain,
        "seamless" => Plain,
        "selected" => Plain,
        "shape" => Plain,
        "size" => Plain,
        "sizes" => Plain,
        "span" => Plain,
        "src" => Url,
        "srcdoc" => Html,
        "srclang" => Plain,
        "srcset" => Srcset,
        "start" => Plain,
        "step" => Plain,
        "style" => Css,
        "tabindex" => Plain,
        "target" => Plain,
        "title" => Plain,
        "type" => Unsafe,
        "usemap" => Url,
        "value" => Unsafe,
        "width" => Plain,
        "wrap" => Plain,
        "xmlns" => Url,
        _ => return None,
    })
}
