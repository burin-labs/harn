//! Structural discovery and validation for user-test fixtures.

use std::collections::BTreeMap;

use harn_lexer::Span;
use harn_parser::{Attribute, Node, SNode};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FixtureScope {
    File,
    Case,
}

#[derive(Clone, Debug)]
pub(super) struct TestFixture {
    pub(super) name: String,
    pub(super) scope: FixtureScope,
}

pub(super) type FixtureRegistry = BTreeMap<String, TestFixture>;

pub(super) fn discover(program: &[SNode]) -> Result<FixtureRegistry, String> {
    let mut fixtures = FixtureRegistry::new();
    for declaration in program {
        let Node::AttributedDecl { attributes, inner } = &declaration.node else {
            continue;
        };
        let fixture_attributes = attributes
            .iter()
            .filter(|attribute| attribute.name == "test_fixture")
            .collect::<Vec<_>>();
        let Some(attribute) = fixture_attributes.first().copied() else {
            continue;
        };
        if fixture_attributes.len() > 1 {
            return Err(at(
                fixture_attributes[1].span,
                "`@test_fixture` must not be repeated on one declaration",
            ));
        }
        let Node::FnDecl {
            name,
            params,
            return_type,
            is_stream,
            ..
        } = &inner.node
        else {
            return Err(at(
                attribute.span,
                "`@test_fixture` only applies to function declarations",
            ));
        };
        if !params.is_empty() {
            return Err(at(
                inner.span,
                &format!("test fixture `{name}` must not declare parameters"),
            ));
        }
        if return_type.is_none() {
            return Err(at(
                inner.span,
                &format!("test fixture `{name}` must declare an explicit return type"),
            ));
        }
        if *is_stream {
            return Err(at(
                inner.span,
                &format!("test fixture `{name}` cannot be a stream function"),
            ));
        }
        let scope = parse_scope(attribute)?;
        if fixtures
            .insert(
                name.clone(),
                TestFixture {
                    name: name.clone(),
                    scope,
                },
            )
            .is_some()
        {
            return Err(at(
                inner.span,
                &format!("duplicate test fixture declaration `{name}`"),
            ));
        }
    }
    Ok(fixtures)
}

pub(super) fn referenced_by(
    attribute: &Attribute,
    fixtures: &FixtureRegistry,
) -> Result<Option<TestFixture>, String> {
    let Some(node) = attribute.named_arg("fixture") else {
        return Ok(None);
    };
    let name = match &node.node {
        Node::Identifier(name) | Node::StringLiteral(name) | Node::RawStringLiteral(name) => name,
        _ => {
            return Err(at(
                node.span,
                "`@test(fixture: ...)` must name a `@test_fixture` function",
            ))
        }
    };
    fixtures.get(name).cloned().map(Some).ok_or_else(|| {
        at(
            node.span,
            &format!("`@test` references unknown test fixture `{name}`"),
        )
    })
}

fn parse_scope(attribute: &Attribute) -> Result<FixtureScope, String> {
    if attribute.args.len() != 1 {
        return Err(at(
            attribute.span,
            "`@test_fixture` requires exactly one `scope: file|case` argument",
        ));
    }
    let Some(scope) = attribute.named_arg("scope") else {
        return Err(at(
            attribute.span,
            "`@test_fixture` requires named argument `scope: file|case`",
        ));
    };
    let value = match &scope.node {
        Node::Identifier(value) | Node::StringLiteral(value) | Node::RawStringLiteral(value) => {
            value.as_str()
        }
        _ => {
            return Err(at(
                scope.span,
                "`@test_fixture(scope: ...)` must be `file` or `case`",
            ))
        }
    };
    match value {
        "file" => Ok(FixtureScope::File),
        "case" => Ok(FixtureScope::Case),
        _ => Err(at(
            scope.span,
            "`@test_fixture(scope: ...)` must be `file` or `case`",
        )),
    }
}

pub(super) fn at(span: Span, message: &str) -> String {
    format!("{}:{}: {message}", span.line, span.column)
}
