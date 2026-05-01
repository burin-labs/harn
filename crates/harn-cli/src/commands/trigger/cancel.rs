use serde::Serialize;

use std::path::Path;
use std::sync::Arc;

use harn_vm::event_log::AnyEventLog;

use crate::cli::TriggerCancelArgs;
use crate::commands::trigger::ops::{
    append_bulk_cancel_requests, append_operation_audit, build_operation_audit,
    install_trigger_runtime, load_bulk_targets, load_targets_for_event_id,
    workspace_root_and_event_log, BulkTriggerTarget, ProgressReporter, RateLimiter,
};

#[derive(Clone, Debug, Serialize)]
pub struct CancelItem {
    pub event_id: String,
    pub binding_id: String,
    pub binding_version: u32,
    pub binding_key: String,
    pub latest_status: String,
    pub status: String,
    pub cancellable: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct CancelReport {
    pub operation: String,
    pub dry_run: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    pub matched_count: usize,
    pub requested_count: usize,
    pub skipped_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_id: Option<String>,
    pub items: Vec<CancelItem>,
}

pub(crate) async fn run(args: TriggerCancelArgs) -> Result<(), String> {
    let (workspace_root, event_log) = workspace_root_and_event_log()?;
    install_trigger_runtime(&workspace_root).await?;

    let (targets, normalized_filter) = match (args.event_id.as_deref(), args.where_expr.as_deref())
    {
        (Some(event_id), None) => (
            load_targets_for_event_id(&event_log, event_id, None).await?,
            None,
        ),
        (None, Some(where_expr)) => {
            let (targets, normalized_filter) =
                load_bulk_targets(&event_log, where_expr, None).await?;
            (targets, Some(normalized_filter))
        }
        _ => return Err("expected either an event id or --where".to_string()),
    };

    let report = cancel_targets(
        &event_log,
        targets,
        normalized_filter,
        args.dry_run,
        args.progress,
        args.rate_limit,
    )
    .await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report)
            .map_err(|error| format!("failed to encode cancel report: {error}"))?
    );
    Ok(())
}

/// In-process entry point for `harn trigger cancel <event_id>`. Used by
/// integration tests to drive the cancel path without spawning the `harn`
/// binary. The binary entry calls the same internals via `run`.
pub async fn cancel_event_in_process(
    event_log: Arc<AnyEventLog>,
    workspace_root: &Path,
    event_id: &str,
) -> Result<CancelReport, String> {
    install_trigger_runtime(workspace_root).await?;
    let targets = load_targets_for_event_id(&event_log, event_id, None).await?;
    cancel_targets(&event_log, targets, None, false, false, None).await
}

async fn cancel_targets(
    event_log: &std::sync::Arc<harn_vm::event_log::AnyEventLog>,
    targets: Vec<BulkTriggerTarget>,
    normalized_filter: Option<String>,
    dry_run: bool,
    progress: bool,
    rate_limit: Option<f64>,
) -> Result<CancelReport, String> {
    let matched_count = targets.len();
    let mut requested_count = 0;
    let mut skipped_count = 0;
    let mut items = Vec::new();
    let mut limiter = RateLimiter::new(rate_limit);
    let mut progress_reporter = ProgressReporter::new(progress, "cancel", matched_count);

    if !dry_run {
        let mut request_targets = Vec::new();
        for target in &targets {
            limiter.wait().await;
            let (status, should_request) = if target.cancellable {
                requested_count += 1;
                ("requested".to_string(), true)
            } else {
                skipped_count += 1;
                ("not_cancellable".to_string(), false)
            };
            if should_request {
                request_targets.push(target.clone());
            }
            progress_reporter.update(status.as_str());
            items.push(CancelItem {
                event_id: target.event_id.clone(),
                binding_id: target.binding_id.clone(),
                binding_version: target.binding_version,
                binding_key: target.binding_key.clone(),
                latest_status: target.latest_status.clone(),
                status,
                cancellable: target.cancellable,
            });
        }
        let audit = build_operation_audit(
            "cancel",
            false,
            normalized_filter.clone(),
            rate_limit,
            matched_count,
            requested_count,
            skipped_count,
            &targets,
        );
        append_bulk_cancel_requests(
            event_log,
            &audit.id,
            audit.requested_by.clone(),
            &request_targets,
        )
        .await?;
        append_operation_audit(event_log, &audit).await?;
        return Ok(CancelReport {
            operation: "cancel".to_string(),
            dry_run: false,
            filter: normalized_filter,
            matched_count,
            requested_count,
            skipped_count,
            audit_id: Some(audit.id),
            items,
        });
    }

    for target in &targets {
        let status = if target.cancellable {
            "dry_run"
        } else {
            skipped_count += 1;
            "not_cancellable"
        };
        progress_reporter.update(status);
        items.push(CancelItem {
            event_id: target.event_id.clone(),
            binding_id: target.binding_id.clone(),
            binding_version: target.binding_version,
            binding_key: target.binding_key.clone(),
            latest_status: target.latest_status.clone(),
            status: status.to_string(),
            cancellable: target.cancellable,
        });
    }

    let audit = build_operation_audit(
        "cancel",
        true,
        normalized_filter.clone(),
        rate_limit,
        matched_count,
        0,
        skipped_count,
        &targets,
    );
    append_operation_audit(event_log, &audit).await?;
    Ok(CancelReport {
        operation: "cancel".to_string(),
        dry_run: true,
        filter: normalized_filter,
        matched_count,
        requested_count: 0,
        skipped_count,
        audit_id: Some(audit.id),
        items,
    })
}
