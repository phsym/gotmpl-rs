//! Context-aware HTML auto-escaping with `gotmpl::html::Template`.
//!
//! Run with: `cargo run --example html --features html`
//!
//! Demonstrates that values interpolated into HTML text, attributes, URLs, and
//! `<script>` bodies are each escaped for their context, while trusted content
//! wrapped in [`gotmpl::html::HTML`] passes through verbatim.

use gotmpl::html::{HTML, Template};
use gotmpl::tmap;

fn main() {
    let src = r#"<p>{{.Comment}}</p>
<a href="{{.Link}}">link</a>
<p class="{{.Cls}}">
<script>var s = "{{.Comment}}";</script>
<p>trusted: {{.Trusted}}</p>"#;

    let data = tmap! {
        "Comment" => "<script>alert('xss')</script>",
        "Link"    => "javascript:alert(document.cookie)",
        "Cls"     => "\"><img src=x onerror=alert(1)>",
        "Trusted" => HTML::from("<em>hand-written &amp; safe</em>"),
    };

    let out = Template::new("page")
        .parse(src)
        .expect("parse")
        .execute_to_string(&data)
        .expect("execute");

    println!("--- rendered (safely escaped) ---\n{out}\n");

    // The dangerous inputs are neutralized for their context...
    assert!(
        out.contains("&lt;script&gt;alert(&#39;xss&#39;)&lt;/script&gt;"),
        "HTML text context escapes the script tag"
    );
    assert!(
        out.contains(r##"href="#ZgotmplZ""##),
        "javascript: URL is filtered to #ZgotmplZ"
    );
    assert!(
        !out.contains("<img src=x onerror"),
        "attribute-breakout payload cannot start a real tag"
    );
    // ...while trusted content passes through verbatim.
    assert!(
        out.contains("<em>hand-written &amp; safe</em>"),
        "html::HTML content is emitted unescaped"
    );

    println!("All escaping assertions passed \u{2713}");
}
