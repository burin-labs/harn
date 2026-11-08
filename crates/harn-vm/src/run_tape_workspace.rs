//! The in-memory filesystem view of a replay. Tape payloads stay immutable.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Project workspace paths in host configuration and model-facing data.
/// This relocates names only. Process execution separately requires an
/// OS-enforced filesystem view; text projection cannot confine a child.
#[derive(Clone, Debug)]
pub struct ReplayWorkspaceProjection {
    roots: Vec<(String, String)>,
}

impl ReplayWorkspaceProjection {
    pub fn new(recorded: &Path, restored: &Path) -> Self {
        Self {
            roots: vec![(
                recorded.display().to_string(),
                restored.display().to_string(),
            )],
        }
    }

    pub(super) fn with_sources(mut self, sources: &[super::ReplaySourceRoot]) -> Self {
        self.roots.extend(sources.iter().map(|root| {
            (
                root.recorded.display().to_string(),
                root.restored.display().to_string(),
            )
        }));
        self.roots
            .sort_by_key(|(from, _)| std::cmp::Reverse(from.len()));
        self
    }

    /// Replace a complete root, including inside prose, shell words and JSON
    /// strings. A prefix of another path component is never a root match.
    pub fn text(&self, text: &str) -> String {
        self.rewrite(text, false)
    }

    pub fn path(&self, path: &Path) -> PathBuf {
        PathBuf::from(self.text(&path.display().to_string()))
    }

    pub fn json(&self, value: &mut Value) {
        self.map_json(value, false);
    }

    pub(super) fn recorded_json(&self, value: &mut Value) {
        self.map_json(value, true);
    }

    pub(super) fn recorded_text(&self, text: &str) -> String {
        self.rewrite(text, true)
    }

    fn map_json(&self, value: &mut Value, reverse: bool) {
        match value {
            Value::String(text) => *text = self.rewrite(text, reverse),
            Value::Array(values) => values.iter_mut().for_each(|v| self.map_json(v, reverse)),
            Value::Object(values) => values.values_mut().for_each(|v| self.map_json(v, reverse)),
            _ => {}
        }
    }

    fn rewrite(&self, text: &str, reverse: bool) -> String {
        let mut result = String::with_capacity(text.len());
        let mut offset = 0;
        while offset < text.len() {
            let rest = &text[offset..];
            let matched = self.roots.iter().find_map(|(recorded, restored)| {
                let (from, to) = if reverse {
                    (restored, recorded)
                } else {
                    (recorded, restored)
                };
                if !root_match(text, offset, from) {
                    return None;
                }
                Some((from.len(), to))
            });
            let existing = self
                .roots
                .iter()
                .map(|(from, to)| if reverse { from } else { to })
                .filter(|root| root_match(text, offset, root))
                .map(String::len)
                .max();
            if let Some(len) =
                existing.filter(|len| matched.as_ref().is_none_or(|(from_len, _)| len >= from_len))
            {
                result.push_str(&rest[..len]);
                offset += len;
                continue;
            }
            if let Some((len, to)) = matched {
                result.push_str(to);
                offset += len;
            } else {
                let character = rest.chars().next().expect("nonempty suffix");
                result.push(character);
                offset += character.len_utf8();
            }
        }
        result
    }
}

fn path_component(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '~')
}

fn root_match(text: &str, offset: usize, root: &str) -> bool {
    let rest = &text[offset..];
    if root.is_empty() || !rest.starts_with(root) {
        return false;
    }
    let before = text[..offset].chars().next_back();
    let after = rest[root.len()..].chars().next();
    (before.is_none_or(|c| !path_component(c) && c != '/' && c != '\\')
        || text[..offset].ends_with("file://"))
        && after.is_none_or(|c| !path_component(c))
}

pub(crate) fn current_projection() -> Option<ReplayWorkspaceProjection> {
    let branch = super::scope::state_branch()?;
    let recorded = branch.recorded_workspace()?;
    Some(
        ReplayWorkspaceProjection::new(recorded, branch.workspace())
            .with_sources(branch.source_roots()),
    )
}

pub(crate) fn project_json(value: &mut Value) {
    if let Some(projection) = current_projection() {
        projection.json(value);
    }
}

pub(crate) fn project_request(request: &mut crate::llm::api::LlmRequestPayload) {
    let Some(projection) = current_projection() else {
        return;
    };
    for message in &mut request.messages {
        projection.json(message);
    }
    for text in [
        &mut request.system,
        &mut request.prediction,
        &mut request.prefill,
    ]
    .into_iter()
    .flatten()
    {
        *text = projection.text(text);
    }
    for stop in request.stop.iter_mut().flatten() {
        *stop = projection.text(stop);
    }
    for value in request
        .native_tools
        .iter_mut()
        .flatten()
        .chain(request.provider_tools.iter_mut())
    {
        projection.json(value);
    }
    for value in [
        &mut request.output_schema,
        &mut request.tool_choice,
        &mut request.provider_overrides,
    ]
    .into_iter()
    .flatten()
    {
        projection.json(value);
    }
    if let crate::llm::api::OutputFormat::JsonSchema { schema, .. } = &mut request.output_format {
        projection.json(schema);
    }
}

pub(super) fn project_tool_result(result: super::RecordedToolResult) -> super::RecordedToolResult {
    let mut value = serde_json::to_value(result).expect("recorded tool result is serializable");
    project_json(&mut value);
    serde_json::from_value(value).expect("path projection preserves tool result shape")
}

pub(crate) fn project_value(value: crate::VmValue) -> crate::VmValue {
    let Some(projection) = current_projection() else {
        return value;
    };
    project_value_with(&projection, value)
}

fn project_value_with(
    projection: &ReplayWorkspaceProjection,
    value: crate::VmValue,
) -> crate::VmValue {
    use crate::VmValue;
    match value {
        VmValue::String(text) => VmValue::string(projection.text(&text)),
        VmValue::List(values) => VmValue::list(
            values
                .iter()
                .cloned()
                .map(|value| project_value_with(projection, value))
                .collect(),
        ),
        VmValue::Dict(values) => VmValue::dict(
            values
                .iter()
                .map(|(k, v)| (k.clone(), project_value_with(projection, v.clone()))),
        ),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_projection_keeps_nested_roots_and_component_boundaries() {
        let projection =
            ReplayWorkspaceProjection::new(Path::new("/recorded"), Path::new("/recorded/arm"));
        let original =
            "file:///recorded/value '/recorded' /recorded-sibling/value /prefix/recorded/value";
        let projected = "file:///recorded/arm/value '/recorded/arm' /recorded-sibling/value /prefix/recorded/value";
        assert_eq!(projection.text(original), projected);
        assert_eq!(projection.text(projected), projected);
        assert_eq!(projection.recorded_text(projected), original);
    }
}
