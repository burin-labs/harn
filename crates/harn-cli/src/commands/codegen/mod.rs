//! Harn owns validation, plugin isolation and publication; backends only render IR.

#[cfg(feature = "hostlib")]
mod artifacts;
#[cfg(feature = "hostlib")]
mod plugin;

use std::io::Write;
use std::path::Path;

use harn_vm::codegen::{CodegenDiagnostic, CodegenDiagnosticCode as Code, ValidatedCodegenIr};
#[cfg(feature = "hostlib")]
use harn_vm::codegen::{CodegenRequest, CodegenRequestVersion};

use crate::cli::{
    CodegenArgs, CodegenCommand, CodegenGenerateArgs, CodegenIrArgs, ToolSchemaSurface,
};

const MAX_IR_BYTES: u64 = 64 * 1024 * 1024;

pub(crate) async fn run(args: CodegenArgs) -> i32 {
    match run_inner(args).await {
        Ok(()) => 0,
        Err(error) => {
            eprintln!(
                "{}",
                serde_json::to_string(&error).expect("codegen diagnostic serializes")
            );
            2
        }
    }
}

async fn run_inner(args: CodegenArgs) -> Result<(), CodegenDiagnostic> {
    match args.command {
        CodegenCommand::Ir(args) => print_ir(compile_ir(&args).await?.0),
        CodegenCommand::ValidateIr(args) => print_ir(read_ir(&args.input)?),
        CodegenCommand::Generate(args) => generate(&args).await,
    }
}

fn read_ir(path: &Path) -> Result<ValidatedCodegenIr, CodegenDiagnostic> {
    let file =
        std::fs::File::open(path).map_err(|_| CodegenDiagnostic::new(Code::SourceUnavailable))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut std::io::Read::take(file, MAX_IR_BYTES + 1), &mut bytes)
        .map_err(|_| CodegenDiagnostic::new(Code::SourceUnavailable))?;
    if bytes.len() as u64 > MAX_IR_BYTES {
        return Err(CodegenDiagnostic::new(Code::InvalidIr));
    }
    harn_vm::codegen::validate_ir(&bytes)
}

async fn compile_ir(
    args: &CodegenIrArgs,
) -> Result<(ValidatedCodegenIr, harn_modules::SourceDocuments), CodegenDiagnostic> {
    match args.surface {
        ToolSchemaSurface::Exports => {
            harn_serve::compile_codegen_ir_with_sources(&args.file, &args.select)
        }
        ToolSchemaSurface::Script => {
            let loaded = crate::commands::run::load_file_tool_registry_with_sources(
                &args.file.to_string_lossy(),
            )
            .await
            .map_err(|_| CodegenDiagnostic::new(Code::SourceUnavailable))?;
            let catalog = harn_vm::tool_registry::tool_registry_catalog_for_audience(
                &loaded.registry,
                harn_vm::tool_registry::ToolAudience::Catalog,
            )
            .map_err(|_| CodegenDiagnostic::new(Code::InvalidIr))?;
            let documents = loaded
                .source_context
                .as_ref()
                .and_then(|context| context.source_documents())
                .ok_or_else(|| CodegenDiagnostic::new(Code::SourceUnavailable))?;
            let catalog = harn_vm::codegen::normalize_catalog_sources(catalog, &documents)?;
            let prepared = harn_vm::tool_registry::PreparedToolCatalog::prepare(catalog)
                .map_err(|_| CodegenDiagnostic::new(Code::InvalidIr))?;
            let sources = harn_serve::codegen_source_refs_for_documents(&documents)?;
            let ir =
                harn_vm::codegen::compile_catalog_with_sources(&prepared, &args.select, &sources)?;
            Ok((ir, documents))
        }
    }
}

fn print_ir(ir: ValidatedCodegenIr) -> Result<(), CodegenDiagnostic> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(&ir.canonical_json())
        .and_then(|()| stdout.write_all(b"\n"))
        .map_err(|_| CodegenDiagnostic::new(Code::ArtifactWriteFailed))
}

#[cfg(feature = "hostlib")]
async fn generate(args: &CodegenGenerateArgs) -> Result<(), CodegenDiagnostic> {
    let (ir, documents) = compile_ir(&args.source).await?;
    if let Some(frozen) = args.ir.as_ref() {
        if read_ir(frozen)?.ir().identity != ir.ir().identity {
            return Err(CodegenDiagnostic::new(Code::InvalidDigest));
        }
    }
    let request = CodegenRequest {
        schema_version: CodegenRequestVersion::V1,
        role: args.role.into(),
        ir: ir.into_ir(),
    };
    let first = plugin::invoke(&args.plugin, &args.plugin_read_root, &request, &documents)?;
    let second = plugin::invoke(&args.plugin, &args.plugin_read_root, &request, &documents)?;
    if first != second {
        return Err(CodegenDiagnostic::new(Code::NonDeterministicOutput));
    }
    first.publish(&args.out, &request.ir.identity, request.role, args.check)?;
    Ok(())
}

#[cfg(not(feature = "hostlib"))]
async fn generate(_args: &CodegenGenerateArgs) -> Result<(), CodegenDiagnostic> {
    Err(CodegenDiagnostic::new(Code::PluginSpawnFailed))
}
