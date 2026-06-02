//! `harn rule test` — run each rule's inline-annotation tests.
//!
//! A rule `foo.toml` pairs with a fixture `foo.<ext>` (the language's primary
//! extension) carrying `// ruleid:` / `// ok:` annotations; the harness
//! (`harn_rules::run_inline_test`) checks that the rule's matches line up.

use crate::cli::RuleArgs;

#[cfg(not(feature = "hostlib"))]
pub(crate) async fn run(_args: RuleArgs) {
    eprintln!(
        "`harn rule` requires the `hostlib` feature (default-on); it is unavailable in this build"
    );
    std::process::exit(2);
}

#[cfg(feature = "hostlib")]
pub(crate) async fn run(args: RuleArgs) {
    match args.command {
        crate::cli::RuleCommand::Test(test) => run_test(test),
    }
}

#[cfg(feature = "hostlib")]
fn run_test(args: crate::cli::RuleTestArgs) {
    use std::fs;
    use std::path::PathBuf;

    use harn_rules::{load_rule_file, run_inline_test, CompiledRule, InlineTestReport};

    let root = PathBuf::from(args.path.as_deref().unwrap_or("."));
    let rule_files = discover_rule_files(&root);
    if rule_files.is_empty() {
        eprintln!(
            "rule test: no `*.toml` rules found under `{}`",
            root.display()
        );
        std::process::exit(2);
    }

    struct Case {
        fixture: PathBuf,
        report: InlineTestReport,
    }
    let mut cases: Vec<Case> = Vec::new();
    let mut missing_fixtures: Vec<PathBuf> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    for rule_path in rule_files {
        let rule = match load_rule_file(&rule_path) {
            Ok(rule) => rule,
            // A non-rule `*.toml` in a scanned directory is silently skipped;
            // an explicit file argument that fails is a hard error.
            Err(err) => {
                if root.is_file() {
                    errors.push(format!("{}: {err}", rule_path.display()));
                }
                continue;
            }
        };
        let compiled = match CompiledRule::compile(&rule) {
            Ok(c) => c,
            Err(err) => {
                errors.push(format!("{}: {err}", rule_path.display()));
                continue;
            }
        };
        let fixture = rule_path.with_extension(compiled.language().primary_extension());
        let Ok(source) = fs::read_to_string(&fixture) else {
            missing_fixtures.push(fixture);
            continue;
        };
        match run_inline_test(&compiled, &source) {
            Ok(report) => cases.push(Case { fixture, report }),
            Err(err) => errors.push(format!("{}: {err}", fixture.display())),
        }
    }

    let failed = cases.iter().filter(|c| !c.report.passed).count();

    if args.json {
        let cases_json: Vec<serde_json::Value> = cases
            .iter()
            .map(|c| {
                serde_json::json!({
                    "fixture": c.fixture.display().to_string(),
                    "rule_id": c.report.rule_id,
                    "passed": c.report.passed,
                    "matches": c.report.matches,
                    "checked": c.report.checked,
                    "failures": c.report.failures.iter().map(|f| f.describe()).collect::<Vec<_>>(),
                })
            })
            .collect();
        let envelope = serde_json::json!({
            "schemaVersion": 1,
            "passed": failed == 0 && errors.is_empty(),
            "cases": cases_json,
            "missing_fixtures": missing_fixtures.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            "errors": errors,
        });
        println!("{}", serde_json::to_string_pretty(&envelope).unwrap());
    } else {
        for case in &cases {
            let tag = if case.report.passed { "PASS" } else { "FAIL" };
            println!(
                "{tag}  {} ({})",
                case.fixture.display(),
                case.report.rule_id
            );
            for failure in &case.report.failures {
                println!("        {}", failure.describe());
            }
        }
        for path in &missing_fixtures {
            println!("SKIP  {} (no fixture)", path.display());
        }
        for err in &errors {
            eprintln!("error: {err}");
        }
        println!(
            "{} passed, {failed} failed, {} skipped",
            cases.len() - failed,
            missing_fixtures.len()
        );
    }

    if failed > 0 || !errors.is_empty() {
        std::process::exit(1);
    }
}

/// Collect rule `*.toml` files: the path itself if it is a file, else every
/// `*.toml` under it (gitignore-aware).
#[cfg(feature = "hostlib")]
fn discover_rule_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    use ignore::WalkBuilder;

    if root.is_file() {
        return vec![root.to_path_buf()];
    }
    let mut out = Vec::new();
    let mut walker = WalkBuilder::new(root);
    walker.git_ignore(true).require_git(false);
    for entry in walker.build().filter_map(Result::ok) {
        let path = entry.path();
        if entry.file_type().is_some_and(|t| t.is_file())
            && path.extension().and_then(|e| e.to_str()) == Some("toml")
        {
            out.push(path.to_path_buf());
        }
    }
    out.sort();
    out
}
