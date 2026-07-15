use super::*;

pub fn eval_ledger_read_report(
    options: Option<serde_json::Value>,
) -> Result<EvalLedgerReadReport, VmError> {
    let options = eval_ledger_options(options)?;
    let namespace = eval_ledger_namespace(&options);
    let topic = eval_ledger_topic(&namespace)?;
    let log = ensure_eval_ledger_event_log(None);
    let rows = futures::executor::block_on(read_eval_ledger_rows(&log, &topic, &options))?;
    Ok(EvalLedgerReadReport { rows })
}

pub fn eval_ledger_append_rows_report(
    rows: serde_json::Value,
    options: Option<serde_json::Value>,
) -> Result<EvalLedgerAppendReport, VmError> {
    let options = eval_ledger_options(options)?;
    let namespace = eval_ledger_namespace(&options);
    let topic = eval_ledger_topic(&namespace)?;
    let provenance = eval_ledger_provenance(None, &options, None);
    let rows = parse_eval_ledger_rows(rows)?
        .into_iter()
        .map(|mut row| {
            normalize_eval_ledger_row(&mut row, &options, &provenance);
            row
        })
        .collect::<Vec<_>>();
    let log = ensure_eval_ledger_event_log(None);
    futures::executor::block_on(append_eval_ledger_rows(&log, &topic, rows))
}

pub fn eval_ledger_prior_commit_rows_report(
    options: serde_json::Value,
) -> Result<EvalLedgerPriorCommitReport, VmError> {
    let options = eval_ledger_options(Some(options))?;
    let namespace = eval_ledger_namespace(&options);
    let topic = eval_ledger_topic(&namespace)?;
    let log = ensure_eval_ledger_event_log(None);
    let mut read_options = options.clone();
    read_options.commit = None;
    read_options.case_fingerprint = None;
    read_options.harness_config_fingerprint = None;
    let rows = futures::executor::block_on(read_eval_ledger_rows(&log, &topic, &read_options))?;
    Ok(prior_commit_report(rows, &options))
}

pub fn eval_ledger_resume_plan_report(
    manifest: &EvalPackManifest,
    options: Option<serde_json::Value>,
) -> Result<EvalLedgerResumePlan, VmError> {
    let split_report = validate_eval_pack_split(manifest)?;
    let harness_config_fingerprint = eval_pack_harness_config_fingerprint(manifest)?;
    let options = eval_ledger_options(options)?;
    let base_dir = manifest.base_dir.as_deref().map(Path::new);
    let suite = options.suite.clone().unwrap_or_else(|| manifest.id.clone());
    let model = options
        .model
        .clone()
        .or_else(|| eval_pack_manifest_model(manifest))
        .unwrap_or_else(|| "unknown".to_string());
    let provenance = eval_ledger_provenance(base_dir, &options, Some(&manifest.metadata));
    let commit = options
        .commit
        .clone()
        .unwrap_or_else(|| provenance.commit.clone());
    let namespace = eval_pack_ledger_namespace(manifest, &options);
    let topic = eval_ledger_topic(&namespace)?;
    let log = ensure_eval_ledger_event_log(base_dir);
    let read_options = eval_pack_ledger_read_options(&suite, &model, &commit);
    let rows = futures::executor::block_on(read_eval_ledger_rows(&log, &topic, &read_options))?;
    Ok(build_eval_ledger_resume_plan(
        manifest,
        &split_report,
        &rows,
        &suite,
        &model,
        &commit,
        &harness_config_fingerprint,
    ))
}

fn eval_ledger_options(value: Option<serde_json::Value>) -> Result<EvalLedgerOptions, VmError> {
    let mut options = match value {
        None | Some(serde_json::Value::Null) => EvalLedgerOptions::default(),
        Some(value) => serde_json::from_value(value)
            .map_err(|e| VmError::Runtime(format!("eval ledger options parse error: {e}")))?,
    };
    normalize_optional_string(&mut options.namespace);
    normalize_optional_string(&mut options.suite);
    normalize_optional_string(&mut options.model);
    normalize_optional_string(&mut options.split);
    normalize_optional_string(&mut options.commit);
    normalize_optional_string(&mut options.branch);
    normalize_optional_string(&mut options.case_name);
    normalize_optional_string(&mut options.case_fingerprint);
    normalize_optional_string(&mut options.harness_config_fingerprint);
    Ok(options)
}

