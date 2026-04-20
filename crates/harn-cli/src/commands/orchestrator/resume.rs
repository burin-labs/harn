use serde::Serialize;

use crate::cli::OrchestratorResumeArgs;

use super::common::{load_local_runtime, print_json};

#[derive(Serialize)]
struct ResumeResult {
    request_id: String,
    reviewer: String,
    reason: Option<String>,
    accepted: bool,
}

pub(super) async fn run(args: OrchestratorResumeArgs) -> Result<(), String> {
    let _ctx = load_local_runtime(&args.local).await?;
    let response = harn_vm::HitlHostResponse {
        request_id: args.event_id.clone(),
        answer: None,
        approved: None,
        accepted: Some(true),
        reviewer: Some(args.reviewer.clone()),
        reason: args.reason.clone(),
        metadata: None,
        responded_at: None,
    };
    harn_vm::append_hitl_response(None, response).await?;

    let result = ResumeResult {
        request_id: args.event_id,
        reviewer: args.reviewer,
        reason: args.reason,
        accepted: true,
    };
    if args.json {
        return print_json(&result);
    }
    println!("Resumed escalation:");
    println!("- request_id={}", result.request_id);
    println!("- reviewer={}", result.reviewer);
    println!("- reason={}", result.reason.as_deref().unwrap_or("-"));
    Ok(())
}
