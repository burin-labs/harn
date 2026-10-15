use crate::CapabilityId;

/// Runtime discriminator for the root grant or one typed capability handle.
/// The capability registry owns the field order, names, and source types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HarnessKind(Option<CapabilityId>);

#[allow(non_upper_case_globals)]
impl HarnessKind {
    pub const Root: Self = Self(None);
    pub const Stdio: Self = Self(Some(CapabilityId::Stdio));
    pub const Term: Self = Self(Some(CapabilityId::Term));
    pub const Clock: Self = Self(Some(CapabilityId::Clock));
    pub const Fs: Self = Self(Some(CapabilityId::Fs));
    pub const Env: Self = Self(Some(CapabilityId::Env));
    pub const Random: Self = Self(Some(CapabilityId::Random));
    pub const Net: Self = Self(Some(CapabilityId::Net));
    pub const Process: Self = Self(Some(CapabilityId::Process));
    pub const Channels: Self = Self(Some(CapabilityId::Channels));
    pub const System: Self = Self(Some(CapabilityId::System));
    pub const Secrets: Self = Self(Some(CapabilityId::Secrets));
    pub const Llm: Self = Self(Some(CapabilityId::Llm));
    pub const Agent: Self = Self(Some(CapabilityId::Agent));
    pub const Tenant: Self = Self(Some(CapabilityId::Tenant));
    pub const Auth: Self = Self(Some(CapabilityId::Auth));
    pub const Obs: Self = Self(Some(CapabilityId::Observability));
    pub const Verdict: Self = Self(Some(CapabilityId::Verdict));
    pub const Tools: Self = Self(Some(CapabilityId::Tools));
    pub const Ast: Self = Self(Some(CapabilityId::Ast));
    pub const CodeIndex: Self = Self(Some(CapabilityId::CodeIndex));
    pub const Computer: Self = Self(Some(CapabilityId::Computer));
    pub const Embed: Self = Self(Some(CapabilityId::Embed));
    pub const Memory: Self = Self(Some(CapabilityId::Memory));
    pub const Sqlite: Self = Self(Some(CapabilityId::Sqlite));
    pub const Postgres: Self = Self(Some(CapabilityId::Postgres));
    pub const FsWatch: Self = Self(Some(CapabilityId::FsWatch));
    pub const HostLease: Self = Self(Some(CapabilityId::HostLease));
    pub const Scanner: Self = Self(Some(CapabilityId::Scanner));
    pub const SecretStore: Self = Self(Some(CapabilityId::SecretStore));
    pub const TerminalSession: Self = Self(Some(CapabilityId::TerminalSession));
    pub const Rules: Self = Self(Some(CapabilityId::Rules));
    pub const Lint: Self = Self(Some(CapabilityId::Lint));
    pub const Runtime: Self = Self(Some(CapabilityId::Runtime));
    pub const Interaction: Self = Self(Some(CapabilityId::Interaction));
    pub const Project: Self = Self(Some(CapabilityId::Project));
    pub const Dashboard: Self = Self(Some(CapabilityId::Dashboard));
    pub const Workspace: Self = Self(Some(CapabilityId::Workspace));
    pub const MergeCaptain: Self = Self(Some(CapabilityId::MergeCaptain));
    pub const Session: Self = Self(Some(CapabilityId::Session));
    pub const Permission: Self = Self(Some(CapabilityId::Permission));
    pub const Text: Self = Self(Some(CapabilityId::Text));
    pub const Lsp: Self = Self(Some(CapabilityId::Lsp));
    pub const Credentials: Self = Self(Some(CapabilityId::Credentials));
    pub const PrMonitor: Self = Self(Some(CapabilityId::PrMonitor));
    pub const Workflow: Self = Self(Some(CapabilityId::Workflow));
    pub const Testing: Self = Self(Some(CapabilityId::Testing));

    pub const fn capability_id(self) -> Option<CapabilityId> {
        self.0
    }

    pub const fn type_name(self) -> &'static str {
        match self.0 {
            None => "Harness",
            Some(capability) => capability.type_name(),
        }
    }

    pub const fn field_name(self) -> Option<&'static str> {
        match self.0 {
            None => None,
            Some(capability) => Some(capability.field_name()),
        }
    }

    pub const fn from_field_name(name: &str) -> Option<Self> {
        match CapabilityId::from_field_name(name) {
            Some(capability) => Some(Self(Some(capability))),
            None => None,
        }
    }

    /// All sub-handles in canonical root-field order.
    pub const SUB_HANDLES: &'static [Self] = &Self::sub_handles();

    /// Every kind a Harn-script type annotation may reference.
    pub const ALL: &'static [Self] = &Self::all();

    const fn sub_handles() -> [Self; CapabilityId::ALL.len()] {
        let mut kinds = [Self::Root; CapabilityId::ALL.len()];
        let mut index = 0;
        while index < kinds.len() {
            kinds[index] = Self(Some(CapabilityId::ALL[index]));
            index += 1;
        }
        kinds
    }

    const fn all() -> [Self; CapabilityId::ALL.len() + 1] {
        let mut kinds = [Self::Root; CapabilityId::ALL.len() + 1];
        let mut index = 0;
        while index < CapabilityId::ALL.len() {
            kinds[index + 1] = Self(Some(CapabilityId::ALL[index]));
            index += 1;
        }
        kinds
    }
}