fn normalize_optional_string(value: &mut Option<String>) {
    if value.as_deref().is_some_and(|text| text.trim().is_empty()) {
        *value = None;
    }
}

fn parse_eval_ledger_rows(value: serde_json::Value) -> Result<Vec<EvalLedgerRow>, VmError> {
    match value {
        serde_json::Value::Array(_) => serde_json::from_value(value)
            .map_err(|e| VmError::Runtime(format!("eval ledger rows parse error: {e}"))),
        serde_json::Value::Object(_) => serde_json::from_value(value)
            .map(|row| vec![row])
            .map_err(|e| VmError::Runtime(format!("eval ledger row parse error: {e}"))),
        _ => Err(VmError::Runtime(
            "eval ledger rows must be a row dict or list of row dicts".to_string(),
        )),
    }
}

fn eval_ledger_namespace(options: &EvalLedgerOptions) -> String {
    options
        .namespace
        .clone()
        .or_else(|| options.suite.clone())
        .unwrap_or_else(|| "default".to_string())
}

fn eval_pack_ledger_namespace(manifest: &EvalPackManifest, options: &EvalLedgerOptions) -> String {
    options
        .namespace
        .clone()
        .or_else(|| metadata_string(&manifest.metadata, &["ledger_namespace", "ledgerNamespace"]))
        .or_else(|| options.suite.clone())
        .unwrap_or_else(|| manifest.id.clone())
}

fn eval_pack_ledger_read_options(suite: &str, model: &str, commit: &str) -> EvalLedgerOptions {
    EvalLedgerOptions {
        suite: Some(suite.to_string()),
        model: Some(model.to_string()),
        commit: Some(commit.to_string()),
        ..EvalLedgerOptions::default()
    }
}

fn eval_ledger_topic(namespace: &str) -> Result<crate::event_log::Topic, VmError> {
    let safe_namespace = crate::event_log::sanitize_topic_component(namespace);
    crate::event_log::Topic::new(format!("{EVAL_LEDGER_TOPIC_PREFIX}.{safe_namespace}"))
        .map_err(eval_ledger_log_error)
}

fn ensure_eval_ledger_event_log(base_dir: Option<&Path>) -> Arc<crate::event_log::AnyEventLog> {
    if let Some(log) = crate::event_log::active_event_log() {
        return log;
    }
    if let Some(base_dir) = base_dir {
        if crate::event_log::install_lazy_default_for_base_dir(base_dir).is_ok() {
            if let Some(log) = crate::event_log::active_event_log() {
                return log;
            }
        }
    } else if let Ok(cwd) = std::env::current_dir() {
        if crate::event_log::install_lazy_default_for_base_dir(&cwd).is_ok() {
            if let Some(log) = crate::event_log::active_event_log() {
                return log;
            }
        }
    }
    crate::event_log::install_memory_for_current_thread(EVAL_LEDGER_QUEUE_DEPTH)
}

async fn read_eval_ledger_rows(
    log: &Arc<crate::event_log::AnyEventLog>,
    topic: &crate::event_log::Topic,
    options: &EvalLedgerOptions,
) -> Result<Vec<EvalLedgerRow>, VmError> {
    let mut rows = Vec::new();
    let mut cursor = None;
    loop {
        let batch = log
            .read_range(topic, cursor, EVAL_LEDGER_READ_BATCH_LIMIT)
            .await
            .map_err(eval_ledger_log_error)?;
        if batch.is_empty() {
            break;
        }
        for (event_id, event) in batch {
            cursor = Some(event_id);
            if let Some(row) = parse_eval_ledger_row(event_id, event) {
                if eval_ledger_row_matches(&row, options) {
                    rows.push(row);
                    if options.limit.is_some_and(|limit| rows.len() >= limit) {
                        return Ok(rows);
                    }
                }
            }
        }
    }
    Ok(rows)
}

