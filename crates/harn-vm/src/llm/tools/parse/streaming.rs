//! Streaming candidate detector for text-mode tool calls (harn#692).
//!
//! Today the post-stream parsers (`parse_text_tool_calls_with_tools` and
//! `parse_bare_calls_in_body`) only run after the full provider response
//! is received, so clients see no progress while the model writes a
//! 200-line `edit({...})` body. This detector consumes the in-flight
//! assistant text buffer one delta at a time and emits the candidate
//! lifecycle events that ACP clients render as a "parsing" chip:
//!
//! - **Candidate started.** A `<tool_call>` opener at the start of a
//!   line, or a known-tool bare call shape `name(` at line start. Emits
//!   `AgentEvent::ToolCall { status: Pending, parsing: Some(true), .. }`.
//!   `tool_name` is populated for the bare path; for the tagged path it
//!   stays empty until the body parses, since the inner `name(` may
//!   arrive in a later delta.
//! - **Candidate promoted.** Args parsed cleanly. Emits
//!   `AgentEvent::ToolCallUpdate { status: Pending, parsing:
//!   Some(false), raw_output: Some(<args>), .. }`. The dispatch path
//!   (`tool_dispatch.rs`) picks up its own `tool_call_id` for the
//!   subsequent `Pending → InProgress → Completed/Failed` flow; the
//!   candidate id is only correlated WITHIN the candidate phase.
//! - **Candidate aborted.** Args parse failed — malformed JSON, unclosed
//!   tag, fallthrough to prose, or stream ended mid-call. Emits
//!   `AgentEvent::ToolCallUpdate { status: Failed, parsing:
//!   Some(false), error_category: Some(ParseAborted), error: Some(<msg>),
//!   .. }` so clients dismiss the chip.
//!
//! The detector respects markdown code-fence context: a `function(x)`
//! shape inside a triple-backtick fence does NOT trigger candidate
//! events, even if `function` happens to be a known tool. The
//! post-stream parsers are more permissive (they parse calls inside
//! fences too), but they aren't running until after the stream
//! completes, so the candidate stream is purely additional UX
//! observability — never the source of truth for dispatch.

use std::collections::BTreeSet;

use crate::agent_events::{AgentEvent, ToolCallErrorCategory, ToolCallStatus};

use super::syntax::{ident_length, parse_ts_call_from};

const TAGGED_OPEN: &str = "<tool_call>";
const TAGGED_CLOSE: &str = "</tool_call>";

/// Streaming candidate detector for text-mode tool calls.
///
/// One detector per LLM call. Construct with [`StreamingToolCallDetector::new`],
/// feed each text delta through [`Self::push`], and call
/// [`Self::finalize`] when the stream ends — finalize emits the
/// terminal abort event for any candidate that was opened but never
/// closed (e.g. `<tool_call>` without a matching `</tool_call>`, or a
/// bare `name(` whose `)` never arrived).
pub(crate) struct StreamingToolCallDetector {
    session_id: String,
    known_tools: BTreeSet<String>,
    buffer: String,
    /// Byte position in `buffer` up to which the idle-state scanner has
    /// consumed. Persists across `push` calls so each delta only
    /// scans the new bytes (modulo state-machine transitions).
    cursor: usize,
    candidate_seq: u64,
    state: DetectorState,
    /// Inside a triple-backtick markdown fence. Suppresses candidate
    /// detection so prose like `function(x)` in a code block doesn't
    /// emit a candidate. Markdown fences are NOT nestable per the spec
    /// — toggled on each fence line.
    in_fence: bool,
    /// Tracks whether the next byte the scanner sees is at the start of
    /// a logical line. Bare-call detection only fires at line starts.
    at_line_start: bool,
}

enum DetectorState {
    Idle,
    /// Inside a `<tool_call>...` block. Buffering until `</tool_call>`.
    InTaggedBlock {
        body_start: usize,
        tool_call_id: String,
    },
    /// Inside a bare `name(` call. Buffering until the TS-call parser
    /// can resolve a balanced `)`.
    InBareCall {
        name_start: usize,
        tool_call_id: String,
        name: String,
    },
}

impl StreamingToolCallDetector {
    pub(crate) fn new(session_id: String, known_tools: BTreeSet<String>) -> Self {
        Self {
            session_id,
            known_tools,
            buffer: String::new(),
            cursor: 0,
            candidate_seq: 0,
            state: DetectorState::Idle,
            in_fence: false,
            at_line_start: true,
        }
    }

