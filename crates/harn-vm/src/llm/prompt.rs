//! Unified prompt-fragment assembly.
//!
//! A system prompt is the deterministic reduction of an ordered list of
//! [`PromptFragment`]s. Host-provided system fragments (the `system` option's
//! list form), the agent's per-turn "primary" system text, rendered
//! system reminders, and capability-gated tool guidance all flow through the
//! same model and the same [`assemble`] reducer. There is no parallel
//! string-concatenation path: this module is the single source of truth for
//! how the system string is built.
//!
//! Every fragment is recorded in the returned assembly manifest
//! — included or excluded, with the reason — so the final prompt is fully
//! auditable: you can answer "why is this sentence here?" and "what would the
//! prompt look like without tool X?" without reverse-engineering a concat.

use std::collections::{BTreeSet, HashSet};

const JOIN_SEPARATOR: &str = "\n\n";
const CONTEXT_MANIFEST_SCHEMA: &str = "harn.llm.context_manifest.v1";

/// Which side of the primary system block a fragment lands on.
///
/// This mirrors the historical two-vector (`before`/`after`) mechanic so the
/// assembled bytes are stable: all included `Before` fragments in declaration
/// order, then all included `After` fragments in declaration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FragmentBucket {
    /// Emitted before/around the primary system text and reminders
    /// (preamble / prefix / context / parts / primary / reminders).
    Before,
    /// Emitted after the primary block (appendix / suffix region).
    After,
}

impl FragmentBucket {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }
}

/// One contributor to the system prompt.
///
/// `body` is already rendered (templates run upstream, in `.harn` or in the
/// reminder pipeline). `assemble` trims the body and skips empty fragments.
#[derive(Clone, Debug)]
pub struct PromptFragment {
    /// Stable, unique-ish identifier, e.g. `host:system[0]`,
    /// `primary`, `reminder`, `tool:todo.guidance`.
    pub id: String,
    /// Who contributed it, for provenance grouping (`host:*`, `primary`,
    /// `reminder`, `tool:<name>`, `stdlib:*`).
    pub source: String,
    /// Ordering bucket relative to the primary block.
    pub bucket: FragmentBucket,
    /// Included only if every named tool is present in the active tool set.
    /// This is the capability gate: a fragment that says "always update the
    /// TODO tracker" carries `requires_tools: ["todo"]` and disappears when
    /// the tool is not registered — instruction and tool can never drift.
    pub requires_tools: Vec<String>,
    /// Included only if every named capability flag is set.
    pub requires_caps: Vec<String>,
    /// Pre-rendered text. Trimmed by `assemble`; empty bodies are excluded.
    pub body: String,
}

impl PromptFragment {
    pub fn new(
        id: impl Into<String>,
        source: impl Into<String>,
        bucket: FragmentBucket,
        body: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            source: source.into(),
            bucket,
            requires_tools: Vec::new(),
            requires_caps: Vec::new(),
            body: body.into(),
        }
    }

    pub fn requiring_tools(mut self, tools: Vec<String>) -> Self {
        self.requires_tools = tools;
        self
    }

    #[allow(dead_code)] // used by capability-gated fragments (Wave 1+)
    pub fn requiring_caps(mut self, caps: Vec<String>) -> Self {
        self.requires_caps = caps;
        self
    }
}

/// Provenance for one fragment: whether it made the prompt and why.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FragmentTrace {
    pub id: String,
    pub source: String,
    pub bucket: &'static str,
    pub included: bool,
    pub reason: String,
    pub bytes: usize,
    /// Digest of the redacted, trimmed fragment body. Excluded fragments keep
    /// their digest so a consumer can distinguish "considered but gated out"
    /// from a different equal-length body.
    pub digest: String,
}

/// Which root produced the assembled prompt.
///
/// Composed roots reduce the ordinary ordered fragment list. A replacement
/// root is exclusive: its bytes are the entire system prompt and no other
/// contributor participates in assembly or provider-bound normalization.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum PromptRoot {
    #[default]
    Composed,
    Replacement,
    Transformed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ContextManifestBoundary {
    #[default]
    ObservedCallOptions,
    RequestPayloadEgress,
}

