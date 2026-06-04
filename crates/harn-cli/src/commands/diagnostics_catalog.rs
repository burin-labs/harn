//! Diagnostic-code catalog renderer.
//!
//! Drives `harn explain --catalog [--format markdown|json|text]` and the
//! `make sync-diagnostics-catalog` / `make check-diagnostics-catalog`
//! drift gates. The in-binary registry from `harn_parser::diagnostic_codes`
//! is the single source of truth — this module formats it, never edits it.
//!
//! The JSON sidecar shape is the contract surface for downstream tooling
//! (burin-code IDE catalog page, harn-cloud hosted error pages). Bumping
//! the shape requires bumping `SCHEMA_VERSION` and coordinating the
//! consumers pinned to epic #1745.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use harn_parser::diagnostic_codes::{Category, Code, RegistryEntry};
use serde::Serialize;

/// JSON sidecar schema version. Increment on any breaking shape change.
pub const SCHEMA_VERSION: u32 = 1;

/// Top-level JSON envelope written to `docs/diagnostics-catalog.json`.
#[derive(Debug, Serialize)]
struct CatalogEnvelope<'a> {
    #[serde(rename = "schemaVersion")]
    schema_version: u32,
    /// Every category in registration order (`TYP`, `PAR`, …). Lets
    /// consumers render a deterministic table of contents without
    /// scanning every code.
    categories: Vec<CategoryEnvelope<'a>>,
    codes: Vec<CodeEnvelope<'a>>,
}

#[derive(Debug, Serialize)]
struct CategoryEnvelope<'a> {
    id: &'a str,
    title: &'a str,
    count: usize,
}

#[derive(Debug, Serialize)]
struct CodeEnvelope<'a> {
    code: &'a str,
    category: &'a str,
    summary: &'a str,
    repairs: Vec<RepairEnvelope<'a>>,
    related: Vec<&'a str>,
    #[serde(rename = "explanationPresent")]
    explanation_present: bool,
    #[serde(rename = "apiStability")]
    api_stability: &'static str,
}

#[derive(Debug, Serialize)]
struct RepairEnvelope<'a> {
    id: &'a str,
    safety: &'a str,
    summary: &'a str,
}

/// Diagnostic-catalog rendering surface.
pub fn render_markdown() -> String {
    Renderer::new().markdown()
}

pub fn render_json() -> String {
    Renderer::new().json()
}

pub fn render_text() -> String {
    Renderer::new().text()
}

struct Renderer {
    by_category: BTreeMap<Category, Vec<&'static RegistryEntry>>,
}

impl Renderer {
    fn new() -> Self {
        let mut by_category: BTreeMap<Category, Vec<&'static RegistryEntry>> = BTreeMap::new();
        for entry in Code::registry() {
            by_category.entry(entry.category).or_default().push(entry);
        }
        // Numerical sort within each category. Registry insertion order is
        // already by code suffix today, but sorting makes the output
        // independent of source-order edits.
        for entries in by_category.values_mut() {
            entries.sort_by_key(|entry| entry.identifier);
        }
        Self { by_category }
    }

    fn ordered_categories(
        &self,
    ) -> impl Iterator<Item = (&Category, &Vec<&'static RegistryEntry>)> {
        // Render categories in their `Category::ALL` order, not
        // BTreeMap's alphabetic order, so docs match the spec.
        Category::ALL
            .iter()
            .filter_map(|category| self.by_category.get_key_value(category))
    }

    fn json(&self) -> String {
        let categories = self
            .ordered_categories()
            .map(|(category, entries)| CategoryEnvelope {
                id: category.as_str(),
                title: category_title(*category),
                count: entries.len(),
            })
            .collect();
        let codes = self
            .ordered_categories()
            .flat_map(|(_, entries)| entries.iter().map(|entry| code_envelope(entry)))
            .collect();
        let envelope = CatalogEnvelope {
            schema_version: SCHEMA_VERSION,
            categories,
            codes,
        };
        // Pretty-print so diffs are reviewable; trailing newline matches
        // POSIX file convention.
        // CatalogEnvelope and CatalogEntry are tagged-string serde shapes —
        // no infallible path inputs, no skip_serializing_if blowups.
        let mut out = serde_json::to_string_pretty(&envelope)
            .expect("CatalogEnvelope contains only String/u8/enum fields");
        out.push('\n');
        out
    }

    fn text(&self) -> String {
        let mut out = String::new();
        for (category, entries) in self.ordered_categories() {
            for entry in entries {
                let repair = entry
                    .code
                    .repair_template()
                    .map(|template| format!(" [repair: {} ({})]", template.id, template.safety))
                    .unwrap_or_default();
                writeln!(
                    out,
                    "{} ({}) — {}{}",
                    entry.identifier,
                    category.as_str(),
                    entry.summary,
                    repair,
                )
                .ok();
            }
        }
        out
    }

