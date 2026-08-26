use super::*;

#[test]
fn summary_requires_every_repeated_native_case_to_pass() {
    let summary = summarize_cases(
        &[
            probe_case(
                ToolProbeMode::NonStreaming,
                true,
                ToolProbeClassification::StructuredNativeToolCall,
            ),
            probe_case(
                ToolProbeMode::NonStreaming,
                false,
                ToolProbeClassification::ProseOnlyNonTool,
            ),
        ],
        ToolProbeFormat::Native,
    );
    assert_eq!(summary.native, ToolProbeStatus::Fail);
    assert_eq!(summary.fallback_mode, ToolProbeFallbackMode::Disabled);
}

#[test]
fn summary_requires_every_repeated_text_case_to_pass() {
    let summary = summarize_cases(
        &[
            probe_case(
                ToolProbeMode::NonStreaming,
                true,
                ToolProbeClassification::ParseableHarnTextToolCall,
            ),
            probe_case(
                ToolProbeMode::NonStreaming,
                false,
                ToolProbeClassification::MalformedJsonArguments,
            ),
        ],
        ToolProbeFormat::Native,
    );
    assert_eq!(summary.native, ToolProbeStatus::Fail);
    assert_eq!(summary.text, ToolProbeStatus::Fail);
    assert_eq!(summary.fallback_mode, ToolProbeFallbackMode::Disabled);
}

#[test]
fn summary_preserves_nonstreaming_text_fallback_when_streaming_fails() {
    let summary = summarize_cases(
        &[
            probe_case(
                ToolProbeMode::NonStreaming,
                true,
                ToolProbeClassification::ParseableHarnTextToolCall,
            ),
            probe_case(
                ToolProbeMode::Streaming,
                false,
                ToolProbeClassification::ProseOnlyNonTool,
            ),
        ],
        ToolProbeFormat::Native,
    );
    assert_eq!(summary.native, ToolProbeStatus::Fail);
    assert_eq!(summary.streaming_native, ToolProbeStatus::Fail);
    assert_eq!(summary.text, ToolProbeStatus::Pass);
    assert_eq!(summary.fallback_mode, ToolProbeFallbackMode::Text);
}

#[test]
fn summary_keeps_untried_native_modes_unknown_for_a_text_probe() {
    let summary = summarize_cases(
        &[probe_case(
            ToolProbeMode::Streaming,
            true,
            ToolProbeClassification::ParseableHarnTextToolCall,
        )],
        ToolProbeFormat::Text,
    );

    assert_eq!(summary.native, ToolProbeStatus::Unknown);
    assert_eq!(summary.streaming_native, ToolProbeStatus::Unknown);
    assert_eq!(summary.text, ToolProbeStatus::Pass);
    assert_eq!(summary.fallback_mode, ToolProbeFallbackMode::Text);
}

fn probe_case(
    mode: ToolProbeMode,
    ok: bool,
    classification: ToolProbeClassification,
) -> ToolConformanceCase {
    let native_tool_call_count =
        usize::from(classification == ToolProbeClassification::StructuredNativeToolCall);
    let text_tool_call_count =
        usize::from(classification == ToolProbeClassification::ParseableHarnTextToolCall);
    ToolConformanceCase {
        mode,
        ok,
        classification,
        fallback_mode: ToolProbeFallbackMode::Disabled,
        failure_reason: None,
        http_status: None,
        elapsed_ms: None,
        native_tool_call_count,
        text_tool_call_count,
        usage: None,
        parser_errors: Vec::new(),
        protocol_violations: Vec::new(),
        content_sample: None,
    }
}
