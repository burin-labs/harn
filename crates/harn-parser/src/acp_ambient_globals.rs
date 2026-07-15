//! Canonical registry of the ambient globals the ACP session executor binds
//! into every `session/prompt` VM before running a pipeline (see harn-serve's
//! `adapters/acp/execute.rs`).
//!
//! Both the type checker's ambient-root whitelist (`typechecker::scope`) and the
//! executor's `set_global` loop derive their names from this one list, so the
//! two cannot drift. That drift is the concrete failure this registry prevents:
//! if the executor injects a global the checker does not whitelist, `harn check`
//! reports an unresolved-identifier error (`HARN-NAM-001`) for a reference to a
//! global that genuinely exists at runtime.

/// A global value the ACP session executor binds before running a pipeline.
///
/// Adding a variant is a deliberate, load-bearing change: the executor's
/// exhaustive `match` will not compile until it constructs a value for the new
/// global, and the checker whitelist picks it up automatically through
/// [`AcpAmbientGlobal::ALL`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcpAmbientGlobal {
    /// The user prompt as plain text (`prompt`).
    Prompt,
    /// The raw ACP content blocks of the prompt (`prompt_content`), including
    /// multimodal image/file blocks that a pipeline forwards as the model's
    /// initial user-turn content.
    PromptContent,
    /// The prompt rendered as chat messages (`prompt_messages`).
    PromptMessages,
    /// The working directory the pipeline runs in (`cwd`).
    Cwd,
    /// Host MCP client handles keyed by server name (`mcp`).
    Mcp,
}

impl AcpAmbientGlobal {
    /// Every injected global, in injection order. Both the checker whitelist and
    /// the executor iterate this list, so it is the single source of truth for
    /// which identifiers are bound at ACP session-prompt time.
    pub const ALL: [AcpAmbientGlobal; 5] = [
        Self::Prompt,
        Self::PromptContent,
        Self::PromptMessages,
        Self::Cwd,
        Self::Mcp,
    ];

    /// The identifier the executor binds and the checker whitelists.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::PromptContent => "prompt_content",
            Self::PromptMessages => "prompt_messages",
            Self::Cwd => "cwd",
            Self::Mcp => "mcp",
        }
    }

    /// The Harn primitive type the checker assigns to references of this global,
    /// matching the value the executor binds.
    pub const fn checker_type(self) -> &'static str {
        match self {
            Self::Prompt | Self::Cwd => "string",
            Self::PromptContent | Self::PromptMessages => "list",
            Self::Mcp => "dict",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_lists_every_variant_exactly_once() {
        // The exhaustive match forces a new variant to be handled here; the
        // assertions then force it into `ALL`, the list both the checker and the
        // executor consume. A variant missing from `ALL` would be silently
        // neither whitelisted nor injected.
        for variant in [
            AcpAmbientGlobal::Prompt,
            AcpAmbientGlobal::PromptContent,
            AcpAmbientGlobal::PromptMessages,
            AcpAmbientGlobal::Cwd,
            AcpAmbientGlobal::Mcp,
        ] {
            let count = AcpAmbientGlobal::ALL
                .iter()
                .filter(|candidate| **candidate == variant)
                .count();
            assert_eq!(
                count,
                1,
                "{} must appear in ALL exactly once",
                variant.name()
            );
        }
    }

    #[test]
    fn names_are_unique() {
        for (index, global) in AcpAmbientGlobal::ALL.iter().enumerate() {
            for other in &AcpAmbientGlobal::ALL[index + 1..] {
                assert_ne!(global.name(), other.name(), "duplicate ambient global name");
            }
        }
    }
}
