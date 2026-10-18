//! Deterministic transcript reduction, source accounting, and compaction policy.

mod policy;
mod tool_output;
pub use policy::{
    compaction_policy_metadata_fields, compaction_policy_option_keys,
    compaction_policy_to_vm_value, parse_compaction_policy_options, CompactionPolicy,
    CompactionRequest,
};
pub use tool_output::{
    microcompact_tool_output, microcompact_tool_output_result, MicrocompactedToolOutput,
};

/// Default byte budget for an observation-mask recap body (about 4k tokens).
pub const DEFAULT_RECAP_BUDGET_BYTES: usize = 16_000;

/// Observation-mask recap metrics, carried verbatim (typed) inside
/// native compaction receipts so recap behavior survives every projection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RecapMetrics {
    pub recap_bytes: usize,
    pub budget_bytes: usize,
    pub kept_results_count: usize,
    pub dropped_count: usize,
    pub carried_prior_recap: bool,
}

impl RecapMetrics {
    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "recap_bytes": self.recap_bytes,
            "budget_bytes": self.budget_bytes,
            "kept_results_count": self.kept_results_count,
            "dropped_count": self.dropped_count,
            "carried_prior_recap": self.carried_prior_recap,
        })
    }
}

/// Estimate token count from a list of JSON messages (chars / 4 heuristic).
pub fn estimate_message_tokens(messages: &[serde_json::Value]) -> usize {
    messages.iter().map(estimate_message_chars).sum::<usize>() / 4
}

fn estimate_message_chars(message: &serde_json::Value) -> usize {
    let mut total = message
        .get("content")
        .map(estimate_content_chars)
        .unwrap_or_default();
    if let Some(reasoning) = message.get("reasoning") {
        total += estimate_content_chars(reasoning);
    }
    if let Some(tool_calls) = message.get("tool_calls") {
        total += estimate_content_chars(tool_calls);
    }
    total
}

fn estimate_content_chars(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::String(text) => text.len(),
        serde_json::Value::Array(items) => items.iter().map(estimate_content_chars).sum(),
        serde_json::Value::Object(map) => map.values().map(estimate_content_chars).sum(),
        serde_json::Value::Null => 0,
        other => other.to_string().len(),
    }
}

fn is_reasoning_or_tool_turn_message(message: &serde_json::Value) -> bool {
    let role = message
        .get("role")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    role == "tool"
        || message.get("tool_calls").is_some()
        || message
            .get("reasoning")
            .map(|value| !value.is_null())
            .unwrap_or(false)
}

fn find_prev_user_boundary(messages: &[serde_json::Value], start: usize) -> Option<usize> {
    (0..=start)
        .rev()
        .find(|idx| messages[*idx].get("role").and_then(|value| value.as_str()) == Some("user"))
}

/// True when the message carries a tool result: the OpenAI durable shape
/// (`role: "tool"`), the Anthropic durable shape (`role: "tool_result"`), or
/// a user message whose content blocks include a `tool_result`. Text-channel
/// results are ordinary user strings and intentionally don't match — they
/// have no provider-level pairing to protect.
fn is_tool_result_message(message: &serde_json::Value) -> bool {
    match message.get("role").and_then(|role| role.as_str()) {
        Some("tool") | Some("tool_result") => true,
        Some("user") => message
            .get("content")
            .and_then(|content| content.as_array())
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    block.get("type").and_then(|value| value.as_str()) == Some("tool_result")
                })
            }),
        _ => false,
    }
}

/// A compaction split must never land between an assistant tool-use message
/// and its tool_result message(s): a kept window that begins with a
/// tool_result whose request was drained is rejected by providers as an
/// orphaned result. Results always immediately follow their request, so a
/// split index is unsafe exactly when it points AT a tool-result message.
/// Walk backward to the message that initiated the result run (keeping the
/// request together with its results); when that would consume the whole
/// compactable window, walk forward past the run instead so compaction still
/// makes progress.
fn snap_split_off_tool_results(
    messages: &[serde_json::Value],
    split_at: usize,
    compact_start: usize,
) -> usize {
    if split_at >= messages.len() || !is_tool_result_message(&messages[split_at]) {
        return split_at;
    }
    let mut backward = split_at;
    while backward > compact_start && is_tool_result_message(&messages[backward]) {
        backward -= 1;
    }
    if backward > compact_start {
        return backward;
    }
    let mut forward = split_at;
    while forward < messages.len() && is_tool_result_message(&messages[forward]) {
        forward += 1;
    }
    forward
}