    fn markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# Diagnostic codes\n\n");
        out.push_str("<!-- GENERATED by `harn explain --catalog --format markdown` -- do not edit by hand. -->\n");
        out.push_str("<!-- Source of truth: crates/harn-parser/src/diagnostic_codes.rs. Run `make sync-diagnostics-catalog` to regenerate. -->\n\n");
        out.push_str("<!-- markdownlint-disable MD013 MD024 -->\n\n");
        out.push_str(
            "Every diagnostic emitted by `harn check`, `harn lint`, and `harn fmt` carries a \
             stable `HARN-<CAT>-<NNN>` code. Codes are dispatchable: agents, IDEs, and the \
             hosted error pages all read the same `apiStability: stable` contract, so cross-\
             tooling integrations never have to regex on prose.\n\n",
        );
        out.push_str("Look up a single code interactively:\n\n");
        out.push_str("```sh\n");
        out.push_str("harn explain HARN-TYP-014\n");
        out.push_str("harn explain HARN-TYP-014 --json\n");
        out.push_str("```\n\n");
        out.push_str(
            "The structured JSON sidecar that drives this page is committed at \
             [`docs/diagnostics-catalog.json`](https://github.com/burin-labs/harn/blob/main/docs/diagnostics-catalog.json) — its \
             `schemaVersion: 1` shape is the contract consumed by downstream tooling \
             (burin-code's IDE diagnostic panel, harn-cloud's hosted error pages). \
             Regenerate locally with `make sync-diagnostics-catalog`.\n\n",
        );
        out.push_str(
            "Prose-style tour of common shape and nilable diagnostics: \
             [Reading shape diagnostics](./reading-shape-diagnostics.md).\n\n",
        );

        out.push_str("## Repair safety classes\n\n");
        out.push_str("Repairs are tagged with a six-level safety class so `harn fix --apply --safety <ceiling>` and IDE auto-apply policies can dispatch without inspecting individual edits:\n\n");
        out.push_str("| Class | Meaning |\n");
        out.push_str("|---|---|\n");
        out.push_str("| `format-only` | Whitespace, trivia, or canonical layout only. Always safe to auto-apply. |\n");
        out.push_str(
            "| `behavior-preserving` | Intended not to change observable runtime behavior. |\n",
        );
        out.push_str("| `scope-local` | Confined to the current local scope or file; blast radius does not cross a public surface. |\n");
        out.push_str("| `surface-changing` | Touches a signature, export, or call-site surface other files can observe. |\n");
        out.push_str(
            "| `capability-changing` | Required capabilities or sandbox profile may change. |\n",
        );
        out.push_str("| `needs-human` | Planning hint only — propose, never auto-apply. |\n\n");

        out.push_str("## Categories\n\n");
        out.push_str("| Category | Title | Codes |\n");
        out.push_str("|---|---|---:|\n");
        for (category, entries) in self.ordered_categories() {
            writeln!(
                out,
                "| [`{}`](#{}) | {} | {} |",
                category.as_str(),
                category_anchor(*category),
                category_title(*category),
                entries.len(),
            )
            .ok();
        }
        out.push('\n');

        for (category, entries) in self.ordered_categories() {
            writeln!(
                out,
                "## {} — {}",
                category.as_str(),
                category_title(*category)
            )
            .ok();
            out.push('\n');
            if let Some(intro) = category_intro(*category) {
                out.push_str(intro);
                out.push_str("\n\n");
            }
            out.push_str("| Code | Summary | Repair | Safety |\n");
            out.push_str("|---|---|---|---|\n");
            for entry in entries {
                let template = entry.code.repair_template();
                let (repair_cell, safety_cell) = match template {
                    Some(template) => (
                        format!("`{}`", template.id),
                        format!("`{}`", template.safety),
                    ),
                    None => ("—".to_string(), "—".to_string()),
                };
                writeln!(
                    out,
                    "| [`{}`](#{}) | {} | {} | {} |",
                    entry.identifier,
                    code_anchor(entry.identifier),
                    escape_pipe(entry.summary),
                    repair_cell,
                    safety_cell,
                )
                .ok();
            }
            out.push('\n');
        }

