//! Typed authority and effect contracts for Harn builtins.
//!
//! This dependency-leaf model is the semantic owner for which script surface
//! may reach a builtin and what that call can do. Runtime handler pointers stay
//! in `harn-vm`; parser, IR, policy, hostlib, and documentation consumers can
//! all depend on this crate without reversing the workspace dependency graph.

/// Where a builtin is visible to Harn source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinExposure {
    /// Contract has not been declared yet. Production registries must reject
    /// this value; it exists only to make migration failures precise.
    Undeclared,
    /// Pure computation available as an ordinary global function.
    PureGlobal,
    /// Imported operation whose authority is carried by one explicit,
    /// unforgeable argument derived from `Harness`. Importing the symbol
    /// itself grants no authority.
    CapabilityFunction { authority_argument: u16 },
    /// Effectful operation available only through a typed harness handle.
    HarnessMethod {
        capability: CapabilityId,
        method: &'static str,
    },
    /// Trusted embedder wire primitive. User modules cannot name or re-export
    /// it; only artifacts stamped with privileged provenance may call it.
    PrivilegedWire,
    /// Pure primitive exposed only while compiling Harn's embedded stdlib.
    /// Public stdlib functions may wrap it, but ordinary source cannot call or
    /// re-export the primitive itself.
    StdlibInternal,
    /// Compiler/runtime implementation detail that is never source-visible.
    RuntimeInternal,
}

/// Closed vocabulary of capability handles exposed by `Harness`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityId {
    Stdio,
    Term,
    Clock,
    Fs,
    Env,
    Random,
    Net,
    Process,
    Channels,
    System,
    Secrets,
    Llm,
    Agent,
    Tenant,
    Auth,
    Observability,
    Verdict,
    Tools,
    Ast,
    CodeIndex,
    Computer,
    Embed,
    Memory,
    Sqlite,
    Postgres,
    FsWatch,
    HostLease,
    Scanner,
    SecretStore,
    TerminalSession,
    Rules,
    Lint,
    Runtime,
    Interaction,
    Project,
    Dashboard,
    Workspace,
    MergeCaptain,
    Session,
    Permission,
    Text,
    Lsp,
    Credentials,
    PrMonitor,
    Workflow,
    Testing,
}

