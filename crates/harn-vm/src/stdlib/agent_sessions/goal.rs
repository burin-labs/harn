use super::*;
use crate::agent_sessions::{GoalPinSpec, SessionGoalMetadata, SessionGoalState};
use crate::vm::AsyncBuiltinCtx;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GoalInput {
    spec: GoalPinSpec,
    metadata: SessionGoalMetadata,
}

harn_vm_core::stdlib_module_installer!(Host, fn register_goal_builtins);

fn snapshot(state: SessionGoalState) -> VmValue {
    crate::stdlib::json_to_vm_value(&serde_json::to_value(state).expect("typed goal serializes"))
}

fn goal_json(
    value: &VmValue,
    remaining: &mut usize,
    depth: usize,
) -> Result<serde_json::Value, VmError> {
    if depth > 64 || *remaining == 0 {
        return Err(err("session goal exceeds its bounded JSON shape"));
    }
    *remaining -= 1;
    match value {
        VmValue::Nil | VmValue::Bool(_) | VmValue::Int(_) => {
            Ok(crate::llm::helpers::vm_value_to_json(value))
        }
        VmValue::Float(number) if number.is_finite() => Ok(serde_json::json!(number)),
        VmValue::String(text) => {
            *remaining = remaining
                .checked_sub(text.len())
                .ok_or_else(|| err("session goal exceeds its byte limit"))?;
            Ok(serde_json::Value::String(text.to_string()))
        }
        VmValue::List(items) => items
            .iter()
            .map(|item| goal_json(item, remaining, depth + 1))
            .collect::<Result<Vec<_>, _>>()
            .map(serde_json::Value::Array),
        VmValue::Dict(fields) => goal_json_fields(fields, remaining, depth),
        VmValue::StructInstance(_) => goal_json_fields(
            &value.struct_fields_map().unwrap_or_default(),
            remaining,
            depth,
        ),
        _ => Err(err(format!(
            "session goal {} has no lossless JSON representation",
            value.type_name()
        ))),
    }
}

fn goal_json_fields(
    fields: &crate::value::DictMap,
    remaining: &mut usize,
    depth: usize,
) -> Result<serde_json::Value, VmError> {
    let mut result = serde_json::Map::new();
    for (key, value) in fields {
        *remaining = remaining
            .checked_sub(key.len())
            .ok_or_else(|| err("session goal exceeds its byte limit"))?;
        result.insert(key.to_string(), goal_json(value, remaining, depth + 1)?);
    }
    Ok(serde_json::Value::Object(result))
}

fn require_owned_run(ctx: &AsyncBuiltinCtx, id: &str) -> Result<(), VmError> {
    // Prepared runs own both cold hydration and cancellation cleanup. A goal
    // operation must never acquire a competing writer or silently stay volatile.
    let owned = agent_sessions::with_live_journal(id, |journal| {
        journal.execution_id() == Some(ctx.execution_id().as_str())
            && journal.task_id() == Some(ctx.task_id().as_str())
    });
    if owned != Some(true) {
        return Err(err(
            "session goal mutation requires this task's canonical run; use harness.agent.with_run",
        ));
    }
    Ok(())
}

#[harn_builtin(
    exposure = "runtime_internal", effects = [],
    sig = "agent_session_goal(id: string) -> dict",
    read_only_preparation = true,
    category = "agent.session",
    doc = "Read typed session goal state. Use with_run to hydrate a cold session from its canonical journal."
)]
fn goal(args: &[VmValue], _out: &mut String) -> Result<VmValue, VmError> {
    let id = arg_string_required(args, 0, "agent_session_goal", "id")?;
    agent_sessions::session_goal(&id).map(snapshot).map_err(err)
}

#[harn_builtin(
    exposure = "runtime_internal", effects = [], kind = "async",
    sig = "agent_session_set_goal(id: string, goal: dict) -> dict",
    category = "agent.session",
    doc = "Replace a canonical session goal using closed spec and metadata records. Requires the caller's prepared or active run."
)]
async fn set_goal(ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let id = arg_string_required(&args, 0, "agent_session_set_goal", "id")?;
    let value = args
        .get(1)
        .ok_or_else(|| err("agent_session_set_goal requires goal"))?;
    let mut remaining = agent_sessions::MAX_SESSION_GOAL_BYTES;
    let json = goal_json(value, &mut remaining, 0)?;
    let input: GoalInput = serde_json::from_value(json).map_err(|error| err(error.to_string()))?;
    require_owned_run(&ctx, &id)?;
    agent_sessions::set_session_goal(&id, input.spec, input.metadata)
        .map(snapshot)
        .map_err(err)
}

