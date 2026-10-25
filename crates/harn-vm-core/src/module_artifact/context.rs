use std::path::Path;

use harn_modules::DefKind;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{needs_imported_symbol_projection, parse_module_source};
use crate::VmError;

/// The complete imported interface that can affect one module's bytecode.
///
/// Dependency bodies remain independently cached. Only the canonical names and
/// kinds consulted during lowering belong here, so every in-memory, on-disk,
/// precompiled, and warm-link identity can share this one typed owner.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModuleCompilationContext {
    enum_candidates: Vec<String>,
    source_callable_names: Vec<String>,
    type_alias_names: Vec<String>,
}

/// Syntax-dependent work captured before the graph finishes resolving imports.
pub(crate) struct ModuleCompilationContextProjection {
    needs_imported_symbols: bool,
}

impl ModuleCompilationContextProjection {
    pub(crate) fn in_graph(
        self,
        graph: &harn_modules::ModuleGraph,
        source_path: &Path,
    ) -> ModuleCompilationContext {
        if self.needs_imported_symbols {
            ModuleCompilationContext::from_graph(graph, source_path)
        } else {
            ModuleCompilationContext::default()
        }
    }
}

impl ModuleCompilationContext {
    pub fn needs_imported_symbols(program: &[harn_parser::SNode]) -> bool {
        needs_imported_symbol_projection(program)
    }

    pub fn new(
        enum_candidates: impl IntoIterator<Item = String>,
        source_callable_names: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut enum_candidates = enum_candidates.into_iter().collect::<Vec<_>>();
        enum_candidates.sort_unstable();
        enum_candidates.dedup();
        let mut source_callable_names = source_callable_names.into_iter().collect::<Vec<_>>();
        source_callable_names.sort_unstable();
        source_callable_names.dedup();
        Self {
            enum_candidates,
            source_callable_names,
            type_alias_names: Vec::new(),
        }
    }

    pub fn from_graph(graph: &harn_modules::ModuleGraph, source_path: &Path) -> Self {
        let mut context = Self::new(
            graph
                .imported_names_by_kind_for_file(source_path, DefKind::Enum)
                .unwrap_or_default(),
            graph
                .imported_callable_names_for_file(source_path)
                .unwrap_or_default(),
        );
        context.type_alias_names = graph
            .imported_names_by_kind_for_file(source_path, DefKind::Type)
            .unwrap_or_default()
            .into_iter()
            .collect();
        context.type_alias_names.sort_unstable();
        context.type_alias_names.dedup();
        context
    }

    /// Project only names that this source's syntax can consult during
    /// lowering. This keeps eager graph prewarming and lazy compilation on the
    /// same canonical identity without forcing graph work for insensitive
    /// modules.
    pub fn for_source_in_graph(
        graph: &harn_modules::ModuleGraph,
        source_path: &Path,
        source: &str,
    ) -> Result<Self, VmError> {
        let program = parse_module_source(source_path, source)?;
        Ok(Self::for_program_in_graph(graph, source_path, &program))
    }

    /// [`Self::for_source_in_graph`] over a remembered source, or `None` when it
    /// does not parse.
    pub(crate) fn for_module_source_in_graph(
        graph: &harn_modules::ModuleGraph,
        source_path: &Path,
        source: &crate::module_source::ModuleSource,
    ) -> Option<Self> {
        let needs_imported_symbols = source.needs_imported_symbols(source_path)?;
        Some(
            ModuleCompilationContextProjection {
                needs_imported_symbols,
            }
            .in_graph(graph, source_path),
        )
    }

    pub(crate) fn source_needs_imported_symbols(source_path: &Path, source: &str) -> Option<bool> {
        let program = parse_module_source(source_path, source).ok()?;
        Some(needs_imported_symbol_projection(&program))
    }

    /// Project the graph's parsed program without reading or parsing it again.
    /// The program and graph must come from the same source snapshot.
    pub fn for_program_in_graph(
        graph: &harn_modules::ModuleGraph,
        source_path: &Path,
        program: &[harn_parser::SNode],
    ) -> Self {
        Self::project_program(program).in_graph(graph, source_path)
    }

    pub(crate) fn project_program(
        program: &[harn_parser::SNode],
    ) -> ModuleCompilationContextProjection {
        ModuleCompilationContextProjection {
            needs_imported_symbols: needs_imported_symbol_projection(program),
        }
    }

