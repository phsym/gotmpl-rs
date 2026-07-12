//! The `_html_template_*` escaper functions and the html funcmap builder.
//!
//! Byte-exact ports of the escaper closures registered in Go's `escape.go`
//! `funcMap`: each stringifies its arguments (via [`stringify`]), inspects the
//! resulting content type, and applies the matching transform from
//! [`super::primitives`]. The registered names and the [`merge`] entry point are
//! consumed by the escaping pass in `escape.rs`.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;

use crate::error::Result;
use crate::value::{Value, ValueFunc};

use super::primitives::{
    ContentType, FILTER_FAILSAFE, HtmlTable, JsTable, css_escaper, css_value_filter,
    html_name_filter, html_replacer, is_safe_url, js_replace, js_val_escaper,
    srcset_filter, stringify, strip_tags, url_processor,
};

/// A bare escaper function, before it is boxed into a [`ValueFunc`].
type EscaperFn = fn(&[Value]) -> Result<Value>;

/// Every escaper registered for html templates, keyed by the exact name the
/// escaping pass emits: Go's 16 `_html_template_*` escapers plus the
/// `_eval_args_` fallback (`escape.go`'s `funcMap`). The single source of truth
/// for [`register`].
pub(crate) const ESCAPERS: [(&str, EscaperFn); 17] = [
    ("_html_template_attrescaper", attr_escaper),
    ("_html_template_commentescaper", comment_escaper),
    ("_html_template_cssescaper", css_escaper_fn),
    ("_html_template_cssvaluefilter", css_value_filter_fn),
    ("_html_template_htmlnamefilter", html_name_filter_escaper),
    ("_html_template_htmlescaper", html_escaper),
    ("_html_template_jsregexpescaper", js_regexp_escaper),
    ("_html_template_jsstrescaper", js_str_escaper),
    ("_html_template_jstmpllitescaper", js_tmpl_lit_escaper),
    ("_html_template_jsvalescaper", js_val_escaper_fn),
    ("_html_template_nospaceescaper", nospace_escaper),
    ("_html_template_rcdataescaper", rcdata_escaper),
    ("_html_template_srcsetescaper", srcset_escaper),
    ("_html_template_urlescaper", url_escaper),
    ("_html_template_urlfilter", url_filter),
    ("_html_template_urlnormalizer", url_normalizer),
    ("_eval_args_", eval_args),
];

/// Wrap a `&str` result in a [`Value::String`] for return to the pipeline.
fn str_value(s: &str) -> Value {
    Value::String(Arc::from(s))
}

// ---------------------------------------------------------------------------
// html.go escapers.
// ---------------------------------------------------------------------------

/// `htmlEscaper`: escape for inclusion in HTML text (HTML-typed content passes
/// through verbatim).
fn html_escaper(args: &[Value]) -> Result<Value> {
    let (s, t) = stringify(args);
    if t == ContentType::Html {
        return Ok(str_value(s.as_ref()));
    }
    Ok(str_value(html_replacer(&s, HtmlTable::Html, true).as_ref()))
}

/// `attrEscaper`: escape for inclusion in quoted attribute values (HTML-typed
/// content is tag-stripped then normalized).
fn attr_escaper(args: &[Value]) -> Result<Value> {
    let (s, t) = stringify(args);
    if t == ContentType::Html {
        let stripped = strip_tags(&s);
        return Ok(str_value(
            html_replacer(&stripped, HtmlTable::HtmlNorm, true).as_ref(),
        ));
    }
    Ok(str_value(html_replacer(&s, HtmlTable::Html, true).as_ref()))
}

/// `rcdataEscaper`: escape for inclusion in an RCDATA element body (HTML-typed
/// content is normalized but *not* tag-stripped).
fn rcdata_escaper(args: &[Value]) -> Result<Value> {
    let (s, t) = stringify(args);
    let table = if t == ContentType::Html {
        HtmlTable::HtmlNorm
    } else {
        HtmlTable::Html
    };
    Ok(str_value(html_replacer(&s, table, true).as_ref()))
}

/// `htmlNospaceEscaper`: escape for inclusion in unquoted attribute values.
fn nospace_escaper(args: &[Value]) -> Result<Value> {
    let (s, t) = stringify(args);
    if s.is_empty() {
        return Ok(str_value(FILTER_FAILSAFE));
    }
    if t == ContentType::Html {
        let stripped = strip_tags(&s);
        return Ok(str_value(
            html_replacer(&stripped, HtmlTable::NospaceNorm, false).as_ref(),
        ));
    }
    Ok(str_value(
        html_replacer(&s, HtmlTable::Nospace, false).as_ref(),
    ))
}

