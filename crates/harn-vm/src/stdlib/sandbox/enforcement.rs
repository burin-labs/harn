//! What each process sandbox backend confines, as one typed table.
//!
//! A backend renders a capability policy with its own kernel mechanism, and
//! the mechanisms do not cover the same ground. This table is the one
//! statement of that coverage: per backend, per dimension, whether a confined
//! child is held to the policy. `harn doctor` reports it, the `os_hardened`
//! refusal is decided from it, and the sandboxing docs render it, so none of
//! them states coverage in prose of its own.
//!
//! A cell is only as good as its measurement. The conformance suite judges
//! every case against the cell for the dimension it measures, so a cell that
//! claims more or less than the backend does fails a named case on that
//! platform. A dimension no case measures cannot hold anything but
//! [`Enforcement::Unmeasured`], and a test holds the table to that.

use serde::{Deserialize, Serialize};

use crate::orchestration::{CapabilityPolicy, SandboxProfile};
use crate::value::VmError;

use super::{
    policy_allows_network, SandboxBackend, SandboxMechanism, SandboxMechanismAvailability,
    SandboxMechanismUnavailable,
};

/// One thing a sandbox can hold a confined child to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfinementDimension {
    /// Writes land only under the policy's writable roots.
    Writes,
    /// Reads reach only the policy's readable roots.
    Reads,
    /// A path on the credential denylist stays unreadable, even under a root
    /// that is otherwise readable.
    CredentialReads,
    /// A policy below the `network` ceiling opens no network connection.
    Network,
    /// A confined child cannot signal or inspect processes outside its own
    /// tree.
    Process,
}

impl ConfinementDimension {
    /// Every dimension, in the order the table reports them.
    pub const ALL: [ConfinementDimension; 5] = [
        Self::Writes,
        Self::Reads,
        Self::CredentialReads,
        Self::Network,
        Self::Process,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Writes => "writes",
            Self::Reads => "reads",
            Self::CredentialReads => "credential_reads",
            Self::Network => "network",
            Self::Process => "process",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Writes => "writes",
            Self::Reads => "reads",
            Self::CredentialReads => "credential reads",
            Self::Network => "network",
            Self::Process => "process",
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Writes => 0,
            Self::Reads => 1,
            Self::CredentialReads => 2,
            Self::Network => 3,
            Self::Process => 4,
        }
    }

    /// Whether a confining `policy` asks the backend to hold this dimension.
    /// Every confining profile bounds writes, reads, and the credential
    /// denylist; the network is bounded only below the `network` ceiling. No
    /// policy term asks for process isolation yet.
    pub fn required_by(self, policy: &CapabilityPolicy) -> bool {
        match self {
            Self::Writes | Self::Reads | Self::CredentialReads => true,
            Self::Network => !policy_allows_network(policy),
            Self::Process => false,
        }
    }
}

/// Whether a backend holds a confined child to one dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    /// Measured: the backend holds the child to the policy.
    Enforced,
    /// Measured: the child reaches past the policy, and the backend says so.
    NotEnforced,
    /// No measurement backs a claim either way.
    Unmeasured,
}

impl Enforcement {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enforced => "enforced",
            Self::NotEnforced => "not_enforced",
            Self::Unmeasured => "unmeasured",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Enforced => "enforced",
            Self::NotEnforced => "not enforced",
            Self::Unmeasured => "unmeasured",
        }
    }
}

/// One backend's row: the mechanism that names it and a cell per dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackendEnforcement {
    pub mechanism: SandboxMechanism,
    cells: [Enforcement; 5],
}

use Enforcement::{Enforced, Unmeasured};

/// The table. Filled only from what the conformance suite measures on each
/// platform's CI; see the module docs.
pub const TABLE: &[BackendEnforcement] = &[
    BackendEnforcement::row(
        SandboxMechanism::LinuxLandlock,
        [Enforced, Enforced, Enforced, Enforced, Unmeasured],
    ),
    BackendEnforcement::row(
        SandboxMechanism::MacosSandboxExec,
        [Enforced, Enforced, Enforced, Enforced, Unmeasured],
    ),
    BackendEnforcement::row(
        SandboxMechanism::WindowsAppContainer,
        [Unmeasured, Unmeasured, Unmeasured, Unmeasured, Unmeasured],
    ),
    BackendEnforcement::row(
        SandboxMechanism::OpenbsdUnveil,
        [Unmeasured, Unmeasured, Unmeasured, Unmeasured, Unmeasured],
    ),
];

