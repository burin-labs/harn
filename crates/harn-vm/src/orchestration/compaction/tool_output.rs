use super::{is_failure_signal_line, snap_to_line_end, snap_to_line_start};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicrocompactedToolOutput {
    pub text: String,
    /// Bytes from the source output that are no longer represented.
    pub dropped_bytes: usize,
}

/// Microcompact a tool result and retain exact source-loss metadata.
pub fn microcompact_tool_output_result(output: &str, max_chars: usize) -> MicrocompactedToolOutput {
    if output.len() <= max_chars || max_chars < 200 {
        return MicrocompactedToolOutput {
            text: output.to_string(),
            dropped_bytes: 0,
        };
    }
    let mut offset = 0;
    let diagnostic_lines = output
        .split_inclusive('\n')
        .filter_map(|segment| {
            let start = offset;
            offset += segment.len();
            let line = segment.strip_suffix('\n').unwrap_or(segment);
            is_failure_signal_line(line).then_some((line, start..start + line.len()))
        })
        .take(32)
        .collect::<Vec<_>>();
    if !diagnostic_lines.is_empty() {
        let diagnostics = diagnostic_lines
            .iter()
            .map(|(line, _)| *line)
            .collect::<Vec<_>>()
            .join("\n");
        let budget = max_chars.saturating_sub(diagnostics.len() + 64);
        let keep = budget / 2;
        if keep >= 80 && output.len() > keep * 2 {
            let head = snap_to_line_end(output, keep);
            let tail = snap_to_line_start(output, output.len().saturating_sub(keep));
            let retained = retained_interval_bytes(
                output.len(),
                std::iter::once(0..head.len())
                    .chain(diagnostic_lines.iter().map(|(_, range)| range.clone()))
                    .chain(std::iter::once(output.len() - tail.len()..output.len())),
            );
            return MicrocompactedToolOutput {
                text: format!(
                    "{head}\n\n[diagnostic lines preserved]\n{diagnostics}\n\n[... output compacted ...]\n\n{tail}"
                ),
                dropped_bytes: output.len().saturating_sub(retained),
            };
        }
    }
    let keep = max_chars / 2;
    let head = snap_to_line_end(output, keep);
    let tail = snap_to_line_start(output, output.len().saturating_sub(keep));
    let snipped = output.len().saturating_sub(head.len() + tail.len());
    MicrocompactedToolOutput {
        text: format!("{head}\n\n[... {snipped} characters snipped ...]\n\n{tail}"),
        dropped_bytes: snipped,
    }
}

/// Microcompact a tool result: if it exceeds `max_chars`, keep the first and
/// last portions with a snip marker in between.
pub fn microcompact_tool_output(output: &str, max_chars: usize) -> String {
    microcompact_tool_output_result(output, max_chars).text
}

fn retained_interval_bytes(
    source_len: usize,
    intervals: impl IntoIterator<Item = std::ops::Range<usize>>,
) -> usize {
    let mut intervals = intervals
        .into_iter()
        .filter(|range| range.start < range.end)
        .collect::<Vec<_>>();
    intervals.sort_by_key(|range| range.start);
    let mut retained = 0;
    let mut covered_end = 0;
    for range in intervals {
        let start = range.start.min(source_len).max(covered_end);
        let end = range.end.min(source_len);
        if start < end {
            retained += end - start;
            covered_end = end;
        }
    }
    retained
}
