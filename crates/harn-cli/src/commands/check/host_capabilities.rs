use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use crate::package::CheckConfig;

#[derive(Clone)]
pub(super) struct ParamDiscriminatorPolicy {
    pub(super) allowed_values: BTreeSet<String>,
    pub(super) allow_dynamic: bool,
}

type OperationParamDiscriminators = BTreeMap<String, ParamDiscriminatorPolicy>;
type CapabilityParamDiscriminators = HashMap<String, OperationParamDiscriminators>;

#[derive(Clone, Default)]
pub(crate) struct HostCapabilities {
    operations: HashMap<String, HashSet<String>>,
    param_discriminators: HashMap<String, CapabilityParamDiscriminators>,
}

impl HostCapabilities {
    pub(crate) fn contains_operation(&self, capability: &str, operation: &str) -> bool {
        self.operations
            .get(capability)
            .is_some_and(|operations| operations.contains(operation))
    }

    pub(super) fn operations_mut(&mut self) -> &mut HashMap<String, HashSet<String>> {
        &mut self.operations
    }

    pub(crate) fn into_operations(self) -> HashMap<String, HashSet<String>> {
        self.operations
    }

    /// Every declared `(capability, operation)` pair.
    pub(super) fn operation_pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.operations.iter().flat_map(|(capability, operations)| {
            operations
                .iter()
                .map(move |operation| (capability.as_str(), operation.as_str()))
        })
    }

    pub(super) fn param_discriminators(
        &self,
        capability: &str,
        operation: &str,
    ) -> Option<&OperationParamDiscriminators> {
        self.param_discriminators
            .get(capability)
            .and_then(|operations| operations.get(operation))
    }

    pub(crate) fn into_manifest_entries(self) -> BTreeMap<String, serde_json::Value> {
        let HostCapabilities {
            operations,
            mut param_discriminators,
        } = self;
        operations
            .into_iter()
            .map(|(capability, operations)| {
                let Some(operation_discriminators) = param_discriminators.remove(&capability)
                else {
                    let mut operations = operations.into_iter().collect::<Vec<_>>();
                    operations.sort();
                    return (capability, serde_json::json!(operations));
                };
                let operations = operations
                    .into_iter()
                    .map(|operation| {
                        let metadata = operation_discriminators.get(&operation).map_or_else(
                            || serde_json::Value::Bool(true),
                            |fields| {
                                let fields = fields
                                    .iter()
                                    .map(|(field, policy)| {
                                        (
                                            field,
                                            serde_json::json!({
                                                "values": policy.allowed_values,
                                                "allow_dynamic": policy.allow_dynamic,
                                            }),
                                        )
                                    })
                                    .collect::<BTreeMap<_, _>>();
                                serde_json::json!({ "param_discriminators": fields })
                            },
                        );
                        (operation, metadata)
                    })
                    .collect::<BTreeMap<_, _>>();
                (capability, serde_json::json!({ "operations": operations }))
            })
            .collect()
    }
}

pub(super) struct ResolvedHostCapabilities {
    pub(super) capabilities: HostCapabilities,
    pub(super) source_content: Option<String>,
    pub(super) reconciliation: Option<HostCapabilityReconciliation>,
}

pub(super) struct HostCapabilityReconciliation {
    pub(super) missing_operations: Vec<harn_modules::host_capabilities::HostCapabilityOperation>,
    pub(super) error: Option<String>,
    pub(super) served_path: String,
}

static DEFAULT_HOST_CAPABILITIES: LazyLock<HostCapabilities> =
    LazyLock::new(default_host_capabilities);

