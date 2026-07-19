use super::json::*;

#[test]
fn timeout_ms_rounds_up_to_seconds() {
    assert_eq!(resolve_timeout_secs(Some(1)), Some(1));
    assert_eq!(resolve_timeout_secs(Some(100)), Some(1));
    assert_eq!(resolve_timeout_secs(Some(1000)), Some(1));
    assert_eq!(resolve_timeout_secs(Some(1001)), Some(2));
    assert_eq!(resolve_timeout_secs(Some(5000)), Some(5));
}

#[test]
fn non_positive_clamps_to_zero() {
    assert_eq!(resolve_timeout_secs(Some(0)), Some(0));
    assert_eq!(resolve_timeout_secs(Some(-1)), Some(0));
}

#[test]
fn returns_none_when_unset() {
    assert_eq!(resolve_timeout_secs(None), None);
}
