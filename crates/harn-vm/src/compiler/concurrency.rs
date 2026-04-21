use harn_parser::{BindingPattern, ParallelMode, SNode, SelectCase, TypeExpr};

use crate::chunk::{CompiledFunction, Constant, Op};

use super::error::CompileError;
use super::Compiler;

impl Compiler {
    pub(super) fn compile_parallel(
        &mut self,
        mode: &ParallelMode,
        expr: &SNode,
        variable: &Option<String>,
        body: &[SNode],
        options: &[(String, SNode)],
    ) -> Result<(), CompileError> {
        // Push the `max_concurrent` cap first so the runtime
        // opcodes can pop it beneath the iterable + closure. A
        // cap of 0 (or a missing `with { ... }` clause) means
        // "unlimited". Unknown option keys are parser errors.
        let cap_expr = options
            .iter()
            .find(|(key, _)| key == "max_concurrent")
            .map(|(_, value)| value);
        if let Some(cap_expr) = cap_expr {
            self.compile_node(cap_expr)?;
        } else {
            let zero_idx = self.chunk.add_constant(Constant::Int(0));
            self.chunk.emit_u16(Op::Constant, zero_idx, self.line);
        }
        let (fn_name, params) = match mode {
            ParallelMode::Count => (
                "<parallel>",
                vec![variable.clone().unwrap_or_else(|| "__i__".to_string())],
            ),
            ParallelMode::Each => (
                "<parallel_each>",
                vec![variable.clone().unwrap_or_else(|| "__item__".to_string())],
            ),
            ParallelMode::Settle => (
                "<parallel_settle>",
                vec![variable.clone().unwrap_or_else(|| "__item__".to_string())],
            ),
        };
        let param_type = match mode {
            ParallelMode::Count => Some(TypeExpr::Named("int".into())),
            ParallelMode::Each | ParallelMode::Settle => self.infer_for_item_type(expr),
        };
        self.compile_node(expr)?;
        let mut fn_compiler = Compiler::for_nested_body();
        fn_compiler.enum_names = self.enum_names.clone();
        fn_compiler.interface_methods = self.interface_methods.clone();
        fn_compiler.type_aliases = self.type_aliases.clone();
        if let Some(param_name) = params.first() {
            fn_compiler
                .record_binding_type(&BindingPattern::Identifier(param_name.clone()), param_type);
        }
        fn_compiler.compile_block(body)?;
        fn_compiler.chunk.emit(Op::Return, self.line);
        let func = CompiledFunction {
            name: fn_name.to_string(),
            params,
            default_start: None,
            chunk: fn_compiler.chunk,
            is_generator: false,
            has_rest_param: false,
        };
        let fn_idx = self.chunk.functions.len();
        self.chunk.functions.push(func);
        self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
        let op = match mode {
            ParallelMode::Count => Op::Parallel,
            ParallelMode::Each => Op::ParallelMap,
            ParallelMode::Settle => Op::ParallelSettle,
        };
        self.chunk.emit(op, self.line);
        Ok(())
    }

    pub(super) fn compile_spawn_expr(&mut self, body: &[SNode]) -> Result<(), CompileError> {
        let mut fn_compiler = Compiler::for_nested_body();
        fn_compiler.enum_names = self.enum_names.clone();
        fn_compiler.interface_methods = self.interface_methods.clone();
        fn_compiler.type_aliases = self.type_aliases.clone();
        fn_compiler.compile_block(body)?;
        fn_compiler.chunk.emit(Op::Return, self.line);
        let func = CompiledFunction {
            name: "<spawn>".to_string(),
            params: vec![],
            default_start: None,
            chunk: fn_compiler.chunk,
            is_generator: false,
            has_rest_param: false,
        };
        let fn_idx = self.chunk.functions.len();
        self.chunk.functions.push(func);
        self.chunk.emit_u16(Op::Closure, fn_idx as u16, self.line);
        self.chunk.emit(Op::Spawn, self.line);
        Ok(())
    }

    pub(super) fn compile_select_expr(
        &mut self,
        cases: &[SelectCase],
        timeout: &Option<(Box<SNode>, Vec<SNode>)>,
        default_body: &Option<Vec<SNode>>,
    ) -> Result<(), CompileError> {
        // Desugar `select` into a builtin call returning a dict with
        // {index, value}, then dispatch on result.index. `index == -1`
        // means timeout / default fell through.
        let builtin_name = if timeout.is_some() {
            "__select_timeout"
        } else if default_body.is_some() {
            "__select_try"
        } else {
            "__select_list"
        };

        let name_idx = self
            .chunk
            .add_constant(Constant::String(builtin_name.into()));
        self.chunk.emit_u16(Op::Constant, name_idx, self.line);

        for case in cases {
            self.compile_node(&case.channel)?;
        }
        self.chunk
            .emit_u16(Op::BuildList, cases.len() as u16, self.line);

        if let Some((duration_expr, _)) = timeout {
            self.compile_node(duration_expr)?;
            self.chunk.emit_u8(Op::Call, 2, self.line);
        } else {
            self.chunk.emit_u8(Op::Call, 1, self.line);
        }

        self.temp_counter += 1;
        let result_name = format!("__sel_result_{}__", self.temp_counter);
        let result_idx = self
            .chunk
            .add_constant(Constant::String(result_name.clone()));
        self.chunk.emit_u16(Op::DefVar, result_idx, self.line);

        let mut end_jumps = Vec::new();

        for (i, case) in cases.iter().enumerate() {
            let get_r = self
                .chunk
                .add_constant(Constant::String(result_name.clone()));
            self.chunk.emit_u16(Op::GetVar, get_r, self.line);
            let idx_prop = self.chunk.add_constant(Constant::String("index".into()));
            self.chunk.emit_u16(Op::GetProperty, idx_prop, self.line);
            let case_i = self.chunk.add_constant(Constant::Int(i as i64));
            self.chunk.emit_u16(Op::Constant, case_i, self.line);
            self.chunk.emit(Op::Equal, self.line);
            let skip = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
            self.chunk.emit(Op::Pop, self.line);
            self.begin_scope();

            let get_r2 = self
                .chunk
                .add_constant(Constant::String(result_name.clone()));
            self.chunk.emit_u16(Op::GetVar, get_r2, self.line);
            let val_prop = self.chunk.add_constant(Constant::String("value".into()));
            self.chunk.emit_u16(Op::GetProperty, val_prop, self.line);
            let var_idx = self
                .chunk
                .add_constant(Constant::String(case.variable.clone()));
            self.chunk.emit_u16(Op::DefLet, var_idx, self.line);

            self.compile_try_body(&case.body)?;
            self.end_scope();
            end_jumps.push(self.chunk.emit_jump(Op::Jump, self.line));
            self.chunk.patch_jump(skip);
            self.chunk.emit(Op::Pop, self.line);
        }

        if let Some((_, ref timeout_body)) = timeout {
            self.compile_try_body(timeout_body)?;
        } else if let Some(ref def_body) = default_body {
            self.compile_try_body(def_body)?;
        } else {
            self.chunk.emit(Op::Nil, self.line);
        }

        for ej in end_jumps {
            self.chunk.patch_jump(ej);
        }
        Ok(())
    }
}
