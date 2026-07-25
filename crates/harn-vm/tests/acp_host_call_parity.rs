//! Census of the cross-cutting behaviour in `dispatch_host_operation_with_ctx`,
//! and who owns each behaviour once a session is hosted over ACP.
//!
//! # Why this exists
//!
//! `host_call` has two implementations. harn-vm's canonical dispatch is one;
//! `harn-serve`'s ACP adapter re-registers the `host_call` builtin outright and
//! is the other. Registration is by name, so the ACP one *shadows* the stdlib
//! one entirely — and ACP is how the editor runs every agent turn.
//!
//! That means every branch of the canonical dispatch is, by default, invisible
//! to the surface that matters most. This has already cost a release: the
//! per-turn memo (harn#5190/#5207) shipped in v0.10.38 and did nothing on the
//! ACP route for its entire life. It was found by measuring a slow agent turn
//! (burin-labs/burin-code#5432), not by CI — and CI could not have found it,
//! because harn-vm's own turn-cache test drives the dispatch path ACP replaces,
//! so it passes either way.
//!
//! The individual gap is fixed (harn#5526). The *class* is not: the next
//! cross-cutting branch added below silently misses ACP too, and the default
//! outcome is silence rather than a failure.
//!
//! # What this does about it
//!
//! `HOST_CALL_CROSS_CUTTING` names every call the canonical dispatch makes,
//! records who owns its semantics, and records whether the ACP route observes
//! it *today*. A test asserts the census and the function agree in both
//! directions, so adding a branch without classifying it fails the build.
//!
//! The census is a ledger of the current truth, not an aspiration. An entry
//! with `acp_observes: false` and a `tracked_by` issue is a known, deliberate
//! divergence; flipping it to `true` is how the fix proves itself.
//!
//! Tracked by harn#5562; the convergence work it defends is harn#5523.

/// Who owns the semantics of a cross-cutting host-call behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SemanticsOwner {
    /// The embedder owns this. Over ACP the editor implements the operation
    /// itself, so the canonical dispatch's version is a fallback for non-ACP
    /// embedders and the ACP route skipping it is *correct*.
    Host,
    /// harn owns this, and an embedder cannot reimplement it correctly because
    /// it depends on runtime state the embedder cannot see (turn boundaries,
    /// command policy, mock registration). A route that skips it is a defect.
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
        callee: "dispatch_mock_host_call",
        owner: SemanticsOwner::Runtime,
        acp_observes: false,
        tracked_by: Some(5523),
        rationale: "Host mocks are registered against harn's own registry, which an editor \
                    cannot see. A mocked operation therefore still reaches the real editor \
                    over ACP, which is the opposite of what registering a mock asked for.",
    },
    CrossCuttingBehaviour {
        callee: "dispatch_process_exec_with_policy",
        owner: SemanticsOwner::Runtime,
        acp_observes: false,
        tracked_by: Some(5523),
        rationale: "Deny-patterns, approval gating and sandbox decisions are harn's, not the \
                    editor's. In practice ACP overrides `exec`/`shell`/`run_command` separately \
                    so ordinary execution is editor-owned by design, but a direct \
                    `host_call(\"process.exec\", ...)` from Harn code skips harn's gating \
                    entirely. Security-adjacent: converging this is a behaviour change that \
                    needs its own testing, which is why #5523 keeps it separate.",
    },
    CrossCuttingBehaviour {
        callee: "dispatch_process_spawn_with_policy",
        owner: SemanticsOwner::Runtime,
        acp_observes: false,
        tracked_by: Some(5523),
        rationale: "Non-blocking sibling of exec, deliberately gated identically. Same \
                    divergence and the same reason for deferring it.",
    },
    CrossCuttingBehaviour {
        callee: "crate::stdlib::process_spawn::dispatch",
        owner: SemanticsOwner::Runtime,
        acp_observes: false,
        tracked_by: Some(5523),
        rationale: "poll/wait/kill/release operate on harn's in-process handle registry for an \
                    already-gated spawn. An editor has no such registry, so these cannot be \
                    served host-side at all.",
    },
    CrossCuttingBehaviour {
        callee: "async_builtin_cancel_token",
        owner: SemanticsOwner::Runtime,
        acp_observes: false,
        tracked_by: Some(5523),
        rationale: "Plumbs the caller's cancellation into the spawn registry. Reachable only \
                    through the entry above, so it shares its disposition.",
    },
    CrossCuttingBehaviour {
        callee: "turn_cache::cached_or",
        owner: SemanticsOwner::Runtime,
        acp_observes: true,
        tracked_by: None,
        rationale: "Turn boundaries are harn's; the memo is keyed on an epoch the editor cannot \
                    observe. This is the entry that was silently false for a whole release \
                    (harn#5190 shipped, ACP never saw it, found in \
                    burin-labs/burin-code#5432). harn#5526 hoisted the allowlist and the \
                    epoch-tagged store into harn-vm so both routes share one owner; \
                    `acp_observes` is backed by \
                    `harn-serve/src/adapters/acp/tests/host_call_turn_cache.rs`, which drives \
                    the ACP route rather than the path ACP replaces.",
    },
    CrossCuttingBehaviour {
        callee: "bridge.dispatch",
        owner: SemanticsOwner::Host,
        acp_observes: true,
        tracked_by: None,
        rationale: "The embedder's own hook. ACP's equivalent is the `host/call` JSON-RPC \
                    request, so both routes reach the embedder — by different transports, \
                    which is the intended difference rather than a divergence.",
    },
    CrossCuttingBehaviour {
        callee: "dispatch_builtin_host_operation",
        owner: SemanticsOwner::Host,
        acp_observes: false,
        tracked_by: None,
        rationale: "Fallback catalog (`process.list_shells`, `template.render`, \
                    `interaction.ask`, `project.metadata_*`, `workspace.*`) for embedders that \
                    do not implement an operation. Over ACP the editor is expected to serve \
                    these, so bypassing the fallback is correct and needs no tracking issue. \
                    Listed so that a *new* arm added here is a deliberate `Host` decision \
                    rather than an unexamined one.",
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
];

