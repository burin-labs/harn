//! Policy type definitions — shapes used to describe agent capability
//! ceilings, turn/model/transcript policies, and the per-tool argument
//! constraint machinery. Everything here is plain data (+ a single
//! helper, `enforce_tool_arg_constraints`, that operates on it).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::super::glob_match;
use super::{reject_tool, PolicyDenial};
use crate::agent_events::DenialGate;
use crate::tool_annotations::ToolAnnotations;
use crate::value::VmValue;

/// Extended policy that supports argument-level constraints.
///
/// `arg_key` names the argument whose string value must match one of
/// `arg_patterns`. It is the self-describing form and should be set
/// explicitly by the policy author. When absent, the enforcer falls
/// back to `tool_annotations[tool].arg_schema.path_params`. If neither is populated,
/// the constraint is skipped with a structured `log_warn` — the VM is
/// intentionally domain-agnostic and does not guess argument semantics
/// by name (no "path"/"file"/"command"/... fallback list).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ToolArgConstraint {
    /// Tool name to constrain (glob-matched against dispatched tool names).
    pub tool: String,
    /// Glob patterns that the resolved argument value must match.
    /// If empty, no argument constraint is applied.
    pub arg_patterns: Vec<String>,
    /// Optional argument key whose string value is the constraint target.
    /// When present, overrides any metadata-derived key.
    #[serde(default)]
    pub arg_key: Option<String>,
}

/// Check if a tool call satisfies argument constraints in the policy.
///
/// Resolution order for which argument value to match:
/// 1. `constraint.arg_key` if set (explicit, self-describing).
/// 2. `policy.tool_annotations[tool].arg_schema.path_params` (first key
///    that yields a string value in the args object).
/// 3. No candidate — `log_warn` and skip the constraint. The VM refuses
///    to guess which argument holds the domain-relevant value.
pub fn enforce_tool_arg_constraints(
    policy: &CapabilityPolicy,
    tool_name: &str,
    args: &serde_json::Value,
) -> Result<(), PolicyDenial> {
    for constraint in &policy.tool_arg_constraints {
        if !glob_match(&constraint.tool, tool_name) {
            continue;
        }
        if constraint.arg_patterns.is_empty() {
            continue;
        }

        // Never guess which arg the constraint targets by common names —
        // that's a pipeline-level concern, not a VM concern.
        let declared_keys: Vec<String> = if let Some(key) = constraint.arg_key.as_ref() {
            vec![key.clone()]
        } else {
            policy
                .tool_annotations
                .get(tool_name)
                .map(|a| a.arg_schema.path_params.clone())
                .unwrap_or_default()
        };

        let (arg_key, arg_value): (String, Option<String>) = if let Some(obj) = args.as_object() {
            if declared_keys.is_empty() {
                // Permissive by design: missing annotations warn instead of
                // blocking, so a misconfigured policy can't silently wedge work.
                crate::events::log_warn(
                    "policy.constraint_unresolved",
                    &format!(
                        "tool_arg_constraint for tool '{tool_name}' has no arg_key and tool_annotations.arg_schema.path_params is empty; skipping (policy author should declare arg_key on the constraint or path_params in the tool's annotations)"
                    ),
                );
                continue;
            }
            let mut found: (String, Option<String>) = (declared_keys[0].clone(), None);
            for param in &declared_keys {
                if let Some(value) = obj.get(param).and_then(|v| v.as_str()) {
                    found = (param.clone(), Some(value.to_string()));
                    break;
                }
            }
            found
        } else {
            ("value".to_string(), args.as_str().map(|s| s.to_string()))
        };

        // Absent arg ≠ rejection — constraint simply does not apply.
        let Some(candidate) = arg_value else {
            continue;
        };
        let matches = constraint
            .arg_patterns
            .iter()
            .any(|pattern| glob_match(pattern, &candidate));
        if !matches {
            return reject_tool(
                DenialGate::ArgConstraint,
                None,
                format!(
                    "tool '{tool_name}' {arg_key} '{candidate}' is outside your allowed scope. \
                     Allowed {arg_key} pattern(s): {:?}. This is fixable: re-issue the call with a \
                     {arg_key} that matches one of those patterns. If '{candidate}' is a \
                     reference/example you only need to consult, read it with `look` instead — never \
                     write to a path outside your scope.",
                    constraint.arg_patterns
                ),
            );
        }
    }
    Ok(())
}

/// Selectable confinement strength, ordered from weakest to strongest.
///
/// A profile decides two separate questions, and callers must ask which
/// one they mean rather than matching on variants:
///
/// - [`SandboxProfile::enforces_path_scope`] — do the in-process
///   `harness.fs.*` builtins check paths against `workspace_roots`?
/// - [`SandboxProfile::confines_processes`] — is an OS mechanism applied
///   to subprocesses this policy spawns?
///
/// The two are genuinely independent: path scoping is portable and
/// deterministic, while OS confinement depends on a platform mechanism
/// that may be unavailable or too coarse for the job.
/// [`SandboxProfile::WorkspacePaths`] is the rung that answers yes to the
/// first and no to the second.
///
/// Defaults to [`SandboxProfile::Worktree`] — path enforcement plus
/// best-effort OS confinement (warn-and-skip if the platform mechanism is
/// unavailable). Stricter callers opt into [`SandboxProfile::OsHardened`],
/// which requires the OS sandbox to engage and surfaces a typed
/// `tool_rejected` error otherwise.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum SandboxProfile {
    /// No path enforcement, no OS confinement. Used by direct VM
    /// embeddings with no orchestration policy and by escape hatches
    /// that explicitly disable isolation.
    Unrestricted,
    /// Workspace-root path enforcement, no OS confinement.
    ///
    /// For callers that own the code they are running and want its
    /// writes confined, but must let it shell out freely — a test runner
    /// isolating cases from each other, a build driver that invokes a
    /// toolchain. OS confinement would buy nothing here (the subprocess
    /// is as trusted as its parent) while costing portability, since the
    /// platform mechanisms differ in what they permit.
    ///
    /// Do not use this for foreign or untrusted code: a subprocess it
    /// spawns is unconfined, so path scoping alone is not containment.
    WorkspacePaths,
    /// Workspace-root path enforcement plus best-effort OS confinement.
    /// Honors `HARN_HANDLER_SANDBOX={off,warn,enforce}` for the OS
    /// portion. Default for `harn run` and orchestrator-launched
    /// workflows.
    #[default]
    Worktree,
    /// Testbench WASI sandbox — subprocess execution is replayed from
    /// recorded WASI modules instead of running on the host. Selected
    /// indirectly by `harn test bench --process-wasi <dir>`. No OS
    /// mechanism is applied on the host spawn path, because a replayed
    /// subprocess never reaches it.
    Wasi,
    /// Workspace-root path enforcement plus required OS confinement.
    /// Spawns fail with `tool_rejected` if the platform's hardening
    /// mechanism (Linux Landlock+seccomp, macOS sandbox-exec, Windows
    /// AppContainer) is unavailable, regardless of
    /// `HARN_HANDLER_SANDBOX`.
    OsHardened,
}

