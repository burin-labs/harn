use harn_parser::{
    format_type, TypeExpr, TypeParam, TypePredicate, TypedParam, Variance, WhereClause,
};

pub(super) fn fn_signature(
    keyword: &str,
    name: &str,
    type_params: &[TypeParam],
    params: &[TypedParam],
    return_type: Option<&TypeExpr>,
    type_predicate: Option<&TypePredicate>,
    where_clauses: &[WhereClause],
) -> String {
    if let Some(predicate) = type_predicate {
        let params = params
            .iter()
            .map(format_param)
            .collect::<Vec<_>>()
            .join(", ");
        let implies = if predicate.one_sided { "implies " } else { "" };
        return format!(
            "{} {}{}({}) -> {implies}{} is {}{}",
            keyword,
            name,
            format_type_params(type_params),
            params,
            predicate.parameter,
            format_type(&predicate.type_expr),
            format_where_clauses(where_clauses)
        );
    }
    callable_signature(
        keyword,
        name,
        type_params,
        params,
        return_type,
        where_clauses,
    )
}

pub(super) fn callable_signature(
    keyword: &str,
    name: &str,
    type_params: &[TypeParam],
    params: &[TypedParam],
    return_type: Option<&TypeExpr>,
    where_clauses: &[WhereClause],
) -> String {
    let params = params
        .iter()
        .map(format_param)
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "{} {}{}({}){}{}",
        keyword,
        name,
        format_type_params(type_params),
        params,
        return_type
            .map(|ty| format!(" -> {}", format_type(ty)))
            .unwrap_or_default(),
        format_where_clauses(where_clauses)
    )
}

fn format_param(param: &TypedParam) -> String {
    let rest = if param.rest { "..." } else { "" };
    match &param.type_expr {
        Some(ty) => format!("{rest}{}: {}", param.name, format_type(ty)),
        None => format!("{rest}{}", param.name),
    }
}

pub(super) fn format_type_params(params: &[TypeParam]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let params = params
        .iter()
        .map(|param| match param.variance {
            Variance::Invariant => param.name.clone(),
            Variance::Covariant => format!("out {}", param.name),
            Variance::Contravariant => format!("in {}", param.name),
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("<{params}>")
}

fn format_where_clauses(clauses: &[WhereClause]) -> String {
    if clauses.is_empty() {
        return String::new();
    }
    let clauses = clauses
        .iter()
        .map(|clause| format!("{}: {}", clause.type_name, format_type(&clause.bound)))
        .collect::<Vec<_>>()
        .join(", ");
    format!(" where {clauses}")
}
