//! The new function's header and body, and the call that replaces each
//! copy of the region.

use super::*;

#[derive(Clone, Debug)]
pub(super) struct Param {
    pub(super) name: String,
    /// Declared type text, empty when untyped.
    pub(super) ty: String,
}

#[derive(Clone, Debug)]
pub(super) struct Header {
    pub(super) name: String,
    pub(super) params: Vec<Param>,
    pub(super) has_return: bool,
    /// The header text without its body opener.
    pub(super) text: String,
}

/// Parse a signature (`header_only`) or a whole helper into a [`Header`].
pub(super) fn parse_function(
    dialect: Dialect,
    language: Language,
    raw: &str,
    header_only: bool,
) -> Result<Header, Box<Refusal>> {
    let label = if header_only { "signature" } else { "helper" };
    let trimmed = raw.trim();
    let header_text = if header_only {
        match dialect {
            Dialect::Python => trimmed.trim_end_matches(':').trim_end(),
            _ => trimmed.trim_end_matches('{').trim_end(),
        }
        .to_string()
    } else {
        String::new()
    };
    let probe = match (header_only, dialect) {
        (true, Dialect::Python) => format!("{header_text}:\n    pass\n"),
        (true, _) => format!("{header_text} {{}}\n"),
        (false, _) => format!("{trimmed}\n"),
    };
    let malformed = || {
        Refusal::new(
            "signature_mismatch",
            format!(
                "`{label}` does not parse as one {} function",
                dialect_name(dialect)
            ),
        )
    };
    let tree = ast_api::parse_tree(&probe, language).map_err(|_| malformed())?;
    let root = tree.root_node();
    if root.has_error() {
        return Err(malformed());
    }
    let items: Vec<Node<'_>> = named_children(root)
        .into_iter()
        .filter(|n| !dialect.syntax().comments.contains(&n.kind()) && n.kind() != "attribute_item")
        .collect();
    let [item] = items.as_slice() else {
        return Err(malformed());
    };
    let function = unwrap_function(*item, dialect).ok_or_else(malformed)?;
    let name = function
        .child_by_field_name("name")
        .map(|n| text(n, &probe).to_string())
        .ok_or_else(malformed)?;
    let params_node = function
        .child_by_field_name("parameters")
        .ok_or_else(malformed)?;
    let mut params = Vec::new();
    for param in named_children(params_node) {
        if dialect.syntax().comments.contains(&param.kind()) {
            continue;
        }
        let (name_node, ty) = param_parts(param, dialect, &probe).ok_or_else(|| {
            Refusal::new(
                "signature_mismatch",
                format!(
                    "`{label}` parameter `{}` is not a plain name; extraction passes values by name",
                    text(param, &probe)
                ),
            )
        })?;
        params.push(Param {
            name: name_node,
            ty,
        });
    }
    Ok(Header {
        name,
        params,
        has_return: function.child_by_field_name("return_type").is_some(),
        text: header_text,
    })
}

