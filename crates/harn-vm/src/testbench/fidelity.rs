//! Replay fidelity oracle.
//!
//! Compares two [`EventTape`]s and emits a structured [`FidelityReport`]
//! listing every diverging record. The oracle drives the
//! `harn test-bench fidelity` CLI subcommand and is the artifact behind
//! [harn-cloud#19][cloud19]'s "byte-for-byte vs. semantic" leaderboard
//! metric.
//!
//! [cloud19]: https://github.com/burin-labs/harn-cloud/issues/19
//!
//! # Modes
//!
//! - **`ByteIdentical`** (strictest). Every record must match position,
//!   kind, and content hash. The mode CI uses to gate "this PR did not
//!   regress replay determinism".
//!
//! - **`Semantic`**. Ignores diffs that are non-meaningful by
//!   construction: monotonic-only sequence numbers, virtual-time stamps
//!   that drift only because of paused-clock rounding, and recorded
//!   `monotonic_ms` deltas. Content hashes still gate every payload, so
//!   anything user-visible must still match.
//!
//! - **`Outcome`** (loosest). Compares only the script's externally
//!   observable result: the final FS write set (overlay diff), the exit
//!   status of the last subprocess, and the count of LLM calls. Useful
//!   for stochastic LLM runs where intermediate token streams legitimately
//!   diverge between record and replay.
//!
//! - **`PhaseAware`**. Compares `UserScript` records byte-identically and
//!   `RuntimeFinalize` records semantically, while ignoring
//!   runtime-finalize clock reads. This keeps user-script replay fixtures
//!   strict without making internal lifecycle observability changes
//!   regenerate every golden tape.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::tape::{EventTape, TapePhase, TapeRecord, TapeRecordKind};

/// Strictness of the comparison. The CLI surface picks one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FidelityMode {
    ByteIdentical,
    Semantic,
    Outcome,
    PhaseAware,
}

impl FidelityMode {
    pub fn parse(label: &str) -> Result<Self, String> {
        match label {
            "byte" | "byte-identical" | "byte_identical" => Ok(Self::ByteIdentical),
            "semantic" => Ok(Self::Semantic),
            "outcome" => Ok(Self::Outcome),
            "phase-aware" | "phase_aware" | "byte-user-semantic-runtime" => {
                Ok(Self::PhaseAware)
            }
            other => Err(format!(
                "unknown fidelity mode `{other}` — expected `byte-identical`, `semantic`, `outcome`, or `phase-aware`"
            )),
        }
    }
}

/// Structured comparison result. Emitted as JSON by the CLI; consumed by
/// CI gates and by the public leaderboard.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FidelityReport {
    pub mode: FidelityMode,
    /// Number of records compared on the recorded side. Lets a consumer
    /// distinguish "100% match over 0 records" from "100% over 1000".
    pub recorded_records: usize,
    /// Number of records compared on the replay side.
    pub replay_records: usize,
    /// Records where one side had an event the other did not, or where
    /// the same-position record had different content. Stable order so
    /// CI snapshots are deterministic.
    pub divergences: Vec<Divergence>,
    /// 0.0–1.0. Defined as `1 - (divergences / max(records, 1))` for
    /// strict modes; in outcome mode it's `1.0` iff zero divergences.
    pub score: f32,
}

impl FidelityReport {
    pub fn is_byte_identical(&self) -> bool {
        self.divergences.is_empty()
    }
}

/// Single divergence between two tapes. Records are paired by position
/// for byte-identical / semantic modes; outcome mode emits one
/// of these per outcome facet that disagreed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Divergence {
    /// Logical sequence number of the diverging record (`None` when the
    /// divergence is an outcome facet, e.g. exit code).
    pub seq: Option<u64>,
    /// Short tag used by tooling to group divergences (`clock_sleep`,
    /// `process_stdout_hash`, `outcome_exit_code`, …).
    pub category: String,
    /// Human-readable explanation. Stable phrasing — CI snapshot tests
    /// match on substrings.
    pub message: String,
}

