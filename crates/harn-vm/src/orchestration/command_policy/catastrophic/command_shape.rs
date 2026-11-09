//! The operator structure of a shell command line: which commands run, how
//! they are chained (`&&`, `||`, `;`, `&`, newline), and which stages each
//! pipeline holds.
//!
//! The command-policy analyzer flattens a program into the commands it may
//! execute and drops the operators between them. Callers that need to know
//! whether a line is a single command, which stage ends a pipeline, or how a
//! chain short-circuits read this shape instead, built from the same grammars.
//! Quoted operators stay argument text. A line the grammar cannot parse, and
//! every `cmd` line, is reported as unmeasured rather than as a single command.

use tree_sitter::Node;

use super::semantic_shell::lex_powershell_words;
use crate::shells::ShellDialect;

/// One pipeline stage. `kind` is `command` for a simple command (optionally
/// with redirects) and `compound` for anything else, such as a subshell or an
/// `if` block, whose words are not an argv.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShellShapeStage {
    pub(crate) kind: &'static str,
    pub(crate) text: String,
    pub(crate) words: Vec<String>,
    /// Where the stage starts in the source line.
    pub(crate) start_byte: usize,
}

/// One chain member: a pipeline of one or more stages, and the operator that
/// joins it to the member before it (empty for the first member).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShellShapeSegment {
    pub(crate) operator: &'static str,
    pub(crate) stages: Vec<ShellShapeStage>,
}

/// `segments` is `None` when the line was not measured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ShellCommandShape {
    pub(crate) segments: Option<Vec<ShellShapeSegment>>,
}

pub(crate) fn shell_command_shape(dialect: ShellDialect, source: &str) -> ShellCommandShape {
    let segments = match dialect {
        ShellDialect::Posix => posix_shape(source),
        ShellDialect::PowerShell => powershell_shape(source),
        ShellDialect::Cmd => None,
    };
    ShellCommandShape {
        segments: segments.filter(|segments| !segments.is_empty()),
    }
}

fn parse(language: tree_sitter::Language, source: &str) -> Option<tree_sitter::Tree> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&language).ok()?;
    let tree = parser.parse(source, None)?;
    (!tree.root_node().has_error()).then_some(tree)
}

fn text<'a>(node: Node<'_>, source: &'a str) -> &'a str {
    source.get(node.byte_range()).unwrap_or_default()
}

/// The separator written between two top-level statements.
fn separator(source: &str, previous: Node<'_>, next: Node<'_>) -> &'static str {
    let gap = source
        .get(previous.end_byte()..next.start_byte())
        .unwrap_or_default();
    if gap.contains(';') {
        ";"
    } else if gap.contains('&') {
        "&"
    } else {
        "\n"
    }
}

fn posix_shape(source: &str) -> Option<Vec<ShellShapeSegment>> {
    let tree = parse(harn_grammars::bash(), source)?;
    let root = tree.root_node();
    let mut segments = Vec::new();
    let mut previous: Option<Node<'_>> = None;
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        if child.kind() == "comment" {
            continue;
        }
        let operator = previous.map_or("", |previous| separator(source, previous, child));
        posix_flatten(child, operator, source, &mut segments)?;
        previous = Some(child);
    }
    Some(segments)
}

