//! End-to-end tests for the `serde` feature: driving templates with data built
//! from `#[derive(Serialize)]` types through the public `to_value` function and
//! the `ToSerdeValue` extension trait.
//!
//! These complement the unit tests in `src/ser.rs` (which assert the exact
//! `Value` tree) by exercising the whole path: serialize → `Template::execute`.
//!
//! The whole file is gated on the `serde` feature, so it is empty otherwise.
#![cfg(feature = "serde")]

use std::collections::HashMap;

use gotmpl::{Template, ToSerdeValue, to_value};
use serde::Serialize;

fn render(src: &str, data: &gotmpl::Value) -> String {
    Template::new("t")
        .parse(src)
        .unwrap()
        .execute_to_string(data)
        .unwrap()
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct Address {
    city: String,
    zip: String,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct User {
    name: String,
    age: u32,
    active: bool,
    roles: Vec<String>,
    address: Address,
    nickname: Option<String>,
}

fn sample_user() -> User {
    User {
        name: "Alice".into(),
        age: 30,
        active: true,
        roles: vec!["admin".into(), "user".into()],
        address: Address {
            city: "Paris".into(),
            zip: "75001".into(),
        },
        nickname: None,
    }
}

#[test]
fn field_access_and_nested_struct() {
    let data = to_value(&sample_user()).unwrap();
    assert_eq!(
        render("{{.Name}} lives in {{.Address.City}} ({{.Address.Zip}})", &data),
        "Alice lives in Paris (75001)"
    );
}

#[test]
fn method_form_matches_function_form() {
    let user = sample_user();
    let src = "{{.Name}}/{{.Age}}";
    let via_fn = render(src, &to_value(&user).unwrap());
    let via_method = render(src, &user.to_serde_value().unwrap());
    assert_eq!(via_fn, "Alice/30");
    assert_eq!(via_fn, via_method);
}

#[test]
fn if_truthiness_bool_and_option() {
    let data = to_value(&sample_user()).unwrap();
    // active == true, nickname == None (nil, falsy).
    assert_eq!(
        render("{{if .Active}}on{{else}}off{{end}}", &data),
        "on"
    );
    assert_eq!(
        render("{{if .Nickname}}{{.Nickname}}{{else}}(none){{end}}", &data),
        "(none)"
    );
}

#[test]
fn range_over_serialized_vec() {
    let data = to_value(&sample_user()).unwrap();
    assert_eq!(
        render("{{range .Roles}}[{{.}}]{{end}}", &data),
        "[admin][user]"
    );
}

#[test]
fn range_over_serialized_map_is_key_sorted() {
    // Serde serializes a HashMap; our Value::Map is BTreeMap-backed, so
    // `{{range $k, $v := .}}` iterates in sorted (lexical) key order, which
    // matches Go for string keys.
    let mut m: HashMap<String, i32> = HashMap::new();
    m.insert("beta".into(), 2);
    m.insert("alpha".into(), 1);
    m.insert("gamma".into(), 3);
    let data = to_value(&m).unwrap();
    assert_eq!(
        render("{{range $k, $v := .}}{{$k}}={{$v}} {{end}}", &data),
        "alpha=1 beta=2 gamma=3 "
    );
}

#[test]
fn range_over_numeric_key_map_is_lexically_sorted() {
    // Value::Map is string-keyed, so numeric keys are stringified and then
    // iterated in LEXICAL order (1, 10, 2), not numeric order. This diverges
    // from Go, which sorts a map[int]V numerically (1, 2, 10). Pinned here so
    // the behavior is a documented choice, not a silent surprise.
    let mut m: HashMap<u32, i32> = HashMap::new();
    m.insert(1, 1);
    m.insert(2, 2);
    m.insert(10, 10);
    let data = to_value(&m).unwrap();
    assert_eq!(
        render("{{range $k, $v := .}}{{$k}} {{end}}", &data),
        "1 10 2 "
    );
}

#[test]
fn index_into_serialized_map_and_vec() {
    let data = to_value(&sample_user()).unwrap();
    assert_eq!(render(r#"{{index .Roles 1}}"#, &data), "user");
    assert_eq!(render(r#"{{index .Address "City"}}"#, &data), "Paris");
}

#[test]
fn externally_tagged_enum_rendering() {
    // `rename_all` renames the variant names; `rename_all_fields` renames the
    // fields *inside* struct variants. Both are needed to match `{{.Circle.Radius}}`.
    #[derive(Serialize)]
    #[serde(rename_all = "PascalCase", rename_all_fields = "PascalCase")]
    enum Shape {
        Circle { radius: f64 },
        Named(String),
    }

    let circle = to_value(&Shape::Circle { radius: 2.5 }).unwrap();
    assert_eq!(render("{{.Circle.Radius}}", &circle), "2.5");

    let named = to_value(&Shape::Named("sq".into())).unwrap();
    assert_eq!(render("{{.Named}}", &named), "sq");
}

#[test]
fn top_level_scalar_as_dot() {
    let data = to_value(&42u64).unwrap();
    assert_eq!(render("value: {{.}}", &data), "value: 42");
}

#[test]
fn top_level_vec_of_structs() {
    let users = vec![
        User {
            name: "A".into(),
            age: 1,
            active: true,
            roles: vec![],
            address: Address {
                city: "X".into(),
                zip: "1".into(),
            },
            nickname: None,
        },
        User {
            name: "B".into(),
            age: 2,
            active: false,
            roles: vec![],
            address: Address {
                city: "Y".into(),
                zip: "2".into(),
            },
            nickname: Some("Bee".into()),
        },
    ];
    let data = to_value(&users).unwrap();
    assert_eq!(
        render("{{range .}}{{.Name}}:{{.Age}} {{end}}", &data),
        "A:1 B:2 "
    );
}

#[test]
fn missing_field_follows_map_semantics() {
    // serde produces a `Value::Map`, so field access uses *map* semantics, not
    // struct semantics: a missing key yields the zero value (`<no value>`) by
    // default (Go's `missingkey=default`) rather than erroring.
    let data = to_value(&sample_user()).unwrap();
    assert_eq!(render("{{.DoesNotExist}}", &data), "<no value>");
}

#[test]
fn missing_field_errors_with_missing_key_error() {
    use gotmpl::MissingKey;
    let data = to_value(&sample_user()).unwrap();
    let err = Template::new("t")
        .missing_key(MissingKey::Error)
        .parse("{{.DoesNotExist}}")
        .unwrap()
        .execute_to_string(&data)
        .unwrap_err();
    assert!(
        err.to_string().contains("DoesNotExist"),
        "unexpected error: {err}"
    );
}

#[test]
fn serialization_error_surfaces_from_to_value() {
    // A map with a non-stringifiable (struct) key cannot become a Value::Map.
    #[derive(Serialize, PartialEq, Eq, PartialOrd, Ord)]
    struct Key {
        a: i32,
    }
    let mut m: std::collections::BTreeMap<Key, i32> = std::collections::BTreeMap::new();
    m.insert(Key { a: 1 }, 9);
    assert!(to_value(&m).is_err());
}
