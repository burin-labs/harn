//! Census of the cross-cutting behaviour in `dispatch_host_operation_with_ctx`,
//! and who owns each behaviour once a session is hosted over ACP.
//!
//! # Why this exists
//!
//! Before harn#5523, `host_call` had two implementations: harn-vm's canonical
//! dispatch, and an ACP adapter that re-registered the builtin by name and
//! forwarded straight to `host/call`. That split hid real defects — the
//! per-turn memo shipped without affecting ACP until it was duplicated.
//!
//! ACP now installs a [`harn_vm::HostCallBridge`] and keeps the stdlib
//! builtin, so Runtime-owned branches below are observed on the editor path.
//! This census still fails the build when a new call is added without
//! classifying ownership, and still records intentional Host-owned fallbacks
//! that ACP may answer first via the bridge.
//!
//! Tracked by harn#5562; convergence landed in harn#5523.

/// Who owns the semantics of a cross-cutting host-call behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticsOwner {
    /// The embedder owns this. Over ACP the editor implements the operation
    /// itself via the host bridge, so the canonical builtin fallback is for
    /// non-ACP / declining-bridge cases.
    Host,
    /// harn owns this, and an embedder cannot reimplement it correctly because
    /// it depends on runtime state the embedder cannot see (turn boundaries,
    /// command policy, mock registration).
    Runtime,
}

/// One call made by `dispatch_host_operation_with_ctx`.
#[derive(Debug, Clone, Copy)]
pub struct CrossCuttingBehaviour {
    /// The callee as it appears in the function body. The guard test extracts
    /// call targets from the source and matches them against this, so it must
    /// stay byte-identical to the code.
    pub callee: &'static str,
    /// Who owns the semantics.
    pub owner: SemanticsOwner,
    /// Whether the ACP `host_call` route reaches this behaviour today.
    pub acp_observes: bool,
    /// Issue tracking convergence, for entries ACP does not observe.
    pub tracked_by: Option<u32>,
    /// Why the disposition above is what it is.
    pub rationale: &'static str,
}

/// Every call the canonical `host_call` dispatch makes, classified.
///
/// Ordered as the function executes, so reading this top to bottom is reading
/// the dispatch.
pub const HOST_CALL_CROSS_CUTTING: &[CrossCuttingBehaviour] = &[
    CrossCuttingBehaviour {
        callee: "dispatch_host",
        owner: SemanticsOwner::Runtime,
        acp_observes: true,
        tracked_by: None,
        rationale: "Explicit Harness fixtures are runtime-owned deterministic authority. ACP \
                    keeps canonical dispatch, so a fixture bound to the current VM intercepts \
                    the host operation before any embedder bridge is consulted.",
    },
    CrossCuttingBehaviour {
        callee: "dispatch_mock_host_call",
        owner: SemanticsOwner::Runtime,
        acp_observes: true,
        tracked_by: None,
        rationale: "Host mocks are registered against harn's own registry. ACP keeps the \
                    stdlib host_call builtin (harn#5523), so mocked ops resolve here instead \
                    of leaking to the editor.",
    },
    CrossCuttingBehaviour {
        callee: "dispatch_process_exec_with_policy",
        owner: SemanticsOwner::Runtime,
        acp_observes: true,
        tracked_by: None,
        rationale: "Deny-patterns, approval gating and sandbox decisions are harn's. Direct \
                    `host_call(\"process.exec\", ...)` hits this path on ACP sessions; the \
                    editor-owned `exec`/`shell`/`run_command` builtins remain separate \
                    (see harn-serve host_ownership).",
    },
    CrossCuttingBehaviour {
        callee: "dispatch_process_spawn_with_policy",
        owner: SemanticsOwner::Runtime,
        acp_observes: true,
        tracked_by: None,
        rationale: "Non-blocking sibling of exec, gated identically. Observed on ACP because \
                    host_call is no longer replaced.",
    },
    CrossCuttingBehaviour {
        callee: "crate::stdlib::process_spawn::dispatch",
        owner: SemanticsOwner::Runtime,
        acp_observes: true,
        tracked_by: None,
        rationale: "poll/wait/kill/release operate on harn's in-process handle registry. ACP \
                    sessions share that registry through canonical dispatch.",
    },
    CrossCuttingBehaviour {
        callee: "async_builtin_cancel_token",
        owner: SemanticsOwner::Runtime,
        acp_observes: true,
        tracked_by: None,
        rationale: "Plumbs the caller's cancellation into the spawn registry. Reachable only \
                    through the entry above, so it shares its disposition.",
    },
    CrossCuttingBehaviour {
        callee: "turn_cache::cached_or",
        owner: SemanticsOwner::Runtime,
        acp_observes: true,
        tracked_by: None,
        rationale: "Turn boundaries are harn's; the memo lives inside canonical dispatch. \
                    ACP observes it by keeping the stdlib builtin (harn#5523). Backed by \
                    `harn-serve/src/adapters/acp/tests/host_call_turn_cache.rs`.",
    },
    CrossCuttingBehaviour {
        callee: "bridge.dispatch",
        owner: SemanticsOwner::Host,
        acp_observes: true,
        tracked_by: None,
        rationale: "The embedder's own hook. ACP's AcpHostCallBridge issues `host/call` \
                    JSON-RPC from this seam — same ownership, one dispatch path.",
    },
    CrossCuttingBehaviour {
        callee: "dispatch_builtin_host_operation",
        owner: SemanticsOwner::Host,
        acp_observes: false,
        tracked_by: None,
        rationale: "Fallback catalog (`process.list_shells`, `template.render`, \
                    `interaction.ask`, `project.metadata_*`, `workspace.*`) for embedders that \
                    do not implement an operation. Over ACP the editor usually answers via the \
                    bridge first, so bypassing the fallback is correct and needs no tracking \
                    issue. Listed so that a *new* arm added here is a deliberate `Host` \
                    decision rather than an unexamined one.",
    },
];

