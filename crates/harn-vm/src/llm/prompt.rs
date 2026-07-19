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
//! Every fragment is recorded in the returned [`AssembledPrompt::provenance`]
//! — included or excluded, with the reason — so the final prompt is fully
//! auditable: you can answer "why is this sentence here?" and "what would the
//! prompt look like without tool X?" without reverse-engineering a concat.

use std::collections::BTreeSet;

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
}

/// Result of [`assemble`]: the system string (if any) plus full provenance
/// for every fragment that was considered.
#[derive(Clone, Debug, Default)]
pub struct AssembledPrompt {
    pub system: Option<String>,
    pub provenance: Vec<FragmentTrace>,
}

impl AssembledPrompt {
    /// Provenance serialized for the `prompt_explain` builtin / CLI and for
    /// transcript audit metadata.
    pub fn provenance_json(&self) -> serde_json::Value {
        serde_json::json!({
            "fragments": self.provenance,
            "included": self.provenance.iter().filter(|t| t.included).count(),
            "excluded": self.provenance.iter().filter(|t| !t.included).count(),
        })
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

    for frag in fragments {
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
        Some(before.join("\n\n"))
    };
    AssembledPrompt { system, provenance }
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
    fn empty_and_whitespace_bodies_are_excluded_with_reason() {
        let frags = vec![
            frag("a", FragmentBucket::Before, "  \n  "),
            frag("b", FragmentBucket::Before, "kept"),
        ];
        let out = assemble(&frags, &AssembleCtx::default());
        assert_eq!(out.system.as_deref(), Some("kept"));
        assert!(!out.provenance[0].included);
        assert_eq!(out.provenance[0].reason, "empty body");
        assert!(out.provenance[1].included);
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
        assert!(!out.provenance[0].included);
        assert!(out.provenance[0].reason.contains("requires tool `todo`"));
        // Tool present: included.
        let ctx = AssembleCtx {
            tool_names: BTreeSet::from(["todo".to_string()]),
            ..Default::default()
        };
        let out = assemble(&[gated], &ctx);
        assert_eq!(out.system.as_deref(), Some("update the tracker"));
        assert!(out.provenance[0].included);
        assert!(out.provenance[0].reason.contains("tool(s) present: todo"));
    }

    #[test]
    fn empty_fragment_set_yields_none() {
        let out = assemble(&[], &AssembleCtx::default());
        assert_eq!(out.system, None);
        assert!(out.provenance.is_empty());
    }
}
