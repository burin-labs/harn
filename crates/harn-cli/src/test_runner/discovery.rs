//! Test-pipeline, table-row, and scheduler-metadata discovery.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use harn_lexer::Lexer;
use harn_parser::const_eval::{const_eval, ConstEnv, ConstValue};
use harn_parser::{Attribute, Node, Parser, SNode};
use harn_vm::VmValue;

use super::{fixtures, TestCase, TestFixture};

pub(super) fn parse_program(source: &str) -> Result<Vec<SNode>, String> {
    let mut lexer = Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|error| format!("{error}"))?;
    Parser::new(tokens)
        .parse()
        .map_err(|error| format!("{error}"))
}

pub(super) fn extract_cases_from_program(
    file: &Path,
    source: &Arc<String>,
    program: &Arc<Vec<SNode>>,
    filter: Option<&str>,
    workers: usize,
) -> Result<Vec<TestCase>, String> {
    let fixtures = fixtures::discover(program)?;
    let mut cases = Vec::new();
    for snode in program.iter() {
        let Some(meta) = inspect_test_pipeline(snode, &fixtures)? else {
            continue;
        };
        let weight = meta.weight.min(workers).max(1);
        if meta.rows.is_empty() {
            if filter.is_some_and(|pattern| !meta.name.contains(pattern)) {
                continue;
            }
            cases.push(TestCase {
                file: file.to_path_buf(),
                name: meta.name.clone(),
                pipeline_name: meta.name,
                source: Arc::clone(source),
                program: Arc::clone(program),
                imported_enum_candidates: Arc::new(Vec::new()),
                serial_group: meta.serial_group,
                weight,
                args: meta.default_args,
                fixture: meta.fixture,
                file_fixture_value: None,
            });
        } else {
            for row in meta.rows {
                let case_name = format!("{}[{}]", meta.name, row.name);
                if filter.is_some_and(|pattern| !case_name.contains(pattern)) {
                    continue;
                }
                cases.push(TestCase {
                    file: file.to_path_buf(),
                    name: case_name,
                    pipeline_name: meta.name.clone(),
                    source: Arc::clone(source),
                    program: Arc::clone(program),
                    imported_enum_candidates: Arc::new(Vec::new()),
                    serial_group: meta.serial_group.clone(),
                    weight,
                    args: row.args,
                    fixture: meta.fixture.clone(),
                    file_fixture_value: None,
                });
            }
        }
    }
    Ok(cases)
}

pub(super) fn seed_imported_enum_candidates(file: &Path, source: &str, cases: &mut [TestCase]) {
    if cases.is_empty() || !harn_parser::visit::contains_identifier_enum_pattern(&cases[0].program)
    {
        return;
    }
    let mut candidates = harn_modules::build_with_source(file, source)
        .imported_names_by_kind_for_file(file, harn_modules::DefKind::Enum)
        .unwrap_or_default()
        .into_iter()
        .collect::<Vec<_>>();
    candidates.sort_unstable();
    let candidates = Arc::new(candidates);
    for case in cases {
        case.imported_enum_candidates = Arc::clone(&candidates);
    }
}

struct PipelineMeta {
    name: String,
    serial_group: Option<String>,
    weight: usize,
    rows: Vec<ParameterizedRow>,
    default_args: Vec<VmValue>,
    fixture: Option<TestFixture>,
}

struct ParameterizedRow {
    name: String,
    args: Vec<VmValue>,
}

