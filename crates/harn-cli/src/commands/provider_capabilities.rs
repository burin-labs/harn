use std::fs;

use crate::cli::{
    ProviderArgs, ProviderCapabilitiesCommand, ProviderCapabilitiesPromoteFromEvalArgs,
    ProviderCommand,
};
use crate::commands::tool_mode_parity::{self, ToolModeParityOverlay, ToolModeParityOverlayRow};

pub(crate) fn run(args: ProviderArgs) -> Result<(), String> {
    match args.command {
        ProviderCommand::Capabilities(capabilities) => match capabilities.command {
            ProviderCapabilitiesCommand::Audit(audit) => run_audit(audit.json),
            ProviderCapabilitiesCommand::PromoteFromEval(promote) => {
                run_promote_from_eval(&promote)
            }
        },
    }
}

pub(crate) fn run_or_exit(args: ProviderArgs) {
    run(args).unwrap_or_else(|error| crate::command_error(&error));
}

fn run_audit(json: bool) -> Result<(), String> {
    let report = harn_vm::llm::capabilities::audit_catalogued_chat_model_tool_capabilities();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report)
                .map_err(|error| format!("failed to render capability audit JSON: {error}"))?
        );
    } else if report.ok() {
        println!("{}", report.render_human());
    } else {
        eprintln!("{}", report.render_human());
    }
    report
        .ok()
        .then_some(())
        .ok_or_else(|| "provider capability audit failed".to_string())
}

fn run_promote_from_eval(args: &ProviderCapabilitiesPromoteFromEvalArgs) -> Result<(), String> {
    let overlay = tool_mode_parity::read_overlay(&args.overlay_path)?;
    let current = fs::read_to_string(&args.catalog)
        .map_err(|error| format!("failed to read {}: {error}", args.catalog.display()))?;
    let updated = apply_overlay_to_catalog(&current, &overlay)?;
    if updated == current {
        println!(
            "provider capability catalog already matches {}",
            args.overlay_path.display()
        );
        return Ok(());
    }
    fs::write(&args.catalog, updated)
        .map_err(|error| format!("failed to write {}: {error}", args.catalog.display()))?;
    println!(
        "updated {} from {}",
        args.catalog.display(),
        args.overlay_path.display()
    );
    Ok(())
}

