//! Central runtime ceilings for VM execution and stdlib resource guards.
//!
//! These limits are intentionally concrete fields instead of a stringly-typed
//! config map. Most are internal guardrails for adversarial recursion, memory
//! growth, or prompt/debug helper expansion. Hosts can report the effective
//! profile through [`RuntimeLimits::report`], but these defaults are not a
//! broad user-facing configuration surface.

use serde::Serialize;

/// Fixed resource and recursion ceilings used by the VM/runtime layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct RuntimeLimits {
    /// Maximum nested Harn call frames before the VM raises stack overflow.
    pub max_vm_frames: usize,
    /// Maximum nested `.harn.prompt` include depth.
    pub max_template_include_depth: usize,
    /// Maximum parsed template assets kept in the per-thread template cache.
    pub max_template_parse_cache_entries: usize,
    /// Maximum UTF-8 file reads kept in the per-thread stdlib file cache.
    pub max_file_text_cache_entries: usize,
    /// Maximum memoized `json_parse` inputs kept per VM thread.
    pub max_json_parse_cache_entries: usize,
    /// Default maximum entries for the persistent `std/cache` backends.
    pub max_std_cache_entries: usize,
    /// Maximum compiled regex entries kept per VM thread.
    pub max_regex_cache_entries: usize,
    /// Maximum compiled JSON-schema `pattern` regexes kept process-wide.
    pub max_schema_pattern_cache_entries: usize,
    /// Maximum canonical parameter schemas kept per VM thread.
    pub max_schema_guard_cache_entries: usize,
    /// Default queue depth for memory/file/sqlite event-log subscribers.
    pub default_event_log_queue_depth: usize,
    /// Default concurrent agent-session cap per VM thread.
    pub max_agent_sessions: usize,
    /// Maximum directory descent for project fingerprint discovery.
    pub max_project_fingerprint_depth: usize,
    /// Maximum nested YAML depth walked while extracting project enrichment hints.
    pub max_project_enrich_yaml_depth: usize,
    /// Maximum nested prompt-template AST/expression depth.
    pub max_template_ast_depth: usize,
    /// Maximum collection items the compiler may materialize while folding constants.
    pub max_constant_folded_collection_items: usize,
    /// Maximum string bytes the compiler may materialize while folding constants.
    pub max_constant_folded_string_bytes: usize,
    /// Default Harn-side nested-execution budget installed by the builtin policy ceiling.
    pub max_nested_execution_depth: usize,
    /// Maximum schema nesting shown in LLM schema-correction nudges.
    pub max_schema_nudge_depth: usize,
    /// Maximum schema lines shown in LLM schema-correction nudges.
    pub max_schema_nudge_lines: usize,
    /// Maximum object keys listed in LLM schema-correction nudges.
    pub max_schema_nudge_keys: usize,
    /// Maximum `VmValue` nesting depth accepted by serializers that hand the
    /// value tree to a third-party recursive encoder we cannot make
    /// stack-safe internally (pretty JSON, YAML). Our own walks grow the
    /// native stack on demand (see `value::recursion`); this ceiling keeps
    /// adversarially nested data from overflowing those external encoders.
    pub max_value_depth: usize,
}

impl RuntimeLimits {
    /// Default profile preserving the legacy hard-coded ceilings.
    pub const DEFAULT: Self = Self {
        max_vm_frames: 512,
        max_template_include_depth: 32,
        max_template_parse_cache_entries: 128,
        max_file_text_cache_entries: 256,
        max_json_parse_cache_entries: 128,
        max_std_cache_entries: 256,
        max_regex_cache_entries: 128,
        max_schema_pattern_cache_entries: 256,
        max_schema_guard_cache_entries: 256,
        default_event_log_queue_depth: 128,
        max_agent_sessions: 128,
        max_project_fingerprint_depth: 4,
        max_project_enrich_yaml_depth: 128,
        max_template_ast_depth: 128,
        max_constant_folded_collection_items: 4_096,
        max_constant_folded_string_bytes: 64 * 1024,
        max_nested_execution_depth: 8,
        max_schema_nudge_depth: 3,
        max_schema_nudge_lines: 8,
        max_schema_nudge_keys: 16,
        max_value_depth: 1024,
    };

