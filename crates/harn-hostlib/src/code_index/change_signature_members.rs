//! Planning the other members of an override family for
//! [`super::plan`]: each member's header and body, projected from the
//! seed's new parameter list.

use std::collections::BTreeMap;
use std::path::Path;

use crate::ast::{api as ast_api, Language};
use crate::error::HostlibError;

use super::super::refactor_core::{first_syntax_error, read_source};
use super::super::signature_syntax::{find_declaration, read_declaration, Declaration, Family};
use super::super::state::IndexState;
use super::{
    check_default_order, header_items, plan_body, EditContent, NewParam, PendingEdit, Refusal,
    Site, BUILTIN,
};

/// Plan one other member of the seed's override family: its header, and
/// its body's uses of removed or renamed parameters.
pub(super) fn plan_member(
    state: &IndexState,
    session: Option<&str>,
    name: &str,
    (seed, seed_path): (&Declaration, &str),
    seed_params: &[NewParam],
    (path, line): (&str, u32),
    edits: &mut BTreeMap<String, Vec<PendingEdit>>,
) -> Result<Result<(), Refusal>, HostlibError> {
    let at = |kind: &'static str, reason: String| Site {
        path: path.to_string(),
        line: line as usize,
        kind,
        reason,
        text: String::new(),
    };
    let language = Language::detect(Path::new(path), None).expect("family members parse");
    let source = read_source(BUILTIN, &state.root, path, session)?;
    if let Some(detail) = first_syntax_error(&source, language) {
        return Ok(Err(Refusal::new(
            "syntax_error",
            format!("`{path}` does not parse before the edit ({detail}); fix it first"),
        )));
    }
    let tree = match ast_api::parse_tree(&source, language) {
        Ok(tree) => tree,
        Err(err) => {
            return Ok(Err(Refusal::new(
                "syntax_error",
                format!("`{path}` failed to parse: {err}"),
            )));
        }
    };
    let root = tree.root_node();
    let Some(node) = find_declaration(root, source.as_bytes(), seed.family, name, line) else {
        return Ok(Err(Refusal::new(
            "error",
            format!("no declaration parses at {path}:{line}; rebuild the index"),
        )));
    };
    let member = match read_declaration(node, &source, seed.family) {
        Ok(member) => member,
        Err(reason) => {
            return Ok(Err(Refusal::new(
                "error",
                format!("cannot rewrite the parameters of the override at {path}:{line}: {reason}"),
            )
            .with_sites(vec![at("declaration", reason)])));
        }
    };
    if path == seed_path && member.params_range == seed.params_range {
        return Ok(Ok(()));
    }
    if let Some(reason) = &member.fixed {
        return Ok(Err(Refusal::new(
            "fixed_signature",
            format!("the override at {path}:{line} {reason}, which fixes its parameter list"),
        )
        .with_sites(vec![at("declaration", reason.clone())])));
    }
    let params = match member_params(seed, &member, seed_params)
        .and_then(|params| check_default_order(&member, &params).map(|()| params))
    {
        Ok(params) => params,
        Err(reason) => {
            return Ok(Err(Refusal::new(
                "overrides_present",
                format!(
                    "the override at {path}:{line} cannot take the new parameter list: {reason}; change it by hand"
                ),
            )
            .with_sites(vec![at("declaration", reason)])));
        }
    };
    let file_edits = edits.entry(path.to_string()).or_default();
    if let Err(refusal) = plan_body(root, &source, path, &member, &params, file_edits) {
        return Ok(Err(refusal));
    }
    let items = header_items(&member, &params);
    file_edits.push(PendingEdit {
        range: member.params_range.clone(),
        start: member.params_start,
        end: member.params_end,
        content: EditContent::Literal(member.layout.render(&items)),
    });
    Ok(Ok(()))
}

/// The seed's new parameter list projected onto another family member.
/// Kept parameters keep the member's own name unless it matches the seed's
/// name, which the request may rename.
fn member_params(
    seed: &Declaration,
    member: &Declaration,
    params: &[NewParam],
) -> Result<Vec<NewParam>, String> {
    if member.receiver != seed.receiver {
        return Err("its receiver differs from the seed's".into());
    }
    if member.params.len() != seed.params.len() {
        return Err(format!(
            "it declares {} parameter(s) where the seed declares {}",
            member.params.len(),
            seed.params.len()
        ));
    }
    for (own, theirs) in member.params.iter().zip(&seed.params) {
        if own.shape != theirs.shape {
            return Err(format!("parameter `{}` has a different shape", own.name));
        }
        if seed.family == Family::Python && own.name != theirs.name {
            return Err(format!(
                "it names parameter `{}` `{}`, and keyword calls pass it by name",
                theirs.name, own.name
            ));
        }
    }
    let projected: Vec<NewParam> = params
        .iter()
        .map(|param| {
            let mut param = param.clone();
            if let Some(index) = param.source {
                let own = &member.params[index].name;
                if *own != seed.params[index].name {
                    param.spec.name = own.clone();
                }
            }
            param
        })
        .collect();
    let mut names = std::collections::HashSet::new();
    if let Some(dup) = projected
        .iter()
        .find(|p| !names.insert(p.spec.name.as_str()))
    {
        return Err(format!("it would declare `{}` twice", dup.spec.name));
    }
    Ok(projected)
}