fn posix_flatten(
    node: Node<'_>,
    operator: &'static str,
    source: &str,
    segments: &mut Vec<ShellShapeSegment>,
) -> Option<()> {
    match node.kind() {
        "list" => {
            let mut pending = operator;
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.is_named() {
                    if child.kind() != "comment" {
                        posix_flatten(child, pending, source, segments)?;
                    }
                } else {
                    pending = match child.kind() {
                        "&&" => "&&",
                        "||" => "||",
                        _ => return None,
                    };
                }
            }
        }
        "pipeline" => {
            // `|` binds tighter than `&&`/`||`, but the grammar parses a chain
            // whose last member carries a redirect (`a || b 2>&1 | tail`) as a
            // pipeline whose first stage is the whole chain. Flatten that chain
            // and continue the pipeline on its last member.
            let start = segments.len();
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                match child.kind() {
                    "comment" => {}
                    "list" | "redirected_statement"
                        if segments.len() == start && redirected_list(child).is_some() =>
                    {
                        posix_flatten(child, operator, source, segments)?;
                    }
                    _ => {
                        let stage = posix_stage(child, source)?;
                        if segments.len() == start {
                            segments.push(ShellShapeSegment {
                                operator,
                                stages: vec![stage],
                            });
                        } else {
                            segments.last_mut()?.stages.push(stage);
                        }
                    }
                }
            }
        }
        "redirected_statement" if redirected_list(node).is_some_and(|list| list != node) => {
            // The grammar attaches a trailing redirect to the whole chain
            // (`a || b 2>&1`); in the shell it belongs to the last command.
            posix_flatten(redirected_list(node)?, operator, source, segments)?;
            let last = segments.last_mut()?.stages.last_mut()?;
            let extended = source
                .get(last.start_byte..node.end_byte())?
                .trim()
                .to_string();
            if last.kind == "command" {
                last.words = shell_words::split(&extended).ok()?;
            }
            last.text = extended;
        }
        _ => segments.push(ShellShapeSegment {
            operator,
            stages: vec![posix_stage(node, source)?],
        }),
    }
    Some(())
}

/// The `&&`/`||` chain a node holds: the node itself, or the body of a
/// redirect the grammar attached to a whole chain.
fn redirected_list(node: Node<'_>) -> Option<Node<'_>> {
    match node.kind() {
        "list" => Some(node),
        "redirected_statement" => node
            .child_by_field_name("body")
            .filter(|body| body.kind() == "list"),
        _ => None,
    }
}

fn posix_stage(node: Node<'_>, source: &str) -> Option<ShellShapeStage> {
    let simple = match node.kind() {
        "command" => true,
        "redirected_statement" => node
            .child_by_field_name("body")
            .is_some_and(|body| body.kind() == "command"),
        _ => false,
    };
    let text = text(node, source).trim().to_string();
    let words = if simple {
        shell_words::split(&text).ok()?
    } else {
        Vec::new()
    };
    Some(ShellShapeStage {
        kind: if simple { "command" } else { "compound" },
        text,
        words,
        start_byte: node.start_byte(),
    })
}

fn powershell_shape(source: &str) -> Option<Vec<ShellShapeSegment>> {
    let tree = parse(harn_grammars::powershell(), source)?;
    let mut segments = Vec::new();
    let mut previous: Option<Node<'_>> = None;
    let mut statements = Vec::new();
    collect_powershell_statements(tree.root_node(), &mut statements);
    for statement in statements {
        let operator = previous.map_or("", |previous| separator(source, previous, statement));
        if statement.kind() == "pipeline" {
            let mut pending = operator;
            let mut cursor = statement.walk();
            for child in statement.named_children(&mut cursor) {
                match child.kind() {
                    "pipeline_chain" => {
                        let mut stage_cursor = child.walk();
                        let stages = child
                            .named_children(&mut stage_cursor)
                            .filter(|stage| stage.kind() != "comment")
                            .map(|stage| powershell_stage(stage, source))
                            .collect::<Vec<_>>();
                        segments.push(ShellShapeSegment {
                            operator: pending,
                            stages,
                        });
                    }
                    "pipeline_chain_tail" => {
                        pending = match text(child, source).trim() {
                            "&&" => "&&",
                            "||" => "||",
                            _ => return None,
                        };
                    }
                    "comment" => {}
                    _ => return None,
                }
            }
        } else {
            segments.push(ShellShapeSegment {
                operator,
                stages: vec![powershell_stage(statement, source)],
            });
        }
        previous = Some(statement);
    }
    Some(segments)
}

/// Top-level statements, skipping the grammar's list wrappers and the empty
/// statements a `;` produces.
fn collect_powershell_statements<'tree>(node: Node<'tree>, out: &mut Vec<Node<'tree>>) {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        match child.kind() {
            "statement_list" => collect_powershell_statements(child, out),
            "empty_statement" | "comment" => {}
            _ => out.push(child),
        }
    }
}

