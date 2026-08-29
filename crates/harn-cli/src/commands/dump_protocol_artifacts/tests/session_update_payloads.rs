use super::super::session_update_payloads::{
    go_payload_type_name, python_payload_type_name, rust_payload_type_name,
    schema_required_identity_gaps, swift_payload_type_name, ts_payload_type_name,
    typed_payload_registry_gaps, TYPED_SESSION_UPDATE_PAYLOADS,
};
use super::*;

#[test]
fn typed_session_update_payloads_cover_every_advertised_extension() {
    let gaps = typed_payload_registry_gaps();
    assert!(
        gaps.is_empty(),
        "typed session-update payload table drifted from HARN_SESSION_UPDATE_EXTENSIONS:\n{}",
        gaps.join("\n")
    );
}

#[test]
fn typed_session_update_payloads_cover_schema_required_identity() {
    let schema = protocol_source()
        .read_text("spec/protocol-artifacts/schemas/acp-session-update.schema.json")
        .expect("session-update schema");
    let gaps = schema_required_identity_gaps(&schema).expect("schema parse");
    assert!(
        gaps.is_empty(),
        "typed session-update payload table dropped a schema-required identity field:\n{}",
        gaps.join("\n")
    );
}

#[test]
fn dump_emits_typed_session_update_payloads_in_every_language() {
    let rust = generate_rust();
    let swift = generate_swift();
    let typescript = generate_typescript();
    let python = generate_python();
    let go = generate_go();

    assert!(
        typescript.contains("export const HARN_TYPED_SESSION_UPDATE_PAYLOADS"),
        "TypeScript dump must publish the payload catalog"
    );
    assert!(
        rust.contains("pub const HARN_TYPED_SESSION_UPDATE_PAYLOADS"),
        "Rust dump must publish the payload catalog"
    );
    assert!(
        typescript.contains("  | ACPSkillActivatedUpdate\n"),
        "TypeScript union must include typed Harn payloads before the catch-all lump"
    );

    for payload in TYPED_SESSION_UPDATE_PAYLOADS {
        let ts_name = ts_payload_type_name(payload);
        let rust_name = rust_payload_type_name(payload);
        let swift_name = swift_payload_type_name(payload);
        let python_name = python_payload_type_name(payload);
        let go_name = go_payload_type_name(payload);
        assert!(
            typescript.contains(&format!("export interface {ts_name}")),
            "TypeScript missing {ts_name}"
        );
        assert!(
            typescript.contains(&format!("  | {ts_name}\n")),
            "TypeScript union missing {ts_name}"
        );
        assert!(
            rust.contains(&format!("pub struct {rust_name}")),
            "Rust missing {rust_name}"
        );
        assert!(
            swift.contains(&format!("public struct {swift_name}")),
            "Swift missing {swift_name}"
        );
        assert!(
            python.contains(&format!("class {python_name}")),
            "Python missing {python_name}"
        );
        assert!(
            go.contains(&format!("type {go_name} struct")),
            "Go missing {go_name}"
        );
        for field in payload
            .fields
            .iter()
            .filter(|field| field.required && field.identity)
        {
            assert!(
                typescript.contains(&format!("  {}: ", field.wire_name)),
                "TypeScript {ts_name} missing identity field {}",
                field.wire_name
            );
            assert!(
                rust.contains(&format!("pub {}: ", field.rust_name)),
                "Rust {rust_name} missing identity field {}",
                field.rust_name
            );
            assert!(
                swift.contains(&format!("public var {}: ", field.wire_name)),
                "Swift {swift_name} missing identity field {}",
                field.wire_name
            );
        }
        assert!(
            typescript.contains(&format!("sessionUpdate: \"{}\"", payload.discriminator)),
            "TypeScript {ts_name} must pin sessionUpdate to {}",
            payload.discriminator
        );
    }
}
