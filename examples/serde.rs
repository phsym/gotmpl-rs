//! Passing a `#[derive(Serialize)]` type to a template via the `serde` feature.
//!
//! Run with: `cargo run --example serde --features serde`

use gotmpl::{Template, ToSerdeValue, to_value};
use serde::Serialize;

// NOTE: Go templates access exported PascalCase fields (`{{.Name}}`), but serde
// derives field names from the Rust identifiers. `rename_all = "PascalCase"`
// makes the map keys match what the template dots into.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct User {
    name: String,
    age: u8,
    roles: Vec<String>,
    active: bool,
}

fn main() {
    let user = User {
        name: "Alice".to_string(),
        age: 30,
        roles: vec!["admin".to_string(), "user".to_string()],
        active: true,
    };

    let tmpl = Template::new("")
        .parse("{{.Name}} ({{.Age}}){{if .Active}} active{{end}}\nRoles:{{range .Roles}} {{.}}{{end}}")
        .unwrap();

    // Free-function form.
    let data = to_value(&user).unwrap();
    println!("{}", tmpl.execute_to_string(&data).unwrap());

    // Extension-method form, identical result (needs `ToSerdeValue` in scope).
    let data = user.to_serde_value().unwrap();
    println!("{}", tmpl.execute_to_string(&data).unwrap());
}
