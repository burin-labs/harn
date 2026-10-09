//! Executable registry projection and direct-adapter invocation eligibility.

use super::{
    registry_dict, registry_entries, tool_registry_catalog, ToolAudience, ToolCatalogEntry,
};
use crate::value::{VmClosure, VmError, VmValue};

/// Normalized catalog entry paired with its one executable Harn handler.
pub struct ExecutableTool {
    pub catalog: ToolCatalogEntry,
    pub handler: VmClosure,
    pub invocation_requirement: ToolInvocationRequirement,
}

/// Execution requirement retained when projecting a local handler to adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ToolInvocationRequirement {
    Direct,
    Prepared,
}

impl ToolInvocationRequirement {
    /// Direct adapters have no preparation or consent owner. They must not
    /// borrow an enclosing invocation's approval for a different call.
    pub fn require_direct(self, name: &str) -> Result<(), VmError> {
        match self {
            Self::Direct => Ok(()),
            Self::Prepared => Err(VmError::CategorizedError {
                message: format!(
                    "tool {name:?} requires approved invocation preparation; this adapter does not support prepared calls"
                ),
                category: crate::value::ErrorCategory::ToolRejected,
            }),
        }
    }
}

/// Normalize a registry and require one local Harn closure per tool.
pub fn executable_tools(registry: &VmValue) -> Result<Vec<ExecutableTool>, VmError> {
    executable_tools_matching(registry, None)
}

/// Normalize executable tools, then require handlers only for entries exposed
/// to the requested adapter. An excluded alternate-executor entry must not
/// prevent an adapter from loading the tools it can actually invoke.
pub fn executable_tools_for_audience(
    registry: &VmValue,
    audience: ToolAudience,
) -> Result<Vec<ExecutableTool>, VmError> {
    executable_tools_matching(registry, Some(audience))
}

fn executable_tools_matching(
    registry: &VmValue,
    audience: Option<ToolAudience>,
) -> Result<Vec<ExecutableTool>, VmError> {
    let registry_dict = registry_dict(registry)?;
    let entries = registry_entries(registry_dict)?;
    let catalog = tool_registry_catalog(registry)?;
    let mut executable = Vec::with_capacity(entries.len());
    for (entry, catalog) in entries.iter().zip(catalog.tools) {
        if audience.is_some_and(|audience| !catalog.governance.allows(audience)) {
            continue;
        }
        let VmValue::Dict(entry) = entry else {
            return Err(VmError::Runtime(
                "tool registry entries must be objects".into(),
            ));
        };
        let Some(VmValue::Closure(handler)) = entry.get("handler") else {
            return Err(VmError::Runtime(format!(
                "tool registry entry {:?} has no local Harn handler closure",
                catalog.name
            )));
        };
        let invocation_requirement = match entry.get("prepare") {
            None | Some(VmValue::Nil) => ToolInvocationRequirement::Direct,
            Some(VmValue::Closure(_)) => ToolInvocationRequirement::Prepared,
            _ => {
                return Err(VmError::Runtime(format!(
                    "tool registry entry {:?} preparation must be a local Harn closure",
                    catalog.name
                )));
            }
        };
        executable.push(ExecutableTool {
            catalog,
            handler: handler.as_ref().clone(),
            invocation_requirement,
        });
    }
    Ok(executable)
}
