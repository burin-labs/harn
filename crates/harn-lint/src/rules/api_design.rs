//! API-shape guidance for capability attenuation and explicit call sites.
//!
//! These are deliberately conservative diagnostics. A root
//! `Harness` is right at entry and orchestration boundaries; an ordinary
//! helper whose entire observed authority is one or two direct sub-handles
//! should advertise those narrower nominal types instead. Public APIs with
//! four or more same-typed positional values should use a named closed record
//! so call sites state which value is which.

use std::{collections::BTreeSet, path::Path};

use harn_builtin_meta::CapabilityId;
use harn_lexer::{FixEdit, Span};
use harn_parser::{visit, DiagnosticCode as Code, Node, SNode, TypeExpr, TypedParam};
use harn_vm::HarnessKind;

use crate::diagnostic::{LintDiagnostic, LintSeverity};

const ATTENUATION_RULE: &str = "capability-attenuation";
const PARAMETER_NAME_RULE: &str = "capability-parameter-name";
const POSITIONAL_RULE: &str = "homogeneous-positional-api";
const POSITIONAL_THRESHOLD: usize = 4;

/// A root `Harness` parameter that local syntax proves can be narrowed.
///
/// This is the semantic contract shared by the advisory lint and the
/// whole-program fixer. Keeping boundary and escape analysis here prevents the
/// fixer from inventing a broader attenuation policy than the diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityAttenuation {
    pub declaration_span: Span,
    pub parameter_name: String,
    pub capabilities: BTreeSet<CapabilityId>,
}

/// Package and source facts that identify host-entered module exports.
///
/// Keep these facts together so diagnostics and whole-program repair cannot
/// disagree about which public signatures a runtime owns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuntimeModuleContext {
    connector_module: bool,
    mcp_module: bool,
}

impl RuntimeModuleContext {
    /// Build runtime-module context from the source path and package manifest.
    #[must_use]
    pub fn for_source(path: Option<&Path>, connector_module: bool) -> Self {
        let mcp_module = path
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".mcp.harn"));
        Self {
            connector_module,
            mcp_module,
        }
    }

    #[cfg(test)]
    fn mcp() -> Self {
        Self {
            mcp_module: true,
            ..Self::default()
        }
    }
}

/// The callables a runtime or host framework enters directly.
///
/// The signatures of these are a contract with a caller no source file in this
/// module can see, so neither the *type* nor the *name* of their parameters is
/// ours to change: narrowing one makes the callable uncallable, and renaming
/// one renames a value the runtime supplies positionally by contract.
/// Computing the set once and sharing it keeps [`capability_attenuations`] and
/// [`check_capability_parameter_names`] from drifting to two answers about
/// what counts as a boundary.
pub struct RuntimeBoundaries {
    /// Named functions installed as a `handler:` field. This structural
    /// registration is stronger evidence than counting body uses.
    callbacks: BTreeSet<String>,
    /// Named functions carrying [`HOST_ENTRY_ATTRIBUTE`].
    host_entries: BTreeSet<String>,
    /// Whether this module is a connector, whose runtime exports are entered
    /// by the connector ABI rather than by local callers.
    connector_module: bool,
    /// Whether public functions in this source are tools entered by the MCP
    /// server runtime rather than ordinary source callers.
    mcp_module: bool,
}

/// Declares that an embedding host, not a caller in this program, supplies a
/// callable's arguments.
///
/// Every other boundary in [`RuntimeBoundaries`] is recognizable from something
/// Harn owns — a name, a trigger signature, a `handler:` field, a package
/// manifest. A function an embedding Rust host reaches through the runtime's
/// call-into-script path has none of those: its only registration lives in the
/// host's own source, which no Harn tool can read. Without a declaration the
/// body is the sole evidence, and `harn fix` narrows the signature to what the
/// body happens to touch — producing a parameter type the host cannot pass and
/// a failure at dispatch rather than at `harn check` (#6193).
///
/// The type checker owns the recognized attribute vocabulary; a rename there
/// that missed this name would show up as `test_host_entry_is_recognized_on_a_function`
/// and `host_entry_suppresses_the_attenuation_diagnostic` failing together.
const HOST_ENTRY_ATTRIBUTE: &str = "host_entry";