#[harn_builtin(
    exposure = "runtime_internal", effects = [], kind = "async",
    sig = "agent_session_clear_goal(id: string) -> bool",
    category = "agent.session",
    doc = "Clear a goal and advance its generation inside the caller's canonical run."
)]
async fn clear_goal(ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let id = arg_string_required(&args, 0, "agent_session_clear_goal", "id")?;
    require_owned_run(&ctx, &id)?;
    agent_sessions::clear_session_goal(&id)
        .map(VmValue::Bool)
        .map_err(err)
}

#[harn_builtin(
    exposure = "runtime_internal", effects = [], kind = "async",
    sig = "agent_session_begin_goal_turn(id: string) -> bool",
    category = "agent.session",
    doc = "Open a goal turn, retiring only a genuinely unreturned predecessor. Requires the caller's canonical run."
)]
async fn begin_goal_turn(ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let id = arg_string_required(&args, 0, "agent_session_begin_goal_turn", "id")?;
    require_owned_run(&ctx, &id)?;
    agent_sessions::begin_session_goal_turn(&id)
        .map(VmValue::Bool)
        .map_err(err)
}

#[harn_builtin(
    exposure = "runtime_internal", effects = [], kind = "async",
    sig = "agent_session_end_goal_turn(id: string, cancelled: bool) -> nil",
    category = "agent.session",
    doc = "Close a goal turn before terminal sealing. Normal agent-run terminalization owns this step automatically."
)]
async fn end_goal_turn(ctx: AsyncBuiltinCtx, args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let id = arg_string_required(&args, 0, "agent_session_end_goal_turn", "id")?;
    let cancelled = match args.get(1) {
        Some(VmValue::Bool(cancelled)) => *cancelled,
        _ => {
            return Err(err(
                "agent_session_end_goal_turn requires a boolean cancelled argument",
            ))
        }
    };
    require_owned_run(&ctx, &id)?;
    agent_sessions::end_session_goal_turn(&id, cancelled).map_err(err)?;
    Ok(VmValue::Nil)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn goal_boundary_refuses_lossy_runtime_values_and_nonfinite_numbers() {
        for value in [
            VmValue::Duration(1),
            VmValue::Float(f64::NAN),
            VmValue::BuiltinRef("println".into()),
        ] {
            let mut budget = crate::value::DictMap::new();
            budget.put("value", value);
            let mut fields = crate::value::DictMap::new();
            fields.put("budget", VmValue::dict(budget));
            let nested = VmValue::dict(fields);
            assert!(goal_json(&nested, &mut 65536, 0).is_err());
        }
        let value =
            crate::stdlib::json_to_vm_value(&serde_json::json!({"budget":{"max_cost_usd":2.5}}));
        assert_eq!(
            goal_json(&value, &mut 65536, 0).unwrap(),
            serde_json::json!({"budget":{"max_cost_usd":2.5}})
        );
    }

    pub(crate) fn run_script(source: &str) -> String {
        let chunk = crate::compile_source(source).unwrap();
        let mut vm = crate::new_vm();
        crate::register_vm_stdlib(&mut vm);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime
            .block_on(tokio::task::LocalSet::new().run_until(vm.execute(&chunk)))
            .unwrap();
        vm.output().to_string()
    }

    const OLD_GOAL: &str = r#"{spec:{objective:"Old objective", success_criteria:[{id:"check", description:"Pass"}], retired_criteria:[], constraints:[], budget:{}}, metadata:{acceptance_note:"", criterion_sources:{}, verification:nil, source:"caller"}}"#;

    pub(crate) fn seed_runs(options: &str) -> String {
        format!(
            r#"
fn main(harness: Harness) {{
  for id in ["reset-control", "reset-subject"] {{
    harness.agent.open(id)
    harness.agent.with_run(id, {options} + {{session_id:id}}, {{ ->
      harness.agent.set_goal(id, {OLD_GOAL})
      harness.agent.inject(id, {{role:"user", content:"old message"}})
    }})
    require harness.agent.goal(id).spec.objective == "Old objective", "control: goal set before reset"
    require harness.agent.length(id) == 1, "control: message present before reset"
  }}
}}
"#
        )
    }

    pub(crate) fn cold_read(options: &str) -> String {
        run_script(&format!(
            r#"
fn main(harness: Harness) {{
  for id in ["reset-control", "reset-subject"] {{
    harness.agent.with_run(id, {options} + {{session_id:id}}, {{ ->
      const goal = harness.agent.goal(id)
      const objective = if goal.spec == nil {{ "nil" }} else {{ goal.spec.objective }}
      harness.stdio.println("${{id}} objective=${{objective}} length=${{harness.agent.length(id)}}")
    }})
  }}
}}
"#
        ))
    }

    pub(crate) fn exit_process() {
        crate::agent_sessions::reset_session_store();
        crate::reset_thread_local_state();
    }

    #[test]
    fn reset_outside_a_run_persists_through_process_exit() {
        let _env = crate::llm::env_guard();
        exit_process();
        let root = tempfile::tempdir().unwrap();
        let options = format!(
            "{{root:{}}}",
            serde_json::to_string(root.path().to_str().unwrap()).unwrap()
        );
        run_script(&seed_runs(&options));
        let reset = run_script(
            r#"
fn main(harness: Harness) {
  harness.agent.reset("reset-subject")
  require harness.agent.goal("reset-subject").spec == nil, "reset clears the live goal"
  harness.stdio.println("reset_returned")
}
"#,
        );
        assert!(reset.contains("reset_returned"), "{reset}");
        exit_process();
        let cold = cold_read(&options);
        assert!(
            cold.contains("reset-control objective=Old objective length=1"),
            "cold hydration control must restore the unreset session: {cold}"
        );
        assert!(
            cold.contains("reset-subject objective=nil length=0"),
            "a reset outside a run must survive process exit: {cold}"
        );
        exit_process();
    }

    #[test]
    fn reset_in_a_fresh_process_persists_through_the_configured_store() {
        let root = tempfile::tempdir().unwrap();
        let _root_env =
            crate::llm::test_env::ScopedEnvVar::set("HARN_SESSION_STORE_ROOT", root.path());
        exit_process();
        run_script(&seed_runs("{}"));
        exit_process();
        let reset = run_script(
            r#"
fn main(harness: Harness) {
  harness.agent.open("reset-subject")
  harness.agent.reset("reset-subject")
  harness.stdio.println("reset_returned")
}
"#,
        );
        assert!(reset.contains("reset_returned"), "{reset}");
        exit_process();
        let cold = cold_read("{}");
        assert!(
            cold.contains("reset-control objective=Old objective length=1"),
            "cold hydration control must restore the unreset session: {cold}"
        );
        assert!(
            cold.contains("reset-subject objective=nil length=0"),
            "a fresh-process reset must reach the configured canonical store: {cold}"
        );
        exit_process();
    }

    #[test]
    fn failed_reset_write_leaves_the_live_session_unchanged() {
        let _env = crate::llm::env_guard();
        exit_process();
        let root = tempfile::tempdir().unwrap();
        let options = format!(
            "{{root:{}}}",
            serde_json::to_string(root.path().to_str().unwrap()).unwrap()
        );
        run_script(&seed_runs(&options));
        // A closed canonical session refuses appends, so the reset's write fails
        // after its writer lease and store open have succeeded.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let store = crate::stdlib::session_store::open_canonical_agent_session(
                &crate::stdlib::session_store::SessionStoreDir::under_root(root.path()),
                "reset-subject",
                None,
                harn_session_store::SessionType::User,
            )
            .await
            .unwrap();
            harn_session_store::SessionStore::close(&*store, "reset-subject")
                .await
                .unwrap();
        });
        let output = run_script(
            r#"
fn main(harness: Harness) {
  const reset = try { harness.agent.reset("reset-subject") }
  const goal = harness.agent.goal("reset-subject")
  const objective = if goal.spec == nil { "nil" } else { goal.spec.objective }
  harness.stdio.println("refused=${is_err(reset)} objective=${objective} length=${harness.agent.length("reset-subject")}")
}
"#,
        );
        assert!(
            output.contains("refused=true objective=Old objective length=1"),
            "a failed reset write must leave goal and transcript intact: {output}"
        );
        exit_process();
    }

    #[test]
    fn abandoned_reset_write_does_not_lock_the_session() {
        let _env = crate::llm::env_guard();
        exit_process();
        let root = tempfile::tempdir().unwrap();
        let options = format!(
            "{{root:{}}}",
            serde_json::to_string(root.path().to_str().unwrap()).unwrap()
        );
        run_script(&seed_runs(&options));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        crate::agent_session_journal::PANIC_IN_STANDALONE_WRITE.with(|armed| armed.set(true));
        let abandoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(crate::agent_sessions::reset_session("reset-subject", None))
        }));
        assert!(abandoned.is_err(), "control: the write must actually panic");
        assert!(
            agent_sessions::session_goal("reset-subject")
                .unwrap()
                .spec
                .is_some(),
            "an abandoned reset leaves the goal in memory"
        );
        let message =
            crate::stdlib::json_to_vm_value(&serde_json::json!({"role":"user","content":"later"}));
        agent_sessions::inject_message("reset-subject", message)
            .expect("a later mutation must not be refused");
        assert!(runtime
            .block_on(crate::agent_sessions::reset_session("reset-subject", None))
            .expect("a later reset must not be refused"));
        assert!(agent_sessions::session_goal("reset-subject")
            .unwrap()
            .spec
            .is_none());
        exit_process();
    }

    #[test]
    fn prepared_run_owns_goal_mutation_and_cold_restore_under_read_only_ceiling() {
        let _env = crate::llm::env_guard();
        crate::agent_sessions::reset_session_store();
        crate::reset_thread_local_state();
        let root = tempfile::tempdir().unwrap();
        let encoded_root = serde_json::to_string(root.path().to_str().unwrap()).unwrap();
        let source = format!(
            r#"
import {{ __goal_pin_spec, goal_under_obligations }} from "std/agent/goal_contract"
fn main(harness: Harness) {{
  const id = harness.agent.open("goal-cold-control")
  const options = {{root:{encoded_root}, session_id:id}}
  const payload = {{spec:{{objective:"Keep the exact objective", success_criteria:[{{id:"check", description:"Pass"}}], retired_criteria:[], constraints:["Keep scope"], budget:{{max_cost_usd:2.5}}}}, metadata:{{acceptance_note:"operator note", criterion_sources:{{check:"operator"}}, verification:{{command:"verify --literal", source:"opaque-source"}}, source:"caller"}}}}
  const outside = try {{ harness.agent.set_goal(id, payload) }}
  harness.stdio.println("outside_refused=${{is_err(outside)}}")
  harness.agent.with_run(id, options, {{ ->
    harness.agent.set_goal(id, payload)
    const retired = harness.agent.begin_goal_turn(id)
    harness.stdio.println("initial_retired=${{retired}}")
    const other = spawn {{
      require harness.agent.goal(id).spec != nil, "the other task must reach the same live goal"
      return is_err(try {{ harness.agent.clear_goal(id) }})
    }}
    harness.stdio.println("other_task_refused=${{await(other)}}")
    const denied = try {{ harness.runtime.store_set("ordinary-write-control", true) }}
    harness.stdio.println("ordinary_write_refused=${{is_err(denied)}}")
    const invalid = try {{ harness.agent.set_goal(id, payload + {{spec:payload.spec + {{success_criteria:payload.spec.success_criteria + payload.spec.success_criteria}}}}) }}
    harness.stdio.println("duplicate_active_refused=${{is_err(invalid)}}")
    const retargeted = goal_under_obligations(payload.spec, {{retarget:{{objective:"Retargeted objective"}}}})
    const amended = __goal_pin_spec(retargeted + {{success_criteria:[{{id:"check", description:"Replacement requirement"}}]}})
    harness.agent.set_goal(id, payload + {{spec:amended}})
  }})
  const forked = harness.agent.fork(id, "goal-fork-control")
  require harness.agent.goal(forked).spec != nil, "the fork must carry its parent's goal"
  harness.agent.with_run(forked, options + {{session_id:forked}}, {{ ->
    harness.stdio.println("fork_goal_retained=${{harness.agent.goal(forked).spec != nil}}")
  }})
  harness.agent.close(forked)
  harness.agent.open(forked)
  harness.agent.with_run(forked, options + {{session_id:forked}}, {{ ->
    const restored = harness.agent.goal(forked)
    const exact_goal = restored.spec.objective == "Retargeted objective"
    harness.stdio.println("fork_goal_cold_restored=${{exact_goal}}")
    harness.agent.clear_goal(forked)
  }})
  require harness.agent.goal(id).spec != nil, "clearing the child must preserve the parent goal"
  harness.agent.close(forked)
  const colliding = harness.agent.fork(id, forked)
  const collision = try {{ harness.agent.with_run(colliding, options + {{session_id:colliding}}, {{ -> nil }}) }}
  harness.stdio.println("canonical_collision_refused=${{is_err(collision)}}")
  harness.agent.close(colliding)
  harness.agent.open(forked)
  harness.agent.with_run(forked, options + {{session_id:forked}}, {{ ->
    require harness.agent.goal(forked).spec == nil, "a refused collision must not replace a cleared canonical goal"
  }})
  harness.agent.close(forked)
  const reset_fork = harness.agent.fork(id, "reset-goal-fork")
  harness.agent.reset(reset_fork)
  harness.agent.with_run(reset_fork, options + {{session_id:reset_fork}}, {{ ->
    const cleared = harness.agent.goal(reset_fork)
    const exact_reset = cleared.spec == nil && cleared.generation == 3
    harness.stdio.println("fork_reset_persisted=${{exact_reset}}")
  }})
  harness.agent.close(reset_fork)
  harness.agent.open(reset_fork)
  harness.agent.with_run(reset_fork, options + {{session_id:reset_fork}}, {{ ->
    require harness.agent.goal(reset_fork).spec == nil, "cold restore must retain an explicit fork reset"
  }})
  harness.agent.close(reset_fork)
  harness.agent.close(id)
  harness.agent.open(id)
  harness.agent.with_run(id, options, {{ ->
    harness.stdio.println(json_stringify(harness.agent.goal(id)))
  }})
}}
"#
        );
        let chunk = crate::compile_source(&source).unwrap();
        let mut vm = crate::new_vm();
        crate::register_vm_stdlib(&mut vm);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        crate::orchestration::push_execution_policy(crate::orchestration::CapabilityPolicy {
            side_effect_level: Some("read_only".into()),
            ..Default::default()
        });
        let result = runtime.block_on(tokio::task::LocalSet::new().run_until(vm.execute(&chunk)));
        crate::orchestration::pop_execution_policy();
        result.unwrap();
        let output = vm.output();
        assert!(output.contains("outside_refused=true"), "{output}");
        assert!(output.contains("initial_retired=false"), "{output}");
        assert!(output.contains("other_task_refused=true"), "{output}");
        assert!(output.contains("fork_goal_retained=true"), "{output}");
        assert!(output.contains("fork_goal_cold_restored=true"), "{output}");
        assert!(output.contains("fork_reset_persisted=true"), "{output}");
        assert!(
            output.contains("canonical_collision_refused=true"),
            "{output}"
        );
        assert!(output.contains("ordinary_write_refused=true"), "{output}");
        assert!(output.contains("duplicate_active_refused=true"), "{output}");
        let state: SessionGoalState = serde_json::from_str(output.lines().last().unwrap()).unwrap();
        let spec = state.spec.unwrap();
        assert_eq!(spec.objective, "Retargeted objective");
        assert_eq!(spec.success_criteria[0].id, spec.retired_criteria[0].id);
        assert_eq!(spec.retired_criteria[0].description, "Pass");
        assert_eq!(state.metadata.verification.unwrap().source, "opaque-source");
        assert_eq!(state.turn, agent_sessions::SessionGoalTurn::Idle);
        assert_eq!(state.generation, 2);
        crate::agent_sessions::reset_session_store();
        crate::reset_thread_local_state();
    }
}
