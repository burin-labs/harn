//! Native adapter over the portable canonical compiler.

use crate::chunk::{Chunk, CompiledFunction};

pub use harn_kernel::compiler::HARN_DISABLE_OPTIMIZATIONS_ENV;
pub use harn_kernel::{CompileError, CompilerOptions};

#[derive(Clone)]
pub struct CompiledCallableEntry {
    pub(crate) bootstrap: Chunk,
    pub(crate) has_fixture: bool,
    pub(crate) fixture_expects_harness: bool,
    pub(crate) expects_harness: bool,
}

pub struct Compiler {
    inner: harn_kernel::Compiler,
}

impl Compiler {
    pub fn new() -> Self {
        install_native_builtin_contracts();
        Self {
            inner: harn_kernel::Compiler::new(),
        }
    }

    pub fn new_trusted_host_dispatch() -> Self {
        install_native_builtin_contracts();
        Self {
            inner: harn_kernel::Compiler::new_trusted_host_dispatch(),
        }
    }

    pub fn with_options(options: CompilerOptions) -> Self {
        install_native_builtin_contracts();
        Self {
            inner: harn_kernel::Compiler::with_options(options),
        }
    }

    pub fn with_imported_enum_candidates(
        self,
        candidates: impl IntoIterator<Item = String>,
    ) -> Self {
        Self {
            inner: self.inner.with_imported_enum_candidates(candidates),
        }
    }

    pub fn compile(self, program: &[harn_parser::SNode]) -> Result<Chunk, CompileError> {
        self.inner.compile(program).map(Chunk::from_portable)
    }

    pub fn compile_named(
        self,
        program: &[harn_parser::SNode],
        name: &str,
    ) -> Result<Chunk, CompileError> {
        self.inner
            .compile_named(program, name)
            .map(Chunk::from_portable)
    }

    pub fn compile_named_pipeline_entry(
        self,
        program: &[harn_parser::SNode],
        name: &str,
        fixture: Option<&str>,
    ) -> Result<CompiledCallableEntry, CompileError> {
        self.inner
            .compile_named_pipeline_entry(program, name, fixture)
            .map(convert_entry)
    }

    pub fn compile_named_function_entry(
        self,
        program: &[harn_parser::SNode],
        name: &str,
    ) -> Result<CompiledCallableEntry, CompileError> {
        self.inner
            .compile_named_function_entry(program, name)
            .map(convert_entry)
    }

    pub fn prepare_module_context(&mut self, program: &[harn_parser::SNode]) {
        self.inner.prepare_module_context(program);
    }

    pub fn add_imported_enum_candidates(&mut self, candidates: impl IntoIterator<Item = String>) {
        self.inner.add_imported_enum_candidates(candidates);
    }

    pub fn compile_module_init(
        self,
        context: &[harn_parser::SNode],
        init_nodes: &[harn_parser::SNode],
        imported_enums: &[String],
    ) -> Result<Chunk, CompileError> {
        self.inner
            .compile_module_init(context, init_nodes, imported_enums)
            .map(Chunk::from_portable)
    }

    pub fn compile_struct_constructor(
        &self,
        name: &str,
        fields: &[harn_parser::StructField],
    ) -> Result<CompiledFunction, CompileError> {
        self.inner
            .compile_struct_constructor(name, fields)
            .map(CompiledFunction::from_portable)
    }

    pub fn compile_pipeline_callable(
        &self,
        program: &[harn_parser::SNode],
        name: &str,
        params: &[harn_parser::TypedParam],
        body: &[harn_parser::SNode],
        extends: Option<&str>,
    ) -> Result<CompiledFunction, CompileError> {
        self.inner
            .compile_pipeline_callable(program, name, params, body, extends)
            .map(CompiledFunction::from_portable)
    }

    pub fn compile_fn_body(
        &mut self,
        type_params: &[harn_parser::TypeParam],
        params: &[harn_parser::TypedParam],
        body: &[harn_parser::SNode],
        source_file: Option<String>,
    ) -> Result<CompiledFunction, CompileError> {
        self.inner
            .compile_fn_body(type_params, params, body, source_file)
            .map(CompiledFunction::from_portable)
    }

    pub fn compile_public_type_schema_initializers(
        program: &[harn_parser::SNode],
        source_file: Option<String>,
    ) -> Result<Option<Chunk>, CompileError> {
        harn_kernel::Compiler::compile_public_type_schema_initializers(program, source_file)
            .map(|chunk| chunk.map(Chunk::from_portable))
    }

    pub fn collect_type_aliases(&mut self, program: &[harn_parser::SNode]) {
        self.inner.collect_type_aliases(program);
    }

    pub fn expand_alias(&self, ty: &harn_parser::TypeExpr) -> harn_parser::TypeExpr {
        self.inner.expand_alias(ty)
    }

    pub fn type_expr_to_schema_value(type_expr: &harn_parser::TypeExpr) -> Option<crate::VmValue> {
        harn_kernel::Compiler::type_expr_to_schema_value(type_expr).map(portable_value_to_vm)
    }
}

fn install_native_builtin_contracts() {
    harn_builtin_registry::install_builtin_manifest(crate::stdlib::all_builtin_manifest());
}

impl Default for Compiler {
    fn default() -> Self {
        Self::new()
    }
}

fn convert_entry(entry: harn_kernel::CompiledCallableEntry) -> CompiledCallableEntry {
    CompiledCallableEntry {
        bootstrap: Chunk::from_portable(entry.bootstrap),
        has_fixture: entry.has_fixture,
        fixture_expects_harness: entry.fixture_expects_harness,
        expects_harness: entry.expects_harness,
    }
}

fn portable_value_to_vm(value: harn_kernel::value::VmValue) -> crate::VmValue {
    match value {
        harn_kernel::value::VmValue::Int(value) => crate::VmValue::Int(value),
        harn_kernel::value::VmValue::Float(value) => crate::VmValue::Float(value),
        harn_kernel::value::VmValue::String(value) => crate::VmValue::string(value),
        harn_kernel::value::VmValue::Bool(value) => crate::VmValue::Bool(value),
        harn_kernel::value::VmValue::Nil => crate::VmValue::Nil,
        harn_kernel::value::VmValue::Duration(value) => crate::VmValue::Duration(value),
        harn_kernel::value::VmValue::List(values) => crate::VmValue::List(std::sync::Arc::new(
            values.iter().cloned().map(portable_value_to_vm).collect(),
        )),
        harn_kernel::value::VmValue::Dict(entries) => crate::VmValue::dict(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), portable_value_to_vm(value.clone()))),
        ),
    }
}
