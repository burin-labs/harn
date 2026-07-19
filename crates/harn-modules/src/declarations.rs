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

fn pattern_names(pattern: &BindingPattern) -> Vec<String> {
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