impl SandboxProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            SandboxProfile::Unrestricted => "unrestricted",
            SandboxProfile::WorkspacePaths => "workspace_paths",
            SandboxProfile::Worktree => "worktree",
            SandboxProfile::Wasi => "wasi",
            SandboxProfile::OsHardened => "os_hardened",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "unrestricted" => Some(SandboxProfile::Unrestricted),
            "workspace_paths" => Some(SandboxProfile::WorkspacePaths),
            "worktree" => Some(SandboxProfile::Worktree),
            "wasi" => Some(SandboxProfile::Wasi),
            "os_hardened" => Some(SandboxProfile::OsHardened),
            _ => None,
        }
    }

    /// Every profile name, weakest first — the single source of truth for
    /// error messages, CLI help, and docs that enumerate the vocabulary.
    pub fn all() -> &'static [SandboxProfile] {
        &[
            SandboxProfile::Unrestricted,
            SandboxProfile::WorkspacePaths,
            SandboxProfile::Worktree,
            SandboxProfile::Wasi,
            SandboxProfile::OsHardened,
        ]
    }

    /// Whether Harn's own in-process path checks are enforced: the
    /// `harness.fs.*` builtins scoping against `workspace_roots` and
    /// `read_only_roots`, and the launch-cwd check for subprocesses.
    ///
    /// This axis is pure Harn bookkeeping — portable, deterministic, and
    /// independent of any platform mechanism.
    pub fn enforces_path_scope(self) -> bool {
        !matches!(self, SandboxProfile::Unrestricted)
    }

    /// Whether an OS mechanism (Linux Landlock+seccomp, macOS
    /// sandbox-exec, Windows AppContainer) is applied to subprocesses
    /// spawned under this policy.
    ///
    /// This axis is the only one that can deny a child something Harn did
    /// not ask about, so it is also the only one entitled to report a
    /// failure as an OS sandbox denial.
    ///
    /// False for `Wasi`: testbench mode intercepts subprocesses before
    /// they reach the host spawn path, so nothing there is confined.
    pub fn confines_processes(self) -> bool {
        matches!(self, SandboxProfile::Worktree | SandboxProfile::OsHardened)
    }
}

/// Named host filesystem presets granted only to child-process OS
/// sandboxes. These do not widen Harn file builtins; they are used so
/// subprocesses can load runtimes, compilers, and cache files that live
/// outside the workspace while Harn's own read/write surface remains
/// scoped by `workspace_roots` and `read_only_roots`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[serde(rename_all = "snake_case")]
pub enum ProcessSandboxPreset {
    /// Minimal host runtime roots needed to execute common system binaries.
    SystemRuntime,
    /// OS/vendor developer toolchains such as Xcode, Command Line Tools,
    /// Homebrew, plus common user-managed runtime roots such as
    /// `~/.local/share/uv`, `~/.rustup`, `~/.cargo`, `~/.pyenv`, and `~/.nvm`.
    DeveloperToolchains,
    /// Per-user package-manager config/cache roots used by npm, pip, cargo,
    /// git credential helpers, and enterprise CA configuration.
    PackageManagerConfig,
    /// Per-user scratch/cache locations used by developer tools. Write access
    /// is granted only when the active policy already allows workspace writes.
    UserTemp,
}

impl ProcessSandboxPreset {
    pub const fn default_presets() -> &'static [Self] {
        &[
            Self::SystemRuntime,
            Self::DeveloperToolchains,
            Self::PackageManagerConfig,
            Self::UserTemp,
        ]
    }
}

/// Process-only filesystem policy layered onto the active sandbox profile.
///
/// `presets: None` means "use the runtime defaults"; `Some([])` is an
/// explicit request for no named presets. Extra roots are process-only:
/// they do not allow Harn file tools to read or write those paths.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ProcessSandboxPolicy {
    pub presets: Option<Vec<ProcessSandboxPreset>>,
    pub read_roots: Vec<String>,
    pub write_roots: Vec<String>,
}

impl ProcessSandboxPolicy {
    pub fn effective_presets(&self) -> Vec<ProcessSandboxPreset> {
        self.presets
            .clone()
            .unwrap_or_else(|| ProcessSandboxPreset::default_presets().to_vec())
    }

    pub fn extend(&mut self, other: &Self) {
        if let Some(presets) = other.presets.as_ref() {
            self.presets = Some(presets.clone());
        }
        extend_unique(&mut self.read_roots, &other.read_roots);
        extend_unique(&mut self.write_roots, &other.write_roots);
    }