impl CapabilityId {
    /// Rust enum variant spelling for generated contract expressions.
    pub const fn variant_name(self) -> &'static str {
        match self {
            Self::Stdio => "Stdio",
            Self::Term => "Term",
            Self::Clock => "Clock",
            Self::Fs => "Fs",
            Self::Env => "Env",
            Self::Random => "Random",
            Self::Net => "Net",
            Self::Process => "Process",
            Self::Channels => "Channels",
            Self::System => "System",
            Self::Secrets => "Secrets",
            Self::Llm => "Llm",
            Self::Agent => "Agent",
            Self::Tenant => "Tenant",
            Self::Auth => "Auth",
            Self::Observability => "Observability",
            Self::Verdict => "Verdict",
            Self::Tools => "Tools",
            Self::Ast => "Ast",
            Self::CodeIndex => "CodeIndex",
            Self::Computer => "Computer",
            Self::Embed => "Embed",
            Self::Memory => "Memory",
            Self::Sqlite => "Sqlite",
            Self::Postgres => "Postgres",
            Self::FsWatch => "FsWatch",
            Self::HostLease => "HostLease",
            Self::Scanner => "Scanner",
            Self::SecretStore => "SecretStore",
            Self::TerminalSession => "TerminalSession",
            Self::Rules => "Rules",
            Self::Lint => "Lint",
            Self::Runtime => "Runtime",
            Self::Interaction => "Interaction",
            Self::Project => "Project",
            Self::Dashboard => "Dashboard",
            Self::Workspace => "Workspace",
            Self::MergeCaptain => "MergeCaptain",
            Self::Session => "Session",
            Self::Permission => "Permission",
            Self::Text => "Text",
            Self::Lsp => "Lsp",
            Self::Credentials => "Credentials",
            Self::PrMonitor => "PrMonitor",
            Self::Workflow => "Workflow",
            Self::Testing => "Testing",
        }
    }

    /// Every capability in canonical root-field order.
    pub const ALL: &'static [Self] = &[
        Self::Stdio,
        Self::Term,
        Self::Clock,
        Self::Fs,
        Self::Env,
        Self::Random,
        Self::Net,
        Self::Process,
        Self::Channels,
        Self::System,
        Self::Secrets,
        Self::Llm,
        Self::Agent,
        Self::Tenant,
        Self::Auth,
        Self::Observability,
        Self::Verdict,
        Self::Tools,
        Self::Ast,
        Self::CodeIndex,
        Self::Computer,
        Self::Embed,
        Self::Memory,
        Self::Sqlite,
        Self::Postgres,
        Self::FsWatch,
        Self::HostLease,
        Self::Scanner,
        Self::SecretStore,
        Self::TerminalSession,
        Self::Rules,
        Self::Lint,
        Self::Runtime,
        Self::Interaction,
        Self::Project,
        Self::Dashboard,
        Self::Workspace,
        Self::MergeCaptain,
        Self::Session,
        Self::Permission,
        Self::Text,
        Self::Lsp,
        Self::Credentials,
        Self::PrMonitor,
        Self::Workflow,
        Self::Testing,
    ];

    /// Canonical source-level `harness.<field>` name.
    pub const fn field_name(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Term => "term",
            Self::Clock => "clock",
            Self::Fs => "fs",
            Self::Env => "env",
            Self::Random => "random",
            Self::Net => "net",
            Self::Process => "process",
            Self::Channels => "channels",
            Self::System => "system",
            Self::Secrets => "secrets",
            Self::Llm => "llm",
            Self::Agent => "agent",
            Self::Tenant => "tenant",
            Self::Auth => "auth",
            Self::Observability => "obs",
            Self::Verdict => "verdict",
            Self::Tools => "tools",
            Self::Ast => "ast",
            Self::CodeIndex => "code_index",
            Self::Computer => "computer",
            Self::Embed => "embed",
            Self::Memory => "memory",
            Self::Sqlite => "sqlite",
            Self::Postgres => "postgres",
            Self::FsWatch => "fs_watch",
            Self::HostLease => "host_lease",
            Self::Scanner => "scanner",
            Self::SecretStore => "secret_store",
            Self::TerminalSession => "terminal",
            Self::Rules => "rules",
            Self::Lint => "lint",
            Self::Runtime => "runtime",
            Self::Interaction => "interaction",
            Self::Project => "project",
            Self::Dashboard => "dashboard",
            Self::Workspace => "workspace",
            Self::MergeCaptain => "merge_captain",
            Self::Session => "session",
            Self::Permission => "permission",
            Self::Text => "text",
            Self::Lsp => "lsp",
            Self::Credentials => "credentials",
            Self::PrMonitor => "pr_monitor",
            Self::Workflow => "workflow",
            Self::Testing => "testing",
        }
    }

    /// Nominal source type carried by this capability handle.
    pub const fn type_name(self) -> &'static str {
        match self {
            Self::Stdio => "HarnessStdio",
            Self::Term => "HarnessTerm",
            Self::Clock => "HarnessClock",
            Self::Fs => "HarnessFs",
            Self::Env => "HarnessEnv",
            Self::Random => "HarnessRandom",
            Self::Net => "HarnessNet",
            Self::Process => "HarnessProcess",
            Self::Channels => "HarnessChannels",
            Self::System => "HarnessSystem",
            Self::Secrets => "HarnessSecrets",
            Self::Llm => "HarnessLlm",
            Self::Agent => "HarnessAgent",
            Self::Tenant => "HarnessTenant",
            Self::Auth => "HarnessAuth",
            Self::Observability => "HarnessObs",
            Self::Verdict => "HarnessVerdict",
            Self::Tools => "HarnessTools",
            Self::Ast => "HarnessAst",
            Self::CodeIndex => "HarnessCodeIndex",
            Self::Computer => "HarnessComputer",
            Self::Embed => "HarnessEmbed",
            Self::Memory => "HarnessMemory",
            Self::Sqlite => "HarnessSqlite",
            Self::Postgres => "HarnessPostgres",
            Self::FsWatch => "HarnessFsWatch",
            Self::HostLease => "HarnessHostLease",
            Self::Scanner => "HarnessScanner",
            Self::SecretStore => "HarnessSecretStore",
            Self::TerminalSession => "HarnessTerminalSession",
            Self::Rules => "HarnessRules",
            Self::Lint => "HarnessLint",
            Self::Runtime => "HarnessRuntime",
            Self::Interaction => "HarnessInteraction",
            Self::Project => "HarnessProject",
            Self::Dashboard => "HarnessDashboard",
            Self::Workspace => "HarnessWorkspace",
            Self::MergeCaptain => "HarnessMergeCaptain",
            Self::Session => "HarnessSession",
            Self::Permission => "HarnessPermission",
            Self::Text => "HarnessText",
            Self::Lsp => "HarnessLsp",
            Self::Credentials => "HarnessCredentials",
            Self::PrMonitor => "HarnessPrMonitor",
            Self::Workflow => "HarnessWorkflow",
            Self::Testing => "HarnessTesting",
        }
    }

    /// Parse the closed source vocabulary used by the builtin macro.
    pub const fn from_field_name(name: &str) -> Option<Self> {
        match name.as_bytes() {
            b"stdio" => Some(Self::Stdio),
            b"term" => Some(Self::Term),
            b"clock" => Some(Self::Clock),
            b"fs" => Some(Self::Fs),
            b"env" => Some(Self::Env),
            b"random" => Some(Self::Random),
            b"net" => Some(Self::Net),
            b"process" => Some(Self::Process),
            b"channels" => Some(Self::Channels),
            b"system" => Some(Self::System),
            b"secrets" => Some(Self::Secrets),
            b"llm" => Some(Self::Llm),
            b"agent" => Some(Self::Agent),
            b"tenant" => Some(Self::Tenant),
            b"auth" => Some(Self::Auth),
            b"obs" => Some(Self::Observability),
            b"verdict" => Some(Self::Verdict),
            b"tools" => Some(Self::Tools),
            b"ast" => Some(Self::Ast),
            b"code_index" => Some(Self::CodeIndex),
            b"computer" => Some(Self::Computer),
            b"embed" => Some(Self::Embed),
            b"memory" => Some(Self::Memory),
            b"sqlite" => Some(Self::Sqlite),
            b"postgres" => Some(Self::Postgres),
            b"fs_watch" => Some(Self::FsWatch),
            b"host_lease" => Some(Self::HostLease),
            b"scanner" => Some(Self::Scanner),
            b"secret_store" => Some(Self::SecretStore),
            b"terminal" => Some(Self::TerminalSession),
            b"rules" => Some(Self::Rules),
            b"lint" => Some(Self::Lint),
            b"runtime" => Some(Self::Runtime),
            b"interaction" => Some(Self::Interaction),
            b"project" => Some(Self::Project),
            b"dashboard" => Some(Self::Dashboard),
            b"workspace" => Some(Self::Workspace),
            b"merge_captain" => Some(Self::MergeCaptain),
            b"session" => Some(Self::Session),
            b"permission" => Some(Self::Permission),
            b"text" => Some(Self::Text),
            b"lsp" => Some(Self::Lsp),
            b"credentials" => Some(Self::Credentials),
            b"pr_monitor" => Some(Self::PrMonitor),
            b"workflow" => Some(Self::Workflow),
            b"testing" => Some(Self::Testing),
            _ => None,
        }
    }

    pub fn from_type_name(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|capability| capability.type_name() == name)
    }

    /// Resolve the namespace half of a host-wire operation name such as
    /// `"prmonitor.run_commands"` or `"code_index.search"`.
    ///
    /// Host wires predate the typed capability vocabulary and spell namespaces
    /// without separators, so `"prmonitor"` and `"pr_monitor"` name the same
    /// capability. [`Self::from_field_name`] stays exact because it parses the
    /// closed source vocabulary, where a spelling either is or is not the
    /// declared field name.
    pub fn from_host_namespace(namespace: &str) -> Option<Self> {
        let wanted = wire_identifier_key(namespace);
        Self::ALL
            .iter()
            .copied()
            .find(|capability| wire_identifier_key(capability.field_name()) == wanted)
    }
}

