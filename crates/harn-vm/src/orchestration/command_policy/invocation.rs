use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::value::{DictMap, VmDictExt, VmError};

pub(crate) async fn run_command_policy_preflight_with_origin(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    params: &DictMap,
    caller: JsonValue,
    origin: super::CommandDispatchOrigin,
) -> Result<super::CommandPolicyPreflight, VmError> {
    super::run_prepared_command_policy_preflight(ctx, params, caller, origin, |params| async move {
        Ok(params)
    })
    .await
}

/// An executor's retained request, prepared before assessment and consumed only
/// after consent. Rewrites replace the entire preparation, never its audit view.
pub trait CommandPolicyParameters {
    fn parameters(&self) -> &DictMap;

    fn invocation(&self) -> Option<JsonValue> {
        None
    }

    fn local_filesystem(&self) -> bool {
        true
    }
}

impl CommandPolicyParameters for DictMap {
    fn parameters(&self) -> &DictMap {
        self
    }
}

pub(super) fn prepared_command_context<P: CommandPolicyParameters>(
    prepared: &P,
    policy: &super::CommandPolicy,
    caller: JsonValue,
) -> JsonValue {
    let mut context = super::command_context_json(prepared.parameters(), policy, caller);
    context["invocation"] = prepared.invocation().unwrap_or(JsonValue::Null);
    context["executor_filesystem_local"] = JsonValue::Bool(prepared.local_filesystem());
    context
}

/// Launch facts supplied by the executor, not inferred from a display string.
/// A remote directory is validated by that executor, never by the VM host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandInvocation {
    #[schemars(length(min = 1))]
    argv: Vec<String>,
    cwd: PathBuf,
}

impl CommandInvocation {
    pub fn new(argv: Vec<String>, cwd: PathBuf) -> Result<Self, VmError> {
        let valid_cwd = cwd.to_str().is_some_and(|cwd| {
            !cwd.contains('\0') && crate::workspace_path::is_absolute_path_syntax(cwd)
        });
        if argv.first().is_none_or(String::is_empty)
            || argv.iter().any(|arg| arg.contains('\0'))
            || !valid_cwd
        {
            return Err(VmError::Runtime(
                "invalid prepared command invocation".into(),
            ));
        }
        Ok(Self { argv, cwd })
    }

    pub fn argv(&self) -> &[String] {
        &self.argv
    }

    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub fn audit(&self) -> serde_json::Value {
        serde_json::json!({"argv": self.argv, "cwd": self.cwd})
    }

    pub fn bind_directory(&self, params: &mut DictMap) {
        params.put_str(
            "cwd",
            self.cwd.to_str().expect("validated invocation directory"),
        );
    }
}

impl<'de> Deserialize<'de> for CommandInvocation {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            argv: Vec<String>,
            cwd: PathBuf,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.argv, wire.cwd).map_err(serde::de::Error::custom)
    }
}

/// Resolve a local launch directory once, including sandbox-selected inheritance.
pub fn prepare_local_command_cwd(requested: Option<&str>) -> Result<PathBuf, VmError> {
    let path = match requested.filter(|path| !path.is_empty()) {
        Some(path) => crate::stdlib::process::resolve_source_relative_path(path),
        None => crate::stdlib::process::inherited_process_cwd()?,
    };
    let canonical = path
        .canonicalize()
        .map_err(|error| VmError::Runtime(format!("command cwd {}: {error}", path.display())))?;
    if !canonical.is_dir() {
        return Err(VmError::Runtime(format!(
            "command cwd is not a directory: {}",
            canonical.display()
        )));
    }
    crate::process_sandbox::enforce_process_cwd(&canonical)?;
    Ok(crate::stdlib::process::child_process_cwd(canonical))
}
