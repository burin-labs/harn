use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;

use crate::portable_builtin::PortableBuiltin;
use crate::type_contract::{
    manifest_signature_is_portable, matches_compiler_schema, matches_manifest_type,
};
use crate::{Chunk, CompiledFunction, Constant, Diagnostic, Op, ProgramArtifact};

mod arithmetic;
mod builtins;
mod methods;
mod ops;
mod resource;
mod runtime_value;
mod snapshot;
mod type_guard;
mod types;

#[cfg(test)]
mod tests;

use crate::value::{semantic_try_compare, semantic_values_equal};
use arithmetic::{add, div, modulo, mul, negate, pow, sub};
use ops::*;
use resource::{validate_runtime_value, MAX_VALUE_BYTES};
use runtime_value::{Closure, EnumValue, RuntimeValue};
use snapshot::{decode_snapshot, encode_snapshot, ReplaySnapshot};
use type_guard::validate_call;
use types::value_kind;
pub use types::{CapabilityRequest, CapabilityResult, DataValue, Execution, GrantSet, ValueShape};

const DEFAULT_FUEL: u64 = 2_000_000;
const MAX_FRAMES: usize = 1_024;
const MAX_SCOPE_DEPTH: usize = 256;
const MAX_OPERAND_STACK: usize = 16_384;
const MAX_ITERATORS: usize = 1_024;
pub const PORTABLE_MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;

pub fn start(program: &ProgramArtifact, input: DataValue, grants: &GrantSet) -> Execution {
    run(program, input, grants, Vec::new())
}

/// Deterministically execute from the beginning with a recorded capability
/// transcript. This is the native replay path and the oracle for snapshot
/// resume: responses are consumed only when their request IDs match.
pub fn replay(
    program: &ProgramArtifact,
    input: DataValue,
    grants: &GrantSet,
    responses: Vec<CapabilityResult>,
) -> Execution {
    run(program, input, grants, responses)
}

pub fn resume(
    program: &ProgramArtifact,
    snapshot: &[u8],
    result: CapabilityResult,
    grants: &GrantSet,
) -> Execution {
    let decoded = match decode_snapshot(snapshot, grants.snapshot_key()) {
        Ok(value) => value,
        Err(error) => return Execution::Failed { diagnostic: error },
    };
    if decoded.artifact_digest != program.digest() {
        return failed(
            "snapshot_program_mismatch",
            "snapshot belongs to a different program artifact",
        );
    }
    if decoded.grant_fingerprint != grants.fingerprint() {
        return failed(
            "snapshot_grant_mismatch",
            "resume grants differ from the grants that created the snapshot",
        );
    }
    if result.request_id() != decoded.pending_request {
        return failed(
            "capability_result_mismatch",
            "capability result request ID does not match the suspended request",
        );
    }
    let mut responses = decoded.responses;
    responses.push(result);
    run_with_fuel(
        program,
        decoded.input,
        grants,
        responses,
        decoded.fuel_consumed,
    )
}

fn run(
    program: &ProgramArtifact,
    input: DataValue,
    grants: &GrantSet,
    responses: Vec<CapabilityResult>,
) -> Execution {
    run_with_fuel(program, input, grants, responses, 0)
}

fn run_with_fuel(
    program: &ProgramArtifact,
    input: DataValue,
    grants: &GrantSet,
    responses: Vec<CapabilityResult>,
    fuel_consumed: u64,
) -> Execution {
    if let Err(diagnostic) = input.validate() {
        return Execution::Failed { diagnostic };
    }
    for response in &responses {
        if let CapabilityResult::Ok { value, .. } = response {
            if let Err(diagnostic) = value.validate() {
                return Execution::Failed { diagnostic };
            }
        }
    }
    let root = Env::root();
    let mut machine = Machine::new(program, root.clone(), grants, responses, fuel_consumed);
    let bootstrap = match machine.execute(program.image().clone(), root, Vec::new()) {
        Step::Value(value) => value,
        Step::Suspend(request) => return machine.suspend(input, request),
        Step::Error(error) => return Execution::Failed { diagnostic: error },
    };
    let RuntimeValue::Closure(closure) = bootstrap else {
        return failed(
            "entry_not_callable",
            "compiled entry bootstrap did not return a callable",
        );
    };
    let mut arguments = vec![RuntimeValue::from(input.clone())];
    if program.expects_harness() {
        arguments.insert(0, RuntimeValue::Harness("root".to_string()));
    }
    let Some(closure_env) = closure.env.upgrade() else {
        return failed(
            "closure_environment",
            "entry closure environment is no longer available",
        );
    };
    let entry_env = match machine.child_env(closure_env) {
        Ok(env) => env,
        Err(diagnostic) => return Execution::Failed { diagnostic },
    };
    if let Err(diagnostic) = machine.charge_call_validation(&arguments) {
        return Execution::Failed { diagnostic };
    }
    if let Err(diagnostic) = validate_call(&closure.function, &arguments) {
        return Execution::Failed { diagnostic };
    }
    match machine.execute_function(&closure.function, entry_env, arguments) {
        Step::Value(value) => match machine
            .charge_value_work(&value)
            .and_then(|()| DataValue::try_from(value))
        {
            Ok(value) => Execution::Completed { value },
            Err(error) => Execution::Failed { diagnostic: error },
        },
        Step::Suspend(request) => machine.suspend(input, request),
        Step::Error(error) => Execution::Failed { diagnostic: error },
    }
}

struct Machine<'a> {
    program: &'a ProgramArtifact,
    grants: &'a GrantSet,
    responses: Vec<CapabilityResult>,
    response_cursor: usize,
    request_ordinal: u64,
    fuel: u64,
    replay_credit: u64,
    environments: Vec<Rc<Env>>,
    modules: BTreeMap<String, Rc<ModuleInstance>>,
    loading_modules: Vec<String>,
    iterators: Vec<IteratorState>,
}