    fn intersect(&self, requested: &Self) -> Self {
        let presets = match (&self.presets, &requested.presets) {
            (None, None) => None,
            _ => Some(intersect_presets(
                &self.effective_presets(),
                &requested.effective_presets(),
            )),
        };
        Self {
            presets,
            read_roots: intersect_roots(&self.read_roots, &requested.read_roots),
            write_roots: intersect_roots(&self.write_roots, &requested.write_roots),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CapabilityPolicy {
    pub tools: Vec<String>,
    pub capabilities: BTreeMap<String, Vec<String>>,
    pub workspace_roots: Vec<String>,
    /// Roots the workload may read but never write. A path resolving
    /// under one of these passes read scope checks yet is rejected for
    /// `write_text`/`delete`, and the generated OS sandbox profile
    /// grants it read-only. Intended to be disjoint from
    /// `workspace_roots` (which are read-write); cloud mounts lower
    /// their `FilesystemAccess::ReadOnly` entries here so a "read-only"
    /// mount is actually unwritable inside the sandbox.
    pub read_only_roots: Vec<String>,
    pub side_effect_level: Option<String>,
    /// Remaining Harn-side nested-execution depth. The
    /// `enter_nested_execution_policy` helper validates this at every
    /// `agent_loop`, `sub_agent_run`, `spawn_agent` worker, workflow
    /// stage, and nested-workflow surface: `Some(0)` rejects the
    /// launch with a categorized `BudgetExceeded` error; `Some(n>0)`
    /// allows the descent and gives the child `Some(n - 1)`. `None`
    /// disables the budget gate entirely.
    pub recursion_limit: Option<usize>,
    /// Argument-level constraints for specific tools.
    pub tool_arg_constraints: Vec<ToolArgConstraint>,
    /// Per-tool annotations (kind, arg schema, capabilities, side-effect
    /// level). Pipelines own the registry; the VM reads it.
    pub tool_annotations: BTreeMap<String, ToolAnnotations>,
    /// Confinement strength applied to subprocesses spawned under this
    /// policy. Defaults to [`SandboxProfile::Worktree`]; pipelines opt
    /// into [`SandboxProfile::OsHardened`] when the workload should
    /// refuse to run if the platform sandbox is unavailable.
    pub sandbox_profile: SandboxProfile,
    /// Process-only filesystem allowances layered into OS subprocess
    /// sandboxes without widening Harn file builtins.
    pub process_sandbox: ProcessSandboxPolicy,
}

const DENY_ALL_SENTINEL: &str = "\0harn:deny-all";

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct CapabilityPolicyWire {
    tools: Vec<String>,
    #[serde(skip_serializing_if = "is_false")]
    tools_restricted: bool,
    capabilities: BTreeMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "is_false")]
    capabilities_restricted: bool,
    workspace_roots: Vec<String>,
    read_only_roots: Vec<String>,
    side_effect_level: Option<String>,
    recursion_limit: Option<usize>,
    tool_arg_constraints: Vec<ToolArgConstraint>,
    tool_annotations: BTreeMap<String, ToolAnnotations>,
    sandbox_profile: SandboxProfile,
    process_sandbox: ProcessSandboxPolicy,
}

impl From<&CapabilityPolicy> for CapabilityPolicyWire {
    fn from(policy: &CapabilityPolicy) -> Self {
        let tools_restricted = policy.tools_deny_all();
        let capabilities_restricted = policy.capabilities_deny_all();
        Self {
            tools: if tools_restricted {
                Vec::new()
            } else {
                policy.tools.clone()
            },
            tools_restricted,
            capabilities: if capabilities_restricted {
                BTreeMap::new()
            } else {
                policy.capabilities.clone()
            },
            capabilities_restricted,
            workspace_roots: policy.workspace_roots.clone(),
            read_only_roots: policy.read_only_roots.clone(),
            side_effect_level: policy.side_effect_level.clone(),
            recursion_limit: policy.recursion_limit,
            tool_arg_constraints: policy.tool_arg_constraints.clone(),
            tool_annotations: policy.tool_annotations.clone(),
            sandbox_profile: policy.sandbox_profile,
            process_sandbox: policy.process_sandbox.clone(),
        }
    }
}

impl From<CapabilityPolicyWire> for CapabilityPolicy {
    fn from(wire: CapabilityPolicyWire) -> Self {
        let tools =
            if wire.tools_restricted || wire.tools.iter().any(|tool| tool == DENY_ALL_SENTINEL) {
                encode_restricted_tools(wire.tools)
            } else {
                wire.tools
            };
        let capabilities =
            if wire.capabilities_restricted || wire.capabilities.contains_key(DENY_ALL_SENTINEL) {
                encode_restricted_capabilities(wire.capabilities)
            } else {
                wire.capabilities
            };
        Self {
            tools,
            capabilities,
            workspace_roots: wire.workspace_roots,
            read_only_roots: wire.read_only_roots,
            side_effect_level: wire.side_effect_level,
            recursion_limit: wire.recursion_limit,
            tool_arg_constraints: wire.tool_arg_constraints,
            tool_annotations: wire.tool_annotations,
            sandbox_profile: wire.sandbox_profile,
            process_sandbox: wire.process_sandbox,
        }
    }
}

impl Serialize for CapabilityPolicy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        CapabilityPolicyWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CapabilityPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        CapabilityPolicyWire::deserialize(deserializer).map(Self::from)
    }
}

fn is_false(value: &bool) -> bool {
    !value
}

impl CapabilityPolicy {
    /// The identity element under [`CapabilityPolicy::intersect`]: a policy
    /// that expresses no opinion on any axis.
    ///
    /// Use this — not [`Default::default`] — to build an *overlay*: a policy
    /// that constrains some axes (tools, capabilities, side-effect level) and
    /// deliberately says nothing about the rest. `Default` is the confinement
    /// decision for a run that has none of its own, so it carries
    /// [`SandboxProfile::Worktree`]. An overlay built on `Default` inherits
    /// that profile and asserts filesystem confinement its author never
    /// intended. Merged into a parent the mistake is invisible, because
    /// `intersect` keeps the strictest profile and the parent's is already at
    /// least as strict; with no parent the overlay is pushed verbatim and a
    /// deliberately unsandboxed run silently becomes sandboxed.
    ///
    /// `Unrestricted` is the identity for the profile axis specifically:
    /// `strictest_sandbox_profile` ranks it lowest, so intersecting an overlay
    /// against any real parent yields the parent's profile unchanged.
    pub fn neutral() -> Self {
        Self {
            sandbox_profile: SandboxProfile::Unrestricted,
            ..Self::default()
        }
    }