impl RuntimeBoundaries {
    /// Collect the boundary set for a parsed module.
    ///
    /// `module_context` contains the source and package facts that syntax alone
    /// cannot prove. A declared connector wins over fallback inference.
    #[must_use]
    pub fn collect(program: &[SNode], module_context: RuntimeModuleContext) -> Self {
        let mut callbacks = BTreeSet::new();
        let mut host_entries = BTreeSet::new();
        let mut public_functions = BTreeSet::new();
        visit::walk_program(program, &mut |node| {
            if let Node::FnDecl {
                name, is_pub: true, ..
            } = &node.node
            {
                public_functions.insert(name.clone());
            }
            if let Node::AttributedDecl { attributes, inner } = &node.node {
                if let Node::FnDecl { name, .. } = &inner.node {
                    if attributes
                        .iter()
                        .any(|attribute| attribute.name == HOST_ENTRY_ATTRIBUTE)
                    {
                        host_entries.insert(name.clone());
                    }
                }
            }
            let Node::DictLiteral(entries) = &node.node else {
                return;
            };
            for entry in entries {
                let is_handler = matches!(
                    &entry.key.node,
                    Node::Identifier(key) | Node::StringLiteral(key) if key == "handler"
                );
                if is_handler {
                    if let Node::Identifier(name) = &entry.value.node {
                        callbacks.insert(name.clone());
                    }
                }
            }
        });
        // A declaration outranks the inference. Requiring every metadata
        // export in this one file is evidence that it looks like a connector
        // module, not a statement that it is one: move `payload_schema` to a
        // sibling module and every runtime export here silently becomes
        // attenuable, so `harn fix` rewrites `normalize_inbound` into a
        // signature the connector ABI rejects. The inference stays for files
        // linted with no package context, where nothing can declare anything.
        let connector_module = module_context.connector_module
            || harn_vm::connectors::harn_module::abi::metadata_exports()
                .iter()
                .all(|name| public_functions.contains(*name));
        Self {
            callbacks,
            host_entries,
            connector_module,
            mcp_module: module_context.mcp_module,
        }
    }

    /// Whether this callable is entered by a runtime or host framework
    /// rather than by a caller in this module.
    #[must_use]
    pub fn contains(
        &self,
        name: &str,
        params: &[TypedParam],
        is_pub: bool,
        attributed_boundary: bool,
    ) -> bool {
        let trigger_boundary = params.len() >= 2
            && matches!(
                params[0].type_expr.as_ref(),
                Some(TypeExpr::Named(type_name)) if type_name == HarnessKind::Root.type_name()
            )
            && matches!(
                params[1].type_expr.as_ref(),
                Some(TypeExpr::Named(type_name)) if type_name == "TriggerEvent"
            );
        let connector_boundary = self.connector_module
            && is_pub
            && harn_vm::connectors::harn_module::abi::is_runtime_export(name);
        let mcp_boundary = self.mcp_module && is_pub;
        name == "main"
            || attributed_boundary
            || trigger_boundary
            || connector_boundary
            || mcp_boundary
            || self.callbacks.contains(name)
            || self.host_entries.contains(name)
    }
}

/// Whether an attribute makes this declaration a **root-`Harness`** runtime
/// entrypoint.
///
/// `@job` is entered by the scheduler with the root handle. A flow predicate is
/// deliberately *not* one of these: flow evaluation injects exactly one
/// `HarnessAst`, so treating it as a root boundary would claim it needs
/// authority the runtime never supplies.
#[must_use]
pub fn root_harness_boundary_attribute(outer: &SNode) -> bool {
    let Node::AttributedDecl { attributes, .. } = &outer.node else {
        return false;
    };
    attributes.iter().any(|attribute| attribute.name == "job")
}

/// Whether the runtime, rather than a caller in this module, supplies this
/// declaration's arguments at all.
///
/// Broader than [`root_harness_boundary_attribute`] because it also covers flow
/// predicates. The distinction matters: a flow predicate's handle is injected
/// *positionally by contract*, so its name is not ours to change even though
/// its type is narrow and known.
#[must_use]
pub fn runtime_supplies_arguments(outer: &SNode) -> bool {
    let Node::AttributedDecl { attributes, inner } = &outer.node else {
        return false;
    };
    root_harness_boundary_attribute(outer)
        || harn_parser::is_flow_predicate_declaration(attributes, inner)
}

/// Return every conservatively attenuable root parameter in a parsed module.
///
/// `source` must be the text `program` was parsed from. It is not a
/// convenience: string-interpolation holes are unparsed source text on the
/// AST, so proving a capability is unused requires re-parsing them.
#[must_use]
pub fn capability_attenuations(
    source: &str,
    program: &[SNode],
    module_context: RuntimeModuleContext,
) -> Vec<CapabilityAttenuation> {
    let boundaries = RuntimeBoundaries::collect(program, module_context);
    let mut attenuations = Vec::new();
    for outer in program {
        let attributed = root_harness_boundary_attribute(outer);
        let inner = match &outer.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => outer,
        };
        let Node::FnDecl {
            name,
            params,
            body,
            is_pub,
            ..
        } = &inner.node
        else {
            continue;
        };

        if !boundaries.contains(name, params, *is_pub, attributed) {
            attenuations.extend(params.iter().filter_map(|parameter| {
                capability_attenuation_for_parameter(source, params, body, parameter).map(
                    |capabilities| CapabilityAttenuation {
                        declaration_span: inner.span,
                        parameter_name: parameter.name.clone(),
                        capabilities,
                    },
                )
            }));
        }
    }
    attenuations
}