    fn next_candidate_id(&mut self) -> String {
        self.candidate_seq += 1;
        format!("text-cand-{}", self.candidate_seq)
    }

    /// Append one streaming delta and return any candidate events that
    /// the new bytes triggered. Always returns; never blocks on more
    /// input.
    pub(crate) fn push(&mut self, delta: &str) -> Vec<AgentEvent> {
        if delta.is_empty() {
            return Vec::new();
        }
        self.buffer.push_str(delta);
        self.scan()
    }

    /// Signal end-of-stream. Emits the terminal abort event for any
    /// candidate left in flight (unclosed `<tool_call>` tag, bare call
    /// missing its `)`, or bare call whose final args reject parsing).
    pub(crate) fn finalize(&mut self) -> Vec<AgentEvent> {
        let mut events = self.scan();
        match std::mem::replace(&mut self.state, DetectorState::Idle) {
            DetectorState::Idle => {}
            DetectorState::InTaggedBlock { tool_call_id, .. } => {
                events.push(AgentEvent::ToolCallUpdate {
                    session_id: self.session_id.clone(),
                    tool_call_id,
                    tool_name: String::new(),
                    status: ToolCallStatus::Failed,
                    raw_output: None,
                    error: Some(
                        "<tool_call> block did not close before the response ended.".to_string(),
                    ),
                    duration_ms: None,
                    execution_duration_ms: None,
                    error_category: Some(ToolCallErrorCategory::ParseAborted),
                    executor: None,
                    parsing: Some(false),
                });
            }
            DetectorState::InBareCall {
                name_start,
                tool_call_id,
                name,
            } => {
                let attempt = parse_ts_call_from(&self.buffer[name_start..], name.clone());
                match attempt {
                    Ok((args, _)) => {
                        events.push(promote_event(&self.session_id, tool_call_id, name, args));
                    }
                    Err(msg) => {
                        events.push(abort_event(&self.session_id, tool_call_id, name, msg));
                    }
                }
            }
        }
        events
    }

