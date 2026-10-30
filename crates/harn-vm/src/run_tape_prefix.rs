//! What a branch prefix may do besides model and tool calls.
//!
//! While a branch replays its prefix, every builtin and capability call is
//! checked against its registry effect contract. Clock, random and env reads
//! are served from the recording. The runtime store opens with what the
//! recorded run opened. Durable state writes land in the branch's own state
//! root (see `scope`). Reads of the restored workspace, state, host facts and
//! secrets run. Anything else would act on the world a second time, so it is
//! refused and the branch diverges.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use harn_builtin_meta::{CapabilityId, EffectAccess, EffectKind, EffectSpec, ResourceSelector};

use super::{
    process_tape, recorded_lookup, BranchState, RecordingLevel, TapeRecordKind, RECORDING_FULL,
};
use crate::value::{VmError, VmValue};

/// Tape namespace for each runtime store's opening contents.
pub(super) const STORE_SEED_NAMESPACE: &str = "harn.runtime.store";
/// Tape namespace for clock, random and env reads.
const READ_NAMESPACE: &str = "harn.read";

/// The capability method a call belongs to, when it has one.
pub(crate) type Owner<'a> = Option<(CapabilityId, &'a str)>;

enum Verdict {
    Run,
    Serve,
    Refuse(EffectSpec),
}

fn verdict(owner: Owner<'_>, effects: &[EffectSpec]) -> Verdict {
    let mut serve = false;
    for effect in effects {
        let reads = matches!(effect.access, EffectAccess::Read | EffectAccess::Observe);
        match effect.kind {
            EffectKind::Stdio
            | EffectKind::Observability
            | EffectKind::Llm
            | EffectKind::Tool
            | EffectKind::Worker => {}
            EffectKind::Clock | EffectKind::Env if reads => serve = true,
            EffectKind::Random => serve = true,
            // Sleeping and setting this process's env change nothing outside it.
            EffectKind::Clock | EffectKind::Env => {}
            EffectKind::Fs | EffectKind::Host | EffectKind::Secret | EffectKind::State if reads => {
            }
            EffectKind::Network if reads && gates_own_network_reads(owner) => {}
            EffectKind::State
                if branch_owns_state(owner) || process_local(effect) || session_entry(owner) => {}
            _ => return Verdict::Refuse(*effect),
        }
    }
    if serve {
        Verdict::Serve
    } else {
        Verdict::Run
    }
}

/// State writes that land in the branch's own state root: the runtime store
/// and the persistent model cache.
pub(super) fn branch_owns_state(owner: Owner<'_>) -> bool {
    match owner {
        Some((CapabilityId::Runtime, method)) => method.starts_with("store_"),
        Some((CapabilityId::Llm, method)) => matches!(method, "cache_put" | "cache_clear"),
        _ => false,
    }
}

/// Methods whose network read is conditional and refused at its own seam when
/// it would happen: the capability probe reaches the network only for a
/// self-hosted provider it has not measured yet.
fn gates_own_network_reads(owner: Owner<'_>) -> bool {
    matches!(
        owner,
        Some((CapabilityId::Llm, "probe_provider_capabilities"))
    )
}

/// These methods reach the command recorder before policy or execution.
fn runs_a_process(owner: Owner<'_>) -> bool {
    match owner {
        Some((CapabilityId::Tools, "run_command")) => true,
        Some((CapabilityId::Process, method)) => matches!(
            method,
            "run"
                | "exec"
                | "shell"
                | "exec_at"
                | "shell_at"
                | "git_status"
                | "git_diff"
                | "git_merge_base"
                | "git_tag_list"
                | "git_describe"
                | "git_repo_discover"
        ),
        _ => false,
    }
}

/// `harness.agent` methods whose state target, the session id argument, is one
/// entry of the in-process session table that `agent-sessions` names. Durable
/// agent state (`state_write`, `state_delete`, `state_handoff`) and live-client
/// routing stay off this list and are refused.
pub(super) const SESSION_ENTRY_METHODS: &[&str] = &[
    "add_root",
    "begin_goal_turn",
    "claim_tool_format",
    "clear_goal",
    "clear_scratchpad",
    "close",
    "compact",
    "compact_transcript",
    "drain_inbox",
    "end_goal_turn",
    "fork",
    "fork_at",
    "inject_feedback",
    "inject_host_event",
    "inject",
    "post_event",
    "reanchor",
    "redo",
    "remove_root",
    "reset",
    "rollback",
    "set_goal",
    "set_scratchpad",
    "set_workspace_anchor",
    "set_workspace_policy",
    "transcript_clear_reminders",
    "transcript_inject_reminder",
    "trim",
];

fn session_entry(owner: Owner<'_>) -> bool {
    matches!(owner, Some((CapabilityId::Agent, method)) if SESSION_ENTRY_METHODS.contains(&method))
}