async fn append_eval_ledger_rows(
    log: &Arc<crate::event_log::AnyEventLog>,
    topic: &crate::event_log::Topic,
    rows: Vec<EvalLedgerRow>,
) -> Result<EvalLedgerAppendReport, VmError> {
    let mut report = EvalLedgerAppendReport {
        appended: rows.len(),
        all_skipped: !rows.is_empty() && rows.iter().all(eval_ledger_row_is_skip),
        ..EvalLedgerAppendReport::default()
    };
    for row in rows {
        let identity = eval_ledger_row_identity(&row)?;
        let mut headers = BTreeMap::new();
        headers.insert(EVAL_LEDGER_IDENTITY_HEADER.to_string(), identity.clone());
        headers.insert("suite".to_string(), row.suite.clone());
        headers.insert("model".to_string(), row.model.clone());
        headers.insert("commit".to_string(), row.commit.clone());
        headers.insert("case_name".to_string(), row.case_name.clone());
        headers.insert("trial".to_string(), row.trial.to_string());
        let payload = serde_json::to_value(&row)
            .map_err(|e| VmError::Runtime(format!("eval ledger row encode error: {e}")))?;
        let outcome = log
            .append_idempotent_by_header(
                topic,
                EVAL_LEDGER_IDENTITY_HEADER,
                &identity,
                crate::event_log::LogEvent::new(EVAL_LEDGER_ROW_KIND, payload)
                    .with_headers(headers),
            )
            .await
            .map_err(eval_ledger_log_error)?;
        if outcome.inserted {
            report.inserted += 1;
        } else {
            report.duplicates += 1;
        }
        report.event_ids.push(outcome.event_id);
        if let Some(stored) = parse_eval_ledger_row(outcome.event_id, outcome.event) {
            report.rows.push(stored);
        }
    }
    log.flush().await.map_err(eval_ledger_log_error)?;
    Ok(report)
}

fn parse_eval_ledger_row(
    event_id: crate::event_log::EventId,
    event: crate::event_log::LogEvent,
) -> Option<EvalLedgerRow> {
    if event.kind != EVAL_LEDGER_ROW_KIND {
        return None;
    }
    let mut row: EvalLedgerRow = serde_json::from_value(event.payload).ok()?;
    if row.schema != EVAL_LEDGER_ROW_SCHEMA {
        return None;
    }
    row.event_id = Some(event_id);
    Some(row)
}

fn eval_ledger_row_matches(row: &EvalLedgerRow, options: &EvalLedgerOptions) -> bool {
    option_matches(options.suite.as_deref(), &row.suite)
        && option_matches(options.model.as_deref(), &row.model)
        && option_matches(options.commit.as_deref(), &row.commit)
        && option_matches(options.case_name.as_deref(), &row.case_name)
        && option_matches(options.case_fingerprint.as_deref(), &row.case_fingerprint)
        && option_matches(
            options.harness_config_fingerprint.as_deref(),
            &row.harness_config_fingerprint,
        )
        && match options.split.as_deref() {
            Some(expected) => row.split.as_deref() == Some(expected),
            None => true,
        }
}

fn option_matches(expected: Option<&str>, actual: &str) -> bool {
    expected.is_none_or(|expected| expected == actual)
}

