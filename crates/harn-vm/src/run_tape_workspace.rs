//! The in-memory filesystem view of a replay. Tape payloads stay immutable.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The stable name seen by a replay and the directory that owns its effects.
#[derive(Clone, Debug)]
pub struct ReplayWorkspaceIdentity {
    logical_root: PathBuf,
    physical_root: PathBuf,
}

impl ReplayWorkspaceIdentity {
    pub fn logical_root(&self) -> &Path {
        &self.logical_root
    }
    pub fn physical_root(&self) -> &Path {
        &self.physical_root
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ReplayWorkspaceProjection {
    roots: Vec<RootMapping>,
}

#[derive(Clone, Debug)]
struct RootMapping {
    recorded: String,
    restored: String,
}

impl ReplayWorkspaceProjection {
    pub fn new(recorded: &Path, restored: &Path) -> Self {
        Self {
            roots: vec![RootMapping {
                recorded: recorded.display().to_string(),
                restored: restored.display().to_string(),
            }],
        }
    }

    pub(super) fn with_sources(mut self, sources: &[super::ReplaySourceRoot]) -> Self {
        self.roots.extend(sources.iter().map(|root| RootMapping {
            recorded: root.recorded.display().to_string(),
            restored: root.restored.display().to_string(),
        }));
        self.roots
            .sort_by_key(|root| std::cmp::Reverse(root.recorded.len()));
        self
    }

    fn with_ancestors(mut self, roots: &[PathBuf], restored: &Path) -> Self {
        self.roots.extend(roots.iter().map(|root| RootMapping {
            recorded: root.display().to_string(),
            restored: restored.display().to_string(),
        }));
        self
    }

    /// Replace a complete root, including inside prose, shell words and JSON
    /// strings. A prefix of another path component is never a root match.
    pub fn text(&self, text: &str) -> String {
        self.rewrite(text)
    }

    pub fn json(&self, value: &mut Value) {
        self.map_json(value);
    }

    fn map_json(&self, value: &mut Value) {
        match value {
            Value::String(text) => *text = self.rewrite(text),
            Value::Array(values) => values.iter_mut().for_each(|v| self.map_json(v)),
            Value::Object(values) => values.values_mut().for_each(|v| self.map_json(v)),
            _ => {}
        }
    }

    fn rewrite(&self, text: &str) -> String {
        let mut result = String::with_capacity(text.len());
        let mut offset = 0;
        while offset < text.len() {
            let rest = &text[offset..];
            let matched = self.roots.iter().find_map(|root| {
                let (from, to) = (&root.recorded, &root.restored);
                if !root_match(text, offset, from) {
                    return None;
                }
                Some((from.len(), to))
            });
            let existing = self
                .roots
                .iter()
                .map(|root| &root.restored)
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
    Some(for_branch(&branch))
}

pub(super) fn for_branch(branch: &super::BranchHandle) -> ReplayWorkspaceProjection {
    let sources: Vec<_> = branch
        .source_roots()
        .iter()
        .map(|root| super::ReplaySourceRoot {
            recorded: root.restored.clone(),
            restored: root.recorded.clone(),
        })
        .collect();
    ReplayWorkspaceProjection::new(branch.workspace(), branch.logical_workspace())
        .with_ancestors(&branch.recorded_workspaces(), branch.logical_workspace())
        .with_sources(&sources)
}

pub(super) fn identity_for(branch: &super::BranchHandle) -> ReplayWorkspaceIdentity {
    ReplayWorkspaceIdentity {
        logical_root: branch.logical_workspace().to_path_buf(),
        physical_root: branch.workspace().to_path_buf(),
    }
}

pub(crate) fn physical_projection() -> Option<ReplayWorkspaceProjection> {
    let branch = super::scope::state_branch()?;
    Some(physical_for_branch(&branch))
}

fn physical_for_branch(branch: &super::BranchHandle) -> ReplayWorkspaceProjection {
    ReplayWorkspaceProjection::new(
        branch.recorded_workspace().unwrap_or(branch.workspace()),
        branch.workspace(),
    )
    .with_ancestors(&branch.recorded_workspaces(), branch.workspace())
    .with_sources(branch.source_roots())
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
    }
}
