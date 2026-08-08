//! Deterministic, model-oriented context synthesized from scanner facts.
//!
//! This is deliberately part of the scanner module: hosts should consume one
//! authoritative projection instead of rebuilding scanner policy from the raw
//! file, symbol, and dependency rows.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::Path;

use super::result::{DependencyEdge, FileRecord, ProjectMetadata, SymbolKind, SymbolRecord};

const MAX_AGENT_INSTRUCTIONS_CHARS: usize = 4_000;

pub(super) fn build(
    root: &Path,
    project: &ProjectMetadata,
    files: &[FileRecord],
    symbols: &[SymbolRecord],
    dependencies: &[DependencyEdge],
) -> String {
    let mut output = String::from("# Codebase fingerprint\n\n");
    output.push_str(&format!("Project: {}\n", project.name));
    let mut ranked_languages = project.languages.iter().collect::<Vec<_>>();
    ranked_languages.sort_by(|left, right| {
        right
            .line_count
            .cmp(&left.line_count)
            .then_with(|| right.file_count.cmp(&left.file_count))
            .then_with(|| left.name.cmp(&right.name))
    });
    let languages = ranked_languages
        .into_iter()
        .take(3)
        .map(|language| format!("{} ({} files)", language.name, language.file_count))
        .collect::<Vec<_>>()
        .join(", ");
    output.push_str(&format!("Languages: {languages}\n"));
    output.push_str(&format!(
        "Files: {}, Lines: {}\n\n",
        project.total_files, project.total_lines
    ));

    append_list_section(
        &mut output,
        "Naming conventions",
        &naming_conventions(symbols),
        false,
    );
    if let Some(pattern) = error_handling_pattern(root, files) {
        append_list_section(&mut output, "Error handling", &[pattern], false);
    }
    append_list_section(&mut output, "Test recipe", &test_recipe(root, files), false);
    append_list_section(
        &mut output,
        "Key signatures",
        &key_signatures(symbols),
        false,
    );
    append_list_section(
        &mut output,
        "Import graph (most-referenced modules)",
        &import_hot_paths(dependencies),
        false,
    );
    append_test_commands(&mut output, project);
    let mut code_patterns = project.code_patterns.clone();
    code_patterns.sort();
    code_patterns.truncate(8);
    append_list_section(&mut output, "Code patterns", &code_patterns, false);
    append_list_section(
        &mut output,
        "Test helpers",
        &test_helper_signatures(root, files),
        true,
    );
    let mut available_dependencies = project.available_dependencies.clone();
    available_dependencies.sort();
    available_dependencies.dedup();
    available_dependencies.truncate(20);
    append_list_section(&mut output, "Dependencies", &available_dependencies, true);
    if let Some(instructions) = agent_instructions(root) {
        output.push_str("## Agent instructions\n");
        output.push_str(&instructions);
        if !instructions.ends_with('\n') {
            output.push('\n');
        }
    }
    output
}

fn append_list_section(output: &mut String, heading: &str, rows: &[String], code: bool) {
    if rows.is_empty() {
        return;
    }
    output.push_str(&format!("## {heading}\n"));
    for row in rows {
        if code {
            output.push_str(&format!("- `{row}`\n"));
        } else {
            output.push_str(&format!("- {row}\n"));
        }
    }
    output.push('\n');
}