/// Normalized spelling used to match a host-wire name against a declared
/// identifier: `_` removed and ASCII case folded.
///
/// One definition so the namespace half and the operation half of a wire name
/// are matched by the same rule.
pub fn wire_identifier_key(value: &str) -> String {
    value
        .chars()
        .filter(|character| *character != '_')
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

/// Closed effect family used for static ceilings and runtime receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EffectKind {
    Stdio,
    Fs,
    Env,
    Clock,
    Random,
    Network,
    Process,
    Llm,
    Tool,
    Mcp,
    Host,
    Authority,
    Worker,
    Secret,
    Observability,
    Channel,
    State,
}

/// How an operation interacts with its effect resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EffectAccess {
    Read,
    Write,
    Mutate,
    Observe,
}

/// Declarative extraction of resource identities from nominal call arguments.
///
/// A contract may carry several selectors, which covers moves/renames, staged
/// batches, and option-dependent scopes without another name-based classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceSelector {
    /// Whole positional argument at the declared index.
    Argument(u16),
    /// A nested field inside one positional argument.
    Field {
        argument: u16,
        path: &'static [&'static str],
    },
    /// Every element in a positional list argument.
    EachArgument(u16),
    /// A registry-owned fixed resource identity.
    Constant(&'static str),
    /// The operation is effectful but the resource cannot be resolved
    /// statically. Runtime receipt resolution may still supply it.
    Dynamic,
}

