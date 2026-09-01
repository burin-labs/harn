//! Cross-entry invariants for adapter-specific tool projections.

use std::collections::BTreeMap;

use super::{ToolAudience, ToolCatalogEntry};

/// Validate the command tree the CLI will actually build.
///
/// Non-CLI entries do not participate: sharing their presentation path with a
/// CLI tool is harmless. Within the CLI projection, both duplicate leaves and
/// parent/leaf collisions are ambiguous regardless of declaration order.
pub(super) fn validate_cli_projection(tools: &[ToolCatalogEntry]) -> Result<(), String> {
    let mut paths = BTreeMap::<Vec<String>, usize>::new();
    for (index, tool) in tools.iter().enumerate() {
        if !tool.governance.allows(ToolAudience::Cli) {
            continue;
        }
        if let Some(previous) = paths.insert(tool.cli.command.clone(), index) {
            return Err(format!(
                "duplicate CLI command path {:?} at tools[{previous}] and tools[{index}]",
                tool.cli.command.join(" ")
            ));
        }
    }

    for (path, index) in &paths {
        for prefix_len in 1..path.len() {
            let prefix = &path[..prefix_len];
            if let Some(parent_index) = paths.get(prefix) {
                return Err(format!(
                    "CLI command path {:?} at tools[{parent_index}] is both a command and a parent of {:?} at tools[{index}]",
                    prefix.join(" "),
                    path.join(" ")
                ));
            }
        }
    }
    Ok(())
}
