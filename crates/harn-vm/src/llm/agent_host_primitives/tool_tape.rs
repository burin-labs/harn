//! The run tape's view of one agent tool dispatch.
//!
//! A dispatch is taped whole: what the agent loop got back after policy,
//! approval, hooks, execution and post-tool interception. A branch prefix
//! serves that value in place of the whole sequence, so no prompt, hook or
//! tool runs a second time.

use crate::run_tape::{RecordedToolResult, ToolCallRecording, ToolServe};
use crate::tool_annotations::SideEffectLevel;
use crate::value::{DictMap, VmError, VmValue};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

tokio::task_local! {
    static IN_RECORDED_DISPATCH: Arc<AtomicBool>;
}

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
    let recording = match begin(&call, tools) {
        Taped::Served(result) => return result,
        Taped::Live(recording) => recording,
    };
    let live = Box::pin(super::dispatch_tool_call_live(ctx, call, tools, options));
    run_recorded(recording, live).await
}

async fn run_recorded(
    mut recording: Option<ToolCallRecording>,
    live: impl std::future::Future<Output = Result<VmValue, VmError>>,
) -> Result<VmValue, VmError> {
    let background = Arc::new(AtomicBool::new(false));
    let live = IN_RECORDED_DISPATCH.scope(background.clone(), live);
    let result = crate::run_tape::capture_transcript_changes(&mut recording, live).await;
    if let Some(recording) = recording {
        let outcome = if background.load(Ordering::Acquire) {
            Err("a background command outlives the recorded dispatch".to_string())
        } else {
            recorded_from_result(&result)
        };
        crate::run_tape::finish_tool_call(recording, outcome);
    }
    result
}

/// Record a foreground host command, or serve its result and workspace effects.
/// Commands inside an agent dispatch belong to that dispatch's recording.
pub async fn taped_command_run(
    name: &str,
    args: &[VmValue],
    background: bool,
    live: impl std::future::Future<Output = Result<VmValue, VmError>>,
) -> Result<VmValue, VmError> {
    if background {
        crate::run_tape::refuse_in_prefix(name, "a background command cannot be replayed")?;
        let _ = IN_RECORDED_DISPATCH.try_with(|flag| flag.store(true, Ordering::Release));
        return live.await;
    }
    if IN_RECORDED_DISPATCH.try_with(|_| ()).is_ok() {
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
        Taped::Live(recording) => run_recorded(recording, live).await,
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
        ToolServe::Served(recorded, changes) => {
            if let Some(mut recording) = recording {
                recording.transcript_changes = changes;
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
