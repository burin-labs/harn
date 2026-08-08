use std::str::FromStr;

use harn_parser::diagnostic_codes::Code;
use serde::Serialize;

use crate::cli::{CatalogFormat, ExplainArgs};
use crate::commands::diagnostics_catalog;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;
use crate::parse_source_file;

/// Env var the embedded `cli/explain` script reads to find the
/// pre-serialized diagnostic entry. The dispatch shim does the
/// `Code::from_str` lookup in Rust (rather than exposing the diagnostic
/// registry as a new VM builtin) and hands the result off as JSON.
const EXPLAIN_ENTRY_ENV: &str = "HARN_EXPLAIN_ENTRY_JSON";

/// Repair shape available for a diagnostic code, sourced from the
/// per-code registry: the namespaced repair id, safety class, and a
/// one-line summary.
#[derive(Debug, Serialize)]
struct RepairEnvelope<'a> {
    id: &'a str,
    safety: &'a str,
    summary: &'a str,
}

/// Run `harn explain`. The single-code render path dispatches to the
/// embedded `cli/explain.harn` script. `--catalog` and `--invariant`
/// remain Rust handlers by design: catalog is a codegen tool, and the
/// invariant explainer reaches parser/control-flow internals.
pub(crate) async fn run_explain(args: &ExplainArgs) -> i32 {
    if args.catalog {
        return run_catalog(args.format);
    }

    if let Some(invariant) = &args.invariant {
        return run_invariant_explain(invariant, args);
    }

    // Without `--catalog` or `--invariant`, a target diagnostic code is
    // required. Clap's ArgGroup enforces presence in the catalog-or-target
    // dimension, so this branch only fires when neither catalog nor an
    // invariant lookup was requested but the positional was supplied.
    let Some(target) = args.target.as_deref() else {
        eprintln!(
            "`harn explain` needs a diagnostic code (e.g. `harn explain HARN-TYP-014`), \
             `--catalog`, or `--invariant <NAME> <FN> <FILE>`."
        );
        return 2;
    };
    let code = match Code::from_str(target) {
        Ok(code) => code,
        Err(_) => {
            eprintln!(
                "Unknown Harn diagnostic code `{target}`. Expected `HARN-<CAT>-<NNN>` (e.g. `HARN-TYP-014`).",
            );
            eprintln!("Run `harn explain HARN-TYP-001` for an example, or `harn explain --catalog` for the full registry.");
            return 2;
        }
    };

    run_explain_dispatch(code, args.json).await
}

/// Run the embedded `cli/explain.harn` script for the single-code render
/// path. The shim pre-serialises the diagnostic entry as JSON and
/// forwards it via [`EXPLAIN_ENTRY_ENV`] — the script just parses and
/// renders, so the diagnostic registry stays the Rust side's single
/// source of truth without needing a new VM builtin.
async fn run_explain_dispatch(code: Code, json: bool) -> i32 {
    let payload = build_entry_payload(code);
    let entry_json = match serde_json::to_string(&payload) {
        Ok(json) => json,
        Err(error) => {
            eprintln!("failed to serialise diagnostic entry: {error}");
            return 1;
        }
    };
    let _entry = ScopedEnvVar::set(EXPLAIN_ENTRY_ENV, &entry_json);
    dispatch::dispatch_to_embedded_script("explain", Vec::new(), json).await
}

/// Per-entry payload the `.harn` script renders. Kept structurally
/// distinct from the user-visible `harn explain --json` envelope (which
/// the script assembles) so the wire format the script sees can evolve
/// independently of that public JSON contract.
#[derive(Debug, Serialize)]
struct EntryPayload<'a> {
    code: &'a str,
    category: &'a str,
    summary: &'a str,
    explanation: &'a str,
    repair: Option<RepairEnvelope<'a>>,
    related: Vec<RelatedEntry<'a>>,
}

#[derive(Debug, Serialize)]
struct RelatedEntry<'a> {
    code: &'a str,
    summary: &'a str,
}

fn build_entry_payload(code: Code) -> EntryPayload<'static> {
    let repair = code.repair_template().map(|template| RepairEnvelope {
        id: template.id,
        safety: template.safety.as_str(),
        summary: template.summary,
    });
    let related = code
        .related()
        .iter()
        .map(|other| RelatedEntry {
            code: other.as_str(),
            summary: other.summary(),
        })
        .collect();
    EntryPayload {
        code: code.as_str(),
        category: code.category().as_str(),
        summary: code.summary(),
        explanation: code.explanation(),
        repair,
        related,
    }
}

