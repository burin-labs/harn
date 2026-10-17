//! Internal planner markers and fences never reach visible assistant text.

use super::*;

#[test]
fn sanitize_visible_assistant_text_strips_internal_markers() {
    let raw = "hello\n##DONE##\nDONE\n[result of read]\nsecret\n[end of read result]\nworld";
    assert_eq!(
        sanitize_visible_assistant_text(raw, false),
        "hello\n\nworld"
    );
}

#[test]
fn sanitize_visible_assistant_text_keeps_normal_code_fences() {
    let raw = "```ts\nconst x = 1\n```";
    assert_eq!(sanitize_visible_assistant_text(raw, false), raw);
}

#[test]
fn sanitize_visible_assistant_text_drops_internal_json_fences() {
    let raw = "```json\n{\"plan\":[{\"tool_name\":\"read\"}]}\n```\n\nVisible";
    assert_eq!(sanitize_visible_assistant_text(raw, false), "Visible");
}

#[test]
fn sanitize_visible_assistant_text_drops_inline_planner_json() {
    let raw = "{\"mode\":\"ask_user\",\"direction\":\"Need one decision\",\"targets\":[\"src\"],\"tasks\":[\"Clarify scope\"],\"unknowns\":[\"Which one?\"]}\n\nVisible";
    assert_eq!(sanitize_visible_assistant_text(raw, false), "Visible");
}

#[test]
fn sanitize_visible_assistant_text_drops_partial_inline_planner_json() {
    let raw = "Visible\n{\"mode\":\"plan_then_execute\",\"direction\":\"Patch the file\"";
    assert_eq!(sanitize_visible_assistant_text(raw, true), "Visible");
}

#[test]
fn sanitize_visible_assistant_text_keeps_normal_json() {
    let raw = "{\"status\":\"ok\",\"message\":\"Visible\"}";
    assert_eq!(sanitize_visible_assistant_text(raw, false), raw);
}