impl ContextManifestBoundary {
    fn as_str(self) -> &'static str {
        match self {
            Self::ObservedCallOptions => "observed_llm_call_pre_egress",
            Self::RequestPayloadEgress => "llm_request_payload_egress",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
struct EgressDelta {
    producer: &'static str,
    input_system_prompt_digest: String,
    input_system_prompt_bytes: usize,
    output_system_prompt_digest: String,
    output_system_prompt_bytes: usize,
    bytes_added: usize,
    bytes_removed: usize,
}

impl PromptRoot {
    fn as_str(self) -> &'static str {
        match self {
            Self::Composed => "composed",
            Self::Replacement => "replacement",
            Self::Transformed => "transformed",
        }
    }
}

/// Typed bill of materials for one assembled system prompt.
///
/// This value is carried from assembly to the provider-call transcript
/// boundary, where one egress projection reconciles it with the send-safe
/// payload. It intentionally scopes segment accounting to the top-level system
/// prompt; the sibling messages channel remains covered by the served-context
/// messages digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ContextAssemblyManifest {
    boundary: ContextManifestBoundary,
    root: PromptRoot,
    call_role: String,
    actor_chain: Option<serde_json::Value>,
    segments: Vec<FragmentTrace>,
    whole_prompt_digest: String,
    system_prompt_bytes: usize,
    egress_delta: Option<EgressDelta>,
}

impl Default for ContextAssemblyManifest {
    fn default() -> Self {
        Self::from_parts(PromptRoot::Composed, "unattributed", Vec::new(), None)
    }
}

impl ContextAssemblyManifest {
    fn from_parts(
        root: PromptRoot,
        call_role: impl Into<String>,
        segments: Vec<FragmentTrace>,
        system: Option<&str>,
    ) -> Self {
        let system = system.unwrap_or("");
        Self {
            boundary: ContextManifestBoundary::ObservedCallOptions,
            root,
            call_role: call_role.into(),
            actor_chain: None,
            segments,
            whole_prompt_digest: crate::llm::content_hash::stable_redacted_string_hash(system),
            system_prompt_bytes: system.len(),
            egress_delta: None,
        }
    }

    /// Represent an internal prompt producer that does not use the script
    /// fragment reducer. Production script calls use [`assemble`] instead.
    #[cfg(test)]
    pub(crate) fn internal(
        id: impl Into<String>,
        producer: impl Into<String>,
        call_role: impl Into<String>,
        system: Option<&str>,
    ) -> Self {
        let id = id.into();
        let producer = producer.into();
        let content = system.unwrap_or("");
        let segments = if content.is_empty() {
            Vec::new()
        } else {
            vec![FragmentTrace {
                id,
                source: producer,
                bucket: "before",
                included: true,
                reason: "internal typed prompt root".to_string(),
                bytes: content.len(),
                digest: crate::llm::content_hash::stable_redacted_string_hash(content),
            }]
        };
        Self::from_parts(PromptRoot::Composed, call_role, segments, system)
    }

    pub(crate) fn root(&self) -> PromptRoot {
        self.root
    }

    pub(crate) fn call_role(&self) -> &str {
        &self.call_role
    }

    pub(crate) fn actor_chain(&self) -> Option<&serde_json::Value> {
        self.actor_chain.as_ref()
    }

    pub(crate) fn segments(&self) -> &[FragmentTrace] {
        &self.segments
    }

    pub(crate) fn set_call_role(&mut self, call_role: impl Into<String>) {
        self.call_role = call_role.into();
    }

    pub(crate) fn set_actor_chain(&mut self, actor_chain: Option<serde_json::Value>) {
        self.actor_chain = actor_chain;
    }

    /// Replace the system-channel rows after an explicit structural
    /// experiment changes the assembled system bytes.
    pub(crate) fn record_system_transform(
        &mut self,
        id: impl Into<String>,
        producer: impl Into<String>,
        reason: impl Into<String>,
        system: Option<&str>,
    ) {
        for segment in &mut self.segments {
            if segment.included {
                segment.included = false;
                segment.bytes = 0;
                segment.reason = "superseded by system transform".to_string();
            }
        }
        let content = system.unwrap_or("");
        if !content.is_empty() {
            self.segments.push(FragmentTrace {
                id: id.into(),
                source: producer.into(),
                bucket: "before",
                included: true,
                reason: reason.into(),
                bytes: content.len(),
                digest: crate::llm::content_hash::stable_redacted_string_hash(content),
            });
        }
        self.root = PromptRoot::Transformed;
        self.whole_prompt_digest = crate::llm::content_hash::stable_redacted_string_hash(content);
        self.system_prompt_bytes = content.len();
    }

