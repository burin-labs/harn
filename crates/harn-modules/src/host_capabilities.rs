//! Shared checks for declared and served host operations.
//!
//! The CLI and runtime adapters both need to find declared operations that a
//! host does not serve. This module owns that set check.

use std::collections::{BTreeMap, BTreeSet};

/// One namespaced host operation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HostCapabilityOperation {
    pub capability: String,
    pub operation: String,
}

impl HostCapabilityOperation {
    #[must_use]
    pub fn qualified_name(&self) -> String {
        format!("{}.{}", self.capability, self.operation)
    }
}

/// A deterministic set of host capability operations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostCapabilitySurface {
    operations: BTreeMap<String, BTreeSet<String>>,
}

impl HostCapabilitySurface {
    #[must_use]
    pub fn from_pairs<I, C, O>(pairs: I) -> Self
    where
        I: IntoIterator<Item = (C, O)>,
        C: Into<String>,
        O: Into<String>,
    {
        let mut surface = Self::default();
        for (capability, operation) in pairs {
            surface
                .operations
                .entry(capability.into())
                .or_default()
                .insert(operation.into());
        }
        surface
    }

    #[must_use]
    pub fn contains(&self, capability: &str, operation: &str) -> bool {
        self.operations
            .get(capability)
            .is_some_and(|operations| operations.contains(operation))
    }

    pub fn operation_pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.operations.iter().flat_map(|(capability, operations)| {
            operations
                .iter()
                .map(move |operation| (capability.as_str(), operation.as_str()))
        })
    }

    /// Return each declared operation the host does not serve, except for
    /// operations whose handlers are added at runtime.
    #[must_use]
    pub fn missing_from(
        &self,
        served: &Self,
        runtime_installed: &HostCapabilityExemptions,
    ) -> Vec<HostCapabilityOperation> {
        self.operation_pairs()
            .filter(|(capability, operation)| {
                !served.contains(capability, operation)
                    && !runtime_installed.contains(capability, operation)
            })
            .map(|(capability, operation)| HostCapabilityOperation {
                capability: capability.to_string(),
                operation: operation.to_string(),
            })
            .collect()
    }
}

/// Exact operations whose handlers are added at runtime.
///
/// Wildcards are not allowed. Each exemption must name one operation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostCapabilityExemptions(HostCapabilitySurface);

impl HostCapabilityExemptions {
    pub fn parse<'a>(values: impl IntoIterator<Item = &'a str>) -> Result<Self, String> {
        let mut operations = Vec::new();
        for value in values {
            let Some((capability, operation)) = value.split_once('.') else {
                return Err(format!(
                    "runtime-installed host operation `{value}` must use `capability.operation`"
                ));
            };
            if capability.is_empty()
                || operation.is_empty()
                || operation.contains('.')
                || capability == "*"
                || operation == "*"
            {
                return Err(format!(
                    "runtime-installed host operation `{value}` must name one exact `capability.operation` pair"
                ));
            }
            operations.push((capability, operation));
        }
        Ok(Self(HostCapabilitySurface::from_pairs(operations)))
    }

    #[must_use]
    pub fn contains(&self, capability: &str, operation: &str) -> bool {
        self.0.contains(capability, operation)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconciliation_is_sorted_and_honors_exact_runtime_installations() {
        let declared = HostCapabilitySurface::from_pairs([
            ("workspace", "write_text"),
            ("project", "runtime_only"),
            ("workspace", "read_text"),
        ]);
        let served = HostCapabilitySurface::from_pairs([("workspace", "read_text")]);
        let exemptions = HostCapabilityExemptions::parse(["project.runtime_only"]).unwrap();

        assert_eq!(
            declared
                .missing_from(&served, &exemptions)
                .into_iter()
                .map(|operation| operation.qualified_name())
                .collect::<Vec<_>>(),
            ["workspace.write_text"]
        );
    }

    #[test]
    fn runtime_installations_reject_wildcards_and_malformed_names() {
        for value in ["workspace", "workspace.*", "*.read_text", "a.b.c"] {
            assert!(HostCapabilityExemptions::parse([value]).is_err(), "{value}");
        }
    }
}
