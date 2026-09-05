use super::*;

#[test]
fn video_content_requires_capability() {
    crate::llm::capabilities::set_user_overrides_toml(
        r#"
[[provider.local]]
model_match = "video-model"
video_supported = true
"#,
    )
    .expect("capability override");
    let video_block = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("type"),
            VmValue::String(arcstr::ArcStr::from("video")),
        ),
        (
            crate::value::intern_key("base64"),
            VmValue::String(arcstr::ArcStr::from("AAAA")),
        ),
        (
            crate::value::intern_key("media_type"),
            VmValue::String(arcstr::ArcStr::from("video/mp4")),
        ),
    ]));
    let message = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("role"),
            VmValue::String(arcstr::ArcStr::from("user")),
        ),
        (
            crate::value::intern_key("content"),
            VmValue::List(std::sync::Arc::new(vec![video_block])),
        ),
    ]));
    let options = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("local")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("video-model")),
        ),
        (
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(vec![message.clone()])),
        ),
    ]));
    extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("")),
        VmValue::Nil,
        options,
    ])
    .expect("video-capable route should accept video content");
    crate::llm::capabilities::clear_user_overrides();

    let bad_options = VmValue::dict(crate::value::DictMap::from_iter([
        (
            crate::value::intern_key("provider"),
            VmValue::String(arcstr::ArcStr::from("mock")),
        ),
        (
            crate::value::intern_key("model"),
            VmValue::String(arcstr::ArcStr::from("gpt-4o")),
        ),
        (
            crate::value::intern_key("messages"),
            VmValue::List(std::sync::Arc::new(vec![message])),
        ),
    ]));
    let err = extract_llm_options(&[
        VmValue::String(arcstr::ArcStr::from("")),
        VmValue::Nil,
        bad_options,
    ])
    .expect_err("non-video model should reject video content");
    assert!(err.to_string().contains("option `video` is not supported"));
}
