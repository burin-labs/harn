use std::sync::Arc;

use crate::value::{VmClosure, VmEnv, VmError, VmValue};

pub(super) fn compile_synthesized_tool_closure(id: &str) -> Result<VmValue, VmError> {
    let source = format!(
        "fn __harn_synthesized_tool(args: unknown) {{ return tool_synth_invoke(\"{id}\", args) }}"
    );
    let program = harn_parser::check_source_strict(&source).map_err(|error| {
        VmError::Runtime(format!("tool_synthesize: internal compile failed: {error}"))
    })?;
    let Some(fn_node) = program.iter().find_map(|node| match &node.node {
        harn_parser::Node::FnDecl {
            type_params,
            params,
            body,
            throws,
            ..
        } => Some((type_params, params, body, throws)),
        _ => None,
    }) else {
        return Err(VmError::Runtime(
            "tool_synthesize: internal closure source had no function".to_string(),
        ));
    };
    let mut compiler = crate::Compiler::new_runtime_owned_source();
    let func = compiler
        .compile_fn_body(
            fn_node.0,
            fn_node.1,
            fn_node.2,
            Some("<tool_synthesize>".to_string()),
            fn_node.3.as_ref(),
        )
        .map_err(|error| VmError::Runtime(format!("tool_synthesize: {error}")))?;
    Ok(VmValue::Closure(Arc::new(VmClosure {
        func: Arc::new(func),
        env: VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
        retained_module_scope: None,
    })))
}