/// Select a reducible window while keeping tool calls with their results and
/// preserving the requested leading and recent context.
pub fn select_compaction_window(
    messages: &[serde_json::Value],
    keep_first: usize,
    keep_last: usize,
) -> Option<std::ops::Range<usize>> {
    if messages.len() <= keep_first.saturating_add(keep_last) {
        return None;
    }
    let compact_start = keep_first.min(messages.len());
    let original_split = messages.len().saturating_sub(keep_last);
    let mut split_at = original_split;
    // The kept suffix starts at a clean turn whenever the window permits it.
    while split_at > compact_start
        && split_at < messages.len()
        && messages[split_at]
            .get("role")
            .and_then(|r| r.as_str())
            .is_none_or(|r| r != "user")
    {
        split_at -= 1;
    }
    // Tool-heavy transcripts may have only one user message, at index zero.
    if split_at == compact_start {
        split_at = original_split;
    }
    if let Some(volatile_start) = messages[split_at..]
        .iter()
        .position(is_reasoning_or_tool_turn_message)
        .map(|offset| split_at + offset)
    {
        if let Some(boundary) = volatile_start
            .checked_sub(1)
            .and_then(|idx| find_prev_user_boundary(messages, idx))
            .filter(|boundary| *boundary > compact_start)
        {
            split_at = boundary;
        }
    }
    split_at = snap_split_off_tool_results(messages, split_at, compact_start);
    (split_at > compact_start).then_some(compact_start..split_at)
}

/// True when `trimmed` (an already-trimmed line) begins with `file:line` —
/// a colon immediately followed by a digit, with no whitespace before the
/// colon. Used as a strong signal that a line carries a located diagnostic.
fn line_has_file_line_prefix(trimmed: &str) -> bool {
    let bytes = trimmed.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i] != b':' {
        i += 1;
    }
    i < bytes.len() && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()
}