pub(crate) fn check_api_design(
    source: &str,
    program: &[SNode],
    module_context: RuntimeModuleContext,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let attenuations = capability_attenuations(source, program, module_context);
    let module_names = module_level_names(program);
    let boundaries = RuntimeBoundaries::collect(program, module_context);
    for outer in program {
        let attributed = root_harness_boundary_attribute(outer);
        // Naming asks the broader question: a flow predicate's handle is
        // injected positionally, so its name is a contract even though it is
        // not a root-Harness boundary.
        let injected = runtime_supplies_arguments(outer);
        let declaration = match &outer.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => outer,
        };
        let Node::FnDecl {
            name,
            params,
            body,
            is_pub,
            ..
        } = &declaration.node
        else {
            continue;
        };
        for attenuation in attenuations
            .iter()
            .filter(|candidate| candidate.declaration_span == declaration.span)
        {
            push_capability_attenuation_diagnostic(name, declaration, attenuation, diagnostics);
        }
        if !injected && !boundaries.contains(name, params, *is_pub, attributed) {
            check_capability_parameter_names(name, params, body, &module_names, diagnostics);
        }
        if *is_pub {
            check_homogeneous_positionals(name, params, declaration, diagnostics);
        }
    }
}

fn capability_attenuation_for_parameter(
    source: &str,
    params: &[TypedParam],
    body: &[SNode],
    parameter: &TypedParam,
) -> Option<BTreeSet<CapabilityId>> {
    if !matches!(
        parameter.type_expr.as_ref(),
        Some(TypeExpr::Named(name)) if name == HarnessKind::Root.type_name()
    ) {
        return None;
    }

    let mut identifier_uses = 0usize;
    let mut direct_subhandle_uses = 0usize;
    let mut unknown_member = false;
    let mut capabilities = BTreeSet::new();
    let mut shadowed_in_nested_callable = false;
    let mut record_use = |node: &SNode| match &node.node {
        Node::Identifier(name) if name == &parameter.name => identifier_uses += 1,
        Node::PropertyAccess { object, property }
        | Node::OptionalPropertyAccess { object, property }
            if matches!(&object.node, Node::Identifier(name) if name == &parameter.name) =>
        {
            direct_subhandle_uses += 1;
            if let Some(capability) = CapabilityId::from_field_name(property) {
                capabilities.insert(capability);
            } else {
                unknown_member = true;
            }
        }
        Node::FnDecl { params, .. } | Node::Closure { params, .. }
            if params.iter().any(|nested| nested.name == parameter.name) =>
        {
            shadowed_in_nested_callable = true;
        }
        _ => {}
    };
    // Defaults execute in the callable's scope and may use authority just
    // like the body. Ignoring them can falsely attenuate a root parameter,
    // leaving the default with an unreachable grant.
    for candidate in params {
        if let Some(default) = &candidate.default_value {
            visit::walk_node(default, &mut record_use);
        }
    }
    // Interpolation-aware, and load-bearing. `"${harness.random.uuid_v7()}"`
    // is a use; walking without the holes counts zero and the attenuation
    // silently omits the capability. `harn fix` applies this set verbatim, so
    // an undercount rewrites working code into code that does not compile.
    visit::walk_program_interpolated(source, body, &mut record_use);

    // Suppress when the root escapes, is forwarded, or touches an unknown
    // member: local syntax no longer proves attenuation is safe.
    (identifier_uses != 0
        && identifier_uses == direct_subhandle_uses
        && !unknown_member
        && !shadowed_in_nested_callable
        && (1..=2).contains(&capabilities.len()))
    .then_some(capabilities)
}

fn push_capability_attenuation_diagnostic(
    function_name: &str,
    declaration: &SNode,
    attenuation: &CapabilityAttenuation,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let mut capabilities = attenuation.capabilities.iter().copied().collect::<Vec<_>>();
    capabilities.sort_by_key(|capability| capability.field_name());
    let signature = capabilities
        .iter()
        .map(|capability| format!("{}: {}", capability.field_name(), capability.type_name()))
        .collect::<Vec<_>>()
        .join(", ");
    let capability_names = capabilities
        .iter()
        .map(|capability| {
            format!(
                "`{}.{}`",
                attenuation.parameter_name,
                capability.field_name()
            )
        })
        .collect::<Vec<_>>()
        .join(" and ");
    diagnostics.push(LintDiagnostic {
        code: Code::LintBroadHarnessParameter,
        rule: ATTENUATION_RULE.into(),
        message: format!(
            "helper `{function_name}` accepts root `Harness` but uses only {capability_names}"
        ),
        span: declaration.span,
        severity: if capabilities.len() == 1 {
            LintSeverity::Warning
        } else {
            LintSeverity::Info
        },
        suggestion: Some(if capabilities.len() == 1 {
            format!(
                "accept the narrow capability parameter `{signature}` and pass the sub-handle at call sites; keep root `Harness` for entrypoints and genuine multi-capability orchestration"
            )
        } else {
            format!(
                "accept one closed capability record `{{{signature}}}` and construct it from the two sub-handles at call sites; keep root `Harness` for entrypoints and genuine multi-capability orchestration"
            )
        }),
        // Narrowing a signature is only safe when every call site moves
        // with it, and a caller can live in a module this rule never sees.
        // Until the fixer resolves capability requirements across module
        // boundaries, this stays advisory.
        fix: None,
    });
}

