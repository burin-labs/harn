//! The other half of the host-call divergence guard (harn#5562).
//!
//! `crates/harn-vm/src/stdlib/host/acp_parity.rs` guards the canonical dispatch:
//! it fails when a new cross-cutting branch is added there without recording
//! whether ACP observes it. That guard is scoped to `host_call`, because
//! `host_call` is the one host-dispatch builtin this adapter currently shadows.
//!
//! Which leaves the same class open from this side. `register_builtin` inserts
//! by name, so re-registering a builtin here silently detaches it from
//! everything harn-vm does inside its own dispatch — with no diagnostic, and no
//! test failure, exactly as happened to the per-turn memo for a whole release.
//! Adding `host_tool_call` to the list below would reopen the class for tool
//! dispatch, and nothing today would say so.
//!
//! So: the set of builtins this adapter replaces is declared, not discovered.
//! Growing it is a decision that has to be written down.

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
        name: "host_call",
        shadows_host_dispatch: true,
        rationale: "THE one. Replaces `dispatch_host_operation_with_ctx` wholesale. Every branch \
                    of that function is classified in harn-vm's `acp_parity` census; the memo is \
                    shared explicitly (harn#5526) and the rest is tracked by harn#5523.",
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
                    approval UX and undo. Deliberate divergence, not an oversight; see the \
                    `process.exec` row of the harn-vm census for what that costs.",
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
        let Some(open) = trimmed.find('"') else {
            continue;
        };
        let rest = &trimmed[open + 1..];
        let Some(close) = rest.find('"') else {
            continue;
        };
        let name = rest[..close].to_string();
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
         burin-labs/burin-code#5432).\n\n\
         Declare it here with `shadows_host_dispatch` set honestly. If it is true, the stdlib \
         version's behaviour needs classifying in harn-vm's `acp_parity` census too. See harn#5562."
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

/// `host_call` should be the only one, and if that ever stops being true the
/// change should be deliberate enough to require editing this assertion.
#[test]
fn host_call_is_the_only_shadowed_host_dispatch_builtin() {
    let shadowing: Vec<&str> = ACP_SHADOWED_BUILTINS
        .iter()
        .filter(|entry| entry.shadows_host_dispatch)
        .map(|entry| entry.name)
        .collect();
    assert_eq!(
        shadowing,
        vec!["host_call"],
        "harn-vm has three host dispatch entry points — `dispatch_host_operation_with_ctx` \
         (host_call), `dispatch_host_tool_call_with_ctx` (host_tool_call) and \
         `dispatch_host_tool_list_with_ctx` (host_tool_list) — and this adapter shadows only the \
         first. The `acp_parity` census in harn-vm covers exactly that one function.\n\n\
         If tool dispatch is now shadowed too, the divergence class is open for it as well and \
         the census needs extending to cover the corresponding function before this assertion is \
         relaxed."
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
