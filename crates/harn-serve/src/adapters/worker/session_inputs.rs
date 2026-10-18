use harn_vm::agent_sessions::{self, CurrentSessionGuard};
use harn_vm::session_inputs::{self, PrivateInputSnapshot};

use super::DispatchError;

pub(super) struct JobInputScope {
    id: String,
    _current: CurrentSessionGuard,
}

impl JobInputScope {
    pub(super) fn enter(snapshot: PrivateInputSnapshot) -> Result<Self, DispatchError> {
        let store = session_inputs::Store::default();
        store
            .lock()
            .apply_private_snapshot(snapshot, session_inputs::now_ms())
            .map_err(|error| DispatchError::Execution(error.to_string()))?;
        let id = agent_sessions::open_or_create(None)
            .map_err(|error| DispatchError::Execution(error.to_string()))?;
        session_inputs::bind(&id, store);
        let current = agent_sessions::enter_current_session(id.clone());
        Ok(Self {
            id,
            _current: current,
        })
    }
}

impl Drop for JobInputScope {
    fn drop(&mut self) {
        agent_sessions::close(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{run_job_once_with_options, JobRunOptions};
    use harn_vm::session_inputs::{InputKind, InputLifetime, InputSpec, InputUsage, SessionInputs};

    #[tokio::test(flavor = "current_thread")]
    async fn hosted_job_session_inputs_reach_the_consumer_but_never_the_result_or_next_run() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let directory = tempfile::tempdir().unwrap();
                let script = super::super::test_support::write_script(
                    directory.path(),
                    r#"
import "std/triggers"

@job("inspect_inputs")
pub fn inspect_inputs(harness: Harness, _event: TriggerEvent) -> dict {
  return {reached: true, inputs: harness.secrets.inputs(), consumed: private_input_consumer()}
}
"#,
                )
                .await;
                let mut inputs = SessionInputs::default();
                inputs
                    .put(
                        InputSpec {
                            name: "SERVICE_KEY".into(),
                            kind: InputKind::Secret,
                            lifetime: InputLifetime::Session,
                            usage: InputUsage::SessionTools,
                        },
                        "hosted-job-canary".into(),
                        false,
                        0,
                    )
                    .unwrap();
                let mut bytes = zeroize::Zeroizing::new(Vec::new());
                inputs
                    .write_private_snapshot("hosted-owner", 0, &mut *bytes)
                    .unwrap();
                let snapshot = serde_json::from_slice(&bytes).unwrap();
                for grant in [Some(snapshot), None] {
                    let has_grant = grant.is_some();
                    let mut options = JobRunOptions::fail_fast();
                    if let Some(grant) = grant {
                        options = options.with_session_inputs(grant);
                    }
                    let outcome = run_job_once_with_options(
                        &script,
                        "inspect_inputs",
                        serde_json::json!({}),
                        options,
                        |vm| {
                            vm.register_async_builtin(
                                "private_input_consumer",
                                |_ctx, _args| async {
                                    let environment = session_inputs::command_environment(
                                        std::collections::BTreeMap::from([(
                                            "SERVICE_KEY".into(),
                                            "SERVICE_KEY".into(),
                                        )]),
                                    );
                                    Ok(harn_vm::value::VmValue::Bool(environment.is_ok_and(
                                        |values| {
                                            values
                                                .get("SERVICE_KEY")
                                                .is_some_and(|value| value == "hosted-job-canary")
                                        },
                                    )))
                                },
                            );
                        },
                    )
                    .await
                    .unwrap();
                    assert!(outcome.succeeded(), "{:?}", outcome.error);
                    let result = outcome.result.unwrap();
                    assert_eq!(result["reached"], true);
                    assert_eq!(result["consumed"], has_grant);
                    assert_eq!(
                        result["inputs"].as_array().unwrap().len(),
                        usize::from(has_grant)
                    );
                    assert!(!result.to_string().contains("hosted-job-canary"));
                }
            })
            .await;
    }
}