fn check_homogeneous_positionals(
    function_name: &str,
    params: &[TypedParam],
    declaration: &SNode,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    let mut groups: Vec<(TypeExpr, Vec<&str>)> = Vec::new();
    for parameter in params {
        let Some(ty) = parameter.type_expr.as_ref() else {
            continue;
        };
        if parameter.rest
            || matches!(ty, TypeExpr::Named(name) if name == HarnessKind::Root.type_name())
        {
            continue;
        }
        if let Some((_, names)) = groups.iter_mut().find(|(candidate, _)| candidate == ty) {
            names.push(parameter.name.as_str());
        } else {
            groups.push((ty.clone(), vec![parameter.name.as_str()]));
        }
    }
    let Some(names) = groups
        .iter()
        .map(|(_, names)| names)
        .filter(|names| names.len() >= POSITIONAL_THRESHOLD)
        .max_by_key(|names| names.len())
    else {
        return;
    };

    diagnostics.push(LintDiagnostic {
        code: Code::LintHomogeneousPositionalApi,
        rule: POSITIONAL_RULE.into(),
        message: format!(
            "public function `{function_name}` has {} same-typed positional parameters ({})",
            names.len(),
            names.join(", ")
        ),
        span: declaration.span,
        severity: LintSeverity::Info,
        suggestion: Some(
            "replace the ambiguous positional group with one named closed-record parameter; construct it with named fields at call sites and destructure it inside the function"
                .to_string(),
        ),
        fix: None,
    });
}

/// Report a parameter that carries a narrow capability handle under a name
/// that does not say which capability it carries.
///
/// A parameter still called `harness` reads as the root handle at every call
/// site, which hides the very attenuation the narrower type performs. This is
/// the counterpart to [`ATTENUATION_RULE`]: that rule narrows the type, this
/// one keeps the name honest once the type is already narrow — including on
/// code the fixer migrated before it named the parameter itself.
fn check_capability_parameter_names(
    function_name: &str,
    params: &[TypedParam],
    body: &[SNode],
    module_names: &BTreeSet<String>,
    diagnostics: &mut Vec<LintDiagnostic>,
) {
    for parameter in params {
        let Some(capability) = narrow_capability_parameter(parameter) else {
            continue;
        };
        let preferred = capability.field_name();
        if parameter.name == preferred || module_names.contains(preferred) {
            continue;
        }
        let Some(references) = renameable_references(params, body, &parameter.name, preferred)
        else {
            continue;
        };

        // `TypedParam::span` runs from an optional `...` through the default
        // value; a non-rest parameter's name starts at its first byte.
        let name_span = Span::with_offsets(
            parameter.span.start,
            parameter.span.start + parameter.name.len(),
            parameter.span.line,
            parameter.span.column,
        );
        let mut fix = vec![FixEdit {
            span: name_span,
            replacement: preferred.to_string(),
        }];
        fix.extend(references.into_iter().map(|span| FixEdit {
            span,
            replacement: preferred.to_string(),
        }));

        diagnostics.push(LintDiagnostic {
            code: Code::LintCapabilityParameterName,
            rule: PARAMETER_NAME_RULE.into(),
            message: format!(
                "`{function_name}` names its `{}` parameter `{}`, not `{preferred}`",
                capability.type_name(),
                parameter.name
            ),
            span: name_span,
            severity: LintSeverity::Warning,
            suggestion: Some(format!(
                "rename the parameter to `{preferred}: {}` so the signature states which capability it carries; Harn arguments are positional, so no call site changes",
                capability.type_name()
            )),
            fix: Some(fix),
        });
    }
}

/// Every name the module already spells that a parameter must not take over.
///
/// A callee is not an identifier node — `FunctionCall` carries its target as a
/// plain `String` — so walking identifiers alone cannot see that `secrets` is
/// the function this body calls. Renaming a parameter onto it produces
/// `secrets(auth, secrets, …)`, which parses, type-checks locally, and calls a
/// capability handle. Collect declarations, imports, and call targets across
/// the whole module and refuse any name among them.
fn module_level_names(program: &[SNode]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    visit::walk_program(program, &mut |node| match &node.node {
        Node::FunctionCall { name, .. } => {
            names.insert(name.clone());
        }
        Node::FnDecl { name, .. } | Node::Pipeline { name, .. } => {
            names.insert(name.clone());
        }
        Node::SelectiveImport {
            names: imported, ..
        } => {
            names.extend(imported.iter().cloned());
        }
        Node::NamespaceImport { alias, .. } => {
            names.insert(alias.clone());
        }
        _ => {}
    });
    names
}