fn apply_overlay_to_catalog(raw: &str, overlay: &ToolModeParityOverlay) -> Result<String, String> {
    let mut rows = overlay.rows.clone();
    rows.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then_with(|| left.model.cmp(&right.model))
    });

    let mut out = raw.to_string();
    for row in &rows {
        if row.native.total_runs == 0 || row.text.total_runs == 0 {
            continue;
        }
        out = apply_overlay_row(&out, row)?;
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

fn apply_overlay_row(raw: &str, row: &ToolModeParityOverlayRow) -> Result<String, String> {
    let mut lines = raw.lines().map(str::to_string).collect::<Vec<_>>();
    let blocks = scan_blocks(&lines)?;
    let note = tool_mode_parity::render_promotion_note(row);

    if let Some(block) = blocks.iter().find(|block| {
        block.provider == row.provider
            && block
                .model_match
                .as_deref()
                .is_some_and(|model| model == row.model)
    }) {
        let mut block_lines = lines[block.start..block.end].to_vec();
        upsert_field(
            &mut block_lines,
            "native_tools",
            format!("native_tools = {}", true),
            &["model_match"],
        );
        upsert_field(
            &mut block_lines,
            "preferred_tool_format",
            format!(
                "preferred_tool_format = {}",
                toml_string(&row.preferred_tool_format)
            ),
            &["native_tools", "model_match"],
        );
        upsert_field(
            &mut block_lines,
            "tool_mode_parity",
            format!("tool_mode_parity = {}", toml_string(&row.tool_mode_parity)),
            &["preferred_tool_format", "native_tools", "model_match"],
        );
        upsert_field(
            &mut block_lines,
            "tool_mode_parity_notes",
            format!("tool_mode_parity_notes = {}", toml_string(&note)),
            &["tool_mode_parity", "preferred_tool_format", "native_tools"],
        );
        lines.splice(block.start..block.end, block_lines);
    } else {
        let insert_at = blocks
            .iter()
            .find(|block| block.provider == row.provider)
            .map(|block| block.start)
            .unwrap_or(lines.len());
        lines.splice(insert_at..insert_at, render_new_block(row, &note));
    }

    Ok(lines.join("\n"))
}

fn render_new_block(row: &ToolModeParityOverlayRow, note: &str) -> Vec<String> {
    vec![
        format!("[[provider.{}]]", row.provider),
        format!("model_match = {}", toml_string(&row.model)),
        "native_tools = true".to_string(),
        format!(
            "preferred_tool_format = {}",
            toml_string(&row.preferred_tool_format)
        ),
        format!("tool_mode_parity = {}", toml_string(&row.tool_mode_parity)),
        format!("tool_mode_parity_notes = {}", toml_string(note)),
        String::new(),
    ]
}

fn upsert_field(block_lines: &mut Vec<String>, key: &str, rendered: String, after_keys: &[&str]) {
    if let Some(index) = field_index(block_lines, key) {
        block_lines[index] = rendered;
        return;
    }

    let insert_after = after_keys
        .iter()
        .find_map(|candidate| field_index(block_lines, candidate))
        .unwrap_or(0);
    block_lines.insert(insert_after + 1, rendered);
}

fn field_index(lines: &[String], key: &str) -> Option<usize> {
    lines.iter().position(|line| {
        let trimmed = line.trim();
        trimmed.starts_with(key) && trimmed[key.len()..].trim_start().starts_with('=')
    })
}

#[derive(Debug, Clone)]
struct ProviderBlock {
    provider: String,
    start: usize,
    end: usize,
    model_match: Option<String>,
}

fn scan_blocks(lines: &[String]) -> Result<Vec<ProviderBlock>, String> {
    let mut starts = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("[[provider.") && trimmed.ends_with("]]") {
            starts.push((index, parse_provider_header(trimmed)?));
        }
    }

    let mut blocks = Vec::new();
    for (offset, (start, provider)) in starts.iter().enumerate() {
        let end = starts
            .get(offset + 1)
            .map(|(next, _)| *next)
            .unwrap_or(lines.len());
        let model_match = lines[*start..end]
            .iter()
            .find_map(|line| parse_string_field(line, "model_match"));
        blocks.push(ProviderBlock {
            provider: provider.clone(),
            start: *start,
            end,
            model_match,
        });
    }
    Ok(blocks)
}

fn parse_provider_header(line: &str) -> Result<String, String> {
    line.strip_prefix("[[provider.")
        .and_then(|rest| rest.strip_suffix("]]"))
        .map(str::to_string)
        .ok_or_else(|| format!("invalid provider capability header `{line}`"))
}

