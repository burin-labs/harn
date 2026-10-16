//! Value-free attribution for host requests crossing an embedder boundary.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::value::{DictMap, VmValue};
use crate::vm::VmCallSite;

#[cfg(test)]
mod tests;

/// One argument's type and, for a list, its cardinality. No argument values
/// are retained, including object members and list elements.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostArgumentShape {
    pub value_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_length: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostRequestTrace {
    pub caller: Option<VmCallSite>,
    pub arguments: BTreeMap<String, HostArgumentShape>,
}

impl HostRequestTrace {
    pub fn new(caller: Option<VmCallSite>, params: &DictMap) -> Self {
        Self {
            caller,
            arguments: params
                .iter()
                .map(|(key, value)| {
                    let list_length = match value {
                        VmValue::List(items) => Some(items.len()),
                        _ => None,
                    };
                    (
                        key.to_string(),
                        HostArgumentShape {
                            value_type: value.type_name().to_string(),
                            list_length,
                        },
                    )
                })
                .collect(),
        }
    }
}
