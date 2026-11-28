//! Typed authority and effect contracts for Harn builtins.
//!
//! This dependency-leaf model is the semantic owner for which script surface
//! may reach a builtin and what that call can do. Runtime handler pointers stay
//! in `harn-vm`; parser, IR, policy, hostlib, and documentation consumers can
//! all depend on this crate without reversing the workspace dependency graph.

/// Where a builtin is visible to Harn source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

macro_rules! define_capabilities {
    ($($variant:ident $(as $kind:ident)? => ($field:literal, $type:literal)),* $(,)?) => {
        /// Closed vocabulary of capability handles exposed by `Harness`.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum CapabilityId { $($variant),* }

        impl CapabilityId {
            /// Every capability in canonical root-field order.
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];

            /// Rust enum variant spelling for generated contract expressions.
            pub const fn variant_name(self) -> &'static str {
                match self { $(Self::$variant => stringify!($variant)),* }
            }

            /// Canonical source-level `harness.<field>` name.
            pub const fn field_name(self) -> &'static str {
                match self { $(Self::$variant => $field),* }
            }

            /// Nominal source type carried by this capability handle.
            pub const fn type_name(self) -> &'static str {
                match self { $(Self::$variant => $type),* }
            }

            /// Parse the closed source vocabulary used by the builtin macro.
            pub const fn from_field_name(name: &str) -> Option<Self> {
                // Named slice patterns keep this a const byte match without a second name table.
                #[allow(non_upper_case_globals)]
                mod fields { $(pub const $variant: &[u8] = $field.as_bytes();)* }
                match name.as_bytes() {
                    $(fields::$variant => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }
    };
}

crate::capability_registry::capability_registry!(define_capabilities);

impl CapabilityId {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BuiltinContract {
    pub exposure: BuiltinExposure,
    pub effects: &'static [EffectSpec],
    pub effects_authorized_by: Option<EffectAuthorization>,
    /// This operation mutates Harn's runtime control plane rather than the
    /// user's workspace or an external system.
    ///
    /// It changes exactly one thing: the coarse side-effect ladder
    /// (`read_only` < `workspace_write` < `process_exec` < `network_read` < `network`) does not
    /// rank these effects. That ladder answers "how much of the user's world
    /// may an operation touch", while opening the session a model call is
    /// recorded in changes Harn-owned control state. `state:mutate` ranked
    /// `workspace_write` all the same, so a `read_only` ceiling rejected the
    /// agent loop's own session lifecycle and killed the turn before its
    /// first model call.
    ///
    /// Everything else still applies. A ceiling that restricts capabilities
    /// governs these effects exactly as before, they stay in receipts and
    /// lineage, and they stay in the effect record. This classifies the
    /// operation's target domain; it does not prove or change caller identity.
    ///
    /// Distinct from [`EffectAuthorization`], which delegates an effect to
    /// another capability grant and is deliberately limited to reads.
    runtime_control_plane: bool,
    /// Explicit admission for a read-only runtime operation whose effect contract
    /// stays empty. This never changes source exposure or execution authority.
    read_only_preparation: bool,
}

impl BuiltinContract {
    pub const UNDECLARED: Self = Self {
        exposure: BuiltinExposure::Undeclared,
        effects: &[],
        effects_authorized_by: None,
        runtime_control_plane: false,
        read_only_preparation: false,
    };

    pub const PURE: Self = Self {
        exposure: BuiltinExposure::PureGlobal,
        effects: &[],
        effects_authorized_by: None,
        runtime_control_plane: false,
        read_only_preparation: false,
    };

    pub const RUNTIME_INTERNAL: Self = Self {
        exposure: BuiltinExposure::RuntimeInternal,
        effects: &[],
        effects_authorized_by: None,
        runtime_control_plane: false,
        read_only_preparation: false,
    };

    pub const STDLIB_INTERNAL: Self = Self {
        exposure: BuiltinExposure::StdlibInternal,
        effects: &[],
        effects_authorized_by: None,
        runtime_control_plane: false,
        read_only_preparation: false,
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
            runtime_control_plane: false,
            read_only_preparation: false,
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
            runtime_control_plane: false,
            read_only_preparation: false,
        }
    }

    /// A Harness method that mutates Harn-owned runtime control-plane state. See
    /// [`BuiltinContract::is_runtime_control_plane`] for exactly what this
    /// relaxes and what it does not.
    pub const fn harness_runtime_control_plane(
        capability: CapabilityId,
        method: &'static str,
        effects: &'static [EffectSpec],
    ) -> Self {
        assert!(
            !effects.is_empty(),
            "runtime control plane requires declared effects: the marker classifies \
             effects outside the user-world side-effect ladder, so a contract with \
             none is decorative and would read as audited while asserting nothing"
        );
        let mut index = 0;
        let mut mutates_state = false;
        while index < effects.len() {
            assert!(
                matches!(effects[index].kind, EffectKind::State),
                "runtime control plane effects must target state"
            );
            if matches!(
                effects[index].access,
                EffectAccess::Write | EffectAccess::Mutate
            ) {
                mutates_state = true;
            }
            index += 1;
        }
        assert!(
            mutates_state,
            "runtime control plane requires at least one state write or mutation"
        );
        Self {
            exposure: BuiltinExposure::HarnessMethod { capability, method },
            effects,
            effects_authorized_by: None,
            runtime_control_plane: true,
            read_only_preparation: false,
        }
    }