fn unwrap_function(node: Node<'_>, dialect: Dialect) -> Option<Node<'_>> {
    let wanted: &[&str] = match dialect {
        Dialect::Rust => &["function_item"],
        Dialect::Python => &["function_definition"],
        _ => &["function_declaration"],
    };
    if wanted.contains(&node.kind()) {
        return Some(node);
    }
    if matches!(node.kind(), "export_statement" | "decorated_definition") {
        return named_children(node)
            .into_iter()
            .find(|c| wanted.contains(&c.kind()));
    }
    None
}

/// `(name, type text)` of one parameter, or `None` when it is a pattern.
fn param_parts(param: Node<'_>, dialect: Dialect, source: &str) -> Option<(String, String)> {
    let ty = param
        .child_by_field_name("type")
        .map(|t| text(t, source).trim_start_matches(':').trim().to_string())
        .unwrap_or_default();
    let name = match (dialect, param.kind()) {
        (_, "identifier") => Some(param),
        (Dialect::Rust, "parameter") => param
            .child_by_field_name("pattern")
            .filter(|p| p.kind() == "identifier"),
        (Dialect::Python, "typed_parameter") => {
            param.named_child(0).filter(|p| p.kind() == "identifier")
        }
        (Dialect::Python, "default_parameter" | "typed_default_parameter") => {
            param.child_by_field_name("name")
        }
        (_, "required_parameter" | "optional_parameter") => param
            .child_by_field_name("pattern")
            .filter(|p| p.kind() == "identifier"),
        (_, "assignment_pattern") => param
            .child_by_field_name("left")
            .filter(|p| p.kind() == "identifier"),
        _ => None,
    }?;
    Some((text(name, source).to_string(), ty))
}

pub(super) fn check_header(
    dialect: Dialect,
    header: &Header,
    new_name: &str,
    summary: &FlowSummary,
) -> Result<(), Box<Refusal>> {
    let refuse =
        |details: String| Err(Refusal::new("signature_mismatch", details).with_flow(summary));
    if header.name != new_name {
        return refuse(format!(
            "the function is named `{}` but new_name is `{new_name}`",
            header.name
        ));
    }
    let declared: HashSet<&str> = header.params.iter().map(|p| p.name.as_str()).collect();
    let computed: HashSet<&str> = summary.inputs.iter().map(String::as_str).collect();
    if declared != computed || declared.len() != header.params.len() {
        let names: Vec<String> = header.params.iter().map(|p| p.name.clone()).collect();
        return refuse(format!(
            "parameters {} do not match the region's inputs {}",
            display_list(&names),
            display_list(&summary.inputs)
        ));
    }
    if dialect == Dialect::Rust && summary.returns_value != header.has_return {
        return refuse(if summary.returns_value {
            "the region produces a value but the signature declares no return type".into()
        } else {
            "the signature declares a return type but the region produces no value".into()
        });
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) fn synthesize(
    dialect: Dialect,
    source: &str,
    target: &Target<'_>,
    flow: &Flow,
    header: Option<&Header>,
    name: &str,
    base: &str,
    body_indent: &str,
    semicolons: bool,
    syn: &Syntax,
) -> String {
    let semi = if semicolons && dialect != Dialect::Rust {
        ";"
    } else {
        ""
    };
    let (body_start, body_end, value_expr) = match target.shape {
        Shape::Expression => (target.start, target.end, true),
        Shape::Statements { .. } => (target.outer_start(), target.outer_end(), false),
        Shape::ClosureBody => {
            let body = target.closure_body().expect("closure target has a body");
            if syn.blocks.contains(&body.kind()) {
                let inner = named_children(body);
                match (inner.first(), inner.last()) {
                    (Some(first), Some(last)) => (first.start_byte(), last.end_byte(), false),
                    _ => (body.start_byte() + 1, body.start_byte() + 1, false),
                }
            } else {
                (body.start_byte(), body.end_byte(), true)
            }
        }
    };
    let original_indent = line_indent(source, body_start);
    let raw = slice(source, body_start, body_end);
    let mut body = if value_expr && dialect != Dialect::Rust {
        format!("return {raw}{semi}")
    } else {
        raw.to_string()
    };
    if !flow.outputs.is_empty() {
        let returned = match (dialect, flow.outputs.as_slice()) {
            (_, [one]) => one.clone(),
            (Dialect::Rust, many) => format!("({})", many.join(", ")),
            (Dialect::Python, many) => many.join(", "),
            (_, many) => format!("{{ {} }}", many.join(", ")),
        };
        body.push('\n');
        body.push_str(&original_indent);
        body.push_str(&match dialect {
            Dialect::Rust => returned,
            _ => format!("return {returned}{semi}"),
        });
    }
    let body = reindent(&body, &original_indent, body_indent);
    let head = match header {
        Some(header) => header.text.clone(),
        None => {
            let params = flow.inputs.join(", ");
            match dialect {
                Dialect::Rust => format!("fn {name}()"),
                Dialect::Python => format!("def {name}({params})"),
                _ => format!("function {name}({params})"),
            }
        }
    };
    match dialect {
        Dialect::Python => format!("{base}{head}:\n{body}"),
        _ => format!("{base}{head} {{\n{body}\n{base}}}"),
    }
}

pub(super) fn call_for(
    dialect: Dialect,
    target: &Target<'_>,
    flow: &Flow,
    name: &str,
    params: &[Param],
    semicolons: bool,
) -> String {
    let args: Vec<String> = params
        .iter()
        .map(|p| {
            if dialect == Dialect::Rust
                && p.ty.starts_with("&mut")
                && flow.mut_locals.contains(&p.name)
            {
                format!("&mut {}", p.name)
            } else {
                p.name.clone()
            }
        })
        .collect();
    let call = format!("{name}({})", args.join(", "));
    let semi = if semicolons { ";" } else { "" };
    match target.shape {
        Shape::Expression | Shape::ClosureBody => call,
        Shape::Statements { tail: true } => call,
        Shape::Statements { tail: false } => {
            let outputs = &flow.outputs;
            if outputs.is_empty() {
                return match dialect {
                    Dialect::Python => call,
                    _ => format!("{call}{semi}"),
                };
            }
            let declared = !flow.declared.is_empty();
            match dialect {
                Dialect::Python => format!("{} = {call}", outputs.join(", ")),
                Dialect::Rust => {
                    let bind = |o: &String| {
                        if declared && flow.mutable.contains(o) {
                            format!("mut {o}")
                        } else {
                            o.clone()
                        }
                    };
                    let pattern = match outputs.as_slice() {
                        [one] => bind(one),
                        many => {
                            format!("({})", many.iter().map(bind).collect::<Vec<_>>().join(", "))
                        }
                    };
                    if declared {
                        format!("let {pattern} = {call};")
                    } else {
                        format!("{pattern} = {call};")
                    }
                }
                Dialect::TypeScript | Dialect::JavaScript => match (outputs.as_slice(), declared) {
                    ([one], true) => format!("{} {one} = {call}{semi}", flow.keyword),
                    ([one], false) => format!("{one} = {call}{semi}"),
                    (many, true) => {
                        format!("{} {{ {} }} = {call}{semi}", flow.keyword, many.join(", "))
                    }
                    (many, false) => format!("({{ {} }} = {call}){semi}", many.join(", ")),
                },
            }
        }
    }
}