        out.push_str("## Code reference\n\n");
        for (_, entries) in self.ordered_categories() {
            for entry in entries {
                writeln!(out, "### `{}`", entry.identifier).ok();
                out.push('\n');
                writeln!(
                    out,
                    "**Category:** `{}` ({}) &nbsp;·&nbsp; **API stability:** `stable`",
                    entry.category,
                    category_title(entry.category),
                )
                .ok();
                out.push('\n');
                writeln!(out, "{}", entry.summary).ok();
                out.push('\n');

                if let Some(template) = entry.code.repair_template() {
                    writeln!(
                        out,
                        "- **Repair:** `{}` &nbsp;·&nbsp; **Safety:** `{}`",
                        template.id, template.safety
                    )
                    .ok();
                    writeln!(out, "- {}", template.summary).ok();
                }
                let related = entry.code.related();
                if !related.is_empty() {
                    let mut links = String::new();
                    for (index, other) in related.iter().enumerate() {
                        if index > 0 {
                            links.push_str(", ");
                        }
                        write!(
                            links,
                            "[`{}`](#{})",
                            other.as_str(),
                            code_anchor(other.as_str())
                        )
                        .ok();
                    }
                    writeln!(out, "- **See also:** {links}").ok();
                }
                out.push('\n');

                // Embed the full markdown explanation, demoting its headings
                // so the page TOC stays well-formed. The first heading in
                // every explanation file echoes the code identifier and
                // summary — we already emitted that under our own
                // `### <code>` anchor, so drop the dupe and shift the rest
                // by two levels (`##` → `####`, …).
                let body = strip_leading_heading(entry.code.explanation());
                out.push_str(&demote_markdown_headings(body, 2));
                if !out.ends_with("\n\n") {
                    if !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push('\n');
                }
            }
        }
        collapse_blank_runs(&out)
    }
}

/// Collapse runs of three or more consecutive newlines down to two so the
/// concatenated explanation files don't trip `MD012/no-multiple-blanks`.
/// Standardises trailing newlines to exactly one.
fn collapse_blank_runs(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut newline_run = 0usize;
    for ch in source.chars() {
        if ch == '\n' {
            newline_run += 1;
            if newline_run <= 2 {
                out.push('\n');
            }
        } else {
            newline_run = 0;
            out.push(ch);
        }
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn code_envelope(entry: &RegistryEntry) -> CodeEnvelope<'static> {
    let repairs = entry
        .code
        .repair_template()
        .map(|template| {
            vec![RepairEnvelope {
                id: template.id,
                safety: template.safety.as_str(),
                summary: template.summary,
            }]
        })
        .unwrap_or_default();
    CodeEnvelope {
        code: entry.identifier,
        category: entry.category.as_str(),
        summary: entry.summary,
        repairs,
        related: entry.code.related().iter().map(|c| c.as_str()).collect(),
        explanation_present: !entry.code.explanation().trim().is_empty(),
        api_stability: "stable",
    }
}

/// One-paragraph orientation shown once at the top of each category section.
/// Returns `None` for categories whose codes are diverse enough that a single
/// preamble would mislead. Hoisting this here keeps per-code explanation
/// files focused on what is unique to each code.
const fn category_intro(category: Category) -> Option<&'static str> {
    let intro = match category {
        Category::Typ => "Harn's static type checker rejects programs whose types do not unify. \
            Type errors block compilation — Harn refuses to run a program until they are fixed.",
        Category::Par => "The lexer or parser raises these before type checking begins. Harn cannot \
            build an AST from the source until the offending token sequence is repaired.",
        Category::Nam => "Name resolution failed: the identifier, field, or attribute referenced does \
            not match anything in the visible scope. Harn cannot proceed without a binding.",
        Category::Cap => "A host capability call (file I/O, network, HITL approval, tool host, etc.) \
            failed static validation. Capabilities are the trust boundary between Harn scripts and \
            the embedding host, so checks are strict by design.",
        Category::Llm => "An `llm_call(...)` invocation violates the schema Harn enforces. \
            Schema-validated, provider-portable LLM calls are a load-bearing Harn contract; drift in \
            the options table is rejected at check time.",
        Category::Orc => "An orchestration construct — agent / workflow / pipeline / tool definition, \
            or a call to an orchestration builtin — is shaped in a way the orchestrator cannot \
            accept.",
        Category::Std => "A stdlib symbol is used in a way Harn does not support, or has been \
            renamed/removed and the call site still references the old surface.",
        Category::Prm => "A prompt template (`.harn.prompt` / `.prompt`) failed validation, either \
            because its front matter is missing required fields or because the body references slots \
            the schema does not declare.",
        Category::Mod => "A module-level import declaration cannot be satisfied or has been authored \
            in a shape Harn rejects. Module boundaries are checked before the body is type-checked.",
        Category::Lnt => "Lints are not hard errors. The code compiles, but Harn flags the pattern as \
            likely-incorrect, unidiomatic, or risky in a production agent. Most lints can be auto-fixed.",
        Category::Fmt => "The formatter could not produce a canonical layout — either the input \
            contains a construct it does not know how to render, or a layout rule was violated in a \
            way auto-fix cannot resolve.",
        Category::Imp => "Import resolution failed at a deeper layer than `MOD` — the file, symbol, or \
            package referenced in an import declaration could not be located, parsed, or exposed.",
        Category::Own => "Harn's binding-and-mutability discipline rejects this usage. `let` bindings \
            may not be reassigned; `mut` bindings should actually be reassigned somewhere.",
        Category::Rcv => "A recovery construct (`try`, `rescue`) is in an invalid position or shaped \
            in a way Harn's error-recovery rules cannot accept.",
        Category::Mat => "A `match` expression is incomplete, ambiguous, or otherwise invalid. \
            Harn requires arms to cover every variant of the scrutinee type — partial matches must \
            opt in with an explicit catch-all.",
        Category::Met => "A `const` binding's right-hand side must be a pure expression evaluable at \
            compile time under the const-eval sandbox. These codes flag constructs the sandbox \
            rejects.",
        Category::Cst => "The bounded const-eval sandbox enforces step, recursion, and capability \
            limits on every `const` initializer so a hostile or accidental expression cannot stall \
            the compiler.",
        Category::Pol => "A runtime policy (pool backpressure, scheduling, quotas) rejected the \
            attempt. Policies are configurable, but defaults are tuned for safety over throughput.",
        Category::Rmd => "Reminder lifecycle errors are raised by `session/remind` and friends when \
            the payload, tags, or scheduling do not match the documented contract.",
        Category::Sus => "Suspend / resume lifecycle errors are raised when a worker is suspended, \
            resumed, or queried outside the lifecycle states the operation supports.",
        Category::Cmp => "The bytecode compiler rejected a program that parsed and type-checked. \
            These are structural / codegen errors the type checker does not model — `harn check` \
            runs the compile pass too, so anything that would stop `harn run` is reported up front.",
    };
    Some(intro)
}

