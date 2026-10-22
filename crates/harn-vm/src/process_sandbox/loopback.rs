//! Whether a loopback-only network grant can be honored on this host, and what
//! happens to the grant when it cannot.
//!
//! One rule lives here. A confined child gets loopback only inside a private
//! network namespace, and on Linux only the namespace launcher can build one.
//! When the host has no usable launcher, the grant is withheld and everything
//! else stays confined; the run is not refused and the filter is never widened
//! without the namespace that bounds it. [`loopback_support`] is the typed
//! query, [`withhold_unsupported_loopback`] applies the rule to a policy, and
//! the spawn path in `crate::stdlib::sandbox::active_sandbox_policy` applies it to every
//! confined child, so a host cannot disagree with the query about what a spawn
//! will do.

use std::path::PathBuf;

use crate::orchestration::{CapabilityPolicy, ProcessSandboxPolicy};

/// Event category of the notice emitted when a grant is withheld.
pub const LOOPBACK_UNAVAILABLE_CATEGORY: &str = "sandbox.loopback_unavailable";

/// The policy field a loopback grant is spelled as.
pub const LOOPBACK_GRANT_FIELD: &str = "allow_tcp_loopback";

/// The policy field that names the namespace launcher.
pub const NETNS_LAUNCHER_FIELD: &str = "process.netns_launcher_path";

/// Why this host cannot give a confined child loopback networking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopbackUnavailableReason {
    /// The policy names no namespace launcher.
    NoLauncherConfigured,
    /// The named launcher path is relative, so it does not name one file.
    LauncherPathNotAbsolute,
    /// The named launcher is not an existing executable file on this host.
    LauncherMissing,
    /// This platform has no mechanism that bounds loopback for a confined child.
    UnsupportedPlatform,
}

impl LoopbackUnavailableReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoLauncherConfigured => "no_launcher_configured",
            Self::LauncherPathNotAbsolute => "launcher_path_not_absolute",
            Self::LauncherMissing => "launcher_missing",
            Self::UnsupportedPlatform => "unsupported_platform",
        }
    }
}

/// A loopback grant this host cannot honor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoopbackUnavailable {
    pub reason: LoopbackUnavailableReason,
    /// The launcher path the policy named, when it named one.
    pub launcher: Option<String>,
}

impl LoopbackUnavailable {
    /// Whether the grant may be dropped to run more confined. A platform with
    /// no loopback mechanism keeps the grant so its own backend refuses it: a
    /// dropped grant there could read as "no network wanted" and widen egress.
    fn is_droppable(&self) -> bool {
        self.reason != LoopbackUnavailableReason::UnsupportedPlatform
    }

    /// What a person is told: the consequence first, then what would fix it.
    pub fn notice(&self) -> String {
        let need = format!(
            "Linux gives a confined command loopback networking only inside a private network \
             namespace, which needs the namespace launcher: an executable copy of the Harn \
             runtime, at an absolute path set as `{NETNS_LAUNCHER_FIELD}` in the host's sandbox \
             policy"
        );
        let cause = match (self.reason, self.launcher.as_deref()) {
            (LoopbackUnavailableReason::NoLauncherConfigured, _) => {
                "No launcher is configured.".to_string()
            }
            (LoopbackUnavailableReason::LauncherPathNotAbsolute, Some(path)) => {
                format!("The configured launcher path `{path}` is not absolute.")
            }
            (LoopbackUnavailableReason::LauncherMissing, Some(path)) => {
                format!("The configured launcher `{path}` is not an existing executable file on this host.")
            }
            (LoopbackUnavailableReason::UnsupportedPlatform, _) => {
                return format!(
                    "Loopback networking is unavailable on this host: this platform has no \
                     mechanism that confines a command to loopback. The `{LOOPBACK_GRANT_FIELD}` \
                     grant was dropped; everything else stays confined, and commands that need \
                     loopback will fail."
                );
            }
            (_, None) => String::new(),
        };
        format!(
            "Loopback networking is unavailable on this host. The `{LOOPBACK_GRANT_FIELD}` grant \
             was dropped; everything else stays confined, and commands that need loopback will \
             fail. {need}. {cause}"
        )
        .trim_end()
        .to_string()
    }