/// True when a single line carries failure signal worth preserving verbatim
/// when we shrink a large tool output. This is the ONE filter shared by both
/// the microcompact path (`microcompact_tool_output`) and the observation-mask
/// path (`default_mask_tool_result`). Keep these paths on one filter so
/// assertion values, rustc continuation lines, and structured failing-line
/// markers survive in the detail the model re-reads to fix the bug.
///
///   - assertion value lines with no `file:line` (`left:`, `right:`,
///     `expected:`, `actual:`, `got`, `want`) — the values the model needs to
///     see the actual-vs-expected mismatch.
///   - rustc continuation lines (`-->` location pointers, `= help:`/`= note:`,
///     numbered source rows like `12 |`, and `^^^` carets).
///   - `Lnnn:` failing-line structure some test harnesses emit.
pub(super) fn is_failure_signal_line(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }
    let lower = trimmed.to_lowercase();

    let has_file_line = line_has_file_line_prefix(trimmed);
    let has_strong_keyword =
        trimmed.contains("FAIL") || trimmed.contains("panic") || trimmed.contains("Panic");
    let has_weak_keyword = trimmed.contains("error")
        || trimmed.contains("undefined")
        || trimmed.contains("expected")
        || trimmed.contains("got")
        || lower.contains("cannot find")
        || lower.contains("not found")
        || lower.contains("no such")
        || lower.contains("unresolved")
        || lower.contains("missing")
        || lower.contains("declared but not used")
        || lower.contains("unused")
        || lower.contains("mismatch");
    let positional = lower.contains(" error ")
        || lower.starts_with("error:")
        || lower.starts_with("warning:")
        || lower.starts_with("note:")
        || lower.contains("panic:");

    let assertion_value = lower.starts_with("left:")
        || lower.starts_with("right:")
        || lower.starts_with("expected:")
        || lower.starts_with("actual:")
        || lower.starts_with("got:")
        || lower.starts_with("want:")
        || lower.starts_with("got ")
        || lower.starts_with("want ")
        || lower.starts_with("assertion")
        || lower.contains("assertionerror");

    // rustc continuation lines: location pointer, help/note, numbered source
    // rows (`12 | ...`), and caret underlines (`^^^`).
    let rustc_continuation = trimmed.starts_with("-->")
        || trimmed.starts_with("= help:")
        || trimmed.starts_with("= note:")
        || trimmed.contains('^')
        || {
            // `<digits> |` numbered source row from rustc's snippet rendering.
            let mut chars = trimmed.chars();
            let mut saw_digit = false;
            let mut rest = trimmed;
            while let Some(c) = chars.clone().next() {
                if c.is_ascii_digit() {
                    saw_digit = true;
                    chars.next();
                    rest = chars.as_str();
                } else {
                    break;
                }
            }
            saw_digit && rest.trim_start().starts_with('|')
        };

    #[expect(
        clippy::string_slice,
        reason = "digits is an ASCII-digit prefix of rest, so its len is a boundary"
    )]
    let failing_line_marker = {
        if let Some(rest) = trimmed.strip_prefix('L') {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            !digits.is_empty() && rest[digits.len()..].starts_with(':')
        } else {
            false
        }
    };

    has_strong_keyword
        || (has_file_line && has_weak_keyword)
        || positional
        || assertion_value
        || rustc_continuation
        || failing_line_marker
}

/// Snap a byte offset to the nearest preceding line boundary (end of a complete line).
/// Returns the substring from the start up to and including the last complete line
/// that fits within `max_bytes`. Never cuts mid-line.
#[expect(
    clippy::string_slice,
    reason = "search_end is a floor_char_boundary; pos indexes an ASCII newline"
)]
fn snap_to_line_end(s: &str, max_bytes: usize) -> &str {
    if max_bytes >= s.len() {
        return s;
    }
    let search_end = s.floor_char_boundary(max_bytes);
    match s[..search_end].rfind('\n') {
        Some(pos) => &s[..pos + 1],
        None => &s[..search_end], // single long line — fall back to char boundary
    }
}

/// Snap a byte offset to the nearest following line boundary (start of a complete line).
/// Returns the substring from the first complete line at or after `start_byte`.
/// Never cuts mid-line.
#[expect(
    clippy::string_slice,
    reason = "search_start is a ceil_char_boundary; line_start follows an ASCII newline"
)]
fn snap_to_line_start(s: &str, start_byte: usize) -> &str {
    if start_byte == 0 {
        return s;
    }
    let search_start = s.ceil_char_boundary(start_byte);
    if search_start >= s.len() {
        return "";
    }
    match s[search_start..].find('\n') {
        Some(pos) => {
            let line_start = search_start + pos + 1;
            if line_start < s.len() {
                &s[line_start..]
            } else {
                &s[search_start..]
            }
        }
        None => &s[search_start..], // already at start of last line
    }
}

/// A compaction summary plus the size of the scaffold the summarizer generated
/// around it (headers, budget notices, drop markers).
///
/// `text.len() - scaffold_bytes` is `carried_source_bytes`: how many bytes of
/// the archived source window actually survived into the summary. A summary can
/// be hundreds of bytes long and still carry nothing — a bare
/// `[auto-compacted N older messages]` header is the shape that silently
/// destroys context — so the byte length of the summary is not a usable
/// measurement on its own.
#[derive(Debug)]
pub struct CompactionSummary {
    pub text: String,
    pub scaffold_bytes: usize,
}

impl CompactionSummary {
    pub fn new(text: String, scaffold_bytes: usize) -> Self {
        let scaffold_bytes = scaffold_bytes.min(text.len());
        Self {
            text,
            scaffold_bytes,
        }
    }

