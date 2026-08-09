//! Explicit ownership for ACP-hosted execution vs canonical `host_call`.
//!
//! #5523 removed the ACP `host_call` builtin replacement so editor sessions
//! share harn-vm's canonical dispatch. Editor ownership remains for the
//! *terminal builtins* (`exec`, `shell`, `run_command`) — that is deliberate,
//! not an accidental shadow of `host_call`.
//!
//! This module is the loud boundary: every ownership decision below is named,
//! classified, and asserted by tests. Growing or shrinking the set requires
//! editing the table, not discovering it from registration order.

#![allow(dead_code)] // Declared boundary; consumed by the unit tests below.

/// Who owns an operation when a session is hosted over ACP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AcpSurfaceOwner {
    /// The editor owns the concrete UX (terminal, approval, undo). ACP
    /// replaces the stdlib builtin.
    Editor,
    /// Harn owns the semantics (policy, registry, mocks, memo, fallbacks).
    /// Reached through canonical `host_call` dispatch + the ACP
    /// [`super::builtins::AcpHostCallBridge`].
    Runtime,
}

/// One ownership decision for an ACP-visible surface.
#[derive(Debug, Clone, Copy)]
pub(super) struct AcpOwnedSurface {
    pub name: &'static str,
    pub owner: AcpSurfaceOwner,
    pub rationale: &'static str,
}

/// Declared ownership for the surfaces #5523 must keep distinct.
///
/// Terminal builtins stay editor-owned. Direct `host_call("process.*", ...)`
/// and built-in host fallbacks stay runtime-owned so policy and registry
/// behaviour cannot be bypassed by forwarding to the editor first.
pub(super) const ACP_OWNED_SURFACES: &[AcpOwnedSurface] = &[
    AcpOwnedSurface {
        name: "exec",
        owner: AcpSurfaceOwner::Editor,
        rationale: "Editor owns the terminal, approval UX, and undo for interactive \
                    command execution. ACP unregisters then re-registers this builtin.",
    },
    AcpOwnedSurface {
        name: "shell",
        owner: AcpSurfaceOwner::Editor,
        rationale: "Same editor-owned terminal path as `exec`; also unregistered before \
                    re-registration rather than merely shadowed.",
    },
    AcpOwnedSurface {
        name: "run_command",
        owner: AcpSurfaceOwner::Editor,
        rationale: "Hostlib-facing alias of editor-owned terminal execution over ACP.",
    },
    AcpOwnedSurface {
        name: "host_call(process.exec)",
        owner: AcpSurfaceOwner::Runtime,
        rationale: "Direct host_call must pass harn command-policy/approval/sandbox \
                    preflight. It must not reach the editor first and skip gating.",
    },
    AcpOwnedSurface {
        name: "host_call(process.spawn)",
        owner: AcpSurfaceOwner::Runtime,
        rationale: "Non-blocking sibling of process.exec; gated identically by canonical \
                    dispatch before any embedder bridge runs.",
    },
    AcpOwnedSurface {
        name: "host_call(process.poll|wait|kill|release)",
        owner: AcpSurfaceOwner::Runtime,
        rationale: "Operate on harn's in-process spawn-handle registry. The editor has no \
                    such registry, so these cannot be served host-side.",
    },
    AcpOwnedSurface {
        name: "host_call builtin fallbacks",
        owner: AcpSurfaceOwner::Runtime,
        rationale: "Canonical fallbacks (interaction.ask, project.metadata_*, workspace.*, \
                    template.render, process.list_shells, ...) run when the ACP bridge \
                    declines. They stay in harn-vm so standalone and ACP routes share one \
                    catalog; the editor may still answer first via host/call.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_builtins_are_editor_owned() {
        for name in ["exec", "shell", "run_command"] {
            let entry = ACP_OWNED_SURFACES
                .iter()
                .find(|surface| surface.name == name)
                .unwrap_or_else(|| panic!("missing ownership row for {name}"));
            assert_eq!(entry.owner, AcpSurfaceOwner::Editor, "{name}");
            assert!(entry.rationale.len() > 40, "{name} needs a real rationale");
        }
    }

    #[test]
    fn process_host_call_surfaces_are_runtime_owned() {
        for name in [
            "host_call(process.exec)",
            "host_call(process.spawn)",
            "host_call(process.poll|wait|kill|release)",
            "host_call builtin fallbacks",
        ] {
            let entry = ACP_OWNED_SURFACES
                .iter()
                .find(|surface| surface.name == name)
                .unwrap_or_else(|| panic!("missing ownership row for {name}"));
            assert_eq!(entry.owner, AcpSurfaceOwner::Runtime, "{name}");
            assert!(entry.rationale.len() > 40, "{name} needs a real rationale");
        }
    }

    #[test]
    fn ownership_table_has_no_duplicate_names() {
        let mut seen = std::collections::BTreeSet::new();
        for entry in ACP_OWNED_SURFACES {
            assert!(
                seen.insert(entry.name),
                "duplicate ownership row for {}",
                entry.name
            );
        }
    }
}
