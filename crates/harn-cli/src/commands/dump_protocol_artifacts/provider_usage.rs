//! Project the VM's live provider-usage schema into protocol bindings.

use super::records::Target;
use super::schema_records::SchemaRecords;

pub(super) fn append(out: &mut String, target: Target) {
    let schema = harn_vm::llm::usage::provider_usage_schema();
    let names = [("", "HarnProviderUsage".to_owned())];
    let records = SchemaRecords {
        schema: &schema,
        names: &names,
        label: "provider usage",
        require_all: false,
        metadata: |_, _, _| Ok(None),
    }
    .load_extensible()
    .expect("provider-usage schema projects to host records");
    for record in records {
        record.append(out, target);
    }
}