    /// The same fact as structured event metadata, for hosts that render
    /// notices themselves.
    pub fn metadata(&self) -> std::collections::BTreeMap<String, serde_json::Value> {
        let mut metadata = std::collections::BTreeMap::from([
            ("grant".to_string(), LOOPBACK_GRANT_FIELD.into()),
            ("reason".to_string(), self.reason.as_str().into()),
            ("launcher_field".to_string(), NETNS_LAUNCHER_FIELD.into()),
        ]);
        if let Some(launcher) = &self.launcher {
            metadata.insert("launcher".to_string(), launcher.clone().into());
        }
        metadata
    }
}

/// Whether a loopback grant would be honored on this host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopbackSupport {
    /// The grant is honored. On Linux this carries the launcher it runs through.
    Supported {
        launcher: Option<PathBuf>,
    },
    Unavailable(LoopbackUnavailable),
}

impl LoopbackSupport {
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Supported { .. })
    }
}

/// Whether this host can give a confined child loopback networking under
/// `process`. Independent of whether the policy grants it.
pub fn loopback_support(process: &ProcessSandboxPolicy) -> LoopbackSupport {
    #[cfg(target_os = "linux")]
    {
        let unavailable = |reason, launcher: Option<String>| {
            LoopbackSupport::Unavailable(LoopbackUnavailable { reason, launcher })
        };
        let Some(path) = process.netns_launcher_path.as_ref() else {
            return unavailable(LoopbackUnavailableReason::NoLauncherConfigured, None);
        };
        let launcher = PathBuf::from(path);
        if !launcher.is_absolute() {
            return unavailable(
                LoopbackUnavailableReason::LauncherPathNotAbsolute,
                Some(path.clone()),
            );
        }
        if !is_executable_file(&launcher) {
            return unavailable(
                LoopbackUnavailableReason::LauncherMissing,
                Some(path.clone()),
            );
        }
        LoopbackSupport::Supported {
            launcher: Some(launcher),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let _ = process;
        LoopbackSupport::Supported { launcher: None }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        LoopbackSupport::Unavailable(LoopbackUnavailable {
            reason: LoopbackUnavailableReason::UnsupportedPlatform,
            launcher: process.netns_launcher_path.clone(),
        })
    }
}

/// A regular file with an execute bit. The launcher has no no-op self-check
/// mode, so whether the namespace itself can be built is not probed here.
#[cfg(target_os = "linux")]
fn is_executable_file(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

/// Drop the loopback grant from `process` when this host cannot honor it.
///
/// Returns what was dropped, or `None` when the policy needed no change or the
/// platform has no loopback mechanism (the backend then refuses the grant). Only
/// the one grant changes: every root, profile, and other grant is untouched.
pub fn withhold_unsupported_loopback(
    process: &mut ProcessSandboxPolicy,
) -> Option<LoopbackUnavailable> {
    if !process.allow_tcp_loopback {
        return None;
    }
    match loopback_support(process) {
        LoopbackSupport::Supported { .. } => None,
        LoopbackSupport::Unavailable(unavailable) if !unavailable.is_droppable() => None,
        LoopbackSupport::Unavailable(unavailable) => {
            process.allow_tcp_loopback = false;
            Some(unavailable)
        }
    }
}

static NOTICE_EMITTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(all(test, target_os = "linux"))]
pub(crate) fn reset_notice() {
    NOTICE_EMITTED.store(false, std::sync::atomic::Ordering::SeqCst);
}

/// Apply the rule to the policy a spawn is about to be confined by, and say so
/// once per process. The spawn path calls this so every surface sees the same
/// warning.
pub(crate) fn reconcile(mut policy: CapabilityPolicy) -> CapabilityPolicy {
    if let Some(unavailable) = withhold_unsupported_loopback(&mut policy.process_sandbox) {
        if !NOTICE_EMITTED.swap(true, std::sync::atomic::Ordering::SeqCst) {
            crate::events::log_warn_meta(
                LOOPBACK_UNAVAILABLE_CATEGORY,
                &unavailable.notice(),
                unavailable.metadata(),
            );
        }
    }
    policy
}

#[cfg(test)]
#[path = "loopback_tests.rs"]
mod tests;
