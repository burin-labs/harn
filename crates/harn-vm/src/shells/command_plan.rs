//! Static execution facts for command strings. Unknown shell state is never
//! evidence that a program is missing.

use std::collections::BTreeMap;

use tree_sitter::Node;

/// The first statically named executable and the environment prefixes that
/// affect its lookup. `argv` is available only for a plain external command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPlan {
    pub program: String,
    pub environment: BTreeMap<String, String>,
    pub argv: Option<Vec<String>>,
}

/// Resolve the first program of an invocation, including a POSIX shell or
/// env wrapper. Other shell dialects retain their native execution behavior.
pub fn plan_invocation(program: &str, args: &[String]) -> Option<CommandPlan> {
    match super::shell_dialect_for_id(program) {
        Some(super::ShellDialect::Posix) => {
            // Login and interactive startup files can define functions or
            // change PATH, so pre-spawn filesystem evidence is insufficient.
            if args.first().map(String::as_str) != Some("-c") {
                return None;
            }
            plan_posix_command(args.get(1)?)
        }
        Some(_) => None,
        None => {
            let source = std::iter::once(program)
                .chain(args.iter().map(String::as_str))
                .map(shell_words::quote)
                .collect::<Vec<_>>()
                .join(" ");
            // An argv invocation may name an external executable with the
            // same name as a shell builtin.
            if program != "env" && !program.ends_with("/env") {
                return Some(CommandPlan {
                    program: program.to_string(),
                    environment: BTreeMap::new(),
                    argv: None,
                });
            }
            plan_posix_command(&source)
        }
    }
}

/// Analyze a POSIX command without evaluating expansions or executing it.
pub fn plan_posix_command(source: &str) -> Option<CommandPlan> {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();
    if root.has_error() {
        return None;
    }
    let mut command = root.named_child(0)?;
    let single = root.child_count() == 1 && command.kind() == "command";
    while matches!(command.kind(), "list" | "pipeline" | "redirected_statement") {
        command = command.named_child(0)?;
    }
    if command.kind() != "command" {
        return None;
    }
    let name = command.child_by_field_name("name")?;
    let mut argv = vec![literal(name, source)?];
    let mut environment = BTreeMap::new();
    let mut cursor = command.walk();
    for child in command.named_children(&mut cursor) {
        if child.kind() == "variable_assignment" {
            let text = child.utf8_text(source.as_bytes()).ok()?;
            let (key, value) = text.split_once('=')?;
            // Assignment expansion can change PATH. Decline the whole probe
            // instead of searching a guessed environment.
            if dynamic(child) || value.contains(['$', '~', '{', '}']) {
                return None;
            }
            let words = shell_words::split(value).ok()?;
            let value = match words.as_slice() {
                [] => String::new(),
                [value] => value.clone(),
                _ => return None,
            };
            environment.insert(key.to_string(), value);
        }
    }
    let mut cursor = command.walk();
    let mut all_literal = true;
    for arg in command.children_by_field_name("argument", &mut cursor) {
        if let Some(value) = literal(arg, source) {
            argv.push(value);
        } else {
            all_literal = false;
            break;
        }
    }
    let mut cursor = command.walk();
    let only_words = command.named_children(&mut cursor).all(|child| {
        child == name
            || command
                .children_by_field_name("argument", &mut command.walk())
                .any(|arg| arg == child)
    });
    let plain = single && environment.is_empty() && all_literal && only_words;
    let mut index = 0;
    if argv[0] == "env" || argv[0].ends_with("/env") {
        // Do not interpret unknown env options (notably -S or -i) using a
        // partial argv or the enclosing shell's environment.
        if !all_literal {
            return None;
        }
        index = 1;
        while let Some(word) = argv.get(index) {
            if let Some((key, value)) = word.split_once('=') {
                if key.is_empty() || key.starts_with('-') {
                    return None;
                }
                environment.insert(key.to_string(), value.to_string());
                index += 1;
            } else if word == "--" {
                index += 1;
                break;
            } else {
                break;
            }
        }
    }
    let program = argv.get(index)?.clone();
    if program.starts_with('-') || (index == 0 && shell_builtin(&program)) {
        return None;
    }
    Some(CommandPlan {
        program,
        environment,
        argv: (plain && index == 0).then_some(argv),
    })
}