/// Call targets that appear in the dispatch body but carry no host-call
/// semantics, so they are not part of the census.
///
/// Declared rather than inferred: a heuristic that quietly dropped unfamiliar
/// callees would defeat the point of the guard.
pub const NON_BEHAVIOURAL_CALLS: &[&str] = &[
    // Constructs the extractor cannot distinguish from calls by shape alone.
    "Some",
    "Ok",
    "serde_json::json",
    "matches",
    // Reads a thread-local for logging attribution inside the `caller` payload;
    // it decides nothing.
    "crate::llm::current_agent_session_id",
    // Thread-local access plumbing for HOST_CALL_BRIDGE: take the registered
    // bridge out of the thread-local without holding the borrow across an await.
    "HOST_CALL_BRIDGE.with",
    "b.borrow",
    "clone",
    // The capability-first fixture lookup follows the current VM's explicit
    // root Harness and does not change host-call ownership or routing.
    "ctx.child_vm",
    "vm.harness",
    "and_then",
    "inner",
    "fixtures",
];

/// Extract the body of `dispatch_host_operation_with_ctx` from the source.
///
/// Reading the source rather than the AST keeps the guard dependency-free
/// and, more importantly, keeps it honest about *textual* additions: a new
/// branch is caught whether or not it type-checks into something familiar.
#[expect(
    clippy::string_slice,
    reason = "offsets come from find/char_indices of ASCII delimiters in the source"
)]
fn dispatch_body() -> String {
    let source = include_str!("../src/stdlib/host.rs");
    let start = source
        .find("pub async fn dispatch_host_operation_with_ctx")
        .expect("dispatch_host_operation_with_ctx not found — did it get renamed?");
    let open = source[start..]
        .find(") -> Result<VmValue, VmError> {")
        .expect("could not find the signature terminator")
        + start;
    let rest = &source[open..];
    // Brace-match to the end of the function so the scan cannot silently
    // spill into whatever is defined after it.
    let mut depth = 0usize;
    for (idx, ch) in rest.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return rest[..=idx].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces while scanning dispatch_host_operation_with_ctx");
}

