//! The run tape's view of one agent tool dispatch.
//!
//! A dispatch is taped whole: what the agent loop got back after policy,
//! approval, hooks, execution and post-tool interception. A branch prefix
//! serves that value in place of the whole sequence, so no prompt, hook or
//! tool runs a second time.

use crate::run_tape::{RecordedToolResult, ToolCallRecording, ToolServe};
use crate::testbench::tape::CommandEffect;
use crate::tool_annotations::SideEffectLevel;
use crate::value::{DictMap, VmError, VmValue};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

tokio::task_local! {
    static IN_RECORDED_DISPATCH: Arc<DispatchEffects>;
}

#[derive(Default)]
struct DispatchEffects {
    command_requested: AtomicBool,
    background: AtomicBool,
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
    let effects = Arc::new(DispatchEffects::default());
    let live = IN_RECORDED_DISPATCH.scope(effects.clone(), live);
    let result = crate::run_tape::capture_transcript_changes(&mut recording, live)
        .await
        .map(crate::run_tape::workspace::project_value);
    if let Some(mut recording) = recording {
        recording.command_effect = if effects.command_requested.load(Ordering::Acquire) {
            CommandEffect::Requested
        } else {
            CommandEffect::None
        };
        let outcome = if effects.background.load(Ordering::Acquire) {
            Err("a background command outlives the recorded dispatch".to_string())
        } else {
            recorded_from_result(&result)
        };
        crate::run_tape::finish_tool_call(recording, outcome);
    }
    result
}

/// A command started in the foreground outlived its inline window and keeps
/// running as a handle, so the recorded dispatch around it cannot be replayed.
pub fn note_command_outlived_dispatch() {
    let _ =
        IN_RECORDED_DISPATCH.try_with(|effects| effects.background.store(true, Ordering::Release));
}

/// Record a host command, or serve its result and workspace effects.
/// Commands inside an agent dispatch belong to that dispatch's recording.
pub async fn taped_command_run(
    name: &str,
    args: &[VmValue],
    background: bool,
    live: impl std::future::Future<Output = Result<VmValue, VmError>>,
) -> Result<VmValue, VmError> {
    if IN_RECORDED_DISPATCH.try_with(|_| ()).is_ok() {
        return run_command(name, background, live)
            .await
            .map(crate::run_tape::workspace::project_value);
    }
    if background {
        crate::run_tape::refuse_in_prefix(name, "a background command cannot be replayed")?;
    }
    let tool = format!("process:{name}");
    let args = serde_json::Value::Array(
        args.iter()
            .map(crate::llm::helpers::vm_value_to_json)
            .collect(),
    );
    match begin_with("", &tool, &args, Caller::HostCommand) {
        Taped::Served(result) => result,
        Taped::Live(recording) => {
            run_recorded(recording, run_command(name, background, live)).await
        }
    }
}

async fn run_command(
    name: &str,
    background: bool,
    live: impl std::future::Future<Output = Result<VmValue, VmError>>,
) -> Result<VmValue, VmError> {
    let _ = IN_RECORDED_DISPATCH
        .try_with(|effects| effects.command_requested.store(true, Ordering::Release));
    if background {
        crate::run_tape::refuse_in_prefix(name, "a background command cannot be replayed")?;
        if crate::run_tape::state_branch().is_some_and(|branch| branch.relocated()) {
            return Err(VmError::Runtime(
                "background command cannot retain a relocated replay filesystem namespace".into(),
            ));
        }
    }
    crate::run_tape::check_live_command_workspace()?;
    if background {
        note_command_outlived_dispatch();
    }
    live.await
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
    begin_with(&id, &name, &args, Caller::Tool { read_only })
}

/// Who is dispatching: the model through a tool, or the script running a host command.
enum Caller {
    Tool { read_only: bool },
    HostCommand,
}

fn begin_with(id: &str, name: &str, args: &serde_json::Value, caller: Caller) -> Taped {
    let mut logical_args = args.clone();
    crate::run_tape::workspace::project_json(&mut logical_args);
    let args = &logical_args;
    // Recording starts before serving, so a branch's own tape keeps what it
    // served and the effects it re-applied.
    let read_only = matches!(caller, Caller::Tool { read_only: true });
    let mut recording = crate::run_tape::begin_tool_call(id, name, args, read_only);
    if let (Some(recording), Caller::HostCommand) = (recording.as_mut(), caller) {
        recording.request = Some(Box::new(readable_command_request(args)));
    }
    match crate::run_tape::serve_tool_call(id, name, args) {
        ToolServe::Live => Taped::Live(recording),
        ToolServe::Served(recorded, changes, store_changes, command_effect) => {
            if let Some(mut recording) = recording {
                recording.transcript_changes = changes;
                recording.store_changes = store_changes;
                recording.command_effect = command_effect;
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

/// What a tape keeps of a host command's request for a reader. An `env` map
/// becomes its sorted variable names: a value can be a credential under any
/// name, and the redaction policy recognizes credentials by name only for some
/// spellings. The rest passes through the shared redaction policy here rather
/// than in the recorder. The recorder marks a tape it had to redact as unable
/// to branch, because served bytes would differ, and serving never reads this
/// request; the args digest still covers the unredacted request.
fn readable_command_request(args: &serde_json::Value) -> serde_json::Value {
    let mut request = args.clone();
    if let serde_json::Value::Array(items) = &mut request {
        for item in items {
            let Some(params) = item.as_object_mut() else {
                continue;
            };
            if let Some(serde_json::Value::Object(env)) = params.get("env") {
                let mut names: Vec<String> = env.keys().cloned().collect();
                names.sort();
                params.insert("env".to_string(), names.into());
            }
        }
    }
    crate::redact::current_policy().redact_json_in_place(&mut request);
    request
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