    pub fn carried_source_bytes(&self) -> usize {
        self.text.len().saturating_sub(self.scaffold_bytes)
    }
}

/// Typed measurement of one compaction's source window against the summary that
/// replaced it.
///
/// Every field is `Option` on purpose: `None` means the measurement was not
/// taken on this path, `Some(0)` means it was taken and read zero. Collapsing
/// the two would make an unmeasured compaction indistinguishable from one that
/// provably carried nothing forward.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct CompactionSourceMeasurement {
    /// Messages drained from the live transcript.
    pub source_message_count: Option<usize>,
    /// Plain-text content bytes in those messages, flattened across the string
    /// and block-array `content` shapes.
    pub source_bytes: Option<usize>,
    /// Bytes of the summary that replaced them.
    pub summary_bytes: Option<usize>,
    /// Bytes of that summary that came from the source window rather than from
    /// generated scaffold.
    pub carried_source_bytes: Option<usize>,
}

impl CompactionSourceMeasurement {
    /// A non-empty source window whose summary carried nothing forward is a
    /// failed compaction, never a successful one.
    pub fn discarded_source_context(&self) -> bool {
        matches!(
            (self.source_bytes, self.carried_source_bytes),
            (Some(source), Some(0)) if source > 0
        )
    }
}

/// Plain-text content bytes of one message, across the string `content` shape
/// and the block-array shape real agent transcripts carry.
pub fn message_content_bytes(message: &serde_json::Value) -> usize {
    message
        .get("content")
        .map(|content| super::repair_ledger::value_text(content).len())
        .unwrap_or(0)
}

pub fn truncate_compaction_summary(
    old_messages: &[serde_json::Value],
    archived_count: usize,
) -> CompactionSummary {
    truncate_compaction_summary_with_context(old_messages, archived_count, false)
}

pub fn truncate_compaction_summary_with_context(
    old_messages: &[serde_json::Value],
    archived_count: usize,
    is_llm_fallback: bool,
) -> CompactionSummary {
    let per_msg_limit = 500_usize;
    // Flatten `content` through `value_text` rather than `as_str`: a real agent
    // transcript carries tool results and multi-part turns as a block array, and
    // reading only the string shape drops every one of them — leaving a bare
    // header as the "summary" of a window that is being deleted.
    let mut carried = 0_usize;
    let summary_parts: Vec<String> = old_messages
        .iter()
        .filter_map(|m| {
            let role = m.get("role")?.as_str()?;
            let content = super::repair_ledger::value_text(m.get("content")?);
            if content.is_empty() {
                return None;
            }
            #[expect(
                clippy::string_slice,
                reason = "floor_char_boundary returns a char boundary"
            )]
            let truncated = if content.len() > per_msg_limit {
                format!(
                    "{}... [truncated from {} chars]",
                    &content[..content.floor_char_boundary(per_msg_limit)],
                    content.len()
                )
            } else {
                content
            };
            carried += truncated.len();
            Some(format!("[{role}] {truncated}"))
        })
        .take(15)
        .collect();
    let header = if is_llm_fallback {
        format!(
            "[auto-compact fallback: LLM summarizer returned empty; {archived_count} older messages abbreviated to ~{per_msg_limit} chars each]"
        )
    } else {
        format!("[auto-compacted {archived_count} older messages via truncate strategy]")
    };
    let text = format!(
        "{header}\n{}{}",
        summary_parts.join("\n"),
        if archived_count > 15 {
            format!("\n... and {} more", archived_count - 15)
        } else {
            String::new()
        }
    );
    // `take(15)` can drop parts that `carried` already counted; clamp so the
    // scaffold size can never be reported as negative work.
    let carried = carried.min(text.len());
    let scaffold_bytes = text.len() - carried;
    CompactionSummary::new(text, scaffold_bytes)
}

