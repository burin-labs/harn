//! `workspace/executeCommand` handlers.
//!
//! Today the only command is `harn.applyRepair`, which resolves a
//! `data.repair_id` (as published on a code action / diagnostic) into the
//! `WorkspaceEdit` that applies it. The code-action handler precomputes
//! inline edits for the cheap lint/type repairs, so this path is the seam
//! for repairs whose fix is too expensive to precompute for every
//! diagnostic — the editor requests the edit on demand and applies the
//! result.

use serde_json::Value;
use tower_lsp::jsonrpc::Result;
use tower_lsp::lsp_types::*;

use super::formatting::build_code_actions;
use crate::source_text::SourceText;
use crate::HarnLsp;

/// Command name advertised in `ServerCapabilities::execute_command_provider`
/// and dispatched on in [`HarnLsp::handle_execute_command`].
pub(crate) const APPLY_REPAIR_COMMAND: &str = "harn.applyRepair";

impl HarnLsp {
    pub(super) async fn handle_execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> Result<Option<Value>> {
        if params.command != APPLY_REPAIR_COMMAND {
            return Ok(None);
        }

        let Some((uri, repair_id)) = parse_apply_repair_args(&params.arguments) else {
            return Ok(None);
        };

        let (source, diagnostics, lint_diags, type_diags, rule_diags) = {
            let docs = self.documents.lock().unwrap();
            let Some(state) = docs.get(&uri) else {
                return Ok(None);
            };
            (
                state.source.clone(),
                state.diagnostics.clone(),
                state.lint_diagnostics.clone(),
                state.type_diagnostics.clone(),
                state.rule_diagnostics.clone(),
            )
        };

        let edit = resolve_repair_edit(
            &uri,
            &source,
            &diagnostics,
            &lint_diags,
            &type_diags,
            &rule_diags,
            &repair_id,
        );

        // Returning the edit (rather than pushing it through a
        // `workspace/applyEdit` reverse request) keeps the client in control
        // of its safety gate — it can preview a `scope-local`+ repair before
        // touching any buffer.
        Ok(Some(match edit {
            Some(edit) => serde_json::to_value(edit).unwrap_or(Value::Null),
            None => Value::Null,
        }))
    }
}

/// Pull `{ "uri": ..., "repair_id": ... }` out of the command arguments.
fn parse_apply_repair_args(arguments: &[Value]) -> Option<(Url, String)> {
    let arg = arguments.first()?;
    let uri = Url::parse(arg.get("uri")?.as_str()?).ok()?;
    let repair_id = arg.get("repair_id")?.as_str()?.to_string();
    Some((uri, repair_id))
}

/// Rebuild the document's code actions and return the `WorkspaceEdit` of the
/// one whose `data.repair_id` matches `repair_id`.
///
/// Reusing `build_code_actions` (over the full stored diagnostic set) keeps a
/// single source of truth for how a repair maps to edits — the
/// `executeCommand` path and the inline code-action path can never drift.
fn resolve_repair_edit(
    uri: &Url,
    source: &SourceText,
    diagnostics: &[Diagnostic],
    lint_diags: &[harn_lint::LintDiagnostic],
    type_diags: &[harn_parser::TypeDiagnostic],
    rule_diags: &[crate::rules::RuleDiagnostic],
    repair_id: &str,
) -> Option<WorkspaceEdit> {
    let context = CodeActionContext {
        diagnostics: diagnostics.to_vec(),
        only: None,
        trigger_kind: None,
    };
    let actions = build_code_actions(uri, source, lint_diags, type_diags, rule_diags, &context);
    actions.into_iter().find_map(|action| match action {
        CodeActionOrCommand::CodeAction(action) => {
            let matches = action
                .data
                .as_ref()
                .and_then(|data| data.get("repair_id"))
                .and_then(|value| value.as_str())
                == Some(repair_id);
            matches.then_some(action.edit).flatten()
        }
        CodeActionOrCommand::Command(_) => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentState;
    use serde_json::json;

    fn doc_url() -> Url {
        Url::parse("file:///repair.harn").unwrap()
    }

    /// A `let` that is never reassigned trips the `mutable-never-reassigned`
    /// lint, whose repair rewrites the `let` keyword to `const`.
    fn make_immutable_repair_id(state: &DocumentState) -> String {
        state
            .lint_diagnostics
            .iter()
            .find(|ld| ld.rule == "mutable-never-reassigned")
            .and_then(|ld| ld.repair())
            .map(|repair| repair.id.as_str().to_string())
            .expect("var-never-reassigned source should publish a make-mutable repair")
    }

    #[test]
    fn resolve_repair_edit_returns_fix_for_known_repair_id() {
        let source = "fn main() {\n  let x = 1\n  print(x)\n}\n";
        let state = DocumentState::new(source.to_string());
        let repair_id = make_immutable_repair_id(&state);

        let edit = resolve_repair_edit(
            &doc_url(),
            &state.source,
            &state.diagnostics,
            &state.lint_diagnostics,
            &state.type_diagnostics,
            &state.rule_diagnostics,
            &repair_id,
        )
        .expect("known repair_id should resolve to a workspace edit");

        let changes = edit.changes.expect("edit should carry per-file changes");
        let text_edits = changes.get(&doc_url()).expect("edit targets the document");
        assert_eq!(text_edits.len(), 1);
        assert_eq!(text_edits[0].new_text, "const");
    }

    #[test]
    fn resolve_repair_edit_is_none_for_unknown_repair_id() {
        let source = "fn main() {\n  let x = 1\n  print(x)\n}\n";
        let state = DocumentState::new(source.to_string());

        let edit = resolve_repair_edit(
            &doc_url(),
            &state.source,
            &state.diagnostics,
            &state.lint_diagnostics,
            &state.type_diagnostics,
            &state.rule_diagnostics,
            "no/such-repair",
        );
        assert!(edit.is_none());
    }

    #[test]
    fn parse_apply_repair_args_reads_uri_and_repair_id() {
        let args =
            vec![json!({ "uri": "file:///repair.harn", "repair_id": "bindings/make-immutable" })];
        let (uri, repair_id) = parse_apply_repair_args(&args).expect("well-formed args parse");
        assert_eq!(uri, doc_url());
        assert_eq!(repair_id, "bindings/make-immutable");
    }

    #[test]
    fn parse_apply_repair_args_rejects_missing_fields() {
        assert!(parse_apply_repair_args(&[]).is_none());
        assert!(parse_apply_repair_args(&[json!({ "uri": "file:///x.harn" })]).is_none());
        assert!(parse_apply_repair_args(&[json!({ "repair_id": "x" })]).is_none());
    }
}
