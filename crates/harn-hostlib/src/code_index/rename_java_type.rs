//! Prepared Java type identity and current workspace binding proof.

use std::path::Path;

use crate::host_call::{HostCallContext, Phase};
use tree_sitter::{Node as SyntaxNode, Tree};

use super::super::java_bindings::{JavaBindings, JavaTypeCatalog};
use super::super::refactor_core::{collect_identifier_spans, IdentifierSpan, ShadowSite};
use super::super::source_positions::{Encoding, LineIndex};
use super::super::state::IndexState;
use super::super::symbol_graph::{Node, NodeKind};
use super::RenameSites;

const MAX_ENTRIES: usize = 50_000;
const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;

pub(in crate::code_index) struct JavaTypeSeed {
    pub path: String,
    pub qualified_name: String,
    pub simple_name: String,
    pub declaration: IdentifierSpan,
    pub public_top_level: bool,
}

pub(in crate::code_index) struct JavaTypeRename {
    pub seed: JavaTypeSeed,
    sources: Vec<(String, String)>,
    catalog: JavaTypeCatalog,
    call: Option<HostCallContext>,
}

fn read_sources(
    root: &Path,
    session_id: Option<&str>,
    call: Option<&HostCallContext>,
) -> Result<Vec<(String, String)>, String> {
    super::super::rename_workspace::read_sources(
        root,
        session_id,
        "java",
        "Java",
        MAX_ENTRIES,
        MAX_SOURCE_BYTES,
        call,
    )
}

pub(in crate::code_index) fn prepare_type(
    state: &IndexState,
    seed: &Node,
    session_id: Option<&str>,
    call: Option<&HostCallContext>,
) -> Result<Option<JavaTypeRename>, String> {
    poll(call)?;
    if seed.language != "java" || !matches!(seed.kind, NodeKind::Type | NodeKind::Function) {
        return Ok(None);
    }
    let source = super::super::refactor_core::read_source(
        super::super::rename::BUILTIN,
        &state.root,
        &seed.path,
        session_id,
    )
    .map_err(|err| err.to_string())?;
    let tree = crate::ast::api::parse_tree(&source, crate::ast::Language::Java)
        .map_err(|err| err.to_string())?;
    let positions = LineIndex::new(&source);
    let declaration = descendants(tree.root_node()).find(|node| {
        let wanted = if seed.kind == NodeKind::Type {
            is_type(*node)
        } else {
            matches!(
                node.kind(),
                "constructor_declaration" | "compact_constructor_declaration"
            )
        };
        wanted
            && node.child_by_field_name("name").is_some_and(|name| {
                source.get(name.byte_range()) == Some(seed.name.as_str())
                    && (node.start_position().row as u32 + 1 == seed.line
                        || name.start_position().row as u32 + 1 == seed.line)
                    && seed.col
                        == Some(
                            positions
                                .position(name.start_byte(), Encoding::Utf16)
                                .character
                                + 1,
                        )
            })
    });
    let Some(mut declaration) = declaration else {
        return if seed.kind == NodeKind::Type {
            Err("the indexed Java type declaration changed; rebuild the index".into())
        } else {
            Ok(None)
        };
    };
    if !is_type(declaration) {
        declaration = std::iter::successors(declaration.parent(), |node| node.parent())
            .find(|node| is_type(*node))
            .ok_or("the Java constructor has no enclosing type")?;
    }
    let bindings = JavaBindings::new(&tree, &source);
    if bindings.is_unresolved() {
        return Err("Java source contains unsupported or invalid binding syntax".into());
    }
    let qualified_name = bindings
        .type_identity(declaration)
        .ok_or("the Java type has no declared identity")?;
    if qualified_name.starts_with('#') {
        return Err("local and anonymous Java type renames require compiler resolution".into());
    }
    let name = declaration
        .child_by_field_name("name")
        .ok_or("the Java type has no declaration name")?;
    let simple_name = source
        .get(name.byte_range())
        .ok_or("the Java type name is outside its source")?
        .to_string();
    let public_top_level = declaration
        .parent()
        .is_some_and(|parent| parent.kind() == "program")
        && has_public_modifier(declaration);
    let seed = JavaTypeSeed {
        path: seed.path.to_string(),
        qualified_name,
        simple_name,
        declaration: IdentifierSpan::of(name),
        public_top_level,
    };
    let sources = read_sources(&state.root, session_id, call)?;
    if !sources
        .iter()
        .any(|(path, expected)| path == &seed.path && expected == &source)
    {
        return Err("the Java seed changed while collecting the workspace binding snapshot".into());
    }
    let mut catalog = JavaTypeCatalog::default();
    for (_, source) in &sources {
        poll(call)?;
        let tree = crate::ast::api::parse_tree(source, crate::ast::Language::Java)
            .map_err(|err| err.to_string())?;
        let bindings = JavaBindings::new(&tree, source);
        if tree.root_node().has_error() || source.contains("\\u") {
            return Err("Java workspace contains unsupported or invalid binding syntax".into());
        }
        catalog.add(&bindings, tree.root_node(), call)?;
    }
    if !catalog.unique(&seed.qualified_name) {
        return Err(
            "the selected Java type identity is absent or duplicated in the current workspace"
                .into(),
        );
    }
    Ok(Some(JavaTypeRename {
        seed,
        sources,
        catalog,
        call: call.cloned(),
    }))
}