/// Compare two tapes under `mode`.
pub fn compare(recorded: &EventTape, replay: &EventTape, mode: FidelityMode) -> FidelityReport {
    let divergences = match mode {
        FidelityMode::ByteIdentical => compare_record_by_record(recorded, replay, true),
        FidelityMode::Semantic => compare_record_by_record(recorded, replay, false),
        FidelityMode::Outcome => compare_outcome(recorded, replay),
        FidelityMode::PhaseAware => compare_phase_aware(recorded, replay),
    };
    let baseline = recorded.records.len().max(replay.records.len()).max(1);
    let score = match mode {
        FidelityMode::ByteIdentical | FidelityMode::Semantic | FidelityMode::PhaseAware => {
            1.0 - (divergences.len() as f32 / baseline as f32).min(1.0)
        }
        FidelityMode::Outcome => {
            if divergences.is_empty() {
                1.0
            } else {
                0.0
            }
        }
    };
    FidelityReport {
        mode,
        recorded_records: recorded.records.len(),
        replay_records: replay.records.len(),
        divergences,
        score,
    }
}

fn compare_phase_aware(recorded: &EventTape, replay: &EventTape) -> Vec<Divergence> {
    let mut out = Vec::new();
    out.extend(compare_record_slices(
        &phase_records(recorded, TapePhase::UserScript, false),
        &phase_records(replay, TapePhase::UserScript, false),
        true,
    ));
    out.extend(compare_record_slices(
        &phase_records(recorded, TapePhase::RuntimeFinalize, true),
        &phase_records(replay, TapePhase::RuntimeFinalize, true),
        false,
    ));
    out
}

fn phase_records(
    tape: &EventTape,
    phase: TapePhase,
    omit_runtime_clock_reads: bool,
) -> Vec<TapeRecord> {
    tape.records
        .iter()
        .filter(|record| {
            record.phase == phase
                && !(omit_runtime_clock_reads
                    && matches!(record.kind, TapeRecordKind::ClockRead { .. }))
        })
        .cloned()
        .collect()
}

fn compare_record_by_record(
    recorded: &EventTape,
    replay: &EventTape,
    byte_strict: bool,
) -> Vec<Divergence> {
    compare_record_slices(&recorded.records, &replay.records, byte_strict)
}

fn compare_record_slices(
    recorded: &[TapeRecord],
    replay: &[TapeRecord],
    byte_strict: bool,
) -> Vec<Divergence> {
    let mut out = Vec::new();
    let max = recorded.len().max(replay.len());
    for idx in 0..max {
        match (recorded.get(idx), replay.get(idx)) {
            (Some(rec), Some(rep)) => compare_pair(rec, rep, byte_strict, &mut out),
            (Some(rec), None) => out.push(Divergence {
                seq: Some(rec.seq),
                category: "missing_in_replay".to_string(),
                message: format!(
                    "replay tape ended at #{idx}; recorded had {} more record(s)",
                    recorded.len() - idx
                ),
            }),
            (None, Some(rep)) => out.push(Divergence {
                seq: Some(rep.seq),
                category: "missing_in_recorded".to_string(),
                message: format!(
                    "replay produced an extra record at #{idx} (kind={})",
                    record_kind_tag(&rep.kind)
                ),
            }),
            (None, None) => break,
        }
    }
    out
}