    fn scan(&mut self) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        loop {
            let progressed = match self.state {
                DetectorState::Idle => self.scan_idle(&mut events),
                DetectorState::InTaggedBlock { .. } => self.scan_tagged(&mut events),
                DetectorState::InBareCall { .. } => self.scan_bare(&mut events),
            };
            if !progressed {
                break;
            }
        }
        events
    }

    /// Walk forward from `cursor` looking for the next candidate marker.
    /// Returns `true` if a candidate was found (and we transitioned out
    /// of `Idle`), `false` if the rest of the buffer is consumed without
    /// finding one. Tracks fence-depth and line-start state in-place so
    /// the next call can resume cleanly.
    fn scan_idle(&mut self, events: &mut Vec<AgentEvent>) -> bool {
        let bytes = self.buffer.as_bytes();
        let mut i = self.cursor;
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'\n' {
                self.at_line_start = true;
                i += 1;
                continue;
            }

            if self.at_line_start {
                // Skip leading whitespace to locate the first non-blank
                // byte of the line.
                let mut j = i;
                while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
                    j += 1;
                }
                if j >= bytes.len() {
                    // Whitespace runs to end of buffer — wait for more.
                    self.cursor = i;
                    return false;
                }

                // Triple-backtick fence: require the line's `\n` to be
                // present before committing to the toggle, otherwise an
                // incomplete fence opener might be misclassified.
                if bytes[j] == b'`'
                    && bytes.get(j + 1) == Some(&b'`')
                    && bytes.get(j + 2) == Some(&b'`')
                {
                    let eol = bytes[j + 3..].iter().position(|&c| c == b'\n');
                    let Some(eol_rel) = eol else {
                        // Fence opener without EOL yet — wait.
                        self.cursor = i;
                        return false;
                    };
                    self.in_fence = !self.in_fence;
                    i = j + 3 + eol_rel + 1;
                    self.at_line_start = true;
                    continue;
                }

                if self.in_fence {
                    // Skip to end of line; fence content is opaque.
                    while i < bytes.len() && bytes[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }

                // Tagged opener.
                let rest = &self.buffer[j..];
                if rest.starts_with(TAGGED_OPEN) {
                    let body_start = j + TAGGED_OPEN.len();
                    let id = self.next_candidate_id();
                    events.push(AgentEvent::ToolCall {
                        session_id: self.session_id.clone(),
                        tool_call_id: id.clone(),
                        tool_name: String::new(),
                        kind: None,
                        status: ToolCallStatus::Pending,
                        raw_input: serde_json::json!({}),
                        parsing: Some(true),
                    });
                    self.state = DetectorState::InTaggedBlock {
                        body_start,
                        tool_call_id: id,
                    };
                    self.cursor = body_start;
                    return true;
                }
                if TAGGED_OPEN.starts_with(rest) {
                    // Partial `<tool_call>` prefix — wait for the rest.
                    self.cursor = i;
                    return false;
                }

                // Bare ident + `(`. Require the identifier to be
                // terminated (next byte not an identifier-continuation)
                // before deciding, so a delta arriving mid-name doesn't
                // commit to a wrong-shape conclusion.
                if let Some(name_len) = ident_length(&bytes[j..]) {
                    let term = j + name_len;
                    if term >= bytes.len() {
                        // Identifier still streaming.
                        self.cursor = i;
                        return false;
                    }
                    if bytes[term] == b'(' {
                        let name = std::str::from_utf8(&bytes[j..term])
                            .unwrap_or("")
                            .to_string();
                        if self.known_tools.contains(&name) {
                            let id = self.next_candidate_id();
                            events.push(AgentEvent::ToolCall {
                                session_id: self.session_id.clone(),
                                tool_call_id: id.clone(),
                                tool_name: name.clone(),
                                kind: None,
                                status: ToolCallStatus::Pending,
                                raw_input: serde_json::json!({}),
                                parsing: Some(true),
                            });
                            self.state = DetectorState::InBareCall {
                                name_start: j,
                                tool_call_id: id,
                                name,
                            };
                            self.cursor = j;
                            return true;
                        }
                    }
                    // Identifier didn't match a tool — skip past it.
                    i = term;
                    self.at_line_start = false;
                    continue;
                }

                // Line starts with non-identifier non-fence non-tag —
                // mark not-line-start and advance past current byte.
                self.at_line_start = false;
                i = j + 1;
                continue;
            }

            // Mid-line: just walk forward.
            i += 1;
        }
        self.cursor = i;
        false
    }

    fn scan_tagged(&mut self, events: &mut Vec<AgentEvent>) -> bool {
        let (body_start, tool_call_id) = match &self.state {
            DetectorState::InTaggedBlock {
                body_start,
                tool_call_id,
            } => (*body_start, tool_call_id.clone()),
            _ => return false,
        };
        let Some(close_rel) = self.buffer[body_start..].find(TAGGED_CLOSE) else {
            return false;
        };
        let body_end = body_start + close_rel;
        let after = body_end + TAGGED_CLOSE.len();
        let body = self.buffer[body_start..body_end].trim().to_string();
        let parse_attempt = if body.is_empty() {
            Err("<tool_call> body was empty.".to_string())
        } else if let Some(name_len) = ident_length(body.as_bytes()) {
            let name = body[..name_len].to_string();
            if !self.known_tools.contains(&name) {
                Err(format!("Unknown tool '{name}' in <tool_call> body."))
            } else if body.as_bytes().get(name_len) != Some(&b'(') {
                Err(format!(
                    "Expected `{name}(` immediately after the tool name in <tool_call> body."
                ))
            } else {
                parse_ts_call_from(&body, name.clone()).map(|(args, _)| (name, args))
            }
        } else {
            Err("<tool_call> body did not begin with a `name(` identifier.".to_string())
        };
        match parse_attempt {
            Ok((name, args)) => {
                events.push(promote_event(&self.session_id, tool_call_id, name, args));
            }
            Err(msg) => {
                events.push(abort_event(
                    &self.session_id,
                    tool_call_id,
                    String::new(),
                    msg,
                ));
            }
        }
        self.state = DetectorState::Idle;
        self.cursor = after;
        // Treat the `</tool_call>` close as a clean line break so the
        // next bare call on its own line is still detected at line
        // start, even if the source emitted them adjacently.
        self.at_line_start = true;
        true
    }

    fn scan_bare(&mut self, events: &mut Vec<AgentEvent>) -> bool {
        let (name_start, tool_call_id, name) = match &self.state {
            DetectorState::InBareCall {
                name_start,
                tool_call_id,
                name,
            } => (*name_start, tool_call_id.clone(), name.clone()),
            _ => return false,
        };
        let attempt = parse_ts_call_from(&self.buffer[name_start..], name.clone());
        match attempt {
            Ok((args, consumed)) => {
                events.push(promote_event(&self.session_id, tool_call_id, name, args));
                self.state = DetectorState::Idle;
                self.cursor = name_start + consumed;
                self.at_line_start = false;
                true
            }
            Err(_) => {
                // Args still streaming. We don't try to distinguish
                // "transient (waiting for `)`)" from "definitely
                // malformed" mid-stream — the same parse runs at
                // finalize() and reports the abort there. This avoids
                // false aborts when a multi-line heredoc pauses between
                // chunks.
                false
            }
        }
    }
}