/// Human-readable title for a diagnostic category. Used in the markdown
/// catalog headers and the JSON sidecar's `categories[]` entries.
const fn category_title(category: Category) -> &'static str {
    match category {
        Category::Typ => "Type checker",
        Category::Par => "Parser / lexer",
        Category::Nam => "Naming and resolution",
        Category::Cap => "Capabilities",
        Category::Llm => "LLM calls",
        Category::Orc => "Orchestration constructs",
        Category::Std => "Stdlib usage",
        Category::Prm => "Prompt templates",
        Category::Mod => "Modules and exports",
        Category::Rmd => "Reminder lifecycle",
        Category::Sus => "Suspend / resume lifecycle",
        Category::Lnt => "Lint rules",
        Category::Fmt => "Formatter",
        Category::Imp => "Import resolution",
        Category::Own => "Ownership and mutability",
        Category::Rcv => "Error recovery",
        Category::Mat => "Match exhaustiveness",
        Category::Pol => "Runtime policies",
        Category::Met => "Compile-time meta restrictions",
        Category::Cst => "Const-eval sandbox",
        Category::Cmp => "Bytecode compilation",
    }
}

fn category_anchor(category: Category) -> String {
    format!(
        "{}--{}",
        category.as_str().to_lowercase(),
        category_title(category)
            .to_lowercase()
            .replace(' ', "-")
            .replace('/', "")
    )
}

fn code_anchor(identifier: &str) -> String {
    identifier.to_lowercase()
}

fn escape_pipe(value: &str) -> String {
    value.replace('|', "\\|")
}

/// Drop the leading `#` heading line from a markdown chunk plus any
/// trailing blank line, so embedding the file under our own heading
/// does not produce a duplicate title. Returns the original source
/// untouched when the first non-blank line is not a heading.
fn strip_leading_heading(source: &str) -> &str {
    for (idx, ch) in source.char_indices() {
        if ch == '\n' || ch == '\r' {
            continue;
        }
        if ch != '#' {
            return source;
        }
        // Found the start of a heading line — advance past the newline.
        let mut end = idx;
        for (next_idx, next_ch) in source[idx..].char_indices() {
            end = idx + next_idx + next_ch.len_utf8();
            if next_ch == '\n' {
                break;
            }
        }
        // Skip a single blank line that follows the heading, if present.
        if source[end..].starts_with('\n') {
            end += 1;
        }
        return &source[end..];
    }
    source
}

