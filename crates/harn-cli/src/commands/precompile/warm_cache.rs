//! Prepare machine-local cache identities without running user code.

use std::collections::BTreeSet;
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
    let mut modules = BTreeSet::new();
    let mut prepared = 0;
    let mut failed = 0;
    for source in &sources {
        match prepare_entry(source, &mut modules) {
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
        modules.len()
    );
    if failed > 0 {
        std::process::exit(1);
    }
}

fn prepare_entry(
    path: &Path,
    prepared: &mut BTreeSet<(
        std::path::PathBuf,
        harn_vm::module_artifact::ModuleProvenance,
    )>,
) -> Result<(), String> {
    let source = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let graph = harn_modules::build_with_source(path, &source);
    crate::package::ensure_reachable_dependencies_materialized(path, &graph)
        .map_err(|error| error.to_string())?;
    // Materialization can replace the generation that resolved the graph.
    let graph = harn_modules::build_with_source(path, &source);
    let authority = SourceCompilerAuthority::for_source(path);
    let (parsed_source, program) = parse_source_file(&path.to_string_lossy());
    if parsed_source != source {
        return Err("source changed during preparation".into());
    }
    let checker = checker_with_resolved_imports(authority.typechecker(), path);
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
    let context = ModuleCompilationContext::for_source_in_graph(&graph, path, &source)
        .map_err(|error| error.to_string())?;
    let chunk = authority
        .compiler_with_imported_symbols(&context)
        .compile(&program)
        .map_err(|error| error.to_string())?;
    for module in graph.module_paths() {
        let identity = (
            harn_modules::canonical_path(&module),
            authority.module_provenance(),
        );
        if prepared.contains(&identity) {
            continue;
        }
        let source = if module
            .to_str()
            .is_some_and(|path| path.starts_with("<std>/"))
        {
            String::new() // The runtime owns embedded source lookup and its key.
        } else {
            std::fs::read_to_string(&module).map_err(|error| error.to_string())?
        };
        bytecode_cache::precompile_module_to_cache(&module, &source, authority.module_provenance())
            .map_err(|error| error.to_string())?;
        prepared.insert(identity);
    }
    let mut store = bytecode_cache::prepare_entry_store(path, &source);
    let manifest = store.manifest.as_mut().ok_or("incomplete entry graph")?;
    manifest.package_import_aliases =
        crate::package::declared_package_import_aliases(path, manifest.package_import_aliases())
            .map_err(|error| error.to_string())?;
    manifest.package_lock_digest =
        crate::package::reachable_dependency_lock_digest(path, manifest.package_import_aliases())
            .map_err(|error| error.to_string())?;
    store.store(&chunk).map_err(|error| error.to_string())
}
