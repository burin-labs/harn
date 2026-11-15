use serde::{Deserialize, Serialize};

use super::WireSourceRef;

pub(crate) const PRIVATE_SCHEMA_METADATA: &[&str] = &[
    "x-harn-source",
    "x-harn-field",
    "x-harn-variant",
    "x-harn-nominal",
    "x-harn-alias",
    "x-harn-wire-id",
];

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FieldOrigin {
    pub id: String,
    pub name: String,
    pub source: WireSourceRef,
    pub documentation: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VariantOrigin {
    pub id: String,
    pub name: String,
    pub source: WireSourceRef,
    pub documentation: Option<String>,
}
