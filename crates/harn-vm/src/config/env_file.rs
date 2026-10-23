//! Literal environment files shared by CLI launches and Harn scripts.

use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvFileError {
    pub line: usize,
    pub message: &'static str,
}

impl fmt::Display for EnvFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for EnvFileError {}

/// Parse KEY=VALUE lines without executing code or expanding variables.
/// Blank lines and comments are ignored; optional `export` and matching
/// outer quotes are stripped. Later entries replace earlier entries.
pub fn parse_env_file(text: &str) -> Result<BTreeMap<String, String>, EnvFileError> {
    let mut values = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let error = |message| EnvFileError {
            line: index + 1,
            message,
        };
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| error("expected KEY=VALUE"))?;
        let key = key.trim();
        if key.is_empty() {
            return Err(error("empty key"));
        }
        if !key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        {
            return Err(error("invalid key"));
        }
        let value = value.trim();
        if value.contains('\0') {
            return Err(error("environment values cannot contain NUL"));
        }
        let value = if let Some(quote @ ('\'' | '"')) = value.chars().next() {
            value
                .strip_prefix(quote)
                .and_then(|rest| rest.strip_suffix(quote))
                .ok_or_else(|| error("unterminated quoted value"))?
        } else {
            value
        };
        values.insert(key.to_string(), value.to_string());
    }
    Ok(values)
}
