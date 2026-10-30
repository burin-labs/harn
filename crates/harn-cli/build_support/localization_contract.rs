//! A translated sentence may surround stable console record field identifiers.

use std::collections::BTreeMap;

pub fn field_identifiers(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let first = |byte: u8| byte.is_ascii_alphabetic() || byte == b'_';
    let rest = |byte: u8| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte);
    let mut identifiers = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if first(bytes[index]) && (index == 0 || !rest(bytes[index - 1])) {
            let start = index;
            index += 1;
            while index < bytes.len() && rest(bytes[index]) {
                index += 1;
            }
            if bytes.get(index) == Some(&b'=') {
                identifiers.push(
                    text.get(start..=index)
                        .expect("ASCII identifier boundaries")
                        .to_owned(),
                );
            }
        } else {
            index += 1;
        }
    }
    identifiers.sort();
    identifiers
}

pub fn validate_fields(
    source: &BTreeMap<String, String>,
    translated: &BTreeMap<String, String>,
) -> Result<(), String> {
    for (key, text) in source {
        let value = translated
            .get(key)
            .ok_or_else(|| format!("missing CLI message: {key}"))?;
        let expected = field_identifiers(text);
        let actual = field_identifiers(value);
        if expected != actual {
            return Err(format!(
                "changed CLI record field identifiers in {key}: {expected:?} != {actual:?}"
            ));
        }
    }
    Ok(())
}