impl BackendEnforcement {
    /// Cells in [`ConfinementDimension::ALL`] order.
    const fn row(mechanism: SandboxMechanism, cells: [Enforcement; 5]) -> Self {
        Self { mechanism, cells }
    }

    /// The row for a mechanism. Every mechanism has one; a test holds that.
    pub fn for_mechanism(mechanism: SandboxMechanism) -> Option<&'static Self> {
        TABLE.iter().find(|row| row.mechanism == mechanism)
    }

    /// A copy of `row` with one cell replaced, for judging a row that does
    /// not exist on this host.
    #[cfg(test)]
    pub(crate) fn with_cell(
        mut row: Self,
        dimension: ConfinementDimension,
        cell: Enforcement,
    ) -> Self {
        row.cells[dimension.index()] = cell;
        row
    }

    pub fn cell(&self, dimension: ConfinementDimension) -> Enforcement {
        self.cells[dimension.index()]
    }

    /// Every dimension with its cell, in table order.
    pub fn cells(&self) -> impl Iterator<Item = (ConfinementDimension, Enforcement)> + '_ {
        ConfinementDimension::ALL
            .into_iter()
            .map(|dimension| (dimension, self.cell(dimension)))
    }

    /// The dimensions `policy` requires that this backend measurably does not
    /// hold. An unmeasured cell is not listed: nothing shows it failing, and
    /// the receipt names it unmeasured instead.
    pub fn unenforced_requirements(&self, policy: &CapabilityPolicy) -> Vec<ConfinementDimension> {
        self.cells()
            .filter(|(dimension, cell)| {
                *cell == Enforcement::NotEnforced && dimension.required_by(policy)
            })
            .map(|(dimension, _)| dimension)
            .collect()
    }

    /// One line naming every cell, such as
    /// `writes enforced; reads not enforced; ...`.
    pub fn receipt(&self) -> String {
        self.cells()
            .map(|(dimension, cell)| {
                format!("{} {}", dimension.display_name(), cell.display_name())
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// The row for the backend compiled into this binary, or `None` on a platform
/// with no process sandbox at all.
pub fn active_enforcement() -> Option<&'static BackendEnforcement> {
    let mechanism = super::active_backend_filesystem_mechanism();
    TABLE.iter().find(|row| row.mechanism.as_str() == mechanism)
}

/// What every spawn entry point checks before a backend prepares the child:
/// the backend can render the policy's egress, and an `os_hardened` policy
/// requires no dimension the backend measurably does not hold. Every other
/// profile runs, and the receipt says what went unconfined.
pub(crate) fn ensure_spawn_enforceable<B: SandboxBackend + ?Sized>(
    policy: &CapabilityPolicy,
) -> Result<(), VmError> {
    if let Some(refusal) = active_enforcement().and_then(|row| refusal_for(row, policy)) {
        return Err(refusal.into_error());
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::marker::PhantomData::<B>;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        if policy.process_network_proxy.is_some() {
            return Err(super::sandbox_rejection(format!(
                "managed child-process egress is not enforceable by the {} process sandbox",
                B::name()
            )));
        }
        Ok(())
    }
}

fn refusal_for(
    row: &BackendEnforcement,
    policy: &CapabilityPolicy,
) -> Option<SandboxMechanismUnavailable> {
    if policy.sandbox_profile != SandboxProfile::OsHardened {
        return None;
    }
    let unenforced = row.unenforced_requirements(policy);
    if unenforced.is_empty() {
        return None;
    }
    let mut refusal = SandboxMechanismUnavailable::new(
        row.mechanism,
        SandboxMechanismAvailability::DoesNotConfine,
        policy.sandbox_profile,
    );
    refusal.unconfined = unenforced;
    Some(refusal)
}

/// The table as a Markdown table, for the sandboxing docs.
pub fn markdown_table() -> String {
    let mut out = String::from("| Backend |");
    for dimension in ConfinementDimension::ALL {
        out.push_str(&format!(" {} |", dimension.display_name()));
    }
    out.push_str("\n|---|");
    out.push_str(&"---|".repeat(ConfinementDimension::ALL.len()));
    out.push('\n');
    for row in TABLE {
        out.push_str(&format!("| {} |", row.mechanism.display_name()));
        for (_, cell) in row.cells() {
            out.push_str(&format!(" {} |", cell.display_name()));
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_sandbox::conformance::ConformanceCase;
    use ConfinementDimension as D;

    fn policy(profile: SandboxProfile, level: &str) -> CapabilityPolicy {
        CapabilityPolicy {
            sandbox_profile: profile,
            side_effect_level: Some(level.to_string()),
            ..CapabilityPolicy::default()
        }
    }

    const WRITES_ONLY: BackendEnforcement = BackendEnforcement::row(
        SandboxMechanism::WindowsAppContainer,
        [
            Enforced,
            Enforcement::NotEnforced,
            Enforcement::NotEnforced,
            Enforcement::NotEnforced,
            Unmeasured,
        ],
    );

    #[test]
    fn every_mechanism_has_exactly_one_row() {
        for mechanism in SandboxMechanism::ALL {
            let rows = TABLE
                .iter()
                .filter(|row| row.mechanism == *mechanism)
                .count();
            assert_eq!(rows, 1, "{mechanism:?} must have exactly one row");
        }
        assert_eq!(TABLE.len(), SandboxMechanism::ALL.len());
    }

    /// A cell may only claim a measurement a conformance case makes. Without
    /// this, a row could say `enforced` for a dimension nothing ever probes.
    #[test]
    fn only_a_dimension_some_conformance_case_measures_holds_a_measured_cell() {
        for row in TABLE {
            for (dimension, cell) in row.cells() {
                let measured = ConformanceCase::ALL
                    .iter()
                    .any(|case| case.dimension() == Some(dimension));
                assert!(
                    measured || cell == Unmeasured,
                    "{:?} claims {cell:?} for {dimension:?}, which no conformance case measures",
                    row.mechanism
                );
            }
        }
    }

    #[test]
    fn the_active_backend_has_a_row() {
        let expected = cfg!(any(
            target_os = "linux",
            target_os = "macos",
            target_os = "windows",
            target_os = "openbsd"
        ));
        assert_eq!(active_enforcement().is_some(), expected);
    }

    /// Both sides of the refusal on a backend that confines only writes: the
    /// hardened profile is refused and names every gap it requires, and the
    /// default profile runs.
    #[test]
    fn os_hardened_is_refused_for_each_required_dimension_the_backend_does_not_hold() {
        let refusal = refusal_for(
            &WRITES_ONLY,
            &policy(SandboxProfile::OsHardened, "process_exec"),
        )
        .expect("os_hardened must refuse");
        assert_eq!(
            refusal.availability,
            SandboxMechanismAvailability::DoesNotConfine
        );
        assert_eq!(
            refusal.unconfined,
            vec![D::Reads, D::CredentialReads, D::Network]
        );
        assert!(
            refusal_for(
                &WRITES_ONLY,
                &policy(SandboxProfile::Worktree, "process_exec")
            )
            .is_none(),
            "the default profile runs and takes the receipt"
        );
    }

    /// At the network ceiling the policy no longer asks for network
    /// confinement, so its absence is not a reason to refuse.
    #[test]
    fn the_network_gap_is_only_required_below_the_network_ceiling() {
        let refusal = refusal_for(&WRITES_ONLY, &policy(SandboxProfile::OsHardened, "network"))
            .expect("reads are still unconfined");
        assert_eq!(refusal.unconfined, vec![D::Reads, D::CredentialReads]);
    }

    #[test]
    fn a_fully_enforcing_row_refuses_nothing() {
        let linux = BackendEnforcement::for_mechanism(SandboxMechanism::LinuxLandlock).unwrap();
        assert!(refusal_for(linux, &policy(SandboxProfile::OsHardened, "process_exec")).is_none());
    }

    #[test]
    fn the_receipt_names_every_cell() {
        assert_eq!(
            WRITES_ONLY.receipt(),
            "writes enforced; reads not enforced; credential reads not enforced; \
             network not enforced; process unmeasured"
        );
    }

    /// The sandboxing docs carry the rendered table between markers, and this
    /// is what keeps it the table's projection rather than a copy.
    #[test]
    fn the_sandboxing_docs_render_the_table() {
        const BEGIN: &str = "<!-- sandbox-enforcement-table:begin -->\n";
        const END: &str = "<!-- sandbox-enforcement-table:end -->";
        let docs_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/src/sandboxing.md");
        let docs = std::fs::read_to_string(&docs_path).expect("read sandboxing docs");
        let (_, after_begin) = docs.split_once(BEGIN).expect("begin marker");
        let (rendered, _) = after_begin.split_once(END).expect("end marker");
        assert_eq!(
            rendered,
            markdown_table(),
            "{} is stale; replace the block between the markers with the rendered table",
            docs_path.display()
        );
    }
}
