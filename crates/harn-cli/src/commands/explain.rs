use std::str::FromStr;

use harn_parser::diagnostic_codes::Code;
use serde::Serialize;

use crate::cli::{CatalogFormat, ExplainArgs};
use crate::commands::diagnostics_catalog;
use crate::parse_source_file;

/// JSON envelope returned by `harn explain <CODE> --json`. Stable shape —
/// downstream tooling (LSPs, IDEs, hosted error pages, agents) can dispatch
/// on `schemaVersion` and `code` without parsing prose.
#[derive(Debug, Serialize)]
struct ExplainEnvelope<'a> {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    code: &'a str,
    category: &'a str,
    summary: &'a str,
    body: &'a str,
    /// Repair shapes available for this code, sourced from the per-code
    /// registry. Each entry carries the namespaced repair id, safety
    /// class, and a one-line summary; today the catalog binds at most
    /// one repair per code, so this is a `Vec` for forward compatibility.
    repairs: Vec<RepairEnvelope<'a>>,
    related: Vec<&'a str>,
    #[serde(rename = "apiStability")]
    api_stability: &'a str,
}

#[derive(Debug, Serialize)]
struct RepairEnvelope<'a> {
    id: &'a str,
    safety: &'a str,
    summary: &'a str,
}

pub(crate) fn run_explain(args: &ExplainArgs) -> i32 {
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

    if args.json {
        print_code_json(code)
    } else {
        print_code_text(code)
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

fn print_code_text(code: Code) -> i32 {
    println!("{} — {}", code, code.summary());
    println!();
    println!("{}", code.explanation().trim_end());
    if let Some(template) = code.repair_template() {
        println!();
        println!(
            "Repair: {} [{}] — {}",
            template.id, template.safety, template.summary
        );
    }
    let related = code.related();
    if !related.is_empty() {
        println!();
        println!("See also:");
        for other in related {
            println!("  - {} — {}", other, other.summary());
        }
    }
    0
}

fn build_envelope(code: Code) -> ExplainEnvelope<'static> {
    let related_strs: Vec<&'static str> = code.related().iter().map(|c| c.as_str()).collect();
    let repairs = code
        .repair_template()
        .map(|template| {
            vec![RepairEnvelope {
                id: template.id,
                safety: template.safety.as_str(),
                summary: template.summary,
            }]
        })
        .unwrap_or_default();
    ExplainEnvelope {
        schema_version: 1,
        code: code.as_str(),
        category: code.category().as_str(),
        summary: code.summary(),
        body: code.explanation(),
        repairs,
        related: related_strs,
        api_stability: "stable",
    }
}

fn print_code_json(code: Code) -> i32 {
    let envelope = build_envelope(code);
    match serde_json::to_string_pretty(&envelope) {
        Ok(json) => {
            println!("{json}");
            0
        }
        Err(err) => {
            eprintln!("failed to serialise explain envelope: {err}");
            1
        }
    }
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
                println!(
                    "No `{}` violations found for `{}` in {}.",
                    invariant, target, file
                );
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

    #[test]
    fn explain_code_text_succeeds_for_registered_code() {
        let code = run_explain(&args_for_code("HARN-TYP-014", false));
        assert_eq!(code, 0);
    }

    #[test]
    fn explain_code_fails_for_unknown_code() {
        let code = run_explain(&args_for_code("HARN-ZZZ-999", false));
        assert_eq!(code, 2);
    }

    #[test]
    fn explain_json_envelope_round_trips_schema_and_code() {
        let envelope =
            super::build_envelope(harn_parser::diagnostic_codes::Code::TypeParameterArity);
        let serialised = serde_json::to_string(&envelope).expect("serialise");
        let value: serde_json::Value = serde_json::from_str(&serialised).expect("parse json");
        assert_eq!(value["schemaVersion"], serde_json::json!(1));
        assert_eq!(value["code"], serde_json::json!("HARN-TYP-014"));
        assert_eq!(value["category"], serde_json::json!("TYP"));
        assert_eq!(value["apiStability"], serde_json::json!("stable"));
        assert!(
            value["body"]
                .as_str()
                .is_some_and(|b| b.contains("HARN-TYP-014")),
            "envelope body should reference the code, got: {value:?}"
        );
        assert!(value["related"].is_array(), "related is an array");
        let related: Vec<String> = value["related"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert!(related.contains(&"HARN-TYP-013".to_string()));
    }

    #[test]
    fn explain_every_registered_code_emits_valid_envelope() {
        use harn_parser::diagnostic_codes::Code;
        for entry in Code::registry() {
            let envelope = super::build_envelope(entry.code);
            let serialised =
                serde_json::to_string(&envelope).expect("serialise registered code envelope");
            let value: serde_json::Value =
                serde_json::from_str(&serialised).expect("parse registered code envelope");
            assert_eq!(value["schemaVersion"], serde_json::json!(1));
            assert_eq!(value["code"], serde_json::json!(entry.identifier));
            assert!(
                value["body"].as_str().is_some_and(|b| !b.trim().is_empty()),
                "{} has empty body in envelope",
                entry.identifier
            );
        }
    }

    #[test]
    fn explain_envelope_surfaces_repair_when_registered() {
        // HARN-OWN-001 (immutable assignment) is mapped to
        // `bindings/make-mutable` / scope-local in the registry. The
        // envelope must surface that so IDE clients and agents can
        // dispatch on safety without parsing prose.
        let envelope =
            super::build_envelope(harn_parser::diagnostic_codes::Code::ImmutableAssignment);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&envelope).unwrap()).unwrap();
        let repairs = value["repairs"]
            .as_array()
            .expect("repairs should always be an array");
        assert_eq!(repairs.len(), 1, "expected one registered repair");
        assert_eq!(repairs[0]["id"], "bindings/make-mutable");
        assert_eq!(repairs[0]["safety"], "scope-local");
    }

    #[test]
    fn explain_envelope_repairs_empty_when_no_template_registered() {
        // Parser errors don't have a registered repair shape (they're
        // diagnose-only). Envelope `repairs` should still be an empty
        // array, not absent — the schema contract is that the field
        // exists for every code.
        let envelope =
            super::build_envelope(harn_parser::diagnostic_codes::Code::ParserUnexpectedToken);
        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&envelope).unwrap()).unwrap();
        assert_eq!(value["repairs"], serde_json::json!([]));
    }

    #[test]
    fn explain_returns_zero_for_configured_violation() {
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
        });

        assert_eq!(code, 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explain_returns_nonzero_for_missing_handler() {
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
        });

        assert_eq!(code, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explain_catalog_json_succeeds() {
        let code = run_explain(&ExplainArgs {
            target: None,
            file: None,
            invariant: None,
            catalog: true,
            format: CatalogFormat::Json,
            json: false,
        });
        assert_eq!(code, 0);
    }

    #[test]
    fn explain_catalog_markdown_succeeds() {
        let code = run_explain(&ExplainArgs {
            target: None,
            file: None,
            invariant: None,
            catalog: true,
            format: CatalogFormat::Markdown,
            json: false,
        });
        assert_eq!(code, 0);
    }
}
