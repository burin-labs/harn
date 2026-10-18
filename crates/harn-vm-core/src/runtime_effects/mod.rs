//! Typed runtime effect receipts and their execution-local recorder.

mod cache;
mod state;

pub use state::RuntimeEffectState;

use crate::VmValue;
use cache::resolve_runtime_resources;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Kind of effect captured in a handoff or execution receipt.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Ord, PartialOrd, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EffectKind {
    /// Reads or writes against the host's stdio streams.
    Stdio,
    /// Filesystem access (read, write, list, delete).
    Fs,
    /// Network access (HTTP, SSE, WebSocket).
    Net,
    /// Environment-variable access.
    Env,
    /// Wall or monotonic clock access.
    Clock,
    /// Nondeterministic random source access.
    Random,
    /// Child-process execution or observation.
    Process,
    /// Secret custody access.
    Secret,
    /// Logs, traces, metrics, and request-correlation state.
    Observability,
    /// Durable transcript channel access.
    Channel,
    /// Durable or execution-local state access.
    State,
    /// Other typed host-service access.
    Host,
    /// Opaque authority that public values cannot manufacture.
    Authority,
    /// LLM calls with provider and model receipt metadata when known.
    Llm {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// Pipeline-declared tool dispatched through the agent loop.
    Tool { name: String },
    /// Bridged host capability call.
    Hostcall { name: String },
    /// Targeted delegation to a named persona or sub-agent identity.
    Persona { id: String },
    /// Spawn, sub-agent, or worker dispatch.
    Spawn,
}

/// Interaction represented by an effect receipt.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Ord, PartialOrd, Hash)]
#[serde(rename_all = "snake_case")]
pub enum EffectScope {
    Read,
    Write,
    Mutate,
    Observe,
}

/// One typed effect. An unknown resource stays absent rather than inventing a target.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub struct EffectRecord {
    pub kind: EffectKind,
    pub scope: EffectScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<crate::value::HarnStr>,
}

impl EffectRecord {
    pub fn new(kind: EffectKind, scope: EffectScope) -> Self {
        Self {
            kind,
            scope,
            resource: None,
        }
    }

    pub fn with_resource(mut self, resource: impl Into<crate::value::HarnStr>) -> Self {
        let resource = resource.into();
        self.resource = if resource.is_empty() {
            None
        } else {
            Some(resource)
        };
        self
    }
}

/// Shared receipt set. Per-VM memoization avoids repeated materialization.
#[derive(Default)]
pub(crate) struct ExecutedEffectRecorder {
    effects: HashSet<EffectRecord>,
}

impl ExecutedEffectRecorder {
    pub(crate) fn record(&mut self, specs: &[harn_builtin_meta::EffectSpec], args: &[VmValue]) {
        self.effects
            .extend(runtime_effects_from_contract(specs, args));
    }

    pub(crate) fn snapshot(&self) -> Vec<EffectRecord> {
        let mut effects = self.effects.iter().cloned().collect::<Vec<_>>();
        effects.sort();
        effects
    }

    pub(crate) fn clear(&mut self) {
        self.effects.clear();
    }
}

pub fn runtime_effects_from_contract(
    specs: &[harn_builtin_meta::EffectSpec],
    args: &[VmValue],
) -> Vec<EffectRecord> {
    let mut records = Vec::new();
    let llm_specs = specs
        .iter()
        .filter(|spec| spec.kind == harn_builtin_meta::EffectKind::Llm)
        .collect::<Vec<_>>();
    if let Some(first) = llm_specs.first() {
        let mut provider = None;
        let mut model = None;
        for spec in &llm_specs {
            for selector in spec.resources {
                let harn_builtin_meta::ResourceSelector::Field { path, .. } = selector else {
                    continue;
                };
                let value = resolve_runtime_resources(*selector, args)
                    .into_iter()
                    .next();
                match path.last().copied() {
                    Some("provider") => provider = value.map(|value| value.to_string()),
                    Some("model") => model = value.map(|value| value.to_string()),
                    _ => {}
                }
            }
        }
        records.push(EffectRecord::new(
            EffectKind::Llm { provider, model },
            effect_scope_from_contract(first.access),
        ));
    }
    for spec in specs {
        if spec.kind == harn_builtin_meta::EffectKind::Llm {
            continue;
        }
        let kind = effect_kind_from_contract(spec.kind);
        let scope = effect_scope_from_contract(spec.access);
        let resources = spec
            .resources
            .iter()
            .flat_map(|selector| resolve_runtime_resources(*selector, args))
            .collect::<Vec<_>>();
        if resources.is_empty() {
            records.push(EffectRecord::new(kind, scope));
        } else {
            records.extend(
                resources
                    .into_iter()
                    .map(|resource| EffectRecord::new(kind.clone(), scope).with_resource(resource)),
            );
        }
    }
    records
}

pub fn effect_kind_from_contract(kind: harn_builtin_meta::EffectKind) -> EffectKind {
    use harn_builtin_meta::EffectKind as ContractKind;
    match kind {
        ContractKind::Stdio => EffectKind::Stdio,
        ContractKind::Fs => EffectKind::Fs,
        ContractKind::Network => EffectKind::Net,
        ContractKind::Llm => EffectKind::Llm {
            provider: None,
            model: None,
        },
        ContractKind::Tool => EffectKind::Tool {
            name: String::new(),
        },
        ContractKind::Mcp => EffectKind::Tool {
            name: "mcp".to_string(),
        },
        ContractKind::Worker => EffectKind::Spawn,
        ContractKind::Process => EffectKind::Process,
        ContractKind::Env => EffectKind::Env,
        ContractKind::Clock => EffectKind::Clock,
        ContractKind::Random => EffectKind::Random,
        ContractKind::Host => EffectKind::Host,
        ContractKind::Authority => EffectKind::Authority,
        ContractKind::Secret => EffectKind::Secret,
        ContractKind::Observability => EffectKind::Observability,
        ContractKind::Channel => EffectKind::Channel,
        ContractKind::State => EffectKind::State,
    }
}

pub fn effect_scope_from_contract(access: harn_builtin_meta::EffectAccess) -> EffectScope {
    match access {
        harn_builtin_meta::EffectAccess::Read => EffectScope::Read,
        harn_builtin_meta::EffectAccess::Write => EffectScope::Write,
        harn_builtin_meta::EffectAccess::Mutate => EffectScope::Mutate,
        harn_builtin_meta::EffectAccess::Observe => EffectScope::Observe,
    }
}