impl JavaTypeRename {
    pub fn unchanged(&self, root: &Path, session_id: Option<&str>) -> bool {
        read_sources(root, session_id, self.call.as_ref())
            .is_ok_and(|sources| sources == self.sources)
    }

    pub fn members(&self) -> impl Iterator<Item = &String> {
        self.sources.iter().map(|(path, _)| path)
    }

    pub fn cover_scope(&self, in_scope: &[String]) -> Result<(), String> {
        for (path, source) in &self.sources {
            poll(self.call.as_ref())?;
            if in_scope.contains(path) {
                continue;
            }
            let tree = crate::ast::api::parse_tree(source, crate::ast::Language::Java)
                .map_err(|err| err.to_string())?;
            let (mut targets, mut shadows) = (Vec::new(), Vec::new());
            collect_identifier_spans(
                tree.root_node(),
                source.as_bytes(),
                &self.seed.simple_name,
                "",
                &["identifier", "type_identifier"],
                path,
                &mut targets,
                &mut shadows,
            );
            if !self
                .sites(
                    path,
                    source,
                    &tree,
                    &self.seed.simple_name,
                    targets,
                    shadows,
                )?
                .targets
                .is_empty()
            {
                return Err(format!(
                    "Java source `{path}` outside the requested scope refers to the selected type"
                ));
            }
        }
        Ok(())
    }

    pub fn sites(
        &self,
        path: &str,
        source: &str,
        tree: &Tree,
        new_name: &str,
        targets: Vec<IdentifierSpan>,
        shadows: Vec<ShadowSite>,
    ) -> Result<RenameSites, String> {
        poll(self.call.as_ref())?;
        if !crate::ast::java::is_type_name(new_name) {
            return Err("the proposed Java name is not a valid type identifier".into());
        }
        if !self
            .sources
            .iter()
            .any(|(expected_path, expected)| expected_path == path && expected == source)
        {
            return Err("Java source changed after the type binding snapshot".into());
        }
        let bindings = JavaBindings::new(tree, source);
        let mut selected = Vec::new();
        let mut conflicts = Vec::new();
        let new_identity = self.seed.qualified_name.rsplit_once('.').map_or_else(
            || new_name.to_string(),
            |(owner, _)| format!("{owner}.{new_name}"),
        );
        for span in targets {
            poll(self.call.as_ref())?;
            let node = occurrence(tree, span)?;
            if bindings.type_occurrence(node, &self.catalog)?.as_deref()
                == Some(&self.seed.qualified_name)
            {
                // An existing lexical/import binding would capture a renamed
                // simple reference even if the conflicting name has no uses.
                if new_name != self.seed.simple_name
                    && bindings.type_name_capture(node, new_name, &self.catalog)?
                {
                    conflicts.push(ShadowSite {
                        path: path.to_string(),
                        row: span.start_row,
                        col: span.start_col,
                    });
                }
                selected.push(span);
            }
        }
        for shadow in shadows {
            poll(self.call.as_ref())?;
            let point = tree_sitter::Point::new(shadow.row, shadow.col);
            let node = tree
                .root_node()
                .named_descendant_for_point_range(point, point)
                .ok_or("the Java shadow occurrence is absent")?;
            if node.start_position() != point {
                return Err("the Java shadow position changed".into());
            }
            if new_identity != self.seed.qualified_name
                && bindings.type_shadow_conflicts(
                    node,
                    &self.seed.qualified_name,
                    &new_identity,
                    &self.catalog,
                )?
            {
                conflicts.push(shadow);
            }
        }
        Ok(RenameSites {
            targets: selected,
            shadows: conflicts,
        })
    }
}

fn poll(call: Option<&HostCallContext>) -> Result<(), String> {
    if let Some(call) = call {
        if call.interrupted().is_some() {
            return Err("Java type binding proof was interrupted".into());
        }
        call.heartbeat(Phase::Resolving, "checking Java type bindings");
    }
    Ok(())
}

fn occurrence(tree: &Tree, span: IdentifierSpan) -> Result<SyntaxNode<'_>, String> {
    tree.root_node()
        .named_descendant_for_byte_range(span.start_byte, span.end_byte)
        .filter(|node| node.byte_range() == (span.start_byte..span.end_byte))
        .ok_or("the Java occurrence no longer matches its identifier span".into())
}