/// State that lives only in this process's VM, so rebuilding it during the
/// prefix touches nothing outside. Fails closed: a target not named here is
/// refused until someone confirms it does not persist.
pub(super) const PROCESS_LOCAL_STATE: &[&str] = &[
    "agent-sessions",
    "atomics",
    "capability-fixtures",
    "checkpoint-hooks",
    "circuits",
    "channel-guardrails",
    "channels",
    "command-policy",
    "egress-policy",
    "http-fixtures",
    "llm-cost-budget",
    "llm-fixture",
    "llm-config",
    "path-scope-guard",
    "pipeline-hooks",
    "provider-capabilities",
    "persona-hooks",
    "reminder-providers",
    "runtime-policy",
    "security-policy",
    "session-hooks",
    "shared-state",
    "sse-mocks",
    "step-hooks",
    "streaming-transport-fixtures",
    "tool-hooks",
    "tool-synthesis-cache",
    "websocket-mocks",
    "workflow-tool-hooks",
];

pub(super) fn process_local(effect: &EffectSpec) -> bool {
    !effect.resources.is_empty()
        && effect.resources.iter().all(|resource| {
            matches!(resource, ResourceSelector::Constant(name) if PROCESS_LOCAL_STATE.contains(name))
        })
}

/// True while a branch replays its prefix.
pub(crate) fn prefix_active() -> bool {
    super::current_branch().is_some_and(|branch| branch.in_prefix())
}

/// True while a full tape may need [`record_read`].
pub(crate) fn records_reads() -> bool {
    RECORDING_FULL.load(Ordering::Acquire) && !prefix_active()
}

/// True while a call may be served from or recorded on a tape.
pub(crate) fn taping() -> bool {
    RECORDING_FULL.load(Ordering::Acquire) || prefix_active()
}

/// Gate one builtin or capability call. `Some` is the call's answer in place
/// of running it: a recorded read, or the divergence that refuses it.
/// `effects: None` means the call declares no contract.
#[inline]
pub(crate) fn serve_read(
    owner: Owner<'_>,
    name: &str,
    effects: Option<&[EffectSpec]>,
    args: &[VmValue],
) -> Option<Result<VmValue, VmError>> {
    if !prefix_active() {
        return None;
    }
    serve_read_in_prefix(owner, name, effects, args)
}

// Out of line so the dispatch frames that call `serve_read` stay small.
#[cold]
#[inline(never)]
fn serve_read_in_prefix(
    owner: Owner<'_>,
    name: &str,
    effects: Option<&[EffectSpec]>,
    args: &[VmValue],
) -> Option<Result<VmValue, VmError>> {
    if runs_a_process(owner) {
        return None;
    }
    let Some(effects) = effects else {
        return refuse_in_prefix(name, "it declares no effect contract")
            .err()
            .map(Err);
    };
    match verdict(owner, effects) {
        Verdict::Run => None,
        Verdict::Refuse(effect) => refuse_in_prefix(
            name,
            &format!(
                "its {:?} {:?} effect is not on the tape",
                effect.kind, effect.access
            ),
        )
        .err()
        .map(Err),
        Verdict::Serve => {
            // The branch went live between the flag and the lookup: run it.
            let mut went_live = false;
            let served =
                recorded_lookup(READ_NAMESPACE, &read_key(owner, name, args), false, || {
                    went_live = true;
                    Ok(None)
                });
            if went_live {
                return None;
            }
            Some(served.map(|value| {
                value.map_or(VmValue::Nil, |value| {
                    crate::schema::json_to_vm_value(&value)
                })
            }))
        }
    }
}

/// Record a clock, random or env read the program just made, if a full tape
/// is recording. A failed read is not recorded, so a branch reaching it
/// diverges instead of inventing an answer.
#[inline]
pub(crate) fn record_read(
    owner: Owner<'_>,
    name: &str,
    effects: Option<&[EffectSpec]>,
    args: &[VmValue],
    result: &Result<VmValue, VmError>,
) {
    if records_reads() {
        record_read_on_tape(owner, name, effects, args, result);
    }
}

#[cold]
#[inline(never)]
fn record_read_on_tape(
    owner: Owner<'_>,
    name: &str,
    effects: Option<&[EffectSpec]>,
    args: &[VmValue],
    result: &Result<VmValue, VmError>,
) {
    let (Some(effects), Ok(value)) = (effects, result) else {
        return;
    };
    if !matches!(verdict(owner, effects), Verdict::Serve) {
        return;
    }
    let Some(tape) = process_tape().filter(|tape| tape.level == RecordingLevel::Full) else {
        return;
    };
    let json = crate::llm::helpers::vm_value_to_json(value);
    tape.recorder.record(TapeRecordKind::CacheLookup {
        namespace: READ_NAMESPACE.to_string(),
        key: read_key(owner, name, args),
        after_calls: tape.next_llm_call.load(Ordering::SeqCst),
        value: Some(
            tape.recorder
                .payload_from_bytes(serde_json::to_vec(&json).unwrap_or_default()),
        ),
    });
}

