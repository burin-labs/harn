use std::collections::BTreeSet;

use super::HARN_AGENT_EVENT_KINDS;

#[test]
fn conformance_schema_accepts_every_advertised_agent_event_kind() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/protocols/schemas/acp-session-update.schema.json");
    let source = std::fs::read_to_string(&path).expect("read ACP conformance schema");
    let schema: serde_json::Value = serde_json::from_str(&source).expect("parse ACP schema");
    let values = schema["$defs"]["HarnAgentEventNotification"]["properties"]["params"]
        ["properties"]["kind"]["enum"]
        .as_array()
        .expect("agent event kind enum");
    let schema_kinds: BTreeSet<&str> = values
        .iter()
        .map(|value| value.as_str().expect("agent event kind string"))
        .collect();
    let advertised_kinds: BTreeSet<&str> = HARN_AGENT_EVENT_KINDS.iter().copied().collect();

    assert_eq!(
        schema_kinds, advertised_kinds,
        "ACP conformance schema and advertised event kinds must change together"
    );
}