/// The capability a parameter's declared type carries, when that type is a
/// narrow sub-handle. Root `Harness` and every non-capability type yield
/// `None`; so does a rest parameter, which collects values rather than one
/// handle.
fn narrow_capability_parameter(parameter: &TypedParam) -> Option<CapabilityId> {
    // A synthetic parameter carries `Span::dummy()`, so there is no name span
    // to rewrite and no source text the rename would improve.
    //
    // A leading underscore is not a spelling of the name — it is the author
    // declaring the parameter deliberately unused, and every unused-binding
    // lint reads it. Renaming `_fs` to `fs` silently revokes that declaration
    // and the next pass reports the parameter as unused.
    if parameter.rest || parameter.name.starts_with('_') || parameter.span == Span::dummy() {
        return None;
    }
    match parameter.type_expr.as_ref() {
        Some(TypeExpr::Named(name)) => CapabilityId::from_type_name(name),
        _ => None,
    }
}

/// Every span that a rename of `current` to `preferred` must rewrite, or
/// `None` when the rename is not provably safe here.
///
/// The rename is refused when `preferred` already appears anywhere in the
/// callable (renaming would capture it) and when a nested callable rebinds
/// `current` (its inner references belong to a different binding). Dict-literal
/// keys are excluded from the rewrite: they are record field names that happen
/// to parse as identifiers, and renaming one changes the record's shape.
fn renameable_references(
    params: &[TypedParam],
    body: &[SNode],
    current: &str,
    preferred: &str,
) -> Option<Vec<Span>> {
    if params.iter().any(|candidate| candidate.name == preferred) {
        return None;
    }

    let mut references = Vec::new();
    let mut dict_keys = BTreeSet::new();
    let mut taken = false;
    let mut shadowed = false;
    let mut record = |node: &SNode| match &node.node {
        Node::Identifier(name) if name == current => references.push(node.span),
        Node::Identifier(name) if name == preferred => taken = true,
        Node::FnDecl { params, .. } | Node::Closure { params, .. }
            if params
                .iter()
                .any(|nested| nested.name == current || nested.name == preferred) =>
        {
            shadowed = true;
        }
        Node::DictLiteral(entries) => {
            for entry in entries {
                if matches!(&entry.key.node, Node::Identifier(key) if key == current) {
                    dict_keys.insert((entry.key.span.start, entry.key.span.end));
                }
            }
        }
        _ => {}
    };
    for candidate in params {
        if let Some(default) = &candidate.default_value {
            visit::walk_node(default, &mut record);
        }
    }
    visit::walk_program(body, &mut record);

    (!taken && !shadowed).then(|| {
        references
            .into_iter()
            .filter(|span| !dict_keys.contains(&(span.start, span.end)))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    use super::*;

    fn lint(source: &str) -> Vec<LintDiagnostic> {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let program = Parser::new(tokens).parse().expect("parse");
        let mut diagnostics = Vec::new();
        check_api_design(
            source,
            &program,
            RuntimeModuleContext::default(),
            &mut diagnostics,
        );
        diagnostics
    }

    fn attenuated(source: &str) -> Vec<BTreeSet<CapabilityId>> {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let program = Parser::new(tokens).parse().expect("parse");
        capability_attenuations(source, &program, RuntimeModuleContext::default())
            .into_iter()
            .map(|attenuation| attenuation.capabilities)
            .collect()
    }

    fn attenuated_in_declared_connector(source: &str) -> Vec<BTreeSet<CapabilityId>> {
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let program = Parser::new(tokens).parse().expect("parse");
        capability_attenuations(
            source,
            &program,
            RuntimeModuleContext::for_source(None, true),
        )
        .into_iter()
        .map(|attenuation| attenuation.capabilities)
        .collect()
    }

    #[test]
    fn public_mcp_tools_keep_runtime_owned_harness_signatures() {
        let source = "pub fn search(harness: Harness) { return harness.net.get(\"https://example.com\") }\n\nfn helper(harness: Harness) { return harness.clock.now_ms() }\n";
        let tokens = Lexer::new(source).tokenize().expect("lex");
        let program = Parser::new(tokens).parse().expect("parse");
        let attenuations = capability_attenuations(source, &program, RuntimeModuleContext::mcp());

        assert_eq!(attenuations.len(), 1, "only the private helper may narrow");
        assert_eq!(
            attenuations[0].capabilities,
            BTreeSet::from([CapabilityId::Clock])
        );
    }

    /// The connector ABI pins every runtime export to a root `Harness`.
    ///
    /// Inferring connector-ness from "all three metadata exports are declared
    /// `pub fn` in this file" is evidence, not a declaration: move one to a
    /// sibling module and `harn fix` rewrites `normalize_inbound` into a
    /// signature the runtime rejects. `payload_schema` is the one missing here.
    const PARTIAL_CONNECTOR: &str = "pub fn provider_id() { return \"probe\" }\n\npub fn kinds() { return [\"probe.event\"] }\n\npub fn normalize_inbound(harness: Harness, raw) {\n  return {id: harness.clock.now_ms(), raw: raw}\n}\n";

    #[test]
    fn a_declared_connector_module_keeps_its_runtime_export_root() {
        assert!(
            attenuated_in_declared_connector(PARTIAL_CONNECTOR).is_empty(),
            "the manifest declares this module, so its runtime exports are boundaries"
        );
    }

    #[test]
    fn an_undeclared_module_still_falls_back_to_in_file_evidence() {
        assert_eq!(
            attenuated(PARTIAL_CONNECTOR),
            vec![BTreeSet::from([CapabilityId::Clock])],
            "with no package context nothing can declare anything, so the \
             inference is all that is left"
        );
    }

    /// A capability reached only through `${...}` is still reached.
    ///
    /// `harn fix` applies this set verbatim to the signature and every call
    /// site, so omitting one deletes a live capability: harn-cloud#1472 shipped
    /// `harness.random.uuid_v7()` with `random` stripped from its parameter
    /// type, and the rewritten tests failed with `value of type nil has no
    /// method uuid_v7`.
    #[test]
    fn a_capability_used_only_inside_interpolation_still_counts() {
        assert_eq!(
            attenuated(
                "fn helper(harness: Harness, body) {\n  const dir = \".tmp-${harness.random.uuid_v7()}\"\n  harness.fs.mkdir(dir)\n  return body(dir)\n}\n"
            ),
            vec![BTreeSet::from([CapabilityId::Fs, CapabilityId::Random])],
            "attenuating to `fs` alone would delete the only use of `random`"
        );
    }

    /// The same program with the call moved out of the hole must attenuate
    /// identically — otherwise the analysis is reporting on syntax position
    /// rather than on use.
    #[test]
    fn interpolated_and_plain_uses_attenuate_the_same() {
        let interpolated = attenuated(
            "fn helper(harness: Harness) {\n  return \"${harness.clock.now_ms()}\" + harness.fs.cwd()\n}\n",
        );
        let plain = attenuated(
            "fn helper(harness: Harness) {\n  return harness.clock.now_ms() + harness.fs.cwd()\n}\n",
        );
        assert_eq!(interpolated, plain);
        assert_eq!(
            plain,
            vec![BTreeSet::from([CapabilityId::Clock, CapabilityId::Fs])]
        );
    }

    /// An unknown member suppresses attenuation entirely. That guard has to see
    /// through interpolation too, or a hole becomes a way to hide the escape
    /// that makes narrowing unsafe.
    #[test]
    fn an_unknown_member_inside_interpolation_still_suppresses() {
        assert!(
            attenuated(
                "fn helper(harness: Harness) {\n  return \"${harness.not_a_capability}\" + harness.fs.cwd()\n}\n"
            )
            .is_empty(),
            "an unrecognized member is only safe to ignore if it does not exist"
        );
    }

    /// Lint `source`, apply every `capability-parameter-name` fix, and return
    /// the rewritten source. Asserts the result still parses, so a test can
    /// never pass on a rename that produced invalid syntax.
    fn rename_fixed(source: &str) -> String {
        let edits = lint(source)
            .into_iter()
            .filter(|diagnostic| diagnostic.rule == PARAMETER_NAME_RULE)
            .filter_map(|diagnostic| diagnostic.fix)
            .flatten()
            .collect::<Vec<_>>();
        let mut fixed = source.to_string();
        for edit in FixEdit::dedupe_overlapping(&edits) {
            fixed.replace_range(edit.span.start..edit.span.end, &edit.replacement);
        }
        Parser::new(Lexer::new(&fixed).tokenize().expect("relex"))
            .parse()
            .expect("fixed source parses");
        fixed
    }

    #[test]
    fn recommends_one_narrow_handle_for_an_ordinary_helper() {
        let source = "fn load(harness: Harness, path: string) { harness.fs.exists(path); return harness.fs.read_text(path) }";
        let diagnostics = lint(source);
        assert_eq!(
            diagnostics
                .iter()
                .filter(|d| d.rule == ATTENUATION_RULE)
                .count(),
            1
        );
        assert!(diagnostics[0]
            .suggestion
            .as_deref()
            .unwrap()
            .contains("HarnessFs"));
        assert_eq!(
            diagnostics[0].repair().expect("repair").safety,
            harn_parser::RepairSafety::SurfaceChanging
        );
        // Advisory only: a caller can live in a module this rule never sees,
        // so narrowing the signature is not safe to apply automatically.
        assert!(diagnostics[0].fix.is_none());
    }

    #[test]
    fn recommends_a_closed_capability_record_for_two_capability_helpers() {
        let multi = lint(
            "fn copy(harness: Harness, path: string) { harness.obs.log_info(path); return harness.fs.read_text(path) }",
        );
        assert_eq!(multi.len(), 1);
        let suggestion = multi[0].suggestion.as_deref().expect("suggestion");
        assert!(
            suggestion.contains("{fs: HarnessFs, obs: HarnessObs}"),
            "{suggestion}"
        );
        assert!(multi[0].fix.is_none());
    }

    #[test]
    fn does_not_fix_shadowed_receivers() {
        let shadowed = lint(
            "fn load(harness: Harness) { const callback = { harness: Harness -> harness.fs.cwd() }; return harness.fs.cwd() }",
        );
        assert!(shadowed
            .iter()
            .all(|diagnostic| diagnostic.rule != ATTENUATION_RULE));
    }

    #[test]
    fn parameter_defaults_participate_in_authority_analysis() {
        let multi = lint(
            "fn load(harness: Harness, root: string = harness.fs.cwd()) { return harness.agent.current_id() }",
        );
        assert_eq!(multi.len(), 1);
        // The default executes in the callable's scope, so its `harness.fs`
        // use has to widen the recommendation alongside the body's agent use.
        let suggestion = multi[0].suggestion.as_deref().expect("suggestion");
        assert!(
            suggestion.contains("{agent: HarnessAgent, fs: HarnessFs}"),
            "{suggestion}"
        );

        let escaped = lint(
            "fn load(harness: Harness, root: string = project_root(harness)) { return harness.fs.read_text(root) }",
        );
        assert!(escaped
            .iter()
            .all(|diagnostic| diagnostic.rule != ATTENUATION_RULE));
    }

    #[test]
    fn preserves_entrypoints_or_root_values_that_escape() {
        let diagnostics = lint(
            "fn main(harness: Harness) { harness.fs.cwd() }\nfn orchestrate(harness: Harness) { delegate(harness) }",
        );
        assert!(diagnostics.iter().all(|d| d.rule != ATTENUATION_RULE));
    }

    #[test]
    fn preserves_runtime_registered_handler_boundaries() {
        let diagnostics = lint(
            "fn on_event(harness: Harness, event) { harness.channels.append(\"seen\", event) }\n\
             fn install(runtime: HarnessRuntime) { runtime.trigger_register({handler: on_event}) }",
        );
        assert!(diagnostics.iter().all(|d| d.rule != ATTENUATION_RULE));
    }

    #[test]
    fn preserves_job_entrypoint_boundaries() {
        let diagnostics =
            lint("@job(\"scan\")\npub fn scan(harness: Harness, event) { return event.kind }");
        assert!(diagnostics.iter().all(|d| d.rule != ATTENUATION_RULE));
    }

    #[test]
    fn preserves_nominal_trigger_handler_boundaries() {
        let diagnostics =
            lint("pub fn on_event(harness: Harness, event: TriggerEvent) { return event.kind }");
        assert!(diagnostics.iter().all(|d| d.rule != ATTENUATION_RULE));
    }

    #[test]
    fn preserves_connector_runtime_export_boundaries() {
        let diagnostics = lint(
            "pub fn provider_id() { return \"example\" }\n\
             pub fn kinds() { return [\"webhook\"] }\n\
             pub fn payload_schema() { return {} }\n\
             pub fn init(harness: Harness, ctx) { harness.runtime.store_set(\"ctx\", ctx) }\n\
             pub fn normalize_inbound(harness: Harness, raw) { return {raw: raw, secret: harness.secrets.read(\"hook\")} }\n\
             pub fn helper(harness: Harness) { harness.runtime.store_get(\"ctx\") }",
        );
        let attenuation = diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.rule == ATTENUATION_RULE)
            .collect::<Vec<_>>();
        assert_eq!(attenuation.len(), 1);
        assert!(attenuation[0].message.contains("helper `helper`"));
    }

    #[test]
    fn ordinary_public_function_named_like_connector_export_is_not_exempt() {
        let diagnostics = lint(
            "pub fn call(harness: Harness, method, args) { harness.net.request(method, args.url) }",
        );
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.rule == ATTENUATION_RULE));
    }

    #[test]
    fn allows_genuine_multi_capability_orchestration() {
        let diagnostics = lint(
            "fn coordinate(harness: Harness) { harness.fs.cwd(); harness.net.get(\"x\"); harness.clock.now() }",
        );
        assert!(diagnostics.iter().all(|d| d.rule != ATTENUATION_RULE));
    }

    #[test]
    fn recommends_a_closed_record_for_ambiguous_public_positionals() {
        let diagnostics = lint(
            "pub fn bounds(left: int, top: int, right: int, bottom: int) -> int { return left }",
        );
        assert_eq!(
            diagnostics
                .iter()
                .filter(|d| d.rule == POSITIONAL_RULE)
                .count(),
            1
        );
        assert!(diagnostics[0]
            .suggestion
            .as_deref()
            .unwrap()
            .contains("closed-record"));
    }

    #[test]
    fn counts_defaulted_parameters_that_remain_positional_at_call_sites() {
        let diagnostics = lint(
            "pub fn connect(host: string, user: string, password: string = \"\", database: string = \"\") {}",
        );
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.rule == POSITIONAL_RULE));
    }

    #[test]
    fn private_or_heterogeneous_signatures_are_not_flagged() {
        let diagnostics = lint(
            "fn private(a: int, b: int, c: int, d: int) {}\npub fn mixed(a: int, b: int, c: int, label: string) -> nil {}",
        );
        assert!(diagnostics.iter().all(|d| d.rule != POSITIONAL_RULE));
    }

    #[test]
    fn renames_a_narrow_capability_parameter_and_its_references() {
        let source =
            "pub fn ack(harness: HarnessNet, url: string) { return harness.http_post(url, {}) }";
        let diagnostics = lint(source)
            .into_iter()
            .filter(|diagnostic| diagnostic.rule == PARAMETER_NAME_RULE)
            .collect::<Vec<_>>();
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0].message.contains("`harness`, not `net`"),
            "{}",
            diagnostics[0].message
        );
        assert_eq!(
            diagnostics[0].repair().expect("repair").safety,
            harn_parser::RepairSafety::SurfaceChanging
        );
        assert_eq!(
            rename_fixed(source),
            "pub fn ack(net: HarnessNet, url: string) { return net.http_post(url, {}) }"
        );
    }

    #[test]
    fn a_root_harness_parameter_keeps_its_name() {
        // Root `Harness` is not a capability sub-handle, so `harness` is the
        // right name for it; only the attenuation rule has anything to say.
        let diagnostics = lint("pub fn main(harness: Harness) { harness.stdio.println(\"hi\") }");
        assert!(diagnostics.iter().all(|d| d.rule != PARAMETER_NAME_RULE));
    }

    #[test]
    fn an_already_named_capability_parameter_is_not_flagged() {
        let diagnostics =
            lint("pub fn read(fs: HarnessFs, path: string) { return fs.read_text(path) }");
        assert!(diagnostics.iter().all(|d| d.rule != PARAMETER_NAME_RULE));
    }

    #[test]
    fn refuses_a_rename_that_would_capture_an_existing_binding() {
        // `net` already means something else in this body; renaming the
        // parameter onto it would silently rebind every use.
        let source =
            "pub fn ack(harness: HarnessNet) { const net = 1; return harness.http_get(net) }";
        let diagnostics = lint(source);
        assert!(diagnostics.iter().all(|d| d.rule != PARAMETER_NAME_RULE));
    }

    #[test]
    fn leaves_a_deliberately_unused_capability_parameter_alone() {
        // `_fs` says "I know this is unused". Renaming it to `fs` revokes that
        // and the unused-parameter lint then fires on the repair's own output.
        let source = "fn inspect(_fs: HarnessFs, left: string) { return left }";
        let diagnostics = lint(source);
        assert!(diagnostics.iter().all(|d| d.rule != PARAMETER_NAME_RULE));
    }

    #[test]
    fn refuses_a_rename_that_would_shadow_a_called_function() {
        // Found on `conformance/tests/stdlib/oauth/oauth_storage_secrets.harn`:
        // `secrets` is the imported function this body calls. A callee is a
        // plain string in the AST, not an identifier node, so walking
        // identifiers alone does not see it — and the rename would have
        // produced `secrets(auth, secrets, …)`, calling a capability handle.
        let source = "import { secrets } from \"std/oauth\"\n\nfn exercise(auth: HarnessAuth, secret_store: HarnessSecrets) {\n  return secrets(auth, secret_store, {})\n}\n";
        let diagnostics = lint(source);
        assert!(
            diagnostics.iter().all(|d| d.rule != PARAMETER_NAME_RULE),
            "{diagnostics:#?}"
        );
    }

    #[test]
    fn refuses_a_rename_when_a_nested_callable_rebinds_the_name() {
        let source =
            "pub fn ack(harness: HarnessNet) { return [1].map(fn(harness) { return harness }) }";
        let diagnostics = lint(source);
        assert!(diagnostics.iter().all(|d| d.rule != PARAMETER_NAME_RULE));
    }

    #[test]
    fn leaves_a_dict_key_that_shares_the_parameter_name_alone() {
        // `{harness: harness}` is a record field named `harness` whose value
        // is the parameter. Only the value moves; renaming the key would
        // change the record's shape.
        let source = "pub fn ack(harness: HarnessNet) { return {harness: harness} }";
        assert_eq!(
            rename_fixed(source),
            "pub fn ack(net: HarnessNet) { return {harness: net} }"
        );
    }

    #[test]
    fn renames_a_parameter_referenced_from_a_later_default() {
        let source = "pub fn ack(harness: HarnessNet, target = harness.base_url()) { return harness.http_get(target) }";
        assert_eq!(
            rename_fixed(source),
            "pub fn ack(net: HarnessNet, target = net.base_url()) { return net.http_get(target) }"
        );
    }
}