fn compare_pair(
    recorded: &TapeRecord,
    replay: &TapeRecord,
    byte_strict: bool,
    out: &mut Vec<Divergence>,
) {
    if record_kind_tag(&recorded.kind) != record_kind_tag(&replay.kind) {
        out.push(Divergence {
            seq: Some(recorded.seq),
            category: "kind_mismatch".to_string(),
            message: format!(
                "record kind diverged: recorded={} replay={}",
                record_kind_tag(&recorded.kind),
                record_kind_tag(&replay.kind),
            ),
        });
        return;
    }
    if byte_strict && recorded.phase != replay.phase {
        out.push(Divergence {
            seq: Some(recorded.seq),
            category: "phase_mismatch".to_string(),
            message: format!(
                "record phase diverged: recorded={} replay={}",
                recorded.phase.label(),
                replay.phase.label(),
            ),
        });
    }
    if byte_strict && recorded.virtual_time_ms != replay.virtual_time_ms {
        out.push(Divergence {
            seq: Some(recorded.seq),
            category: "virtual_time_drift".to_string(),
            message: format!(
                "virtual_time_ms diverged: recorded={} replay={}",
                recorded.virtual_time_ms, replay.virtual_time_ms,
            ),
        });
    }
    if byte_strict && recorded.monotonic_ms != replay.monotonic_ms {
        out.push(Divergence {
            seq: Some(recorded.seq),
            category: "monotonic_drift".to_string(),
            message: format!(
                "monotonic_ms diverged: recorded={} replay={}",
                recorded.monotonic_ms, replay.monotonic_ms,
            ),
        });
    }
    compare_kind(&recorded.kind, &replay.kind, recorded.seq, byte_strict, out);
}

