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
//!
//! A family that keeps thread-local state declares its reset the same way,
//! with [`stdlib_state_reset!`](crate::stdlib_state_reset).

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

/// Define a family's installer as "register every `#[harn_builtin]` declared
/// in this module", and declare it.
///
/// ```ignore
/// harn_vm_core::stdlib_module_installer!(Pure, pub(crate) fn register_math_builtins);
/// ```
#[macro_export]
macro_rules! stdlib_module_installer {
    ($(#[$attr:meta])* $tier:ident, $vis:vis fn $install:ident $(, $flag:ident)?) => {
        $(#[$attr])*
        $vis fn $install(vm: &mut $crate::vm::Vm) {
            $crate::builtin_support::register_module_builtins(vm, module_path!());
        }
        $crate::stdlib_installer!($tier, $install $(, $flag)?);
    };
}

/// A family's thread-local state reset, run between test runs and host
/// sessions by [`reset_stdlib_state`].
#[derive(Debug)]
pub struct StdlibStateReset {
    /// Unique identity: the declaring module path and reset function.
    pub name: &'static str,
    pub reset: fn(),
}

/// Every linked state reset contributes one entry.
#[linkme::distributed_slice]
pub static STDLIB_STATE_RESETS: [StdlibStateReset];

/// Run every linked state reset, sorted by name.
///
/// # Panics
/// Panics if two resets share a name.
pub fn reset_stdlib_state() {
    use std::sync::OnceLock;

    static ORDERED: OnceLock<Vec<&'static StdlibStateReset>> = OnceLock::new();
    let resets = ORDERED.get_or_init(|| {
        let mut resets: Vec<_> = STDLIB_STATE_RESETS.iter().collect();
        resets.sort_by_key(|reset| reset.name);
        for pair in resets.windows(2) {
            assert_ne!(
                pair[0].name, pair[1].name,
                "stdlib state reset `{}` is declared twice",
                pair[0].name
            );
        }
        resets
    });
    for reset in resets {
        (reset.reset)();
    }
}

/// Declare a function that resets a family's thread-local state.
///
/// ```ignore
/// harn_vm_core::stdlib_state_reset!(reset_timing_state);
/// ```
#[macro_export]
macro_rules! stdlib_state_reset {
    ($reset:path) => {
        const _: () = {
            #[$crate::builtin_support::distributed_slice(
                $crate::stdlib_installers::STDLIB_STATE_RESETS
            )]
            #[linkme(crate = $crate::builtin_support::linkme)]
            static RESET: $crate::stdlib_installers::StdlibStateReset =
                $crate::stdlib_installers::StdlibStateReset {
                    name: concat!(module_path!(), "::", stringify!($reset)),
                    reset: $reset,
                };
        };
    };
}
