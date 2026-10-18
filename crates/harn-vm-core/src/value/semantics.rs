use super::{char_range_to_byte_range, string_char_count, VmError, VmValue};

/// Bucket keys must be strings: displaying a scalar would merge distinct keys.
pub fn string_discriminator(value: &VmValue, builtin: &str) -> Result<String, VmError> {
    match value {
        VmValue::String(s) => Ok((**s).to_string()),
        VmValue::Nil => Err(VmError::TypeError(format!(
            "{builtin}: callback returned nil; expected a string discriminator (wrap with to_string(...) if you intended a scalar)"
        ))),
        other => Err(VmError::TypeError(format!(
            "{builtin}: callback must return a string discriminator, got {}; wrap with to_string(...) so the bucket key is unambiguous",
            other.type_name()
        ))),
    }
}

pub fn split_separator_required() -> VmError {
    VmError::Thrown(VmValue::string(
        "split: separator is required (use split(text, sep) or text.split(sep))",
    ))
}

/// Character offsets clamp to the string bounds. Unlike slice indices,
/// negative substring offsets clamp to zero instead of counting from the end.
#[expect(
    clippy::string_slice,
    reason = "char_range_to_byte_range returns clamped char-boundary offsets"
)]
pub fn char_substring(s: &str, start: i64, end: Option<i64>) -> String {
    let len = string_char_count(s) as i64;
    let start = start.clamp(0, len);
    let end = end.unwrap_or(len).clamp(0, len).max(start);
    let (start_byte, end_byte) = char_range_to_byte_range(s, start as usize, end as usize);
    s[start_byte..end_byte].to_string()
}