fn compare_kind(
    recorded: &TapeRecordKind,
    replay: &TapeRecordKind,
    seq: u64,
    byte_strict: bool,
    out: &mut Vec<Divergence>,
) {
    use TapeRecordKind::*;
    match (recorded, replay) {
        (
            ClockRead {
                source: r_source,
                value_ms: r_val,
            },
            ClockRead {
                source: p_source,
                value_ms: p_val,
            },
        ) => {
            if r_source != p_source {
                out.push(Divergence {
                    seq: Some(seq),
                    category: "clock_read_source".to_string(),
                    message: format!(
                        "clock_read source diverged: recorded={r_source:?} replay={p_source:?}"
                    ),
                });
            }
            if r_val != p_val {
                out.push(Divergence {
                    seq: Some(seq),
                    category: "clock_read_value".to_string(),
                    message: format!(
                        "clock_read value_ms diverged: recorded={r_val} replay={p_val}"
                    ),
                });
            }
        }
        (
            ClockSleep {
                duration_ms: recorded_dur,
            },
            ClockSleep {
                duration_ms: replay_dur,
            },
        ) => {
            if recorded_dur != replay_dur {
                out.push(Divergence {
                    seq: Some(seq),
                    category: "clock_sleep_duration".to_string(),
                    message: format!(
                        "sleep duration diverged: recorded={recorded_dur}ms replay={replay_dur}ms"
                    ),
                });
            }
        }
        (
            LlmCall {
                request_digest: recorded_req,
                response: recorded_res,
            },
            LlmCall {
                request_digest: replay_req,
                response: replay_res,
            },
        ) => {
            if recorded_req != replay_req {
                out.push(Divergence {
                    seq: Some(seq),
                    category: "llm_request_digest".to_string(),
                    message: format!(
                        "LLM request digest diverged: recorded={recorded_req} replay={replay_req}"
                    ),
                });
            }
            if recorded_res.content_hash() != replay_res.content_hash() {
                out.push(Divergence {
                    seq: Some(seq),
                    category: "llm_response_hash".to_string(),
                    message: format!(
                        "LLM response hash diverged: recorded={} replay={}",
                        recorded_res.content_hash(),
                        replay_res.content_hash(),
                    ),
                });
            }
        }
        (
            FileRead {
                path: rp,
                content_hash: rh,
                len_bytes: rl,
            },
            FileRead {
                path: pp,
                content_hash: ph,
                len_bytes: pl,
            },
        ) => compare_file(seq, "file_read", rp, rh, *rl, pp, ph, *pl, byte_strict, out),
        (
            FileWrite {
                path: rp,
                content_hash: rh,
                len_bytes: rl,
            },
            FileWrite {
                path: pp,
                content_hash: ph,
                len_bytes: pl,
            },
        ) => compare_file(
            seq,
            "file_write",
            rp,
            rh,
            *rl,
            pp,
            ph,
            *pl,
            byte_strict,
            out,
        ),
        (FileDelete { path: rp }, FileDelete { path: pp }) => {
            if rp != pp {
                out.push(Divergence {
                    seq: Some(seq),
                    category: "file_delete_path".to_string(),
                    message: format!("file_delete path diverged: recorded={rp} replay={pp}"),
                });
            }
        }
        (
            ProcessSpawn {
                program: r_program,
                args: r_args,
                cwd: r_cwd,
                exit_code: r_exit,
                duration_ms: r_dur,
                stdout_payload: r_stdout,
                stderr_payload: r_stderr,
            },
            ProcessSpawn {
                program: p_program,
                args: p_args,
                cwd: p_cwd,
                exit_code: p_exit,
                duration_ms: p_dur,
                stdout_payload: p_stdout,
                stderr_payload: p_stderr,
            },
        ) => {
            if r_program != p_program {
                out.push(Divergence {
                    seq: Some(seq),
                    category: "process_program".to_string(),
                    message: format!(
                        "subprocess program diverged: recorded={r_program} replay={p_program}"
                    ),
                });
            }
            if r_args != p_args {
                out.push(Divergence {
                    seq: Some(seq),
                    category: "process_args".to_string(),
                    message: format!(
                        "subprocess args diverged: recorded={r_args:?} replay={p_args:?}"
                    ),
                });
            }
            if r_cwd != p_cwd {
                out.push(Divergence {
                    seq: Some(seq),
                    category: "process_cwd".to_string(),
                    message: format!(
                        "subprocess cwd diverged: recorded={r_cwd:?} replay={p_cwd:?}"
                    ),
                });
            }
            if r_exit != p_exit {
                out.push(Divergence {
                    seq: Some(seq),
                    category: "process_exit_code".to_string(),
                    message: format!(
                        "subprocess exit code diverged: recorded={r_exit} replay={p_exit}"
                    ),
                });
            }
            if byte_strict && r_dur != p_dur {
                out.push(Divergence {
                    seq: Some(seq),
                    category: "process_duration".to_string(),
                    message: format!(
                        "subprocess duration diverged: recorded={r_dur}ms replay={p_dur}ms"
                    ),
                });
            }
            if r_stdout.content_hash() != p_stdout.content_hash() {
                out.push(Divergence {
                    seq: Some(seq),
                    category: "process_stdout_hash".to_string(),
                    message: format!(
                        "subprocess stdout hash diverged: recorded={} replay={}",
                        r_stdout.content_hash(),
                        p_stdout.content_hash(),
                    ),
                });
            }
            if r_stderr.content_hash() != p_stderr.content_hash() {
                out.push(Divergence {
                    seq: Some(seq),
                    category: "process_stderr_hash".to_string(),
                    message: format!(
                        "subprocess stderr hash diverged: recorded={} replay={}",
                        r_stderr.content_hash(),
                        p_stderr.content_hash(),
                    ),
                });
            }
        }
        (Unknown, _) | (_, Unknown) => out.push(Divergence {
            seq: Some(seq),
            category: "unknown_kind".to_string(),
            message: "encountered an unknown record kind — produced by a newer harn-vm version"
                .to_string(),
        }),
        // Kind tag matched at the top of `compare_pair`; reaching here
        // means a new variant was added without extending this match.
        // Surface a structured divergence rather than panicking so older
        // CI runners stay functional after a tape-format upgrade.
        _ => out.push(Divergence {
            seq: Some(seq),
            category: "comparator_gap".to_string(),
            message: format!(
                "no comparator wired for record kind `{}`",
                record_kind_tag(recorded)
            ),
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn compare_file(
    seq: u64,
    category: &str,
    recorded_path: &str,
    recorded_hash: &str,
    recorded_len: u64,
    replay_path: &str,
    replay_hash: &str,
    replay_len: u64,
    byte_strict: bool,
    out: &mut Vec<Divergence>,
) {
    if recorded_path != replay_path {
        out.push(Divergence {
            seq: Some(seq),
            category: format!("{category}_path"),
            message: format!(
                "{category} path diverged: recorded={recorded_path} replay={replay_path}"
            ),
        });
    }
    if recorded_hash != replay_hash {
        out.push(Divergence {
            seq: Some(seq),
            category: format!("{category}_hash"),
            message: format!(
                "{category} content hash diverged: recorded={recorded_hash} replay={replay_hash}"
            ),
        });
    }
    if byte_strict && recorded_len != replay_len {
        out.push(Divergence {
            seq: Some(seq),
            category: format!("{category}_len"),
            message: format!(
                "{category} length diverged: recorded={recorded_len} replay={replay_len}"
            ),
        });
    }
}

fn compare_outcome(recorded: &EventTape, replay: &EventTape) -> Vec<Divergence> {
    let mut out = Vec::new();

    let recorded_writes = collect_final_writes(recorded);
    let replay_writes = collect_final_writes(replay);
    if recorded_writes != replay_writes {
        let recorded_paths: Vec<&String> = recorded_writes.keys().collect();
        let replay_paths: Vec<&String> = replay_writes.keys().collect();
        out.push(Divergence {
            seq: None,
            category: "outcome_fs_diff".to_string(),
            message: format!(
                "final FS write set diverged: recorded={recorded_paths:?} replay={replay_paths:?}"
            ),
        });
    }

    let recorded_exit = last_process_exit(recorded);
    let replay_exit = last_process_exit(replay);
    if recorded_exit != replay_exit {
        out.push(Divergence {
            seq: None,
            category: "outcome_exit_code".to_string(),
            message: format!(
                "last subprocess exit code diverged: recorded={recorded_exit:?} replay={replay_exit:?}"
            ),
        });
    }

    let recorded_llm = count_llm_calls(recorded);
    let replay_llm = count_llm_calls(replay);
    if recorded_llm != replay_llm {
        out.push(Divergence {
            seq: None,
            category: "outcome_llm_call_count".to_string(),
            message: format!(
                "LLM call count diverged: recorded={recorded_llm} replay={replay_llm}"
            ),
        });
    }
    out
}

fn collect_final_writes(tape: &EventTape) -> BTreeMap<String, Option<String>> {
    let mut state: BTreeMap<String, Option<String>> = BTreeMap::new();
    for record in &tape.records {
        match &record.kind {
            TapeRecordKind::FileWrite {
                path, content_hash, ..
            } => {
                state.insert(path.clone(), Some(content_hash.clone()));
            }
            TapeRecordKind::FileDelete { path } => {
                state.insert(path.clone(), None);
            }
            _ => {}
        }
    }
    state
}

fn last_process_exit(tape: &EventTape) -> Option<i32> {
    tape.records
        .iter()
        .rev()
        .find_map(|record| match &record.kind {
            TapeRecordKind::ProcessSpawn { exit_code, .. } => Some(*exit_code),
            _ => None,
        })
}

fn count_llm_calls(tape: &EventTape) -> usize {
    tape.records
        .iter()
        .filter(|record| matches!(record.kind, TapeRecordKind::LlmCall { .. }))
        .count()
}

fn record_kind_tag(kind: &TapeRecordKind) -> &'static str {
    match kind {
        TapeRecordKind::ClockRead { .. } => "clock_read",
        TapeRecordKind::ClockSleep { .. } => "clock_sleep",
        TapeRecordKind::LlmCall { .. } => "llm_call",
        TapeRecordKind::FileRead { .. } => "file_read",
        TapeRecordKind::FileWrite { .. } => "file_write",
        TapeRecordKind::FileDelete { .. } => "file_delete",
        TapeRecordKind::ProcessSpawn { .. } => "process_spawn",
        TapeRecordKind::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testbench::tape::{ClockSource, TapeHeader, TapePayload, TapeRecord};

    fn empty_tape() -> EventTape {
        EventTape::new(TapeHeader::current(None, None, Vec::new()))
    }

    fn record(seq: u64, kind: TapeRecordKind) -> TapeRecord {
        TapeRecord {
            seq,
            phase: TapePhase::UserScript,
            virtual_time_ms: 0,
            monotonic_ms: 0,
            kind,
        }
    }

    #[test]
    fn byte_identical_matches_when_records_align() {
        let mut a = empty_tape();
        let mut b = empty_tape();
        a.records
            .push(record(0, TapeRecordKind::ClockSleep { duration_ms: 5 }));
        b.records
            .push(record(0, TapeRecordKind::ClockSleep { duration_ms: 5 }));
        let report = compare(&a, &b, FidelityMode::ByteIdentical);
        assert!(report.is_byte_identical(), "{report:?}");
        assert_eq!(report.score, 1.0);
    }

    #[test]
    fn byte_identical_flags_a_drifted_clock_read() {
        // Acceptance criterion: deliberately introduce a wall-clock
        // divergence and confirm the oracle flags it.
        let mut a = empty_tape();
        let mut b = empty_tape();
        a.records
            .push(record(0, TapeRecordKind::ClockSleep { duration_ms: 5 }));
        b.records
            .push(record(0, TapeRecordKind::ClockSleep { duration_ms: 7 }));
        let report = compare(&a, &b, FidelityMode::ByteIdentical);
        assert_eq!(report.divergences.len(), 1);
        assert_eq!(report.divergences[0].category, "clock_sleep_duration");
    }

    #[test]
    fn semantic_mode_ignores_pure_timing_drift() {
        let mut a = empty_tape();
        let mut b = empty_tape();
        let make = |seq: u64, vt: i64| TapeRecord {
            seq,
            phase: TapePhase::UserScript,
            virtual_time_ms: vt,
            monotonic_ms: vt,
            kind: TapeRecordKind::FileWrite {
                path: "/tmp/out.txt".to_string(),
                content_hash: "abc".to_string(),
                len_bytes: 3,
            },
        };
        a.records.push(make(0, 0));
        b.records.push(make(0, 1)); // virtual time drifted by 1ms
        let strict = compare(&a, &b, FidelityMode::ByteIdentical);
        assert!(!strict.is_byte_identical());
        let semantic = compare(&a, &b, FidelityMode::Semantic);
        assert!(
            semantic.is_byte_identical(),
            "semantic should not flag pure timing drift, got {semantic:?}"
        );
    }

    #[test]
    fn outcome_mode_only_compares_final_writes_and_exit() {
        let mut a = empty_tape();
        let mut b = empty_tape();
        // Both end with the same final FS state but take different paths.
        a.records.push(record(
            0,
            TapeRecordKind::FileWrite {
                path: "/tmp/a".to_string(),
                content_hash: "h1".to_string(),
                len_bytes: 1,
            },
        ));
        a.records
            .push(record(1, TapeRecordKind::ClockSleep { duration_ms: 1000 }));
        b.records
            .push(record(0, TapeRecordKind::ClockSleep { duration_ms: 50 }));
        b.records.push(record(
            1,
            TapeRecordKind::FileWrite {
                path: "/tmp/a".to_string(),
                content_hash: "h1".to_string(),
                len_bytes: 1,
            },
        ));
        let report = compare(&a, &b, FidelityMode::Outcome);
        assert!(
            report.divergences.is_empty(),
            "outcome mode should ignore intermediate diffs, got {report:?}"
        );
        assert_eq!(report.score, 1.0);
    }

    #[test]
    fn outcome_mode_flags_exit_code_drift() {
        let mut a = empty_tape();
        let mut b = empty_tape();
        let payload = TapePayload::Inline {
            content_hash: "ehash".to_string(),
            text: String::new(),
        };
        a.records.push(record(
            0,
            TapeRecordKind::ProcessSpawn {
                program: "git".to_string(),
                args: Vec::new(),
                cwd: None,
                exit_code: 0,
                duration_ms: 1,
                stdout_payload: payload.clone(),
                stderr_payload: payload.clone(),
            },
        ));
        b.records.push(record(
            0,
            TapeRecordKind::ProcessSpawn {
                program: "git".to_string(),
                args: Vec::new(),
                cwd: None,
                exit_code: 1,
                duration_ms: 1,
                stdout_payload: payload.clone(),
                stderr_payload: payload,
            },
        ));
        let report = compare(&a, &b, FidelityMode::Outcome);
        assert_eq!(report.divergences.len(), 1);
        assert_eq!(report.divergences[0].category, "outcome_exit_code");
    }

    #[test]
    fn phase_aware_ignores_extra_runtime_finalize_clock_reads() {
        let mut recorded = empty_tape();
        let mut replay = empty_tape();
        recorded
            .records
            .push(record(0, TapeRecordKind::ClockSleep { duration_ms: 5 }));
        replay
            .records
            .push(record(0, TapeRecordKind::ClockSleep { duration_ms: 5 }));

        let mut finalize_read = record(
            1,
            TapeRecordKind::ClockRead {
                source: ClockSource::Wall,
                value_ms: 123,
            },
        );
        finalize_read.phase = TapePhase::RuntimeFinalize;
        replay.records.push(finalize_read);

        let strict = compare(&recorded, &replay, FidelityMode::ByteIdentical);
        assert_eq!(strict.divergences[0].category, "missing_in_recorded");

        let phase_aware = compare(&recorded, &replay, FidelityMode::PhaseAware);
        assert!(
            phase_aware.is_byte_identical(),
            "runtime finalize clock reads should not force tape regeneration: {phase_aware:?}"
        );
    }

    #[test]
    fn phase_aware_flags_extra_user_script_clock_reads() {
        let mut recorded = empty_tape();
        let mut replay = empty_tape();
        recorded
            .records
            .push(record(0, TapeRecordKind::ClockSleep { duration_ms: 5 }));
        replay
            .records
            .push(record(0, TapeRecordKind::ClockSleep { duration_ms: 5 }));
        replay.records.push(record(
            1,
            TapeRecordKind::ClockRead {
                source: ClockSource::Wall,
                value_ms: 123,
            },
        ));

        let report = compare(&recorded, &replay, FidelityMode::PhaseAware);
        assert_eq!(report.divergences.len(), 1);
        assert_eq!(report.divergences[0].category, "missing_in_recorded");
    }

    #[test]
    fn phase_aware_flags_runtime_finalize_effect_drift() {
        let recorded = empty_tape();
        let mut replay = empty_tape();
        let mut replay_write = record(
            0,
            TapeRecordKind::FileWrite {
                path: "/tmp/finalized".to_string(),
                content_hash: "abc".to_string(),
                len_bytes: 3,
            },
        );
        replay_write.phase = TapePhase::RuntimeFinalize;
        replay.records.push(replay_write);

        let report = compare(&recorded, &replay, FidelityMode::PhaseAware);
        assert_eq!(report.divergences.len(), 1);
        assert_eq!(report.divergences[0].category, "missing_in_recorded");
    }

    #[test]
    fn parse_mode_accepts_aliases() {
        assert_eq!(
            FidelityMode::parse("byte").unwrap(),
            FidelityMode::ByteIdentical
        );
        assert_eq!(
            FidelityMode::parse("byte-identical").unwrap(),
            FidelityMode::ByteIdentical
        );
        assert_eq!(
            FidelityMode::parse("semantic").unwrap(),
            FidelityMode::Semantic
        );
        assert_eq!(
            FidelityMode::parse("outcome").unwrap(),
            FidelityMode::Outcome
        );
        assert_eq!(
            FidelityMode::parse("phase-aware").unwrap(),
            FidelityMode::PhaseAware
        );
        assert!(FidelityMode::parse("nope").is_err());
    }
}