/// A random value the runtime draws for itself, outside any builtin, that
/// reaches a model request. A full tape records it and a branch prefix
/// replays it. If the prefix diverges here, the next model call fails
/// closed, so the fresh draw returned never reaches a provider.
pub(crate) fn taped_random(name: &str, draw: impl Fn() -> String) -> String {
    let key = format!("runtime.{name}");
    match recorded_lookup(READ_NAMESPACE, &key, false, || {
        Ok(Some(serde_json::Value::String(draw())))
    }) {
        Ok(Some(serde_json::Value::String(value))) => value,
        _ => draw(),
    }
}

fn read_key(owner: Owner<'_>, name: &str, args: &[VmValue]) -> String {
    let args = serde_json::Value::Array(
        args.iter()
            .map(crate::llm::helpers::vm_value_to_json)
            .collect(),
    );
    let name = owner.map_or_else(
        || name.to_string(),
        |(capability, method)| format!("{capability:?}.{method}"),
    );
    format!("{name}:{}", super::args_digest(&args))
}

/// Refuse `name` if a branch is replaying its prefix: it would act on the
/// world a second time. Used directly where a call has no builtin contract
/// (host permission prompts, native decisions, MCP calls, bridge builtins).
pub(crate) fn refuse_in_prefix(name: &str, reason: &str) -> Result<(), VmError> {
    if !prefix_active() {
        return Ok(());
    }
    let branch = super::current_branch();
    let mut guard = branch.as_ref().map(|branch| branch.state());
    let Some(state) = guard.as_mut() else {
        return Ok(());
    };
    if state.live {
        return Ok(());
    }
    let call = state.report.served_calls + 1;
    Err(state.diverge(
        call,
        "refused effect",
        format!("`{name}` would run in the replayed prefix: {reason}"),
    ))
}

/// Open the runtime store at `path`. A branch starts it from the recorded
/// run's opening contents, so a prefix read never sees what the store gained
/// after the recording; `read` loads the file otherwise. A full tape records
/// what the store opened with.
pub(crate) fn store_seed(
    path: &Path,
    read: impl FnOnce() -> Option<serde_json::Value>,
) -> Result<Option<serde_json::Value>, VmError> {
    let seeded = {
        let branch = super::current_branch();
        let mut guard = branch.as_ref().map(|branch| branch.state());
        match (branch.as_ref(), guard.as_mut()) {
            (Some(branch), Some(state)) => {
                // `path` has already been remapped under the branch's root.
                let id = store_identity(path, &[(branch.state_root().to_path_buf(), "")]);
                match state.store_seeds.get(&id) {
                    Some(seed) => Some(Some(seed.clone())),
                    None if state.live => Some(None),
                    None => {
                        let call = state.report.served_calls + 1;
                        return Err(state.diverge(
                            call,
                            "runtime store",
                            format!(
                                "the program opened store `{id}`, which the recording never opened"
                            ),
                        ));
                    }
                }
            }
            _ => None,
        }
    };
    let contents = match seeded {
        Some(Some(seed)) => seed,
        _ => read(),
    };
    if let Some(tape) = process_tape().filter(|tape| tape.level == RecordingLevel::Full) {
        tape.recorder.record(TapeRecordKind::CacheLookup {
            namespace: STORE_SEED_NAMESPACE.to_string(),
            key: store_identity(
                path,
                &tape
                    .workspace_root()
                    .map(|root| {
                        vec![
                            (crate::runtime_paths::state_root(root), ""),
                            (root.to_path_buf(), "workspace"),
                        ]
                    })
                    .unwrap_or_default(),
            ),
            after_calls: tape.next_llm_call.load(Ordering::SeqCst),
            value: contents.as_ref().map(|value| {
                tape.recorder
                    .payload_from_bytes(serde_json::to_vec(value).unwrap_or_default())
            }),
        });
    }
    Ok(contents)
}

/// A store's identity across the recorded run and a branch: where it lands
/// under a branch's state root. `roots` pairs each root with its mount there,
/// the state root before the workspace that may hold it.
fn store_identity(path: &Path, roots: &[(PathBuf, &str)]) -> String {
    roots
        .iter()
        .find_map(|(root, mount)| {
            path.strip_prefix(root)
                .ok()
                .map(|rest| Path::new(mount).join(rest))
        })
        .unwrap_or_else(|| path.to_path_buf())
        .display()
        .to_string()
}

impl BranchState {
    /// Record the branch's first divergence and return the error that
    /// refuses the call that caused it.
    pub(super) fn diverge(&mut self, call: u64, what: &str, detail: String) -> VmError {
        let divergence = self
            .report
            .divergence
            .get_or_insert(super::BranchDivergence {
                call,
                what: what.to_string(),
                detail,
            });
        super::divergence_error(divergence)
    }
}