/// Extract the body of `dispatch_host_operation_with_ctx` from the source.
///
/// Reading the source rather than the AST keeps the guard dependency-free
/// and, more importantly, keeps it honest about *textual* additions: a new
/// branch is caught whether or not it type-checks into something familiar.
fn dispatch_body() -> String {
    let source = include_str!("../src/stdlib/host.rs");
    let start = source
        .find("pub(crate) async fn dispatch_host_operation_with_ctx")
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
         This is the guard doing its job. A new branch here does NOT reach ACP-hosted \
         sessions: the ACP adapter re-registers the `host_call` builtin and shadows this \
         function entirely, so whatever you just added is invisible to the surface the \
         editor uses for every agent turn.\n\n\
         Classify it in crates/harn-vm/src/stdlib/host/acp_parity.rs — `SemanticsOwner::Host` \
         if the embedder is meant to serve it, `SemanticsOwner::Runtime` if it depends on \
         runtime state an embedder cannot see. For a Runtime entry that ACP does not yet \
         observe, set `acp_observes: false` with a `tracked_by` issue rather than leaving it \
         unrecorded. See harn#5562."
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
///
/// Pinned as its own assertion rather than left implicit in the table: if
/// someone ever flips this back to `false` to make a test pass, that is the
/// v0.10.38 defect returning and it should require deleting a test that
/// says so.
#[test]
fn the_per_turn_memo_is_observed_over_acp() {
    let memo = HOST_CALL_CROSS_CUTTING
        .iter()
        .find(|entry| entry.callee == "turn_cache::cached_or")
        .expect("the per-turn memo must stay in the census");
    assert_eq!(memo.owner, SemanticsOwner::Runtime);
    assert!(
        memo.acp_observes,
        "The per-turn memo shipped in v0.10.38 and did nothing on the ACP route for its \
         entire life (harn#5190 -> burin-labs/burin-code#5432). harn#5526 fixed that. If \
         this is false again, the ACP route has stopped sharing the memo — see \
         harn-serve/src/adapters/acp/tests/host_call_turn_cache.rs."
    );
}

/// An entry without a stated reason is a label, not a census. The whole
/// value here is that the next person can tell a deliberate divergence
/// from an unexamined one without re-deriving it.
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