    /// Return the value for a named limit.
    pub fn value(&self, name: &str) -> Option<usize> {
        Some(match name {
            "max_vm_frames" => self.max_vm_frames,
            "max_template_include_depth" => self.max_template_include_depth,
            "max_template_parse_cache_entries" => self.max_template_parse_cache_entries,
            "max_file_text_cache_entries" => self.max_file_text_cache_entries,
            "max_json_parse_cache_entries" => self.max_json_parse_cache_entries,
            "max_std_cache_entries" => self.max_std_cache_entries,
            "max_regex_cache_entries" => self.max_regex_cache_entries,
            "max_schema_pattern_cache_entries" => self.max_schema_pattern_cache_entries,
            "max_schema_guard_cache_entries" => self.max_schema_guard_cache_entries,
            "default_event_log_queue_depth" => self.default_event_log_queue_depth,
            "max_agent_sessions" => self.max_agent_sessions,
            "max_project_fingerprint_depth" => self.max_project_fingerprint_depth,
            "max_project_enrich_yaml_depth" => self.max_project_enrich_yaml_depth,
            "max_template_ast_depth" => self.max_template_ast_depth,
            "max_constant_folded_collection_items" => self.max_constant_folded_collection_items,
            "max_constant_folded_string_bytes" => self.max_constant_folded_string_bytes,
            "max_nested_execution_depth" => self.max_nested_execution_depth,
            "max_schema_nudge_depth" => self.max_schema_nudge_depth,
            "max_schema_nudge_lines" => self.max_schema_nudge_lines,
            "max_schema_nudge_keys" => self.max_schema_nudge_keys,
            "max_value_depth" => self.max_value_depth,
            _ => return None,
        })
    }

    /// Build a host-readable report for the effective limit profile.
    pub fn report(&self) -> RuntimeLimitsReport {
        RuntimeLimitsReport {
            entries: RUNTIME_LIMIT_DESCRIPTIONS
                .iter()
                .map(|description| RuntimeLimitEntry {
                    name: description.name,
                    value: self
                        .value(description.name)
                        .expect("runtime limit description must name a RuntimeLimits field"),
                    user_visible: description.user_visible,
                    host_configurable: description.host_configurable,
                    protects: description.protects,
                })
                .collect(),
        }
    }
}

impl Default for RuntimeLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// Host/debug report for an effective runtime-limits profile.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuntimeLimitsReport {
    pub entries: Vec<RuntimeLimitEntry>,
}

/// One documented runtime limit with its effective value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RuntimeLimitEntry {
    pub name: &'static str,
    pub value: usize,
    pub user_visible: bool,
    pub host_configurable: bool,
    pub protects: &'static str,
}

/// Static documentation for every runtime limit field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RuntimeLimitDescription {
    pub name: &'static str,
    pub user_visible: bool,
    pub host_configurable: bool,
    pub protects: &'static str,
}

