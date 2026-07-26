//! The loud-boundary invariant (harn#5142).
//!
//! Between the bytes a model produces and the action a harness executes sit a
//! dozen filters: a tool-call parser, a response-content extractor, a visible
//! text sanitizer, a host event allowlist, an admission gate, a turn cap. Each
//! one can decline to pass something through. When one of them declines
//! *silently*, the loss is not merely unobserved — it is actively misattributed,
//! because every downstream signal (stall feedback, the governor, the verdict
//! layer) sees an agent that produced no action and concludes the model is
//! pathological. #5142 is the class: an allowlist that falls through to prose is
//! indistinguishable, from the outside, from a model that said nothing.
//!
//! This module is the single funnel every such boundary reports through, so
//! there is exactly one vocabulary for "model output died here" and exactly one
//! event on the wire.
//!
//! # Why the silent path is structurally hard
//!
//! A boundary that can drop model output cannot express the drop without
//! constructing a [`BoundaryFailure`], and a constructed `BoundaryFailure` is
//! very difficult to lose:
//!
//! 1. It is `#[must_use]`, so ignoring the value of a constructor or a
//!    combinator is a compiler warning (the workspace denies warnings in CI).
//! 2. Its [`Drop`] impl is a backstop: a failure that goes out of scope without
//!    [`BoundaryFailure::report`] still emits, flagged `unreported: true`, and
//!    additionally trips a `debug_assert` so the omission fails tests rather
//!    than shipping. There is no destructor path that discards the record —
//!    including a boundary that collects several failures in a `Vec` and then
//!    loses track of it.
//! 3. [`BoundaryId`] is a closed enum, and `make check-loud-boundaries` requires
//!    a 1:1 correspondence between its variants, the entries in
//!    `scripts/loud_boundaries.toml`, and the live construction sites in the
//!    tree. Adding a drop path in a new file fails the gate until the registry
//!    names it; deleting the last construction site for a registered boundary
//!    fails it too.
//!
//! The residual freedom a future engineer has is to write a drop path that
//! never mentions this module at all. That is what the registry's audit trail
//! and the per-boundary tests exist to make visible; it is not something a type
//! can prevent. What a type *can* prevent — building the record and then losing
//! it — is prevented here.
//!
//! # Which observers see a Rust-side failure
//!
//! [`BoundaryFailure::report`] goes out through [`crate::agent_events::emit_event`],
//! which reaches every external sink: the ACP adapter, the event log, the JSONL
//! run record, and any wildcard observer. It does *not* reach `.harn` closure
//! subscribers registered with `agent_subscribe`, because notifying those needs
//! an async VM context and most drop sites are deep synchronous code with no
//! context in hand. Harn-emitted failures (below) go through the host emit
//! builtin and therefore reach both.
//!
//! That asymmetry is deliberate rather than an oversight: the consumers this
//! invariant exists for — the stall detector, the rate governor, the verdict
//! layer, and the host UI — all read the persisted/external surface.
//!
//! # Emitting from Harn
//!
//! `boundary_failure` is an allowlisted `__host_agent_emit_event` type, so a
//! `.harn` boundary reports through the same funnel and the same event:
//!
//! ```harn
//! agent_emit_event(session_id, "boundary_failure", {
//!   boundary: "chat_turn_cap",
//!   kind: "capped",
//!   detail: "agent_chat_loop reached max_turns",
//! })
//! ```

use serde::{Deserialize, Serialize};

use crate::agent_events::AgentEvent;

/// Longest excerpt of model bytes carried on a boundary event. Long enough to
/// identify the dialect or the truncated span, short enough that a pathological
/// turn cannot flood the event stream.
const MAX_EXCERPT_CHARS: usize = 240;

/// Every place in the runtime where model-produced bytes become — or fail to
/// become — executed action.
///
/// Closed on purpose. A new variant is a new entry in
/// `scripts/loud_boundaries.toml`, enforced by `make check-loud-boundaries`;
/// that pairing is what keeps the enumeration in #5142 from rotting.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryId {
    /// Batch text-tool-call parsing: model text in, normalized tool calls out.
    /// Drops anything it consumed without producing a call.
    TextToolParse,
    /// Provider response ingestion: content blocks, output items, and message
    /// parts the runtime has no handler for.
    ResponseContentExtraction,
    /// Visible-text sanitization: internal protocol blocks stripped out of the
    /// text a host renders.
    VisibleTextSanitize,
    /// `__host_agent_emit_event` ingress: a `.harn` boundary tried to report and
    /// the payload did not typecheck into an [`AgentEvent`].
    HostEventIngest,
    /// The provider rate governor's admission gate: a call that proceeds
    /// without the reservation the governor's back-pressure assumes.
    ProviderAdmissionGate,
    /// `agent_chat_loop` turn/input caps: the loop stops mid-conversation.
    ChatTurnCap,
}

impl BoundaryId {
    pub const ALL: [Self; 6] = [
        Self::TextToolParse,
        Self::ResponseContentExtraction,
        Self::VisibleTextSanitize,
        Self::HostEventIngest,
        Self::ProviderAdmissionGate,
        Self::ChatTurnCap,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TextToolParse => "text_tool_parse",
            Self::ResponseContentExtraction => "response_content_extraction",
            Self::VisibleTextSanitize => "visible_text_sanitize",
            Self::HostEventIngest => "host_event_ingest",
            Self::ProviderAdmissionGate => "provider_admission_gate",
            Self::ChatTurnCap => "chat_turn_cap",
        }
    }
}

