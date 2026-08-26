//! Flow refinements produced by schema checks and declared predicates.

use crate::ast::{Node, SNode, TypeExpr};

use super::super::schema_inference::schema_type_expr_from_node;
use super::super::scope::{PathNarrowing, Refinements, TypeScope};
use super::super::union::{intersect_types, reference_path_key, subtract_type};
use super::super::TypeChecker;

impl TypeChecker {
    /// Extract `schema_is(x, S)` / `is_type(x, S)` refinements for a bare
    /// variable or reference path.
    pub(in crate::typechecker) fn extract_schema_refinements(
        &self,
        args: &[SNode],
        scope: &TypeScope,
    ) -> Refinements {
        let Some(schema_type) = schema_type_expr_from_node(&args[1], scope) else {
            return Refinements::empty();
        };
        if let Node::Identifier(var_name) = &args[0].node {
            let Some(Some(var_type)) = scope.get_var(var_name).cloned() else {
                return Refinements::empty();
            };
            let truthy = intersect_types(&var_type, &schema_type)
                .map(|ty| vec![(var_name.clone(), Some(ty))])
                .unwrap_or_default();
            let falsy = subtract_type(&var_type, &schema_type)
                .map(|ty| vec![(var_name.clone(), Some(ty))])
                .unwrap_or_default();
            return Refinements {
                truthy,
                falsy,
                ..Refinements::default()
            };
        }
        if let Some(key) = reference_path_key(&args[0]) {
            return Refinements {
                truthy_paths: vec![(key.clone(), PathNarrowing::Intersect(schema_type.clone()))],
                falsy_paths: vec![(key, PathNarrowing::Subtract(schema_type))],
                ..Refinements::default()
            };
        }
        Refinements::empty()
    }

    pub(in crate::typechecker) fn extract_declared_predicate_refinements(
        &self,
        name: &str,
        args: &[SNode],
        type_args: &[TypeExpr],
        scope: &TypeScope,
    ) -> Refinements {
        let Some(signature) = scope.get_fn(name) else {
            return Refinements::empty();
        };
        let Some(predicate) = &signature.type_predicate else {
            return Refinements::empty();
        };
        let Some(parameter_index) = signature
            .params
            .iter()
            .position(|(parameter, _)| parameter == &predicate.parameter)
        else {
            return Refinements::empty();
        };
        let Some(subject) = args.get(parameter_index) else {
            return Refinements::empty();
        };
        let bindings = self.infer_function_call_type_bindings(signature, type_args, args, scope);
        let target = super::super::substitute_type_expr(&predicate.type_expr, &bindings);
        let target = self.resolve_alias(&target, scope);
        self.extract_subject_type_refinements(subject, &target, !predicate.one_sided, scope)
    }

    pub(in crate::typechecker) fn extract_namespace_predicate_refinements(
        &self,
        object: &SNode,
        member: &str,
        args: &[SNode],
        scope: &TypeScope,
    ) -> Refinements {
        let Node::Identifier(alias) = &object.node else {
            return Refinements::empty();
        };
        let Some(binding) = self.namespace_imports.get(alias) else {
            return Refinements::empty();
        };
        let Some(predicate) = binding.member_type_predicates.get(member) else {
            return Refinements::empty();
        };
        let Some(parameter_index) = binding
            .member_param_names
            .get(member)
            .and_then(|names| names.iter().position(|name| name == &predicate.parameter))
        else {
            return Refinements::empty();
        };
        let Some(subject) = args.get(parameter_index) else {
            return Refinements::empty();
        };
        self.extract_subject_type_refinements(
            subject,
            &predicate.type_expr,
            !predicate.one_sided,
            scope,
        )
    }

    /// Apply a validated predicate contract to one call argument. This is the
    /// same intersect/subtract operation used by `schema_is`, so predicates do
    /// not create a second narrowing model.
    fn extract_subject_type_refinements(
        &self,
        subject: &SNode,
        target: &TypeExpr,
        two_sided: bool,
        scope: &TypeScope,
    ) -> Refinements {
        if let Node::Identifier(var_name) = &subject.node {
            let Some(Some(var_type)) = scope.get_var(var_name).cloned() else {
                return Refinements::empty();
            };
            let current = self.resolve_alias(&var_type, scope);
            let truthy = intersect_types(&current, target)
                .map(|ty| vec![(var_name.clone(), Some(ty))])
                .unwrap_or_default();
            let falsy = if two_sided {
                subtract_type(&current, target)
                    .map(|ty| vec![(var_name.clone(), Some(ty))])
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            return Refinements {
                truthy,
                falsy,
                ..Refinements::default()
            };
        }
        if let Some(key) = reference_path_key(subject) {
            return Refinements {
                truthy_paths: vec![(key.clone(), PathNarrowing::Intersect(target.clone()))],
                falsy_paths: if two_sided {
                    vec![(key, PathNarrowing::Subtract(target.clone()))]
                } else {
                    Vec::new()
                },
                ..Refinements::default()
            };
        }
        Refinements::empty()
    }
}