impl<'a> Machine<'a> {
    fn new(
        program: &'a ProgramArtifact,
        root: Rc<Env>,
        grants: &'a GrantSet,
        responses: Vec<CapabilityResult>,
        fuel_consumed: u64,
    ) -> Self {
        Self {
            program,
            grants,
            responses,
            response_cursor: 0,
            request_ordinal: 0,
            fuel: DEFAULT_FUEL.saturating_sub(fuel_consumed),
            replay_credit: fuel_consumed.min(DEFAULT_FUEL),
            environments: vec![root],
            modules: BTreeMap::new(),
            loading_modules: Vec::new(),
            iterators: Vec::new(),
        }
    }

    fn import_root_module(
        &mut self,
        path: &str,
        selected_names: Option<&[String]>,
        namespace_alias: Option<NamespaceProjection<'_>>,
        env: &Rc<Env>,
    ) -> OpStep {
        let Some(spec) = self
            .program
            .root_imports()
            .iter()
            .find(|spec| spec.path == path)
        else {
            return OpStep::Error(diagnostic(
                "portable_import",
                format!("root import `{path}` is not present in the package closure"),
            ));
        };
        let target = spec.target.clone();
        let module = match self.load_module(&target) {
            ModuleStep::Ready(module) => module,
            ModuleStep::Suspend(request) => return OpStep::Suspend(request),
            ModuleStep::Error(error) => return OpStep::Error(error),
        };
        match self.bind_module_projection(&module, selected_names, namespace_alias, env) {
            Ok(()) => OpStep::Continue,
            Err(error) => OpStep::Error(error),
        }
    }

    fn load_module(&mut self, id: &str) -> ModuleStep {
        if let Some(module) = self.modules.get(id).cloned() {
            return ModuleStep::Ready(module);
        }
        if self.loading_modules.iter().any(|loading| loading == id) {
            return ModuleStep::Error(diagnostic(
                "portable_import_cycle",
                format!("portable package import cycle reaches `{id}`"),
            ));
        }
        let Some(module) = self
            .program
            .modules()
            .iter()
            .find(|module| module.id() == id)
            .cloned()
        else {
            return ModuleStep::Error(diagnostic(
                "portable_import",
                format!("portable package does not contain module `{id}`"),
            ));
        };
        let module_env = Env::root();
        self.retain_environment(&module_env);
        self.loading_modules.push(id.to_string());

        for import in module.imports() {
            let imported = match self.load_module(&import.target) {
                ModuleStep::Ready(imported) => imported,
                ModuleStep::Suspend(request) => {
                    self.loading_modules.pop();
                    return ModuleStep::Suspend(request);
                }
                ModuleStep::Error(error) => {
                    self.loading_modules.pop();
                    return ModuleStep::Error(error);
                }
            };
            if let Err(error) = self.bind_module_projection(
                &imported,
                import.selected_names.as_deref(),
                // The manifest records an alias without member demand, so a
                // module-to-module namespace import is projected whole.
                import.namespace_alias.as_deref().map(|alias| (alias, None)),
                &module_env,
            ) {
                self.loading_modules.pop();
                return ModuleStep::Error(error);
            }
        }

        if let Some(init) = module.init() {
            match self.execute(init.clone(), module_env.clone(), Vec::new()) {
                Step::Value(_) => {}
                Step::Suspend(request) => {
                    self.loading_modules.pop();
                    return ModuleStep::Suspend(request);
                }
                Step::Error(error) => {
                    self.loading_modules.pop();
                    return ModuleStep::Error(error);
                }
            }
        }

        for (name, function) in module.functions() {
            self.retain_environment(&module_env);
            let value = RuntimeValue::Closure(Closure {
                function: function.clone(),
                env: Rc::downgrade(&module_env),
            });
            module_env.define(name.clone(), value.clone());
        }
        let instance = Rc::new(ModuleInstance {
            env: module_env,
            exports: module.exports().clone(),
        });
        self.modules.insert(id.to_string(), instance.clone());
        self.loading_modules.pop();
        ModuleStep::Ready(instance)
    }

    fn bind_module_projection(
        &self,
        module: &ModuleInstance,
        selected_names: Option<&[String]>,
        namespace_alias: Option<NamespaceProjection<'_>>,
        env: &Rc<Env>,
    ) -> Result<(), Diagnostic> {
        if let Some((alias, demanded)) = namespace_alias {
            if env.contains_local(alias) {
                return Err(diagnostic(
                    "portable_import_collision",
                    format!("import namespace `{alias}` collides with an existing binding"),
                ));
            }
            let mut entries = BTreeMap::new();
            for (name, kind) in &module.exports {
                if !kind.has_runtime_value() {
                    continue;
                }
                if demanded.is_some_and(|members| !members.iter().any(|member| member == name)) {
                    continue;
                }
                if let Some(value) = module.env.get(name) {
                    entries.insert(name.clone(), value);
                }
            }
            if let Some(members) = demanded {
                for member in members {
                    if !module.exports.contains_key(member) {
                        return Err(diagnostic(
                            "portable_import",
                            format!("module does not export `{member}`"),
                        ));
                    }
                }
            }
            env.define(alias.to_string(), RuntimeValue::Record(Rc::new(entries)));
            return Ok(());
        }
        let names = selected_names
            .map(|names| names.to_vec())
            .unwrap_or_else(|| module.exports.keys().cloned().collect());
        for name in names {
            let Some(kind) = module.exports.get(&name) else {
                return Err(diagnostic(
                    "portable_import",
                    format!("module does not export `{name}`"),
                ));
            };
            if !kind.has_runtime_value() {
                continue;
            }
            let Some(value) = module.env.get(&name) else {
                return Err(diagnostic(
                    "portable_import",
                    format!("module export `{name}` has no runtime value"),
                ));
            };
            if env.contains_local(&name) {
                return Err(diagnostic(
                    "portable_import_collision",
                    format!("imported binding `{name}` collides with an existing binding"),
                ));
            }
            env.define(name, value);
        }
        Ok(())
    }

    fn child_env(&mut self, parent: Rc<Env>) -> Result<Rc<Env>, Diagnostic> {
        Env::child(parent)
    }

    fn retain_environment(&mut self, environment: &Rc<Env>) {
        self.environments.push(environment.clone());
    }