fn normalize_eval_ledger_row(
    row: &mut EvalLedgerRow,
    options: &EvalLedgerOptions,
    provenance: &EvalLedgerProvenance,
) {
    if row.schema.is_empty() {
        row.schema = EVAL_LEDGER_ROW_SCHEMA.to_string();
    }
    if row.suite.is_empty() {
        row.suite = options
            .suite
            .clone()
            .unwrap_or_else(|| eval_ledger_namespace(options));
    }
    if row.model.is_empty() {
        row.model = options
            .model
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
    }
    if row.split.is_none() {
        row.split = options.split.clone();
    }
    if row.commit.is_empty() {
        row.commit = options
            .commit
            .clone()
            .unwrap_or_else(|| provenance.commit.clone());
    }
    if row.case_name.is_empty() {
        row.case_name = options
            .case_name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| row.name.clone());
    }
    if row.name.is_empty() {
        row.name = row.case_name.clone();
    }
    if row.case_fingerprint.is_empty() {
        row.case_fingerprint = options.case_fingerprint.clone().unwrap_or_default();
    }
    if row.harness_config_fingerprint.is_empty() {
        row.harness_config_fingerprint = options
            .harness_config_fingerprint
            .clone()
            .unwrap_or_default();
    }
    if row.trial == 0 {
        row.trial = 1;
    }
    if row.trials == 0 {
        row.trials = 1;
    }
    if row.status.is_empty() {
        row.status = if row.passes > 0 {
            "PASS"
        } else if row.fails > 0 {
            "FAIL"
        } else {
            "skip"
        }
        .to_string();
    }
    if row.verification.is_empty() {
        row.verification = row.status.clone();
    }
    if row.passes + row.fails + row.skips == 0 {
        match row.status.to_ascii_uppercase().as_str() {
            "PASS" => row.passes = 1,
            "FAIL" => row.fails = 1,
            _ => row.skips = 1,
        }
    }
    if row.pass_rate == 0.0 && row.passes > 0 {
        row.pass_rate = row.passes as f64 / row.trials.max(1) as f64;
    }
    if row.provenance.commit.is_empty() {
        row.provenance.commit = row.commit.clone();
    }
    if row.provenance.branch.is_none() {
        row.provenance.branch = provenance.branch.clone();
    }
    if row.provenance.ts.is_empty() {
        row.provenance.ts = provenance.ts.clone();
    }
    if row.provenance.harn_version.is_empty() {
        row.provenance.harn_version = provenance.harn_version.clone();
    }
    if row.provenance.host.is_empty() {
        row.provenance.host = provenance.host.clone();
    }
}

