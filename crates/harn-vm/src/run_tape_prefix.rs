//! What a branch prefix may do besides model and tool calls.
//!
//! While a branch replays its prefix, every builtin and capability call is
//! checked against its registry effect contract. Clock, random and env reads
//! are served from the recording. The runtime store runs on an overlay seeded
//! with what the recorded run opened, and persistent cache writes were already
//! made by the recorded run. Reads of the restored workspace, state, host facts
//! and secrets run. Anything else would act on the world a second time, so it
//! is refused and the branch diverges.

use std::path::Path;
use std::sync::atomic::Ordering;

use harn_builtin_meta::{CapabilityId, EffectAccess, EffectKind, EffectSpec, ResourceSelector};

use super::{
    process_tape, recorded_lookup, BranchState, RecordingLevel, TapeRecordKind, PREFIX_ACTIVE,
    PROCESS_BRANCH, RECORDING_FULL,
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
            EffectKind::State if branch_owns_state(owner) || process_local(effect) => {}
            _ => return Verdict::Refuse(*effect),
        }
    }
    if serve {
        Verdict::Serve
    } else {
        Verdict::Run
    }
}

/// State writes the branch itself absorbs: the runtime store's overlay and
/// the persistent cache, whose prefix writes the recorded run already made.
fn branch_owns_state(owner: Owner<'_>) -> bool {
    match owner {
        Some((CapabilityId::Runtime, method)) => method.starts_with("store_"),
        Some((CapabilityId::Llm, method)) => matches!(method, "cache_put" | "cache_clear"),
        _ => false,
    }
}

/// State that lives only in this process's VM, so rebuilding it during the
/// prefix touches nothing outside. Fails closed: a target not named here is
/// refused until someone confirms it does not persist.
const PROCESS_LOCAL_STATE: &[&str] = &[
    "agent-sessions",
    "capability-fixtures",
    "channel-guardrails",
    "command-policy",
    "llm-cost-budget",
    "llm-fixture",
    "path-scope-guard",
    "persona-hooks",
    "reminder-providers",
    "runtime-policy",
    "security-policy",
    "session-hooks",
    "shared-state",
    "step-hooks",
    "tool-hooks",
    "workflow-tool-hooks",
];

fn process_local(effect: &EffectSpec) -> bool {
    !effect.resources.is_empty()
        && effect.resources.iter().all(|resource| {
            matches!(resource, ResourceSelector::Constant(name) if PROCESS_LOCAL_STATE.contains(name))
        })
}

/// True while a branch replays its prefix.
pub(crate) fn prefix_active() -> bool {
    PREFIX_ACTIVE.load(Ordering::Acquire)
}

/// True while a full tape may need [`record_read`].
pub(crate) fn records_reads() -> bool {
    RECORDING_FULL.load(Ordering::Acquire) && !prefix_active()
}

/// Gate one builtin or capability call. `Some` is the call's answer in place
/// of running it: a recorded read, or the divergence that refuses it.
/// `effects: None` means the call declares no contract.
pub(crate) fn serve_read(
    owner: Owner<'_>,
    name: &str,
    effects: Option<&[EffectSpec]>,
    args: &[VmValue],
) -> Option<Result<VmValue, VmError>> {
    if !prefix_active() {
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
pub(crate) fn record_read(
    owner: Owner<'_>,
    name: &str,
    effects: Option<&[EffectSpec]>,
    args: &[VmValue],
    result: &Result<VmValue, VmError>,
) {
    if !records_reads() {
        return;
    }
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
                .payload_from_bytes(crate::canonical_json::to_vec(&json)),
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
    let mut guard = PROCESS_BRANCH.lock().expect("branch lock poisoned");
    let Some(state) = guard.as_mut() else {
        return Ok(());
    };
    if state.live {
        return Ok(());
    }
    let call = state.report.served_calls;
    Err(state.diverge(
        call,
        "refused effect",
        format!("`{name}` would run in the replayed prefix: {reason}"),
    ))
}

/// What a runtime store opened at `path` starts from.
pub(crate) struct OpenedStore {
    pub(crate) contents: Option<serde_json::Value>,
    /// The store lives in memory and never writes its file: a branch must not
    /// touch the store the recorded run left behind.
    pub(crate) overlay: bool,
}

/// Open the runtime store at `path`. A branch starts it from the recorded
/// run's opening contents, so a prefix read never sees what the store gained
/// after the recording; `read` loads the file otherwise. A full tape records
/// what the store opened with.
pub(crate) fn store_seed(
    path: &Path,
    read: impl FnOnce() -> Option<serde_json::Value>,
) -> Result<OpenedStore, VmError> {
    let seeded = {
        let mut guard = PROCESS_BRANCH.lock().expect("branch lock poisoned");
        match guard.as_mut() {
            None => None,
            Some(state) => {
                let id = store_identity(path, Some(&state.workspace_root));
                match state.store_seeds.get(&id) {
                    Some(seed) => Some(Some(seed.clone())),
                    None if state.live => Some(None),
                    None => {
                        let call = state.report.served_calls;
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
        }
    };
    let overlay = seeded.is_some();
    let contents = match seeded {
        Some(Some(seed)) => seed,
        _ => read(),
    };
    if let Some(tape) = process_tape().filter(|tape| tape.level == RecordingLevel::Full) {
        tape.recorder.record(TapeRecordKind::CacheLookup {
            namespace: STORE_SEED_NAMESPACE.to_string(),
            key: store_identity(path, tape.workspace_root()),
            after_calls: tape.next_llm_call.load(Ordering::SeqCst),
            value: contents.as_ref().map(|value| {
                tape.recorder
                    .payload_from_bytes(crate::canonical_json::to_vec(value))
            }),
        });
    }
    Ok(OpenedStore { contents, overlay })
}

/// A store's identity across the recorded workspace and a branch's copy of it.
fn store_identity(path: &Path, root: Option<&Path>) -> String {
    root.and_then(|root| path.strip_prefix(root).ok())
        .unwrap_or(path)
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