fn naming_conventions(symbols: &[SymbolRecord]) -> Vec<String> {
    let functions = symbols
        .iter()
        .filter(|symbol| matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method))
        .collect::<Vec<_>>();
    let types = symbols
        .iter()
        .filter(|symbol| {
            matches!(
                symbol.kind,
                SymbolKind::ClassDecl | SymbolKind::StructDecl | SymbolKind::InterfaceDecl
            )
        })
        .collect::<Vec<_>>();
    let constants = symbols
        .iter()
        .filter(|symbol| {
            symbol.name.len() > 2
                && symbol.name.chars().any(char::is_alphabetic)
                && symbol.name == symbol.name.to_uppercase()
        })
        .count();

    let mut rows = Vec::new();
    if !functions.is_empty() {
        let camel = functions
            .iter()
            .filter(|symbol| {
                symbol.name.starts_with(char::is_lowercase)
                    && symbol.name.chars().any(char::is_uppercase)
            })
            .count();
        let snake = functions
            .iter()
            .filter(|symbol| {
                symbol.name.contains('_') && !symbol.name.chars().any(char::is_uppercase)
            })
            .count();
        if camel > snake.saturating_mul(2) {
            rows.push("Functions: camelCase".to_string());
        } else if snake > camel.saturating_mul(2) {
            rows.push("Functions: snake_case".to_string());
        }
    }
    if !types.is_empty()
        && types
            .iter()
            .filter(|symbol| symbol.name.starts_with(char::is_uppercase))
            .count()
            > types.len() / 2
    {
        rows.push("Types: PascalCase".to_string());
    }
    if constants > 3 {
        rows.push("Constants: UPPER_SNAKE_CASE".to_string());
    }
    rows
}

fn error_handling_pattern(root: &Path, files: &[FileRecord]) -> Option<String> {
    let mut route_files = files
        .iter()
        .filter(|file| {
            let path = format!("/{}", file.relative_path);
            path.contains("/routes/")
                || path.contains("/handlers/")
                || path.contains("/controllers/")
        })
        .collect::<Vec<_>>();
    route_files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    route_files
        .into_iter()
        .take(3)
        .filter_map(|file| fs::read_to_string(root.join(&file.relative_path)).ok())
        .find_map(|content| {
            if content.contains("res.status(") && content.contains(".json({ error:") {
                Some("Returns `{ error: string }` with HTTP status codes (Express-style)".into())
            } else if content.contains("raise HTTPException") || content.contains("HTTPException(")
            {
                Some("Raises HTTPException with status code and detail (FastAPI-style)".into())
            } else if content.contains("http.Error(") {
                Some("Uses http.Error() with status code (Go net/http style)".into())
            } else if content.contains("Result<") || content.contains("Result.failure") {
                Some("Uses Result type for error handling (Swift-style)".into())
            } else {
                None
            }
        })
}