fn inspect_test_pipeline(
    snode: &SNode,
    fixtures: &fixtures::FixtureRegistry,
) -> Result<Option<PipelineMeta>, String> {
    let (attributes, inner) = match &snode.node {
        Node::AttributedDecl { attributes, inner } => (attributes.as_slice(), inner.as_ref()),
        _ => (&[][..], snode),
    };
    let (name, params) = match &inner.node {
        Node::Pipeline { name, params, .. } => (name.clone(), params),
        _ => return Ok(None),
    };
    let test_attributes = attributes
        .iter()
        .filter(|attribute| attribute.name == "test")
        .collect::<Vec<_>>();
    if test_attributes.len() > 1 {
        return Err(fixtures::at(
            test_attributes[1].span,
            &format!("test pipeline `{name}` must not repeat `@test`"),
        ));
    }
    let has_test_attr = !test_attributes.is_empty();
    if !has_test_attr && !name.starts_with("test_") {
        return Ok(None);
    }
    let serial_group = attributes
        .iter()
        .find(|attribute| attribute.name == "serial")
        .map(serial_group_for);
    let weight = attributes
        .iter()
        .find(|attribute| attribute.name == "heavy")
        .and_then(heavy_weight_for)
        .unwrap_or(1);
    let test_attribute = test_attributes.first().copied();
    let fixture = match test_attribute {
        Some(attribute) => {
            validate_test_attribute(attribute, &name)?;
            fixtures::referenced_by(attribute, fixtures)?
        }
        None => None,
    };
    let explicit_argument_count = params
        .len()
        .checked_sub(usize::from(fixture.is_some()))
        .ok_or_else(|| {
            fixtures::at(
                inner.span,
                &format!(
                    "test pipeline `{name}` must declare the fixture value as its first parameter"
                ),
            )
        })?;
    let rows = match test_attribute {
        Some(attribute) => parameterized_rows(attribute, &name, explicit_argument_count)?,
        None => Vec::new(),
    };
    if rows.is_empty() && fixture.is_some() && explicit_argument_count != 0 {
        return Err(fixtures::at(
            inner.span,
            &format!(
                "test pipeline `{name}` declares {explicit_argument_count} non-fixture parameters but has no `@test(cases: [...])` rows"
            ),
        ));
    }
    Ok(Some(PipelineMeta {
        name,
        serial_group,
        weight,
        rows,
        // Legacy `test_*` declarations commonly retain an ignored `task`
        // parameter. Preserve that contract while still invoking the callable
        // through its ordinary arity path.
        default_args: vec![VmValue::Nil; explicit_argument_count],
        fixture,
    }))
}

fn validate_test_attribute(attribute: &Attribute, pipeline_name: &str) -> Result<(), String> {
    let mut names = HashSet::new();
    for argument in &attribute.args {
        let Some(name) = argument.name.as_deref() else {
            return Err(fixtures::at(
                argument.span,
                &format!(
                    "`@test` arguments for `{pipeline_name}` must be named `cases` or `fixture`"
                ),
            ));
        };
        if !matches!(name, "cases" | "fixture") {
            return Err(fixtures::at(
                argument.span,
                &format!(
                    "unknown `@test` argument `{name}` for `{pipeline_name}`; expected `cases` or `fixture`"
                ),
            ));
        }
        if !names.insert(name) {
            return Err(fixtures::at(
                argument.span,
                &format!("duplicate `@test` argument `{name}` for `{pipeline_name}`"),
            ));
        }
    }
    Ok(())
}

