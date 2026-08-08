//! What a text document is, as far as this server is concerned.
//!
//! Every handler that reaches for the Harn lexer, parser, or formatter
//! is assuming it holds a Harn program. Prompt templates are a
//! different language that lives in the same workspace and is served by
//! the same server, and an editor may hand us documents that are
//! neither. Classifying once, here, keeps that decision from being
//! re-derived — differently — in each handler.

use tower_lsp::lsp_types::Url;

/// Language id for Harn programs.
pub(crate) const HARN_LANGUAGE_ID: &str = "harn";
/// Language id the Harn editor extensions use for prompt templates.
pub(crate) const PROMPT_LANGUAGE_ID: &str = "harn-prompt";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DocumentKind {
    /// A `.harn` program: lexed, parsed, type-checked, linted, formatted.
    Harn,
    /// A `.harn.prompt` or `.prompt` template. Parsed by the
    /// prompt-template engine and never by the Harn parser — Harn
    /// syntax has nothing to say about prompt text, so parsing one as
    /// the other produces noise rather than diagnostics.
    Prompt,
    /// Anything else the editor opened against us. Only the
    /// language-agnostic rule engine has an opinion about it.
    Other,
}

impl DocumentKind {
    pub(crate) fn classify(uri: &Url, language_id: &str) -> Self {
        let path = uri.path();
        // The file name wins for prompts. `.harn.prompt` and a bare
        // `.prompt` are the same language — the double extension exists
        // so a prompt sits next to the `.harn` file that renders it —
        // and neither can sensibly be a Harn program, so a client that
        // mislabels one must not talk us into running the Harn parser
        // over prompt text.
        if path.ends_with(".prompt") {
            return Self::Prompt;
        }
        match language_id {
            HARN_LANGUAGE_ID => Self::Harn,
            // An unsaved buffer has no informative path, so the
            // declared language id is all we have to go on.
            PROMPT_LANGUAGE_ID => Self::Prompt,
            // A client that doesn't know Harn's language ids — a
            // `plaintext` buffer, or a change notification for a
            // document we never saw opened — still gets the right
            // treatment from the file name.
            _ if path.ends_with(".harn") => Self::Harn,
            _ => Self::Other,
        }
    }

    /// Whether Harn's own lexer, parser, formatter and lint rules apply.
    pub(crate) fn is_harn(self) -> bool {
        self == Self::Harn
    }
}

#[cfg(test)]
mod tests {
    use super::DocumentKind;
    use tower_lsp::lsp_types::Url;

    fn classify(path: &str, language_id: &str) -> DocumentKind {
        DocumentKind::classify(
            &Url::parse(&format!("file:///w/{path}")).unwrap(),
            language_id,
        )
    }

    #[test]
    fn language_id_decides_when_the_client_knows_harn() {
        assert_eq!(classify("main.harn", "harn"), DocumentKind::Harn);
        assert_eq!(
            classify("greet.harn.prompt", "harn-prompt"),
            DocumentKind::Prompt
        );
    }

    #[test]
    fn file_name_decides_when_the_client_does_not() {
        assert_eq!(
            classify("greet.harn.prompt", "plaintext"),
            DocumentKind::Prompt
        );
        assert_eq!(classify("greet.prompt", ""), DocumentKind::Prompt);
        assert_eq!(classify("main.harn", ""), DocumentKind::Harn);
    }

    #[test]
    fn unrelated_documents_are_neither() {
        assert_eq!(classify("main.ts", "typescript"), DocumentKind::Other);
        assert_eq!(classify("README.md", "markdown"), DocumentKind::Other);
    }

    #[test]
    fn a_declared_prompt_is_never_parsed_as_harn() {
        // The extension declares `harn-prompt` for both spellings; a
        // client that mislabels one must not get the Harn pipeline.
        assert!(!classify("greet.prompt", "harn-prompt").is_harn());
        assert!(!classify("greet.harn.prompt", "harn").is_harn());
    }
}
