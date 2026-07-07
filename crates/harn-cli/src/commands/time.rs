//! `harn time` — wrap a subcommand with structured phase timing.
//!
//! Today only `harn time run` is supported. The wrapper enables both VM
//! and LLM tracing, drives the run through the same code path as
//! `harn run`, and emits a versioned [`JsonEnvelope`] with per-phase
//! wall-clock + cache hit/miss + per-LLM-call + per-tool-call latency.
//!
//! Phases are emitted in fixed order — `parse`, `typecheck`,
//! `bytecode_compile`, `run_setup`, `run_main` — even when a cache hit
//! lets us skip parse/typecheck. That keeps consumers' shape stable so
//! `phases.length >= 5` is a safe assertion and `cache: "hit"` always
//! lives on the `bytecode_compile` row.

use std::fs;
use std::path::PathBuf;
use std::process;
use std::time::Duration;

use serde::Serialize;

use crate::cli::TimeRunArgs;
use crate::commands::run::{
    execute_run_with_timing, prepare_eval_temp_file, StdoutPassthroughGuard,
};
use crate::env_guard::ScopedEnvVar;
use crate::json_envelope::{to_string_pretty, JsonEnvelope};

/// Schema version for the `harn time run --json` envelope. Bump when
/// the [`TimingReport`] shape changes in a way agents must detect.
pub const TIME_RUN_SCHEMA_VERSION: u32 = 1;

/// Per-phase wall-clock samples recorded by the run path. Filled in by
/// [`crate::commands::run`] when timing is requested; absent fields
/// (e.g. parse on a cache hit) stay zero.
#[derive(Debug, Default, Clone)]
pub struct RunTiming {
    pub parse: Duration,
    pub typecheck: Duration,
    pub bytecode_compile: Duration,
    pub run_setup: Duration,
    pub run_main: Duration,
    /// Source size in bytes, captured before parse to populate the
    /// `input_bytes` field on the parse phase row.
    pub input_bytes: u64,
    /// True when the bytecode cache short-circuited parse/typecheck.
    pub cache_hit: bool,
}

#[derive(Debug, Serialize)]
pub struct TimingReport {
    /// The wrapped subcommand. Always `"run"` today; future expansions
    /// (e.g. `"check"`) reuse the same envelope.
    pub command: String,
    /// Resolved script path. `None` for `-e <code>` invocations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    pub phases: Vec<PhaseRecord>,
    pub llm_calls: Vec<LlmCallTiming>,
    pub tool_calls: Vec<ToolCallTiming>,
    pub totals: TimingTotals,
    /// Forwarded exit code from the wrapped subcommand. Non-zero exit
    /// still emits a successful envelope — the wrapper's job is to
    /// describe what happened, not to mask failures.
    pub exit_code: i32,
}

#[derive(Debug, Serialize)]
pub struct PhaseRecord {
    pub name: String,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_bytes: Option<u64>,
    /// `"hit"` or `"miss"` on `bytecode_compile`; absent on other phases.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache: Option<String>,
    /// Count of completed VM spans observed inside this phase. Only
    /// populated on `run_main` today.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub events: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct LlmCallTiming {
    pub model: String,
    pub latency_ms: u64,
    /// Total tokens for the call (input + output). Per-call input/output
    /// split is available via `harn run --trace`; this surface keeps the
    /// shape compact for agent consumption.
    pub tokens: i64,
}