/// Every `path(` occurrence in the body, with comments stripped first so a
/// prose mention of a callee cannot satisfy or trip the guard.
fn called_paths(body: &str) -> Vec<String> {
    let without_comments: String = body
        .lines()
        .map(|line| match line.find("//") {
            #[expect(clippy::string_slice, reason = "idx is a find offset on line")]
            Some(idx) => &line[..idx],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");

    let bytes: Vec<char> = without_comments.chars().collect();
    let mut out = Vec::new();
    for (idx, ch) in bytes.iter().enumerate() {
        if *ch != '(' {
            continue;
        }
        let mut start = idx;
        while start > 0 {
            let prev = bytes[start - 1];
            // `!` is in the set so a macro invocation yields `name!` rather
            // than stopping the walk-back dead and vanishing from the scan.
            if prev.is_alphanumeric() || prev == '_' || prev == ':' || prev == '.' || prev == '!' {
                start -= 1;
            } else {
                break;
            }
        }
        if start == idx {
            continue;
        }
        let path: String = bytes[start..idx].iter().collect();
        let path = path
            .trim_start_matches('.')
            .trim_end_matches('!')
            .to_string();
        if path.is_empty() || path.chars().next().is_some_and(|c| c.is_numeric()) {
            continue;
        }
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

#[test]
fn every_call_in_the_canonical_dispatch_is_classified() {
    let body = dispatch_body();
    let calls = called_paths(&body);

    let unclassified: Vec<&String> = calls
        .iter()
        .filter(|call| {
            !HOST_CALL_CROSS_CUTTING
                .iter()
                .any(|entry| entry.callee == call.as_str())
                && !NON_BEHAVIOURAL_CALLS.contains(&call.as_str())
        })
        .collect();

    assert!(
        unclassified.is_empty(),
        "dispatch_host_operation_with_ctx calls {unclassified:?}, which is not in \
         HOST_CALL_CROSS_CUTTING or NON_BEHAVIOURAL_CALLS.\n\n\
         Classify it here — `SemanticsOwner::Host` if the embedder is meant to serve it, \
         `SemanticsOwner::Runtime` if it depends on runtime state an embedder cannot see. \
         After harn#5523 ACP keeps the stdlib host_call builtin, so Runtime entries should \
         normally set `acp_observes: true`. See harn#5562."
    );
}

#[test]
fn the_census_describes_only_calls_that_exist() {
    let body = dispatch_body();
    let calls = called_paths(&body);

    for entry in HOST_CALL_CROSS_CUTTING {
        assert!(
            calls.iter().any(|call| call == entry.callee),
            "HOST_CALL_CROSS_CUTTING lists `{}`, but dispatch_host_operation_with_ctx no \
             longer calls it. A census that describes code that is gone stops being \
             evidence — drop the entry, or fix the `callee` if it was renamed. Calls found: \
             {calls:?}",
            entry.callee
        );
    }

    for call in &NON_BEHAVIOURAL_CALLS.iter().collect::<Vec<_>>() {
        assert!(
            calls.iter().any(|found| &found.as_str() == *call),
            "NON_BEHAVIOURAL_CALLS lists `{call}`, which no longer appears in the dispatch. \
             Stale exemptions silently widen the guard's blind spot; remove it."
        );
    }
}

#[test]
fn runtime_owned_divergences_carry_a_tracking_issue() {
    for entry in HOST_CALL_CROSS_CUTTING {
        if entry.owner == SemanticsOwner::Runtime && !entry.acp_observes {
            assert!(
                entry.tracked_by.is_some(),
                "`{}` is Runtime-owned and not observed over ACP, which makes it a known \
                 defect rather than a design choice. Record the issue tracking convergence \
                 so it cannot become permanent by inattention.",
                entry.callee
            );
        }
        if entry.owner == SemanticsOwner::Host {
            assert!(
                entry.tracked_by.is_none(),
                "`{}` is Host-owned, so ACP serving it differently is intended and there is \
                 nothing to converge. A tracking issue here means the disposition is wrong.",
                entry.callee
            );
        }
    }
}

/// The regression that motivated the whole census.
#[test]
fn the_per_turn_memo_is_observed_over_acp() {
    let memo = HOST_CALL_CROSS_CUTTING
        .iter()
        .find(|entry| entry.callee == "turn_cache::cached_or")
        .expect("the per-turn memo must stay in the census");
    assert_eq!(memo.owner, SemanticsOwner::Runtime);
    assert!(
        memo.acp_observes,
        "The per-turn memo must remain on the ACP route after harn#5523. If this is false \
         again, ACP has stopped sharing canonical dispatch — see \
         harn-serve/src/adapters/acp/tests/host_call_turn_cache.rs."
    );
}

/// After #5523 every Runtime-owned cross-cutting branch is on the ACP path.
#[test]
fn every_runtime_owned_branch_is_observed_over_acp() {
    for entry in HOST_CALL_CROSS_CUTTING {
        if entry.owner == SemanticsOwner::Runtime {
            assert!(
                entry.acp_observes,
                "`{}` is Runtime-owned but acp_observes=false — that reopens the dual-route \
                 defect #5523 closed.",
                entry.callee
            );
        }
    }
}

/// An entry without a stated reason is a label, not a census.
#[test]
fn every_entry_states_its_reasoning() {
    for entry in HOST_CALL_CROSS_CUTTING {
        assert!(
            entry.rationale.len() > 40,
            "`{}` needs a rationale explaining why its owner and acp_observes are what they \
             are, not a placeholder.",
            entry.callee
        );
    }
}
