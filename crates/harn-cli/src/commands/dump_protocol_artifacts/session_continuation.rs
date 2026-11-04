//! Project the session lifecycle's opaque continuation into host bindings.

use super::records::Target;
use super::schema_records::SchemaRecords;

pub(super) fn append(out: &mut String, target: Target) {
    let schema = harn_serve::AcpSessionCloseResult::schema();
    let names = [("", "HarnACPSessionContinuation".into())];
    let records = SchemaRecords {
        schema: &schema,
        names: &names,
        label: "ACP session continuation",
        require_all: false,
        metadata: |_, _, _| Ok(None),
    }
    .load_extensible()
    .expect("ACP session continuation projects to host records");
    for record in records {
        record.append(out, target);
    }
}