/// `commentEscaper`: drop content interpolated into comments.
fn comment_escaper(_args: &[Value]) -> Result<Value> {
    Ok(str_value(""))
}

/// `htmlNameFilter`: accept only valid HTML attribute/tag name parts.
fn html_name_filter_escaper(args: &[Value]) -> Result<Value> {
    let (s, t) = stringify(args);
    Ok(str_value(html_name_filter(&s, t).as_ref()))
}

// ---------------------------------------------------------------------------
// css.go escapers.
// ---------------------------------------------------------------------------

/// `cssEscaper`: escape HTML and CSS specials with `\<hex>+` escapes.
fn css_escaper_fn(args: &[Value]) -> Result<Value> {
    let (s, _) = stringify(args);
    Ok(str_value(css_escaper(&s).as_ref()))
}

/// `cssValueFilter`: allow innocuous CSS values, defang unsafe ones.
fn css_value_filter_fn(args: &[Value]) -> Result<Value> {
    let (s, t) = stringify(args);
    Ok(str_value(css_value_filter(&s, t).as_ref()))
}

// ---------------------------------------------------------------------------
// js.go escapers.
// ---------------------------------------------------------------------------

/// `jsValEscaper`: escape to a side-effect-free JS Expression.
fn js_val_escaper_fn(args: &[Value]) -> Result<Value> {
    Ok(str_value(&js_val_escaper(args)))
}

/// `jsStrEscaper`: escape for inclusion between quotes in JS (JSStr-typed
/// content uses the norm table so existing escapes are not doubled).
fn js_str_escaper(args: &[Value]) -> Result<Value> {
    let (s, t) = stringify(args);
    let table = if t == ContentType::JsStr {
        JsTable::StrNorm
    } else {
        JsTable::Str
    };
    Ok(str_value(js_replace(&s, table).as_ref()))
}

/// `jsTmplLitEscaper`: escape for inclusion in a JS template literal.
fn js_tmpl_lit_escaper(args: &[Value]) -> Result<Value> {
    let (s, _) = stringify(args);
    Ok(str_value(js_replace(&s, JsTable::BqStr).as_ref()))
}

/// `jsRegexpEscaper`: escape for literal inclusion in a JS regexp (empty input
/// becomes the empty-but-valid `(?:)`).
fn js_regexp_escaper(args: &[Value]) -> Result<Value> {
    let (s, _) = stringify(args);
    let out = js_replace(&s, JsTable::Regexp);
    if out.is_empty() {
        return Ok(str_value("(?:)"));
    }
    Ok(str_value(out.as_ref()))
}

// ---------------------------------------------------------------------------
// url.go escapers.
// ---------------------------------------------------------------------------

/// `urlEscaper`: produce output embeddable in a URL query.
fn url_escaper(args: &[Value]) -> Result<Value> {
    let (s, t) = stringify(args);
    Ok(str_value(url_processor(false, &s, t).as_ref()))
}

/// `urlNormalizer`: normalize URL content for a quote/paren-delimited context.
fn url_normalizer(args: &[Value]) -> Result<Value> {
    let (s, t) = stringify(args);
    Ok(str_value(url_processor(true, &s, t).as_ref()))
}

/// `urlFilter`: defang URLs whose scheme is not http/https/mailto.
fn url_filter(args: &[Value]) -> Result<Value> {
    let (s, t) = stringify(args);
    if t == ContentType::Url {
        return Ok(str_value(s.as_ref()));
    }
    if !is_safe_url(&s) {
        return Ok(str_value("#ZgotmplZ"));
    }
    Ok(str_value(s.as_ref()))
}

/// `srcsetFilterAndEscaper`: filter and normalize a `srcset` value.
fn srcset_escaper(args: &[Value]) -> Result<Value> {
    let (s, t) = stringify(args);
    Ok(str_value(srcset_filter(&s, t).as_ref()))
}

// ---------------------------------------------------------------------------
// escape.go: evalArgs.
// ---------------------------------------------------------------------------

/// `evalArgs`: `fmt.Sprint` of the args, returning a lone string argument
/// unchanged.
fn eval_args(args: &[Value]) -> Result<Value> {
    if args.len() == 1
        && let Value::String(s) = &args[0]
    {
        return Ok(Value::String(Arc::clone(s)));
    }
    Ok(str_value(&crate::go::sprint(args)))
}

// ---------------------------------------------------------------------------
// Registration.
// ---------------------------------------------------------------------------

/// Merge the html escaper functions onto a base funcmap (builtins plus any
/// user-registered functions), returning a new shared map via copy-on-write.
pub(crate) fn merge(base: &Arc<BTreeMap<String, ValueFunc>>) -> Arc<BTreeMap<String, ValueFunc>> {
    let mut funcs = base.clone();
    register(Arc::make_mut(&mut funcs));
    funcs
}