/// One conservative effect entry for a builtin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectSpec {
    pub kind: EffectKind,
    pub access: EffectAccess,
    pub resources: &'static [ResourceSelector],
}

/// An explicit capability grant that may authorize a builtin's declared
/// read-only effects.
///
/// This is deliberately part of the builtin contract rather than a policy
/// exception keyed by method or resource name. It lets runtime-owned helper
/// reads travel with the operation they support while keeping the effects
/// themselves visible to receipts and audit tooling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectAuthorization {
    pub capability: CapabilityId,
    pub operation: &'static str,
}

impl EffectAuthorization {
    pub const fn new(capability: CapabilityId, operation: &'static str) -> Self {
        Self {
            capability,
            operation,
        }
    }
}

impl EffectSpec {
    pub const fn new(
        kind: EffectKind,
        access: EffectAccess,
        resources: &'static [ResourceSelector],
    ) -> Self {
        Self {
            kind,
            access,
            resources,
        }
    }
}

/// Complete source exposure and effect contract paired with one builtin
/// implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinContract {
    pub exposure: BuiltinExposure,
    pub effects: &'static [EffectSpec],
    pub effects_authorized_by: Option<EffectAuthorization>,
    /// This operation is the agent runtime's own bookkeeping, not an action a
    /// model chose to take.
    ///
    /// It changes exactly one thing: the coarse side-effect ladder
    /// (`read_only` < `workspace_write` < `process_exec` < `network`) does not
    /// rank these effects. That ladder answers "how much of the user's world
    /// may a *tool* touch", and the runtime opening the session a model call
    /// is recorded in is not a tool the model ran — but `state:mutate` ranked
    /// `workspace_write` all the same, so a `read_only` ceiling rejected the
    /// agent loop's own session lifecycle and killed the turn before its
    /// first model call.
    ///
    /// Everything else still applies. A ceiling that restricts capabilities
    /// governs these effects exactly as before, they stay in receipts and
    /// lineage, and they stay in the effect record. This is not a way to make
    /// a write invisible; it is a statement about *who* performed it.
    ///
    /// Distinct from [`EffectAuthorization`], which delegates an effect to
    /// another capability grant and is deliberately limited to reads.
    pub runtime_infrastructure: bool,
}

