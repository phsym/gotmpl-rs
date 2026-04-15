use gotmpl::{Template, ToValue};

fn main() {
    let tmpl = Template::new("").parse("Hello, {{.}}!").unwrap();
    let output = tmpl.execute_to_string(&"world".to_value()).unwrap();
    println!("{}", output);
}