/// Marker the host emits inside a tool-output (or message) body to pin its
/// live grounding — the current file view and just-edited window — so it
/// survives a compaction pass. The host renders this literal substring inside
/// markdown headings (e.g. `## Exact current file text [no-compact]`,
/// `## Edited region now reads (...) [no-compact]`) in
/// `lib/tools/result-format.harn`. Compaction matches the substring; it does
/// not invent a new vocabulary.
pub const NO_COMPACT_MARKER: &str = "[no-compact]";

/// Upper bound on how many of the most-recent pinned segments survive a
/// compaction pass verbatim. A pin that could never be evicted would let a
/// long session accumulate unbounded pinned snapshots (e.g. one edited-window
/// per edit) and eventually overflow the context window — defeating the
/// purpose of compaction. Keeping only the latest few preserves the agent's
/// *current* grounding (the file it is editing now, emitted as the exact-text
/// block plus the numbered-lines block in one or two adjacent outputs) while
/// letting stale duplicates from earlier in the session compact normally.
pub const MAX_PINNED_SEGMENTS: usize = 3;

/// Whether a content body carries the host's `[no-compact]` pin marker.
fn is_pinned_content(content: &str) -> bool {
    content.contains(NO_COMPACT_MARKER)
}

/// Compute the set of message indices into `messages` that are pinned AND fall
/// within the most-recent [`MAX_PINNED_SEGMENTS`] pinned bodies. Older pinned
/// bodies are intentionally excluded so they compact normally (the bound).
/// `content_of` extracts the body text to inspect for each message.
pub fn latest_pinned_indices<'a, F>(
    messages: impl Iterator<Item = &'a serde_json::Value>,
    content_of: F,
) -> std::collections::HashSet<usize>
where
    F: Fn(&serde_json::Value) -> Option<&str>,
{
    // Walk newest-first, collecting up to MAX_PINNED_SEGMENTS pinned indices.
    let pinned: Vec<usize> = messages
        .enumerate()
        .filter(|(_, msg)| content_of(msg).is_some_and(is_pinned_content))
        .map(|(idx, _)| idx)
        .collect();
    pinned.into_iter().rev().take(MAX_PINNED_SEGMENTS).collect()
}

/// Check whether a tool-result string should be preserved verbatim during
/// observation masking. Uses content length as the primary heuristic:
/// short results (< 500 chars) are kept since they're typically error messages,
/// status lines, or concise answers that are cheap to retain and risky to mask.
/// Long results are masked to save context budget.
fn content_should_preserve(content: &str) -> bool {
    content.len() < 500
}

/// Default per-message masking for tool results.
///
/// Beyond the first-line preview, this KEEPS any failure-signal lines
/// (assertion values, located diagnostics, rustc help/caret/source rows,
/// `Lnnn:` markers) via the shared [`is_failure_signal_line`] filter, so the
/// model re-reads the actual-vs-expected detail it needs to fix the bug. The
/// previous mask dropped everything but the first line, silently shredding the
/// structured failure that the strong microcompact filter preserves at the
/// emission side.
fn default_mask_tool_result(role: &str, content: &str) -> String {
    let first_line = content.lines().next().unwrap_or(content);
    let line_count = content.lines().count();
    let char_count = content.len();
    if line_count <= 3 {
        return format!("[{role}] {content}");
    }
    #[expect(
        clippy::string_slice,
        reason = "floor_char_boundary returns a char boundary"
    )]
    let preview = &first_line[..first_line.floor_char_boundary(120)];
    // Preserve failure-signal lines (bounded so a huge log can't defeat the
    // mask). Skip the first line itself — it is already in the preview.
    let kept: Vec<&str> = content
        .lines()
        .skip(1)
        .filter(|line| is_failure_signal_line(line))
        .take(32)
        .collect();
    if kept.is_empty() {
        format!("[{role}] {preview}... [{line_count} lines, {char_count} chars masked]")
    } else {
        format!(
            "[{role}] {preview}... [{line_count} lines, {char_count} chars masked; \
             failure lines preserved]\n{}",
            kept.join("\n")
        )
    }
}