fn eval_ledger_row_identity(row: &EvalLedgerRow) -> Result<String, VmError> {
    let material = serde_json::json!({
        "schema": EVAL_LEDGER_ROW_SCHEMA,
        "suite": row.suite,
        "model": row.model,
        "split": row.split,
        "commit": row.commit,
        "case_name": row.case_name,
        "case_fingerprint": row.case_fingerprint,
        "harness_config_fingerprint": row.harness_config_fingerprint,
        "trial": row.trial,
    });
    let bytes = serde_json::to_vec(&material)
        .map_err(|e| VmError::Runtime(format!("eval ledger identity encode error: {e}")))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

fn eval_ledger_row_is_skip(row: &EvalLedgerRow) -> bool {
    row.skipped || row.skips > 0 || row.status.eq_ignore_ascii_case("skip")
}

fn eval_ledger_provenance(
    base_dir: Option<&Path>,
    options: &EvalLedgerOptions,
    metadata: Option<&BTreeMap<String, serde_json::Value>>,
) -> EvalLedgerProvenance {
    let commit = options
        .commit
        .clone()
        .or_else(|| {
            metadata.and_then(|metadata| {
                metadata_string(metadata, &["commit", "git_commit", "source_commit"])
            })
        })
        .or_else(|| env_string(&["HARN_EVAL_COMMIT", "HARN_GIT_COMMIT", "GITHUB_SHA"]))
        .or_else(|| git_output(base_dir, &["rev-parse", "HEAD"]))
        .unwrap_or_else(|| "unknown".to_string());
    let branch = options
        .branch
        .clone()
        .or_else(|| {
            metadata.and_then(|metadata| {
                metadata_string(metadata, &["branch", "git_branch", "source_branch"])
            })
        })
        .or_else(|| env_string(&["HARN_EVAL_BRANCH", "HARN_GIT_BRANCH", "GITHUB_REF_NAME"]))
        .or_else(|| git_output(base_dir, &["rev-parse", "--abbrev-ref", "HEAD"]));
    EvalLedgerProvenance {
        commit,
        branch,
        ts: now_rfc3339(),
        harn_version: crate::bytecode_cache::HARN_VERSION.to_string(),
        host: env_string(&["HOSTNAME", "COMPUTERNAME"]).unwrap_or_else(|| "unknown".to_string()),
    }
}

fn env_string(keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        std::env::var(key)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn git_output(base_dir: Option<&Path>, args: &[&str]) -> Option<String> {
    let mut command = std::process::Command::new("git");
    if let Some(base_dir) = base_dir {
        command.arg("-C").arg(base_dir);
    }
    let output = command.args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn metadata_string(
    metadata: &BTreeMap<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| json_value_string(metadata.get(*key)?))
}

fn json_value_string(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.trim().to_string()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
    .filter(|value| !value.is_empty())
}

pub(super) fn eval_pack_manifest_model(manifest: &EvalPackManifest) -> Option<String> {
    metadata_string(&manifest.metadata, &["model", "provider_model", "route"])
        .or_else(|| {
            manifest
                .judge
                .as_ref()
                .and_then(|judge| judge.model.clone())
        })
        .or_else(|| {
            manifest
                .defaults
                .judge
                .as_ref()
                .and_then(|judge| judge.model.clone())
        })
}

fn prior_commit_report(
    rows: Vec<EvalLedgerRow>,
    options: &EvalLedgerOptions,
) -> EvalLedgerPriorCommitReport {
    let current_commit = options.commit.as_deref().unwrap_or_default();
    let mut fingerprint_mismatches = Vec::new();
    let mut candidates = Vec::new();
    let mut latest_event_by_commit = BTreeMap::<String, u64>::new();
    for row in rows {
        if row.commit == current_commit {
            continue;
        }
        if let Some(mismatch) = fingerprint_mismatch_for_row(&row, options) {
            fingerprint_mismatches.push(mismatch);
            continue;
        }
        let event_id = row.event_id.unwrap_or_default();
        latest_event_by_commit
            .entry(row.commit.clone())
            .and_modify(|existing| *existing = (*existing).max(event_id))
            .or_insert(event_id);
        candidates.push(row);
    }
    let selected_commit = latest_event_by_commit
        .iter()
        .max_by_key(|(_, event_id)| *event_id)
        .map(|(commit, _)| commit.clone());
    let rows = selected_commit
        .as_ref()
        .map(|commit| {
            candidates
                .into_iter()
                .filter(|row| &row.commit == commit)
                .collect()
        })
        .unwrap_or_default();
    EvalLedgerPriorCommitReport {
        commit: selected_commit,
        model: options.model.clone().unwrap_or_default(),
        split: options.split.clone(),
        rows,
        fingerprint_mismatches,
    }
}

fn fingerprint_mismatch_for_row(
    row: &EvalLedgerRow,
    options: &EvalLedgerOptions,
) -> Option<EvalLedgerFingerprintMismatch> {
    let expected_case = options.case_fingerprint.as_deref();
    let expected_harness = options.harness_config_fingerprint.as_deref();
    let case_mismatch = expected_case.is_some_and(|expected| expected != row.case_fingerprint);
    let harness_mismatch =
        expected_harness.is_some_and(|expected| expected != row.harness_config_fingerprint);
    if !(case_mismatch || harness_mismatch) {
        return None;
    }
    Some(EvalLedgerFingerprintMismatch {
        case_name: row.case_name.clone(),
        split: row.split.clone(),
        commit: row.commit.clone(),
        trial: row.trial,
        case_fingerprint: row.case_fingerprint.clone(),
        harness_config_fingerprint: row.harness_config_fingerprint.clone(),
        expected_case_fingerprint: expected_case.unwrap_or_default().to_string(),
        expected_harness_config_fingerprint: expected_harness.unwrap_or_default().to_string(),
    })
}

fn build_eval_ledger_resume_plan(
    manifest: &EvalPackManifest,
    split_report: &EvalPackSplitValidationReport,
    rows: &[EvalLedgerRow],
    suite: &str,
    model: &str,
    commit: &str,
    harness_config_fingerprint: &str,
) -> EvalLedgerResumePlan {
    let split_by_case = split_by_case_id(split_report);
    let mut cells = Vec::new();
    let mut fingerprint_refusals = Vec::new();
    let mut skipped_cells = 0usize;
    for (index, case) in manifest.cases.iter().enumerate() {
        let case_id = eval_pack_case_id(case, index);
        let split = split_by_case.get(&case_id).cloned();
        let trial_count = case.trials.unwrap_or(manifest.trials);
        for trial in 1..=trial_count {
            let matching = ledger_rows_for_cell(
                rows,
                suite,
                model,
                split.as_deref(),
                commit,
                &case_id,
                trial,
            );
            let exact = matching
                .iter()
                .copied()
                .filter(|row| {
                    row.case_fingerprint == case.case_fingerprint
                        && row.harness_config_fingerprint == harness_config_fingerprint
                })
                .max_by_key(|row| row.event_id.unwrap_or_default());
            if let Some(row) = exact {
                skipped_cells += 1;
                cells.push(EvalLedgerResumeCell {
                    case_name: case_id.clone(),
                    split: split.clone(),
                    trial,
                    status: "skip".to_string(),
                    reason: "matching ledger row".to_string(),
                    event_id: row.event_id,
                });
                continue;
            }
            let mut refused = false;
            for row in matching {
                if row.case_fingerprint != case.case_fingerprint
                    || row.harness_config_fingerprint != harness_config_fingerprint
                {
                    fingerprint_refusals.push(EvalLedgerFingerprintMismatch {
                        case_name: case_id.clone(),
                        split: split.clone(),
                        commit: row.commit.clone(),
                        trial,
                        case_fingerprint: row.case_fingerprint.clone(),
                        harness_config_fingerprint: row.harness_config_fingerprint.clone(),
                        expected_case_fingerprint: case.case_fingerprint.clone(),
                        expected_harness_config_fingerprint: harness_config_fingerprint.to_string(),
                    });
                    refused = true;
                }
            }
            cells.push(EvalLedgerResumeCell {
                case_name: case_id.clone(),
                split: split.clone(),
                trial,
                status: "run".to_string(),
                reason: if refused {
                    "fingerprint mismatch".to_string()
                } else {
                    "missing ledger row".to_string()
                },
                event_id: None,
            });
        }
    }
    let requested_cells = cells.len();
    let remaining_cells = requested_cells.saturating_sub(skipped_cells);
    EvalLedgerResumePlan {
        schema: EVAL_LEDGER_RESUME_PLAN_SCHEMA.to_string(),
        suite: suite.to_string(),
        model: model.to_string(),
        commit: commit.to_string(),
        harness_config_fingerprint: harness_config_fingerprint.to_string(),
        requested_cells,
        completed_cells: skipped_cells,
        skipped_cells,
        remaining_cells,
        all_skipped: requested_cells > 0 && remaining_cells == 0,
        fingerprint_refusals,
        cells,
    }
}

fn ledger_rows_for_cell<'a>(
    rows: &'a [EvalLedgerRow],
    suite: &str,
    model: &str,
    split: Option<&str>,
    commit: &str,
    case_name: &str,
    trial: usize,
) -> Vec<&'a EvalLedgerRow> {
    rows.iter()
        .filter(|row| {
            row.suite == suite
                && row.model == model
                && row.split.as_deref() == split
                && row.commit == commit
                && row.case_name == case_name
                && row.trial == trial
        })
        .collect()
}

