//! Wrap a value-referenced callable so it can gain a capability parameter.
//!
//! Freezing (#6146) is correct when the registry still holds a bare name: the
//! invisible `handler(args)` call would shift arguments. The mechanical repair
//! is to replace each escaping reference with a closure that preserves the
//! pre-migration arity at the hand-over site:
//!
//! ```text
//! handler: web_search_handler
//! // becomes
//! handler: { args -> web_search_handler(args) }
//! ```
//!
//! The inner call deliberately omits the new capability argument. The ordinary
//! signature-threading pass then inserts it (`harness` / `harness.fs` / …)
//! using the same rules as every other static call, so the wrap cannot drift
//! from the carrier the body actually received.

use harn_lexer::{FixEdit, Span};

/// One bare-identifier read of a callable's value.
#[derive(Debug, Clone)]
pub(super) struct ValueReferenceSite {
    pub(super) name: String,
    pub(super) span: Span,
}

/// Build the closure that keeps `param_names` as the observable arity.
///
/// The site's source text must be exactly `callable_name` — anything else is
/// not a bare hand-over we can prove. The inner call lists only those original
/// parameters; capability threading fills the new leading argument afterward.
pub(super) fn wrap_value_reference_edit(
    source: &str,
    site: Span,
    callable_name: &str,
    param_names: &[String],
) -> Option<FixEdit> {
    let region = source.get(site.start..site.end)?;
    if region != callable_name {
        return None;
    }
    let params = param_names.join(", ");
    let replacement = if params.is_empty() {
        format!("{{ -> {callable_name}() }}")
    } else {
        format!("{{ {params} -> {callable_name}({params}) }}")
    };
    Some(FixEdit {
        span: site,
        replacement,
    })
}

/// Format escape sites for a freeze/decline message.
pub(super) fn format_escape_sites(sites: &[(String, usize)]) -> String {
    if sites.is_empty() {
        return String::new();
    }
    let rendered = sites
        .iter()
        .map(|(file, line)| format!("{file}:{line}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("; escaping reference(s) at {rendered}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_lexer::Span;

    #[test]
    fn wrap_keeps_declared_arity_in_the_closure() {
        let source = "handler: web_search_handler,";
        let start = source.find("web_search_handler").unwrap();
        let span = Span::with_offsets(start, start + "web_search_handler".len(), 1, 1);
        let edit =
            wrap_value_reference_edit(source, span, "web_search_handler", &["args".to_string()])
                .expect("wrap");
        assert_eq!(edit.replacement, "{ args -> web_search_handler(args) }");
    }
}