impl std::fmt::Display for BoundaryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the boundary did to the model output.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoundaryFailureKind {
    /// Bytes were consumed and produced neither action nor error.
    Dropped,
    /// Bytes carried the shape of an action no handler claimed.
    Unrecognized,
    /// Content passed through but was shortened.
    Truncated,
    /// An in-flight action was terminated before it completed.
    Killed,
    /// A cap or policy refused to let the flow continue.
    Capped,
}

impl BoundaryFailureKind {
    pub const ALL: [Self; 5] = [
        Self::Dropped,
        Self::Unrecognized,
        Self::Truncated,
        Self::Killed,
        Self::Capped,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dropped => "dropped",
            Self::Unrecognized => "unrecognized",
            Self::Truncated => "truncated",
            Self::Killed => "killed",
            Self::Capped => "capped",
        }
    }

    /// Coarse attribution, in the same vocabulary as
    /// [`crate::agent_events::AgentTerminalKind::owner`].
    ///
    /// Every kind here attributes away from the model. That is the whole point
    /// of the class: before #5142 these losses reached the verdict layer as an
    /// agent that produced nothing, i.e. as `agent` fault. A cap is a `policy`
    /// decision; everything else is the harness declining to carry bytes the
    /// model did in fact produce.
    pub fn owner(self) -> &'static str {
        match self {
            Self::Capped => "policy",
            Self::Dropped | Self::Unrecognized | Self::Truncated | Self::Killed => "harness",
        }
    }
}

impl std::fmt::Display for BoundaryFailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One thing a boundary declined to turn into action.
///
/// Constructing this value is the *only* way a boundary in this crate expresses
/// a drop, and the value cannot be discarded quietly: see the module docs for
/// the `#[must_use]` / `Drop`-backstop pairing.
#[must_use = "a BoundaryFailure that is never reported is the silent drop this type exists to \
              prevent — call .report()"]
#[derive(Debug)]
pub struct BoundaryFailure {
    boundary: BoundaryId,
    kind: BoundaryFailureKind,
    detail: String,
    excerpt: Option<String>,
    session_id: String,
    dropped_count: usize,
    dropped_bytes: usize,
    reported: bool,
}

impl BoundaryFailure {
    /// Open a failure record. `detail` names what was lost in terms a human
    /// triaging a dead run can act on — the dialect, the block type, the cap.
    pub fn new(boundary: BoundaryId, kind: BoundaryFailureKind, detail: impl Into<String>) -> Self {
        Self {
            boundary,
            kind,
            detail: detail.into(),
            excerpt: None,
            session_id: crate::agent_sessions::current_session_id().unwrap_or_default(),
            dropped_count: 1,
            dropped_bytes: 0,
            reported: false,
        }
    }

    /// Attach the model bytes themselves, truncated to [`MAX_EXCERPT_CHARS`].
    /// `dropped_bytes` records the untruncated length, so a triage reader can
    /// tell a stray token from three kilobytes of narration.
    pub fn with_excerpt(mut self, text: &str) -> Self {
        self.dropped_bytes = text.len();
        let trimmed = text.trim();
        let excerpt: String = trimmed.chars().take(MAX_EXCERPT_CHARS).collect();
        self.excerpt = (!excerpt.is_empty()).then_some(excerpt);
        self
    }

    /// Record how many distinct items this failure covers, when a boundary
    /// coalesces a run of drops into one report.
    pub fn with_count(mut self, count: usize) -> Self {
        self.dropped_count = count;
        self
    }

    /// Override the session the failure is attributed to. Defaults to the
    /// ambient agent session, which is what deep runtime code has in hand.
    pub fn in_session(mut self, session_id: &str) -> Self {
        self.session_id = session_id.to_string();
        self
    }

    pub fn boundary(&self) -> BoundaryId {
        self.boundary
    }

    pub fn kind(&self) -> BoundaryFailureKind {
        self.kind
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    fn to_event(&self, unreported: bool) -> AgentEvent {
        AgentEvent::BoundaryFailure {
            session_id: self.session_id.clone(),
            boundary: self.boundary,
            kind: self.kind,
            owner: self.kind.owner().to_string(),
            detail: self.detail.clone(),
            excerpt: self.excerpt.clone(),
            dropped_count: self.dropped_count,
            dropped_bytes: self.dropped_bytes,
            unreported,
        }
    }

    /// Put the failure on the event bus. Consumes the record so a boundary
    /// cannot report the same loss twice.
    pub fn report(mut self) {
        self.reported = true;
        crate::agent_events::emit_event(&self.to_event(false));
    }
}

impl Drop for BoundaryFailure {
    fn drop(&mut self) {
        if self.reported {
            return;
        }
        self.reported = true;
        // Emit first: even when the debug assertion below is about to fail the
        // test, the record should exist. In release this is the whole backstop.
        crate::agent_events::emit_event(&self.to_event(true));
        debug_assert!(
            std::thread::panicking(),
            "BoundaryFailure({}, {}) went out of scope without report(): {}. \
             Reaching this backstop means a drop path forgot the funnel.",
            self.boundary,
            self.kind,
            self.detail,
        );
    }
}

#[cfg(test)]
pub(crate) mod tests;