fn eval_ledger_log_error(error: crate::event_log::LogError) -> VmError {
    VmError::Runtime(format!("eval ledger: event log: {error}"))
}

impl EvalPackLedgerRun {
    pub(super) fn start(
        manifest: &EvalPackManifest,
        base_dir: Option<&Path>,
        options: Option<serde_json::Value>,
    ) -> Result<Self, VmError> {
        let options = eval_ledger_options(options)?;
        let suite = options.suite.clone().unwrap_or_else(|| manifest.id.clone());
        let model = options
            .model
            .clone()
            .or_else(|| eval_pack_manifest_model(manifest))
            .unwrap_or_else(|| "unknown".to_string());
        let provenance = eval_ledger_provenance(base_dir, &options, Some(&manifest.metadata));
        let commit = options
            .commit
            .clone()
            .unwrap_or_else(|| provenance.commit.clone());
        let namespace = eval_pack_ledger_namespace(manifest, &options);
        let topic = eval_ledger_topic(&namespace)?;
        let log = ensure_eval_ledger_event_log(base_dir);
        let read_options = eval_pack_ledger_read_options(&suite, &model, &commit);
        let rows = futures::executor::block_on(read_eval_ledger_rows(&log, &topic, &read_options))?;
        Ok(Self {
            log,
            topic,
            rows,
            suite,
            model,
            commit,
            branch: provenance.branch.clone(),
            provenance,
            inserted: 0,
            duplicates: 0,
            fingerprint_refusals: Vec::new(),
        })
    }