    pub fn is_unbounded(&self) -> bool {
        self == &Self::default()
    }

    pub fn tools_are_restricted(&self) -> bool {
        !self.tools.is_empty()
    }

    pub fn tools_deny_all(&self) -> bool {
        self.tools.iter().any(|tool| tool == DENY_ALL_SENTINEL)
    }

    pub fn allowed_tool_patterns(&self) -> impl Iterator<Item = &str> {
        self.tools
            .iter()
            .map(String::as_str)
            .filter(|_| !self.tools_deny_all())
    }

    pub fn restrict_tools(&mut self, tools: Vec<String>) {
        self.tools = encode_restricted_tools(tools);
    }

    pub fn tool_pattern_allows(&self, tool: &str) -> bool {
        !self.tools_are_restricted()
            || (!self.tools_deny_all()
                && self.tools.iter().any(|pattern| glob_match(pattern, tool)))
    }

    pub fn capabilities_are_restricted(&self) -> bool {
        !self.capabilities.is_empty()
    }

    pub fn capabilities_deny_all(&self) -> bool {
        self.capabilities.contains_key(DENY_ALL_SENTINEL)
    }

    pub fn allowed_capabilities(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.capabilities
            .iter()
            .filter(|_| !self.capabilities_deny_all())
            .map(|(capability, operations)| (capability.as_str(), operations.as_slice()))
    }

    pub fn capability_operations(&self, capability: &str) -> Option<&[String]> {
        if self.capabilities_deny_all() {
            None
        } else {
            self.capabilities.get(capability).map(Vec::as_slice)
        }
    }

    pub fn restrict_capabilities(&mut self, capabilities: BTreeMap<String, Vec<String>>) {
        self.capabilities = encode_restricted_capabilities(capabilities);
    }

    pub fn intersect(&self, requested: &CapabilityPolicy) -> Result<CapabilityPolicy, String> {
        let side_effect_level = match (&self.side_effect_level, &requested.side_effect_level) {
            (Some(a), Some(b)) => Some(min_side_effect(a, b).to_string()),
            (Some(a), None) => Some(a.clone()),
            (None, Some(b)) => Some(b.clone()),
            (None, None) => None,
        };

        let self_tools_restricted = self.tools_are_restricted();
        let requested_tools_restricted = requested.tools_are_restricted();
        if self_tools_restricted && !requested.tools_deny_all() {
            let denied: Vec<String> = requested
                .tools
                .iter()
                .filter(|tool| tool.as_str() != DENY_ALL_SENTINEL)
                .filter(|tool| {
                    !self
                        .allowed_tool_patterns()
                        .any(|allowed| tool_pattern_within_ceiling(allowed, tool))
                })
                .cloned()
                .collect();
            if !denied.is_empty() {
                return Err(format!(
                    "requested tools exceed host ceiling: {}",
                    denied.join(", ")
                ));
            }
        }

        if !requested.capabilities_deny_all() {
            for (capability, requested_ops) in &requested.capabilities {
                if let Some(allowed_ops) = self.capability_operations(capability) {
                    let denied: Vec<String> = requested_ops
                        .iter()
                        .filter(|op| !allowed_ops.is_empty() && !allowed_ops.contains(*op))
                        .cloned()
                        .collect();
                    if !denied.is_empty() {
                        return Err(format!(
                            "requested capability operations exceed host ceiling: {}.{}",
                            capability,
                            denied.join(",")
                        ));
                    }
                } else if self.capabilities_are_restricted() {
                    return Err(format!(
                        "requested capability exceeds host ceiling: {capability}"
                    ));
                }
            }
        }

        let tools = if !self_tools_restricted {
            requested.tools.clone()
        } else if !requested_tools_restricted {
            self.tools.clone()
        } else if self.tools_deny_all() || requested.tools_deny_all() {
            encode_restricted_tools(Vec::new())
        } else {
            requested
                .tools
                .iter()
                .filter(|tool| {
                    self.allowed_tool_patterns()
                        .any(|allowed| tool_pattern_within_ceiling(allowed, tool))
                })
                .cloned()
                .collect()
        };

        let self_capabilities_restricted = self.capabilities_are_restricted();
        let requested_capabilities_restricted = requested.capabilities_are_restricted();
        let capabilities = if !self_capabilities_restricted {
            requested.capabilities.clone()
        } else if !requested_capabilities_restricted {
            self.capabilities.clone()
        } else if self.capabilities_deny_all() || requested.capabilities_deny_all() {
            encode_restricted_capabilities(BTreeMap::new())
        } else {
            requested
                .capabilities
                .iter()
                .filter_map(|(capability, requested_ops)| {
                    self.capability_operations(capability).map(|allowed_ops| {
                        let operations = if allowed_ops.is_empty() {
                            requested_ops.clone()
                        } else if requested_ops.is_empty() {
                            allowed_ops.to_vec()
                        } else {
                            requested_ops
                                .iter()
                                .filter(|op| allowed_ops.contains(*op))
                                .cloned()
                                .collect()
                        };
                        (capability.clone(), operations)
                    })
                })
                .collect()
        };

        let workspace_roots = intersect_roots(&self.workspace_roots, &requested.workspace_roots);
        let read_only_roots = intersect_roots(&self.read_only_roots, &requested.read_only_roots);

        let recursion_limit = match (self.recursion_limit, requested.recursion_limit) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };

        let mut tool_arg_constraints = self.tool_arg_constraints.clone();
        tool_arg_constraints.extend(requested.tool_arg_constraints.clone());

        let tool_annotations = tools
            .iter()
            .filter_map(|tool| {
                requested
                    .tool_annotations
                    .get(tool)
                    .or_else(|| self.tool_annotations.get(tool))
                    .cloned()
                    .map(|annotations| (tool.clone(), annotations))
            })
            .collect();

