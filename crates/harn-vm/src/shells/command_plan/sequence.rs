//! Literal check sequences, using the command planner's parser and word decoder.

use tree_sitter::Node;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandStep {
    pub argv: Vec<String>,
    pub cwd: String,
}

/// Observe a fixed POSIX `&&` sequence without executing it. Unknown syntax or
/// shell state yields no plan, never an empty successful observation. Directory
/// changes persist through lists and are restored after a subshell.
pub fn plan_posix_sequence(source: &str, cwd: &str) -> Option<Vec<CommandStep>> {
    if source.len() > 65_536 {
        return None;
    }
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&harn_grammars::bash()).ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let mut cwd = relative_directory("", cwd)?;
    let mut steps = Vec::new();
    visit(root, source, &mut cwd, &mut steps, 0)?;
    (!steps.is_empty()).then_some(steps)
}

fn relative_directory(base: &str, directory: &str) -> Option<String> {
    if directory.starts_with(['/', '-']) || directory.contains(['\\', '\0']) {
        return None;
    }
    let mut parts = base
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    for part in directory.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            _ => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

fn visit(
    node: Node<'_>,
    source: &str,
    cwd: &mut String,
    steps: &mut Vec<CommandStep>,
    depth: usize,
) -> Option<()> {
    if depth > 64 || steps.len() >= 128 {
        return None;
    }
    match node.kind() {
        "program" | "subshell" => {
            let children = node.named_children(&mut node.walk()).collect::<Vec<_>>();
            let expected_children = if node.kind() == "program" { 1 } else { 3 };
            if children.len() != 1 || node.child_count() != expected_children {
                return None;
            }
            let mut scoped = cwd.clone();
            visit(children[0], source, &mut scoped, steps, depth + 1)?;
            if node.kind() == "program" {
                *cwd = scoped;
            }
        }
        "list" => {
            let children = node.children(&mut node.walk()).collect::<Vec<_>>();
            if children.len() != 3 || children[1].kind() != "&&" {
                return None;
            }
            visit(children[0], source, cwd, steps, depth + 1)?;
            visit(children[2], source, cwd, steps, depth + 1)?;
        }
        "command" => {
            let name = node.child_by_field_name("name")?;
            let mut argv = vec![super::literal(name, source)?];
            let arguments = node
                .children_by_field_name("argument", &mut node.walk())
                .collect::<Vec<_>>();
            if node
                .named_children(&mut node.walk())
                .any(|child| child != name && !arguments.contains(&child))
            {
                return None;
            }
            for argument in arguments {
                argv.push(super::literal(argument, source)?);
            }
            if argv[0] == "cd" {
                if argv.len() != 2 || argv[1].is_empty() {
                    return None;
                }
                *cwd = relative_directory(cwd, &argv[1])?;
            } else {
                // Builtins can replace/terminate the shell, define functions or
                // alter lookup state. Do not turn that state into check proof.
                if super::shell_builtin(&argv[0]) || argv[0].starts_with('-') {
                    return None;
                }
                steps.push(CommandStep {
                    argv,
                    cwd: cwd.clone(),
                });
            }
        }
        _ => return None,
    }
    Some(())
}