fn default_host_capabilities() -> HostCapabilities {
    HostCapabilities {
        operations: HashMap::from([
            (
                "workspace".to_string(),
                HashSet::from([
                    "read_text".to_string(),
                    "write_text".to_string(),
                    "apply_edit".to_string(),
                    "delete".to_string(),
                    "exists".to_string(),
                    "file_exists".to_string(),
                    "list".to_string(),
                    "project_root".to_string(),
                    "roots".to_string(),
                ]),
            ),
            (
                "process".to_string(),
                HashSet::from([
                    "exec".to_string(),
                    // #3252: non-blocking spawn lifecycle ops.
                    "spawn".to_string(),
                    "poll".to_string(),
                    "wait".to_string(),
                    "kill".to_string(),
                    "release".to_string(),
                    "get_default_shell".to_string(),
                    "list_shells".to_string(),
                    "set_default_shell".to_string(),
                    "shell_invocation".to_string(),
                ]),
            ),
            (
                "template".to_string(),
                HashSet::from(["render".to_string()]),
            ),
            (
                "interaction".to_string(),
                HashSet::from(["ask".to_string()]),
            ),
            (
                "runtime".to_string(),
                HashSet::from([
                    "approved_plan".to_string(),
                    "dry_run".to_string(),
                    "pipeline_input".to_string(),
                    "prompt_content".to_string(),
                    "record_run".to_string(),
                    "set_result".to_string(),
                    "task".to_string(),
                ]),
            ),
            (
                "project".to_string(),
                HashSet::from([
                    "agent_instructions".to_string(),
                    "code_patterns".to_string(),
                    "compute_content_hash".to_string(),
                    "ide_context".to_string(),
                    "lessons".to_string(),
                    "mcp_config".to_string(),
                    "metadata_get".to_string(),
                    "metadata_inspect".to_string(),
                    "metadata_refresh_hashes".to_string(),
                    "metadata_save".to_string(),
                    "metadata_set".to_string(),
                    "metadata_stale".to_string(),
                    "path_metadata_entries".to_string(),
                    "path_metadata_get".to_string(),
                    "path_metadata_set".to_string(),
                    "scan".to_string(),
                    "scope_test_command".to_string(),
                    "test_commands".to_string(),
                ]),
            ),
            (
                "session".to_string(),
                HashSet::from([
                    "active_roots".to_string(),
                    "changed_paths".to_string(),
                    "preread_get".to_string(),
                    "preread_read_many".to_string(),
                ]),
            ),
            (
                "editor".to_string(),
                HashSet::from([
                    "get_active_file".to_string(),
                    "get_selection".to_string(),
                    "get_visible_files".to_string(),
                ]),
            ),
            (
                "diagnostics".to_string(),
                HashSet::from(["get_causal_traces".to_string(), "get_errors".to_string()]),
            ),
            (
                "git".to_string(),
                HashSet::from(["get_branch".to_string(), "get_diff".to_string()]),
            ),
            (
                "learning".to_string(),
                HashSet::from([
                    "get_learned_rules".to_string(),
                    "report_correction".to_string(),
                ]),
            ),
        ]),
        param_discriminators: HashMap::new(),
    }
}

fn merge_host_capability_map(target: &mut HostCapabilities, source: HostCapabilities) {
    let HostCapabilities {
        operations,
        param_discriminators,
    } = source;
    for (capability, ops) in operations {
        target.operations.entry(capability).or_default().extend(ops);
    }
    for (capability, operations) in param_discriminators {
        let target_operations = target.param_discriminators.entry(capability).or_default();
        for (operation, fields) in operations {
            let target_fields = target_operations.entry(operation).or_default();
            for (field, policy) in fields {
                target_fields
                    .entry(field)
                    .and_modify(|target_policy| {
                        target_policy
                            .allowed_values
                            .extend(policy.allowed_values.iter().cloned());
                        target_policy.allow_dynamic |= policy.allow_dynamic;
                    })
                    .or_insert(policy);
            }
        }
    }
}

pub(super) fn parse_host_capability_value(value: &serde_json::Value) -> HostCapabilities {
    let root = value.get("capabilities").unwrap_or(value);
    let mut result = HostCapabilities::default();
    let Some(capabilities) = root.as_object() else {
        return HostCapabilities::default();
    };
    for (capability, entry) in capabilities {
        let mut ops = HashSet::new();
        let mut discriminators = CapabilityParamDiscriminators::new();
        if let Some(list) = entry.as_array() {
            for item in list {
                if let Some(op) = item.as_str() {
                    ops.insert(op.to_string());
                }
            }
        } else if let Some(obj) = entry.as_object() {
            let operation_value = obj.get("operations").or_else(|| obj.get("ops"));
            if let Some(list) = operation_value.and_then(serde_json::Value::as_array) {
                parse_operation_list(list, &mut ops);
            } else if let Some(operation_map) =
                operation_value.and_then(serde_json::Value::as_object)
            {
                parse_operation_map(operation_map, &mut ops, &mut discriminators);
            } else {
                parse_operation_map(obj, &mut ops, &mut discriminators);
            }
        }
        if !ops.is_empty() {
            result.operations.insert(capability.clone(), ops);
        }
        if !discriminators.is_empty() {
            result
                .param_discriminators
                .insert(capability.clone(), discriminators);
        }
    }
    result
}

