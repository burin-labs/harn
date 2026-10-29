//! Capability ceilings and sandbox authority, independent of execution state.

mod process_sandbox;

/// The credential denylist: the credential home paths of the shared registry.
pub use crate::known_paths::credential_home_paths as default_read_deny_home_paths;
pub use process_sandbox::{
    LoopbackEnforcement, ProcessNetworkProxy, ProcessSandboxPolicy, ProcessSandboxPreset,
    UnixSocketEnforcement,
};

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::capability_lattice::operation_is_covered;
use crate::tool_annotations::ToolAnnotations;
use harn_glob::match_name as glob_match;

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
    /// mechanism (Linux Landlock+seccomp, macOS sandbox-exec) is
    /// unavailable or does not confine what the policy requires, regardless
    /// of `HARN_HANDLER_SANDBOX`. Windows has no mechanism, so it always
    /// refuses there.
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
            SandboxProfile::Wasi,
            SandboxProfile::Worktree,
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
    /// sandbox-exec) is applied to subprocesses spawned under this policy,
    /// where the platform has one.
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
    ///
    /// Boxed so adding a grant (socket roots, another denylist) does not
    /// grow every stack frame that carries a `CapabilityPolicy` by value.
    pub process_sandbox: Box<ProcessSandboxPolicy>,
    /// Managed proxy endpoints installed by the host for child traffic.
    /// `None` preserves the existing deny-all/unrestricted socket ceiling
    /// selected by `side_effect_level`.
    pub process_network_proxy: Option<ProcessNetworkProxy>,
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
    // Host-runtime transport state must never enter a serialized/nested
    // policy request: an untrusted requester could otherwise name an
    // arbitrary loopback service as its "proxy" and make the OS profile grant
    // that port. The outer host installs this only after binding the proxy.
    #[serde(skip)]
    process_network_proxy: Option<ProcessNetworkProxy>,
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
            process_sandbox: (*policy.process_sandbox).clone(),
            process_network_proxy: policy.process_network_proxy,
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
            process_sandbox: Box::new(wire.process_sandbox),
            process_network_proxy: wire.process_network_proxy,
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

pub(super) fn is_false(value: &bool) -> bool {
    !value
}

impl CapabilityPolicy {
    /// Whether the side-effect ceiling admits ordinary network operations.
    pub fn allows_network(&self) -> bool {
        use crate::tool_annotations::SideEffectLevel;
        self.side_effect_level
            .as_ref()
            .map(|level| SideEffectLevel::rank_str(level) >= SideEffectLevel::Network.rank())
            .unwrap_or(true)
    }

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

    /// Whether a child process spawned under this policy may write its
    /// writable roots: the workspace roots, the process-only `write_roots`,
    /// the session's temp dir, and the toolchain caches.
    ///
    /// The one owner of that decision for every OS backend. It is true when
    /// the `workspace` capability grants a write (or capabilities are
    /// unrestricted), or when the process sandbox grants
    /// [`ProcessSandboxPolicy::allow_child_workspace_write`]. It never decides
    /// what Harn's own file builtins may write; those read the capability
    /// alone.
    pub fn children_may_write(&self) -> bool {
        self.process_sandbox.allow_child_workspace_write
            || !self.capabilities_are_restricted()
            || self
                .capability_operations("workspace")
                .is_some_and(|ops| ops.iter().any(|op| op == "write_text" || op == "delete"))
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
                        .filter(|op| {
                            !allowed_ops.is_empty()
                                && !allowed_ops
                                    .iter()
                                    .any(|allowed| operation_is_covered(capability, allowed, op))
                        })
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
                                .filter(|op| {
                                    allowed_ops.iter().any(|allowed| {
                                        operation_is_covered(capability, allowed, op)
                                    })
                                })
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
        let process_sandbox = self
            .process_sandbox
            .intersect(&requested.process_sandbox, self.children_may_write());
        // Only the outer/ceiling policy may contribute transport endpoints.
        // Never inherit them from a nested request: naming a loopback port is
        // authority because the OS sandbox will grant that exact destination.
        let process_network_proxy = self.process_network_proxy;

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
            process_sandbox: Box::new(process_sandbox),
            process_network_proxy,
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
                            .filter(|op| {
                                !allowed_ops.is_empty()
                                    && !allowed_ops.iter().any(|allowed| {
                                        operation_is_covered(capability, allowed, op)
                                    })
                            })
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
        if requested_ps.ssh_agent_socket.is_some()
            && requested_ps.ssh_agent_socket != ceiling_ps.ssh_agent_socket
        {
            return Err("flattened stage policy widened the SSH agent socket grant".to_string());
        }
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
            (
                "process_sandbox.unix_socket_roots",
                &ceiling_ps.unix_socket_roots,
                &requested_ps.unix_socket_roots,
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
        let dropped_read_denials = ceiling_ps
            .read_deny_roots
            .iter()
            .filter(|root| !requested_ps.read_deny_roots.contains(root))
            .collect::<Vec<_>>();
        if !dropped_read_denials.is_empty() {
            return Err(format!(
                "flattened stage policy dropped process_sandbox read denials: {dropped_read_denials:?}"
            ));
        }
        if requested_ps.netns_launcher_path != ceiling_ps.netns_launcher_path {
            return Err(
                "flattened stage policy changed the host-owned process_sandbox namespace helper"
                    .to_string(),
            );
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
        if requested_ps.allow_tcp_loopback && !ceiling_ps.allow_tcp_loopback {
            return Err(
                "flattened stage policy enabled TCP loopback beyond the stage grant".to_string(),
            );
        }
        if requested_ps.allow_process_self_introspection
            && !ceiling_ps.allow_process_self_introspection
        {
            return Err(
                "flattened stage policy enabled process self-introspection beyond the stage grant"
                    .to_string(),
            );
        }
        if requested_ps.allow_child_workspace_write && !self.children_may_write() {
            return Err(
                "flattened stage policy let child processes write beyond the stage grant"
                    .to_string(),
            );
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
pub(super) fn intersect_roots(host: &[String], requested: &[String]) -> Vec<String> {
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
        SandboxProfile::Wasi => 2,
        SandboxProfile::Worktree => 3,
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

pub(super) fn intersect_presets(
    left: &[ProcessSandboxPreset],
    right: &[ProcessSandboxPreset],
) -> Vec<ProcessSandboxPreset> {
    left.iter()
        .filter(|preset| right.contains(preset))
        .copied()
        .collect()
}

pub(super) fn extend_unique(target: &mut Vec<String>, roots: &[String]) {
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

#[cfg(test)]
mod tests;
