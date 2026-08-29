//! The other half of the host-call divergence guard (harn#5562 / harn#5523).
//!
//! `crates/harn-vm/tests/acp_host_call_parity.rs` guards the canonical dispatch:
//! it fails when a new cross-cutting branch is added there without recording
//! whether ACP observes it.
//!
//! This side declares every builtin the ACP adapter replaces. Growing that set
//! is a decision that has to be written down — `register_builtin` inserts by
//! name, so an undeclared replacement silently detaches the stdlib path.
//!
//! After harn#5523, `host_call` is **not** in this set: ACP installs a
//! `HostCallBridge` and keeps the stdlib builtin. The drift guard still fails
//! if a new overlapping host-dispatch builtin is introduced without being
//! allowlisted here with `shadows_host_dispatch: true`.

/// A builtin the ACP adapter registers over the stdlib's.
struct ShadowedBuiltin {
    name: &'static str,
    /// Whether shadowing this one detaches it from a harn-vm dispatch path that
    /// carries runtime-owned semantics. `false` means the stdlib version has
    /// nothing an embedder needs to inherit.
    shadows_host_dispatch: bool,
    rationale: &'static str,
}

/// Every builtin `register_acp_builtins` installs.
const ACP_SHADOWED_BUILTINS: &[ShadowedBuiltin] = &[
    ShadowedBuiltin {
        name: "log",
        shadows_host_dispatch: false,
        rationale: "Presentation. Routes output to the ACP session/update channel instead of \
                    stdio; no host dispatch involved.",
    },
    ShadowedBuiltin {
        name: "print",
        shadows_host_dispatch: false,
        rationale: "Presentation. Same redirection to the session/update channel as `log`, \
                    without the trailing newline; no host dispatch involved.",
    },
    ShadowedBuiltin {
        name: "println",
        shadows_host_dispatch: false,
        rationale: "Presentation. Same redirection as `print`, with a trailing newline; no host \
                    dispatch involved.",
    },
    ShadowedBuiltin {
        name: "host_capabilities",
        shadows_host_dispatch: false,
        rationale: "Serves the manifest captured during the `host/capabilities` handshake. The \
                    stdlib version reads an embedder-registered manifest, so this is the same \
                    contract over a different transport rather than a bypass.",
    },
    ShadowedBuiltin {
        name: "host_has",
        shadows_host_dispatch: false,
        rationale: "Pure predicate over the same handshake manifest.",
    },
    ShadowedBuiltin {
        name: "run_command",
        shadows_host_dispatch: false,
        rationale: "Execution is editor-owned by design over ACP — the editor owns the terminal, \
                    approval UX and undo. Deliberate divergence from stdlib `run_command`, kept \
                    distinct from `host_call` routing (see host_ownership.rs).",
    },
    ShadowedBuiltin {
        name: "exec",
        shadows_host_dispatch: false,
        rationale: "Editor-owned execution, like `run_command`. Note this one is \
                    `unregister_builtin`ed before being re-registered, so the stdlib version is \
                    removed rather than shadowed.",
    },
    ShadowedBuiltin {
        name: "shell",
        shadows_host_dispatch: false,
        rationale: "Editor-owned execution, like `run_command`; also unregistered before being \
                    re-registered rather than merely shadowed.",
    },
    ShadowedBuiltin {
        name: "trace_end",
        shadows_host_dispatch: false,
        rationale: "Presentation: closes a trace span on the session/update channel.",
    },
    ShadowedBuiltin {
        name: "progress",
        shadows_host_dispatch: false,
        rationale: "Presentation: progress notifications to the editor.",
    },
    ShadowedBuiltin {
        name: "emit_response",
        shadows_host_dispatch: false,
        rationale: "Presentation: emits the assistant-visible response text.",
    },
];

/// Names registered through the `for level in [...]` loop rather than a literal
/// call, so the source scan cannot see them as string arguments.
const TRACE_LEVEL_BUILTINS: &[&str] = &["trace_end"];

