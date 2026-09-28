//! One allowance and one report for a complete provider probe run.
use super::{
    execute_live_probe_case, normalized_modes, report_from_cases, resolved_probe_model_id,
    ToolConformanceCase, ToolConformanceProbeOptions, ToolConformanceReport,
    ToolProbeEvidenceSource, ToolProbeFormatPolicy,
};
use crate::llm_config;

pub async fn run_tool_conformance_probe(
    options: ToolConformanceProbeOptions,
) -> ToolConformanceReport {
    let Some(ceiling) = options.max_cost_usd else {
        return run_inner(options).await;
    };
    let budget = match crate::llm::ConservativeLlmBudget::new(ceiling) {
        Ok(budget) => budget,
        Err(error) => return refused_report(&options, error),
    };
    match budget.scope(run_inner(options.clone())).await {
        Ok(report) => report,
        Err(error) => refused_report(&options, error),
    }
}

fn refused_report(
    options: &ToolConformanceProbeOptions,
    error: crate::value::VmError,
) -> ToolConformanceReport {
    report_from_cases(
        options.provider.clone(),
        options.model.clone(),
        options.base_url.clone(),
        if options.base_url.is_some() {
            ToolProbeEvidenceSource::LiveRawEndpoint
        } else {
            ToolProbeEvidenceSource::LiveRequest
        },
        options.tool_format,
        options.probe_case,
        options.marker.clone(),
        vec![ToolConformanceCase::transport_error(
            normalized_modes(&options.modes)[0],
            error.to_string(),
            Some(0),
        )],
    )
}

async fn run_inner(options: ToolConformanceProbeOptions) -> ToolConformanceReport {
    let model = llm_config::resolve_model_info(&options.model);
    let provider = if options.provider.trim().is_empty() {
        model.provider.clone()
    } else {
        options.provider.clone()
    };
    let model_id = resolved_probe_model_id(&model.id);
    let base_url = options.base_url.clone().or_else(|| {
        llm_config::provider_config(&provider).map(|def| llm_config::resolve_base_url(&def))
    });
    let mut cases = Vec::new();
    let modes = normalized_modes(&options.modes);
    let expected_value = options.probe_case.expected_value(&options.marker);
    for _ in 0..options.repeat.max(1) {
        for mode in &modes {
            cases.push(
                execute_live_probe_case(
                    &provider,
                    &model_id,
                    options.base_url.as_deref(),
                    *mode,
                    ToolProbeFormatPolicy {
                        format: options.tool_format,
                        strict: options.strict_tool_format,
                    },
                    options.probe_case,
                    &expected_value,
                    options.timeout_secs,
                )
                .await,
            );
        }
    }
    let mut report = report_from_cases(
        provider,
        model_id,
        base_url,
        if options.base_url.is_some() {
            ToolProbeEvidenceSource::LiveRawEndpoint
        } else {
            ToolProbeEvidenceSource::LiveRequest
        },
        options.tool_format,
        options.probe_case,
        options.marker,
        cases,
    );
    report.admission = crate::llm::admission::typed_receipt();
    report
}
