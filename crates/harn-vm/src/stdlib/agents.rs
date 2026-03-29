//! Agent orchestration primitives.
//!
//! Provides `agent()` for creating named, configured agents, and `agent_call()`
//! for invoking them. These are ergonomic wrappers around `agent_loop` that
//! make multi-agent pipelines natural to express.

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_agent_builtins(vm: &mut Vm) {
    // agent(name, config) -> agent dict
    // config = {system, provider?, model?, tools?, max_iterations?, tool_format?}
    vm.register_builtin("agent", |args, _out| {
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        let config = match args.get(1) {
            Some(VmValue::Dict(map)) => (**map).clone(),
            Some(_) => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "agent: second argument must be a config dict",
                ))));
            }
            None => BTreeMap::new(),
        };

        let mut agent = config;
        agent.insert("_type".to_string(), VmValue::String(Rc::from("agent")));
        agent.insert("name".to_string(), VmValue::String(Rc::from(name)));

        Ok(VmValue::Dict(Rc::new(agent)))
    });

    // agent_config(agent) -> {prompt, system, options} for passing to agent_loop
    // Usage: let cfg = agent_config(my_agent, "Do something")
    //        let result = agent_loop(cfg.prompt, cfg.system, cfg.options)
    vm.register_builtin("agent_config", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "agent_config: requires agent and prompt",
            ))));
        }

        let agent = match &args[0] {
            VmValue::Dict(map) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "agent_config: first argument must be an agent",
                ))));
            }
        };

        match agent.get("_type") {
            Some(VmValue::String(t)) if &**t == "agent" => {}
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "agent_config: first argument must be an agent (created with agent())",
                ))));
            }
        }

        // Build options dict from agent config for agent_loop
        let mut options = BTreeMap::new();
        for key in [
            "provider",
            "model",
            "tools",
            "max_iterations",
            "tool_format",
            "tool_retries",
            "tool_backoff_ms",
        ] {
            if let Some(val) = agent.get(key) {
                options.insert(key.to_string(), val.clone());
            }
        }

        let prompt = args[1].clone();
        let system = agent.get("system").cloned().unwrap_or(VmValue::Nil);

        let mut result = BTreeMap::new();
        result.insert("prompt".to_string(), prompt);
        result.insert("system".to_string(), system);
        result.insert("options".to_string(), VmValue::Dict(Rc::new(options)));

        Ok(VmValue::Dict(Rc::new(result)))
    });

    // agent_name(agent) -> string
    vm.register_builtin("agent_name", |args, _out| {
        let agent = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "agent_name: argument must be an agent",
                ))));
            }
        };
        Ok(agent.get("name").cloned().unwrap_or(VmValue::Nil))
    });
}