impl BuiltinContract {
    pub const UNDECLARED: Self = Self {
        exposure: BuiltinExposure::Undeclared,
        effects: &[],
        effects_authorized_by: None,
        runtime_infrastructure: false,
    };

    pub const PURE: Self = Self {
        exposure: BuiltinExposure::PureGlobal,
        effects: &[],
        effects_authorized_by: None,
        runtime_infrastructure: false,
    };

    pub const RUNTIME_INTERNAL: Self = Self {
        exposure: BuiltinExposure::RuntimeInternal,
        effects: &[],
        effects_authorized_by: None,
        runtime_infrastructure: false,
    };

    pub const STDLIB_INTERNAL: Self = Self {
        exposure: BuiltinExposure::StdlibInternal,
        effects: &[],
        effects_authorized_by: None,
        runtime_infrastructure: false,
    };

    pub const fn harness(
        capability: CapabilityId,
        method: &'static str,
        effects: &'static [EffectSpec],
    ) -> Self {
        Self {
            exposure: BuiltinExposure::HarnessMethod { capability, method },
            effects,
            effects_authorized_by: None,
            runtime_infrastructure: false,
        }
    }

    pub const fn harness_with_effect_authorization(
        capability: CapabilityId,
        method: &'static str,
        effects: &'static [EffectSpec],
        effects_authorized_by: EffectAuthorization,
    ) -> Self {
        assert!(!effects.is_empty(), "effect authorization requires effects");
        let mut index = 0;
        while index < effects.len() {
            assert!(
                matches!(
                    effects[index].access,
                    EffectAccess::Read | EffectAccess::Observe
                ),
                "effect authorization is limited to read-only effects"
            );
            index += 1;
        }
        Self {
            exposure: BuiltinExposure::HarnessMethod { capability, method },
            effects,
            effects_authorized_by: Some(effects_authorized_by),
            runtime_infrastructure: false,
        }
    }

    /// A Harness method the agent runtime performs on its own behalf. See
    /// [`BuiltinContract::runtime_infrastructure`] for exactly what this
    /// relaxes and what it does not.
    pub const fn harness_runtime_infrastructure(
        capability: CapabilityId,
        method: &'static str,
        effects: &'static [EffectSpec],
    ) -> Self {
        assert!(
            !effects.is_empty(),
            "runtime infrastructure requires declared effects: the marker exempts \
             effects from the side-effect ladder, so a contract with none is \
             decorative and would read as audited while asserting nothing"
        );
        Self {
            exposure: BuiltinExposure::HarnessMethod { capability, method },
            effects,
            effects_authorized_by: None,
            runtime_infrastructure: true,
        }
    }

    pub const fn capability_function(
        authority_argument: u16,
        effects: &'static [EffectSpec],
    ) -> Self {
        Self {
            exposure: BuiltinExposure::CapabilityFunction { authority_argument },
            effects,
            effects_authorized_by: None,
            runtime_infrastructure: false,
        }
    }

    pub const fn privileged_wire(effects: &'static [EffectSpec]) -> Self {
        Self {
            exposure: BuiltinExposure::PrivilegedWire,
            effects,
            effects_authorized_by: None,
            runtime_infrastructure: false,
        }
    }

    pub const fn is_declared(self) -> bool {
        !matches!(self.exposure, BuiltinExposure::Undeclared)
    }

    pub const fn is_pure(self) -> bool {
        self.effects.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static WRITE_EFFECTS: &[EffectSpec] = &[EffectSpec::new(
        EffectKind::State,
        EffectAccess::Write,
        &[ResourceSelector::Dynamic],
    )];

    #[test]
    #[should_panic(expected = "effect authorization is limited to read-only effects")]
    fn effect_authorization_rejects_write_effects() {
        let _ = BuiltinContract::harness_with_effect_authorization(
            CapabilityId::Runtime,
            "test_write",
            WRITE_EFFECTS,
            EffectAuthorization::new(CapabilityId::Llm, "call"),
        );
    }
}