#[derive(Debug, Serialize)]
pub struct ToolCallTiming {
    pub name: String,
    pub latency_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct TimingTotals {
    pub wall_ms: u64,
    pub cpu_ms: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

pub(crate) async fn run(args: TimeRunArgs) {
    // Lift the bytecode cache via env var so the existing `harn run`
    // code path observes the override. The guard restores the previous
    // value on drop; the CLI is single-shot, but tests reuse the
    // process and rely on a clean revert.
    let _cache_guard = args
        .no_cache
        .then(|| ScopedEnvVar::set(harn_vm::bytecode_cache::CACHE_ENABLED_ENV, "0"));

    // Make sure LLM + VM tracing capture per-call durations. Both are
    // thread-local and reset by `set_tracing_enabled(true)`.
    harn_vm::llm::enable_tracing();
    harn_vm::tracing::set_tracing_enabled(true);
    let _ = harn_vm::tracing::take_spans();
    let _ = harn_vm::llm::take_trace();

    let mut timing = RunTiming::default();
    let cpu_start = cpu_ms();
    let wall_start = std::time::Instant::now();
    let sandbox = if args.no_sandbox {
        crate::commands::run::RunSandboxOptions::disabled()
    } else {
        crate::commands::run::RunSandboxOptions::default()
            .with_write_roots(args.write_root.iter().cloned())
            .with_read_only_roots(args.read_only_root.iter().cloned())
    };

    // In `--json` mode stdout is owned by the envelope, so we keep
    // script output buffered (passthrough disabled) and write it to
    // stderr afterwards. In human mode we mirror `harn run` and stream
    // script output directly to the terminal stdout.
    let _stdout_guard = (!args.json).then(StdoutPassthroughGuard::enable);

    let (target, outcome) = match (args.eval.as_deref(), args.file.as_deref()) {
        (Some(code), None) => {
            let (wrapped, tmp) = prepare_eval_temp_file(code).unwrap_or_else(|e| {
                eprintln!("error: {e}");
                process::exit(1);
            });
            let tmp_path: PathBuf = tmp.path().to_path_buf();
            if let Err(e) = fs::write(&tmp_path, &wrapped) {
                eprintln!("error: failed to write temp file for -e: {e}");
                process::exit(1);
            }
            let path_str = tmp_path.to_string_lossy().into_owned();
            let outcome = execute_run_with_timing(
                &path_str,
                args.argv.clone(),
                Some(&mut timing),
                sandbox.clone(),
            )
            .await;
            drop(tmp);
            (None, outcome)
        }
        (None, Some(file)) => {
            let outcome =
                execute_run_with_timing(file, args.argv.clone(), Some(&mut timing), sandbox).await;
            (Some(file.to_string()), outcome)
        }
        (Some(_), Some(_)) => {
            eprintln!(
                "error: `harn time run` accepts either `-e <code>` or `<file.harn>`, not both"
            );
            process::exit(2);
        }
        (None, None) => {
            eprintln!("error: `harn time run` requires either `-e <code>` or `<file.harn>`");
            process::exit(2);
        }
    };

    let wall_ms = wall_start.elapsed().as_millis() as u64;
    let cpu_ms_total = cpu_ms().saturating_sub(cpu_start);

    if !outcome.stderr.is_empty() {
        eprint!("{}", outcome.stderr);
    }
    if !outcome.stdout.is_empty() {
        if args.json {
            // JSON mode owns stdout for the envelope. Script output
            // would corrupt downstream `jq` pipelines, so mirror it to
            // stderr where humans can still see it.
            eprint!("{}", outcome.stdout);
        } else {
            // Passthrough already delivered output to the terminal in
            // human mode, but on cache-hit paths some bytes can land
            // in the captured buffer (stdout passthrough only catches
            // bytes flushed after it was installed). Re-emit so they
            // aren't lost.
            print!("{}", outcome.stdout);
        }
    }

    let llm_trace = harn_vm::llm::take_trace();
    let spans = harn_vm::tracing::take_spans();

    let llm_calls: Vec<LlmCallTiming> = llm_trace
        .iter()
        .map(|entry| LlmCallTiming {
            model: entry.model.clone(),
            latency_ms: entry.duration_ms,
            tokens: entry.input_tokens + entry.output_tokens,
        })
        .collect();

    let tool_calls: Vec<ToolCallTiming> = spans
        .iter()
        .filter(|span| span.kind.as_str() == "tool_call")
        .map(|span| ToolCallTiming {
            name: span.name.clone(),
            latency_ms: span.duration_ms,
        })
        .collect();

    let cache_hit = timing.cache_hit;
    let phases = build_phase_records(&timing, spans.len() as u64);

    let report = TimingReport {
        command: "run".into(),
        target,
        phases,
        llm_calls,
        tool_calls,
        totals: TimingTotals {
            wall_ms,
            cpu_ms: cpu_ms_total,
            cache_hits: u64::from(cache_hit),
            cache_misses: u64::from(!cache_hit),
        },
        exit_code: outcome.exit_code,
    };

    if args.json {
        println!(
            "{}",
            to_string_pretty(&JsonEnvelope::ok(TIME_RUN_SCHEMA_VERSION, &report))
        );
    } else {
        eprint!("{}", render_human(&report));
    }

    if outcome.exit_code != 0 {
        process::exit(outcome.exit_code);
    }
}

fn render_human(report: &TimingReport) -> String {
    use std::fmt::Write;

    let mut out = String::new();
    let _ = writeln!(out, "\n\x1b[2m─── harn time ───\x1b[0m");
    let _ = writeln!(
        out,
        "  wall {} · cpu {} · {} cache",
        format_ms(report.totals.wall_ms),
        format_ms(report.totals.cpu_ms),
        if report.totals.cache_hits > 0 {
            "hit"
        } else {
            "miss"
        },
    );
    let _ = writeln!(out, "\n  Phases:");
    for phase in &report.phases {
        let suffix = match (phase.input_bytes, phase.cache.as_deref(), phase.events) {
            (Some(bytes), _, _) => format!("  ({bytes} input bytes)"),
            (_, Some(cache), _) => format!("  (cache {cache})"),
            (_, _, Some(events)) => format!("  ({events} events)"),
            _ => String::new(),
        };
        let _ = writeln!(
            out,
            "    {:<18} {:>10}{suffix}",
            phase.name,
            format_ms(phase.duration_ms),
        );
    }
    if !report.llm_calls.is_empty() {
        let _ = writeln!(out, "\n  LLM calls:");
        for call in &report.llm_calls {
            let _ = writeln!(
                out,
                "    {:<24} {:>10}  ({} tokens)",
                call.model,
                format_ms(call.latency_ms),
                call.tokens,
            );
        }
    }
    if !report.tool_calls.is_empty() {
        let _ = writeln!(out, "\n  Tool calls:");
        for call in &report.tool_calls {
            let _ = writeln!(
                out,
                "    {:<24} {:>10}",
                call.name,
                format_ms(call.latency_ms),
            );
        }
    }
    out
}

fn format_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else {
        format!("{:.3} s", ms as f64 / 1000.0)
    }
}

/// Total user + system CPU time consumed by the current process, in
/// milliseconds. Falls back to `0` on platforms where `getrusage` is
/// unavailable so the field is always present in the envelope.
#[cfg(unix)]
pub(crate) fn cpu_ms() -> u64 {
    use std::mem::MaybeUninit;
    // SAFETY: `getrusage` writes a fully-initialized `rusage` on success;
    // we treat the value as live only after the syscall reports OK.
    unsafe {
        let mut ru = MaybeUninit::<libc::rusage>::zeroed();
        if libc::getrusage(libc::RUSAGE_SELF, ru.as_mut_ptr()) != 0 {
            return 0;
        }
        let ru = ru.assume_init();
        let user = duration_ms(ru.ru_utime.tv_sec, ru.ru_utime.tv_usec);
        let system = duration_ms(ru.ru_stime.tv_sec, ru.ru_stime.tv_usec);
        user.saturating_add(system)
    }
}

#[cfg(not(unix))]
pub(crate) fn cpu_ms() -> u64 {
    0
}

#[cfg(unix)]
fn duration_ms(secs: libc::time_t, micros: libc::suseconds_t) -> u64 {
    // `libc::time_t` and `libc::suseconds_t` are platform-defined
    // (i64 + i32 on macOS, i64 + i64 on glibc Linux). Going through
    // i128 once dodges the per-platform clippy lint on a no-op cast
    // and gives plenty of headroom for the *1000.
    let secs_ms = i128::from(secs).saturating_mul(1000);
    let micros_ms = i128::from(micros) / 1000;
    secs_ms.saturating_add(micros_ms).max(0) as u64
}

pub(crate) fn build_phase_records(timing: &RunTiming, main_events: u64) -> Vec<PhaseRecord> {
    let cache_hit = timing.cache_hit;
    vec![
        PhaseRecord {
            name: "parse".into(),
            duration_ms: timing.parse.as_millis() as u64,
            input_bytes: if cache_hit {
                None
            } else {
                Some(timing.input_bytes)
            },
            cache: None,
            events: None,
        },
        PhaseRecord {
            name: "typecheck".into(),
            duration_ms: timing.typecheck.as_millis() as u64,
            input_bytes: None,
            cache: None,
            events: None,
        },
        PhaseRecord {
            name: "bytecode_compile".into(),
            duration_ms: timing.bytecode_compile.as_millis() as u64,
            input_bytes: None,
            cache: Some(if cache_hit {
                "hit".into()
            } else {
                "miss".into()
            }),
            events: None,
        },
        PhaseRecord {
            name: "run_setup".into(),
            duration_ms: timing.run_setup.as_millis() as u64,
            input_bytes: None,
            cache: None,
            events: None,
        },
        PhaseRecord {
            name: "run_main".into(),
            duration_ms: timing.run_main.as_millis() as u64,
            input_bytes: None,
            cache: None,
            events: Some(main_events),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::common::json_envelope::assert_envelope;

    fn fixture_timing(cache_hit: bool) -> RunTiming {
        RunTiming {
            parse: if cache_hit {
                Duration::default()
            } else {
                Duration::from_millis(12)
            },
            typecheck: if cache_hit {
                Duration::default()
            } else {
                Duration::from_millis(80)
            },
            bytecode_compile: Duration::from_millis(35),
            run_setup: Duration::from_millis(8),
            run_main: Duration::from_millis(1200),
            input_bytes: 4096,
            cache_hit,
        }
    }

    fn make_report(cache_hit: bool) -> TimingReport {
        let timing = fixture_timing(cache_hit);
        TimingReport {
            command: "run".into(),
            target: Some("examples/hello.harn".into()),
            phases: vec![
                PhaseRecord {
                    name: "parse".into(),
                    duration_ms: timing.parse.as_millis() as u64,
                    input_bytes: if cache_hit {
                        None
                    } else {
                        Some(timing.input_bytes)
                    },
                    cache: None,
                    events: None,
                },
                PhaseRecord {
                    name: "typecheck".into(),
                    duration_ms: timing.typecheck.as_millis() as u64,
                    input_bytes: None,
                    cache: None,
                    events: None,
                },
                PhaseRecord {
                    name: "bytecode_compile".into(),
                    duration_ms: timing.bytecode_compile.as_millis() as u64,
                    input_bytes: None,
                    cache: Some(if cache_hit {
                        "hit".into()
                    } else {
                        "miss".into()
                    }),
                    events: None,
                },
                PhaseRecord {
                    name: "run_setup".into(),
                    duration_ms: timing.run_setup.as_millis() as u64,
                    input_bytes: None,
                    cache: None,
                    events: None,
                },
                PhaseRecord {
                    name: "run_main".into(),
                    duration_ms: timing.run_main.as_millis() as u64,
                    input_bytes: None,
                    cache: None,
                    events: Some(14),
                },
            ],
            llm_calls: vec![LlmCallTiming {
                model: "claude-sonnet-4-6".into(),
                latency_ms: 850,
                tokens: 1500,
            }],
            tool_calls: vec![ToolCallTiming {
                name: "mcp_call".into(),
                latency_ms: 200,
            }],
            totals: TimingTotals {
                wall_ms: 1335,
                cpu_ms: 320,
                cache_hits: u64::from(cache_hit),
                cache_misses: u64::from(!cache_hit),
            },
            exit_code: 0,
        }
    }

    #[test]
    fn miss_envelope_has_five_phases_and_cache_miss_marker() {
        let envelope = JsonEnvelope::ok(TIME_RUN_SCHEMA_VERSION, make_report(false));
        let value = serde_json::to_value(&envelope).unwrap();
        let data = assert_envelope(&value, TIME_RUN_SCHEMA_VERSION);
        let phases = data["phases"].as_array().expect("phases is array");
        assert_eq!(phases.len(), 5);
        assert_eq!(phases[0]["name"], "parse");
        assert_eq!(phases[0]["input_bytes"], 4096);
        assert_eq!(phases[2]["name"], "bytecode_compile");
        assert_eq!(phases[2]["cache"], "miss");
        assert_eq!(data["totals"]["cache_misses"], 1);
        assert_eq!(data["totals"]["cache_hits"], 0);
    }

    #[test]
    fn hit_envelope_zeros_parse_typecheck_and_marks_cache_hit() {
        let envelope = JsonEnvelope::ok(TIME_RUN_SCHEMA_VERSION, make_report(true));
        let value = serde_json::to_value(&envelope).unwrap();
        let data = assert_envelope(&value, TIME_RUN_SCHEMA_VERSION);
        let phases = data["phases"].as_array().expect("phases is array");
        assert_eq!(phases[0]["duration_ms"], 0);
        assert_eq!(phases[1]["duration_ms"], 0);
        // input_bytes is omitted on a hit since the parse path didn't run.
        assert!(phases[0].get("input_bytes").is_none());
        assert_eq!(phases[2]["cache"], "hit");
        assert_eq!(data["totals"]["cache_hits"], 1);
    }

    #[test]
    fn render_human_lists_phases_and_calls() {
        let rendered = render_human(&make_report(false));
        assert!(rendered.contains("harn time"));
        assert!(rendered.contains("parse"));
        assert!(rendered.contains("bytecode_compile"));
        assert!(rendered.contains("cache miss"));
        assert!(rendered.contains("claude-sonnet-4-6"));
        assert!(rendered.contains("mcp_call"));
    }

    #[test]
    fn render_human_for_hit_includes_cache_hit_marker() {
        let rendered = render_human(&make_report(true));
        assert!(rendered.contains("cache hit"));
    }
}