/// Every builtin name passed to a `register_*` call in `builtins.rs`.
fn registered_builtin_names() -> Vec<String> {
    let source = include_str!("../src/adapters/acp/builtins.rs");
    let mut out = Vec::new();
    for line in source.lines() {
        let trimmed = line.trim_start();
        // Only the registration calls, and only the literal-name form. The
        // `for level in [...]` loop registers by variable; those names are
        // covered by TRACE_LEVEL_BUILTINS and asserted separately.
        if !trimmed.starts_with("vm.register_builtin(")
            && !trimmed.starts_with("vm.register_async_builtin(")
        {
            continue;
        }
        let Some((_, rest)) = trimmed.split_once('"') else {
            continue;
        };
        let Some((name, _)) = rest.split_once('"') else {
            continue;
        };
        let name = name.to_string();
        if !out.contains(&name) {
            out.push(name);
        }
    }
    out
}

#[test]
fn every_builtin_the_adapter_replaces_is_declared() {
    let registered = registered_builtin_names();
    assert!(
        !registered.is_empty(),
        "found no `vm.register_*builtin(\"...\")` calls in builtins.rs — the scan broke, which \
         would make this guard pass vacuously forever"
    );

    let undeclared: Vec<&String> = registered
        .iter()
        .filter(|name| {
            !ACP_SHADOWED_BUILTINS
                .iter()
                .any(|entry| entry.name == name.as_str())
        })
        .collect();

    assert!(
        undeclared.is_empty(),
        "register_acp_builtins installs {undeclared:?}, which is not declared in \
         ACP_SHADOWED_BUILTINS.\n\n\
         `register_builtin` inserts by name, so this DETACHES the builtin from whatever harn-vm \
         does inside its own dispatch — silently, and with no test failure. That is precisely how \
         the per-turn memo was inert on the ACP route for a full release (harn#5190 -> \
         a downstream host regression).\n\n\
         Declare it here with `shadows_host_dispatch` set honestly. If it is true, the stdlib \
         version's behaviour needs classifying in harn-vm's ACP host_call census too. See \
         harn#5562 / harn#5523."
    );
}

#[test]
fn the_declaration_describes_only_builtins_that_exist() {
    let registered = registered_builtin_names();
    for entry in ACP_SHADOWED_BUILTINS {
        let found = registered.iter().any(|name| name == entry.name)
            || TRACE_LEVEL_BUILTINS.contains(&entry.name);
        assert!(
            found,
            "ACP_SHADOWED_BUILTINS declares `{}`, but the adapter no longer registers it. A \
             declaration that outlives its code stops being evidence — drop the entry.",
            entry.name
        );
    }
}

/// After #5523, no ACP registration may shadow a host-dispatch builtin.
/// Reintroducing `host_call` (or adding `host_tool_call`) without flipping
/// this assertion is exactly the defect class the guard exists to catch.
#[test]
fn no_shadowed_host_dispatch_builtin_is_allowlisted() {
    let shadowing: Vec<&str> = ACP_SHADOWED_BUILTINS
        .iter()
        .filter(|entry| entry.shadows_host_dispatch)
        .map(|entry| entry.name)
        .collect();
    assert!(
        shadowing.is_empty(),
        "ACP must not replace host-dispatch builtins after harn#5523 (found {shadowing:?}). \
         Install a HostCallBridge instead, or deliberately extend the census before relaxing \
         this assertion."
    );
    assert!(
        !registered_builtin_names()
            .iter()
            .any(|name| name == "host_call"),
        "register_acp_builtins must not re-register host_call; that reopens the dual-route \
         defect tracked by harn#5523"
    );
}

#[test]
fn every_declaration_states_its_reasoning() {
    for entry in ACP_SHADOWED_BUILTINS {
        assert!(
            entry.rationale.len() > 40,
            "`{}` needs a rationale for its `shadows_host_dispatch` value, not a placeholder.",
            entry.name
        );
    }
}
