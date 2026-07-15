use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn eval_pack_trial_report(
    trial: usize,
    severity: &str,
    blocking: bool,
    run_id: String,
    workflow_id: String,
    source_path: Option<String>,
    stage_count: usize,
    timed_out: bool,
    wall_time_seconds: f64,
    cost_usd: f64,
    mut failures: Vec<String>,
    mut warnings: Vec<String>,
    mut informational: Vec<String>,
    comparison: Option<RunDiffReport>,
) -> EvalPackTrialReport {
    let verification = if failures.is_empty() { "PASS" } else { "FAIL" }.to_string();
    let pass = failures.is_empty() || !blocking;
    if !failures.is_empty() && !blocking {
        if severity == "warning" {
            warnings.append(&mut failures);
        } else {
            informational.append(&mut failures);
        }
    }
    EvalPackTrialReport {
        trial,
        verification,
        verification_exit_code: None,
        pass,
        blocking,
        run_id,
        workflow_id,
        source_path,
        stage_count,
        failures,
        warnings,
        informational,
        comparison,
        timed_out,
        wall_time_seconds,
        cost_usd,
        produced_paths: Vec::new(),
        tool_call_summary: serde_json::Value::Null,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn eval_ledger_row_from_trial(
    case: &EvalPackCase,
    case_id: &str,
    split: Option<String>,
    suite: &str,
    model: &str,
    commit: &str,
    provenance: &EvalLedgerProvenance,
    harness_config_fingerprint: &str,
    trial: &EvalPackTrialReport,
) -> EvalLedgerRow {
    let passes = usize::from(trial.verification == "PASS");
    let fails = usize::from(trial.verification == "FAIL");
    let skips = usize::from(trial.verification.eq_ignore_ascii_case("skip"));
    let timeouts = usize::from(trial.timed_out);
    EvalLedgerRow {
        schema: EVAL_LEDGER_ROW_SCHEMA.to_string(),
        suite: suite.to_string(),
        model: model.to_string(),
        split,
        commit: commit.to_string(),
        case_name: case_id.to_string(),
        name: case_id.to_string(),
        case_fingerprint: case.case_fingerprint.clone(),
        harness_config_fingerprint: harness_config_fingerprint.to_string(),
        trial: trial.trial,
        trials: 1,
        passes,
        fails,
        skips,
        timeouts,
        pass_rate: passes as f64,
        status: trial.verification.clone(),
        verification: trial.verification.clone(),
        skipped: false,
        wall_time_seconds: trial.wall_time_seconds,
        cost_usd: trial.cost_usd,
        mean_wall_time_seconds: trial.wall_time_seconds,
        total_cost_usd: trial.cost_usd,
        run_id: trial.run_id.clone(),
        workflow_id: trial.workflow_id.clone(),
        source_path: trial.source_path.clone(),
        trial_report: Some(trial.clone()),
        provenance: provenance.clone(),
        metadata: case.metadata.clone(),
        ..EvalLedgerRow::default()
    }
}

pub(super) fn eval_pack_trial_report_from_ledger_row(
    row: &EvalLedgerRow,
    blocking: bool,
) -> EvalPackTrialReport {
    if let Some(mut report) = row.trial_report.clone() {
        report.trial = row.trial;
        return report;
    }
    let mut failures = Vec::new();
    let verification = if row.verification.is_empty() {
        row.status.clone()
    } else {
        row.verification.clone()
    };
    if verification == "FAIL" {
        failures.push("ledger row recorded a failed trial".to_string());
    }
    EvalPackTrialReport {
        trial: row.trial,
        verification: verification.clone(),
        verification_exit_code: None,
        pass: verification != "FAIL" || !blocking,
        blocking,
        run_id: row.run_id.clone(),
        workflow_id: row.workflow_id.clone(),
        source_path: row.source_path.clone(),
        stage_count: 0,
        failures,
        warnings: Vec::new(),
        informational: Vec::new(),
        comparison: None,
        timed_out: row.timeouts > 0,
        wall_time_seconds: row.wall_time_seconds,
        cost_usd: row.cost_usd,
        produced_paths: Vec::new(),
        tool_call_summary: serde_json::Value::Null,
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn eval_pack_case_report_from_trials(
    case: &EvalPackCase,
    case_id: String,
    label: String,
    severity: String,
    split: Option<String>,
    blocking: bool,
    harness_config_fingerprint: String,
    trials: Vec<EvalPackTrialReport>,
) -> EvalPackCaseReport {
    let reliability = eval_pack_reliability_report(&trials);
    let stats_row = eval_pack_stats_row(
        case,
        &case_id,
        &harness_config_fingerprint,
        split.clone(),
        &trials,
        &reliability,
    );
    let first = trials.first();
    let pass = if blocking {
        reliability.status == "all-pass"
    } else {
        true
    };
    let failures = prefixed_trial_messages(&trials, |trial| &trial.failures);
    let warnings = prefixed_trial_messages(&trials, |trial| &trial.warnings);
    let informational = prefixed_trial_messages(&trials, |trial| &trial.informational);
    EvalPackCaseReport {
        id: case_id,
        label,
        severity,
        split,
        case_fingerprint: case.case_fingerprint.clone(),
        harness_config_fingerprint,
        pass,
        blocking,
        run_id: first.map(|trial| trial.run_id.clone()).unwrap_or_default(),
        workflow_id: first
            .map(|trial| trial.workflow_id.clone())
            .unwrap_or_default(),
        source_path: first.and_then(|trial| trial.source_path.clone()),
        stage_count: first.map(|trial| trial.stage_count).unwrap_or_default(),
        trial_count: trials.len(),
        total_stage_count: trials.iter().map(|trial| trial.stage_count).sum(),
        reliability,
        stats_row,
        comparison: first.and_then(|trial| trial.comparison.clone()),
        trials,
        failures,
        warnings,
        informational,
    }
}

fn prefixed_trial_messages<F>(trials: &[EvalPackTrialReport], messages: F) -> Vec<String>
where
    F: Fn(&EvalPackTrialReport) -> &Vec<String>,
{
    let include_prefix = trials.len() > 1;
    let mut out = Vec::new();
    for trial in trials {
        for message in messages(trial) {
            if include_prefix {
                out.push(format!("trial {}: {message}", trial.trial));
            } else {
                out.push(message.clone());
            }
        }
    }
    out
}

fn eval_pack_reliability_report(trials: &[EvalPackTrialReport]) -> EvalPackReliabilityReport {
    let passes = trials
        .iter()
        .filter(|trial| trial.verification == "PASS")
        .count();
    let fails = trials
        .iter()
        .filter(|trial| trial.verification == "FAIL")
        .count();
    let skips = trials
        .iter()
        .filter(|trial| trial.verification.eq_ignore_ascii_case("skip"))
        .count();
    let timeouts = trials.iter().filter(|trial| trial.timed_out).count();
    let decided = passes + fails;
    let majority = if passes > 0 && fails > 0 {
        Some(if passes >= fails { "PASS" } else { "FAIL" }.to_string())
    } else {
        None
    };
    let status = if decided == 0 {
        "no-decision"
    } else if fails == 0 {
        "all-pass"
    } else if passes == 0 {
        "all-fail"
    } else {
        "flaky"
    };
    EvalPackReliabilityReport {
        status: status.to_string(),
        trials: trials.len(),
        passes,
        fails,
        skips,
        timeouts,
        decided,
        pass_rate: if trials.is_empty() {
            0.0
        } else {
            passes as f64 / trials.len() as f64
        },
        majority,
    }
}

fn eval_pack_stats_row(
    case: &EvalPackCase,
    case_id: &str,
    harness_config_fingerprint: &str,
    split: Option<String>,
    trials: &[EvalPackTrialReport],
    reliability: &EvalPackReliabilityReport,
) -> EvalPackStatsRow {
    let wall_times = trials
        .iter()
        .map(|trial| trial.wall_time_seconds)
        .collect::<Vec<_>>();
    let costs = trials
        .iter()
        .map(|trial| trial.cost_usd)
        .collect::<Vec<_>>();
    let group = case
        .metadata
        .get("group")
        .or_else(|| case.metadata.get("language"))
        .or_else(|| case.metadata.get("bucket"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_string();
    EvalPackStatsRow {
        name: case_id.to_string(),
        case_name: case_id.to_string(),
        case_fingerprint: case.case_fingerprint.clone(),
        harness_config_fingerprint: harness_config_fingerprint.to_string(),
        group,
        metadata: case.metadata.clone(),
        split,
        trials: trials.len(),
        passes: reliability.passes,
        fails: reliability.fails,
        skips: reliability.skips,
        timeouts: reliability.timeouts,
        pass_rate: reliability.pass_rate,
        status: match reliability.status.as_str() {
            "all-pass" => "PASS",
            "all-fail" => "FAIL",
            "flaky" => "FLAKY",
            _ => "skip",
        }
        .to_string(),
        majority: reliability.majority.clone(),
        wall_time_seconds: mean(&wall_times),
        cost_usd: costs.iter().sum(),
        mean_wall_time_seconds: mean(&wall_times),
        stdev_wall_time_seconds: stdev(&wall_times),
        total_cost_usd: costs.iter().sum(),
    }
}

pub(super) fn eval_pack_stats_report(rows: &[EvalPackStatsRow]) -> EvalPackStatsReport {
    EvalPackStatsReport {
        macro_pass_at_1: macro_pass_at_1(rows),
        reliability: eval_pack_reliability_breakdown(rows),
    }
}

fn macro_pass_at_1(rows: &[EvalPackStatsRow]) -> f64 {
    let decided = rows
        .iter()
        .filter(|row| row.passes + row.fails > 0)
        .collect::<Vec<_>>();
    if decided.is_empty() {
        return 0.0;
    }
    decided.iter().map(|row| row.pass_rate).sum::<f64>() / decided.len() as f64
}

fn eval_pack_reliability_breakdown(rows: &[EvalPackStatsRow]) -> EvalPackReliabilityBreakdown {
    let total_cases = rows.len();
    let all_pass_cases = rows
        .iter()
        .filter(|row| row.passes > 0 && row.fails == 0)
        .count();
    let flaky_cases = rows
        .iter()
        .filter(|row| row.passes > 0 && row.fails > 0)
        .count();
    let all_fail_cases = rows
        .iter()
        .filter(|row| row.passes == 0 && row.fails > 0)
        .count();
    let no_decision_cases = rows
        .iter()
        .filter(|row| row.passes + row.fails == 0)
        .count();
    EvalPackReliabilityBreakdown {
        all_pass_cases,
        flaky_cases,
        all_fail_cases,
        no_decision_cases,
        total_cases,
        all_pass_fraction: rate(all_pass_cases, total_cases),
        flaky_fraction: rate(flaky_cases, total_cases),
        all_fail_fraction: rate(all_fail_cases, total_cases),
        no_decision_fraction: rate(no_decision_cases, total_cases),
    }
}

pub(super) fn split_by_case_id(report: &EvalPackSplitValidationReport) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (partition, cases) in &report.partitions {
        for case_id in cases {
            out.insert(case_id.clone(), partition.clone());
        }
    }
    out
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f64>() / values.len() as f64
}

fn stdev(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mean = mean(values);
    let variance = values
        .iter()
        .map(|value| {
            let diff = value - mean;
            diff * diff
        })
        .sum::<f64>()
        / values.len() as f64;
    variance.sqrt()
}

fn rate(count: usize, denom: usize) -> f64 {
    if denom == 0 {
        0.0
    } else {
        count as f64 / denom as f64
    }
}

pub(super) fn eval_pack_case_severity(manifest: &EvalPackManifest, case: &EvalPackCase) -> String {
    normalize_eval_pack_severity(
        case.severity
            .as_deref()
            .or(case.thresholds.severity.as_deref())
            .or(manifest.defaults.severity.as_deref())
            .or(manifest.defaults.thresholds.severity.as_deref())
            .unwrap_or("blocking"),
    )
}

pub(super) fn normalize_eval_pack_severity(value: &str) -> String {
    match value.trim().to_ascii_lowercase().as_str() {
        "warn" | "warning" => "warning".to_string(),
        "info" | "informational" => "informational".to_string(),
        _ => "blocking".to_string(),
    }
}
