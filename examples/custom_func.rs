use gotmpl::{Result, Template, TemplateError, Value, tmap};

fn title(s: &[Value]) -> Result<Value> {
    //TODO: Add helper for validating argument count and types, to simplify writing custom functions.
    if s.len() != 1 {
        return Err(TemplateError::ArgCount {
            name: "title".to_string(),
            expected: 1,
            got: s.len(),
        });
    }
    let Some(Value::String(s)) = s.first() else {
        return Err(TemplateError::Exec(
            "title function expects a single string argument".to_string(),
        ));
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
    Ok(Value::String(s.into()))
}

fn main() {
    let data = tmap! { "name" => "world"};

    let tmpl = Template::new("")
        .func("title", title)
        .parse("Hello {{ .name | title }}")
        .unwrap();

    let res = tmpl.execute_to_string(&data).unwrap();
    println!("result: {}", res);
}
