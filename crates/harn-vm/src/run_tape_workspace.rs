//! The in-memory filesystem view of a replay. Tape payloads stay immutable.

use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub(super) struct RecordedWorkspace {
    pub root: Option<PathBuf>,
    pub aliases: Vec<PathBuf>,
    pub ancestors: Vec<PathBuf>,
}

impl RecordedWorkspace {
    pub fn from_tape(tape: &super::EventTape) -> Result<Self, String> {
        let Some(run) = &tape.header.run else {
            return Ok(Self::default());
        };
        let recorded = Self {
            root: run.workspace_root.clone(),
            aliases: run.workspace_aliases.clone(),
            ancestors: run.workspace_ancestors.clone(),
        };
        if recorded
            .ancestors
            .iter()
            .chain(recorded.aliases.iter())
            .any(|root| {
                !root.is_absolute()
                    || root
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir))
            })
        {
            return Err("recorded workspace names must be absolute normalized paths".into());
        }
        Ok(recorded)
    }

    pub fn roots(&self) -> Vec<PathBuf> {
        let mut roots = self.physical_roots();
        for alias in &self.aliases {
            if !roots.contains(alias) {
                roots.push(alias.clone());
            }
        }
        roots
    }

    pub fn physical_roots(&self) -> Vec<PathBuf> {
        let mut roots = self.ancestors.clone();
        if let Some(root) = &self.root {
            if !roots.contains(root) {
                roots.push(root.clone());
            }
        }
        roots
    }

    pub fn identity(&self, physical_root: &Path) -> ReplayWorkspaceIdentity {
        ReplayWorkspaceIdentity {
            logical_root: self
                .ancestors
                .first()
                .or(self.root.as_ref())
                .map_or_else(|| physical_root.to_path_buf(), Clone::clone),
            physical_root: physical_root.to_path_buf(),
        }
    }
}

pub(super) fn recording_workspace(
    workspace: Option<&super::TapeWorkspace>,
) -> Result<RecordedWorkspace, String> {
    let mut recorded = RecordedWorkspace {
        root: workspace.map(|workspace| {
            crate::workspace_path::canonicalize_existing_ancestor(&workspace.root)
                .unwrap_or_else(|_| workspace.root.clone())
        }),
        ..RecordedWorkspace::default()
    };
    if let Some(branch) = super::scope::state_branch().filter(|_| recorded.root.is_some()) {
        if recorded.root.as_deref() != Some(branch.workspace()) {
            return Err("a branch recording must name its scoped workspace".into());
        }
        recorded.ancestors = branch.recorded_physical_workspaces();
        recorded.aliases = branch.recorded_aliases().to_vec();
    }
    if let (Some(workspace), Some(canonical)) = (workspace, &recorded.root) {
        if workspace.root.is_absolute()
            && &workspace.root != canonical
            && !recorded.aliases.contains(&workspace.root)
        {
            recorded.aliases.push(workspace.root.clone());
        }
    }
    Ok(recorded)
}

/// The stable name seen by a replay and the directory that owns its effects.
#[derive(Clone, Debug)]
pub struct ReplayWorkspaceIdentity {
    logical_root: PathBuf,
    physical_root: PathBuf,
}

impl ReplayWorkspaceIdentity {
    /// The stable directory name used in pipeline and model data.
    pub fn logical_root(&self) -> &Path {
        &self.logical_root
    }
    /// The private working copy where filesystem effects land.
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
            let rest = text.get(offset..).expect("offset follows UTF-8 boundaries");
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
                result.push_str(rest.get(..len).expect("matched UTF-8 root"));
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
    let (Some(before), Some(rest)) = (text.get(..offset), text.get(offset..)) else {
        return false;
    };
    let Some(after) = rest.strip_prefix(root).filter(|_| !root.is_empty()) else {
        return false;
    };
    (before
        .chars()
        .next_back()
        .is_none_or(|c| !path_component(c) && c != '/' && c != '\\')
        || before.ends_with("file://"))
        && after.chars().next().is_none_or(|c| !path_component(c))
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
        .with_ancestors(
            &branch.recorded_physical_workspaces(),
            branch.logical_workspace(),
        )
        .with_sources(&sources)
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
        assert_eq!(projection.text("é '/recorded/猫'"), "é '/recorded/arm/猫'");
    }
}
