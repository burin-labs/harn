use harn_parser::{BindingPattern, Node, SNode};

/// Kind of symbol that can be exported by a module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum DefKind {
    Function,
    Pipeline,
    Tool,
    Skill,
    EvalPack,
    Struct,
    Enum,
    Interface,
    Type,
    Variable,
    Parameter,
}

impl DefKind {
    /// Whether an exported declaration has a runtime binding that can be
    /// projected into an importing module. Type and interface declarations
    /// remain valid imports, but carry only static information.
    pub const fn has_runtime_value(self) -> bool {
        !matches!(self, Self::Type | Self::Interface | Self::Parameter)
    }
}

/// One public name introduced by a single top-level declaration.
///
/// This is the language-level export contract shared by the module graph and
/// the VM artifact compiler. Consumers must not re-derive declaration kinds
/// with independent AST matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicDeclaration {
    pub name: String,
    pub kind: DefKind,
}

/// Return every public name introduced by one declaration.
///
/// Interfaces are public by language design: unlike the other declaration
/// forms, the grammar does not accept a `pub` modifier for them. Attributes
/// preserve the visibility of their wrapped declaration.
pub fn public_declarations(snode: &SNode) -> Vec<PublicDeclaration> {
    match &snode.node {
        Node::AttributedDecl { inner, .. } => public_declarations(inner),
        Node::FnDecl {
            name, is_pub: true, ..
        } => declaration(name, DefKind::Function),
        Node::Pipeline {
            name, is_pub: true, ..
        } => declaration(name, DefKind::Pipeline),
        Node::ToolDecl {
            name, is_pub: true, ..
        } => declaration(name, DefKind::Tool),
        Node::SkillDecl {
            name, is_pub: true, ..
        } => declaration(name, DefKind::Skill),
        Node::EvalPackDecl {
            binding_name,
            is_pub: true,
            ..
        } => declaration(binding_name, DefKind::EvalPack),
        Node::StructDecl {
            name, is_pub: true, ..
        } => declaration(name, DefKind::Struct),
        Node::EnumDecl {
            name, is_pub: true, ..
        } => declaration(name, DefKind::Enum),
        Node::InterfaceDecl { name, .. } => declaration(name, DefKind::Interface),
        Node::TypeDecl {
            name, is_pub: true, ..
        } => declaration(name, DefKind::Type),
        Node::LetBinding {
            pattern,
            is_pub: true,
            ..
        }
        | Node::ConstBinding {
            pattern,
            is_pub: true,
            ..
        } => pattern_names(pattern)
            .into_iter()
            .map(|name| PublicDeclaration {
                name,
                kind: DefKind::Variable,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn declaration(name: &str, kind: DefKind) -> Vec<PublicDeclaration> {
    vec![PublicDeclaration {
        name: name.to_string(),
        kind,
    }]
}

pub(crate) fn pattern_names(pattern: &BindingPattern) -> Vec<String> {
    match pattern {
        BindingPattern::Identifier(name) => vec![name.clone()],
        BindingPattern::Dict(fields) => fields
            .iter()
            .filter_map(|field| field.alias.as_ref().or(Some(&field.key)).cloned())
            .collect(),
        BindingPattern::List(elements) => elements
            .iter()
            .map(|element| element.name.clone())
            .collect(),
        BindingPattern::Pair(a, b) => vec![a.clone(), b.clone()],
    }
}

// ---------------------------------------------------------------------------
// Graph-building declaration walks.
//
// The exports above answer "what is public here?" for callers outside the
// crate. These answer "what does this file declare, and where?" for the module
// graph itself. Both read the same AST, so they share an owner rather than
// letting `lib.rs` grow a second syntax walk beside the graph builder.
// ---------------------------------------------------------------------------

use std::path::Path;

use harn_lexer::Span;

use crate::{import_recording, DefSite, ModuleInfo, PackageSnapshot};

pub(crate) fn collect_module_info(
    file: &Path,
    snode: &SNode,
    module: &mut ModuleInfo,
    package_snapshots: &[PackageSnapshot],
) {
    if let Node::AttributedDecl { inner, .. } = &snode.node {
        collect_module_info(file, inner, module, package_snapshots);
        return;
    }

    for public in public_declarations(snode) {
        module.own_exports.insert(public.name);
    }

    match &snode.node {
        Node::FnDecl { name, params, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Function),
            );
            for param_name in params.iter().map(|param| param.name.clone()) {
                module.declarations.insert(
                    param_name.clone(),
                    decl_site(file, snode.span, &param_name, DefKind::Parameter),
                );
            }
        }
        Node::Pipeline { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Pipeline),
            );
        }
        Node::ToolDecl { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Tool),
            );
        }
        Node::SkillDecl { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Skill),
            );
        }
        Node::EvalPackDecl { binding_name, .. } => {
            module.declarations.insert(
                binding_name.clone(),
                decl_site(file, snode.span, binding_name, DefKind::EvalPack),
            );
        }
        Node::StructDecl { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Struct),
            );
        }
        Node::EnumDecl { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Enum),
            );
        }
        Node::InterfaceDecl { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Interface),
            );
        }
        Node::TypeDecl { name, .. } => {
            module.declarations.insert(
                name.clone(),
                decl_site(file, snode.span, name, DefKind::Type),
            );
        }
        Node::LetBinding { pattern, .. } | Node::ConstBinding { pattern, .. } => {
            for name in pattern_names(pattern) {
                module.declarations.insert(
                    name.clone(),
                    decl_site(file, snode.span, &name, DefKind::Variable),
                );
            }
        }
        _ if import_recording::record_import_node(module, file, snode, package_snapshots) => {}
        _ => {}
    }
}

pub(crate) fn collect_type_declarations(snode: &SNode, decls: &mut Vec<SNode>) {
    match &snode.node {
        Node::TypeDecl { .. }
        | Node::StructDecl { .. }
        | Node::EnumDecl { .. }
        | Node::InterfaceDecl { .. } => decls.push(snode.clone()),
        Node::AttributedDecl { inner, .. } => collect_type_declarations(inner, decls),
        _ => {}
    }
}

pub(crate) fn collect_callable_declarations(snode: &SNode, decls: &mut Vec<SNode>) {
    match &snode.node {
        Node::FnDecl { .. } | Node::Pipeline { .. } | Node::ToolDecl { .. } => {
            decls.push(snode.clone());
        }
        Node::AttributedDecl { inner, .. } => collect_callable_declarations(inner, decls),
        _ => {}
    }
}

pub(crate) fn type_decl_name(snode: &SNode) -> Option<&str> {
    match &snode.node {
        Node::TypeDecl { name, .. }
        | Node::StructDecl { name, .. }
        | Node::EnumDecl { name, .. }
        | Node::InterfaceDecl { name, .. } => Some(name.as_str()),
        _ => None,
    }
}

pub(crate) fn callable_decl_name(snode: &SNode) -> Option<&str> {
    match &snode.node {
        Node::FnDecl { name, .. } | Node::Pipeline { name, .. } | Node::ToolDecl { name, .. } => {
            Some(name.as_str())
        }
        Node::AttributedDecl { inner, .. } => callable_decl_name(inner),
        _ => None,
    }
}

pub(crate) fn decl_site(file: &Path, span: Span, name: &str, kind: DefKind) -> DefSite {
    DefSite {
        name: name.to_string(),
        file: file.to_path_buf(),
        kind,
        span,
    }
}
