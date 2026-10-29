//! The run tape's view of one agent tool dispatch.
//!
//! A dispatch is taped whole: what the agent loop got back after policy,
//! approval, hooks, execution and post-tool interception. A branch prefix
//! serves that value in place of the whole sequence, so no prompt, hook or
//! tool runs a second time.

use crate::run_tape::{RecordedToolResult, ToolCallRecording, ToolServe};
use crate::tool_annotations::SideEffectLevel;
use crate::value::{DictMap, VmError, VmValue};

/// Dispatch one agent tool call: served whole from a branch prefix, or run
/// live and recorded.
pub(crate) async fn host_agent_dispatch_tool_call(
    ctx: crate::vm::AsyncBuiltinCtx,
    call: VmValue,
    tools: Option<&VmValue>,
    options: &DictMap,
) -> Result<VmValue, VmError> {
    let call = match call {
        VmValue::Dict(call) => call,
        other => {
            return Err(VmError::Runtime(format!(
            "__host_agent_dispatch_tool_call(call, tools?, options?): call must be a dict; got {}",
            other.type_name()
        )))
        }
    };
    let mut recording = match begin(&call, tools) {
        Taped::Served(result) => return result,
        Taped::Live(recording) => recording,
    };
    let live = Box::pin(IN_TOOL_DISPATCH.scope(
        (),
        super::dispatch_tool_call_live(ctx, call, tools, options),
    ));
    let result = crate::run_tape::capture_tool_reminders(&mut recording, live).await;
    finish(recording, &result);
    result
}

tokio::task_local! {
    /// Set while a tool dispatch runs: the dispatch is taped whole, so what
    /// its hooks and handler run is not taped again.
    static IN_TOOL_DISPATCH: ();
}

/// Run one process outside any tool dispatch, taped like a tool call: its
/// result and the workspace changes it made are recorded, and a branch prefix
/// serves them instead of running it.
pub(crate) async fn taped_process_run(
    name: &str,
    args: &[VmValue],
    live: impl std::future::Future<Output = Result<VmValue, VmError>>,
) -> Result<VmValue, VmError> {
    if IN_TOOL_DISPATCH.try_with(|()| ()).is_ok() {
        return live.await;
    }
    let tool = format!("process:{name}");
    let args = serde_json::Value::Array(
        args.iter()
            .map(crate::llm::helpers::vm_value_to_json)
            .collect(),
    );
    match begin_with("", &tool, &args, false) {
        Taped::Served(result) => result,
        Taped::Live(recording) => {
            let result = live.await;
            finish(recording, &result);
            result
        }
    }
}

enum Taped {
    /// The branch answered the call; return this.
    Served(Result<VmValue, VmError>),
    /// Run the call, then pass its result to [`finish`].
    Live(Option<ToolCallRecording>),
}

/// Look the call up on the tape before it dispatches.
fn begin(call: &DictMap, tools: Option<&VmValue>) -> Taped {
    let id = ["id", "tool_call_id", "call_id"]
        .iter()
        .find_map(|key| match call.get(*key) {
            Some(VmValue::String(id)) if !id.is_empty() => Some(id.to_string()),
            _ => None,
        })
        .unwrap_or_default();
    let name = super::primitive_args::qualified_tool_name(call).unwrap_or_default();
    let args = call
        .get("arguments")
        .map(crate::llm::helpers::vm_value_to_json)
        .unwrap_or(serde_json::Value::Null);
    let read_only = super::tool_catalog::annotations_for(tools, &name).is_some_and(|annotations| {
        matches!(
            annotations.side_effect_level,
            SideEffectLevel::ReadOnly | SideEffectLevel::NetworkRead
        )
    });
    begin_with(&id, &name, &args, read_only)
}

fn begin_with(id: &str, name: &str, args: &serde_json::Value, read_only: bool) -> Taped {
    // Recording starts before serving, so a branch's own tape keeps what it
    // served and the effects it re-applied.
    let recording = crate::run_tape::begin_tool_call(id, name, args, read_only);
    match crate::run_tape::serve_tool_call(id, name, args) {
        ToolServe::Live => Taped::Live(recording),
        ToolServe::Served(recorded, reminders) => {
            if let Some(mut recording) = recording {
                recording.reminders = reminders;
                crate::run_tape::finish_tool_call(recording, Ok(recorded.clone()));
            }
            Taped::Served(result_from_recorded(recorded))
        }
        ToolServe::Diverged(error) => {
            if let Some(recording) = recording {
                crate::run_tape::finish_tool_call(
                    recording,
                    Err("the branch prefix diverged".to_string()),
                );
            }
            Taped::Served(Err(error))
        }
    }
}

/// Record what a live dispatch returned.
fn finish(recording: Option<ToolCallRecording>, result: &Result<VmValue, VmError>) {
    if let Some(recording) = recording {
        crate::run_tape::finish_tool_call(recording, recorded_from_result(result));
    }
}

/// Project a dispatch result onto the tape's recorded shape. Errors with no
/// faithful recorded form are named, so a branch refuses those calls instead
/// of serving an approximation.
fn recorded_from_result(result: &Result<VmValue, VmError>) -> Result<RecordedToolResult, String> {
    Ok(match result {
        Ok(value) => RecordedToolResult::Ok {
            value: crate::llm::helpers::vm_value_to_json(value),
        },
        Err(VmError::CategorizedError { message, category }) => RecordedToolResult::Categorized {
            message: message.clone(),
            category: category.as_str().to_string(),
        },
        Err(VmError::Thrown(VmValue::String(message))) => RecordedToolResult::Thrown {
            message: message.to_string(),
        },
        Err(VmError::Thrown(value)) => RecordedToolResult::ThrownValue {
            value: crate::llm::helpers::vm_value_to_json(value),
        },
        Err(VmError::Runtime(message)) => RecordedToolResult::Runtime {
            message: message.clone(),
        },
        Err(other) => return Err(format!("tool error has no recorded shape: {other}")),
    })
}

fn result_from_recorded(recorded: RecordedToolResult) -> Result<VmValue, VmError> {
    match recorded {
        RecordedToolResult::Ok { value } => Ok(crate::schema::json_to_vm_value(&value)),
        RecordedToolResult::Categorized { message, category } => Err(VmError::CategorizedError {
            message,
            category: crate::value::ErrorCategory::parse(&category),
        }),
        RecordedToolResult::Thrown { message } => {
            Err(VmError::Thrown(VmValue::String(message.into())))
        }
        RecordedToolResult::ThrownValue { value } => {
            Err(VmError::Thrown(crate::schema::json_to_vm_value(&value)))
        }
        RecordedToolResult::Runtime { message } => Err(VmError::Runtime(message)),
    }
}