fn test_recipe(root: &Path, files: &[FileRecord]) -> Vec<String> {
    let mut test_files = files
        .iter()
        .filter(|file| is_test_file(&file.relative_path))
        .collect::<Vec<_>>();
    test_files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let Some(file) = test_files.first() else {
        return Vec::new();
    };
    let Ok(content) = fs::read_to_string(root.join(&file.relative_path)) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    let import_lines = content
        .lines()
        .filter(|line| line.contains("import ") || line.contains("from "))
        .take(5)
        .collect::<Vec<_>>();
    for (keyword, label) in [
        ("vitest", "Framework: vitest"),
        ("jest", "Framework: jest"),
        ("pytest", "Framework: pytest"),
        ("XCTest", "Framework: XCTest"),
    ] {
        if import_lines.iter().any(|line| line.contains(keyword)) {
            rows.push(label.to_string());
            break;
        }
    }
    if !rows.iter().any(|row| row.starts_with("Framework:"))
        && import_lines
            .iter()
            .any(|line| line.contains("testing") && line.contains("\"testing\""))
    {
        rows.push("Framework: Go testing".to_string());
    }

    let helper_names = [
        ("createAgent", "createAgent()"),
        ("createTestUser", "createTestUser()"),
        ("setupDatabase", "setupDatabase()"),
        ("loginAs", "loginAs()"),
        ("TestClient", "TestClient"),
    ]
    .into_iter()
    .filter_map(|(needle, label)| content.contains(needle).then_some(label))
    .collect::<BTreeSet<_>>();
    if !helper_names.is_empty() {
        rows.push(format!(
            "Helpers: {}",
            helper_names.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    if content.contains("beforeEach") || content.contains("beforeAll") {
        rows.push("Setup: beforeEach/afterEach blocks".to_string());
    }
    if content.contains("conftest") || content.contains("@pytest.fixture") {
        rows.push("Setup: pytest fixtures in conftest.py".to_string());
    }
    if content.contains("expect(") && content.contains(".toBe(") {
        rows.push("Assertions: expect().toBe/toEqual/toMatchObject".to_string());
    }
    if content.contains("assert ") || content.contains("assertEqual") {
        rows.push("Assertions: assert / assertEqual".to_string());
    }
    if content.contains("XCTAssert") {
        rows.push("Assertions: XCTAssertEqual/XCTAssertTrue".to_string());
    }
    let parent = Path::new(&file.relative_path)
        .parent()
        .and_then(Path::to_str)
        .unwrap_or("");
    rows.push(format!("Test location: {parent}/"));
    rows
}

fn key_signatures(symbols: &[SymbolRecord]) -> Vec<String> {
    let ui_segments = [
        "/pages/",
        "/components/",
        "/views/",
        "/screens/",
        "/widgets/",
    ];
    let priority_segments = [
        "/routes/",
        "/services/",
        "/middleware/",
        "/utils/",
        "/lib/",
        "/shared/",
        "/helpers/",
        "/api/",
        "/handlers/",
        "/controllers/",
    ];
    let ui_handlers = [
        "handleSubmit",
        "handleDelete",
        "handleClose",
        "handleKeyDown",
        "handleChange",
        "handleClick",
        "handleSave",
        "handleCancel",
    ];
    let mut candidates = symbols
        .iter()
        .filter(|symbol| symbol.reference_count >= 2 && !symbol.signature.is_empty())
        .map(|symbol| {
            let wrapped = format!("/{}", symbol.file_path);
            let mut weight = symbol.importance_score;
            if ui_segments.iter().any(|segment| wrapped.contains(segment))
                || symbol.file_path.ends_with(".tsx")
                || symbol.file_path.ends_with(".jsx")
            {
                weight *= 0.3;
            } else if is_test_file(&symbol.file_path) {
                weight *= 0.2;
            } else if priority_segments
                .iter()
                .any(|segment| wrapped.contains(segment))
            {
                weight *= 2.0;
            }
            if ui_handlers.contains(&symbol.name.as_str()) {
                weight *= 0.1;
            }
            (symbol, weight)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left, left_weight), (right, right_weight)| {
        right_weight
            .total_cmp(left_weight)
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut rows = Vec::new();
    let mut file_counts = HashMap::<String, usize>::new();
    let mut seen = BTreeSet::new();
    for (symbol, _) in candidates {
        let signature_key = truncate_chars(&symbol.signature, 80);
        if !seen.insert(signature_key) {
            continue;
        }
        let file_name = Path::new(&symbol.file_path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&symbol.file_path)
            .to_string();
        let count = file_counts.entry(file_name.clone()).or_default();
        if *count >= 3 {
            continue;
        }
        *count += 1;
        let kind = match symbol.kind {
            SymbolKind::Function => "fn",
            SymbolKind::ClassDecl => "class",
            SymbolKind::InterfaceDecl => "interface",
            SymbolKind::StructDecl => "struct",
            _ => "val",
        };
        rows.push(format!(
            "{kind} {} [{file_name}, refs:{}]",
            truncate_chars(&symbol.signature, 100),
            symbol.reference_count
        ));
        if rows.len() >= 15 {
            break;
        }
    }
    rows
}

fn import_hot_paths(dependencies: &[DependencyEdge]) -> Vec<String> {
    let mut counts = BTreeMap::<&str, usize>::new();
    for dependency in dependencies {
        *counts.entry(&dependency.to_module).or_default() += 1;
    }
    let mut rows = counts.into_iter().collect::<Vec<_>>();
    rows.sort_by(|(left_name, left_count), (right_name, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_name.cmp(right_name))
    });
    rows.into_iter()
        .take(8)
        .map(|(name, count)| format!("{name} (imported by {count} files)"))
        .collect()
}

fn append_test_commands(output: &mut String, project: &ProjectMetadata) {
    if project.test_commands.is_empty() && project.detected_test_command.is_none() {
        return;
    }
    output.push_str("## Test commands\n");
    if let Some(command) = project
        .detected_test_command
        .as_deref()
        .filter(|command| !command.is_empty())
    {
        output.push_str(&format!("- Preferred: `{command}`\n"));
    }
    for (command, description) in project.test_commands.iter().take(5) {
        if Some(command.as_str()) == project.detected_test_command.as_deref() {
            continue;
        }
        output.push_str(&format!("- `{command}` — {description}\n"));
    }
    output.push('\n');
}

fn test_helper_signatures(root: &Path, files: &[FileRecord]) -> Vec<String> {
    let helper_keywords = ["helper", "setup", "fixture", "factory", "mock", "utils"];
    let mut helpers = files
        .iter()
        .filter(|file| {
            let name = file.file_name.to_lowercase();
            let path = format!("/{}", file.relative_path);
            helper_keywords
                .iter()
                .any(|keyword| name.contains(keyword) || path.contains(keyword))
                && (is_test_file(&file.relative_path)
                    || path.contains("/test/")
                    || path.contains("/tests/")
                    || path.contains("/__tests__/")
                    || path.contains("/__test__/"))
        })
        .collect::<Vec<_>>();
    helpers.sort_by(|left, right| {
        right
            .line_count
            .cmp(&left.line_count)
            .then_with(|| right.relative_path.cmp(&left.relative_path))
    });
    let mut rows = Vec::new();
    for file in helpers.into_iter().take(3) {
        let Ok(content) = fs::read_to_string(root.join(&file.relative_path)) else {
            continue;
        };
        rows.extend(parse_helper_signatures(&content));
        if rows.len() >= 10 {
            break;
        }
    }
    rows.truncate(10);
    rows
}

fn parse_helper_signatures(content: &str) -> Vec<String> {
    let lines = content.lines().collect::<Vec<_>>();
    let mut rows = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let matches = (trimmed.starts_with("export ")
            && (trimmed.contains("function ") || trimmed.contains("const ")))
            || (trimmed.starts_with("def ") && *line == trimmed)
            || (trimmed.starts_with("func ") && *line == trimmed)
            || trimmed.starts_with("pub fn ")
            || trimmed.starts_with("pub async fn ")
            || (trimmed.starts_with("public ") && trimmed.contains('('));
        if matches {
            let signature = collect_signature(&lines, index);
            if !signature.is_empty() {
                rows.push(signature);
            }
        }
    }
    rows
}

fn collect_signature(lines: &[&str], start: usize) -> String {
    let mut signature = lines[start].trim().to_string();
    let mut depth = paren_delta(&signature);
    let mut index = start + 1;
    while depth > 0 && index < lines.len() && index < start + 5 {
        let next = lines[index].trim();
        signature.push(' ');
        signature.push_str(next);
        depth += paren_delta(next);
        index += 1;
    }
    if let Some(brace) = signature.find('{') {
        signature.truncate(brace);
        signature = signature.trim().to_string();
    }
    truncate_chars(&signature, 150)
}

fn paren_delta(value: &str) -> i32 {
    value.chars().fold(0, |depth, character| match character {
        '(' => depth + 1,
        ')' => depth - 1,
        _ => depth,
    })
}

fn agent_instructions(root: &Path) -> Option<String> {
    ["AGENTS.md", "CLAUDE.md"]
        .into_iter()
        .find_map(|name| fs::read_to_string(root.join(name)).ok())
        .map(|instructions| truncate_chars(instructions.trim(), MAX_AGENT_INSTRUCTIONS_CHARS))
        .filter(|instructions| !instructions.is_empty())
}

fn is_test_file(path: &str) -> bool {
    let lower = format!("/{}", path.to_lowercase());
    let file_name = Path::new(&lower)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains("/__tests__/")
        || lower.contains("/__test__/")
        || lower.ends_with("_test.rs")
        || lower.ends_with("_test.go")
        || lower.ends_with("tests.swift")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || file_name.starts_with("test_")
        || file_name.ends_with("_test.py")
}

fn truncate_chars(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::{LanguageStat, SymbolKind};
    use tempfile::tempdir;

    fn file(path: &str, lines: usize) -> FileRecord {
        FileRecord {
            id: path.to_string(),
            relative_path: path.to_string(),
            file_name: Path::new(path)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string(),
            language: Path::new(path)
                .extension()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            line_count: lines,
            size_bytes: 1,
            last_modified_unix_ms: 0,
            imports: Vec::new(),
            churn_score: 0.0,
            corresponding_test_file: None,
        }
    }

    fn symbol(
        name: &str,
        kind: SymbolKind,
        path: &str,
        signature: &str,
        refs: usize,
    ) -> SymbolRecord {
        SymbolRecord {
            id: format!("{path}:{name}:1"),
            name: name.to_string(),
            kind,
            file_path: path.to_string(),
            line: 1,
            signature: signature.to_string(),
            container: None,
            reference_count: refs,
            importance_score: refs as f64,
        }
    }

    #[test]
    fn builds_complete_deterministic_projection_from_scanner_facts() {
        let root = tempdir().unwrap();
        fs::create_dir_all(root.path().join("src/routes")).unwrap();
        fs::create_dir_all(root.path().join("tests/helpers")).unwrap();
        fs::write(
            root.path().join("src/routes/users.ts"),
            "res.status(400).json({ error: message });\n",
        )
        .unwrap();
        fs::write(
            root.path().join("tests/helpers/user.test.ts"),
            "import { expect } from \"vitest\";\nbeforeEach(() => createTestUser());\nexpect(ok).toBe(true);\nexport function createTestUser(name: string) { return name; }\n",
        )
        .unwrap();
        fs::write(root.path().join("AGENTS.md"), "Use one scanner owner.\n").unwrap();

        let project = ProjectMetadata {
            name: "fixture".into(),
            root_path: root.path().display().to_string(),
            languages: vec![LanguageStat {
                name: "ts".into(),
                file_count: 2,
                line_count: 5,
                percentage: 100.0,
            }],
            test_commands: BTreeMap::from([("pnpm test".into(), "package script".into())]),
            detected_test_command: Some("pnpm test".into()),
            code_patterns: vec!["Express routes".into()],
            total_files: 2,
            total_lines: 5,
            last_scanned_at: "2026-08-08T00:00:00Z".into(),
            available_dependencies: vec!["vitest".into()],
        };
        let files = vec![
            file("src/routes/users.ts", 1),
            file("tests/helpers/user.test.ts", 4),
        ];
        let symbols = vec![
            symbol(
                "fetchUser",
                SymbolKind::Function,
                "src/routes/users.ts",
                "fetchUser(id: string)",
                4,
            ),
            symbol(
                "UserStore",
                SymbolKind::ClassDecl,
                "src/routes/users.ts",
                "class UserStore",
                3,
            ),
        ];
        let dependencies = vec![
            DependencyEdge {
                from_file: "src/a.ts".into(),
                to_module: "shared/http".into(),
            },
            DependencyEdge {
                from_file: "src/b.ts".into(),
                to_module: "shared/http".into(),
            },
        ];

        let first = build(root.path(), &project, &files, &symbols, &dependencies);
        let second = build(root.path(), &project, &files, &symbols, &dependencies);
        assert_eq!(first, second);
        for expected in [
            "Project: fixture",
            "Languages: ts (2 files)",
            "## Naming conventions\n- Functions: camelCase\n- Types: PascalCase",
            "## Error handling\n- Returns `{ error: string }` with HTTP status codes (Express-style)",
            "## Test recipe\n- Framework: vitest\n- Helpers: createTestUser()",
            "## Key signatures\n- fn fetchUser(id: string) [users.ts, refs:4]",
            "## Import graph (most-referenced modules)\n- shared/http (imported by 2 files)",
            "## Test commands\n- Preferred: `pnpm test`",
            "## Code patterns\n- Express routes",
            "## Test helpers\n- `export function createTestUser(name: string)`",
            "## Dependencies\n- `vitest`",
            "## Agent instructions\nUse one scanner owner.",
        ] {
            assert!(first.contains(expected), "missing {expected:?} in:\n{first}");
        }
    }
}
