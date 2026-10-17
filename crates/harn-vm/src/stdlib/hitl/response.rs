//! Host response delivery and settlement on the request's owning event log.

use super::*;
use crate::stdlib::waitpoint::{cancel_waitpoint_on, complete_waitpoint_on};

pub async fn append_hitl_response(
    base_dir: Option<&Path>,
    response: HitlHostResponse,
) -> Result<u64, String> {
    let log = ensure_hitl_event_log_for(base_dir)?;
    append_hitl_response_on(&log, response).await
}

/// Answer a waitpoint on its owning log, independent of ambient execution state.
pub async fn append_hitl_response_on(
    log: &Arc<AnyEventLog>,
    mut response: HitlHostResponse,
) -> Result<u64, String> {
    let kind = HitlRequestKind::from_request_id(&response.request_id)
        .ok_or_else(|| format!("unknown HITL request id '{}'", response.request_id))?;
    if response.responded_at.is_none() {
        response.responded_at = Some(now_rfc3339());
    }
    let headers = response_headers(&response.request_id);
    let topic = Topic::new(kind.topic()).map_err(|error| error.to_string())?;
    let event_id = log
        .append(
            &topic,
            LogEvent::new(
                match kind {
                    HitlRequestKind::Escalation => "hitl.escalation_accepted",
                    _ => "hitl.response_received",
                },
                serde_json::to_value(&response).map_err(|error| error.to_string())?,
            )
            .with_headers(headers),
        )
        .await
        .map_err(|error| error.to_string())?;
    finalize_hitl_response(log, kind, &response).await?;
    Ok(event_id)
}

async fn finalize_hitl_response(
    log: &Arc<AnyEventLog>,
    kind: HitlRequestKind,
    response: &HitlHostResponse,
) -> Result<(), String> {
    match kind {
        HitlRequestKind::Question => {
            if waitpoint_is_terminal(log, &response.request_id).await? {
                return Ok(());
            }
            complete_waitpoint_on(
                log,
                &response.request_id,
                response.answer.clone(),
                response.reviewer.clone(),
                response.reason.clone(),
                response.metadata.clone(),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
        }
        HitlRequestKind::Escalation => {
            if !response.accepted.unwrap_or(false)
                || waitpoint_is_terminal(log, &response.request_id).await?
            {
                return Ok(());
            }
            complete_waitpoint_on(
                log,
                &response.request_id,
                Some(json!({
                    "accepted": true,
                    "reviewer": response.reviewer,
                    "reason": response.reason,
                    "responded_at": response.responded_at,
                })),
                response.reviewer.clone(),
                response.reason.clone(),
                response.metadata.clone(),
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
        }
        HitlRequestKind::Approval | HitlRequestKind::DualControl => {
            if waitpoint_is_terminal(log, &response.request_id).await? {
                return Ok(());
            }
            let request = load_request_envelope(log, kind, &response.request_id)
                .await
                .map_err(|error| error.to_string())?;
            match resolve_approval_state(log, kind, &request)
                .await
                .map_err(|error| error.to_string())?
            {
                ApprovalResolution::Pending => Ok(()),
                ApprovalResolution::Approved(progress) => {
                    let record = approval_record_json(&progress);
                    append_named_event(
                        log,
                        kind,
                        approved_event_kind(kind),
                        &request.request_id,
                        &request.trace_id,
                        json!({
                            "request_id": request.request_id.clone(),
                            "record": record.clone(),
                        }),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                    complete_waitpoint_on(
                        log,
                        &request.request_id,
                        Some(record),
                        response.reviewer.clone(),
                        progress.reason.clone(),
                        response.metadata.clone(),
                    )
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
                }
                ApprovalResolution::Denied(denied) => {
                    append_named_event(
                        log,
                        kind,
                        denied_event_kind(kind),
                        &request.request_id,
                        &request.trace_id,
                        json!({
                            "request_id": request.request_id.clone(),
                            "reviewer": denied.reviewer.clone(),
                            "reason": denied.reason.clone(),
                        }),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                    cancel_waitpoint_on(
                        log,
                        &request.request_id,
                        denied.reviewer.clone(),
                        denied.reason.clone(),
                        denied.metadata.clone(),
                    )
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
                }
            }
        }
    }
}

async fn waitpoint_is_terminal(log: &Arc<AnyEventLog>, request_id: &str) -> Result<bool, String> {
    Ok(inspect_waitpoint_on(log, request_id)
        .await
        .map_err(|error| error.to_string())?
        .is_some_and(|record| record.status != WaitpointStatus::Open))
}
