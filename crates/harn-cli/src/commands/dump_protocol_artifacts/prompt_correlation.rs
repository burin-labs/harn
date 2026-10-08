//! Project caller-message correlation from the owning ACP input record.

use super::records::Target;
use super::schema_records::SchemaRecords;

pub(super) fn append(out: &mut String, target: Target) {
    let schema = harn_serve::adapters::acp::AcpPromptCorrelation::schema();
    let names = [("", "HarnACPPromptCorrelation".into())];
    let records = SchemaRecords {
        schema: &schema,
        names: &names,
        label: "ACP prompt correlation",
        require_all: false,
        metadata: |_, _, _| Ok(None),
    }
    .load_extensible()
    .expect("ACP prompt correlation projects to host records");
    for record in records {
        record.append(out, target);
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
}