fn literal(node: Node<'_>, source: &str) -> Option<String> {
    if dynamic(node) {
        return None;
    }
    let raw = node.utf8_text(source.as_bytes()).ok()?;
    if raw.contains(['*', '?', '[', '{', '~', '$', '(', ')']) || raw.starts_with('=') {
        return None;
    }
    let words = shell_words::split(raw).ok()?;
    (words.len() == 1).then(|| words[0].clone())
}

fn dynamic(node: Node<'_>) -> bool {
    if matches!(
        node.kind(),
        "expansion"
            | "simple_expansion"
            | "command_substitution"
            | "arithmetic_expansion"
            | "process_substitution"
    ) {
        return true;
    }
    let mut cursor = node.walk();
    let found = node.named_children(&mut cursor).any(dynamic);
    found
}

fn shell_builtin(program: &str) -> bool {
    matches!(
        program,
        ":" | "."
            | "["
            | "alias"
            | "bg"
            | "bind"
            | "break"
            | "builtin"
            | "cd"
            | "command"
            | "caller"
            | "compgen"
            | "complete"
            | "compopt"
            | "coproc"
            | "continue"
            | "declare"
            | "dirs"
            | "disown"
            | "echo"
            | "enable"
            | "eval"
            | "exec"
            | "exit"
            | "export"
            | "false"
            | "fc"
            | "fg"
            | "getopts"
            | "hash"
            | "help"
            | "history"
            | "jobs"
            | "kill"
            | "let"
            | "local"
            | "logout"
            | "mapfile"
            | "popd"
            | "printf"
            | "pushd"
            | "pwd"
            | "read"
            | "readarray"
            | "readonly"
            | "return"
            | "set"
            | "shift"
            | "shopt"
            | "source"
            | "test"
            | "times"
            | "time"
            | "trap"
            | "true"
            | "type"
            | "typeset"
            | "ulimit"
            | "umask"
            | "unalias"
            | "unset"
            | "wait"
            | "autoload"
            | "bindkey"
            | "bye"
            | "chdir"
            | "compadd"
            | "comparguments"
            | "compcall"
            | "compctl"
            | "compdescribe"
            | "compfiles"
            | "compgroups"
            | "compquote"
            | "compset"
            | "comptags"
            | "comptry"
            | "compvalues"
            | "disable"
            | "echotc"
            | "echoti"
            | "emulate"
            | "float"
            | "functions"
            | "getln"
            | "integer"
            | "limit"
            | "log"
            | "noglob"
            | "nocorrect"
            | "print"
            | "private"
            | "pushln"
            | "r"
            | "rehash"
            | "sched"
            | "setopt"
            | "suspend"
            | "ttyctl"
            | "unfunction"
            | "unhash"
            | "unlimit"
            | "unsetopt"
            | "vared"
            | "whence"
            | "where"
            | "which"
            | "zcompile"
            | "zformat"
            | "zle"
            | "zmodload"
            | "zparseopts"
            | "zregexparse"
            | "zstyle"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_plan_preserves_shell_semantics() {
        let plain = plan_posix_command("cargo test 'a b'").unwrap();
        assert_eq!(plain.argv.unwrap(), ["cargo", "test", "a b"]);
        for source in [
            "cargo test && true",
            "cargo test | cat",
            "cargo test > out",
            "> out cargo test",
            "cargo test <<< input",
            "X=1 cargo test",
            "cargo *.rs",
            "cargo test &",
            "cargo $'quoted\\nargument'",
        ] {
            let plan = plan_posix_command(source).unwrap();
            assert_eq!(plan.program, "cargo");
            assert!(plan.argv.is_none(), "{source}");
        }
        for source in [
            "$PROGRAM test",
            "PATH=$OTHER cargo test",
            "PATH=~/tools cargo test",
            "f() { absent; }; f",
            "echo hi",
            "exit 127",
            "print hello",
            "$'cargo' test",
        ] {
            assert!(plan_posix_command(source).is_none(), "{source}");
        }
        let env = plan_posix_command("env VAR=1 PATH=/tools ./relative/prog").unwrap();
        assert_eq!(env.program, "./relative/prog");
        assert_eq!(env.environment["PATH"], "/tools");
        assert!(env.argv.is_none());
    }
}