/// Register the 16 `_html_template_*` escapers and `_eval_args_` into `m`.
fn register(m: &mut BTreeMap<String, ValueFunc>) {
    for (name, f) in ESCAPERS {
        let vf: ValueFunc = Arc::new(f);
        m.insert(name.to_string(), vf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: &[Value]) -> String {
        let base: Arc<BTreeMap<String, ValueFunc>> = Arc::new(BTreeMap::new());
        let funcs = merge(&base);
        let f = funcs.get(name).expect("escaper registered");
        match f(args).expect("escaper ok") {
            Value::String(s) => s.to_string(),
            other => panic!("expected string, got {other:?}"),
        }
    }

    fn s(v: &str) -> Value {
        Value::String(Arc::from(v))
    }

    #[test]
    fn all_names_registered() {
        let base: Arc<BTreeMap<String, ValueFunc>> = Arc::new(BTreeMap::new());
        let funcs = merge(&base);
        for (name, _) in ESCAPERS {
            assert!(funcs.contains_key(name), "missing {name}");
        }
    }

    #[test]
    fn html_escaper_text_and_safe() {
        assert_eq!(
            call(
                "_html_template_htmlescaper",
                &[s("<b> \"foo%\" O'Reilly &bar;")]
            ),
            "&lt;b&gt; &#34;foo%&#34; O&#39;Reilly &amp;bar;"
        );
        // HTML-typed content passes through verbatim.
        assert_eq!(
            call(
                "_html_template_htmlescaper",
                &[Value::Safe {
                    kind: crate::SafeKind::Html,
                    s: Arc::from("Hello, <b>World</b> &amp;tc!"),
                }]
            ),
            "Hello, <b>World</b> &amp;tc!"
        );
    }

    #[test]
    fn attr_escaper_strips_tags_for_html() {
        // Ported from content_test.go `<a title='{{.}}'>` HTML row.
        assert_eq!(
            call(
                "_html_template_attrescaper",
                &[Value::Safe {
                    kind: crate::SafeKind::Html,
                    s: Arc::from("Hello, <b>World</b> &amp;tc!"),
                }]
            ),
            "Hello, World &amp;tc!"
        );
    }

    #[test]
    fn nospace_escaper_empty_is_failsafe() {
        assert_eq!(call("_html_template_nospaceescaper", &[s("")]), "ZgotmplZ");
        // Plain content: spaces and specials escaped.
        assert_eq!(
            call(
                "_html_template_nospaceescaper",
                &[s("<b> \"foo%\" O'Reilly &bar;")]
            ),
            "&lt;b&gt;&#32;&#34;foo%&#34;&#32;O&#39;Reilly&#32;&amp;bar;"
        );
    }

    #[test]
    fn comment_escaper_drops_everything() {
        assert_eq!(call("_html_template_commentescaper", &[s("anything")]), "");
    }

    #[test]
    fn js_val_escaper_via_funcmap() {
        assert_eq!(
            call("_html_template_jsvalescaper", &[Value::Int(42)]),
            " 42 "
        );
        assert_eq!(call("_html_template_jsvalescaper", &[s("foo")]), "\"foo\"");
    }

    #[test]
    fn url_filter_defangs_javascript() {
        assert_eq!(
            call("_html_template_urlfilter", &[s("javascript:alert(1)")]),
            "#ZgotmplZ"
        );
        assert_eq!(
            call("_html_template_urlfilter", &[s("http://example.com")]),
            "http://example.com"
        );
        // URL-typed content bypasses the filter.
        assert_eq!(
            call(
                "_html_template_urlfilter",
                &[Value::Safe {
                    kind: crate::SafeKind::Url,
                    s: Arc::from("javascript:ok()"),
                }]
            ),
            "javascript:ok()"
        );
    }

    #[test]
    fn url_escaper_and_normalizer() {
        assert_eq!(
            call(
                "_html_template_urlescaper",
                &[s("greeting=H%69,&addressee=(World)")]
            ),
            "greeting%3dH%2569%2c%26addressee%3d%28World%29"
        );
        assert_eq!(
            call(
                "_html_template_urlnormalizer",
                &[s("greeting=H%69,&addressee=(World)")]
            ),
            "greeting=H%69,&addressee=%28World%29"
        );
    }

    #[test]
    fn eval_args_sprint() {
        assert_eq!(call("_eval_args_", &[s("hi")]), "hi");
        assert_eq!(call("_eval_args_", &[Value::Int(1), Value::Int(2)]), "1 2");
        assert_eq!(call("_eval_args_", &[Value::Nil]), "<nil>");
    }
}