fn parameterized_rows(
    attribute: &Attribute,
    pipeline_name: &str,
    parameter_count: usize,
) -> Result<Vec<ParameterizedRow>, String> {
    let Some(cases) = attribute.named_arg("cases") else {
        return Ok(Vec::new());
    };
    let Node::ListLiteral(items) = &cases.node else {
        return Err(fixtures::at(
            cases.span,
            &format!("@test cases for `{pipeline_name}` must be a list of {{name, args}} rows"),
        ));
    };
    if items.is_empty() {
        return Err(fixtures::at(
            cases.span,
            &format!("@test cases for `{pipeline_name}` must not be empty"),
        ));
    }

    let mut rows = Vec::with_capacity(items.len());
    let mut names = HashSet::new();
    for item in items {
        let Node::DictLiteral(entries) = &item.node else {
            return Err(fixtures::at(
                item.span,
                &format!("@test case in `{pipeline_name}` must be a {{name, args}} dict"),
            ));
        };
        let mut fields = HashSet::new();
        for entry in entries {
            let field = match &entry.key.node {
                Node::Identifier(value) | Node::StringLiteral(value) => value.as_str(),
                _ => {
                    return Err(fixtures::at(
                        entry.key.span,
                        &format!("@test case fields in `{pipeline_name}` must be `name` or `args`"),
                    ))
                }
            };
            if !matches!(field, "name" | "args") {
                return Err(fixtures::at(
                    entry.key.span,
                    &format!(
                        "unknown @test case field `{field}` in `{pipeline_name}`; expected `name` or `args`"
                    ),
                ));
            }
            if !fields.insert(field) {
                return Err(fixtures::at(
                    entry.key.span,
                    &format!("duplicate @test case field `{field}` in `{pipeline_name}`"),
                ));
            }
        }
        let name_node = dict_entry(entries, "name").ok_or_else(|| {
            fixtures::at(
                item.span,
                &format!("@test case in `{pipeline_name}` is missing string field `name`"),
            )
        })?;
        let name = match &name_node.node {
            Node::StringLiteral(value) | Node::RawStringLiteral(value) => value.trim().to_string(),
            _ => {
                return Err(fixtures::at(
                    name_node.span,
                    &format!("@test case name in `{pipeline_name}` must be a string literal"),
                ))
            }
        };
        if name.is_empty() || !names.insert(name.clone()) {
            return Err(fixtures::at(
                name_node.span,
                &format!(
                    "@test case names in `{pipeline_name}` must be non-empty and unique: `{name}`"
                ),
            ));
        }
        let args_node = dict_entry(entries, "args").ok_or_else(|| {
            fixtures::at(
                item.span,
                &format!("@test case `{name}` in `{pipeline_name}` is missing list field `args`"),
            )
        })?;
        let Node::ListLiteral(args) = &args_node.node else {
            return Err(fixtures::at(
                args_node.span,
                &format!("@test case `{name}` in `{pipeline_name}` must provide `args` as a list"),
            ));
        };
        if args.len() != parameter_count {
            return Err(fixtures::at(
                args_node.span,
                &format!(
                    "@test case `{name}` in `{pipeline_name}` has {} arguments; expected {parameter_count}",
                    args.len()
                ),
            ));
        }
        let args = args
            .iter()
            .map(attribute_value)
            .collect::<Result<Vec<_>, _>>()?;
        rows.push(ParameterizedRow { name, args });
    }
    Ok(rows)
}

fn dict_entry<'a>(entries: &'a [harn_parser::DictEntry], key: &str) -> Option<&'a SNode> {
    entries.iter().find_map(|entry| {
        let matches = match &entry.key.node {
            Node::Identifier(value) | Node::StringLiteral(value) => value == key,
            _ => false,
        };
        matches.then_some(&entry.value)
    })
}

fn attribute_value(node: &SNode) -> Result<VmValue, String> {
    let value = const_eval(node, &ConstEnv::new()).map_err(|error| {
        fixtures::at(
            node.span,
            &format!("@test case arguments must be compile-time values: {error:?}"),
        )
    })?;
    Ok(const_value_to_vm(value))
}

fn const_value_to_vm(value: ConstValue) -> VmValue {
    match value {
        ConstValue::Int(value) => VmValue::Int(value),
        ConstValue::Float(value) => VmValue::Float(value),
        ConstValue::Bool(value) => VmValue::Bool(value),
        ConstValue::String(value) => VmValue::String(value.into()),
        ConstValue::Nil => VmValue::Nil,
        ConstValue::List(items) => {
            VmValue::List(Arc::new(items.into_iter().map(const_value_to_vm).collect()))
        }
        ConstValue::Dict(entries) => VmValue::dict(
            entries
                .into_iter()
                .map(|(key, value)| (key, const_value_to_vm(value)))
                .collect::<Vec<(String, VmValue)>>(),
        ),
    }
}

fn serial_group_for(attribute: &Attribute) -> String {
    attribute
        .string_arg("group")
        .unwrap_or_else(|| "__default__".to_string())
}

fn heavy_weight_for(attribute: &Attribute) -> Option<usize> {
    attribute
        .args
        .iter()
        .find(|argument| argument.name.as_deref() == Some("threads"))
        .and_then(|argument| match &argument.value.node {
            Node::IntLiteral(value) if *value >= 1 => Some(*value as usize),
            _ => None,
        })
}
