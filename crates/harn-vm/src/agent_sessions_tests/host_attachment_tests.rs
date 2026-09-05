use super::*;

struct TestAttachmentResolver(crate::host_attachments::MaterializedAttachment);

impl crate::host_attachments::HostAttachmentResolver for TestAttachmentResolver {
    fn resolve(
        &self,
        _artifact_pointer: &str,
        _media_type: &str,
    ) -> Result<crate::host_attachments::MaterializedAttachment, String> {
        Ok(self.0.clone())
    }
}

#[test]
fn inject_host_attachment_records_pointer_event_without_inline_bytes() {
    reset_all_sinks();
    reset_session_store();
    let id = open_or_create(Some("host-attachment-session".into()));

    let result = inject_host_event(
        &id,
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "kind": "host_attachment",
            "delivery": "immediate",
            "payload": {
                "media_type": "text/plain",
                "flavor": "text_frame",
                "artifact_pointer": ".burin/chat-assets/frame.txt",
                "sha256": "b".repeat(64),
                "size_bytes": 21,
                "description": "visible terminal frame",
                "description_model": "vision-model"
            },
            "provenance": {
                "initiator": "host_auto",
                "source": "auto_frame_capture",
                "host": "tui",
                "ts_ms": 1782000000001i64
            }
        })),
    )
    .expect("host attachment injects");

    assert_eq!(result["sequence"], 1);
    let transcript_events = events_by_kind_json(&id, "host_attachment");
    assert_eq!(transcript_events.len(), 1);
    assert_eq!(
        transcript_events[0]["metadata"]["artifact_pointer"],
        ".burin/chat-assets/frame.txt"
    );
    assert_eq!(
        transcript_events[0]["metadata"]["sanitization"]["trust"],
        "semi_trusted"
    );
    assert_eq!(
        transcript_events[0]["metadata"]["rendered"],
        "description_plus_pointer"
    );
    assert_eq!(
        transcript_events[0]["metadata"]["sanitization"]["summary_model"],
        "vision-model"
    );
    assert!(transcript_events[0]["text"]
        .as_str()
        .expect("text")
        .contains("visible terminal frame"));
    reset_all_sinks();
}

#[test]
fn host_attachment_rejects_retired_host_owned_rendering_policy() {
    reset_session_store();
    let id = open_or_create(Some("host-attachment-invalid-policy-session".into()));
    let error = inject_host_event(
        &id,
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "kind": "host_attachment",
            "payload": {
                "media_type": "image/png",
                "flavor": "image",
                "artifact_pointer": "artifact:frame",
                "sha256": "e".repeat(64),
                "size_bytes": 42,
                "rendered": "image_block"
            },
            "provenance": {
                "initiator": "user",
                "source": "user_attachment",
                "ts_ms": 1782000000006i64
            }
        })),
    )
    .unwrap_err();
    assert!(error.contains("unknown field `rendered`"), "{error}");
}

#[test]
fn host_attachment_delivery_is_model_capability_aware() {
    reset_all_sinks();
    reset_session_store();
    let id = open_or_create(Some("host-vision-attachment-session".into()));
    set_pinned_model(&id, Some("gpt-4o".into())).unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    register_sink(&id, Arc::new(CapturingSink(captured.clone())));
    let _resolver = crate::host_attachments::register_host_attachment_resolver(Arc::new(
        TestAttachmentResolver(crate::host_attachments::MaterializedAttachment::ImageUrl(
            "https://example.test/frame.png".into(),
        )),
    ));

    inject_host_event(
        &id,
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "kind": "host_attachment",
            "payload": {
                "media_type": "image/png",
                "flavor": "image",
                "artifact_pointer": "artifact:frame",
                "sha256": "c".repeat(64),
                "size_bytes": 42
            },
            "provenance": {
                "initiator": "user",
                "source": "user_attachment",
                "ts_ms": 1782000000003i64
            }
        })),
    )
    .unwrap();

    let snapshot = snapshot(&id).unwrap();
    let message = snapshot
        .as_dict()
        .and_then(|dict| dict.get("messages"))
        .and_then(|messages| match messages {
            VmValue::List(messages) => messages.first(),
            _ => None,
        })
        .expect("injected message");
    let message = crate::llm::helpers::vm_value_to_json(message);
    assert_eq!(message["content"][1]["type"], "image");
    assert_eq!(
        message["content"][1]["url"],
        "https://example.test/frame.png"
    );
    let events = captured.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::HostAttachment {
            rendered: AttachmentRendering::ImageBlock,
            ..
        }
    )));
    reset_all_sinks();
}

#[test]
fn host_attachment_resolution_failure_degrades_to_pointer_only() {
    reset_all_sinks();
    reset_session_store();
    let id = open_or_create(Some("host-pointer-attachment-session".into()));
    let captured = Arc::new(Mutex::new(Vec::new()));
    register_sink(&id, Arc::new(CapturingSink(captured.clone())));

    inject_host_event(
        &id,
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "kind": "host_attachment",
            "payload": {
                "media_type": "image/png",
                "flavor": "image",
                "artifact_pointer": "artifact:missing",
                "sha256": "d".repeat(64),
                "size_bytes": 42
            },
            "provenance": {
                "initiator": "user",
                "source": "user_attachment",
                "ts_ms": 1782000000004i64
            }
        })),
    )
    .expect("pointer-only delivery must not fail");

    let events = captured.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::HostAttachment {
            rendered: AttachmentRendering::PointerOnly,
            sanitization: SanitizationVerdict {
                action: SanitizationAction::Pointerized,
                ..
            },
            ..
        }
    )));
    reset_all_sinks();
}
