use std::{future::Future, pin::Pin, sync::Arc};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    orchestration::CommandInvocation,
    value::{DictMap, VmError, VmValue},
};

use super::{HostCommandPreparation, PreparedHostCommand};
use crate::stdlib::host::trace::HostRequestTrace;

pub type CommandTransportFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Value, VmError>> + Send + 'a>>;

/// Transport for the executor's prepared-command protocol. Cleanup requests
/// remain deliverable after cancellation of the operation that created them.
pub trait PreparedCommandTransport: Send + Sync {
    fn call<'a>(
        &'a self,
        operation: &'a str,
        args: Value,
        trace: &'a HostRequestTrace,
        cleanup: bool,
    ) -> CommandTransportFuture<'a>;
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Preparation {
    Bound {
        #[schemars(length(min = 1, max = 4096))]
        token: String,
        invocation: CommandInvocation,
    },
    Unhandled,
    Unsupported,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CommandRelease {
    status: Released,
    token: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Released {
    Released,
}

pub fn command_binding_schemas() -> [(&'static str, Value); 2] {
    [
        (
            "CommandPreparation",
            serde_json::to_value(schemars::schema_for!(Preparation))
                .expect("command preparation schema"),
        ),
        (
            "CommandRelease",
            serde_json::to_value(schemars::schema_for!(CommandRelease))
                .expect("command release schema"),
        ),
    ]
}

async fn release(
    transport: &dyn PreparedCommandTransport,
    token: &str,
    trace: &HostRequestTrace,
) -> Result<(), VmError> {
    let reply = transport
        .call("release_prepared", json!({"token": token}), trace, true)
        .await?;
    acknowledge_release(reply, token)
}

fn acknowledge_release(reply: Value, token: &str) -> Result<(), VmError> {
    let CommandRelease {
        status: Released::Released,
        token: released,
    } = serde_json::from_value(reply).map_err(|error| {
        VmError::Runtime(format!("invalid executor release acknowledgement: {error}"))
    })?;
    if released != token {
        return Err(VmError::Runtime(
            "executor released a different command token".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preparation_and_release_require_closed_executor_facts() {
        let bound = json!({"status":"bound", "token":"one", "invocation":{
            "argv":["python3", "", " spaced argument ", "日本語"], "cwd":"/executor/workspace",
        }});
        let Preparation::Bound { invocation, .. } = serde_json::from_value(bound.clone()).unwrap()
        else {
            panic!("the positive control must bind an executor");
        };
        assert_eq!(
            invocation.argv(),
            ["python3", "", " spaced argument ", "日本語"]
        );
        let schemas = command_binding_schemas();
        assert!(jsonschema::validator_for(&schemas[0].1)
            .unwrap()
            .is_valid(&bound));
        for (pointer, value) in [
            ("/invocation/argv", json!([])),
            ("/invocation/argv", json!(["python3", 3])),
            ("/invocation/argv", json!(["python3", "nul\u{0}byte"])),
            ("/invocation/cwd", json!("relative/workspace")),
            ("/invocation/cwd", Value::Null),
            ("/status", json!("unknown")),
        ] {
            let mut invalid = bound.clone();
            *invalid.pointer_mut(pointer).unwrap() = value;
            assert!(
                serde_json::from_value::<Preparation>(invalid).is_err(),
                "{pointer}"
            );
        }
        let released = json!({"status":"released", "token":"one"});
        assert!(acknowledge_release(released.clone(), "one").is_ok());
        assert!(jsonschema::validator_for(&schemas[1].1)
            .unwrap()
            .is_valid(&released));
        for invalid in [
            json!({}),
            json!({"status":"released"}),
            json!({"status":"pending", "token":"one"}),
            json!({"status":"released", "token":"another"}),
            json!({"status":"released", "token":"one", "still_running":true}),
        ] {
            assert!(acknowledge_release(invalid, "one").is_err());
        }
    }
}

pub async fn prepare_remote_command(
    transport: Arc<dyn PreparedCommandTransport>,
    operation: &str,
    params: &DictMap,
    trace: &HostRequestTrace,
) -> Result<HostCommandPreparation, VmError> {
    let cleanup_runtime = tokio::runtime::Handle::try_current().map_err(|error| {
        VmError::Runtime(format!(
            "prepared host command needs an async cleanup runtime: {error}"
        ))
    })?;
    let reply = transport
        .call(
            "prepare",
            json!({
                "operation": operation,
                "request": crate::llm::vm_value_to_json(&VmValue::dict(params.clone())),
            }),
            trace,
            false,
        )
        .await?;
    match serde_json::from_value(reply).map_err(|error| {
        VmError::Runtime(format!("invalid executor command preparation: {error}"))
    })? {
        Preparation::Bound { token, invocation } => {
            if token.is_empty() || token.len() > 4096 {
                return Err(VmError::Runtime("invalid executor command token".into()));
            }
            Ok(HostCommandPreparation::Bound(Box::new(BoundProcess {
                transport,
                token: Some(token),
                invocation,
                trace: trace.clone(),
                cleanup_runtime,
            })))
        }
        Preparation::Unhandled => Ok(HostCommandPreparation::Unhandled),
        Preparation::Unsupported => Ok(HostCommandPreparation::Unsupported),
    }
}

struct BoundProcess {
    transport: Arc<dyn PreparedCommandTransport>,
    token: Option<String>,
    invocation: CommandInvocation,
    trace: HostRequestTrace,
    cleanup_runtime: tokio::runtime::Handle,
}

impl PreparedHostCommand for BoundProcess {
    fn invocation(&self) -> &CommandInvocation {
        &self.invocation
    }

    fn execute(
        mut self: Box<Self>,
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + Send>> {
        Box::pin(async move {
            let result = self
                .transport
                .call(
                    "execute_prepared",
                    json!({"token": self.token}),
                    &self.trace,
                    false,
                )
                .await;
            let token = self.token.as_ref().expect("bound command token");
            release(self.transport.as_ref(), token, &self.trace).await?;
            self.token.take();
            result.map(|result| crate::bridge::json_result_to_vm_value(&result))
        })
    }
}

impl Drop for BoundProcess {
    fn drop(&mut self) {
        let Some(token) = self.token.take() else {
            return;
        };
        let transport = self.transport.clone();
        let trace = self.trace.clone();
        // Unused preparations never started a child. A dropped execution still
        // sends cancellation; an awaited execution waits for cleanup's reply.
        self.cleanup_runtime.spawn(async move {
            if let Err(error) = release(transport.as_ref(), &token, &trace).await {
                tracing::warn!(%error, "releasing executor-bound command failed");
            }
        });
    }
}
