//! `harn demo` — bundled, fully-offline scenarios that demonstrate the
//! Harn moat (persona supervision, replay determinism, provider
//! routing) without any API keys. See issue #1650.
//!
//! Each scenario ships with:
//!   - a `.harn` script (`assets/demo/<id>/scenario.harn`)
//!   - a JSONL `--llm-mock` tape (`assets/demo/<id>/tape.jsonl`)
//!
//! Both are `include_str!`'d into the binary so `harn demo <id>` works
//! from a static-linked install with no repo checkout.

use std::collections::HashSet;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::cli::DemoArgs;
use crate::commands::run::{execute_run, CliLlmMockMode, RunOutcome, RunProfileOptions};

/// Bundled scenarios shipped with the binary. Keep ordered by the
/// "first-touch impact" we want stranger users to see — the menu and
/// `--list` print in this order, and a bare `harn demo` (no scenario
/// arg, non-interactive context) defaults to the first entry.
const SCENARIOS: &[Scenario] = &[
    Scenario {
        id: "merge-captain",
        title: "Merge Captain triages 3 PRs",
        description: "A merge_captain persona triages three mocked PRs (trivial, risky, buggy), \
                      asks an LLM per PR, and emits a structured supervision receipt. \
                      Demonstrates persona supervision, approval gates, and trust receipts.",
        script: include_str!("../../assets/demo/merge-captain/scenario.harn"),
        tape: include_str!("../../assets/demo/merge-captain/tape.jsonl"),
    },
    Scenario {
        id: "review-captain",
        title: "Review Captain inspects a 5-file diff",
        description: "A review_captain reviews a 5-file diff, asks one clarifying question \
                      (HITL surfaced via the receipt), then renders a verdict with \
                      reasoning. Demonstrates clarifying-question loops and structured \
                      review receipts.",
        script: include_str!("../../assets/demo/review-captain/scenario.harn"),
        tape: include_str!("../../assets/demo/review-captain/tape.jsonl"),
    },
    Scenario {
        id: "provider-race",
        title: "Provider race with cost attribution",
        description: "Race three providers on one prompt with `parallel each`, pick the \
                      lowest-latency winner, and emit a cost-attribution receipt. \
                      Previews the routing_policy primitive (#1649).",
        script: include_str!("../../assets/demo/provider-race/scenario.harn"),
        tape: include_str!("../../assets/demo/provider-race/tape.jsonl"),
    },
    Scenario {
        id: "routing-policy",
        title: "routing_policy escalates a cheap chain to a frontier link",
        description: "Drive the v0.8.40 routing_policy primitive through three mocked tasks: a \
                      clean cheap-link success, a 429 that fails over to the frontier, and a \
                      TODO-poisoned cheap reply that the lint verifier escalates. Demonstrates \
                      per-attempt routing receipts, failover, and verifier-signal escalation \
                      (canary scenario for the demo gate, #2437).",
        script: include_str!("../../assets/demo/routing-policy/scenario.harn"),
        tape: include_str!("../../assets/demo/routing-policy/tape.jsonl"),
    },
    Scenario {
        id: "stdlib-toolkit",
        title: "stdlib toolkit assembles an XML system-prompt context",
        description: "Walk the new clone / deep_merge / unique / dict_from_pairs / to_xml / \
                      from_xml / word_wrap / indent / repeat built-ins through a realistic \
                      pre-flight render: layer per-task overrides onto a shared defaults dict, \
                      dedupe the operator's `previous_chats` list, emit an XML `<context>` \
                      block, round-trip it back through the parser, and frame the result in a \
                      60-column prompt margin. Fully offline.",
        script: include_str!("../../assets/demo/stdlib-toolkit/scenario.harn"),
        tape: include_str!("../../assets/demo/stdlib-toolkit/tape.jsonl"),
    },
    Scenario {
        id: "compaction-policy",
        title: "compaction.{policy,check,run} drives a session through the lifecycle",
        description: "Declare a per-session compaction policy with thresholds, call \
                      `compaction.check` to get a `compact_now` / `defer` decision, then drive \
                      the canonical #2323 lifecycle via `compaction.run`. Demonstrates the \
                      lifted-from-TUI policy primitive (#2505) entirely offline using a custom \
                      summarize closure.",
        script: include_str!("../../assets/demo/compaction-policy/scenario.harn"),
        tape: include_str!("../../assets/demo/compaction-policy/tape.jsonl"),
    },
    Scenario {
        id: "mcp-host",
        title: "harn.mcp.* host primitive lazy-spawn + status round-trip",
        description: "Drive the supervised MCP-host primitive (#2504): register two lazy MCP \
                      server specs, snapshot the supervision status (restart_count, circuit, \
                      cache_entries), stop one of them, and re-snapshot. Stays fully offline \
                      because lazy spawn doesn't try to connect. The receipt asserts every \
                      initial entry starts with the circuit closed and cache empty — the \
                      invariants downstream observability hooks lean on.",
        script: include_str!("../../assets/demo/mcp-host/scenario.harn"),
        tape: include_str!("../../assets/demo/mcp-host/tape.jsonl"),
    },
    Scenario {
        id: "http-transport",
        title: "http_etag / http_choose / http_not_modified / http_upgrade_ws",
        description: "Drive the A.12 transport-completeness builtins (#2515) offline: pick a \
                      content type from a simulated `Accept` header via `http_choose`, derive a \
                      strong ETag from the JSON payload, build the matching `http_not_modified` \
                      envelope, and assemble the `http_upgrade_ws` envelope with subprotocol \
                      negotiation. The conformance test in `crates/harn-serve/tests/\
                      transport_conformance.rs` exercises the live HTTP / WS path; the demo \
                      keeps the offline smoke covered.",
        script: include_str!("../../assets/demo/http-transport/scenario.harn"),
        tape: include_str!("../../assets/demo/http-transport/tape.jsonl"),
    },
    Scenario {
        id: "obs-primitive",
        title: "harness.obs.* spans + counter/histogram/gauge + audit roundtrip",
        description: "Drive the standardized observability primitive (#2513): open a span over a \
                      simulated `harn.session.put`, record one of each instrument variant \
                      (counter / histogram / gauge), emit a structured log inside the span, then \
                      drain the in-process buffer and surface a receipt of events-by-kind plus \
                      the bound request_id. Validates vocabulary at emit time and routes through \
                      the `test` backend so the scenario stays fully offline.",
        script: include_str!("../../assets/demo/obs-primitive/scenario.harn"),
        tape: include_str!("../../assets/demo/obs-primitive/tape.jsonl"),
    },
    Scenario {
        id: "edit-rename-symbol",
        title: "edit.rename_symbol rewrites a Rust struct across the workspace",
        description: "Stage a tiny Rust workspace, build the typed symbol graph (#2434), then \
                      drive `edit_rename_symbol` (#2508) through dry-run + applied + conflict \
                      paths: a workspace-scoped plan with per-edit byte/(row,col) spans, the \
                      same plan committed for real with identifier-context rewrites (skipping \
                      string literals and comments), and a follow-up rename whose new name \
                      already shadows another identifier — host short-circuits without \
                      touching disk. Fully offline.",
        script: include_str!("../../assets/demo/edit-rename-symbol/scenario.harn"),
        tape: include_str!("../../assets/demo/edit-rename-symbol/tape.jsonl"),
    },
];