    fn charge(&mut self, amount: u64) -> Result<(), Diagnostic> {
        let replayed = amount.min(self.replay_credit);
        self.replay_credit -= replayed;
        let fresh = amount - replayed;
        if fresh > self.fuel {
            self.fuel = 0;
            return Err(diagnostic(
                "execution_fuel",
                "portable execution exhausted its deterministic fuel limit",
            ));
        }
        self.fuel -= fresh;
        Ok(())
    }

    fn charge_value_work(&mut self, value: &RuntimeValue) -> Result<(), Diagnostic> {
        let usage = validate_runtime_value(value)?;
        self.charge(usage.nodes as u64)
    }

    fn charge_call_validation(&mut self, arguments: &[RuntimeValue]) -> Result<(), Diagnostic> {
        let mut nodes = 0_u64;
        for argument in arguments {
            let usage = validate_runtime_value(argument)?;
            nodes = nodes.saturating_add(usage.nodes as u64);
        }
        self.charge(nodes)
    }

    fn charge_values_work(&mut self, values: &[&RuntimeValue]) -> Result<(), Diagnostic> {
        let mut nodes = 0_u64;
        for value in values {
            let usage = validate_runtime_value(value)?;
            nodes = nodes.saturating_add(usage.nodes as u64);
        }
        self.charge(nodes)
    }

    fn render_value(&mut self, value: &RuntimeValue) -> Result<String, Diagnostic> {
        self.charge_value_work(value)?;
        Ok(value.display())
    }

    fn push_charged(&mut self, value: RuntimeValue) -> OpStep {
        match self.charge_value_work(&value) {
            Ok(()) => OpStep::Push(value),
            Err(diagnostic) => OpStep::Error(diagnostic),
        }
    }

    fn values_equal(
        &mut self,
        left: &RuntimeValue,
        right: &RuntimeValue,
    ) -> Result<bool, Diagnostic> {
        self.charge_values_work(&[left, right])?;
        Ok(equal(left, right))
    }

    fn suspend(&self, input: DataValue, request: CapabilityRequest) -> Execution {
        let Some(snapshot_key) = self.grants.snapshot_key() else {
            return failed(
                "snapshot_key_required",
                "suspendable capability grants require a host-owned snapshot key",
            );
        };
        let snapshot = ReplaySnapshot {
            artifact_digest: self.program.digest(),
            grant_fingerprint: self.grants.fingerprint(),
            fuel_consumed: DEFAULT_FUEL - self.fuel,
            input,
            responses: self.responses[..self.response_cursor].to_vec(),
            pending_request: request.id.clone(),
        };
        match encode_snapshot(&snapshot, snapshot_key) {
            Ok(snapshot) => Execution::Suspended { request, snapshot },
            Err(diagnostic) => Execution::Failed { diagnostic },
        }
    }

    fn execute(&mut self, chunk: Arc<Chunk>, env: Rc<Env>, arguments: Vec<RuntimeValue>) -> Step {
        let mut frames = vec![Frame::new(chunk, env, arguments)];
        self.execute_frames(&mut frames)
    }

    fn execute_function(
        &mut self,
        function: &CompiledFunction,
        env: Rc<Env>,
        arguments: Vec<RuntimeValue>,
    ) -> Step {
        let frame = match self.function_frame(function, env, arguments) {
            Ok(frame) => frame,
            Err(diagnostic) => return Step::Error(diagnostic),
        };
        let mut frames = vec![frame];
        self.execute_frames(&mut frames)
    }

    fn function_frame(
        &mut self,
        function: &CompiledFunction,
        env: Rc<Env>,
        arguments: Vec<RuntimeValue>,
    ) -> Result<Frame, Diagnostic> {
        let frame = Frame::for_function(function, env, arguments);
        if function.has_rest_param && !function.params.is_empty() {
            let rest_index = function.params.len() - 1;
            if let Some(Some(rest)) = frame.locals.get(rest_index) {
                self.charge_value_work(rest)?;
            }
        }
        Ok(frame)
    }