fn promote_event(
    session_id: &str,
    tool_call_id: String,
    name: String,
    args: serde_json::Value,
) -> AgentEvent {
    AgentEvent::ToolCallUpdate {
        session_id: session_id.to_string(),
        tool_call_id,
        tool_name: name,
        status: ToolCallStatus::Pending,
        raw_output: Some(args),
        error: None,
        duration_ms: None,
        execution_duration_ms: None,
        error_category: None,
        executor: None,
        parsing: Some(false),
    }
}

fn abort_event(
    session_id: &str,
    tool_call_id: String,
    tool_name: String,
    msg: String,
) -> AgentEvent {
    AgentEvent::ToolCallUpdate {
        session_id: session_id.to_string(),
        tool_call_id,
        tool_name,
        status: ToolCallStatus::Failed,
        raw_output: None,
        error: Some(msg),
        duration_ms: None,
        execution_duration_ms: None,
        error_category: Some(ToolCallErrorCategory::ParseAborted),
        executor: None,
        parsing: Some(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known_set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| (*s).to_string()).collect()
    }

    fn detector(names: &[&str]) -> StreamingToolCallDetector {
        StreamingToolCallDetector::new("session-1".to_string(), known_set(names))
    }

    /// Walk every event the detector emits when fed `chunks`, finalize,
    /// and return them in order. Tests assert on this flat list.
    fn run(chunks: &[&str], detector: &mut StreamingToolCallDetector) -> Vec<AgentEvent> {
        let mut all = Vec::new();
        for chunk in chunks {
            all.extend(detector.push(chunk));
        }
        all.extend(detector.finalize());
        all
    }

    fn unwrap_call(event: &AgentEvent) -> (&str, &str, Option<bool>) {
        match event {
            AgentEvent::ToolCall {
                tool_call_id,
                tool_name,
                parsing,
                ..
            } => (tool_call_id.as_str(), tool_name.as_str(), *parsing),
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    fn unwrap_update(
        event: &AgentEvent,
    ) -> (
        &str,
        &str,
        ToolCallStatus,
        Option<bool>,
        Option<ToolCallErrorCategory>,
    ) {
        match event {
            AgentEvent::ToolCallUpdate {
                tool_call_id,
                tool_name,
                status,
                parsing,
                error_category,
                ..
            } => (
                tool_call_id.as_str(),
                tool_name.as_str(),
                *status,
                *parsing,
                *error_category,
            ),
            other => panic!("expected ToolCallUpdate, got {other:?}"),
        }
    }

    #[test]
    fn bare_candidate_promotes_on_balanced_close() {
        let mut det = detector(&["read"]);
        let events = run(&["read({", " path: \"a.md\" })"], &mut det);
        assert_eq!(events.len(), 2, "events={events:#?}");
        let (id, name, parsing) = unwrap_call(&events[0]);
        assert_eq!(name, "read");
        assert_eq!(parsing, Some(true));
        let (id2, name2, status, parsing, cat) = unwrap_update(&events[1]);
        assert_eq!(id, id2, "candidate id reused on promotion");
        assert_eq!(name2, "read");
        assert_eq!(status, ToolCallStatus::Pending);
        assert_eq!(parsing, Some(false));
        assert!(cat.is_none(), "promote has no error_category");
    }

    #[test]
    fn bare_candidate_aborts_on_malformed_args() {
        // The `name(` opens a candidate. The body is broken JSON and
        // the stream ends without a balanced `)`. finalize() emits the
        // abort with parse_aborted.
        let mut det = detector(&["edit"]);
        let events = run(&["edit({ broken: , }"], &mut det);
        assert_eq!(events.len(), 2, "events={events:#?}");
        let (start_id, start_name, start_parsing) = unwrap_call(&events[0]);
        assert_eq!(start_name, "edit");
        assert_eq!(start_parsing, Some(true));
        let (terminal_id, _name, status, parsing, cat) = unwrap_update(&events[1]);
        assert_eq!(start_id, terminal_id);
        assert_eq!(status, ToolCallStatus::Failed);
        assert_eq!(parsing, Some(false));
        assert_eq!(cat, Some(ToolCallErrorCategory::ParseAborted));
    }

    #[test]
    fn tagged_candidate_promotes_when_block_closes() {
        let mut det = detector(&["run"]);
        let events = run(
            &[
                "<tool_call>\n",
                "run({ command: \"ls\" })\n",
                "</tool_call>",
            ],
            &mut det,
        );
        assert_eq!(events.len(), 2, "events={events:#?}");
        let (start_id, start_name, parsing) = unwrap_call(&events[0]);
        assert_eq!(start_name, "");
        assert_eq!(parsing, Some(true));
        let (terminal_id, terminal_name, status, parsing, cat) = unwrap_update(&events[1]);
        assert_eq!(start_id, terminal_id, "ids match across promote");
        assert_eq!(terminal_name, "run");
        assert_eq!(status, ToolCallStatus::Pending);
        assert_eq!(parsing, Some(false));
        assert!(cat.is_none());
    }

    #[test]
    fn tagged_candidate_aborts_when_tag_never_closes() {
        let mut det = detector(&["run"]);
        let events = run(&["<tool_call>\nrun({ command: \"ls\" })"], &mut det);
        assert_eq!(events.len(), 2);
        let (start_id, _, parsing) = unwrap_call(&events[0]);
        assert_eq!(parsing, Some(true));
        let (terminal_id, _, status, parsing, cat) = unwrap_update(&events[1]);
        assert_eq!(start_id, terminal_id);
        assert_eq!(status, ToolCallStatus::Failed);
        assert_eq!(parsing, Some(false));
        assert_eq!(cat, Some(ToolCallErrorCategory::ParseAborted));
    }

    #[test]
    fn prose_inside_code_fence_does_not_trigger_candidate() {
        // `read(x)` inside a fenced code block must not emit a
        // candidate, even though `read` is in the known-tools set.
        let mut det = detector(&["read"]);
        let events = run(
            &[
                "Here is some code:\n",
                "```python\n",
                "read(x)\n",
                "```\n",
                "Done.\n",
            ],
            &mut det,
        );
        assert!(
            events.is_empty(),
            "expected no candidate events inside fence, got: {events:#?}"
        );
    }

    #[test]
    fn unknown_tool_at_line_start_does_not_open_candidate() {
        let mut det = detector(&["read", "edit"]);
        let events = run(&["mystery({ foo: 1 })"], &mut det);
        assert!(
            events.is_empty(),
            "unknown tool name must not open a candidate, got: {events:#?}"
        );
    }

    #[test]
    fn deltas_split_mid_identifier_do_not_commit_prematurely() {
        // First delta arrives with only `re`. We must not conclude
        // "no tool call" — once `read({...})` arrives we still emit
        // a candidate. The detector waits when the identifier hasn't
        // terminated yet.
        let mut det = detector(&["read"]);
        let events = run(&["re", "ad({ path: \"a.md\" })"], &mut det);
        assert_eq!(events.len(), 2, "events={events:#?}");
        let (_, name, parsing) = unwrap_call(&events[0]);
        assert_eq!(name, "read");
        assert_eq!(parsing, Some(true));
        let (_, _, status, parsing, _) = unwrap_update(&events[1]);
        assert_eq!(status, ToolCallStatus::Pending);
        assert_eq!(parsing, Some(false));
    }

    #[test]
    fn finalize_on_empty_stream_emits_nothing() {
        let mut det = detector(&["read"]);
        let events = run(&[], &mut det);
        assert!(events.is_empty());
    }

    #[test]
    fn empty_delta_is_a_noop() {
        let mut det = detector(&["read"]);
        let events = det.push("");
        assert!(events.is_empty());
    }

    #[test]
    fn multiple_sequential_candidates_each_get_a_distinct_id() {
        let mut det = detector(&["read", "run"]);
        let events = run(
            &["read({ path: \"a.md\" })\n", "run({ command: \"ls\" })\n"],
            &mut det,
        );
        // Expect: start1, promote1, start2, promote2.
        assert_eq!(events.len(), 4, "events={events:#?}");
        let (id1, name1, _) = unwrap_call(&events[0]);
        let (id1u, _, _, _, _) = unwrap_update(&events[1]);
        let (id2, name2, _) = unwrap_call(&events[2]);
        let (id2u, _, _, _, _) = unwrap_update(&events[3]);
        assert_eq!(id1, id1u);
        assert_eq!(id2, id2u);
        assert_ne!(id1, id2, "each candidate gets its own id");
        assert_eq!(name1, "read");
        assert_eq!(name2, "run");
    }
}