fn powershell_stage(node: Node<'_>, source: &str) -> ShellShapeStage {
    let text = text(node, source).trim().to_string();
    let simple = node.kind() == "command";
    let words = if simple {
        lex_powershell_words(&text).0
    } else {
        Vec::new()
    };
    ShellShapeStage {
        kind: if simple { "command" } else { "compound" },
        text,
        words,
        start_byte: node.start_byte(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(dialect: ShellDialect, source: &str) -> Option<Vec<(String, Vec<Vec<String>>)>> {
        shell_command_shape(dialect, source)
            .segments
            .map(|segments| {
                segments
                    .into_iter()
                    .map(|segment| {
                        (
                            segment.operator.to_string(),
                            segment
                                .stages
                                .into_iter()
                                .map(|stage| match stage.kind {
                                    "command" => stage.words,
                                    _ => vec![format!("<{}>", stage.text)],
                                })
                                .collect(),
                        )
                    })
                    .collect()
            })
    }

    fn words(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn unspaced_and_chain_is_two_commands() {
        assert_eq!(
            summary(ShellDialect::Posix, "make lint&&make test"),
            Some(vec![
                (String::new(), vec![words(&["make", "lint"])]),
                ("&&".into(), vec![words(&["make", "test"])]),
            ])
        );
    }

    #[test]
    fn quoted_operators_stay_argument_text() {
        assert_eq!(
            summary(ShellDialect::Posix, "echo \"a && b; c | d\""),
            Some(vec![(
                String::new(),
                vec![words(&["echo", "a && b; c | d"])]
            )])
        );
        assert_eq!(
            summary(ShellDialect::PowerShell, "Write-Output 'a && b'"),
            Some(vec![(
                String::new(),
                vec![words(&["Write-Output", "a && b"])]
            )])
        );
    }

    #[test]
    fn every_chain_operator_and_pipeline_stage_is_typed() {
        assert_eq!(
            summary(ShellDialect::Posix, "a; b || X=1 c 2>&1 | tail -5\nd & e"),
            Some(vec![
                (String::new(), vec![words(&["a"])]),
                (";".into(), vec![words(&["b"])]),
                (
                    "||".into(),
                    vec![words(&["X=1", "c", "2>&1"]), words(&["tail", "-5"])]
                ),
                ("\n".into(), vec![words(&["d"])]),
                ("&".into(), vec![words(&["e"])]),
            ])
        );
    }

    #[test]
    fn a_trailing_redirect_belongs_to_the_last_command() {
        assert_eq!(
            summary(ShellDialect::Posix, "make lint || make test 2>&1"),
            Some(vec![
                (String::new(), vec![words(&["make", "lint"])]),
                ("||".into(), vec![words(&["make", "test", "2>&1"])]),
            ])
        );
    }

    #[test]
    fn compound_stages_are_not_read_as_argv() {
        assert_eq!(
            summary(ShellDialect::Posix, "cd sub && (make; true)"),
            Some(vec![
                (String::new(), vec![words(&["cd", "sub"])]),
                ("&&".into(), vec![vec!["<(make; true)>".to_string()]]),
            ])
        );
    }

    #[test]
    fn powershell_chains_statements_and_pipelines() {
        assert_eq!(
            summary(
                ShellDialect::PowerShell,
                "npm ci && npm test || echo fail; dotnet test | Select-Object -Last 5"
            ),
            Some(vec![
                (String::new(), vec![words(&["npm", "ci"])]),
                ("&&".into(), vec![words(&["npm", "test"])]),
                ("||".into(), vec![words(&["echo", "fail"])]),
                (
                    ";".into(),
                    vec![
                        words(&["dotnet", "test"]),
                        words(&["Select-Object", "-Last", "5"])
                    ]
                ),
            ])
        );
    }

    #[test]
    fn unparseable_empty_and_cmd_lines_are_unmeasured() {
        assert_eq!(summary(ShellDialect::Posix, "echo \"unterminated"), None);
        assert_eq!(summary(ShellDialect::Posix, "   "), None);
        assert_eq!(summary(ShellDialect::Cmd, "dir && echo ok"), None);
    }
}
