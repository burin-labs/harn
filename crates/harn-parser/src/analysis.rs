use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::Path;

use harn_lexer::{Lexer, LexerError, Token};

use crate::InlayHintInfo;
use crate::{Parser, ParserError, SNode, TypeChecker, TypeDiagnostic};

/// Stable source identity used by the incremental analysis cache.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceId(String);

impl SourceId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn path(path: &Path) -> Self {
        Self(path.to_string_lossy().into_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Monotonic caller-owned version for a source input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceVersion(pub u64);

/// Deterministic content digest for a source input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceDigest(u64);

impl SourceDigest {
    pub fn from_source(source: &str) -> Self {
        let mut hash = 0xcbf29ce484222325u64;
        for byte in source.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        Self(hash)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceUpdate {
    Inserted,
    Changed,
    Unchanged,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnalysisStats {
    pub lex_runs: usize,
    pub parse_runs: usize,
    pub typecheck_runs: usize,
}

#[derive(Debug, Clone)]
pub struct ParseOutput {
    pub source: String,
    pub program: Vec<SNode>,
}

#[derive(Debug, Clone)]
pub struct TypeCheckOutput {
    pub source: String,
    pub program: Vec<SNode>,
    pub diagnostics: Vec<TypeDiagnostic>,
    pub inlay_hints: Vec<InlayHintInfo>,
}

#[derive(Debug, Clone)]
pub enum AnalysisError {
    MissingSource(SourceId),
    Lex {
        source: String,
        error: LexerError,
    },
    Parse {
        source: String,
        errors: Vec<ParserError>,
    },
}

impl AnalysisError {
    pub fn source(&self) -> Option<&str> {
        match self {
            AnalysisError::MissingSource(_) => None,
            AnalysisError::Lex { source, .. } | AnalysisError::Parse { source, .. } => Some(source),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct TypeCheckConfig {
    pub strict_types: bool,
    pub imported_names: Option<HashSet<String>>,
    pub imported_type_decls: Vec<SNode>,
    pub imported_callable_decls: Vec<SNode>,
    pub namespace_imports: Vec<(String, crate::NamespaceImportBinding)>,
}

impl TypeCheckConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_strict_types(mut self, strict_types: bool) -> Self {
        self.strict_types = strict_types;
        self
    }

    pub fn with_imported_names(mut self, imported_names: Option<HashSet<String>>) -> Self {
        self.imported_names = imported_names;
        self
    }

    pub fn with_imported_type_decls(mut self, imported_type_decls: Vec<SNode>) -> Self {
        self.imported_type_decls = imported_type_decls;
        self
    }

    pub fn with_imported_callable_decls(mut self, imported_callable_decls: Vec<SNode>) -> Self {
        self.imported_callable_decls = imported_callable_decls;
        self
    }

    pub fn with_namespace_imports(
        mut self,
        namespace_imports: Vec<(String, crate::NamespaceImportBinding)>,
    ) -> Self {
        self.namespace_imports = namespace_imports;
        self
    }

    fn cache_key(&self) -> TypeCheckCacheKey {
        let mut imported_names = self
            .imported_names
            .as_ref()
            .map(|names| names.iter().cloned().collect::<Vec<_>>());
        if let Some(names) = &mut imported_names {
            names.sort();
        }
        let mut namespace_aliases: Vec<String> = self
            .namespace_imports
            .iter()
            .map(|(alias, _)| alias.clone())
            .collect();
        namespace_aliases.sort();
        TypeCheckCacheKey {
            strict_types: self.strict_types,
            imported_names,
            imported_type_decls_digest: debug_digest(&self.imported_type_decls),
            imported_callable_decls_digest: debug_digest(&self.imported_callable_decls),
            namespace_imports_digest: debug_digest(&namespace_aliases),
        }
    }

    fn build_checker(&self) -> TypeChecker {
        let mut checker = TypeChecker::with_strict_types(self.strict_types);
        if let Some(imported) = self.imported_names.clone() {
            checker = checker.with_imported_names(imported);
        }
        if !self.imported_type_decls.is_empty() {
            checker = checker.with_imported_type_decls(self.imported_type_decls.clone());
        }
        if !self.imported_callable_decls.is_empty() {
            checker = checker.with_imported_callable_decls(self.imported_callable_decls.clone());
        }
        if !self.namespace_imports.is_empty() {
            checker = checker.with_namespace_imports(self.namespace_imports.clone());
        }
        checker
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TypeCheckCacheKey {
    strict_types: bool,
    imported_names: Option<Vec<String>>,
    imported_type_decls_digest: u64,
    imported_callable_decls_digest: u64,
    namespace_imports_digest: u64,
}

#[derive(Debug, Clone)]
struct CachedTypeCheck {
    diagnostics: Vec<TypeDiagnostic>,
    inlay_hints: Vec<InlayHintInfo>,
}

#[derive(Debug, Clone)]
struct SourceEntry {
    source: String,
    version: SourceVersion,
    digest: SourceDigest,
    tokens: Option<Result<Vec<Token>, LexerError>>,
    program: Option<Result<Vec<SNode>, Vec<ParserError>>>,
    typechecks: HashMap<TypeCheckCacheKey, CachedTypeCheck>,
}

impl SourceEntry {
    fn new(source: String, version: SourceVersion, digest: SourceDigest) -> Self {
        Self {
            source,
            version,
            digest,
            tokens: None,
            program: None,
            typechecks: HashMap::new(),
        }
    }

    fn replace_source(&mut self, source: String, version: SourceVersion, digest: SourceDigest) {
        self.source = source;
        self.version = version;
        self.digest = digest;
        self.tokens = None;
        self.program = None;
        self.typechecks.clear();
    }
}

/// Persistent query cache for lexing, parsing, and type checking Harn sources.
#[derive(Debug, Default)]
pub struct AnalysisDatabase {
    entries: HashMap<SourceId, SourceEntry>,
    stats: AnalysisStats,
}

impl AnalysisDatabase {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn stats(&self) -> AnalysisStats {
        self.stats.clone()
    }

    pub fn set_source(
        &mut self,
        id: SourceId,
        source: String,
        version: SourceVersion,
    ) -> SourceUpdate {
        let digest = SourceDigest::from_source(&source);
        match self.entries.get_mut(&id) {
            None => {
                self.entries
                    .insert(id, SourceEntry::new(source, version, digest));
                SourceUpdate::Inserted
            }
            Some(entry) if entry.digest == digest => {
                entry.version = version;
                SourceUpdate::Unchanged
            }
            Some(entry) => {
                entry.replace_source(source, version, digest);
                SourceUpdate::Changed
            }
        }
    }

    pub fn set_parsed_source(
        &mut self,
        id: SourceId,
        source: String,
        version: SourceVersion,
        program: Vec<SNode>,
    ) -> SourceUpdate {
        let digest = SourceDigest::from_source(&source);
        match self.entries.get_mut(&id) {
            None => {
                let mut entry = SourceEntry::new(source, version, digest);
                entry.program = Some(Ok(program));
                self.entries.insert(id, entry);
                SourceUpdate::Inserted
            }
            Some(entry) if entry.digest == digest => {
                entry.version = version;
                entry.program = Some(Ok(program));
                SourceUpdate::Unchanged
            }
            Some(entry) => {
                entry.replace_source(source, version, digest);
                entry.program = Some(Ok(program));
                SourceUpdate::Changed
            }
        }
    }

    pub fn parse(&mut self, id: &SourceId) -> Result<ParseOutput, AnalysisError> {
        if let Some(entry) = self.entries.get(id) {
            if let Some(program) = &entry.program {
                return match program {
                    Ok(program) => Ok(ParseOutput {
                        source: entry.source.clone(),
                        program: program.clone(),
                    }),
                    Err(errors) => Err(AnalysisError::Parse {
                        source: entry.source.clone(),
                        errors: errors.clone(),
                    }),
                };
            }
        }

        let mut lexed = false;
        let mut parsed_now = false;
        let entry = self.entry_mut(id)?;
        if entry.tokens.is_none() {
            lexed = true;
            let mut lexer = Lexer::new(&entry.source);
            entry.tokens = Some(lexer.tokenize());
        }
        let tokens = match entry.tokens.as_ref().expect("tokens initialized") {
            Ok(tokens) => tokens.clone(),
            Err(error) => {
                let source = entry.source.clone();
                let error = error.clone();
                if lexed {
                    self.stats.lex_runs += 1;
                }
                return Err(AnalysisError::Lex { source, error });
            }
        };

        if entry.program.is_none() {
            parsed_now = true;
            let mut parser = Parser::new(tokens);
            entry.program = Some(match parser.parse() {
                Ok(program) => Ok(program),
                Err(error) => {
                    let mut errors = parser.all_errors().to_vec();
                    if errors.is_empty() {
                        errors.push(error);
                    }
                    Err(errors)
                }
            });
        }

        let result = match entry.program.as_ref().expect("program initialized") {
            Ok(program) => Ok(ParseOutput {
                source: entry.source.clone(),
                program: program.clone(),
            }),
            Err(errors) => Err(AnalysisError::Parse {
                source: entry.source.clone(),
                errors: errors.clone(),
            }),
        };
        if lexed {
            self.stats.lex_runs += 1;
        }
        if parsed_now {
            self.stats.parse_runs += 1;
        }
        result
    }

    pub fn typecheck(
        &mut self,
        id: &SourceId,
        config: TypeCheckConfig,
    ) -> Result<TypeCheckOutput, AnalysisError> {
        let parsed = self.parse(id)?;
        let key = config.cache_key();
        if let Some(cached) = self
            .entries
            .get(id)
            .expect("parse verified source entry")
            .typechecks
            .get(&key)
        {
            return Ok(TypeCheckOutput {
                source: parsed.source,
                program: parsed.program,
                diagnostics: cached.diagnostics.clone(),
                inlay_hints: cached.inlay_hints.clone(),
            });
        }

        self.stats.typecheck_runs += 1;
        let (diagnostics, inlay_hints) = config
            .build_checker()
            .check_with_hints(&parsed.program, &parsed.source);
        let cached = CachedTypeCheck {
            diagnostics: diagnostics.clone(),
            inlay_hints: inlay_hints.clone(),
        };
        self.entries
            .get_mut(id)
            .expect("parse verified source entry")
            .typechecks
            .insert(key, cached);
        Ok(TypeCheckOutput {
            source: parsed.source,
            program: parsed.program,
            diagnostics,
            inlay_hints,
        })
    }

    fn entry_mut(&mut self, id: &SourceId) -> Result<&mut SourceEntry, AnalysisError> {
        self.entries
            .get_mut(id)
            .ok_or_else(|| AnalysisError::MissingSource(id.clone()))
    }
}

fn debug_digest<T: std::fmt::Debug>(value: &T) -> u64 {
    let mut hasher = StableHasher::default();
    format!("{value:?}").hash(&mut hasher);
    hasher.finish()
}

#[derive(Default)]
struct StableHasher(u64);

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = if self.0 == 0 {
            0xcbf29ce484222325u64
        } else {
            self.0
        };
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
        self.0 = hash;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DiagnosticSeverity;

    fn source_id() -> SourceId {
        SourceId::new("test.harn")
    }

    #[test]
    fn parse_reuses_cached_program_for_unchanged_source() {
        let mut db = AnalysisDatabase::new();
        let id = source_id();
        assert_eq!(
            db.set_source(id.clone(), "const x = 1\n".to_string(), SourceVersion(1)),
            SourceUpdate::Inserted
        );
        db.parse(&id).expect("initial parse");
        db.parse(&id).expect("cached parse");
        assert_eq!(db.stats().lex_runs, 1);
        assert_eq!(db.stats().parse_runs, 1);

        assert_eq!(
            db.set_source(id.clone(), "const x = 1\n".to_string(), SourceVersion(2)),
            SourceUpdate::Unchanged
        );
        db.parse(&id).expect("same digest parse");
        assert_eq!(db.stats().lex_runs, 1);
        assert_eq!(db.stats().parse_runs, 1);
    }

    #[test]
    fn source_change_invalidates_parse_and_typecheck_outputs() {
        let mut db = AnalysisDatabase::new();
        let id = source_id();
        db.set_source(id.clone(), "const x = 1\n".to_string(), SourceVersion(1));
        db.typecheck(&id, TypeCheckConfig::new())
            .expect("initial check");
        assert_eq!(
            db.set_source(id.clone(), "const x = 2\n".to_string(), SourceVersion(2)),
            SourceUpdate::Changed
        );
        db.typecheck(&id, TypeCheckConfig::new())
            .expect("changed check");
        assert_eq!(db.stats().lex_runs, 2);
        assert_eq!(db.stats().parse_runs, 2);
        assert_eq!(db.stats().typecheck_runs, 2);
    }

    #[test]
    fn parsed_source_seed_skips_lex_and_parse() {
        let mut db = AnalysisDatabase::new();
        let id = source_id();
        let source = "const x = 1\n".to_string();
        let mut lexer = Lexer::new(&source);
        let tokens = lexer.tokenize().expect("tokenize");
        let mut parser = Parser::new(tokens);
        let program = parser.parse().expect("parse");

        assert_eq!(
            db.set_parsed_source(id.clone(), source, SourceVersion(1), program),
            SourceUpdate::Inserted
        );
        db.typecheck(&id, TypeCheckConfig::new())
            .expect("seeded check");
        assert_eq!(db.stats().lex_runs, 0);
        assert_eq!(db.stats().parse_runs, 0);
        assert_eq!(db.stats().typecheck_runs, 1);
    }

    #[test]
    fn typecheck_cache_is_keyed_by_options() {
        let mut db = AnalysisDatabase::new();
        let id = source_id();
        db.set_source(
            id.clone(),
            "pipeline main() {\n  const x = read_file(\"a\")\n  log(x.foo)\n}\n".to_string(),
            SourceVersion(1),
        );
        db.typecheck(&id, TypeCheckConfig::new())
            .expect("default check");
        db.typecheck(&id, TypeCheckConfig::new())
            .expect("cached default check");
        db.typecheck(&id, TypeCheckConfig::new().with_strict_types(true))
            .expect("strict check");
        assert_eq!(db.stats().typecheck_runs, 2);
    }

    #[test]
    fn typecheck_diagnostics_are_cached_with_hints() {
        let mut db = AnalysisDatabase::new();
        let id = source_id();
        db.set_source(
            id.clone(),
            "pipeline main() {\n  const x: int = \"nope\"\n}\n".to_string(),
            SourceVersion(1),
        );
        let first = db.typecheck(&id, TypeCheckConfig::new()).expect("check");
        let second = db.typecheck(&id, TypeCheckConfig::new()).expect("cached");
        assert!(first
            .diagnostics
            .iter()
            .any(|diag| diag.severity == DiagnosticSeverity::Error));
        assert_eq!(first.diagnostics.len(), second.diagnostics.len());
        assert_eq!(db.stats().typecheck_runs, 1);
    }
}
