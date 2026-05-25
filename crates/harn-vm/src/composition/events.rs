use crate::agent_events::AgentEvent;

use super::types::CompositionExecutionReport;

pub fn composition_report_events(
    session_id: impl Into<String>,
    report: &CompositionExecutionReport,
) -> Vec<AgentEvent> {
    let session_id = session_id.into();
    let mut start_run = report.run.clone();
    start_run.stdout = None;
    start_run.stderr = None;
    start_run.artifacts = Vec::new();
    start_run.result = None;
    start_run.failure_category = None;
    start_run.error = None;
    start_run.duration_ms = None;

    let mut events = vec![AgentEvent::CompositionStart {
        session_id: session_id.clone(),
        run: start_run,
    }];
    for call in &report.child_calls {
        events.push(AgentEvent::CompositionChildCall {
            session_id: session_id.clone(),
            call: call.clone(),
        });
        for result in report
            .child_results
            .iter()
            .filter(|result| result.tool_call_id == call.tool_call_id)
        {
            events.push(AgentEvent::CompositionChildResult {
                session_id: session_id.clone(),
                result: result.clone(),
            });
        }
    }
    if report.ok {
        events.push(AgentEvent::CompositionFinish {
            session_id,
            run: report.run.clone(),
        });
    } else {
        events.push(AgentEvent::CompositionError {
            session_id,
            run: report.run.clone(),
        });
    }
    events
}

pub(super) fn emit_composition_report_events(
    session_id: &str,
    report: &CompositionExecutionReport,
) {
    for event in composition_report_events(session_id, report) {
        crate::llm::emit_live_agent_event_sync(&event);
    }
}