    /// Whether this contract targets Harn-owned runtime control-plane state.
    ///
    /// The marker is private so callers cannot bypass the structural checks in
    /// [`BuiltinContract::harness_runtime_control_plane`].
    pub const fn is_runtime_control_plane(self) -> bool {
        self.runtime_control_plane
    }

    pub const fn capability_function(
        authority_argument: u16,
        effects: &'static [EffectSpec],
    ) -> Self {
        Self {
            exposure: BuiltinExposure::CapabilityFunction { authority_argument },
            effects,
            effects_authorized_by: None,
            runtime_control_plane: false,
            read_only_preparation: false,
        }
    }

    pub const fn privileged_wire(effects: &'static [EffectSpec]) -> Self {
        Self {
            exposure: BuiltinExposure::PrivilegedWire,
            effects,
            effects_authorized_by: None,
            runtime_control_plane: false,
            read_only_preparation: false,
        }
    }

    /// Admit one declared read-only runtime operation during invocation preparation.
    /// Effectful operations use their existing read effects instead. Only
    /// explicitly declared read-only runtime operations may opt in; the trusted declaration
    /// must match the implementation. This does not prove body purity.
    pub const fn with_read_only_preparation(mut self) -> Self {
        assert!(
            matches!(
                self.exposure,
                BuiltinExposure::RuntimeInternal | BuiltinExposure::HarnessMethod { .. }
            ),
            "read-only preparation marker requires a runtime or Harness operation"
        );
        assert!(
            self.effects.is_empty(),
            "read-only preparation marker cannot coexist with effects"
        );
        self.read_only_preparation = true;
        self
    }

    /// One admission predicate shared by every preparation dispatch surface.
    pub fn permits_read_only_preparation(self) -> bool {
        if self.read_only_preparation
            && (!matches!(
                self.exposure,
                BuiltinExposure::RuntimeInternal | BuiltinExposure::HarnessMethod { .. }
            ) || !self.effects.is_empty())
        {
            return false;
        }
        let pure = matches!(
            self.exposure,
            BuiltinExposure::PureGlobal | BuiltinExposure::StdlibInternal
        );
        (pure || self.read_only_preparation || !self.effects.is_empty())
            && self.effects.iter().all(|effect| {
                effect.access == EffectAccess::Read
                    && matches!(
                        effect.kind,
                        EffectKind::Fs | EffectKind::Env | EffectKind::State | EffectKind::Host
                    )
            })
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
    fn read_only_preparation_is_explicit_without_changing_effects_or_exposure() {
        assert!(!BuiltinContract::RUNTIME_INTERNAL.permits_read_only_preparation());
        assert!(!BuiltinContract::privileged_wire(&[]).permits_read_only_preparation());
        let read = BuiltinContract::RUNTIME_INTERNAL.with_read_only_preparation();
        assert!(read.permits_read_only_preparation());
        assert_eq!(read.exposure, BuiltinExposure::RuntimeInternal);
        assert!(read.effects.is_empty());
        let mut changed = read;
        changed.exposure = BuiltinExposure::PrivilegedWire;
        assert!(!changed.permits_read_only_preparation());
        changed = read;
        changed.effects = WRITE_EFFECTS;
        assert!(!changed.permits_read_only_preparation());
        let capability = BuiltinContract::harness(CapabilityId::Agent, "journal_read", &[])
            .with_read_only_preparation();
        assert!(capability.permits_read_only_preparation());
    }

    #[test]
    fn read_only_preparation_marker_refuses_effectful_and_privileged_contracts() {
        for kind in [EffectKind::State, EffectKind::Process, EffectKind::Network] {
            let effects = Box::leak(Box::new([EffectSpec::new(
                kind,
                EffectAccess::Write,
                &[ResourceSelector::Dynamic],
            )]));
            let contract = BuiltinContract::harness(CapabilityId::Agent, "not_a_read", effects);
            assert!(std::panic::catch_unwind(|| contract.with_read_only_preparation()).is_err());
        }
        assert!(std::panic::catch_unwind(
            || BuiltinContract::privileged_wire(&[]).with_read_only_preparation()
        )
        .is_err());
        assert!(std::panic::catch_unwind(
            || BuiltinContract::UNDECLARED.with_read_only_preparation()
        )
        .is_err());
    }

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

    #[test]
    #[should_panic(expected = "runtime control plane effects must target state")]
    fn runtime_control_plane_rejects_non_state_effects() {
        static EFFECTS: &[EffectSpec] = &[EffectSpec::new(
            EffectKind::Fs,
            EffectAccess::Write,
            &[ResourceSelector::Dynamic],
        )];
        let _ = BuiltinContract::harness_runtime_control_plane(
            CapabilityId::Agent,
            "unsafe_write",
            EFFECTS,
        );
    }

    #[test]
    #[should_panic(expected = "requires at least one state write or mutation")]
    fn runtime_control_plane_rejects_read_only_state_effects() {
        static EFFECTS: &[EffectSpec] = &[EffectSpec::new(
            EffectKind::State,
            EffectAccess::Read,
            &[ResourceSelector::Dynamic],
        )];
        let _ = BuiltinContract::harness_runtime_control_plane(
            CapabilityId::Agent,
            "state_read",
            EFFECTS,
        );
    }
}