/// Stable marker on the first line of every observation-mask recap. A prior
/// recap re-enters the archive window on a later compaction as an ordinary
/// `{role: "user"}` message; matching this sentinel lets the next compaction
/// carry it forward once (bounded) instead of re-expanding and re-masking it —
/// the "recap of a recap = the same recap, updated" contract that stops recaps
/// from compounding across compactions.
pub const RECAP_HEADER_SENTINEL: &str = "via observation masking]";
pub const CLASSIFIED_RECAP_HEADER_SENTINEL: &str = "via classified compaction]";

/// Byte cap on a single carried-forward prior recap. Bounds the compound-growth
/// term independently of the per-message budget so a chain of compactions
/// converges instead of accreting.
const PRIOR_RECAP_CARRY_CAP: usize = 6_000;

/// Byte cap on the preview kept for an archived assistant turn. Assistant turns
/// carry the *calls* the model issued, not what it *learned*; keeping only a
/// short preview spends the recap budget on tool results (the observations)
/// rather than replaying verbatim call syntax.
const ASSISTANT_PREVIEW_CHARS: usize = 240;

/// Whether a message body is a previous observation-mask recap.
pub fn is_prior_recap(content: &str) -> bool {
    content.contains(RECAP_HEADER_SENTINEL) || content.contains(CLASSIFIED_RECAP_HEADER_SENTINEL)
}

/// Render one archived assistant turn as a bounded preview. Short turns pass
/// through; long turns keep a head slice plus a masked-marker tail so the drop
/// is visible in the same vocabulary as [`default_mask_tool_result`].
fn assistant_preview(content: &str) -> String {
    if content.len() <= ASSISTANT_PREVIEW_CHARS {
        return format!("[assistant] {content}");
    }
    let head = snap_to_line_end(content, ASSISTANT_PREVIEW_CHARS);
    let dropped = content.len().saturating_sub(head.len());
    format!("[assistant] {head}... [assistant turn truncated, {dropped} chars masked]")
}

/// Collapse runs of byte-identical consecutive lines into a single line with an
/// `(xN)` suffix. Turns repetitive call/read sequences ("looked at X 4 times")
/// into a count instead of N verbatim copies.
fn collapse_repeats(lines: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut run = 0usize;
    for line in lines {
        if out
            .last()
            .is_some_and(|prev| strip_repeat_suffix(prev) == line)
        {
            run += 1;
            let base =
                strip_repeat_suffix(out.last().expect("run implies a last line")).to_string();
            *out.last_mut().expect("run implies a last line") = format!("{base} (x{})", run + 1);
        } else {
            run = 0;
            out.push(line);
        }
    }
    out
}

#[expect(
    clippy::string_slice,
    reason = "idx is an rfind offset on the same line"
)]
fn strip_repeat_suffix(line: &str) -> &str {
    line.rfind(" (x")
        .filter(|_| line.ends_with(')'))
        .map(|idx| &line[..idx])
        .unwrap_or(line)
}

/// Deterministic observation-mask compaction.
#[cfg(any(test, feature = "test-support"))]
pub fn observation_mask_compaction(
    old_messages: &[serde_json::Value],
    archived_count: usize,
) -> String {
    observation_mask_compaction_with_callback(
        old_messages,
        archived_count,
        None,
        DEFAULT_RECAP_BUDGET_BYTES,
    )
    .0
    .text
}

/// Test-only accessor exposing the recap body together with its
/// [`RecapMetrics`] and taking an explicit byte budget.
#[cfg(any(test, feature = "test-support"))]
pub fn observation_mask_compaction_for_test(
    old_messages: &[serde_json::Value],
    archived_count: usize,
    budget_bytes: usize,
) -> (String, RecapMetrics) {
    let (summary, metrics) =
        observation_mask_compaction_with_callback(old_messages, archived_count, None, budget_bytes);
    (summary.text, metrics)
}