    /// Project the assembly manifest onto the exact send-safe system channel
    /// produced by `LlmRequestPayload::from`. When egress normalization changes
    /// the bytes, the final payload becomes one typed transform row while the
    /// delta retains the input/output join needed to audit the transition.
    pub(crate) fn for_request_payload_egress(&self, system: Option<&str>) -> Result<Self, String> {
        let content = system.unwrap_or("");
        let output_digest = crate::llm::content_hash::stable_redacted_string_hash(content);
        let output_bytes = content.len();
        let mut manifest = self.clone();
        manifest.boundary = ContextManifestBoundary::RequestPayloadEgress;

        if manifest.whole_prompt_digest != output_digest
            || manifest.system_prompt_bytes != output_bytes
        {
            let input_digest = manifest.whole_prompt_digest.clone();
            let input_bytes = manifest.system_prompt_bytes;
            manifest.record_system_transform(
                "egress:system",
                "llm_request_payload",
                "provider capability and system-placement normalization",
                system,
            );
            manifest.egress_delta = Some(EgressDelta {
                producer: "llm_request_payload",
                input_system_prompt_digest: input_digest,
                input_system_prompt_bytes: input_bytes,
                output_system_prompt_digest: output_digest,
                output_system_prompt_bytes: output_bytes,
                bytes_added: output_bytes.saturating_sub(input_bytes),
                bytes_removed: input_bytes.saturating_sub(output_bytes),
            });
        }

        manifest.validate(system)?;
        Ok(manifest)
    }

    pub(crate) fn as_json(&self) -> serde_json::Value {
        let segments = self
            .segments
            .iter()
            .map(|segment| {
                serde_json::json!({
                    "id": segment.id,
                    "producer": segment.source,
                    "bucket": segment.bucket,
                    "included": segment.included,
                    "reason": segment.reason,
                    "bytes": segment.bytes,
                    "digest": segment.digest,
                })
            })
            .collect::<Vec<_>>();
        let included = self
            .segments
            .iter()
            .filter(|segment| segment.included)
            .count();
        let mut value = serde_json::json!({
            "schema": CONTEXT_MANIFEST_SCHEMA,
            "scope": "system_prompt",
            "boundary": self.boundary.as_str(),
            "messages": "served_context_digest_only",
            "call_role": self.call_role,
            "actor_chain": self.actor_chain,
            "root": self.root.as_str(),
            "segments": segments,
            "whole_prompt_digest": self.whole_prompt_digest,
            "system_prompt_bytes": self.system_prompt_bytes,
            "join_separator": JOIN_SEPARATOR,
            "join_separator_bytes": JOIN_SEPARATOR.len(),
            "join_separator_count": included.saturating_sub(1),
        });
        if let Some(delta) = &self.egress_delta {
            value["egress_delta"] = serde_json::to_value(delta).unwrap_or_default();
        }
        value
    }

