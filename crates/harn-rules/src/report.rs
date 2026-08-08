//! Data tables — rules emit structured findings/metrics (#2837).
//!
//! Adopted from OpenRewrite's first-class data tables: a rule run can emit
//! columnar findings + a metrics summary alongside (or instead of) diffs.
//! This is **report-only** — building a table never edits anything — which
//! is what inventory, impact analysis, and audit need.
//!
//! The envelope is plain `serde` (`Serialize`), consistent with the rest of
//! the `--json` surfaces; [`DataTable::to_json`] renders it.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::engine::{BindingMetadata, CompiledRule};
use crate::error::RulesError;
use crate::recipe::SourceFile;

/// A columnar table of a rule's findings across a project.
#[derive(Debug, Clone, Serialize)]
pub struct DataTable {
    /// The rule that produced the table.
    pub rule_id: String,
    /// The metavar column names present across all rows (sorted, stable).
    pub columns: Vec<String>,
    /// One row per match.
    pub rows: Vec<TableRow>,
    /// Roll-up metrics.
    pub summary: TableSummary,
}

/// One finding: where it is, the matched text, and the metavar bindings.
#[derive(Debug, Clone, Serialize)]
pub struct TableRow {
    /// The file the match is in.
    pub path: String,
    /// 0-based start row.
    pub start_row: usize,
    /// 0-based start column.
    pub start_col: usize,
    /// The matched text.
    pub text: String,
    /// The metavar bindings, keyed by name.
    pub bindings: BTreeMap<String, String>,
    /// Optional semantic metadata for Harn captures, keyed by metavar name.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub capture_metadata: BTreeMap<String, BindingMetadata>,
}

/// Roll-up metrics for a [`DataTable`].
#[derive(Debug, Clone, Serialize)]
pub struct TableSummary {
    /// Total number of findings (rows).
    pub total_rows: usize,
    /// Number of files with at least one finding.
    pub files: usize,
    /// Per-file finding counts (the #2824 "sites / files" measurement).
    pub per_file: BTreeMap<String, usize>,
}

impl DataTable {
    /// Render the table as a JSON string.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("DataTable serializes")
    }

    /// Render the table as a `serde_json::Value`.
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("DataTable serializes")
    }
}

/// Run `rule` over `files` in **report-only** mode and collect a
/// [`DataTable`] — one row per match, with per-file and total counts. No
/// edits are made.
pub fn data_table(rule: &CompiledRule, files: &[SourceFile]) -> Result<DataTable, RulesError> {
    let mut rows: Vec<TableRow> = Vec::new();
    let mut per_file: BTreeMap<String, usize> = BTreeMap::new();

    for file in files {
        if file.language != rule.language() {
            continue;
        }
        let matches = rule.run(&file.source)?;
        if matches.is_empty() {
            continue;
        }
        let path = file.path.display().to_string();
        per_file.insert(path.clone(), matches.len());
        for m in matches {
            let capture_metadata = m
                .bindings
                .iter()
                .filter(|(_, binding)| !binding.metadata.is_empty())
                .map(|(name, binding)| (name.clone(), binding.metadata.clone()))
                .collect();
            rows.push(TableRow {
                path: path.clone(),
                start_row: m.span.start_row,
                start_col: m.span.start_col,
                text: m.text,
                bindings: m
                    .bindings
                    .into_iter()
                    .map(|(name, binding)| (name, binding.text))
                    .collect(),
                capture_metadata,
            });
        }
    }

    let columns: BTreeSet<String> = rows
        .iter()
        .flat_map(|r| r.bindings.keys().cloned())
        .collect();

    let summary = TableSummary {
        total_rows: rows.len(),
        files: per_file.len(),
        per_file,
    };

    Ok(DataTable {
        rule_id: rule.id().to_string(),
        columns: columns.into_iter().collect(),
        rows,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Rule;

    fn rule(toml: &str) -> CompiledRule {
        CompiledRule::compile(&Rule::from_toml_str(toml).unwrap()).unwrap()
    }

    fn ts(path: &str, source: &str) -> SourceFile {
        SourceFile::detect(path, source).unwrap()
    }

    #[test]
    fn report_only_table_counts_sites_per_file() {
        let rule = rule(
            r#"
            id = "find-calls"
            language = "typescript"
            [rule]
            pattern = "$FN()"
            "#,
        );
        let files = vec![
            ts("a.ts", "foo();\nbar();\n"),
            ts("b.ts", "baz();\n"),
            ts("c.ts", "const x = 1;\n"),
        ];
        let table = data_table(&rule, &files).unwrap();
        assert_eq!(table.summary.total_rows, 3);
        assert_eq!(table.summary.files, 2);
        assert_eq!(table.summary.per_file["a.ts"], 2);
        assert_eq!(table.summary.per_file["b.ts"], 1);
        assert!(!table.summary.per_file.contains_key("c.ts"));
        assert_eq!(table.columns, vec!["FN"]);
        assert_eq!(table.rows[0].bindings["FN"], "foo");
    }

    #[test]
    fn table_serializes_to_json() {
        let rule = rule(
            r#"
            id = "r"
            language = "typescript"
            [rule]
            pattern = "$FN()"
            "#,
        );
        let table = data_table(&rule, &[ts("a.ts", "go();\n")]).unwrap();
        let value = table.to_json_value();
        assert_eq!(value["rule_id"], "r");
        assert_eq!(value["summary"]["total_rows"], 1);
        assert_eq!(value["rows"][0]["bindings"]["FN"], "go");
    }

    #[test]
    fn table_serializes_harn_capture_metadata() {
        let rule = rule(
            r#"
            id = "typed-log"
            language = "harn"
            [rule]
            pattern = "log($VALUE)"
            "#,
        );
        let table = data_table(
            &rule,
            &[ts(
                "a.harn",
                "fn main() {\n  let count: int = 1\n  log(count)\n}\n",
            )],
        )
        .unwrap();
        let value = table.to_json_value();
        let metadata = &value["rows"][0]["capture_metadata"]["VALUE"];
        assert_eq!(metadata["type"], "int");
        assert_eq!(metadata["resolved"]["name"], "count");
        assert_eq!(metadata["resolved"]["start_row"], 1);
        assert!(metadata["resolved"]["span"].is_null());
    }
}
