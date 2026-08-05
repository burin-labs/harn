//! Source-file ↔ test-file pairing.
//!
//! The patterns are inlined here rather than loaded from JSON so the
//! scanner has zero filesystem dependencies. Adding language coverage means
//! updating this table and the matching scanner fixtures together.

use crate::scanner::result::FileRecord;

type PairingKey = (String, String);

/// Substrings / suffixes that mark a file as a test for at least one
/// language.
const TEST_PATTERNS: &[&str] = &[
    // C++
    "_test.cpp",
    "_test.cc",
    // C# (Test.cs is a substring within Tests.cs)
    "Tests.cs",
    "Test.cs",
    "_test.cs",
    // Dart
    "_test.dart",
    // Elixir
    "_test.exs",
    // Go
    "_test.go",
    // Haskell
    "Spec.hs",
    "Test.hs",
    // Java
    "Test.java",
    "Tests.java",
    "IT.java",
    // JavaScript
    ".test.js",
    ".spec.js",
    ".integration.test.js",
    // Kotlin
    "Test.kt",
    "Tests.kt",
    "Spec.kt",
    // Lua
    "_spec.lua",
    "_test.lua",
    // PHP
    "Test.php",
    "_test.php",
    // Python
    "_test.py",
    "tests.py",
    // R
    "test-",
    // Ruby
    "_test.rb",
    "_spec.rb",
    // Rust
    "_test.rs",
    "tests.rs",
    // Scala
    "Spec.scala",
    "Test.scala",
    "Suite.scala",
    // Shell
    ".bats",
    "_test.sh",
    ".test.sh",
    // Swift
    "Tests.swift",
    "Test.swift",
    "Spec.swift",
    // TypeScript
    ".test.ts",
    ".spec.ts",
    ".integration.test.ts",
    ".e2e.test.ts",
    // tsx
    ".test.tsx",
    ".spec.tsx",
    // Zig
    "test.zig",
    "_test.zig",
    // Python / Lua / C++ generic prefix patterns matched via `contains`.
    "test_",
    // Common in monorepos: __tests__/ directory marker.
    "__tests__/",
];

/// True when `relative_path` matches any test-file pattern.
pub fn is_test_file(relative_path: &str) -> bool {
    TEST_PATTERNS
        .iter()
        .any(|pat| relative_path.contains(pat) || relative_path.ends_with(pat))
}

/// Populate [`FileRecord::corresponding_test_file`] for every non-test file
/// that has a recognizable test-file partner. Mutates `files` in place.
///
/// Heuristic:
/// 1. Index test files by a normalized source basename scoped to the final
///    extension (`accounts.integration.test.ts`, `accounts_test.go`,
///    `test_accounts.py`, and `AccountsTest.java` all normalize their
///    basename while staying in their own language extension bucket).
/// 2. For each non-test file, look up by its normalized basename plus
///    extension.
/// 3. Among candidates, pick the one with the largest shared directory
///    prefix.
pub fn map_test_files(files: &mut [FileRecord]) {
    let mut by_base: std::collections::BTreeMap<PairingKey, Vec<String>> =
        std::collections::BTreeMap::new();
    for file in files.iter() {
        if !is_test_file(&file.relative_path) {
            continue;
        }
        let base = pairing_key(&file.file_name, true);
        by_base
            .entry(base)
            .or_default()
            .push(file.relative_path.clone());
    }

    for file in files.iter_mut() {
        if is_test_file(&file.relative_path) {
            continue;
        }
        let base = pairing_key(&file.file_name, false);
        let candidates = match by_base.get(&base) {
            Some(c) if !c.is_empty() => c,
            _ => continue,
        };
        let source_dir = parent_dir_owned(&file.relative_path);
        let best = candidates
            .iter()
            .max_by_key(|cand| common_prefix_components(cand, &source_dir))
            .cloned();
        file.corresponding_test_file = best;
    }
}

