use harn_parser::{BindingPattern, ShapeField, TypeExpr, TypedParam};
use std::sync::Arc;

use crate::chunk::{CompiledFunction, Op};

use super::{CompileError, Compiler};

// Not a source identifier: a tool parameter or captured source binding cannot
// shadow the dictionary used to initialize later parameters.
const TOOL_ARGUMENTS: &str = "<tool arguments>";

impl Compiler {
    /// Registry calls use named arguments; language calls use the original
    /// positional body. Compile that body once and invoke it from this adapter.
    pub(super) fn compile_tool_argument_adapter(
        &self,
        name: &str,
        params: &[TypedParam],
        body: Arc<CompiledFunction>,
    ) -> Result<Arc<CompiledFunction>, CompileError> {
        let mut adapter = self.nested_body();
        adapter.enum_names = self.enum_names.clone();
        adapter.enum_variant_owners = self.enum_variant_owners.clone();
        adapter.imported_enum_candidates = self.imported_enum_candidates.clone();
        adapter.imported_enum_candidates_authoritative =
            self.imported_enum_candidates_authoritative;
        adapter.interface_methods = self.interface_methods.clone();
        adapter.type_aliases = self.type_aliases.clone();
        adapter.struct_layouts = self.struct_layouts.clone();
        let handler_params = adapter.emit_tool_parameter_bindings(params)?;
        // Default expressions may have already compiled nested closures.
        let body_idx = adapter.chunk.functions.len();
        adapter.chunk.functions.push(body);
        adapter
            .chunk
            .emit_u16(Op::Closure, body_idx as u16, self.line);
        let fixed_count = params.len() - usize::from(params.last().is_some_and(|p| p.rest));
        for param in &params[..fixed_count] {
            adapter.emit_get_binding(&param.name);
        }
        adapter
            .chunk
            .emit_u16(Op::BuildList, fixed_count as u16, self.line);
        if let Some(rest) = params.last().filter(|p| p.rest) {
            adapter.emit_get_binding(&rest.name);
            adapter.chunk.emit(Op::Add, self.line);
        }
        adapter.chunk.emit(Op::CallSpread, self.line);
        adapter.chunk.emit(Op::Return, self.line);
        let param_slots = adapter.compile_param_slots(&handler_params);
        let has_runtime_type_checks =
            CompiledFunction::has_runtime_type_checks_for_params(&param_slots);
        super::ensure_chunk_addressable(&adapter.chunk, &format!("tool `{name}`"), self.line)?;
        Ok(Arc::new(CompiledFunction {
            name: name.to_string(),
            type_params: Vec::new(),
            nominal_type_names: adapter.nominal_type_names(),
            params: param_slots,
            default_start: None,
            chunk: Arc::new(adapter.chunk),
            is_generator: false,
            is_stream: false,
            has_rest_param: false,
            has_runtime_type_checks,
        }))
    }

    /// Every registry consumer invokes a handler with one dictionary. Lower
    /// declared parameters at the compiler boundary, before the original body,
    /// so agent, CLI, and MCP execution share that invocation contract.
    pub(super) fn emit_tool_parameter_bindings(
        &mut self,
        params: &[TypedParam],
    ) -> Result<Vec<TypedParam>, CompileError> {
        let fields = params
            .iter()
            .map(|param| {
                let value_type = param
                    .type_expr
                    .clone()
                    .unwrap_or_else(|| TypeExpr::Named("unknown".into()));
                ShapeField::synthetic(
                    &param.name,
                    if param.rest {
                        TypeExpr::List(Box::new(value_type))
                    } else {
                        value_type
                    },
                    param.default_value.is_some() || param.rest,
                )
            })
            .collect();
        let handler_params = vec![TypedParam::typed(TOOL_ARGUMENTS, TypeExpr::Shape(fields))];
        self.declare_param_slots(&handler_params);
        self.declare_param_slots(params);
        self.record_param_types(params);

        for (index, param) in params.iter().enumerate() {
            let key = self.string_constant(&param.name);
            let absent_jump = if param.default_value.is_some() || param.rest {
                self.emit_get_binding(TOOL_ARGUMENTS);
                self.chunk.emit_u16(Op::Constant, key, self.line);
                let has = self.string_constant("has");
                self.chunk.emit_method_call(has, 1, self.line);
                let jump = self.chunk.emit_jump(Op::JumpIfFalse, self.line);
                self.chunk.emit(Op::Pop, self.line);
                Some(jump)
            } else {
                None
            };

            self.emit_get_binding(TOOL_ARGUMENTS);
            self.chunk.emit_u16(Op::Constant, key, self.line);
            self.chunk.emit(Op::Subscript, self.line);

            if let Some(absent_jump) = absent_jump {
                let supplied_jump = self.chunk.emit_jump(Op::Jump, self.line);
                self.chunk.patch_jump(absent_jump);
                self.chunk.emit(Op::Pop, self.line);
                if let Some(default) = &param.default_value {
                    // Match function defaults: earlier parameters are visible;
                    // this parameter and later ones resolve in the outer scope.
                    let masked = self.mask_param_names(&params[index..]);
                    let result = self.compile_node(default);
                    self.restore_param_names(masked);
                    result?;
                } else {
                    self.chunk.emit_u16(Op::BuildList, 0, self.line);
                }
                self.chunk.patch_jump(supplied_jump);
            }

            let type_ann = param.type_expr.as_ref().map(|value_type| {
                if param.rest {
                    TypeExpr::List(Box::new(value_type.clone()))
                } else {
                    value_type.clone()
                }
            });
            self.emit_binding_type_assertion(
                &BindingPattern::Identifier(param.name.clone()),
                type_ann.as_ref(),
            );
            self.emit_init_or_define_binding(&param.name, false);
        }

        // Ordinary values and defaults were checked above. Retain the existing
        // interface checks, whose method metadata is owned by the compiler.
        let mut bound_params = params.to_vec();
        for param in &mut bound_params {
            param.default_value = None;
        }
        self.emit_type_checks(&bound_params);
        Ok(handler_params)
    }
}