fn is_type(node: SyntaxNode<'_>) -> bool {
    super::super::java_bindings::is_type(node)
}
fn has_public_modifier(node: SyntaxNode<'_>) -> bool {
    super::super::java_bindings::has_modifier(node, "public")
}
fn descendants(node: SyntaxNode<'_>) -> impl Iterator<Item = SyntaxNode<'_>> {
    super::super::java_bindings::descendants(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const SOURCE: &str = "package lab;\npublic class DocumentStore {\n public DocumentStore() {}\n void DocumentStore() {}\n}\n";

    #[test]
    fn caller_interruption_refuses_census_catalog_and_retained_proof_work() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("DocumentStore.java"), SOURCE).unwrap();
        let state = IndexState::build_from_root(dir.path()).0;
        let node = state
            .symbols
            .iter_nodes()
            .find(|node| node.name == "DocumentStore" && node.kind == NodeKind::Type)
            .unwrap();
        let expired = HostCallContext::new(None).with_budget(std::time::Duration::ZERO);
        assert!(prepare_type(&state, node, None, Some(&expired))
            .err()
            .unwrap()
            .contains("interrupted"));
        let mut proof = prepare_type(&state, node, None, None).unwrap().unwrap();
        assert_eq!(
            proof.members().count(),
            1,
            "the positive must inspect a real source"
        );
        let tree = crate::ast::api::parse_tree(SOURCE, crate::ast::Language::Java).unwrap();
        let bindings = JavaBindings::new(&tree, SOURCE);
        let mut catalog = JavaTypeCatalog::default();
        assert!(catalog
            .add(&bindings, tree.root_node(), Some(&expired))
            .is_err());
        assert!(!catalog.contains("lab.DocumentStore"));
        proof.call = Some(expired);
        assert!(!proof.unchanged(dir.path(), None));
        assert!(proof.cover_scope(&[]).is_err());
        assert!(proof
            .sites(
                "DocumentStore.java",
                SOURCE,
                &tree,
                "Library",
                Vec::new(),
                Vec::new()
            )
            .is_err());
    }

    #[test]
    fn prepared_type_and_constructor_select_the_same_declaration_but_not_the_method() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("DocumentStore.java"), SOURCE).unwrap();
        let state = IndexState::build_from_root(dir.path()).0;
        let mut prepared = Vec::new();
        for line in [2, 3, 4] {
            let node = state
                .symbols
                .iter_nodes()
                .find(|node| node.name == "DocumentStore" && node.line == line)
                .unwrap();
            let result = prepare_type(&state, node, None, None).unwrap();
            if line == 4 {
                assert!(
                    result.is_none(),
                    "the same-named method is not a constructor"
                );
            } else {
                let proof = result.expect("the actual type/constructor must be selected");
                assert_eq!(proof.seed.qualified_name, "lab.DocumentStore");
                assert!(proof.seed.public_top_level);
                assert_eq!(proof.seed.declaration.start_row, 1);
                prepared.push(proof.seed.declaration);
            }
        }
        assert_eq!(prepared.len(), 2);
        assert_eq!(prepared[0], prepared[1]);
    }

    #[test]
    fn workspace_proof_covers_unindexed_java_and_detects_new_deleted_and_changed_sources() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("DocumentStore.java"), SOURCE).unwrap();
        fs::create_dir(dir.path().join("build")).unwrap();
        let hidden = dir.path().join("build/Hidden.java");
        fs::write(
            &hidden,
            "package app; import lab.DocumentStore; class Hidden { DocumentStore value; }",
        )
        .unwrap();
        let state = IndexState::build_from_root(dir.path()).0;
        assert!(
            !state.path_to_id.contains_key("build/Hidden.java"),
            "the ordinary index must actually omit the consumer"
        );
        let node = state
            .symbols
            .iter_nodes()
            .find(|node| node.name == "DocumentStore" && node.kind == NodeKind::Type)
            .unwrap();
        let proof = prepare_type(&state, node, None, None).unwrap().unwrap();
        assert!(proof.members().any(|path| path == "build/Hidden.java"));
        assert!(proof
            .cover_scope(&["DocumentStore.java".to_string()])
            .is_err());
        assert!(proof.unchanged(dir.path(), None));
        let added = dir.path().join("Added.java");
        fs::write(&added, "class Added {}").unwrap();
        assert!(!proof.unchanged(dir.path(), None));
        fs::remove_file(&added).unwrap();
        assert!(proof.unchanged(dir.path(), None));
        fs::remove_file(&hidden).unwrap();
        assert!(!proof.unchanged(dir.path(), None));
        fs::write(&hidden, "class Changed {}").unwrap();
        assert!(!proof.unchanged(dir.path(), None));
    }
}
