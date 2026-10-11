//! Source compilation at explicit inline and file-backed boundaries.

use std::path::Path;

use crate::{Chunk, Compiler};

/// Lex, parse, type-check, and compile inline source to bytecode.
/// Bails on the first type error without consulting filesystem imports.
/// File-backed hosts should use [`compile_source_at_path`] instead.
pub fn compile_source(source: &str) -> Result<Chunk, String> {
    let program = harn_parser::check_source_strict(source).map_err(|e| e.to_string())?;
    compile_entry(Compiler::new(), &program, None)
}

/// Compile inline source with a specific named pipeline as the entry point.
/// Like [`compile_source`], this does not resolve filesystem imports.
pub fn compile_source_named(source: &str, pipeline_name: &str) -> Result<Chunk, String> {
    let program = harn_parser::check_source_strict(source).map_err(|e| e.to_string())?;
    compile_entry(Compiler::new(), &program, Some(pipeline_name))
}

/// Compile caller-owned source anchored at a real module path.
///
/// The module graph resolves imported types, callable contracts, namespaces,
/// and enum constructors from relative files, packages, and embedded stdlib.
/// `source` supplies the root buffer even when the file differs on disk.
/// This entry point retains ordinary user-source compiler authority; a nearby
/// manifest cannot grant privileged host dispatch or runtime implementation calls.
pub fn compile_source_at_path(
    source: &str,
    source_path: &Path,
    pipeline_name: Option<&str>,
) -> Result<Chunk, String> {
    let program = harn_parser::parse_source(source).map_err(|error| error.to_string())?;
    let graph = harn_modules::build_with_source(source_path, source);
    for import in graph.imports_for_module(source_path) {
        if import.resolved_path.is_none() {
            return Err(format!("cannot resolve import `{}`", import.raw_path));
        }
    }
    if let Some(failure) = graph.import_compile_failures(source_path).first() {
        return Err(format!(
            "{}: {}",
            failure.module_path.display(),
            failure.error.message
        ));
    }
    if let Some(issue) = graph.selective_import_issues(source_path).first() {
        return Err(issue.message());
    }
    let imported =
        crate::module_artifact::ModuleCompilationContext::from_graph(&graph, source_path);
    let compiler = Compiler::new()
        .with_imported_enum_candidates(imported.enum_candidates().iter().cloned())
        .with_imported_source_callable_names(imported.source_callable_names().iter().cloned());
    let facts = graph
        .typecheck_import_config_for_file(source_path)
        .check_with_facts(&program, source);
    if let Some(error) = facts
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.severity == harn_parser::DiagnosticSeverity::Error)
    {
        return Err(harn_parser::PipelineError::TypeCheck(Box::new(error.clone())).to_string());
    }
    compile_entry(compiler, &program, pipeline_name)
}

fn compile_entry(
    compiler: Compiler,
    program: &[harn_parser::SNode],
    pipeline_name: Option<&str>,
) -> Result<Chunk, String> {
    if let Some(name) = pipeline_name {
        let has_pipeline = program.iter().any(|node| {
            let (_, inner) = harn_parser::peel_attributes(node);
            matches!(&inner.node, harn_parser::Node::Pipeline { name: candidate, .. } if candidate == name)
        });
        if !has_pipeline {
            return Err(format!("no pipeline named `{name}` in source"));
        }
        compiler.compile_named(program, name)
    } else {
        compiler.compile(program)
    }
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_backed_compilation_resolves_reexported_type_dependencies() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("types.harn"),
            "type Count = int\npub type Options = {count: Count}\npub fn options(value: Options) -> Options { value }\n").unwrap();
        std::fs::write(
            root.path().join("facade.harn"),
            "pub import {Options, options} from \"./types.harn\"\n",
        )
        .unwrap();
        let path = root.path().join("main.harn");
        // The caller's root buffer wins over the different disk contents.
        std::fs::write(&path, "this is not a valid pipeline").unwrap();
        let source = "import {Options, options} from \"./facade.harn\"\npipeline custom(harness: Harness) { const value: Options = {count: 1}; const validated = options(value); assert_eq(validated.count, 1) }";
        assert!(compile_source(source).unwrap_err().contains("Options"));
        compile_source_at_path(source, &path, Some("custom")).unwrap();
        let invalid = source.replace("count: 1", "count: \"wrong\"");
        assert!(compile_source_at_path(&invalid, &path, Some("custom"))
            .unwrap_err()
            .contains("Options"));
        let unknown = source.replace("options(value)", "options({count: 1, typo: true})");
        assert!(compile_source_at_path(&unknown, &path, None)
            .unwrap_err()
            .contains("unknown field `typo`"));
        let private = source
            .replace("./facade.harn", "./types.harn")
            .replace("{Options, options}", "{Count}")
            .replace("const value: Options", "const value: Count");
        assert!(compile_source_at_path(&private, &path, None)
            .unwrap_err()
            .contains("not exported"));
    }

    #[test]
    fn file_backed_compilation_checks_stdlib_options_and_keeps_user_authority() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("main.harn");
        let source = "import {AgentSpec, agent_options} from \"std/agent/options\"\npipeline main(harness: Harness) { const options: AgentSpec = {provider: \"mock\", model: \"proof\", max_iterations: 1, on_delta: {delta -> harness.stdio.print(delta)}}; const checked = agent_options(options) }";
        compile_source_at_path(source, &path, None).unwrap();
        let invalid = source.replace("provider: \"mock\"", "provider: 7");
        assert!(compile_source_at_path(&invalid, &path, None)
            .unwrap_err()
            .contains("AgentSpec"));
        let unknown = source.replace(
            "agent_options(options)",
            "agent_options({max_iteratons: 1})",
        );
        assert!(compile_source_at_path(&unknown, &path, None)
            .unwrap_err()
            .contains("unknown field `max_iteratons`"));
        std::fs::write(
            root.path().join("harn.toml"),
            "[check]\ntrusted_host_dispatch = true\n",
        )
        .unwrap();
        let privileged = "pipeline main(harness: Harness) { host_call(\"proof\", {}) }";
        assert!(compile_source_at_path(privileged, &path, None)
            .unwrap_err()
            .contains("host_call"));
        assert!(compile_source_at_path(source, &path, Some("missing"))
            .unwrap_err()
            .contains("no pipeline named `missing`"));
    }
}