fn pairing_key(file_name: &str, is_test: bool) -> PairingKey {
    let token = file_name.split('.').next().unwrap_or(file_name).to_string();
    let base = if is_test {
        normalize_test_token(&token).to_ascii_lowercase()
    } else {
        token.to_ascii_lowercase()
    };
    (base, file_extension(file_name).to_ascii_lowercase())
}

fn file_extension(file_name: &str) -> &str {
    file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension)
        .unwrap_or("")
}

fn normalize_test_token(token: &str) -> &str {
    let without_prefix = token
        .strip_prefix("test_")
        .or_else(|| token.strip_prefix("test-"))
        .unwrap_or(token);
    strip_first_suffix(
        without_prefix,
        &[
            "_test", "_tests", "_spec", "-test", "-tests", "-spec", "Tests", "Test", "Spec",
            "Suite", "IT",
        ],
    )
}

fn strip_first_suffix<'a>(value: &'a str, suffixes: &[&str]) -> &'a str {
    for suffix in suffixes {
        if let Some(stripped) = value.strip_suffix(suffix) {
            if !stripped.is_empty() {
                return stripped;
            }
        }
    }
    value
}

fn parent_dir_owned(path: &str) -> String {
    crate::scanner::extensions::parent_dir(path).to_string()
}

fn common_prefix_components(a: &str, b: &str) -> usize {
    a.split('/')
        .zip(b.split('/'))
        .take_while(|(x, y)| x == y)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(path: &str) -> FileRecord {
        FileRecord {
            id: path.to_string(),
            relative_path: path.to_string(),
            file_name: path.rsplit('/').next().unwrap_or(path).to_string(),
            language: "ts".to_string(),
            line_count: 1,
            size_bytes: 1,
            last_modified_unix_ms: 0,
            imports: Vec::new(),
            churn_score: 0.0,
            corresponding_test_file: None,
        }
    }

    #[test]
    fn pairs_source_with_same_dir_test() {
        let mut files = vec![
            record("src/routes/accounts.ts"),
            record("src/routes/__tests__/accounts.test.ts"),
            record("src/routes/__tests__/accounts.integration.test.ts"),
        ];
        map_test_files(&mut files);
        let src = files
            .iter()
            .find(|f| f.relative_path == "src/routes/accounts.ts")
            .unwrap();
        assert!(src.corresponding_test_file.is_some());
        assert!(src
            .corresponding_test_file
            .as_deref()
            .unwrap()
            .ends_with(".test.ts"));
    }

    #[test]
    fn pairs_swift_source_with_swift_tests() {
        // Dotted test-name variants keep their first basename token.
        let mut files = vec![record("Sources/Foo.swift"), record("Tests/Foo.Tests.swift")];
        map_test_files(&mut files);
        let src = files
            .iter()
            .find(|f| f.relative_path == "Sources/Foo.swift")
            .unwrap();
        assert_eq!(
            src.corresponding_test_file.as_deref(),
            Some("Tests/Foo.Tests.swift")
        );
    }

    #[test]
    fn pairs_common_test_affix_conventions() {
        let mut files = vec![
            record("src/foo.go"),
            record("src/foo_test.go"),
            record("src/user.py"),
            record("tests/test_user.py"),
            record("src/account.rb"),
            record("spec/account_spec.rb"),
            record("src/Foo.java"),
            record("test/FooTest.java"),
            record("src/Widget.scala"),
            record("test/WidgetSpec.scala"),
            record("Sources/App.swift"),
            record("Tests/AppTests.swift"),
        ];
        map_test_files(&mut files);
        for (source, test) in [
            ("src/foo.go", "src/foo_test.go"),
            ("src/user.py", "tests/test_user.py"),
            ("src/account.rb", "spec/account_spec.rb"),
            ("src/Foo.java", "test/FooTest.java"),
            ("src/Widget.scala", "test/WidgetSpec.scala"),
            ("Sources/App.swift", "Tests/AppTests.swift"),
        ] {
            let src = files
                .iter()
                .find(|f| f.relative_path == source)
                .unwrap_or_else(|| panic!("missing source record {source}"));
            assert_eq!(src.corresponding_test_file.as_deref(), Some(test));
        }
    }
}
