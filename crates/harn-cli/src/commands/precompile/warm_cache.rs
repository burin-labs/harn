//! Prepare machine-local cache identities without running user code.

use std::path::Path;

use harn_vm::bytecode_cache;

use super::*;

pub(super) fn run(args: &PrecompileArgs) {
    let target = args.target.as_ref().expect("required precompile target");
    let mut sources = Vec::new();
    if target.is_dir() {
        collect_harn_files(target, &mut sources);
    } else {
        sources.push(target.clone());
    }
    sources.sort();
    sources.dedup();
    if sources.is_empty() {
        command_error(&format!("no .harn files found under {}", target.display()));
    }
    ensure_builtin_signatures_installed();
    let mut compiler = bytecode_cache::CachePrecompiler::default();
    if !args.quiet {
        cli_diagnostic!("precompile cache: preparing the runtime stdlib");
    }
    compiler
        .prepare_stdlib_catalog()
        .unwrap_or_else(|error| command_error(&error.to_string()));
    let mut prepared = 0;
    let mut failed = 0;
    for source in &sources {
        match prepare_entry(source, &mut compiler) {
            Ok(()) => {
                prepared += 1;
                if !args.quiet {
                    println!("{} -> runtime cache", source.display());
                }
            }
            Err(error) => {
                failed += 1;
                eprintln!("{}: {error}", source.display());
                if !args.keep_going {
                    break;
                }
            }
        }
    }
    eprintln!(
        "precompile cache: {prepared} entries, {} modules, {failed} failed",
        compiler.module_count()
    );
    if failed > 0 {
        std::process::exit(1);
    }
}

fn prepare_entry(
    path: &Path,
    compiler: &mut bytecode_cache::CachePrecompiler,
) -> Result<(), String> {
    let source = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut graph = harn_modules::build_with_source(path, &source);
    if crate::package::ensure_reachable_dependencies_materialized(path, &graph)
        .map_err(|error| error.to_string())?
    {
        // Materializing may publish a package generation this graph predates.
        graph = harn_modules::build_with_source(path, &source);
    }
    let authority = SourceCompilerAuthority::for_source(path);
    let (parsed_source, program) = parse_source_file(&path.to_string_lossy());
    if parsed_source != source {
        return Err("source changed during preparation".into());
    }
    let checker = checker_with_resolved_graph(authority.typechecker(), path, &graph);
    let errors = checker
        .check_with_source(&program, &source)
        .into_iter()
        .filter(|diag| matches!(diag.severity, DiagnosticSeverity::Error))
        .map(|diag| {
            harn_parser::diagnostic::render_type_diagnostic(&source, &path.to_string_lossy(), &diag)
        })
        .collect::<String>();
    if !errors.is_empty() {
        return Err(errors);
    }
    let (context, mut store) = compiler
        .prepare(path, &source, authority.module_provenance())
        .map_err(|error| error.to_string())?;
    let chunk = authority
        .compiler_with_imported_symbols(&context)
        .compile(&program)
        .map_err(|error| error.to_string())?;
    let manifest = store.manifest.as_mut().ok_or("incomplete entry graph")?;
    manifest.package_import_aliases =
        crate::package::declared_package_import_aliases(path, manifest.package_import_aliases())
            .map_err(|error| error.to_string())?;
    manifest.package_lock_digest =
        crate::package::reachable_dependency_lock_digest(path, manifest.package_import_aliases())
            .map_err(|error| error.to_string())?;
    store.store(&chunk).map_err(|error| error.to_string())
}
