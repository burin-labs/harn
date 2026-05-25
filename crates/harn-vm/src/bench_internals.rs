use std::rc::Rc;

use crate::{Chunk, CompiledFunction, Vm, VmClosure, VmEnv, VmValue};

pub const VMENV_CAPTURE_COUNTS: [usize; 4] = [0, 5, 25, 100];

pub struct NonModuleClosureCallFixture {
    capture_count: usize,
    last_capture_name: Option<String>,
    caller_env: VmEnv,
    closure: VmClosure,
}

impl NonModuleClosureCallFixture {
    pub fn new(capture_count: usize) -> Self {
        let nested_inner = synthetic_closure("nested_inner", VmEnv::new());

        let mut caller_env = VmEnv::new();
        caller_env
            .define(
                "nested_inner",
                VmValue::Closure(Rc::new(nested_inner)),
                false,
            )
            .expect("synthetic caller closure binding should be valid");

        let mut closure_env = VmEnv::new();
        for index in 0..capture_count {
            closure_env
                .define(
                    &format!("captured_{index:03}"),
                    VmValue::Int(index as i64),
                    false,
                )
                .expect("synthetic captured binding should be valid");
        }

        let closure = synthetic_closure(&format!("capture_{capture_count:03}"), closure_env);
        Self {
            capture_count,
            last_capture_name: capture_count
                .checked_sub(1)
                .map(|index| format!("captured_{index:03}")),
            caller_env,
            closure,
        }
    }

    pub fn capture_count(&self) -> usize {
        self.capture_count
    }

    pub fn invoke(&self) -> usize {
        let env = Vm::closure_call_env(&self.caller_env, &self.closure);
        let mut score = env.scope_depth();
        if let Some(name) = self.last_capture_name.as_deref() {
            if let Some(VmValue::Int(value)) = env.get(name) {
                score += value as usize;
            }
        }
        if matches!(env.get("nested_inner"), Some(VmValue::Closure(_))) {
            score += 1;
        }
        score
    }
}

fn synthetic_closure(name: &str, env: VmEnv) -> VmClosure {
    let func = CompiledFunction {
        name: name.to_string(),
        type_params: Vec::new(),
        nominal_type_names: Vec::new(),
        params: Vec::new(),
        default_start: None,
        chunk: Rc::new(Chunk::new()),
        is_generator: false,
        is_stream: false,
        has_rest_param: false,
        has_runtime_type_checks: false,
    };
    VmClosure {
        func: Rc::new(func),
        env,
        source_dir: None,
        module_functions: None,
        module_state: None,
    }
}
