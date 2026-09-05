use crate::value::{VmError, VmValue};

pub(super) fn parse_string_list(
    value: Option<&VmValue>,
    label: &str,
) -> Result<Vec<String>, VmError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let VmValue::List(list) = value else {
        return Err(VmError::Runtime(format!(
            "{label}: expected a list of strings"
        )));
    };
    let mut values = Vec::new();
    for item in list.iter() {
        let VmValue::String(text) = item else {
            return Err(VmError::Runtime(format!(
                "{label}: expected a list of strings"
            )));
        };
        let text = text.trim();
        if !text.is_empty() && !values.iter().any(|existing| existing == text) {
            values.push(text.to_string());
        }
    }
    Ok(values)
}
