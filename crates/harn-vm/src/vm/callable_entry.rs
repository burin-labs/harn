use std::sync::Arc;
use std::time::Duration;

use crate::chunk::ChunkRef;
use crate::value::{VmError, VmValue};

use super::{ScopeSpan, Vm};

pub(super) enum TopLevelEntry {
    Chunk(ChunkRef),
    Callable {
        bootstrap: ChunkRef,
        has_fixture: bool,
        fixture_expects_harness: bool,
        expects_harness: bool,
        args: Vec<VmValue>,
    },
}

impl TopLevelEntry {
    pub(super) async fn run(self, vm: &mut Vm) -> Result<VmValue, VmError> {
        match self {
            Self::Chunk(chunk) => vm.run_chunk(chunk).await,
            Self::Callable {
                bootstrap,
                has_fixture,
                fixture_expects_harness,
                expects_harness,
                mut args,
            } => {
                let value = vm.run_chunk(bootstrap).await?;
                let target = if has_fixture {
                    let VmValue::List(callables) = value else {
                        return Err(VmError::Runtime(
                            "callable entry bootstrap did not return [fixture, target]".to_string(),
                        ));
                    };
                    let [VmValue::Closure(fixture), VmValue::Closure(target)] =
                        callables.as_slice()
                    else {
                        return Err(VmError::Runtime(
                            "callable entry bootstrap returned invalid fixture callables"
                                .to_string(),
                        ));
                    };
                    let fixture_args = if fixture_expects_harness {
                        vec![vm.root_harness_value().ok_or_else(|| {
                            VmError::Runtime(
                                "test fixture requires Harness, but no root Harness is installed"
                                    .to_string(),
                            )
                        })?]
                    } else {
                        Vec::new()
                    };
                    let fixture_value = vm.call_closure_pub(fixture, &fixture_args).await?;
                    args.insert(0, fixture_value);
                    Arc::clone(target)
                } else {
                    let VmValue::Closure(target) = value else {
                        return Err(VmError::Runtime(
                            "callable entry bootstrap did not return a callable".to_string(),
                        ));
                    };
                    target
                };
                if expects_harness {
                    let harness = vm.root_harness_value().ok_or_else(|| {
                        VmError::Runtime(
                            "callable entry requires Harness, but no root Harness is installed"
                                .to_string(),
                        )
                    })?;
                    args.insert(0, harness);
                }
                vm.call_closure_pub(&target, &args).await
            }
        }
    }
}

impl Vm {
    /// Execute a compiled callable entry with explicit values under a host
    /// wall-clock limit.
    ///
    /// The entry bootstrap initializes top-level state once. A bundled fixture
    /// is then invoked with no arguments and its result is prepended to `args`;
    /// the target callable is invoked through ordinary arity/type guards.
    /// Pipeline-finish hooks wrap the complete operation exactly once.
    pub async fn execute_callable_entry_with_timeout(
        &mut self,
        entry: &crate::CompiledCallableEntry,
        args: &[VmValue],
        timeout: Duration,
    ) -> Result<VmValue, VmError> {
        self.execute_top_level_with_timeout(
            TopLevelEntry::Callable {
                bootstrap: Arc::new(entry.bootstrap.clone()),
                has_fixture: entry.has_fixture,
                fixture_expects_harness: entry.fixture_expects_harness,
                expects_harness: entry.expects_harness,
                args: args.to_vec(),
            },
            timeout,
        )
        .await
    }

    pub(super) async fn execute_top_level(
        &mut self,
        entry: TopLevelEntry,
    ) -> Result<VmValue, VmError> {
        self.ensure_execution_available()?;
        let registry = self.pool_registry.clone();
        let owner = crate::observability::execution_scope::mint_execution_scope();
        let ambient = crate::orchestration::AmbientExecutionScope::capture_for_top_level_execution(
            owner,
            self.llm_mock_context.clone(),
        );
        let execution = crate::stdlib::pool::with_pool_registry_scope(registry, async {
            self.execute_entry_scoped(entry).await
        });
        crate::orchestration::scope_ambient(ambient, execution).await
    }

    async fn execute_entry_scoped(&mut self, entry: TopLevelEntry) -> Result<VmValue, VmError> {
        let _execution_activity = self
            .wait_for_graph
            .register_task(self.runtime_context.task_id.clone());
        let _span = ScopeSpan::new(crate::tracing::SpanKind::Pipeline, "main".into());
        match entry.run(self).await {
            Ok(value) => self.run_pipeline_finish_lifecycle(value).await,
            Err(error) => {
                crate::orchestration::clear_pipeline_on_finish();
                Err(error)
            }
        }
    }
}