    fn execute_frames(&mut self, frames: &mut Vec<Frame>) -> Step {
        loop {
            if let Err(diagnostic) = self.charge(1) {
                return Step::Error(diagnostic);
            }
            let Some(frame) = frames.last_mut() else {
                return Step::Error(diagnostic(
                    "execution_state",
                    "execution has no active frame",
                ));
            };
            if frame.ip >= frame.chunk.code.len() {
                return Step::Error(diagnostic(
                    "instruction_pointer",
                    "instruction pointer escaped its chunk",
                ));
            }
            let offset = frame.ip;
            let byte = frame.chunk.code[frame.ip];
            frame.ip += 1;
            let Some(op) = Op::from_byte(byte) else {
                return Step::Error(diagnostic(
                    "invalid_opcode",
                    format!("invalid opcode 0x{byte:02x}"),
                ));
            };
            let result = match self.execute_op(op, offset, frames) {
                Ok(result) | Err(result) => result,
            };
            match result {
                OpStep::Continue => {}
                OpStep::Push(value) => frames
                    .last_mut()
                    .expect("active frame accepts operation result")
                    .stack
                    .push(value),
                OpStep::Call(closure, args, tail) => {
                    if frames.len() >= MAX_FRAMES {
                        return Step::Error(diagnostic(
                            "frame_limit",
                            "portable execution exceeded its frame limit",
                        ));
                    }
                    let Some(closure_env) = closure.env.upgrade() else {
                        return Step::Error(diagnostic(
                            "closure_environment",
                            "closure environment is no longer available",
                        ));
                    };
                    let env = match self.child_env(closure_env) {
                        Ok(env) => env,
                        Err(diagnostic) => return Step::Error(diagnostic),
                    };
                    if let Err(diagnostic) = self.charge_call_validation(&args) {
                        return Step::Error(diagnostic);
                    }
                    if let Err(diagnostic) = validate_call(&closure.function, &args) {
                        return Step::Error(diagnostic);
                    }
                    let next = match self.function_frame(&closure.function, env, args) {
                        Ok(frame) => frame,
                        Err(diagnostic) => return Step::Error(diagnostic),
                    };
                    if tail {
                        *frames.last_mut().expect("caller exists") = next;
                    } else {
                        frames.push(next);
                    }
                }
                OpStep::Return(value) => {
                    frames.pop();
                    if let Some(caller) = frames.last_mut() {
                        caller.stack.push(value);
                    } else {
                        return Step::Value(value);
                    }
                }
                OpStep::Suspend(request) => return Step::Suspend(request),
                OpStep::Throw(value) => {
                    if !handle_throw(frames, value.clone()) {
                        let message = match self.render_value(&value) {
                            Ok(message) => message,
                            Err(diagnostic) => return Step::Error(diagnostic),
                        };
                        return Step::Error(diagnostic("harn_throw", message));
                    }
                }
                OpStep::Error(error) => return Step::Error(error),
            }
            if frames
                .last()
                .is_some_and(|frame| frame.stack.len() > MAX_OPERAND_STACK)
            {
                return Step::Error(diagnostic(
                    "operand_stack_limit",
                    "portable execution exceeded its operand stack limit",
                ));
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn execute_op(
        &mut self,
        op: Op,
        offset: usize,
        frames: &mut [Frame],
    ) -> Result<OpStep, OpStep> {
        let frame = frames.last_mut().expect("active frame");
        macro_rules! pop {
            () => {
                match frame.stack.pop() {
                    Some(value) => value,
                    None => {
                        return Err(OpStep::Error(diagnostic(
                            "stack_underflow",
                            format!("{} at {offset}", op.name()),
                        )))
                    }
                }
            };
        }
        match op {
            Op::Constant => {
                let index = read_u16(frame)?;
                let Some(value) = frame.chunk.constants.get(index).cloned() else {
                    return Err(invalid_index("constant", index));
                };
                frame.stack.push(RuntimeValue::from(value));
            }
            Op::Nil => frame.stack.push(RuntimeValue::Nil),
            Op::True => frame.stack.push(RuntimeValue::Bool(true)),
            Op::False => frame.stack.push(RuntimeValue::Bool(false)),
            Op::RootHarness => frame.stack.push(RuntimeValue::Harness("root".to_string())),
            Op::GetVar => {
                let name = read_constant_string(frame)?;
                frame.stack.push(
                    frame
                        .env
                        .get(&name)
                        .unwrap_or_else(|| RuntimeValue::Builtin(name)),
                );
            }
            Op::DefLet | Op::DefVar | Op::DefCell => {
                let name = read_constant_string(frame)?;
                let value = pop!();
                frame.env.define(name, value);
            }
            Op::SetVar => {
                let name = read_constant_string(frame)?;
                let value = pop!();
                frame.env.set(&name, value);
            }
            Op::PushScope => {
                frame.env = self.child_env(frame.env.clone()).map_err(OpStep::Error)?;
            }
            Op::PopScope => {
                if let Some(parent) = &frame.env.parent {
                    frame.env = parent.clone();
                }
            }
            Op::GetLocalSlot => {
                let slot = read_u16(frame)?;
                let Some(value) = frame.locals.get(slot).and_then(Clone::clone) else {
                    return Err(invalid_index("local", slot));
                };
                frame.stack.push(value);
            }
            Op::DefLocalSlot | Op::SetLocalSlot => {
                let slot = read_u16(frame)?;
                let value = pop!();
                if slot >= frame.locals.len() {
                    return Err(invalid_index("local", slot));
                }
                frame.locals[slot] = Some(value.clone());
                if let Some(local) = frame.chunk.local_slots.get(slot) {
                    if op == Op::DefLocalSlot {
                        frame.env.define(local.name.clone(), value);
                    } else {
                        frame.env.set(&local.name, value);
                    }
                }
            }
            Op::ConcatAssignLocal => {
                let slot = read_u16(frame)?;
                let rhs = pop!();
                let Some(local) = frame.chunk.local_slots.get(slot) else {
                    return Err(invalid_index("local", slot));
                };
                if !local.mutable {
                    return Err(OpStep::Error(diagnostic(
                        "immutable_assignment",
                        format!("cannot assign to immutable binding `{}`", local.name),
                    )));
                }
                let Some(lhs) = frame.locals.get(slot).and_then(Clone::clone) else {
                    return Err(invalid_index("local", slot));
                };
                let value = add(lhs, rhs).map_err(OpStep::Error)?;
                self.charge_value_work(&value).map_err(OpStep::Error)?;
                frame.locals[slot] = Some(value.clone());
                frame.env.set(&local.name, value);
            }
            Op::GetArgc => frame.stack.push(RuntimeValue::Int(frame.argc as i64)),
            Op::Pop => {
                pop!();
            }
            Op::Dup => {
                let value = pop!();
                frame.stack.push(value.clone());
                frame.stack.push(value);
            }
            Op::Swap => {
                let right = pop!();
                let left = pop!();
                frame.stack.push(right);
                frame.stack.push(left);
            }
            Op::Add | Op::AddInt | Op::AddFloat => {
                let value = binary(frame, add)?;
                self.charge_value_work(&value).map_err(OpStep::Error)?;
                frame.stack.push(value);
            }
            Op::Sub | Op::SubInt | Op::SubFloat => {
                let value = binary(frame, sub)?;
                self.charge_value_work(&value).map_err(OpStep::Error)?;
                frame.stack.push(value);
            }
            Op::Mul | Op::MulInt | Op::MulFloat => {
                let value = binary(frame, mul)?;
                self.charge_value_work(&value).map_err(OpStep::Error)?;
                frame.stack.push(value);
            }
            Op::Div | Op::DivInt | Op::DivFloat => {
                let value = binary(frame, div)?;
                self.charge_value_work(&value).map_err(OpStep::Error)?;
                frame.stack.push(value);
            }
            Op::Mod | Op::ModInt | Op::ModFloat => {
                let value = binary(frame, modulo)?;
                self.charge_value_work(&value).map_err(OpStep::Error)?;
                frame.stack.push(value);
            }
            Op::Pow => {
                let value = binary(frame, pow)?;
                self.charge_value_work(&value).map_err(OpStep::Error)?;
                frame.stack.push(value);
            }
            Op::Negate => {
                let value = pop!();
                frame.stack.push(negate(value).map_err(OpStep::Error)?);
            }
            Op::Not => {
                let value = pop!();
                frame.stack.push(RuntimeValue::Bool(!value.truthy()));
            }
            Op::Equal | Op::EqualInt | Op::EqualFloat | Op::EqualBool | Op::EqualString => {
                compare_equality(self, frame, true)?;
            }
            Op::NotEqual
            | Op::NotEqualInt
            | Op::NotEqualFloat
            | Op::NotEqualBool
            | Op::NotEqualString => compare_equality(self, frame, false)?,
            Op::Less | Op::LessInt | Op::LessFloat => compare(self, frame, |value| value < 0)?,
            Op::Greater | Op::GreaterInt | Op::GreaterFloat => {
                compare(self, frame, |value| value > 0)?;
            }
            Op::LessEqual | Op::LessEqualInt | Op::LessEqualFloat => {
                compare(self, frame, |value| value <= 0)?;
            }
            Op::GreaterEqual | Op::GreaterEqualInt | Op::GreaterEqualFloat => {
                compare(self, frame, |value| value >= 0)?;
            }
            Op::Jump => frame.ip = read_u16(frame)?,
            Op::JumpIfFalse => {
                let target = read_u16(frame)?;
                if !frame.stack.last().is_some_and(RuntimeValue::truthy) {
                    frame.ip = target;
                }
            }
            Op::JumpIfTrue => {
                let target = read_u16(frame)?;
                if frame.stack.last().is_some_and(RuntimeValue::truthy) {
                    frame.ip = target;
                }
            }
            Op::Closure => {
                let index = read_u16(frame)?;
                let Some(function) = frame.chunk.functions.get(index).cloned() else {
                    return Err(invalid_index("function", index));
                };
                self.retain_environment(&frame.env);
                frame.stack.push(RuntimeValue::Closure(Closure {
                    function,
                    env: Rc::downgrade(&frame.env),
                }));
            }
            Op::Call | Op::TailCall => {
                let argc = read_u8(frame)?;
                let args = pop_args(frame, argc)?;
                let callee = pop!();
                return Ok(call_value(
                    self,
                    &frame.env,
                    callee,
                    args,
                    op == Op::TailCall,
                ));
            }
            Op::Return => {
                return Ok(OpStep::Return(
                    frame.stack.pop().unwrap_or(RuntimeValue::Nil),
                ))
            }
            Op::BuildList => {
                let count = read_u16(frame)?;
                let values = pop_args(frame, count)?;
                let value = RuntimeValue::List(Rc::new(values));
                self.charge_value_work(&value).map_err(OpStep::Error)?;
                frame.stack.push(value);
            }
            Op::BuildDict => {
                let count = read_u16(frame)?;
                let values = pop_args(frame, count * 2)?;
                let mut map = BTreeMap::new();
                for pair in values.chunks_exact(2) {
                    let key = self.render_value(&pair[0]).map_err(OpStep::Error)?;
                    map.insert(key, pair[1].clone());
                }
                let value = RuntimeValue::Record(Rc::new(map));
                self.charge_value_work(&value).map_err(OpStep::Error)?;
                frame.stack.push(value);
            }
            Op::GetProperty | Op::GetPropertyOpt => {
                let name = read_constant_string(frame)?;
                let value = pop!();
                match get_property(&value, &name) {
                    Some(value) => frame.stack.push(value),
                    None if op == Op::GetPropertyOpt => frame.stack.push(RuntimeValue::Nil),
                    None => {
                        return Err(OpStep::Error(diagnostic(
                            "missing_property",
                            format!("value has no property `{name}`"),
                        )))
                    }
                }
            }
            Op::Subscript | Op::SubscriptOpt => {
                let index = pop!();
                let value = pop!();
                match self.subscript(&value, &index).map_err(OpStep::Error)? {
                    Some(value) => frame.stack.push(value),
                    None if op == Op::SubscriptOpt => frame.stack.push(RuntimeValue::Nil),
                    None => {
                        return Err(OpStep::Error(diagnostic(
                            "subscript",
                            "subscript does not exist",
                        )))
                    }
                }
            }
            Op::SetProperty => {
                let property = read_constant_string(frame)?;
                let binding = read_constant_string(frame)?;
                let value = pop!();
                let Some(target) = frame.env.get(&binding) else {
                    return Err(OpStep::Error(diagnostic(
                        "undefined_variable",
                        format!("cannot assign property on undefined binding `{binding}`"),
                    )));
                };
                let updated =
                    set_property_value(target, &property, value).map_err(OpStep::Error)?;
                self.charge_value_work(&updated).map_err(OpStep::Error)?;
                frame.env.set(&binding, updated);
            }
            Op::SetSubscript => {
                let binding = read_constant_string(frame)?;
                let index = pop!();
                let value = pop!();
                let Some(target) = frame.env.get(&binding) else {
                    return Err(OpStep::Error(diagnostic(
                        "undefined_variable",
                        format!("cannot assign subscript on undefined binding `{binding}`"),
                    )));
                };
                let updated = set_subscript_value(target, index, value).map_err(OpStep::Error)?;
                self.charge_value_work(&updated).map_err(OpStep::Error)?;
                frame.env.set(&binding, updated);
            }
            Op::SetLocalSlotProperty => {
                let property = read_constant_string(frame)?;
                let slot = read_u16(frame)?;
                let value = pop!();
                let Some(local) = frame.chunk.local_slots.get(slot) else {
                    return Err(invalid_index("local", slot));
                };
                if !local.mutable {
                    return Err(OpStep::Error(diagnostic(
                        "immutable_assignment",
                        format!("cannot assign to immutable binding `{}`", local.name),
                    )));
                }
                let Some(target) = frame.locals.get(slot).and_then(Clone::clone) else {
                    return Err(invalid_index("local", slot));
                };
                let updated =
                    set_property_value(target, &property, value).map_err(OpStep::Error)?;
                self.charge_value_work(&updated).map_err(OpStep::Error)?;
                frame.locals[slot] = Some(updated.clone());
                frame.env.set(&local.name, updated);
            }
            Op::SetLocalSlotSubscript => {
                let slot = read_u16(frame)?;
                let index = pop!();
                let value = pop!();
                let Some(local) = frame.chunk.local_slots.get(slot) else {
                    return Err(invalid_index("local", slot));
                };
                if !local.mutable {
                    return Err(OpStep::Error(diagnostic(
                        "immutable_assignment",
                        format!("cannot assign to immutable binding `{}`", local.name),
                    )));
                }
                let Some(target) = frame.locals.get(slot).and_then(Clone::clone) else {
                    return Err(invalid_index("local", slot));
                };
                let updated = set_subscript_value(target, index, value).map_err(OpStep::Error)?;
                self.charge_value_work(&updated).map_err(OpStep::Error)?;
                frame.locals[slot] = Some(updated.clone());
                frame.env.set(&local.name, updated);
            }
            Op::Slice => {
                let end = pop!();
                let start = pop!();
                let value = pop!();
                let value = slice(value, start, end).map_err(OpStep::Error)?;
                self.charge_value_work(&value).map_err(OpStep::Error)?;
                frame.stack.push(value);
            }
            Op::MethodCall | Op::MethodCallOpt => {
                let name = read_constant_string(frame)?;
                let argc = read_u8(frame)?;
                let args = pop_args(frame, argc)?;
                let receiver = pop!();
                if op == Op::MethodCallOpt && matches!(receiver, RuntimeValue::Nil) {
                    frame.stack.push(RuntimeValue::Nil);
                } else {
                    return Ok(self.call_method(receiver, &name, args));
                }
            }
            Op::Concat => {
                let count = read_u16(frame)?;
                let values = pop_args(frame, count)?;
                let mut rendered = String::new();
                for value in &values {
                    let part = self.render_value(value).map_err(OpStep::Error)?;
                    if rendered.len().saturating_add(part.len()) > MAX_VALUE_BYTES {
                        return Err(OpStep::Error(diagnostic(
                            "value_byte_limit",
                            "string interpolation exceeds the portable value byte limit",
                        )));
                    }
                    rendered.push_str(&part);
                }
                frame.stack.push(RuntimeValue::String(Arc::from(rendered)));
            }
            Op::Contains => {
                let container = pop!();
                let item = pop!();
                let found = self.contains(&container, &item).map_err(OpStep::Error)?;
                frame.stack.push(RuntimeValue::Bool(found));
            }
            Op::IterInit => {
                if self.iterators.len() >= MAX_ITERATORS {
                    return Err(OpStep::Error(diagnostic(
                        "iterator_limit",
                        "portable execution exceeded its iterator limit",
                    )));
                }
                let iterable = pop!();
                let iterator = match iterable {
                    RuntimeValue::List(values) => IteratorState::List { values, index: 0 },
                    RuntimeValue::Record(values) => IteratorState::Record {
                        keys: values.keys().cloned().collect(),
                        values,
                        index: 0,
                    },
                    other => {
                        return Err(OpStep::Error(diagnostic(
                            "iterator_type",
                            format!(
                                "cannot iterate over {} in the portable kernel",
                                runtime_value_kind(&other)
                            ),
                        )))
                    }
                };
                self.iterators.push(iterator);
            }
            Op::IterNext => {
                let target = read_u16(frame)?;
                let Some(iterator) = self.iterators.last_mut() else {
                    return Err(OpStep::Error(diagnostic(
                        "iterator_state",
                        "iterator step has no active iterator",
                    )));
                };
                match iterator {
                    IteratorState::List { values, index } => {
                        if let Some(value) = values.get(*index).cloned() {
                            *index += 1;
                            frame.stack.push(value);
                        } else {
                            self.iterators.pop();
                            frame.ip = target;
                        }
                    }
                    IteratorState::Record {
                        keys,
                        values,
                        index,
                    } => {
                        if let Some(key) = keys.get(*index) {
                            let value = values.get(key).cloned().unwrap_or(RuntimeValue::Nil);
                            *index += 1;
                            frame
                                .stack
                                .push(RuntimeValue::Record(Rc::new(BTreeMap::from([
                                    (
                                        "key".to_string(),
                                        RuntimeValue::String(Arc::from(key.as_str())),
                                    ),
                                    ("value".to_string(), value),
                                ]))));
                        } else {
                            self.iterators.pop();
                            frame.ip = target;
                        }
                    }
                }
            }
            Op::PopIterator => {
                self.iterators.pop();
            }
            Op::TryCatchSetup => {
                let target = read_u16(frame)?;
                let _type_name = read_u16(frame)?;
                frame.handlers.push(Handler {
                    target,
                    stack_depth: frame.stack.len(),
                    env: frame.env.clone(),
                });
            }
            Op::PopHandler => {
                frame.handlers.pop();
            }
            Op::Throw => return Ok(OpStep::Throw(pop!())),
            Op::Import => {
                let path = read_constant_string(frame)?;
                let env = frame.env.clone();
                return Ok(self.import_root_module(&path, None, None, &env));
            }
            Op::SelectiveImport => {
                let path = read_constant_string(frame)?;
                let names = read_constant_string(frame)?;
                let selected = names
                    .split(',')
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let env = frame.env.clone();
                return Ok(self.import_root_module(&path, Some(&selected), None, &env));
            }
            Op::NamespaceImport => {
                let path = read_constant_string(frame)?;
                let alias = read_constant_string(frame)?;
                let env = frame.env.clone();
                return Ok(self.import_root_module(&path, None, Some((&alias, None)), &env));
            }
            // The narrowed form the compiler emits when every use of the
            // namespace is a statically known member. The projection is the
            // demand set rather than the module's whole export surface;
            // escaping or dynamic uses compile to `NamespaceImport` instead,
            // so a missing member here is a compiler contract violation, not a
            // program error.
            Op::NamespaceImportMembers => {
                let path = read_constant_string(frame)?;
                let alias = read_constant_string(frame)?;
                let members = read_constant_string(frame)?;
                let demanded = members
                    .split(',')
                    .filter(|name| !name.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let env = frame.env.clone();
                return Ok(self.import_root_module(
                    &path,
                    None,
                    Some((&alias, Some(&demanded))),
                    &env,
                ));
            }
            Op::BuildEnum => {
                let enum_name = read_constant_string(frame)?;
                let variant = read_constant_string(frame)?;
                let field_count = read_u16(frame)?;
                let fields = pop_args(frame, field_count)?;
                let value = RuntimeValue::Enum(Rc::new(EnumValue {
                    enum_name: Arc::from(enum_name),
                    variant: Arc::from(variant),
                    fields: Rc::new(fields),
                }));
                self.charge_value_work(&value).map_err(OpStep::Error)?;
                frame.stack.push(value);
            }
            Op::MatchEnum => {
                let enum_name = read_constant_string(frame)?;
                let variant = read_constant_string(frame)?;
                let value = pop!();
                let matches = matches!(
                    &value,
                    RuntimeValue::Enum(candidate)
                        if candidate.is_variant(&enum_name, &variant)
                );
                frame.stack.push(value);
                frame.stack.push(RuntimeValue::Bool(matches));
            }
            Op::TryWrapOk => {
                let value = pop!();
                if matches!(&value, RuntimeValue::Enum(candidate) if candidate.enum_name.as_ref() == "Result")
                {
                    frame.stack.push(value);
                } else {
                    frame.stack.push(RuntimeValue::Enum(Rc::new(EnumValue {
                        enum_name: Arc::from("Result"),
                        variant: Arc::from("Ok"),
                        fields: Rc::new(vec![value]),
                    })));
                }
            }
            Op::TryUnwrap => {
                let value = pop!();
                let RuntimeValue::Enum(result) = &value else {
                    return Err(OpStep::Error(diagnostic(
                        "try_unwrap_type",
                        format!(
                            "? operator requires a Result value, got {}",
                            runtime_value_kind(&value)
                        ),
                    )));
                };
                if result.enum_name.as_ref() != "Result" {
                    return Err(OpStep::Error(diagnostic(
                        "try_unwrap_type",
                        format!(
                            "? operator requires a Result value, got {}",
                            runtime_value_kind(&value)
                        ),
                    )));
                }
                if result.variant.as_ref() == "Ok" {
                    frame
                        .stack
                        .push(result.fields.first().cloned().unwrap_or(RuntimeValue::Nil));
                } else {
                    return Ok(OpStep::Return(value));
                }
            }
            Op::AssertBindingType => {
                let index = read_u16(frame)?;
                let Some(slot) = frame.chunk.binding_types.get(index) else {
                    return Err(invalid_index("binding type", index));
                };
                let Some(value) = frame.stack.last() else {
                    return Err(OpStep::Error(diagnostic(
                        "stack_underflow",
                        "AssertBindingType requires the bound value on the stack",
                    )));
                };
                type_guard::validate_binding(value, slot).map_err(OpStep::Error)?;
            }
            Op::CheckType => {
                return Err(OpStep::Error(diagnostic(
                    "unsupported_portable_opcode",
                    format!("{} is not implemented by the portable kernel", op.name()),
                )))
            }
            Op::CallBuiltin => {
                frame.ip += 8;
                let name = read_constant_string(frame)?;
                let argc = read_u8(frame)?;
                let args = pop_args(frame, argc)?;
                return Ok(call_named(self, &frame.env, &name, args, false));
            }
            Op::CallBuiltinSpread => {
                frame.ip += 8;
                let name = read_constant_string(frame)?;
                let spread = pop!();
                let RuntimeValue::List(args) = spread else {
                    return Err(OpStep::Error(diagnostic(
                        "spread_type",
                        "spread call requires a list",
                    )));
                };
                return Ok(call_named(
                    self,
                    &frame.env,
                    &name,
                    Rc::unwrap_or_clone(args),
                    false,
                ));
            }
            unsupported @ (Op::Pipe
            | Op::Parallel
            | Op::ParallelMap
            | Op::ParallelMapStream
            | Op::ParallelSettle
            | Op::Spawn
            | Op::SyncMutexEnter
            | Op::SyncMutexEnterKeyed
            | Op::TaskScopeEnter
            | Op::TaskScopeExit
            | Op::DeadlineSetup
            | Op::DeadlineEnd
            | Op::CallSpread
            | Op::MethodCallSpread
            | Op::Yield) => {
                return Err(OpStep::Error(diagnostic(
                    "unsupported_portable_opcode",
                    format!(
                        "{} is not implemented by the portable kernel",
                        unsupported.name()
                    ),
                )))
            }
        }
        Ok(OpStep::Continue)
    }
}

struct Env {
    values: RefCell<BTreeMap<String, RuntimeValue>>,
    parent: Option<Rc<Env>>,
    depth: usize,
}

struct ModuleInstance {
    env: Rc<Env>,
    exports: BTreeMap<String, crate::PortableExportKind>,
}

enum IteratorState {
    List {
        values: Rc<Vec<RuntimeValue>>,
        index: usize,
    },
    Record {
        values: Rc<BTreeMap<String, RuntimeValue>>,
        keys: Vec<String>,
        index: usize,
    },
}

enum ModuleStep {
    Ready(Rc<ModuleInstance>),
    Suspend(CapabilityRequest),
    Error(Diagnostic),
}

/// A namespace import's alias and, when the compiler proved the whole set of
/// members a module actually demands, that demand set. `None` members means
/// the namespace escaped static analysis and must be projected whole.
type NamespaceProjection<'a> = (&'a str, Option<&'a [String]>);

impl Env {
    fn root() -> Rc<Self> {
        Rc::new(Self {
            values: RefCell::new(BTreeMap::new()),
            parent: None,
            depth: 0,
        })
    }
    fn child(parent: Rc<Self>) -> Result<Rc<Self>, Diagnostic> {
        if parent.depth >= MAX_SCOPE_DEPTH {
            return Err(diagnostic(
                "scope_depth_limit",
                "portable execution exceeded its lexical scope depth limit",
            ));
        }
        let depth = parent.depth + 1;
        Ok(Rc::new(Self {
            values: RefCell::new(BTreeMap::new()),
            parent: Some(parent),
            depth,
        }))
    }
    fn define(&self, name: String, value: RuntimeValue) {
        self.values.borrow_mut().insert(name, value);
    }
    fn contains_local(&self, name: &str) -> bool {
        self.values.borrow().contains_key(name)
    }
    fn get(&self, name: &str) -> Option<RuntimeValue> {
        self.values
            .borrow()
            .get(name)
            .cloned()
            .or_else(|| self.parent.as_ref().and_then(|parent| parent.get(name)))
    }
    fn set(&self, name: &str, value: RuntimeValue) {
        if self.values.borrow().contains_key(name) {
            self.values.borrow_mut().insert(name.to_string(), value);
        } else if let Some(parent) = &self.parent {
            parent.set(name, value);
        } else {
            self.values.borrow_mut().insert(name.to_string(), value);
        }
    }
}

struct Frame {
    chunk: Arc<Chunk>,
    ip: usize,
    stack: Vec<RuntimeValue>,
    locals: Vec<Option<RuntimeValue>>,
    env: Rc<Env>,
    handlers: Vec<Handler>,
    argc: usize,
}
impl Frame {
    fn new(chunk: Arc<Chunk>, env: Rc<Env>, arguments: Vec<RuntimeValue>) -> Self {
        let argc = arguments.len();
        let mut locals = vec![None; chunk.local_slots.len()];
        for (index, value) in arguments.into_iter().enumerate().take(locals.len()) {
            locals[index] = Some(value);
        }
        Self {
            chunk,
            ip: 0,
            stack: Vec::new(),
            locals,
            env,
            handlers: Vec::new(),
            argc,
        }
    }

    fn for_function(
        function: &CompiledFunction,
        env: Rc<Env>,
        mut arguments: Vec<RuntimeValue>,
    ) -> Self {
        let supplied = arguments.len();
        if function.has_rest_param && !function.params.is_empty() {
            let rest_index = function.params.len() - 1;
            let rest = if arguments.len() > rest_index {
                arguments.split_off(rest_index)
            } else {
                Vec::new()
            };
            arguments.push(RuntimeValue::List(Rc::new(rest)));
        } else {
            arguments.truncate(function.params.len());
        }
        let mut frame = Self::new(function.chunk.clone(), env, arguments);
        for (parameter, value) in function.params.iter().zip(frame.locals.iter()) {
            if let Some(value) = value {
                frame.env.define(parameter.name.clone(), value.clone());
            }
        }
        frame.argc = supplied;
        frame
    }
}
struct Handler {
    target: usize,
    stack_depth: usize,
    env: Rc<Env>,
}
enum Step {
    Value(RuntimeValue),
    Suspend(CapabilityRequest),
    Error(Diagnostic),
}
enum OpStep {
    Continue,
    Push(RuntimeValue),
    Call(Closure, Vec<RuntimeValue>, bool),
    Return(RuntimeValue),
    Suspend(CapabilityRequest),
    Throw(RuntimeValue),
    Error(Diagnostic),
}

fn read_u8(frame: &mut Frame) -> Result<usize, OpStep> {
    let value = *frame.chunk.code.get(frame.ip).ok_or_else(|| {
        OpStep::Error(diagnostic(
            "truncated_instruction",
            "u8 operand is truncated",
        ))
    })?;
    frame.ip += 1;
    Ok(value as usize)
}
fn read_u16(frame: &mut Frame) -> Result<usize, OpStep> {
    let bytes = frame
        .chunk
        .code
        .get(frame.ip..frame.ip + 2)
        .ok_or_else(|| {
            OpStep::Error(diagnostic(
                "truncated_instruction",
                "u16 operand is truncated",
            ))
        })?;
    frame.ip += 2;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]) as usize)
}
fn read_constant_string(frame: &mut Frame) -> Result<String, OpStep> {
    let index = read_u16(frame)?;
    match frame.chunk.constants.get(index) {
        Some(Constant::String(value)) => Ok(value.clone()),
        _ => Err(invalid_index("string constant", index)),
    }
}
fn pop_args(frame: &mut Frame, count: usize) -> Result<Vec<RuntimeValue>, OpStep> {
    if frame.stack.len() < count {
        return Err(OpStep::Error(diagnostic(
            "stack_underflow",
            "call argument stack is truncated",
        )));
    }
    Ok(frame.stack.split_off(frame.stack.len() - count))
}
fn invalid_index(kind: &str, index: usize) -> OpStep {
    OpStep::Error(diagnostic(
        "invalid_index",
        format!("{kind} index {index} is out of bounds"),
    ))
}

fn call_value(
    machine: &mut Machine<'_>,
    env: &Rc<Env>,
    callee: RuntimeValue,
    args: Vec<RuntimeValue>,
    tail: bool,
) -> OpStep {
    match callee {
        RuntimeValue::Closure(closure) => OpStep::Call(closure, args, tail),
        RuntimeValue::Builtin(name) => machine.call_builtin(&name, args),
        // Optimized named tail calls carry the source name as a string. Match
        // the native VM's lexical-first late binding so recursion, mutual
        // recursion, and sibling calls all retain one compiler representation.
        RuntimeValue::String(name) => call_named(machine, env, &name, args, tail),
        RuntimeValue::Harness(capability) => {
            machine.call_method(RuntimeValue::Harness(capability), "call", args)
        }
        other => OpStep::Error(diagnostic(
            "not_callable",
            format!("{} is not callable", runtime_value_kind(&other)),
        )),
    }
}

fn call_named(
    machine: &mut Machine<'_>,
    env: &Rc<Env>,
    name: &str,
    args: Vec<RuntimeValue>,
    tail: bool,
) -> OpStep {
    match env.get(name) {
        Some(callee) => call_value(machine, env, callee, args, tail),
        None => machine.call_builtin(name, args),
    }
}
