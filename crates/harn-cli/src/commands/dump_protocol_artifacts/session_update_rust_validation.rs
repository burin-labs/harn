//! Project schema constraints as data interpreted by one Rust validator.

use serde_json::Value;

pub(super) const SUPPORT: &str = r#"
#[derive(Clone, Copy)]
enum SessionUpdateRule {
    Required,
    MinLen(u64),
    Minimum(i64),
    NonBlank,
    Choices(&'static [&'static str]),
    Present(&'static [(&'static str, SessionUpdateRule)]),
    When(&'static [&'static str], &'static [(&'static str, SessionUpdateRule)]),
    RequiredForPhase,
}

fn validate_session_update(value: &Value, rules: &[(&str, SessionUpdateRule)]) -> Result<(), String> {
    for (path, rule) in rules {
        let field = value.pointer(path);
        let reason = match rule {
            SessionUpdateRule::Required if field.is_none() => Some("is required"),
            SessionUpdateRule::MinLen(minimum) if field.and_then(Value::as_str).is_some_and(|text| (text.chars().count() as u64) < *minimum) => Some("is too short"),
            SessionUpdateRule::Minimum(minimum) if field.and_then(Value::as_i64).is_some_and(|number| number < *minimum) => Some("is below its minimum"),
            SessionUpdateRule::NonBlank if field.and_then(Value::as_str).is_some_and(|text| text.trim().is_empty()) => Some("must not be blank"),
            SessionUpdateRule::Choices(choices) if field.and_then(Value::as_str).is_some_and(|text| !choices.contains(&text)) => Some("has an unknown value"),
            SessionUpdateRule::Present(children) if field.is_some() => { validate_session_update(value, children)?; None },
            SessionUpdateRule::When(choices, children) if field.and_then(Value::as_str).is_some_and(|text| choices.contains(&text)) => { validate_session_update(value, children)?; None },
            SessionUpdateRule::RequiredForPhase if field.is_none() => Some("is required for this phase"),
            _ => None,
        };
        if let Some(reason) = reason {
            let path = path.trim_start_matches('/').replace('/', ".").replace("~1", "/").replace("~0", "~");
            return Err(format!("session update {path} {reason}"));
        }
    }
    Ok(())
}

"#;

pub(super) fn append_rules(out: &mut String, shape: &Value) {
    out.push_str("&[\n");
    append_object(out, shape, &[]);
    out.push(']');
}

fn entry(out: &mut String, path: &[String], rule: &str) {
    let pointer = format!(
        "/{}",
        path.iter()
            .map(|key| key.replace('~', "~0").replace('/', "~1"))
            .collect::<Vec<_>>()
            .join("/")
    );
    let rule = rule.replace('\n', "\n    ");
    out.push_str(&format!("    ({pointer:?}, SessionUpdateRule::{rule}),\n"));
}

fn append_object(out: &mut String, shape: &Value, path: &[String]) {
    let Some(properties) = shape["properties"].as_object() else {
        return;
    };
    for (key, field) in properties {
        let mut child = path.to_vec();
        child.push(key.clone());
        if shape["required"]
            .as_array()
            .is_some_and(|required| required.iter().any(|name| name == key))
        {
            entry(out, &child, "Required");
        }
        if let Some(minimum) = field["minLength"].as_u64() {
            entry(out, &child, &format!("MinLen({minimum})"));
        }
        if let Some(minimum) = field["minimum"].as_i64() {
            entry(out, &child, &format!("Minimum({minimum})"));
        }
        if let Some(pattern) = field["pattern"].as_str() {
            assert_eq!(pattern, "\\S", "unsupported session-update string pattern");
            entry(out, &child, "NonBlank");
        }
        if let Some(choices) = field["enum"].as_array() {
            entry(
                out,
                &child,
                &format!("Choices({})", string_choices(choices)),
            );
        }
        if field["properties"].is_object() {
            let mut children = String::new();
            append_object(&mut children, field, &child);
            if !children.is_empty() {
                entry(out, &child, &format!("Present(&[\n{children}])"));
            }
        }
    }
    if let Some(conditions) = shape["allOf"].as_array() {
        for rule in conditions {
            let condition = rule["if"]["properties"]
                .as_object()
                .expect("conditional presence properties");
            assert_eq!(
                condition.len(),
                1,
                "conditional presence has one discriminator"
            );
            let (key, condition) = condition.iter().next().unwrap();
            let mut condition_path = path.to_vec();
            condition_path.push(key.clone());
            let choices = string_choices(
                condition["enum"]
                    .as_array()
                    .expect("conditional string enum"),
            );
            let mut children = String::new();
            for field in rule["then"]["required"]
                .as_array()
                .expect("conditional required fields")
            {
                let mut required = path.to_vec();
                required.push(field.as_str().expect("field name").to_owned());
                entry(&mut children, &required, "RequiredForPhase");
            }
            if rule["then"]["properties"].is_object() {
                append_object(&mut children, &rule["then"], path);
            }
            entry(
                out,
                &condition_path,
                &format!("When({choices}, &[\n{children}])"),
            );
        }
    }
}

fn string_choices(choices: &[Value]) -> String {
    format!(
        "&[{}]",
        choices
            .iter()
            .map(|choice| format!("{:?}", choice.as_str().expect("string enum")))
            .collect::<Vec<_>>()
            .join(", ")
    )
}