        // The sandbox profile composes via "max strictness wins" so
        // intersecting a worktree ceiling with an os_hardened request
        // yields os_hardened (the host gets the stricter of the two).
        let sandbox_profile =
            strictest_sandbox_profile(self.sandbox_profile, requested.sandbox_profile);
        let process_sandbox = self.process_sandbox.intersect(&requested.process_sandbox);

        Ok(CapabilityPolicy {
            tools,
            capabilities,
            workspace_roots,
            read_only_roots,
            side_effect_level,
            recursion_limit,
            tool_arg_constraints,
            tool_annotations,
            sandbox_profile,
            process_sandbox,
        })
    }

    /// Enforce the ceiling invariant on a `requested` policy: every
    /// capability / budget / permission dimension must stay within (or
    /// narrower than) `self`, the workflow-level grant. Used where a
    /// Harn-computed flattened stage policy re-enters Rust: the flattener may
    /// *narrow* a ceiling but must never *widen* one. Returns a categorized
    /// error naming the widened dimension.
    ///
    /// "Empty means unbounded" matches [`intersect`](Self::intersect): an
    /// empty `tools` / `workspace_roots` / `read_only_roots` / `capabilities`
    /// ceiling imposes no bound on that dimension, so anything passes.
    pub fn assert_within_ceiling(&self, requested: &CapabilityPolicy) -> Result<(), String> {
        if self.tools_are_restricted() {
            if !requested.tools_are_restricted() {
                return Err("flattened stage policy dropped the stage tool ceiling".to_string());
            }
            if requested.tools_deny_all() {
                // An explicit deny-all is always a valid narrowing, including
                // malformed marker-plus-grant inputs which fail closed.
            } else if self.tools_deny_all() {
                return Err("flattened stage policy widened a deny-all tool ceiling".to_string());
            } else {
                let widened: Vec<String> = requested
                    .tools
                    .iter()
                    .filter(|tool| tool.as_str() != DENY_ALL_SENTINEL)
                    .filter(|tool| {
                        !self
                            .allowed_tool_patterns()
                            .any(|allowed| tool_pattern_within_ceiling(allowed, tool))
                    })
                    .cloned()
                    .collect();
                if !widened.is_empty() {
                    return Err(format!(
                        "flattened stage policy widened tools beyond the stage grant: {}",
                        widened.join(", ")
                    ));
                }
            }
        }

        if self.capabilities_are_restricted() && !requested.capabilities_are_restricted() {
            return Err("flattened stage policy dropped the stage capability ceiling".to_string());
        }
        if !requested.capabilities_deny_all() {
            for (capability, requested_ops) in &requested.capabilities {
                match self.capability_operations(capability) {
                    Some(allowed_ops) => {
                        if requested_ops.is_empty() && !allowed_ops.is_empty() {
                            return Err(format!(
                                "flattened stage policy widened capability `{capability}` to every operation"
                            ));
                        }
                        let widened: Vec<String> = requested_ops
                            .iter()
                            .filter(|op| !allowed_ops.is_empty() && !allowed_ops.contains(*op))
                            .cloned()
                            .collect();
                        if !widened.is_empty() {
                            return Err(format!(
                                "flattened stage policy widened capability `{capability}` beyond the stage grant: {}",
                                widened.join(",")
                            ));
                        }
                    }
                    None if self.capabilities_are_restricted() => {
                        return Err(format!(
                            "flattened stage policy added capability `{capability}` beyond the stage grant"
                        ));
                    }
                    None => {}
                }
            }
        }

        for (label, ceiling_roots, requested_roots) in [
            (
                "workspace_roots",
                &self.workspace_roots,
                &requested.workspace_roots,
            ),
            (
                "read_only_roots",
                &self.read_only_roots,
                &requested.read_only_roots,
            ),
        ] {
            if !ceiling_roots.is_empty() {
                let widened: Vec<String> = requested_roots
                    .iter()
                    .filter(|root| !ceiling_roots.contains(*root))
                    .cloned()
                    .collect();
                if !widened.is_empty() {
                    return Err(format!(
                        "flattened stage policy widened {label} beyond the stage grant: {}",
                        widened.join(", ")
                    ));
                }
            }
        }

        // Recursion budget can only be narrowed: a lower ceiling forbids a
        // higher request, and once the stage carries a budget the flattener
        // may not drop it (which would disable the nested-execution gate).
        if let Some(ceiling_limit) = self.recursion_limit {
            match requested.recursion_limit {
                Some(requested_limit) if requested_limit <= ceiling_limit => {}
                Some(requested_limit) => {
                    return Err(format!(
                        "flattened stage policy widened the recursion budget beyond the stage grant: {requested_limit} > {ceiling_limit}"
                    ));
                }
                None => {
                    return Err(
                        "flattened stage policy dropped the recursion budget granted by the stage"
                            .to_string(),
                    );
                }
            }
        }

        // Side-effect level can only be narrowed (a lower rank). Once the
        // stage carries a ceiling the flattener may not remove it. Rank through
        // the canonical `SideEffectLevel` ladder (fail-closed: an unknown value
        // ranks as `none`/0, and the ladder grows at the top so a future level
        // above `desktop_control` can never sneak under a lesser ceiling).
        use crate::tool_annotations::SideEffectLevel;
        if let Some(ceiling_level) = &self.side_effect_level {
            match &requested.side_effect_level {
                Some(requested_level)
                    if SideEffectLevel::rank_str(requested_level)
                        <= SideEffectLevel::rank_str(ceiling_level) => {}
                Some(requested_level) => {
                    return Err(format!(
                        "flattened stage policy widened the side-effect level beyond the stage grant: {requested_level} > {ceiling_level}"
                    ));
                }
                None => {
                    return Err(
                        "flattened stage policy dropped the side-effect ceiling granted by the stage"
                            .to_string(),
                    );
                }
            }
        }

        // Sandbox confinement can only stay the same or grow stricter.
        if sandbox_profile_strictness(requested.sandbox_profile)
            < sandbox_profile_strictness(self.sandbox_profile)
        {
            return Err(
                "flattened stage policy loosened the sandbox profile below the stage grant"
                    .to_string(),
            );
        }

        // Process-sandbox filesystem grants (subprocess host-FS read/write
        // OUTSIDE the workspace) narrow via `intersect_roots` / `intersect_presets`;
        // re-check the returned policy did not add any. Unlike the workspace /
        // read-only roots above, these are ADDITIVE grants that
        // `normalized_process_roots` maps empty→empty with no fallback: empty
        // roots = ZERO extra subprocess FS access = MOST restrictive, NOT
        // unbounded. So the subset check is UNCONDITIONAL — an empty ceiling
        // must reject any non-empty requested roots (the common default-stage
        // case a `!ceiling_roots.is_empty()` guard would silently wave through).
        let ceiling_ps = &self.process_sandbox;
        let requested_ps = &requested.process_sandbox;
        for (label, ceiling_roots, requested_roots) in [
            (
                "process_sandbox.read_roots",
                &ceiling_ps.read_roots,
                &requested_ps.read_roots,
            ),
            (
                "process_sandbox.write_roots",
                &ceiling_ps.write_roots,
                &requested_ps.write_roots,
            ),
        ] {
            let widened: Vec<String> = requested_roots
                .iter()
                .filter(|root| !ceiling_roots.contains(*root))
                .cloned()
                .collect();
            if !widened.is_empty() {
                return Err(format!(
                    "flattened stage policy widened {label} beyond the stage grant: {}",
                    widened.join(", ")
                ));
            }
        }
        // Presets compose through `effective_presets()` (None resolves to the
        // defaults), so compare the effective sets: any preset the request would
        // enable that the ceiling does not is a widening.
        let ceiling_presets = ceiling_ps.effective_presets();
        let widened_presets: Vec<String> = requested_ps
            .effective_presets()
            .into_iter()
            .filter(|preset| !ceiling_presets.contains(preset))
            .map(|preset| format!("{preset:?}"))
            .collect();
        if !widened_presets.is_empty() {
            return Err(format!(
                "flattened stage policy widened process_sandbox presets beyond the stage grant: {}",
                widened_presets.join(", ")
            ));
        }

        // Argument-level constraints (e.g. edit scoped to `src/**`) compose by
        // union in `intersect`, so more constraints = stricter. Dropping one the
        // stage granted widens "scoped" back to "anywhere". Require every ceiling
        // constraint to survive in the returned policy (adding more is fine).
        let dropped_constraints: Vec<String> = self
            .tool_arg_constraints
            .iter()
            .filter(|constraint| !requested.tool_arg_constraints.contains(constraint))
            .map(|constraint| constraint.tool.clone())
            .collect();
        if !dropped_constraints.is_empty() {
            return Err(format!(
                "flattened stage policy dropped tool_arg_constraints granted by the stage: {}",
                dropped_constraints.join(", ")
            ));
        }

        // Tool annotations drive constraint resolution (`arg_schema.path_params`)
        // and per-tool side-effect/capability classification. Dropping or
        // rewriting the annotation for a still-granted tool widens it (a lost
        // `path_params` makes a path constraint unresolvable → permissive; a
        // lowered `side_effect_level` slips past the effect ceiling). For every
        // tool the ceiling still grants, its annotation must survive unchanged.
        let tool_still_granted = |tool: &str| {
            !requested.tools_are_restricted()
                || (!requested.tools_deny_all()
                    && requested.tools.iter().any(|granted| granted == tool))
        };
        for (tool, annotation) in &self.tool_annotations {
            if !tool_still_granted(tool) {
                continue;
            }
            match requested.tool_annotations.get(tool) {
                Some(requested_annotation) if requested_annotation == annotation => {}
                Some(_) => {
                    return Err(format!(
                        "flattened stage policy rewrote tool_annotations for `{tool}` (annotations are authority metadata and may not be weakened by the flattener)"
                    ));
                }
                None => {
                    return Err(format!(
                        "flattened stage policy dropped tool_annotations for still-granted tool `{tool}`"
                    ));
                }
            }
        }

        Ok(())
    }
}

