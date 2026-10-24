//! Link-time registry of standard-library installers.
//!
//! A builtin family declares its installer beside the function that installs
//! it, with [`stdlib_installer!`](crate::stdlib_installer). The native runtime
//! runs every linked installer, so the crate that owns a family is free to sit
//! above or beside the runtime: no central list names the families, and adding
//! one is a declaration in its own module.
//!
//! No two installers may register the same builtin name or capability method.
//! That makes installation order irrelevant, so installers run sorted by name
//! and a family cannot silently replace another family's handler.

use crate::vm::Vm;

/// Which surface an installer belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StdlibTier {
    /// Deterministic builtins with no I/O. Sandboxed VMs install only these.
    Pure,
    /// Builtins that reach the host: files, processes, network, persistence,
    /// agents, and providers.
    Host,
}

/// One builtin family's installer.
#[derive(Debug)]
pub struct StdlibInstaller {
    /// Unique identity: the declaring module path and installer function.
    pub name: &'static str,
    pub tier: StdlibTier,
    /// Whether a VM that rebinds its execution state must run this installer
    /// again, because its handlers capture that state.
    pub rebinds_execution_state: bool,
    pub install: fn(&mut Vm),
}

/// Every linked installer contributes one entry.
#[linkme::distributed_slice]
pub static STDLIB_INSTALLERS: [StdlibInstaller];

/// Linked installers sorted by tier, then name.
///
/// # Panics
/// Panics if two installers share a name.
pub fn stdlib_installers() -> &'static [&'static StdlibInstaller] {
    use std::sync::OnceLock;

    static ORDERED: OnceLock<Vec<&'static StdlibInstaller>> = OnceLock::new();
    ORDERED.get_or_init(|| {
        let mut installers: Vec<_> = STDLIB_INSTALLERS.iter().collect();
        installers.sort_by_key(|installer| (installer.tier, installer.name));
        for pair in installers.windows(2) {
            assert_ne!(
                pair[0].name, pair[1].name,
                "stdlib installer `{}` is declared twice",
                pair[0].name
            );
        }
        installers
    })
}

/// Run every linked installer of one tier.
pub fn install_stdlib_tier(vm: &mut Vm, tier: StdlibTier) {
    for installer in stdlib_installers() {
        if installer.tier == tier {
            (installer.install)(vm);
        }
    }
}

/// Run the installers whose handlers capture execution state.
pub fn rebind_execution_state(vm: &mut Vm) {
    for installer in stdlib_installers() {
        if installer.rebinds_execution_state {
            (installer.install)(vm);
        }
    }
}

/// Declare a builtin family's installer.
///
/// ```ignore
/// harn_vm_core::stdlib_installer!(Pure, register_math_builtins);
/// harn_vm_core::stdlib_installer!(Host, register_concurrency_builtins, rebinds_execution_state);
/// ```
#[macro_export]
macro_rules! stdlib_installer {
    ($tier:ident, $install:path) => {
        $crate::stdlib_installer!(@declare $tier, $install, false);
    };
    ($tier:ident, $install:path, rebinds_execution_state) => {
        $crate::stdlib_installer!(@declare $tier, $install, true);
    };
    (@declare $tier:ident, $install:path, $rebinds:literal) => {
        const _: () = {
            #[$crate::builtin_support::distributed_slice($crate::stdlib_installers::STDLIB_INSTALLERS)]
            #[linkme(crate = $crate::builtin_support::linkme)]
            static INSTALLER: $crate::stdlib_installers::StdlibInstaller =
                $crate::stdlib_installers::StdlibInstaller {
                    name: concat!(module_path!(), "::", stringify!($install)),
                    tier: $crate::stdlib_installers::StdlibTier::$tier,
                    rebinds_execution_state: $rebinds,
                    install: $install,
                };
        };
    };
}