    /// Check that the manifest is a complete, non-contradictory account of
    /// the prompt that is about to cross the provider-call boundary.
    pub(crate) fn validate(&self, system: Option<&str>) -> Result<(), String> {
        let system = system.unwrap_or("");
        if self.call_role.trim().is_empty() {
            return Err("call_role is empty".to_string());
        }
        if self.system_prompt_bytes != system.len() {
            return Err(format!(
                "manifest system_prompt_bytes={} but provider request has {} bytes",
                self.system_prompt_bytes,
                system.len()
            ));
        }
        let actual_digest = crate::llm::content_hash::stable_redacted_string_hash(system);
        if self.whole_prompt_digest != actual_digest {
            return Err(format!(
                "manifest whole_prompt_digest {} does not match provider request {}",
                self.whole_prompt_digest, actual_digest
            ));
        }

        let included = self
            .segments
            .iter()
            .filter(|segment| segment.included)
            .collect::<Vec<_>>();
        let mut seen = HashSet::new();
        for segment in &included {
            let key = (segment.id.as_str(), segment.digest.as_str());
            if !seen.insert(key) {
                return Err(format!(
                    "duplicate included context segment ({}, {})",
                    segment.id, segment.digest
                ));
            }
        }

        if self.root == PromptRoot::Replacement
            && (self.segments.len() != 1 || included.len() != 1 || included[0].id != "replacement")
        {
            return Err(
                "replacement root must contain exactly one included `replacement` segment"
                    .to_string(),
            );
        }

        let fragment_bytes: usize = included.iter().map(|segment| segment.bytes).sum();
        let separator_bytes = included.len().saturating_sub(1) * JOIN_SEPARATOR.len();
        if fragment_bytes + separator_bytes != system.len() {
            return Err(format!(
                "included segment bytes ({fragment_bytes}) + join separators \
                 ({separator_bytes}) do not reconcile with system prompt bytes ({})",
                system.len()
            ));
        }
        let mut offset = 0usize;
        for (index, segment) in included.iter().enumerate() {
            let end = offset + segment.bytes;
            let body = system.get(offset..end).ok_or_else(|| {
                format!(
                    "included segment `{}` byte boundary does not align with the system prompt",
                    segment.id
                )
            })?;
            let actual_segment_digest = crate::llm::content_hash::stable_redacted_string_hash(body);
            if segment.digest != actual_segment_digest {
                return Err(format!(
                    "included segment `{}` digest {} does not match retained bytes {}",
                    segment.id, segment.digest, actual_segment_digest
                ));
            }
            offset = end;
            if index + 1 < included.len() {
                let separator_end = offset + JOIN_SEPARATOR.len();
                if system.get(offset..separator_end) != Some(JOIN_SEPARATOR) {
                    return Err(format!(
                        "included segment `{}` is not followed by the documented join separator",
                        segment.id
                    ));
                }
                offset = separator_end;
            }
        }
        Ok(())
    }
}

/// Result of [`assemble`]: the system string (if any) plus full provenance
/// for every fragment that was considered.
#[derive(Clone, Debug, Default)]
pub struct AssembledPrompt {
    pub system: Option<String>,
    manifest: ContextAssemblyManifest,
}

impl AssembledPrompt {
    pub(crate) fn root(&self) -> PromptRoot {
        self.manifest.root()
    }

    pub(crate) fn manifest(&self) -> &ContextAssemblyManifest {
        &self.manifest
    }

    pub(crate) fn set_call_role(&mut self, call_role: impl Into<String>) {
        self.manifest.set_call_role(call_role);
    }

    pub(crate) fn set_actor_chain(&mut self, actor_chain: Option<serde_json::Value>) {
        self.manifest.set_actor_chain(actor_chain);
    }

    /// Provenance serialized for the `prompt_explain` builtin / CLI and for
    /// transcript audit metadata.
    pub fn provenance_json(&self) -> serde_json::Value {
        let fragments = self
            .manifest
            .segments()
            .iter()
            .map(|segment| {
                let mut value = serde_json::to_value(segment).unwrap_or_default();
                value["producer"] = serde_json::Value::String(segment.source.clone());
                value
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "schema": CONTEXT_MANIFEST_SCHEMA,
            "scope": "system_prompt",
            "boundary": "observed_llm_call_pre_egress",
            "messages": "served_context_digest_only",
            "call_role": self.manifest.call_role(),
            "actor_chain": self.manifest.actor_chain(),
            "root": self.root().as_str(),
            "fragments": fragments,
            "whole_prompt_digest": self.manifest.whole_prompt_digest,
            "system_prompt_bytes": self.manifest.system_prompt_bytes,
            "join_separator": JOIN_SEPARATOR,
            "join_separator_bytes": JOIN_SEPARATOR.len(),
            "join_separator_count": self.manifest.segments().iter().filter(|t| t.included).count().saturating_sub(1),
            "included": self.manifest.segments().iter().filter(|t| t.included).count(),
            "excluded": self.manifest.segments().iter().filter(|t| !t.included).count(),
        })
    }
}

/// Select an exclusive replacement root.
///
/// Unlike ordinary fragment assembly, replacement bytes are not trimmed or
/// joined. This is the structural escape hatch for exact prompt ablations and
/// conformance probes: the caller's content is the complete system prompt.
pub(crate) fn replace(content: String) -> AssembledPrompt {
    let bytes = content.len();
    let digest = crate::llm::content_hash::stable_redacted_string_hash(&content);
    let provenance = vec![FragmentTrace {
        id: "replacement".to_string(),
        source: "caller".to_string(),
        bucket: "before",
        included: true,
        reason: "exclusive replacement root".to_string(),
        bytes,
        digest,
    }];
    let manifest = ContextAssemblyManifest::from_parts(
        PromptRoot::Replacement,
        "unattributed",
        provenance,
        Some(&content),
    );
    AssembledPrompt {
        system: Some(content),
        manifest,
    }
}

