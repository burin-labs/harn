use super::*;

#[test]
fn retry_after_from_runtime_error() {
    let err = VmError::Runtime("rate limited, retry-after: 5".to_string());
    assert_eq!(extract_retry_after_ms(&err), Some(5000));
}

#[test]
fn retry_after_from_thrown_string() {
    let err = VmError::Thrown(VmValue::String(Rc::from(
        "HTTP 429 Retry-After: 2.5 seconds",
    )));
    assert_eq!(extract_retry_after_ms(&err), Some(2500));
}

#[test]
fn retry_after_case_insensitive() {
    let err = VmError::Runtime("RETRY-AFTER: 10".to_string());
    assert_eq!(extract_retry_after_ms(&err), Some(10000));
}

#[test]
fn retry_after_missing() {
    let err = VmError::Runtime("rate limited".to_string());
    assert_eq!(extract_retry_after_ms(&err), None);
}

#[test]
fn retry_after_non_numeric() {
    let err = VmError::Runtime("retry-after: tomorrow".to_string());
    assert_eq!(extract_retry_after_ms(&err), None);
}

#[test]
fn retry_after_at_end_of_message() {
    let err = VmError::Runtime("retry-after: 3".to_string());
    assert_eq!(extract_retry_after_ms(&err), Some(3000));
}

#[test]
fn retry_after_fractional_seconds() {
    let err = VmError::Runtime("retry-after: 0.5".to_string());
    assert_eq!(extract_retry_after_ms(&err), Some(500));
}

#[test]
fn retry_after_non_string_error() {
    let err = VmError::Thrown(VmValue::Int(42));
    assert_eq!(extract_retry_after_ms(&err), None);
}

#[test]
fn retry_after_with_extra_whitespace() {
    let err = VmError::Runtime("retry-after:   7  ".to_string());
    assert_eq!(extract_retry_after_ms(&err), Some(7000));
}
