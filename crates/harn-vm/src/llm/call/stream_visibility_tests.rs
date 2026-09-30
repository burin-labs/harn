//! Which calls stream their visible text to the host.

use super::*;

fn options(pairs: &[(&str, bool)]) -> Option<crate::value::DictMap> {
    Some(
        pairs
            .iter()
            .map(|(key, value)| (crate::value::intern_key(key), VmValue::Bool(*value)))
            .collect(),
    )
}

#[test]
fn the_agent_turn_marker_requests_a_visible_stream() {
    assert!(requests_user_visible_stream(&options(&[(
        "_user_visible",
        true
    )])));
}

#[test]
fn the_direct_option_still_requests_a_visible_stream() {
    assert!(requests_user_visible_stream(&options(&[(
        "user_visible",
        true
    )])));
}

#[test]
fn a_side_call_streams_nothing_visible() {
    assert!(!requests_user_visible_stream(&None));
    assert!(!requests_user_visible_stream(&options(&[(
        "_user_visible",
        false
    )])));
}