fn parse_string_field(line: &str, key: &str) -> Option<String> {
    let trimmed = line.trim();
    let remainder = trimmed.strip_prefix(key)?.trim_start();
    let raw = remainder.strip_prefix('=')?.trim();
    raw.parse::<toml::Value>()
        .ok()?
        .as_str()
        .map(str::to_string)
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlay_row() -> ToolModeParityOverlayRow {
        ToolModeParityOverlayRow {
            provider: "openrouter".to_string(),
            model: "qwen/qwen3-coder".to_string(),
            tool_mode_parity: "native_unreliable".to_string(),
            preferred_tool_format: "text".to_string(),
            confidence: "low".to_string(),
            sample_size: 5,
            last_updated: "2026-05-24T00:00:00Z".to_string(),
            evidence_path: ".harn-runs/coding-agent-bench/latest".to_string(),
            verifier_divergence_rate: 0.8,
            native: tool_mode_parity::ToolModeParityFormatStats {
                total_runs: 5,
                passed_runs: 1,
                unique_fixtures: 5,
                replicate_count: 1,
                pass_rate: 0.2,
            },
            text: tool_mode_parity::ToolModeParityFormatStats {
                total_runs: 5,
                passed_runs: 5,
                unique_fixtures: 5,
                replicate_count: 1,
                pass_rate: 1.0,
            },
        }
    }

    #[test]
    fn apply_overlay_inserts_specific_rule_before_wildcard() {
        let raw = r#"
[[provider.openrouter]]
model_match = "qwen/*"
native_tools = true
preferred_tool_format = "native"
"#;

        let overlay = ToolModeParityOverlay {
            schema_version: 1,
            generated_at: "2026-05-24T00:00:00Z".to_string(),
            fixture_suite: "coding-agent".to_string(),
            rows: vec![overlay_row()],
        };

        let updated = apply_overlay_to_catalog(raw, &overlay).expect("apply");
        let first = updated
            .lines()
            .find(|line| !line.trim().is_empty())
            .expect("first non-empty line");
        assert_eq!(first, "[[provider.openrouter]]");
        assert!(updated.contains("model_match = \"qwen/qwen3-coder\""));
        assert!(updated.contains("tool_mode_parity = \"native_unreliable\""));
        let wildcard_index = updated.find("model_match = \"qwen/*\"").expect("wildcard");
        let exact_index = updated
            .find("model_match = \"qwen/qwen3-coder\"")
            .expect("exact");
        assert!(exact_index < wildcard_index);
    }

    #[test]
    fn apply_overlay_updates_existing_exact_rule_idempotently() {
        let raw = r#"
[[provider.openrouter]]
model_match = "qwen/qwen3-coder"
native_tools = true
preferred_tool_format = "native"

[[provider.openrouter]]
model_match = "qwen/*"
native_tools = true
preferred_tool_format = "native"
"#;

        let overlay = ToolModeParityOverlay {
            schema_version: 1,
            generated_at: "2026-05-24T00:00:00Z".to_string(),
            fixture_suite: "coding-agent".to_string(),
            rows: vec![overlay_row()],
        };

        let updated = apply_overlay_to_catalog(raw, &overlay).expect("apply");
        assert!(updated.contains("preferred_tool_format = \"text\""));
        assert!(updated.contains("tool_mode_parity = \"native_unreliable\""));

        let updated_again = apply_overlay_to_catalog(&updated, &overlay).expect("reapply");
        assert_eq!(updated, updated_again);
    }

    #[test]
    fn promote_from_eval_skips_rows_without_both_formats() {
        let raw = r#"
[[provider.openrouter]]
model_match = "qwen/*"
native_tools = true
preferred_tool_format = "native"
"#;

        let mut row = overlay_row();
        row.native.total_runs = 0;
        let overlay = ToolModeParityOverlay {
            schema_version: 1,
            generated_at: "2026-05-24T00:00:00Z".to_string(),
            fixture_suite: "coding-agent".to_string(),
            rows: vec![row],
        };

        let updated = apply_overlay_to_catalog(raw, &overlay).expect("apply");
        assert!(!updated.contains("tool_mode_parity"));
        assert_eq!(updated.trim(), raw.trim());
    }

    #[test]
    fn run_promote_from_eval_updates_catalog_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let catalog = temp.path().join("capabilities.toml");
        let overlay_path = temp.path().join("overlay.toml");
        fs::write(
            &catalog,
            r#"
[[provider.openrouter]]
model_match = "qwen/*"
native_tools = true
preferred_tool_format = "native"
"#,
        )
        .expect("write catalog");
        tool_mode_parity::write_overlay(
            &overlay_path,
            &ToolModeParityOverlay {
                schema_version: 1,
                generated_at: "2026-05-24T00:00:00Z".to_string(),
                fixture_suite: "coding-agent".to_string(),
                rows: vec![overlay_row()],
            },
        )
        .expect("write overlay");

        run_promote_from_eval(&ProviderCapabilitiesPromoteFromEvalArgs {
            overlay_path,
            catalog: catalog.clone(),
        })
        .expect("promote");

        let updated = fs::read_to_string(&catalog).expect("read updated catalog");
        assert!(updated.contains("model_match = \"qwen/qwen3-coder\""));
        assert!(updated.contains("tool_mode_parity = \"native_unreliable\""));
    }
}
