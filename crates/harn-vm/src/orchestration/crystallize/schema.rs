//! Serialized bundle contract derived from the manifest's owning wire types.

use super::{types::*, CrystallizationBundleManifest};
use serde_json::{json, Value};

pub const CRYSTALLIZATION_BUNDLE_SCHEMA_ARTIFACT: &str = "crystallization-bundle.schema.json";

pub fn crystallization_bundle_json_schema() -> Value {
    let settings = schemars::generate::SchemaSettings::draft2020_12().for_serialize();
    let schema = settings
        .into_generator()
        .into_root_schema_for::<CrystallizationBundleManifest>();
    let mut schema = serde_json::to_value(schema).expect("bundle schema serializes");
    schema["properties"]["schema"]["const"] = json!(BUNDLE_SCHEMA);
    schema["properties"]["schema_version"]["const"] = json!(BUNDLE_SCHEMA_VERSION);
    schema["x-harn-bundle"] = json!({
        "schemaIdentifier": BUNDLE_SCHEMA,
        "schemaVersion": BUNDLE_SCHEMA_VERSION,
        "manifestFile": BUNDLE_MANIFEST_FILE,
        "reportFile": BUNDLE_REPORT_FILE,
        "workflowFile": BUNDLE_WORKFLOW_FILE,
        "evalPackFile": BUNDLE_EVAL_PACK_FILE,
        "fixturesDir": BUNDLE_FIXTURES_DIR,
        "skillDir": BUNDLE_SKILL_DIR,
        "skillFile": BUNDLE_SKILL_FILE,
        "skillGateFile": BUNDLE_SKILL_GATE_FILE,
    });
    schema
}