/// Build the observation-mask recap body under `budget_bytes`.
///
/// Contract (harn#4731):
///   - A single header line names how many messages were archived.
///   - The most-recent *prior* recap in the archive window is carried forward
///     once, bounded to [`PRIOR_RECAP_CARRY_CAP`], and never re-expanded, so
///     recaps do not compound across successive compactions.
///   - The remaining budget is spent NEWEST-first on load-bearing observations
///     (tool results — verify/test/build outcomes and the errors that preceded
///     edits, preserved by [`default_mask_tool_result`]) rather than on verbatim
///     assistant call replay, which is reduced to a short preview.
///   - Pinned `[no-compact]` grounding is always kept (already bounded to
///     [`MAX_PINNED_SEGMENTS`]).
///   - Overflow past the budget is dropped and summarized in one trailing
///     masked-marker line; repetitive identical lines collapse to `(xN)`.
pub fn observation_mask_compaction_with_callback(
    old_messages: &[serde_json::Value],
    archived_count: usize,
    mask_results: Option<&[Option<String>]>,
    budget_bytes: usize,
) -> (CompactionSummary, RecapMetrics) {
    let header =
        format!("[auto-compacted {archived_count} older messages via observation masking]");
    let pinned = latest_pinned_indices(old_messages.iter(), |msg| {
        msg.get("content").and_then(|v| v.as_str())
    });
    // Carry forward only the single most-recent prior recap; older recaps in the
    // window are already folded into it and compact as ordinary text.
    let prior_recap_idx = old_messages
        .iter()
        .enumerate()
        .rev()
        .find(|(_, msg)| {
            msg.get("content")
                .and_then(|v| v.as_str())
                .is_some_and(is_prior_recap)
        })
        .map(|(idx, _)| idx);

    let mut metrics = RecapMetrics {
        budget_bytes,
        ..RecapMetrics::default()
    };
    // Render newest-first so the budget lands on the most decision-relevant tail;
    // reverse to chronological order for output.
    let mut rendered_rev: Vec<String> = Vec::new();
    let mut used = header.len();

    for (idx, msg) in old_messages.iter().enumerate().rev() {
        let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
        let content = msg
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if content.is_empty() {
            continue;
        }

        if Some(idx) == prior_recap_idx {
            let carried = snap_to_line_end(content, PRIOR_RECAP_CARRY_CAP);
            rendered_rev.push(format!("[prior recap] {carried}"));
            metrics.carried_prior_recap = true;
            continue;
        }

        // Pinned grounding is unconditional (and already bounded).
        if pinned.contains(&idx) {
            rendered_rev.push(format!("[{role}] {content}"));
            continue;
        }

        let (line, is_result) = if role == "assistant" {
            (assistant_preview(content), false)
        } else if content_should_preserve(content) {
            (format!("[{role}] {content}"), true)
        } else if let Some(Some(custom)) = mask_results.and_then(|r| r.get(idx)) {
            (custom.clone(), true)
        } else {
            (default_mask_tool_result(role, content), true)
        };

        if used + line.len() + 1 > budget_bytes {
            metrics.dropped_count += 1;
            continue;
        }
        used += line.len() + 1;
        if is_result {
            metrics.kept_results_count += 1;
        }
        rendered_rev.push(line);
    }

    let mut body: Vec<String> = rendered_rev.into_iter().rev().collect();
    body = collapse_repeats(body);
    let carried: usize = body.iter().map(String::len).sum();
    let mut parts = vec![header];
    parts.append(&mut body);
    if metrics.dropped_count > 0 {
        parts.push(format!(
            "[{} older message(s) dropped to fit recap budget]",
            metrics.dropped_count
        ));
    }
    let summary = parts.join("\n");
    metrics.recap_bytes = summary.len();
    let carried = carried.min(summary.len());
    let scaffold_bytes = summary.len() - carried;
    (CompactionSummary::new(summary, scaffold_bytes), metrics)
}

pub fn apply_model_visible_policy(mut summary: String, policy: &CompactionPolicy) -> String {
    if !policy.is_model_visible_scope() {
        return summary;
    }
    let Some(directives) = policy.prompt_directives() else {
        return summary;
    };
    summary.push_str("\n\n[compaction instructions]\n");
    summary.push_str(&directives);
    summary
}

#[cfg(test)]
mod tests;