/// Intersect two root allowlists with the same "empty means unbounded"
/// convention the rest of `intersect` uses: an empty list on either side
/// is treated as "no ceiling on this dimension", so the other side passes
/// through untouched. When both are populated the result keeps only the
/// roots present in both.
fn encode_restricted_tools(tools: Vec<String>) -> Vec<String> {
    if tools.is_empty() || tools.iter().any(|tool| tool == DENY_ALL_SENTINEL) {
        vec![DENY_ALL_SENTINEL.to_string()]
    } else {
        tools
    }
}

fn tool_pattern_within_ceiling(ceiling: &str, requested: &str) -> bool {
    ceiling == "*"
        || ceiling == requested
        || (!requested
            .chars()
            .any(|character| matches!(character, '*' | '?' | '[' | ']' | '{' | '}'))
            && glob_match(ceiling, requested))
}

fn encode_restricted_capabilities(
    mut capabilities: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, Vec<String>> {
    if capabilities.is_empty() || capabilities.contains_key(DENY_ALL_SENTINEL) {
        capabilities.clear();
        capabilities.insert(DENY_ALL_SENTINEL.to_string(), Vec::new());
    }
    capabilities
}

/// Clamp `requested` filesystem roots to what `host` already allows.
///
/// Roots name directory trees, so containment — not string equality — decides
/// whether a request stays inside the ceiling. A request nested under a host
/// root is a narrowing and survives as itself; a request that contains a host
/// root is a widening and is clamped to the host root. Comparing the strings
/// instead would drop both, and an empty root list means "fall back to the
/// execution root", so the narrowing a caller asked for would silently come
/// back *wider* than either side intended.
fn intersect_roots(host: &[String], requested: &[String]) -> Vec<String> {
    if host.is_empty() {
        return requested.to_vec();
    }
    if requested.is_empty() {
        return host.to_vec();
    }
    let mut roots: Vec<String> = Vec::new();
    for root in requested {
        if host.iter().any(|allowed| root_is_within(root, allowed)) {
            roots.push(root.clone());
        } else {
            roots.extend(
                host.iter()
                    .filter(|allowed| root_is_within(allowed, root))
                    .cloned(),
            );
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

/// Whether `candidate` names the same tree as `root` or one nested inside it.
///
/// Compared component-wise so a sibling whose name merely starts with the
/// root's (`/repo-backup` against `/repo`) is not mistaken for a child.
fn root_is_within(candidate: &str, root: &str) -> bool {
    Path::new(candidate).starts_with(Path::new(root))
}

/// Rank within the confinement ladder, weakest first. Exhaustive so that
/// a new profile cannot be added without being placed on the ladder —
/// [`CapabilityPolicy::intersect`] takes the stricter of two profiles, and
/// an unranked variant would silently intersect as the weakest.
fn sandbox_profile_strictness(profile: SandboxProfile) -> u8 {
    match profile {
        SandboxProfile::Unrestricted => 0,
        SandboxProfile::WorkspacePaths => 1,
        SandboxProfile::Worktree => 2,
        SandboxProfile::Wasi => 3,
        SandboxProfile::OsHardened => 4,
    }
}

fn strictest_sandbox_profile(left: SandboxProfile, right: SandboxProfile) -> SandboxProfile {
    if sandbox_profile_strictness(left) >= sandbox_profile_strictness(right) {
        left
    } else {
        right
    }
}

fn intersect_presets(
    left: &[ProcessSandboxPreset],
    right: &[ProcessSandboxPreset],
) -> Vec<ProcessSandboxPreset> {
    left.iter()
        .filter(|preset| right.contains(preset))
        .copied()
        .collect()
}

fn extend_unique(target: &mut Vec<String>, roots: &[String]) {
    for root in roots {
        if !target.contains(root) {
            target.push(root.clone());
        }
    }
}

fn min_side_effect<'a>(a: &'a str, b: &'a str) -> &'a str {
    fn rank(v: &str) -> usize {
        crate::tool_annotations::SideEffectLevel::rank_str(v)
    }
    if rank(a) <= rank(b) {
        a
    } else {
        b
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TurnPolicy {
    /// When true, text-only responses in a tool-capable stage are treated as
    /// invalid unless they switch phase / finish the stage. This keeps action
    /// stages moving instead of drifting into narration.
    pub require_action_or_yield: bool,
    /// When false, workflow-owned action stages should hand control back via
    /// successful tool calls instead of advertising an additional done
    /// sentinel pathway in corrective nudges.
    #[serde(default = "default_true")]
    pub allow_done_sentinel: bool,
    /// Optional visible prose budget for a single assistant turn. When the
    /// assistant exceeds it, the recorded transcript keeps only a shortened
    /// version and the next corrective nudge reminds the model to stay brief.
    pub max_prose_chars: Option<usize>,
}

impl Default for TurnPolicy {
    fn default() -> Self {
        Self {
            require_action_or_yield: false,
            allow_done_sentinel: true,
            max_prose_chars: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NativeToolFallbackPolicy {
    Allow,
    AllowOnce,
    #[default]
    Reject,
}

impl NativeToolFallbackPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::AllowOnce => "allow_once",
            Self::Reject => "reject",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "allow" => Some(Self::Allow),
            "allow_once" => Some(Self::AllowOnce),
            "reject" => Some(Self::Reject),
            _ => None,
        }
    }
}

fn default_true() -> bool {
    true
}

/// One `require_successful_tools` clause. Every outer clause is mandatory;
/// `AnyOf` is an alternative group where one successful tool satisfies the
/// clause. This matches the agent-loop contract's `list<string|list<string>>`
/// shape without weakening workflow graph validation to untyped JSON.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RequiredSuccessfulTool {
    Tool(String),
    AnyOf(Vec<String>),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct ModelPolicy {
    pub provider: Option<String>,
    pub model: Option<String>,
    pub model_tier: Option<String>,
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    /// Maximum agent_loop iterations for this stage. Overrides the default 16.
    /// Static cap; prefer `iteration_budget` so the per-stage `agent_loop`
    /// receives Harn's adaptive `loop_control` policy and emits
    /// `loop_control_decision` events on extension/stop. Both fields are
    /// passed through to `agent_loop`; if both are present, the budget's
    /// `max` wins because `agent_loop` reads `iteration_budget` first.
    pub max_iterations: Option<usize>,
    /// Adaptive iteration budget for this stage. Surfaced on the per-stage
    /// `agent_loop` so the runtime emits `loop_control_decision` events
    /// when extending or stopping the loop. Pipelines author this through
    /// `agent_preset(...)` from `std/agent/presets`,
    /// or as a literal dict like `{mode: "adaptive", initial: 4, max: 16, extend_by: 2}`.
    /// Stored as a free-form JSON value so the std/agent budget shapes
    /// can evolve without churn here; `__normalize_iteration_budget` in
    /// `std/agent/options` does the per-call validation.
    pub iteration_budget: Option<serde_json::Value>,
    /// Maximum consecutive text-only (no tool call) responses before declaring stuck.
    pub max_nudges: Option<usize>,
    /// Custom nudge message injected when the model produces text without tool calls.
    /// If omitted, the VM uses a generic "Continue — use a tool call" message.
    pub nudge: Option<String>,
    /// Few-shot tool-call examples injected into the tool contract prompt,
    /// shown before the tool schema listing. Pipelines provide these —
    /// the VM has no hardcoded tool names.
    pub tool_examples: Option<String>,
    /// Optional Harn closure called after each tool-calling turn.
    /// Receives turn metadata; returns either a string user message to inject,
    /// a bool stop flag, or a dict like {message, stop}.
    /// Wrapped in EqIgnored so it doesn't affect PartialEq derivation.
    #[serde(skip)]
    pub post_turn_callback: Option<EqIgnored<VmValue>>,
    /// When set, the stage stops after any tool-calling turn whose successful
    /// results include one of these tool names. This is useful for
    /// workflow-owned verify loops where a productive write turn should hand
    /// control back to verification immediately.
    pub stop_after_successful_tools: Option<Vec<String>>,
    /// When set, the stage is reported as failed unless every outer clause is
    /// satisfied. A string requires that exact tool; a nested list is an OR
    /// group where any one member may succeed.
    pub require_successful_tools: Option<Vec<RequiredSuccessfulTool>>,
    /// Turn-shape constraints for action stages.
    pub turn_policy: Option<TurnPolicy>,
    /// Tool-calling contract format for the per-stage agent loop.
    /// `"native"` routes user tools through the provider's native
    /// function-call channel; `"text"` embeds the `<tool_call>` contract
    /// prompt. Mirrors the top-level `agent_loop(..., {tool_format: ...})`
    /// option. When `None`, std/workflow/options.harn composes the fallback
    /// from host-provided env/default facts.
    pub tool_format: Option<String>,
    /// Policy for native-tool stages when a provider emits text-mode
    /// `<tool_call>` output instead of native tool calls.
    pub native_tool_fallback: NativeToolFallbackPolicy,
}

/// Wrapper that always compares equal, allowing non-Eq types in derived PartialEq structs.
#[derive(Clone, Debug, Default)]
pub struct EqIgnored<T>(pub T);

impl<T> PartialEq for EqIgnored<T> {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl<T> std::ops::Deref for EqIgnored<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

/// Per-call auto-compaction settings. Lifecycle is driven by explicit
/// `agent_session_*` builtins; this policy only controls how the agent loop
/// manages its context window.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AutoCompactPolicy {
    /// Enable per-turn auto-compaction within agent loops.
    pub enabled: bool,
    /// Token threshold for tier-1 compaction.
    pub token_threshold: Option<usize>,
    /// Max chars per tool result before compression.
    pub tool_output_max_chars: Option<usize>,
    /// Tier-1 compaction strategy name (e.g., "observation_mask", "llm").
    pub compact_strategy: Option<String>,
    /// Token threshold for tier-2 aggressive compaction.
    pub hard_limit_tokens: Option<usize>,
    /// Tier-2 compaction strategy name.
    pub hard_limit_strategy: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextPolicy {
    pub max_artifacts: Option<usize>,
    pub max_tokens: Option<usize>,
    pub reserve_tokens: Option<usize>,
    pub include_kinds: Vec<String>,
    pub exclude_kinds: Vec<String>,
    pub prioritize_kinds: Vec<String>,
    pub pinned_ids: Vec<String>,
    pub include_stages: Vec<String>,
    pub prefer_recent: bool,
    pub prefer_fresh: bool,
    pub render: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct RetryPolicy {
    pub max_attempts: usize,
    pub verify: bool,
    pub repair: bool,
    /// Initial backoff duration in milliseconds between retry attempts.
    /// When `None`, retries proceed without delay.
    #[serde(default)]
    pub backoff_ms: Option<u64>,
    /// Multiplier applied to `backoff_ms` after each retry attempt.
    /// Defaults to 2.0 when `backoff_ms` is set and this field is `None`.
    #[serde(default)]
    pub backoff_multiplier: Option<f64>,
    /// Retry-with-feedback: when set, a failed attempt's verification findings
    /// are threaded into the next attempt's task via a bounded default
    /// template. `feedback: true` uses the default budget; `feedback:
    /// {max_chars}` bounds the injected findings. `None` = today's blind
    /// retry (byte-identical replay). Interpreted by the embedded stage loop
    /// in `std/workflow/stage.harn`, not by Rust.
    ///
    /// `skip_serializing_if` keeps the unset default OUT of serialized
    /// graphs so pre-existing `WorkflowBundle` graph digests (pinned in
    /// docs/src/workflow-authoring-quickstart.md and enforced by
    /// scripts/check_docs_workflow_quickstart.harn) stay byte-stable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<FeedbackPolicy>,
    /// Retry-with-feedback: an optional Harn closure that receives the full
    /// retry context (`{task, attempt, findings, verification, error,
    /// prior_text, stage}`) and returns the replacement task for the next
    /// attempt. Carries a closure, so it is `#[serde(skip)]` (absent from the
    /// serialized default) and travels to the embedded stage loop as a raw
    /// value — Rust never invokes it. Wrapped in `EqIgnored` so it does not
    /// affect `PartialEq` derivation.
    #[serde(skip)]
    pub repair_prompt_builder: Option<EqIgnored<VmValue>>,
}

/// Retry-with-feedback bounding for `RetryPolicy.feedback`. `true` enables
/// the default-budget template; `{max_chars}` bounds the injected findings.
/// Untagged so `feedback: true` and `feedback: {max_chars: 500}` both parse.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum FeedbackPolicy {
    Enabled(bool),
    Bounded(FeedbackBounds),
}

/// Bounded feedback budget shape (`feedback: {max_chars}`).
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct FeedbackBounds {
    /// Maximum characters of prior-attempt findings to inject into the retry
    /// task. Defaults (in the stage loop) to ~2000 when unset.
    pub max_chars: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct StageContract {
    pub input_kinds: Vec<String>,
    pub output_kinds: Vec<String>,
    pub min_inputs: Option<usize>,
    pub max_inputs: Option<usize>,
    pub require_transcript: bool,
    pub schema: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BranchSemantics {
    pub success: Option<String>,
    pub failure: Option<String>,
    pub verify_pass: Option<String>,
    pub verify_fail: Option<String>,
    pub condition_true: Option<String>,
    pub condition_false: Option<String>,
    pub loop_continue: Option<String>,
    pub loop_exit: Option<String>,
    pub escalation: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct MapPolicy {
    pub items: Vec<serde_json::Value>,
    pub item_artifact_kind: Option<String>,
    pub output_kind: Option<String>,
    pub max_items: Option<usize>,
    pub max_concurrent: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct JoinPolicy {
    pub strategy: String,
    pub require_all_inputs: bool,
    pub min_completed: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ReducePolicy {
    pub strategy: String,
    pub separator: Option<String>,
    pub output_kind: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct EscalationPolicy {
    pub level: Option<String>,
    pub queue: Option<String>,
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests;
