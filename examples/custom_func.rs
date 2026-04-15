use go_template::{Result, Template, Value};

fn title(s: &[Value]) -> Result<Value> {
    let Some(Value::String(s)) = s.into_iter().next() else {
        //TODO: Don't panic, return a TemplateError instead (may have to create a new error variant for this)
        panic!("title function expects a single string argument");
    };
    // Simple implementation of title case: uppercase the first character, leave the rest unchanged.
    let s = s
        .chars()
        .enumerate()
        .fold(String::with_capacity(s.len()), |mut acc, (i, c)| {
            if i == 0 {
                acc.extend(c.to_uppercase());
            } else {
                acc.push(c);
            };
            acc
        });
    Ok(Value::String(s))
}

fn main() {
    let tmpl = Template::new("")
        .func("title", title)
        .parse("Hello {{ . | title }}")
        .unwrap();

    let res = tmpl
        .execute_to_string(&Value::String("world".into()))
        .unwrap();
    println!("result: {}", res);
}
