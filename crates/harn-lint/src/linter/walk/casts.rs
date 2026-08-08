//! Conservative classification for redundant builtin conversion calls.
//!
//! Returning `None` is intentional for conversions whose source type is not
//! statically certain, including `to_int("42")` and wrong-arity calls.

use harn_parser::{Node, SNode};

/// Return the target type when a one-argument conversion is statically redundant.
pub(super) fn unnecessary_cast_target(name: &str, args: &[SNode]) -> Option<&'static str> {
    if args.len() != 1 {
        return None;
    }
    let target = match name {
        "to_string" => "string",
        "to_int" => "int",
        "to_float" => "float",
        "to_list" => "list",
        "to_dict" => "dict",
        _ => return None,
    };
    expr_has_known_type(&args[0].node, name).then_some(target)
}

/// Recognize only literal shapes and nested calls whose result type is certain.
fn expr_has_known_type(node: &Node, cast: &str) -> bool {
    if let Node::FunctionCall {
        name: inner_name,
        args: inner_args,
        ..
    } = node
    {
        if inner_name == cast && inner_args.len() == 1 {
            return true;
        }
    }
    matches!(
        (cast, node),
        (
            "to_string",
            Node::StringLiteral(_) | Node::RawStringLiteral(_) | Node::InterpolatedString(_),
        ) | ("to_int", Node::IntLiteral(_))
            | ("to_float", Node::FloatLiteral(_))
            | ("to_list", Node::ListLiteral(_))
            | ("to_dict", Node::DictLiteral(_))
    )
}