    pub fn enum_candidates(&self) -> &[String] {
        &self.enum_candidates
    }

    pub fn source_callable_names(&self) -> &[String] {
        &self.source_callable_names
    }

    /// Configure one compiler from the complete resolved import interface.
    /// Every source projection uses this boundary instead of selecting a
    /// subset of names and silently dropping another lowering dependency.
    pub fn configure_compiler(&self, compiler: crate::Compiler) -> crate::Compiler {
        compiler
            .with_imported_enum_candidates(self.enum_candidates.iter().cloned())
            .with_imported_source_callable_names(self.source_callable_names.iter().cloned())
            .with_imported_type_alias_names(self.type_alias_names.iter().cloned())
    }

    pub fn type_alias_names(&self) -> &[String] {
        &self.type_alias_names
    }

    pub(super) fn with_enum_candidates(mut self, names: impl IntoIterator<Item = String>) -> Self {
        self.enum_candidates = names.into_iter().collect();
        self.enum_candidates.sort_unstable();
        self.enum_candidates.dedup();
        self
    }

    pub(super) fn with_source_callable_names(
        mut self,
        names: impl IntoIterator<Item = String>,
    ) -> Self {
        self.source_callable_names = names.into_iter().collect();
        self.source_callable_names.sort_unstable();
        self.source_callable_names.dedup();
        self
    }

    /// Stable, framed identity for cache keys. The domain tag makes future
    /// context families non-aliasing without coupling callers to this layout.
    pub fn digest(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"harn-module-compilation-context-v2\0");
        hash_names(&mut hasher, b"enum\0", &self.enum_candidates);
        hash_names(
            &mut hasher,
            b"source-callable\0",
            &self.source_callable_names,
        );
        hash_names(&mut hasher, b"type-alias\0", &self.type_alias_names);
        hasher.finalize().into()
    }
}

fn hash_names(hasher: &mut Sha256, label: &[u8], names: &[String]) {
    hasher.update(label);
    hasher.update((names.len() as u64).to_le_bytes());
    for name in names {
        hasher.update((name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsed_projection_preserves_imported_lowering_identity() {
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("library.harn");
        let root = dir.path().join("main.harn");
        std::fs::write(
            &library,
            "pub enum Color { Ready(message: string) Empty }\npub type Box<T> = {payload: T}\npub fn context() { return 42 }\n",
        )
        .unwrap();
        std::fs::write(
            &root,
            r#"import "./library"
pub fn describe(value: any) -> string {
  const result = context()
  match value {
    Color.Ready(message) -> { return message }
    _ -> { return "other" }
  }
}
"#,
        )
        .unwrap();
        let build = harn_modules::build_closed_program(std::slice::from_ref(&root));
        let root = harn_modules::canonical_path(&root);
        let parsed = &build.parsed_sources[&root];
        let parses = || super::super::MODULE_PARSES.with(std::cell::Cell::get);
        let before = parses();
        let from_program =
            ModuleCompilationContext::for_program_in_graph(&build.graph, &root, &parsed.program);
        assert_eq!(
            parses(),
            before,
            "projection must reuse the graph's syntax tree"
        );
        assert_eq!(from_program.enum_candidates(), ["Color"]);
        assert_eq!(from_program.source_callable_names(), ["context"]);
        assert_eq!(from_program.type_alias_names(), ["Box"]);
        let without_types = ModuleCompilationContext::new(
            from_program.enum_candidates().to_vec(),
            from_program.source_callable_names().to_vec(),
        );
        assert_ne!(
            from_program.digest(),
            without_types.digest(),
            "an imported type changes the compilation cache identity"
        );
        let from_source =
            ModuleCompilationContext::for_source_in_graph(&build.graph, &root, &parsed.source)
                .unwrap();
        assert_eq!(
            parses(),
            before + 1,
            "the source adapter must exercise the parse counter"
        );
        assert_eq!(from_program, from_source);

        let library = harn_modules::canonical_path(&library);
        let insensitive = ModuleCompilationContext::for_program_in_graph(
            &build.graph,
            &library,
            &build.parsed_sources[&library].program,
        );
        assert_eq!(insensitive, ModuleCompilationContext::default());
    }
}