pub const RUNTIME_LIMIT_DESCRIPTIONS: &[RuntimeLimitDescription] = &[
    RuntimeLimitDescription {
        name: "max_vm_frames",
        user_visible: true,
        host_configurable: false,
        protects: "prevents unbounded Harn function recursion from exhausting the VM stack",
    },
    RuntimeLimitDescription {
        name: "max_template_include_depth",
        user_visible: true,
        host_configurable: false,
        protects: "bounds recursive prompt-template includes after cycle detection",
    },
    RuntimeLimitDescription {
        name: "max_template_parse_cache_entries",
        user_visible: false,
        host_configurable: false,
        protects: "bounds per-thread memory held by parsed prompt-template assets",
    },
    RuntimeLimitDescription {
        name: "max_file_text_cache_entries",
        user_visible: false,
        host_configurable: false,
        protects: "bounds per-thread memory held by cached UTF-8 file reads",
    },
    RuntimeLimitDescription {
        name: "max_json_parse_cache_entries",
        user_visible: false,
        host_configurable: false,
        protects: "bounds per-thread memory held by memoized JSON parse inputs and values",
    },
    RuntimeLimitDescription {
        name: "max_std_cache_entries",
        user_visible: true,
        host_configurable: false,
        protects: "bounds retained entries in std/cache memory, filesystem, and sqlite backends",
    },
    RuntimeLimitDescription {
        name: "max_regex_cache_entries",
        user_visible: false,
        host_configurable: false,
        protects: "bounds per-thread memory held by compiled stdlib regex patterns",
    },
    RuntimeLimitDescription {
        name: "max_schema_pattern_cache_entries",
        user_visible: false,
        host_configurable: false,
        protects: "bounds process-wide memory held by compiled JSON-schema pattern regexes",
    },
    RuntimeLimitDescription {
        name: "max_schema_guard_cache_entries",
        user_visible: false,
        host_configurable: false,
        protects: "bounds per-thread memory held by canonical runtime parameter schemas",
    },
    RuntimeLimitDescription {
        name: "default_event_log_queue_depth",
        user_visible: true,
        host_configurable: true,
        protects: "bounds queued subscriber notifications for memory, file, and sqlite event logs",
    },
    RuntimeLimitDescription {
        name: "max_agent_sessions",
        user_visible: true,
        host_configurable: false,
        protects: "bounds concurrent first-class agent sessions stored on one VM thread",
    },
    RuntimeLimitDescription {
        name: "max_project_fingerprint_depth",
        user_visible: false,
        host_configurable: false,
        protects: "bounds project fingerprint discovery in large or adversarial directory trees",
    },
    RuntimeLimitDescription {
        name: "max_project_enrich_yaml_depth",
        user_visible: false,
        host_configurable: false,
        protects: "bounds project enrichment traversal of nested hook YAML",
    },
    RuntimeLimitDescription {
        name: "max_template_ast_depth",
        user_visible: true,
        host_configurable: false,
        protects: "bounds nested prompt-template control structures and expressions",
    },
    RuntimeLimitDescription {
        name: "max_constant_folded_collection_items",
        user_visible: false,
        host_configurable: false,
        protects: "prevents compile-time constant folding from materializing huge collections",
    },
    RuntimeLimitDescription {
        name: "max_constant_folded_string_bytes",
        user_visible: false,
        host_configurable: false,
        protects: "prevents compile-time constant folding from materializing huge strings",
    },
    RuntimeLimitDescription {
        name: "max_nested_execution_depth",
        user_visible: true,
        host_configurable: false,
        protects: "bounds nested agent loops, sub-agents, workers, and workflow stages",
    },
    RuntimeLimitDescription {
        name: "max_schema_nudge_depth",
        user_visible: false,
        host_configurable: false,
        protects: "keeps schema-retry correction prompts compact for deeply nested schemas",
    },
    RuntimeLimitDescription {
        name: "max_schema_nudge_lines",
        user_visible: false,
        host_configurable: false,
        protects: "keeps schema-retry correction prompts from growing with wide schemas",
    },
    RuntimeLimitDescription {
        name: "max_schema_nudge_keys",
        user_visible: false,
        host_configurable: false,
        protects: "keeps schema-retry object-key previews compact for wide objects",
    },
    RuntimeLimitDescription {
        name: "max_value_depth",
        user_visible: false,
        host_configurable: false,
        protects: "bounds value nesting handed to external pretty-JSON/YAML encoders so deep data cannot overflow their recursion",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_runtime_limits_match_legacy_values() {
        let limits = RuntimeLimits::default();
        assert_eq!(limits.max_vm_frames, 512);
        assert_eq!(limits.max_template_include_depth, 32);
        assert_eq!(limits.max_template_parse_cache_entries, 128);
        assert_eq!(limits.max_file_text_cache_entries, 256);
        assert_eq!(limits.max_json_parse_cache_entries, 128);
        assert_eq!(limits.max_std_cache_entries, 256);
        assert_eq!(limits.max_regex_cache_entries, 128);
        assert_eq!(limits.max_schema_pattern_cache_entries, 256);
        assert_eq!(limits.max_schema_guard_cache_entries, 256);
        assert_eq!(limits.default_event_log_queue_depth, 128);
        assert_eq!(limits.max_agent_sessions, 128);
        assert_eq!(limits.max_project_fingerprint_depth, 4);
        assert_eq!(limits.max_project_enrich_yaml_depth, 128);
        assert_eq!(limits.max_template_ast_depth, 128);
        assert_eq!(limits.max_constant_folded_collection_items, 4_096);
        assert_eq!(limits.max_constant_folded_string_bytes, 64 * 1024);
        assert_eq!(limits.max_nested_execution_depth, 8);
        assert_eq!(limits.max_schema_nudge_depth, 3);
        assert_eq!(limits.max_schema_nudge_lines, 8);
        assert_eq!(limits.max_schema_nudge_keys, 16);
        assert_eq!(limits.max_value_depth, 1024);
    }

    #[test]
    fn runtime_limit_report_documents_every_field() {
        let report = RuntimeLimits::default().report();
        let names = report
            .entries
            .iter()
            .map(|entry| entry.name)
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "max_vm_frames",
                "max_template_include_depth",
                "max_template_parse_cache_entries",
                "max_file_text_cache_entries",
                "max_json_parse_cache_entries",
                "max_std_cache_entries",
                "max_regex_cache_entries",
                "max_schema_pattern_cache_entries",
                "max_schema_guard_cache_entries",
                "default_event_log_queue_depth",
                "max_agent_sessions",
                "max_project_fingerprint_depth",
                "max_project_enrich_yaml_depth",
                "max_template_ast_depth",
                "max_constant_folded_collection_items",
                "max_constant_folded_string_bytes",
                "max_nested_execution_depth",
                "max_schema_nudge_depth",
                "max_schema_nudge_lines",
                "max_schema_nudge_keys",
                "max_value_depth",
            ]
        );
        assert!(report.entries.iter().all(|entry| entry.value > 0));
        assert!(report
            .entries
            .iter()
            .all(|entry| !entry.protects.is_empty()));
    }
}