fn run_catalog(format: CatalogFormat) -> i32 {
    let rendered = match format {
        CatalogFormat::Markdown => diagnostics_catalog::render_markdown(),
        CatalogFormat::Json => diagnostics_catalog::render_json(),
        CatalogFormat::Text => diagnostics_catalog::render_text(),
    };
    print!("{rendered}");
    0
}

fn run_invariant_explain(invariant: &str, args: &ExplainArgs) -> i32 {
    let Some(target) = args.target.as_deref() else {
        eprintln!(
            "`--invariant` requires a handler / function / tool / pipeline name and a file path. Usage: `harn explain --invariant <NAME> <FUNCTION> <FILE>`"
        );
        return 2;
    };
    let Some(file) = args.file.as_deref() else {
        eprintln!(
            "`--invariant` requires both a function name and a path. Usage: `harn explain --invariant <NAME> <FUNCTION> <FILE>`"
        );
        return 2;
    };
    let (_, program) = parse_source_file(file);
    match harn_ir::explain_handler_invariant(&program, target, invariant) {
        Ok(diagnostics) => {
            if diagnostics.is_empty() {
                println!("No `{invariant}` violations found for `{target}` in {file}.");
                return 0;
            }
            for diagnostic in diagnostics {
                println!(
                    "{}: {} ({})",
                    diagnostic.invariant, diagnostic.message, diagnostic.handler
                );
                for (index, step) in diagnostic.path.iter().enumerate() {
                    println!(
                        "  {}. {}:{}:{} {}",
                        index + 1,
                        file,
                        step.span.line,
                        step.span.column,
                        step.label
                    );
                }
                if let Some(help) = &diagnostic.help {
                    println!("  help: {help}");
                }
            }
            0
        }
        Err(message) => {
            eprintln!("{message}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::run_explain;
    use crate::cli::{CatalogFormat, ExplainArgs};
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_dir(prefix: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("{prefix}-{pid}-{n}"))
    }

    fn args_for_code(code: &str, json: bool) -> ExplainArgs {
        ExplainArgs {
            target: Some(code.to_string()),
            file: None,
            invariant: None,
            catalog: false,
            format: CatalogFormat::Markdown,
            json,
        }
    }

    #[tokio::test]
    async fn explain_code_text_succeeds_for_registered_code() {
        let code = run_explain(&args_for_code("HARN-TYP-014", false)).await;
        assert_eq!(code, 0);
    }

    #[tokio::test]
    async fn explain_code_fails_for_unknown_code() {
        let code = run_explain(&args_for_code("HARN-ZZZ-999", false)).await;
        assert_eq!(code, 2);
    }

    #[tokio::test]
    async fn explain_returns_zero_for_configured_violation() {
        let dir = unique_temp_dir("harn-explain");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.harn");
        std::fs::write(
            &file,
            r#"
@invariant("approval.reachability")
fn handler() {
  write_file("src/main.rs", "unsafe")
}
"#,
        )
        .unwrap();

        let code = run_explain(&ExplainArgs {
            target: Some("handler".to_string()),
            file: Some(file.to_string_lossy().into_owned()),
            invariant: Some("approval.reachability".to_string()),
            catalog: false,
            format: CatalogFormat::Markdown,
            json: false,
        })
        .await;

        assert_eq!(code, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn explain_returns_nonzero_for_missing_handler() {
        let dir = unique_temp_dir("harn-explain-missing");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("main.harn");
        std::fs::write(
            &file,
            r#"
fn handler() {
  log("ok")
}
"#,
        )
        .unwrap();

        let code = run_explain(&ExplainArgs {
            target: Some("missing".to_string()),
            file: Some(file.to_string_lossy().into_owned()),
            invariant: Some("approval.reachability".to_string()),
            catalog: false,
            format: CatalogFormat::Markdown,
            json: false,
        })
        .await;

        assert_eq!(code, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn explain_catalog_json_succeeds() {
        let code = run_explain(&ExplainArgs {
            target: None,
            file: None,
            invariant: None,
            catalog: true,
            format: CatalogFormat::Json,
            json: false,
        })
        .await;
        assert_eq!(code, 0);
    }

    #[tokio::test]
    async fn explain_catalog_markdown_succeeds() {
        let code = run_explain(&ExplainArgs {
            target: None,
            file: None,
            invariant: None,
            catalog: true,
            format: CatalogFormat::Markdown,
            json: false,
        })
        .await;
        assert_eq!(code, 0);
    }
}