/// Inputs that gate fragment inclusion: which tools and capability flags are
/// active for this assembly.
#[derive(Default, Debug)]
pub struct AssembleCtx {
    pub tool_names: BTreeSet<String>,
    pub caps: BTreeSet<String>,
}

impl AssembleCtx {
    fn missing_tool<'a>(&self, frag: &'a PromptFragment) -> Option<&'a str> {
        frag.requires_tools
            .iter()
            .find(|tool| !self.tool_names.contains(*tool))
            .map(String::as_str)
    }

    fn missing_cap<'a>(&self, frag: &'a PromptFragment) -> Option<&'a str> {
        frag.requires_caps
            .iter()
            .find(|cap| !self.caps.contains(*cap))
            .map(String::as_str)
    }
}

/// Reduce fragments to the final system string, recording provenance for
/// every fragment in declaration order.
///
/// Ordering is faithful to the legacy `before`/`after` mechanic: included
/// `Before` fragments in declaration order, then included `After` fragments
/// in declaration order, joined with a blank line. Bodies are trimmed; empty
/// (or gated-out) fragments are excluded but still recorded with a reason.
pub fn assemble(fragments: &[PromptFragment], ctx: &AssembleCtx) -> AssembledPrompt {
    let mut provenance = Vec::with_capacity(fragments.len());
    let mut before: Vec<String> = Vec::new();
    let mut after: Vec<String> = Vec::new();

    // Manifest order is wire order: before-bucket rows followed by after
    // rows, with declaration order preserved within each bucket.
    for frag in fragments
        .iter()
        .filter(|frag| frag.bucket == FragmentBucket::Before)
        .chain(
            fragments
                .iter()
                .filter(|frag| frag.bucket == FragmentBucket::After),
        )
    {
        let trimmed = frag.body.trim();
        let (included, reason) = if trimmed.is_empty() {
            (false, "empty body".to_string())
        } else if let Some(tool) = ctx.missing_tool(frag) {
            (false, format!("requires tool `{tool}` (not available)"))
        } else if let Some(cap) = ctx.missing_cap(frag) {
            (false, format!("requires capability `{cap}` (not set)"))
        } else if !frag.requires_tools.is_empty() {
            (
                true,
                format!("tool(s) present: {}", frag.requires_tools.join(", ")),
            )
        } else if !frag.requires_caps.is_empty() {
            (
                true,
                format!("capabilit(ies) present: {}", frag.requires_caps.join(", ")),
            )
        } else {
            (true, "unconditional".to_string())
        };

        provenance.push(FragmentTrace {
            id: frag.id.clone(),
            source: frag.source.clone(),
            bucket: frag.bucket.as_str(),
            included,
            reason,
            bytes: if included { trimmed.len() } else { 0 },
            digest: crate::llm::content_hash::stable_redacted_string_hash(trimmed),
        });

        if included {
            match frag.bucket {
                FragmentBucket::Before => before.push(trimmed.to_string()),
                FragmentBucket::After => after.push(trimmed.to_string()),
            }
        }
    }

    before.extend(after);
    let system = if before.is_empty() {
        None
    } else {
        Some(before.join(JOIN_SEPARATOR))
    };
    let manifest = ContextAssemblyManifest::from_parts(
        PromptRoot::Composed,
        "unattributed",
        provenance,
        system.as_deref(),
    );
    AssembledPrompt { system, manifest }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(id: &str, bucket: FragmentBucket, body: &str) -> PromptFragment {
        PromptFragment::new(id, id, bucket, body)
    }

    #[test]
    fn before_then_after_join_is_blank_line_separated() {
        let frags = vec![
            frag("parts", FragmentBucket::Before, "parts"),
            frag("appendix", FragmentBucket::After, "appendix"),
            frag("base", FragmentBucket::Before, "base"),
            frag("reminder", FragmentBucket::Before, "reminder"),
        ];
        let out = assemble(&frags, &AssembleCtx::default());
        // Before fragments in declaration order, then After fragments.
        assert_eq!(
            out.system.as_deref(),
            Some("parts\n\nbase\n\nreminder\n\nappendix")
        );
    }

    #[test]
    fn replacement_root_preserves_exact_bytes_and_has_single_provenance_entry() {
        let out = replace("  exact replacement\n".to_string());
        assert_eq!(out.system.as_deref(), Some("  exact replacement\n"));
        assert_eq!(out.root(), PromptRoot::Replacement);
        assert_eq!(out.manifest().segments().len(), 1);
        assert_eq!(out.manifest().segments()[0].bytes, 20);
        assert_eq!(
            out.manifest().segments()[0].reason,
            "exclusive replacement root"
        );
        out.manifest().validate(out.system.as_deref()).unwrap();
    }

    #[test]
    fn empty_and_whitespace_bodies_are_excluded_with_reason() {
        let frags = vec![
            frag("a", FragmentBucket::Before, "  \n  "),
            frag("b", FragmentBucket::Before, "kept"),
        ];
        let out = assemble(&frags, &AssembleCtx::default());
        assert_eq!(out.system.as_deref(), Some("kept"));
        assert!(!out.manifest().segments()[0].included);
        assert_eq!(out.manifest().segments()[0].reason, "empty body");
        assert!(out.manifest().segments()[1].included);
    }

    #[test]
    fn requires_tools_gates_inclusion() {
        let gated = PromptFragment::new(
            "todo.guidance",
            "tool:todo",
            FragmentBucket::Before,
            "update the tracker",
        )
        .requiring_tools(vec!["todo".to_string()]);
        // Tool absent: excluded.
        let out = assemble(&[gated.clone()], &AssembleCtx::default());
        assert_eq!(out.system, None);
        assert!(!out.manifest().segments()[0].included);
        assert!(out.manifest().segments()[0]
            .reason
            .contains("requires tool `todo`"));
        // Tool present: included.
        let ctx = AssembleCtx {
            tool_names: BTreeSet::from(["todo".to_string()]),
            ..Default::default()
        };
        let out = assemble(&[gated], &ctx);
        assert_eq!(out.system.as_deref(), Some("update the tracker"));
        assert!(out.manifest().segments()[0].included);
        assert!(out.manifest().segments()[0]
            .reason
            .contains("tool(s) present: todo"));
    }

    #[test]
    fn empty_fragment_set_yields_none() {
        let out = assemble(&[], &AssembleCtx::default());
        assert_eq!(out.system, None);
        assert!(out.manifest().segments().is_empty());
        out.manifest().validate(out.system.as_deref()).unwrap();
    }

    #[test]
    fn duplicate_included_id_and_digest_is_rejected() {
        let repeated = frag("same", FragmentBucket::Before, "duplicate");
        let out = assemble(&[repeated.clone(), repeated], &AssembleCtx::default());
        let error = out
            .manifest()
            .validate(out.system.as_deref())
            .expect_err("duplicate included segment must fail");
        assert!(
            error.contains("duplicate included context segment"),
            "{error}"
        );
    }

    #[test]
    fn included_segment_digest_must_match_its_exact_prompt_slice() {
        let mut out = assemble(
            &[
                frag("first", FragmentBucket::Before, "alpha"),
                frag("second", FragmentBucket::After, "omega"),
            ],
            &AssembleCtx::default(),
        );
        out.manifest.segments[0].digest =
            crate::llm::content_hash::stable_redacted_string_hash("wrong");
        let error = out
            .manifest()
            .validate(out.system.as_deref())
            .expect_err("tampered segment digest must fail");
        assert!(error.contains("does not match retained bytes"), "{error}");
    }

    #[test]
    fn replacement_root_with_a_surviving_segment_is_rejected() {
        let mut out = replace("replacement".to_string());
        out.manifest.segments.push(FragmentTrace {
            id: "leaked".to_string(),
            source: "test".to_string(),
            bucket: "before",
            included: true,
            reason: "deliberate leak".to_string(),
            bytes: 4,
            digest: crate::llm::content_hash::stable_redacted_string_hash("leak"),
        });
        out.system = Some("replacement\n\nleak".to_string());
        out.manifest.whole_prompt_digest =
            crate::llm::content_hash::stable_redacted_string_hash("replacement\n\nleak");
        out.manifest.system_prompt_bytes = "replacement\n\nleak".len();
        let error = out
            .manifest()
            .validate(out.system.as_deref())
            .expect_err("replacement root leak must fail");
        assert!(
            error.contains("replacement root must contain exactly one"),
            "{error}"
        );
    }
}