    pub(super) fn replay_row_for_cell(
        &mut self,
        case_id: &str,
        split: Option<&str>,
        trial: usize,
        case_fingerprint: &str,
        harness_config_fingerprint: &str,
    ) -> Option<EvalLedgerRow> {
        let matching = ledger_rows_for_cell(
            &self.rows,
            &self.suite,
            &self.model,
            split,
            &self.commit,
            case_id,
            trial,
        );
        let exact = matching
            .iter()
            .copied()
            .filter(|row| {
                row.case_fingerprint == case_fingerprint
                    && row.harness_config_fingerprint == harness_config_fingerprint
            })
            .max_by_key(|row| row.event_id.unwrap_or_default())
            .cloned();
        if exact.is_some() {
            return exact;
        }
        for row in matching {
            if row.case_fingerprint != case_fingerprint
                || row.harness_config_fingerprint != harness_config_fingerprint
            {
                self.fingerprint_refusals
                    .push(EvalLedgerFingerprintMismatch {
                        case_name: case_id.to_string(),
                        split: split.map(str::to_string),
                        commit: row.commit.clone(),
                        trial,
                        case_fingerprint: row.case_fingerprint.clone(),
                        harness_config_fingerprint: row.harness_config_fingerprint.clone(),
                        expected_case_fingerprint: case_fingerprint.to_string(),
                        expected_harness_config_fingerprint: harness_config_fingerprint.to_string(),
                    });
            }
        }
        None
    }

    pub(super) fn append_trial_row(&mut self, row: EvalLedgerRow) -> Result<(), VmError> {
        let report = futures::executor::block_on(append_eval_ledger_rows(
            &self.log,
            &self.topic,
            vec![row],
        ))?;
        self.inserted += report.inserted;
        self.duplicates += report.duplicates;
        self.rows.extend(report.rows);
        Ok(())
    }

    pub(super) fn finish(
        &self,
        requested_cells: usize,
        skipped_cells: usize,
        executed_cells: usize,
    ) -> Result<EvalPackRunState, VmError> {
        let remaining_cells = requested_cells.saturating_sub(skipped_cells + executed_cells);
        let mut state = EvalPackRunState {
            schema: EVAL_LEDGER_RUN_STATE_SCHEMA.to_string(),
            suite: self.suite.clone(),
            model: self.model.clone(),
            commit: self.commit.clone(),
            branch: self.branch.clone(),
            requested_cells,
            completed_cells: skipped_cells + executed_cells,
            skipped_cells,
            executed_cells,
            remaining_cells,
            ledger_rows_inserted: self.inserted,
            ledger_rows_duplicate: self.duplicates,
            fingerprint_refusals: self.fingerprint_refusals.len(),
            all_skipped: requested_cells > 0 && skipped_cells == requested_cells,
            heartbeat_event_id: None,
        };
        let event_id = self.append_run_state(&state)?;
        state.heartbeat_event_id = Some(event_id);
        Ok(state)
    }

    fn append_run_state(&self, state: &EvalPackRunState) -> Result<u64, VmError> {
        let payload = serde_json::to_value(state)
            .map_err(|e| VmError::Runtime(format!("eval run-state encode error: {e}")))?;
        let event_id = futures::executor::block_on(self.log.append(
            &self.topic,
            crate::event_log::LogEvent::new(EVAL_LEDGER_RUN_STATE_KIND, payload),
        ))
        .map_err(eval_ledger_log_error)?;
        futures::executor::block_on(self.log.flush()).map_err(eval_ledger_log_error)?;
        Ok(event_id)
    }
}