/// Demote every `#`-prefixed heading in a markdown chunk by `levels` so
/// embedding it under an outer heading stays well-formed. Lines inside
/// fenced code blocks are passed through untouched.
fn demote_markdown_headings(source: &str, levels: usize) -> String {
    let mut out = String::with_capacity(source.len() + 16);
    let mut in_fence = false;
    for line in source.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            out.push_str(line);
            out.push('\n');
            continue;
        }
        if !in_fence && trimmed.starts_with('#') {
            let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
            if (1..=6).contains(&hashes) {
                let prefix_len = line.len() - trimmed.len();
                out.push_str(&line[..prefix_len]);
                for _ in 0..(hashes + levels).min(6) {
                    out.push('#');
                }
                out.push_str(&trimmed[hashes..]);
                out.push('\n');
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_envelope_round_trips_schema_version() {
        let json = render_json();
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse catalog json");
        assert_eq!(value["schemaVersion"], serde_json::json!(1));
        assert!(
            value["codes"]
                .as_array()
                .expect("codes array")
                .iter()
                .all(|entry| entry["apiStability"] == serde_json::json!("stable")),
            "every code envelope must declare apiStability=stable"
        );
    }

    #[test]
    fn json_envelope_includes_every_registered_code() {
        let json = render_json();
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse catalog json");
        let codes = value["codes"].as_array().expect("codes array");
        assert_eq!(codes.len(), Code::ALL.len());
        let identifiers: std::collections::HashSet<&str> = codes
            .iter()
            .map(|entry| entry["code"].as_str().unwrap())
            .collect();
        for entry in Code::registry() {
            assert!(
                identifiers.contains(entry.identifier),
                "catalog json missing {}",
                entry.identifier
            );
        }
    }

    #[test]
    fn json_envelope_lists_every_category_with_count() {
        let json = render_json();
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse catalog json");
        let categories = value["categories"].as_array().expect("categories array");
        assert_eq!(categories.len(), Category::ALL.len());
        for envelope in categories {
            let id = envelope["id"].as_str().unwrap();
            let count = envelope["count"].as_u64().unwrap();
            let expected = Code::registry()
                .iter()
                .filter(|entry| entry.category.as_str() == id)
                .count() as u64;
            assert_eq!(count, expected, "category {id} count drifted");
        }
    }

    #[test]
    fn json_envelope_surfaces_repair_safety() {
        let json = render_json();
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse catalog json");
        let entry = value["codes"]
            .as_array()
            .expect("codes array")
            .iter()
            .find(|entry| entry["code"] == serde_json::json!("HARN-OWN-001"))
            .expect("HARN-OWN-001 present");
        let repairs = entry["repairs"].as_array().expect("repairs array");
        assert_eq!(repairs.len(), 1);
        assert_eq!(repairs[0]["id"], serde_json::json!("bindings/make-mutable"));
        assert_eq!(repairs[0]["safety"], serde_json::json!("scope-local"));
    }

    #[test]
    fn markdown_renders_every_code_anchor() {
        let markdown = render_markdown();
        for entry in Code::registry() {
            let anchor = format!("### `{}`", entry.identifier);
            assert!(
                markdown.contains(&anchor),
                "markdown missing per-code section for {}",
                entry.identifier
            );
        }
    }

    #[test]
    fn markdown_starts_with_generated_banner() {
        let markdown = render_markdown();
        assert!(markdown.starts_with("# Diagnostic codes\n"));
        assert!(markdown.contains("<!-- GENERATED by"));
    }

    #[test]
    fn text_renders_one_line_per_code() {
        let text = render_text();
        let registry_count = Code::registry().len();
        assert_eq!(text.lines().count(), registry_count);
        for entry in Code::registry() {
            assert!(
                text.contains(entry.identifier),
                "text catalog missing {}",
                entry.identifier
            );
        }
    }

    #[test]
    fn strip_leading_heading_removes_first_heading_and_blank() {
        let body = "# HARN-TYP-014 — title\n\nFirst paragraph.\n";
        assert_eq!(strip_leading_heading(body), "First paragraph.\n");
    }

    #[test]
    fn strip_leading_heading_is_noop_when_first_line_is_prose() {
        let body = "Just some prose.\n# heading later\n";
        assert_eq!(strip_leading_heading(body), body);
    }

    #[test]
    fn heading_demotion_skips_code_fences() {
        let input = "# top\n\n```sh\n# not a heading\n```\n\n## inner\n";
        let demoted = demote_markdown_headings(input, 2);
        assert!(demoted.contains("### top"));
        // The hash inside the fenced block stays untouched.
        assert!(demoted.contains("# not a heading"));
        assert!(demoted.contains("#### inner"));
    }
}
