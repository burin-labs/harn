//! Load rules from disk: a single TOML rule file, or a directory of them.

use std::path::Path;

use crate::error::RulesError;
use crate::model::Rule;

/// Load a single rule from a `.toml` file.
pub fn load_rule_file(path: impl AsRef<Path>) -> Result<Rule, RulesError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path).map_err(|source| RulesError::Read {
        path: path.display().to_string(),
        source,
    })?;
    Rule::from_toml_str(&text).map_err(|source| RulesError::Parse {
        path: path.display().to_string(),
        source,
    })
}

/// Load every `.toml` rule directly under `dir`, sorted by filename for a
/// deterministic order. Non-`.toml` files and subdirectories are ignored.
pub fn load_rule_dir(dir: impl AsRef<Path>) -> Result<Vec<Rule>, RulesError> {
    let dir = dir.as_ref();
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|source| RulesError::Read {
            path: dir.display().to_string(),
            source,
        })?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    entries.sort();

    entries.into_iter().map(load_rule_file).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(dir: &Path, name: &str, body: &str) {
        let mut f = std::fs::File::create(dir.join(name)).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn loads_a_single_rule_file() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "r.toml",
            r#"
            id = "r"
            language = "rust"
            [rule]
            kind = "macro_invocation"
            "#,
        );
        let rule = load_rule_file(dir.path().join("r.toml")).unwrap();
        assert_eq!(rule.id, "r");
    }

    #[test]
    fn loads_a_directory_in_sorted_order() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "b.toml",
            "id = \"b\"\nlanguage = \"rust\"\n[rule]\nkind = \"x\"\n",
        );
        write(
            dir.path(),
            "a.toml",
            "id = \"a\"\nlanguage = \"rust\"\n[rule]\nkind = \"x\"\n",
        );
        write(dir.path(), "ignore.txt", "not a rule");
        let rules = load_rule_dir(dir.path()).unwrap();
        let ids: Vec<_> = rules.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
    }

    #[test]
    fn parse_error_names_the_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "bad.toml", "id = \nthis is not toml");
        let err = load_rule_file(dir.path().join("bad.toml")).unwrap_err();
        assert!(matches!(err, RulesError::Parse { .. }));
    }
}
