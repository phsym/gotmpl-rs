//! Trusted-content wrappers — analogs of Go's `template.HTML`, `template.JS`,
//! etc. A value wrapped in one of these becomes a [`Value::Safe`] and bypasses
//! escaping in its matching context (see [`crate::SafeKind`]).
//!
//! Construct from a string and drop into template data:
//!
//! ```
//! use gotmpl::html::HTML;
//! use gotmpl::{SafeKind, ToValue, Value};
//!
//! let v = HTML::from("<b>hi</b>").to_value();
//! assert!(matches!(v, Value::Safe { kind: SafeKind::Html, .. }));
//! ```

use alloc::string::String;
use alloc::sync::Arc;

use crate::value::{SafeKind, ToValue, Value};

macro_rules! safe_content {
    ($($(#[$m:meta])* $name:ident => $kind:ident),+ $(,)?) => {$(
        $(#[$m])*
        #[derive(Debug, Clone, PartialEq, Eq)]
        pub struct $name(pub Arc<str>);

        impl From<&str> for $name {
            fn from(s: &str) -> Self { $name(Arc::from(s)) }
        }
        impl From<String> for $name {
            fn from(s: String) -> Self { $name(Arc::from(s)) }
        }
        impl From<Arc<str>> for $name {
            fn from(s: Arc<str>) -> Self { $name(s) }
        }
        impl ToValue for $name {
            fn to_value(&self) -> Value {
                Value::Safe { kind: SafeKind::$kind, s: Arc::clone(&self.0) }
            }
        }
        impl From<$name> for Value {
            fn from(v: $name) -> Value {
                Value::Safe { kind: SafeKind::$kind, s: v.0 }
            }
        }
    )+};
}

safe_content! {
    /// Content that is known-safe HTML markup (Go's `template.HTML`).
    HTML => Html,
    /// A known-safe HTML attribute name/value fragment (Go's `template.HTMLAttr`).
    HTMLAttr => HtmlAttr,
    /// Known-safe JavaScript source (Go's `template.JS`).
    JS => Js,
    /// A known-safe JavaScript string-literal body (Go's `template.JSStr`).
    JSStr => JsStr,
    /// Known-safe CSS source (Go's `template.CSS`).
    CSS => Css,
    /// A known-safe URL (Go's `template.URL`).
    URL => Url,
    /// A known-safe `<img srcset>` value (Go's `template.Srcset`).
    Srcset => Srcset,
}