fn parse_operation_list(list: &[serde_json::Value], ops: &mut HashSet<String>) {
    for item in list {
        if let Some(operation) = item.as_str() {
            ops.insert(operation.to_string());
        }
    }
}

fn parse_operation_map(
    operation_map: &serde_json::Map<String, serde_json::Value>,
    ops: &mut HashSet<String>,
    discriminators: &mut CapabilityParamDiscriminators,
) {
    for (operation, metadata) in operation_map {
        if !metadata.as_bool().unwrap_or(true) {
            continue;
        }
        ops.insert(operation.clone());
        let Some(fields) = metadata
            .get("param_discriminators")
            .and_then(serde_json::Value::as_object)
        else {
            continue;
        };
        for (field, values) in fields {
            let (values, allow_dynamic) = if let Some(values) = values.as_array() {
                (values, false)
            } else if let Some(policy) = values.as_object() {
                let Some(values) = policy.get("values").and_then(serde_json::Value::as_array)
                else {
                    continue;
                };
                (
                    values,
                    policy
                        .get("allow_dynamic")
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false),
                )
            } else {
                continue;
            };
            let allowed_values = values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToString::to_string)
                .collect::<BTreeSet<_>>();
            if !allowed_values.is_empty() {
                discriminators.entry(operation.clone()).or_default().insert(
                    field.clone(),
                    ParamDiscriminatorPolicy {
                        allowed_values,
                        allow_dynamic,
                    },
                );
            }
        }
    }
}

pub(super) fn resolve_host_capabilities(config: &CheckConfig) -> ResolvedHostCapabilities {
    let mut capabilities = DEFAULT_HOST_CAPABILITIES.clone();
    let inline = config
        .host
        .host_capabilities
        .iter()
        .map(|(capability, ops)| {
            (
                capability.clone(),
                ops.iter().cloned().collect::<HashSet<String>>(),
            )
        })
        .collect::<HashMap<_, _>>();
    let inline = HostCapabilities {
        operations: inline,
        param_discriminators: HashMap::new(),
    };
    merge_host_capability_map(&mut capabilities, inline);
    let resolved =
        harn_modules::host_capability_config::resolve_host_capability_config(&config.host);
    if let Some(value) = resolved.source_value.as_ref() {
        merge_host_capability_map(&mut capabilities, parse_host_capability_value(value));
    }
    let declared = resolved.declared;
    let source_content = resolved.source_content;
    let declaration_error = resolved.error;
    let reconciliation = config
        .host
        .host_served_capabilities_path
        .as_deref()
        .map(|path| {
            let mut result = HostCapabilityReconciliation {
                missing_operations: Vec::new(),
                error: declaration_error.clone(),
                served_path: path.to_string(),
            };
            if result.error.is_some() {
                return result;
            }
            let served = std::fs::read_to_string(path)
                .map_err(|error| {
                    format!("failed to read served host operations from `{path}`: {error}")
                })
                .and_then(|content| {
                    harn_modules::host_capabilities::parse_host_capability_document(
                        &content, path, "served",
                    )
                })
                .map(|value| {
                    harn_modules::host_capabilities::HostCapabilitySurface::from_value(&value)
                });
            let exemptions = harn_modules::host_capabilities::HostCapabilityExemptions::parse(
                config
                    .host
                    .runtime_installed_host_operations
                    .iter()
                    .map(String::as_str),
            );
            match (served, exemptions) {
                (Ok(served), Ok(exemptions)) => {
                    result.missing_operations = declared.missing_from(&served, &exemptions);
                }
                (Err(error), _) | (_, Err(error)) => result.error = Some(error),
            }
            result
        });
    ResolvedHostCapabilities {
        capabilities,
        source_content,
        reconciliation,
    }
}

pub(crate) fn load_host_capabilities(config: &CheckConfig) -> HostCapabilities {
    resolve_host_capabilities(config).capabilities
}

pub(super) fn is_known_host_operation(
    capabilities: &HostCapabilities,
    capability: &str,
    operation: &str,
) -> bool {
    capabilities.contains_operation(capability, operation)
}
