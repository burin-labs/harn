//! Acceptance for #2837: report-only data tables. Running the destructuring
//! rule yields a table with per-file site counts, a total, and enough
//! structure to derive the alias count — the #2824 "~2,576 sites / 109
//! files" measurement, produced automatically.

use harn_rules::{data_table, CompiledRule, Rule, SourceFile};

fn ts(path: &str, source: &str) -> SourceFile {
    SourceFile::detect(path, source).unwrap()
}

#[test]
fn destructuring_report_only_table_matches_measurement_style() {
    let rule = CompiledRule::compile(
        &Rule::from_toml_str(
            r#"
            id = "destructure-sites"
            language = "typescript"
            [rule]
            pattern = "let $NAME = $SRC?.$KEY ?? $DEFAULT"
            "#,
        )
        .unwrap(),
    )
    .unwrap();

    let files = vec![
        ts(
            "config.ts",
            "let id = src?.userId ?? 0;\nlet name = src?.name ?? \"\";\n",
        ),
        ts("opts.ts", "let count = o?.count ?? 0;\n"),
        ts("empty.ts", "const x = 1;\n"),
    ];

    let table = data_table(&rule, &files).unwrap();

    // Total foldable runs (sites) and per-file counts, OpenRewrite-style.
    assert_eq!(table.summary.total_rows, 3);
    assert_eq!(table.summary.files, 2);
    assert_eq!(table.summary.per_file["config.ts"], 2);
    assert_eq!(table.summary.per_file["opts.ts"], 1);
    assert!(!table.summary.per_file.contains_key("empty.ts"));

    // The metavar columns are present for downstream analysis.
    assert!(table.columns.contains(&"NAME".to_string()));
    assert!(table.columns.contains(&"KEY".to_string()));

    // Alias count = sites where the binding name differs from the key
    // (`let id = src?.userId` is an alias; `let name = src?.name` is not).
    let aliases = table
        .rows
        .iter()
        .filter(|r| r.bindings["NAME"] != r.bindings["KEY"])
        .count();
    assert_eq!(aliases, 1);

    // Report-only: the table is the whole output; nothing was edited.
    // (data_table never calls apply — it only runs the matcher.)
    let json = table.to_json_value();
    assert_eq!(json["summary"]["total_rows"], 3);
    assert_eq!(json["rule_id"], "destructure-sites");
}