#[derive(Clone, Copy)]
struct Scenario {
    id: &'static str,
    title: &'static str,
    description: &'static str,
    script: &'static str,
    tape: &'static str,
}

/// Public list of bundled scenario ids — used by tests and the smoke
/// loop that exercises every demo on every PR.
pub fn scenario_ids() -> Vec<&'static str> {
    SCENARIOS.iter().map(|s| s.id).collect()
}

pub(crate) async fn run(args: DemoArgs) -> i32 {
    if args.list {
        print_list_table(args.json);
        return 0;
    }

    let Some(scenario_id) = args.scenario.clone() else {
        if args.json {
            print_list_table(true);
            return 0;
        }
        if std::io::stdout().is_terminal() {
            return interactive_pick(&args).await;
        }
        // Non-interactive default: pick the first scenario.
        return run_scenario(&args, SCENARIOS[0]).await;
    };

    let Some(scenario) = lookup_scenario(&scenario_id) else {
        eprintln!("error: unknown scenario `{scenario_id}`");
        eprintln!();
        print_list_table(false);
        return 2;
    };
    run_scenario(&args, *scenario).await
}

fn lookup_scenario(id: &str) -> Option<&'static Scenario> {
    SCENARIOS.iter().find(|s| s.id == id)
}

fn print_list_table(as_json: bool) {
    if as_json {
        let entries: Vec<serde_json::Value> = SCENARIOS
            .iter()
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "title": s.title,
                    "description": s.description,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"scenarios": entries}))
                .unwrap_or_default()
        );
        return;
    }
    println!("Available demos (replayable offline, no API keys required):");
    println!();
    for s in SCENARIOS {
        println!("  {:<16}  {}", s.id, s.title);
        for line in wrap_text(s.description, 70) {
            println!("    {line}");
        }
        println!();
    }
    println!("Run a scenario:    harn demo <id>");
    println!("Use real provider: harn demo <id> --live");
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
            continue;
        }
        if current.len() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        } else {
            current.push(' ');
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

async fn interactive_pick(args: &DemoArgs) -> i32 {
    use std::io::Write;
    println!("Pick a Harn demo (offline replay, no API keys required):");
    println!();
    for (idx, s) in SCENARIOS.iter().enumerate() {
        println!("  {}) {:<16}  {}", idx + 1, s.id, s.title);
    }
    println!();
    print!("Choice [1-{}, default 1]: ", SCENARIOS.len());
    let _ = std::io::stdout().flush();
    let mut buf = String::new();
    let n = std::io::stdin().read_line(&mut buf).unwrap_or(0);
    let trimmed = buf.trim();
    let pick: usize = if n == 0 || trimmed.is_empty() {
        1
    } else {
        match trimmed.parse::<usize>() {
            Ok(n) if (1..=SCENARIOS.len()).contains(&n) => n,
            _ => {
                eprintln!("error: invalid selection `{trimmed}`");
                return 2;
            }
        }
    };
    run_scenario(args, SCENARIOS[pick - 1]).await
}

async fn run_scenario(args: &DemoArgs, scenario: Scenario) -> i32 {
    let staged = match stage_scenario(scenario) {
        Ok(s) => s,
        Err(error) => {
            eprintln!("error: {error}");
            return 1;
        }
    };

    if !args.json {
        println!("=== harn demo · {} ===", scenario.id);
        println!("{}", scenario.title);
        println!();
        if !args.live {
            println!("(offline replay — no API keys required)");
            println!();
        }
    }

    let llm_mock_mode = if args.live {
        if !args.json {
            println!("(--live: routing through the configured provider — set HARN_LLM_PROVIDER if none is wired)");
            println!();
        }
        CliLlmMockMode::Off
    } else {
        CliLlmMockMode::Replay {
            fixture_path: staged.tape_path.clone(),
        }
    };

    let started = Instant::now();
    let outcome = execute_run(
        staged.script_path.to_string_lossy().as_ref(),
        false,
        HashSet::new(),
        Vec::new(),
        Vec::new(),
        llm_mock_mode,
        None,
        RunProfileOptions::default(),
    )
    .await;
    let elapsed = started.elapsed();

    if !args.json && !outcome.stdout.is_empty() {
        print!("{}", outcome.stdout);
    }
    if !outcome.stderr.is_empty() {
        eprint!("{}", outcome.stderr);
    }

    if outcome.exit_code != 0 {
        if !args.json {
            eprintln!(
                "error: demo `{}` failed (exit {})",
                scenario.id, outcome.exit_code
            );
            if args.live && live_failure_looks_like_provider_misconfig(&outcome) {
                eprintln!();
                eprintln!("hint: --live needs a configured LLM provider. Re-run without --live");
                eprintln!("      to use the bundled offline tape, or run `harn quickstart`");
                eprintln!("      to wire a provider.");
            }
        } else {
            print_json_summary(scenario, &outcome, elapsed.as_millis(), None);
        }
        return outcome.exit_code;
    }

    let receipt_dir = if args.no_record {
        None
    } else {
        match write_run_record(scenario, &outcome) {
            Ok(path) => Some(path),
            Err(error) => {
                eprintln!("warning: failed to write demo run record: {error}");
                None
            }
        }
    };

    if args.json {
        print_json_summary(
            scenario,
            &outcome,
            elapsed.as_millis(),
            receipt_dir.as_deref(),
        );
    } else {
        println!();
        println!("--- demo complete in {} ms ---", elapsed.as_millis());
        if let Some(dir) = &receipt_dir {
            println!("  run record: {}", dir.join("run.json").display());
        }
        println!();
        println!("Next steps:");
        println!("  harn demo --list           list every bundled scenario");
        if !args.live {
            println!(
                "  harn demo {} --live      run again against the configured provider",
                scenario.id
            );
        }
        println!("  harn portal                browse run records in the UI");
    }
    0
}

struct StagedScenario {
    _temp_root: tempfile::TempDir,
    script_path: PathBuf,
    tape_path: PathBuf,
}

fn stage_scenario(scenario: Scenario) -> Result<StagedScenario, String> {
    let dir = tempfile::Builder::new()
        .prefix(&format!("harn-demo-{}-", scenario.id))
        .tempdir()
        .map_err(|e| format!("failed to create demo tempdir: {e}"))?;
    let script_path = dir.path().join(format!("{}.harn", scenario.id));
    let tape_path = dir.path().join(format!("{}.tape.jsonl", scenario.id));
    fs::write(&script_path, scenario.script)
        .map_err(|e| format!("failed to stage demo script: {e}"))?;
    fs::write(&tape_path, scenario.tape).map_err(|e| format!("failed to stage demo tape: {e}"))?;
    Ok(StagedScenario {
        _temp_root: dir,
        script_path,
        tape_path,
    })
}

fn write_run_record(scenario: Scenario, outcome: &RunOutcome) -> Result<PathBuf, String> {
    let cwd = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    let runs_root = cwd.join(".harn-runs");
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let started_iso = time::OffsetDateTime::from_unix_timestamp(ts as i64)
        .ok()
        .and_then(|t| {
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .unwrap_or_else(|| format!("1970-01-01T00:00:{ts:02}Z"));
    let dir = runs_root.join(format!("demo-{}-{ts}", scenario.id));
    fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    // Conform to the `run_record` envelope the portal scans for so the
    // demo shows up in `harn portal` alongside real workflow runs. The
    // demo-specific payload (script, tape ref, stdout/stderr) lives
    // under `metadata.demo` so portal listings can group on it.
    let record = serde_json::json!({
        "_type": "run_record",
        "id": format!("demo-{}-{ts}", scenario.id),
        "workflow_id": format!("harn-demo:{}", scenario.id),
        "workflow_name": scenario.title,
        "task": scenario.id,
        "status": if outcome.exit_code == 0 { "complete" } else { "failed" },
        "started_at": started_iso,
        "finished_at": started_iso,
        "stages": [],
        "transitions": [],
        "checkpoints": [],
        "pending_nodes": [],
        "completed_nodes": [],
        "child_runs": [],
        "artifacts": [],
        "policy": {},
        "metadata": {
            "demo": {
                "scenario": scenario.id,
                "title": scenario.title,
                "description": scenario.description,
                "exit_code": outcome.exit_code,
                "stdout": outcome.stdout,
                "stderr": outcome.stderr,
                "recorded_at_unix_seconds": ts,
            }
        },
    });
    let path = dir.join("run.json");
    fs::write(
        &path,
        serde_json::to_string_pretty(&record).unwrap_or_default(),
    )
    .map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(dir)
}

fn live_failure_looks_like_provider_misconfig(outcome: &RunOutcome) -> bool {
    // Heuristic on the rendered diagnostic — every error path Harn
    // surfaces for "no key / wrong key / no provider" is one of these
    // category strings or substrings. Avoids over-firing on script
    // bugs that happen to fail under `--live`.
    let blob = format!("{}{}", outcome.stderr, outcome.stdout);
    blob.contains("category: auth")
        || blob.contains("auth_failure")
        || blob.contains("HTTP 401")
        || blob.contains("HTTP 403")
        || blob.contains("api_key")
        || blob.contains("HARN_LLM_PROVIDER")
        || blob.contains("no provider configured")
}

fn print_json_summary(
    scenario: Scenario,
    outcome: &RunOutcome,
    elapsed_ms: u128,
    record_dir: Option<&Path>,
) {
    let record = serde_json::json!({
        "scenario": scenario.id,
        "title": scenario.title,
        "exit_code": outcome.exit_code,
        "elapsed_ms": elapsed_ms,
        "stdout": outcome.stdout,
        "stderr": outcome.stderr,
        "run_record_dir": record_dir.map(|p| p.display().to_string()),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&record).unwrap_or_default()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenarios_have_unique_nonempty_ids() {
        let mut seen = HashSet::new();
        for s in SCENARIOS {
            assert!(!s.id.is_empty(), "scenario id is empty");
            assert!(!s.title.is_empty(), "scenario {} has empty title", s.id);
            assert!(
                !s.description.is_empty(),
                "scenario {} has empty description",
                s.id
            );
            assert!(!s.script.is_empty(), "scenario {} script is empty", s.id);
            assert!(!s.tape.is_empty(), "scenario {} tape is empty", s.id);
            assert!(seen.insert(s.id), "duplicate scenario id: {}", s.id);
        }
    }

    #[test]
    fn scenario_tape_lines_parse_as_json() {
        for s in SCENARIOS {
            for (i, line) in s.tape.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|e| {
                    panic!("scenario {} tape line {} is invalid JSON: {e}", s.id, i + 1)
                });
            }
        }
    }

    #[test]
    fn scenario_ids_match_assets_dir_names() {
        // Sanity: the const SCENARIOS array's ids should mirror the
        // checked-in asset directories. If a developer adds a new
        // scenario but forgets to wire it into SCENARIOS, this test
        // does nothing — but if they rename an asset dir without
        // updating the const, the include_str! at top will fail to
        // compile, which is the better failure mode.
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let assets = std::path::Path::new(manifest_dir).join("assets/demo");
        for s in SCENARIOS {
            let dir = assets.join(s.id);
            assert!(dir.is_dir(), "missing demo asset dir for {}", s.id);
            assert!(
                dir.join("scenario.harn").is_file(),
                "missing scenario.harn for {}",
                s.id
            );
            assert!(
                dir.join("tape.jsonl").is_file(),
                "missing tape.jsonl for {}",
                s.id
            );
        }
    }
}
