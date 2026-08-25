//! Structural shell analysis shared by command-policy classifiers.
//!
//! The interface deliberately separates already-resolved argv from shell text:
//! argv is copied losslessly, while only real shell programs (`sh -c` payloads
//! included) cross the parser seam. The parser is a classification aid, not the
//! security boundary; child processes remain confined by the operating system.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use tree_sitter::Node;
use wait_timeout::ChildExt;

use super::{
    command_basename, parse_env_invocation, shell_c_script, unwrapped_command_index,
    ShellCommandStage, MAX_DEPTH,
};

/// Commands and argument words that a policy classifier can reason about.
///
/// `unresolved` is set whenever syntax recovery or dynamic execution prevents
/// the parser from naming an executable. Callers must not interpret an empty
/// `stages` list as evidence of safety when this bit is set.
#[derive(Debug, Default)]
pub(crate) struct ShellAnalysis {
    pub(crate) stages: Vec<ShellCommandStage>,
    pub(crate) argument_words: Vec<String>,
    /// Complete redirect syntax nodes, including redirects nested inside
    /// substitutions and quoted words. The catastrophic floor consumes these
    /// rather than trying to rediscover AST nesting from flattened text.
    pub(crate) redirects: Vec<ShellRedirect>,
    /// An invoked self-recursive colon function was identified structurally.
    pub(crate) fork_bomb: bool,
    pub(crate) unresolved: bool,
}

use crate::shells::ShellDialect;

#[derive(Debug)]
pub(crate) struct ShellRedirect {
    pub(crate) operator: String,
    pub(crate) destination: Option<String>,
    pub(crate) dynamic: bool,
}

impl ShellRedirect {
    pub(crate) fn writes_file(&self) -> bool {
        matches!(
            self.operator.as_str(),
            ">" | ">>" | ">|" | "&>" | "&>>" | "<>"
        )
    }
}

impl ShellAnalysis {
    pub(crate) fn unresolved() -> Self {
        Self {
            unresolved: true,
            ..Self::default()
        }
    }
}

/// Analyze exact, already-resolved argv without round-tripping it through shell
/// syntax. A literal shell `-c` payload is the sole recursive parser entry.
pub(crate) fn analyze_argv(argv: &[String]) -> ShellAnalysis {
    let mut analysis = ShellAnalysis::default();
    collect_argv(argv, 0, &mut analysis);
    analysis
}

/// Parse a POSIX/bash program and collect every potentially executed command.
pub(crate) fn analyze_shell(command: &str) -> ShellAnalysis {
    let mut analysis = ShellAnalysis::default();
    collect_shell(command, 0, &mut analysis);
    analysis
}

/// Parse shell text through the single dialect registry.
pub(crate) fn analyze_shell_dialect(dialect: ShellDialect, command: &str) -> ShellAnalysis {
    match dialect {
        ShellDialect::Posix => analyze_shell(command),
        ShellDialect::PowerShell => analyze_powershell(command),
        ShellDialect::Cmd => analyze_cmd(command),
    }
}

fn collect_argv(argv: &[String], depth: usize, analysis: &mut ShellAnalysis) {
    if depth > MAX_DEPTH {
        analysis.unresolved = true;
        return;
    }
    for word in argv {
        push_unique(&mut analysis.argument_words, word);
    }
    collect_env_split_execution(argv, &vec![false; argv.len()], depth, analysis);
    let index = unwrapped_command_index(argv, 0, argv.len());
    if index >= argv.len() {
        return;
    }
    let effective = &argv[index..];
    let executable = command_basename(&effective[0]);
    if matches!(executable, "bash" | "sh" | "zsh") {
        if let Some(script) = shell_c_script(&effective[1..]) {
            collect_shell(script, depth + 1, analysis);
            return;
        }
        if !shell_is_introspection_only(&effective[1..]) {
            analysis.unresolved = true;
        }
    } else if matches!(
        executable,
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe"
    ) {
        match powershell_payload(&effective[1..]) {
            Some(Ok(script)) => collect_powershell(&script, depth + 1, analysis),
            Some(Err(())) => analysis.unresolved = true,
            None if !shell_is_introspection_only(&effective[1..]) => analysis.unresolved = true,
            None => {}
        }
        return;
    } else if matches!(executable, "cmd" | "cmd.exe") {
        if let Some(script) = cmd_payload(&effective[1..]) {
            collect_cmd(&script, depth + 1, analysis);
        } else if !shell_is_introspection_only(&effective[1..]) {
            analysis.unresolved = true;
        }
        return;
    }
    collect_argument_execution(effective, &vec![false; effective.len()], depth, analysis);
    analysis.stages.push(ShellCommandStage {
        text: effective.join(" "),
        argv: effective.to_vec(),
        dynamic: vec![false; effective.len()],
    });
}

fn analyze_powershell(command: &str) -> ShellAnalysis {
    let mut analysis = ShellAnalysis::default();
    collect_powershell(command, 0, &mut analysis);
    analysis
}

fn collect_powershell(command: &str, depth: usize, analysis: &mut ShellAnalysis) {
    if depth > MAX_DEPTH {
        analysis.unresolved = true;
        return;
    }

    // Windows PowerShell ships its authoritative parser with the platform. It
    // is used as the syntax oracle when available; tree-sitter remains the
    // deterministic typed extractor for offline/cross-compiled builds.
    if depth == 0 && cfg!(windows) {
        if let Some(parsed) = native_powershell_accepts(command) {
            analysis.unresolved |= !parsed;
        }
    }

    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&tree_sitter_powershell::LANGUAGE.into())
        .is_err()
    {
        analysis.unresolved = true;
        return;
    }
    let Some(tree) = parser.parse(command, None) else {
        analysis.unresolved = true;
        return;
    };
    let root = tree.root_node();
    analysis.unresolved |= root.has_error();
    visit_powershell_tree(root, command.as_bytes(), depth, analysis);
}

fn visit_powershell_tree(
    root: Node<'_>,
    source: &[u8],
    depth: usize,
    analysis: &mut ShellAnalysis,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "command" {
            visit_powershell_command(node, source, depth, analysis);
        }
        if matches!(
            node.kind(),
            "assignment_expression"
                | "function_statement"
                | "class_statement"
                | "data_statement"
                | "inlinescript_statement"
                | "variable"
                | "sub_expression"
                | "script_block_expression"
                | "invokation_expression"
                | "stop_parsing"
        ) {
            analysis.unresolved = true;
        }
        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        stack.extend(children.into_iter().rev());
    }
}

fn visit_powershell_command(
    command: Node<'_>,
    source: &[u8],
    depth: usize,
    analysis: &mut ShellAnalysis,
) {
    let Some(text) = node_text(command, source) else {
        analysis.unresolved = true;
        return;
    };
    let (argv, word_dynamic, unresolved) = lex_powershell_words(text);
    analysis.unresolved |= unresolved;
    if argv.is_empty() {
        analysis.unresolved = true;
        return;
    }
    analysis.unresolved |= word_dynamic[0];
    for word in &argv {
        push_unique(&mut analysis.argument_words, word);
    }

    let index = unwrapped_command_index(&argv, 0, argv.len());
    if index >= argv.len() {
        return;
    }
    let effective = &argv[index..];
    let executable = command_basename(&effective[0]);
    if matches!(
        executable,
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe"
    ) {
        match powershell_payload(&effective[1..]) {
            Some(Ok(script)) => collect_powershell(&script, depth + 1, analysis),
            Some(Err(())) => analysis.unresolved = true,
            None => analysis.unresolved = true,
        }
        return;
    }
    collect_argument_execution(effective, &word_dynamic[index..], depth, analysis);
    analysis.stages.push(ShellCommandStage {
        text: node_text(command, source).unwrap_or_default().to_string(),
        argv: effective.to_vec(),
        dynamic: word_dynamic[index..].to_vec(),
    });
}

fn lex_powershell_words(command: &str) -> (Vec<String>, Vec<bool>, bool) {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Quote {
        None,
        Single,
        Double,
    }

    let mut words = Vec::new();
    let mut dynamics = Vec::new();
    let mut word = String::new();
    let mut dynamic = false;
    let mut quote = Quote::None;
    let mut unresolved = false;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        match (quote, ch) {
            (Quote::None, '\'') => quote = Quote::Single,
            (Quote::Single, '\'') if chars.peek() == Some(&'\'') => {
                chars.next();
                word.push('\'');
            }
            (Quote::Single, '\'') => quote = Quote::None,
            (Quote::None, '"') => quote = Quote::Double,
            (Quote::Double, '"') => quote = Quote::None,
            (Quote::Single, _) => word.push(ch),
            (_, '`') => match chars.next() {
                Some(escaped) => word.push(escaped),
                None => unresolved = true,
            },
            (Quote::None | Quote::Double, '$') => {
                dynamic = true;
                word.push(ch);
            }
            (Quote::None, '@' | '{' | '}' | '(' | ')') => {
                unresolved = true;
                word.push(ch);
            }
            (Quote::None, ch) if ch.is_whitespace() => {
                push_typed_word(&mut words, &mut dynamics, &mut word, &mut dynamic);
            }
            _ => word.push(ch),
        }
    }
    unresolved |= quote != Quote::None;
    push_typed_word(&mut words, &mut dynamics, &mut word, &mut dynamic);
    (words, dynamics, unresolved)
}

fn push_typed_word(
    words: &mut Vec<String>,
    dynamics: &mut Vec<bool>,
    word: &mut String,
    dynamic: &mut bool,
) {
    if !word.is_empty() {
        words.push(std::mem::take(word));
        dynamics.push(std::mem::take(dynamic));
    }
}

fn analyze_cmd(command: &str) -> ShellAnalysis {
    let mut analysis = ShellAnalysis::default();
    collect_cmd(command, 0, &mut analysis);
    analysis
}

fn collect_cmd(command: &str, depth: usize, analysis: &mut ShellAnalysis) {
    if depth > MAX_DEPTH {
        analysis.unresolved = true;
        return;
    }
    for stage in lex_cmd_stages(command, analysis) {
        for word in &stage {
            push_unique(&mut analysis.argument_words, word);
        }
        let index = unwrapped_command_index(&stage, 0, stage.len());
        if index >= stage.len() {
            continue;
        }
        let effective = &stage[index..];
        if matches!(command_basename(&effective[0]), "cmd" | "cmd.exe") {
            if let Some(payload) = cmd_payload(&effective[1..]) {
                collect_cmd(&payload, depth + 1, analysis);
            } else {
                analysis.unresolved = true;
            }
            continue;
        }
        analysis.stages.push(ShellCommandStage {
            text: effective.join(" "),
            argv: effective.to_vec(),
            dynamic: vec![false; effective.len()],
        });
    }
}

fn lex_cmd_stages(command: &str, analysis: &mut ShellAnalysis) -> Vec<Vec<String>> {
    let mut stages = Vec::new();
    let mut stage = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut chars = command.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '^' => match chars.next() {
                Some(escaped) => word.push(escaped),
                None => analysis.unresolved = true,
            },
            '"' => quoted = !quoted,
            '%' | '!' => {
                analysis.unresolved = true;
                word.push(ch);
            }
            '&' | '|' | '\n' | '\r' if !quoted => {
                push_cmd_word(&mut stage, &mut word);
                push_cmd_stage(&mut stages, &mut stage);
                if chars.peek() == Some(&ch) {
                    chars.next();
                }
            }
            '(' | ')' if !quoted => {
                analysis.unresolved = true;
                push_cmd_word(&mut stage, &mut word);
            }
            ch if ch.is_whitespace() && !quoted => push_cmd_word(&mut stage, &mut word),
            _ => word.push(ch),
        }
    }
    if quoted {
        analysis.unresolved = true;
    }
    push_cmd_word(&mut stage, &mut word);
    push_cmd_stage(&mut stages, &mut stage);
    stages
}

fn push_cmd_word(stage: &mut Vec<String>, word: &mut String) {
    if !word.is_empty() {
        stage.push(std::mem::take(word));
    }
}

fn push_cmd_stage(stages: &mut Vec<Vec<String>>, stage: &mut Vec<String>) {
    if !stage.is_empty() {
        stages.push(std::mem::take(stage));
    }
}

fn powershell_payload(args: &[String]) -> Option<Result<String, ()>> {
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].to_ascii_lowercase();
        if is_powershell_command_flag(&flag) {
            return Some(
                (index + 1 < args.len())
                    .then(|| args[index + 1..].join(" "))
                    .ok_or(()),
            );
        }
        if is_powershell_encoded_flag(&flag) {
            return Some(
                args.get(index + 1)
                    .ok_or(())
                    .and_then(|value| decode_powershell_encoded(value).ok_or(())),
            );
        }
        index += 1;
    }
    None
}

fn is_powershell_command_flag(flag: &str) -> bool {
    matches!(flag, "/c" | "/command") || (flag.starts_with('-') && "-command".starts_with(flag))
}

fn is_powershell_encoded_flag(flag: &str) -> bool {
    flag == "/encodedcommand" || (flag.starts_with('-') && "-encodedcommand".starts_with(flag))
}

fn decode_powershell_encoded(value: &str) -> Option<String> {
    let bytes = BASE64_STANDARD.decode(value.trim()).ok()?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let utf16 = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();
    String::from_utf16(&utf16)
        .ok()
        .map(|text| text.trim_start_matches('\u{feff}').to_string())
}

fn cmd_payload(args: &[String]) -> Option<String> {
    args.iter()
        .position(|arg| arg.eq_ignore_ascii_case("/c"))
        .and_then(|index| (index + 1 < args.len()).then(|| args[index + 1..].join(" ")))
}

/// Ask PowerShell's parser whether input is syntactically complete without
/// evaluating the input. The command itself is base64 data passed through the
/// child environment to a fixed parser program; it is never interpolated into
/// `-Command`.
fn native_powershell_accepts(command: &str) -> Option<bool> {
    const PARSER: &str = "$s=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($env:HARN_POWERSHELL_PARSE_PAYLOAD));$t=$null;$e=$null;[System.Management.Automation.Language.Parser]::ParseInput($s,[ref]$t,[ref]$e)>$null;if($e.Count){exit 2}";
    let payload = BASE64_STANDARD.encode(command.as_bytes());
    for executable in native_powershell_candidates() {
        let mut parser = Command::new(&executable);
        if let Some(parent) = executable
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            parser.current_dir(parent);
        }
        match parser
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                PARSER,
            ])
            .env("HARN_POWERSHELL_PARSE_PAYLOAD", &payload)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(mut child) => match child.wait_timeout(Duration::from_secs(2)) {
                Ok(Some(status)) => {
                    // Only the fixed parser program's exit code 2 proves a
                    // syntax error. Startup and runtime failures leave the
                    // native oracle unavailable.
                    return powershell_parse_result(status.code());
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                Err(_) => return None,
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        }
    }
    None
}

fn powershell_parse_result(exit_code: Option<i32>) -> Option<bool> {
    match exit_code {
        Some(0) => Some(true),
        Some(2) => Some(false),
        _ => None,
    }
}

#[cfg(windows)]
fn native_powershell_candidates() -> Vec<PathBuf> {
    // Do not search PATH or the request cwd at this pre-launch safety seam: on
    // Windows that could execute an attacker-controlled `pwsh.exe`. The inbox
    // parser is addressed through its absolute system installation instead.
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .filter(|root| root.is_absolute())
        .map(|root| {
            vec![root
                .join("System32")
                .join("WindowsPowerShell")
                .join("v1.0")
                .join("powershell.exe")]
        })
        .unwrap_or_default()
}

#[cfg(not(windows))]
fn native_powershell_candidates() -> Vec<PathBuf> {
    // Non-Windows candidates are used only by the differential test.
    vec![PathBuf::from("pwsh")]
}

fn collect_shell(command: &str, depth: usize, analysis: &mut ShellAnalysis) {
    if depth > MAX_DEPTH {
        analysis.unresolved = true;
        return;
    }
    let mut parser = tree_sitter::Parser::new();
    if parser
        .set_language(&tree_sitter_bash::LANGUAGE.into())
        .is_err()
    {
        analysis.unresolved = true;
        return;
    }
    let Some(tree) = parser.parse(command, None) else {
        analysis.unresolved = true;
        return;
    };
    let root = tree.root_node();
    analysis.unresolved |= root.has_error();
    analysis.fork_bomb |= structural_fork_bomb(root, command.as_bytes());
    visit_tree(root, command.as_bytes(), depth, analysis);
}

/// Recognize the canonical shell fork-bomb family from executable structure,
/// not substring text. The definition must name `:`, contain at least two
/// recursive `:` command nodes, and be invoked later in the same program.
/// Quoted examples and dormant definitions therefore remain inert.
fn structural_fork_bomb(root: Node<'_>, source: &[u8]) -> bool {
    let mut bomb_definitions = Vec::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "function_definition"
            && node
                .child_by_field_name("name")
                .and_then(|name| node_text(name, source))
                == Some(":")
            && colon_command_count(node, source) >= 2
        {
            bomb_definitions.push((node.start_byte(), node.end_byte()));
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    if bomb_definitions.is_empty() {
        return false;
    }

    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "command"
            && command_name(node, source) == Some(":")
            && bomb_definitions
                .iter()
                .any(|(_, end)| node.start_byte() >= *end)
        {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
}

fn colon_command_count(root: Node<'_>, source: &[u8]) -> usize {
    let mut count = 0;
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.kind() == "command" && command_name(node, source) == Some(":") {
            count += 1;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    count
}

fn command_name<'a>(command: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    command
        .child_by_field_name("name")
        .and_then(|name| node_text(name, source))
}

fn visit_tree(root: Node<'_>, source: &[u8], depth: usize, analysis: &mut ShellAnalysis) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "function_definition" => {
                // A definition is inert, while resolving a later invocation
                // requires shell state. Fail closed without classifying its
                // dormant body as if it had already executed.
                analysis.unresolved = true;
                continue;
            }
            "command" => visit_command(node, source, depth, analysis),
            "file_redirect" => collect_redirect(node, source, analysis),
            _ => {}
        }
        let mut cursor = node.walk();
        let children = node.named_children(&mut cursor).collect::<Vec<_>>();
        stack.extend(children.into_iter().rev());
    }
}

fn visit_command(command: Node<'_>, source: &[u8], depth: usize, analysis: &mut ShellAnalysis) {
    let Some(name_node) = command.child_by_field_name("name") else {
        analysis.unresolved = true;
        return;
    };
    let (command_name, command_dynamic) = shell_word(name_node, source);
    let Some(command_name) = command_name else {
        analysis.unresolved = true;
        return;
    };
    analysis.unresolved |= command_dynamic;
    push_unique(&mut analysis.argument_words, &command_name);

    let mut argv = vec![command_name];
    let mut word_dynamic = vec![command_dynamic];
    let mut cursor = command.walk();
    for argument in command.children_by_field_name("argument", &mut cursor) {
        let (value, dynamic) = shell_word(argument, source);
        let Some(value) = value else {
            // Argument expansion does not obscure the executable identity, but
            // retaining a lossy word would create false path classifications.
            continue;
        };
        push_unique(&mut analysis.argument_words, &value);
        argv.push(value);
        word_dynamic.push(dynamic);
    }
    collect_env_split_execution(&argv, &word_dynamic, depth, analysis);

    let index = unwrapped_command_index(&argv, 0, argv.len());
    if index >= argv.len() {
        return;
    }
    if index > 0 && word_dynamic.get(index).copied().unwrap_or(true) {
        analysis.unresolved = true;
    }
    let effective = &argv[index..];
    let executable = command_basename(&effective[0]);
    if matches!(executable, "bash" | "sh" | "zsh") {
        if let Some(script_index) = shell_c_script_index(&effective[1..]) {
            let argument_index = index + 1 + script_index;
            if word_dynamic.get(argument_index).copied().unwrap_or(true) {
                analysis.unresolved = true;
            } else if let Some(script) = argv.get(argument_index) {
                collect_shell(script, depth + 1, analysis);
            }
            return;
        }
        if !shell_is_introspection_only(&effective[1..]) {
            analysis.unresolved = true;
        }
    } else if matches!(
        executable,
        "powershell" | "powershell.exe" | "pwsh" | "pwsh.exe"
    ) {
        match powershell_payload(&effective[1..]) {
            Some(Ok(script)) => collect_powershell(&script, depth + 1, analysis),
            Some(Err(())) => analysis.unresolved = true,
            None => analysis.unresolved = true,
        }
        return;
    } else if matches!(executable, "cmd" | "cmd.exe") {
        if let Some(script) = cmd_payload(&effective[1..]) {
            collect_cmd(&script, depth + 1, analysis);
        } else {
            analysis.unresolved = true;
        }
        return;
    }
    collect_argument_execution(effective, &word_dynamic[index..], depth, analysis);
    analysis.stages.push(ShellCommandStage {
        text: node_text(command, source).unwrap_or_default().to_string(),
        argv: effective.to_vec(),
        dynamic: word_dynamic[index..].to_vec(),
    });
}

fn collect_redirect(redirect: Node<'_>, source: &[u8], analysis: &mut ShellAnalysis) {
    let Some(destination_node) = redirect.child_by_field_name("destination") else {
        analysis.unresolved = true;
        return;
    };
    let operator = redirect
        .child_by_field_name("operator")
        .and_then(|node| node_text(node, source))
        .map(ToString::to_string)
        .or_else(|| redirect_operator_from_children(redirect, source));
    let Some(operator) = operator else {
        analysis.unresolved = true;
        return;
    };
    let (destination, dynamic) = shell_word(destination_node, source);
    analysis.unresolved |= dynamic;
    if let Some(value) = destination.as_deref() {
        push_unique(&mut analysis.argument_words, value);
    }
    analysis.redirects.push(ShellRedirect {
        operator,
        destination,
        dynamic,
    });
}

fn redirect_operator_from_children(redirect: Node<'_>, source: &[u8]) -> Option<String> {
    (0..redirect.child_count()).find_map(|index| {
        let child = redirect.child(index as u32)?;
        let text = node_text(child, source)?;
        matches!(
            text,
            ">" | ">>" | ">|" | "&>" | "&>>" | "<>" | "<" | "<<" | "<<<"
        )
        .then(|| text.to_string())
    })
}

fn shell_word(node: Node<'_>, source: &[u8]) -> (Option<String>, bool) {
    let Some(raw) = node_text(node, source) else {
        return (None, true);
    };
    let dynamic =
        !(raw.starts_with('\'') && raw.ends_with('\'')) && contains_dynamic_expansion(node);
    match shell_words::split(raw) {
        Ok(words) if words.len() == 1 => (words.into_iter().next(), dynamic),
        _ => (None, dynamic),
    }
}

fn contains_dynamic_expansion(root: Node<'_>) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(
            node.kind(),
            "arithmetic_expansion"
                | "command_substitution"
                | "expansion"
                | "process_substitution"
                | "simple_expansion"
        ) {
            return true;
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    false
}

fn shell_c_script_index(args: &[String]) -> Option<usize> {
    let mut index = 0;
    while index < args.len() {
        let token = &args[index];
        if token == "--" {
            index += 1;
            continue;
        }
        if token.starts_with('-') && token != "-" {
            if token.chars().skip(1).any(|flag| flag == 'c') {
                return (index + 1 < args.len()).then_some(index + 1);
            }
            index += 1;
            continue;
        }
        return None;
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArgumentExecution {
    /// Runtime input or shell state determines what gets executed.
    Opaque,
    /// A child boundary is exact only before any option parsing begins or
    /// after an explicit `--`; all option-bearing forms remain opaque.
    InputArgv,
    /// GNU Parallel's exact command operands form a shell command template.
    /// Option-bearing forms remain opaque for the same reason as InputArgv.
    InputShell,
    /// A literal shell program is carried by a command argument.
    ShellLiteral,
    /// `find` carries one or more argv programs after `-exec`/`-execdir`.
    FindExec,
}

fn argument_execution(command: &str) -> Option<ArgumentExecution> {
    match command {
        "eval" | "source" | "." => Some(ArgumentExecution::Opaque),
        "xargs" => Some(ArgumentExecution::InputArgv),
        "parallel" => Some(ArgumentExecution::InputShell),
        "trap" => Some(ArgumentExecution::ShellLiteral),
        "find" => Some(ArgumentExecution::FindExec),
        _ => None,
    }
}

fn collect_argument_execution(
    argv: &[String],
    dynamic: &[bool],
    depth: usize,
    analysis: &mut ShellAnalysis,
) {
    if argv.is_empty() {
        return;
    }
    match argument_execution(command_basename(&argv[0])) {
        Some(ArgumentExecution::Opaque) => {
            analysis.unresolved = true;
            if command_basename(&argv[0]) == "eval" && argv.len() > 1 {
                if dynamic
                    .get(1..)
                    .unwrap_or_default()
                    .iter()
                    .any(|value| *value)
                {
                    return;
                }
                collect_shell(&argv[1..].join(" "), depth + 1, analysis);
            }
        }
        Some(ArgumentExecution::InputArgv) => {
            analysis.unresolved = true;
            let child_index = match argv.get(1).map(String::as_str) {
                Some("--") if argv.len() > 2 => Some(2),
                Some(argument) if !argument.starts_with('-') => Some(1),
                _ => None,
            };
            if let Some(child_index) = child_index {
                if !dynamic.get(child_index).copied().unwrap_or(true) {
                    collect_argv(&argv[child_index..], depth + 1, analysis);
                }
            }
        }
        Some(ArgumentExecution::InputShell) => {
            analysis.unresolved = true;
            let start = match argv.get(1).map(String::as_str) {
                Some("--") if argv.len() > 2 => Some(2),
                Some(argument) if !argument.starts_with('-') => Some(1),
                _ => None,
            };
            let Some(start) = start else {
                return;
            };
            let end = argv[start..]
                .iter()
                .position(|argument| matches!(argument.as_str(), ":::" | "::::" | ":::+" | "::::+"))
                .map_or(argv.len(), |offset| start + offset);
            if start == end
                || dynamic
                    .get(start..end)
                    .is_none_or(|items| items.iter().any(|dynamic| *dynamic))
            {
                return;
            }
            let command = argv[start..end].join(" ");
            collect_shell(&command, depth + 1, analysis);
        }
        Some(ArgumentExecution::ShellLiteral) => {
            let Some(script_index) = trap_script_index(&argv[1..]).map(|index| index + 1) else {
                return;
            };
            if dynamic.get(script_index).copied().unwrap_or(true) {
                analysis.unresolved = true;
            } else {
                collect_shell(&argv[script_index], depth + 1, analysis);
            }
        }
        Some(ArgumentExecution::FindExec) => {
            collect_find_exec(argv, dynamic, depth, analysis);
        }
        None => {}
    }
}

fn collect_env_split_execution(
    argv: &[String],
    dynamic: &[bool],
    depth: usize,
    analysis: &mut ShellAnalysis,
) {
    if argv.is_empty() || command_basename(&argv[0]) != "env" {
        return;
    }
    let invocation = parse_env_invocation(argv, 1, argv.len());
    analysis.unresolved |= !invocation.resolved;
    let Some(split) = invocation.split else {
        return;
    };
    analysis.unresolved = true;
    let trailing_dynamic = dynamic
        .get(split.trailing_start..)
        .is_none_or(|items| items.iter().any(|dynamic| *dynamic));
    if split.payload.is_empty()
        || dynamic.get(split.payload_index).copied().unwrap_or(true)
        || trailing_dynamic
    {
        return;
    }
    if let Some(split_argv) = split_static_env_string(split.payload) {
        let mut expanded = vec!["env".to_string()];
        expanded.extend(split_argv);
        expanded.extend_from_slice(&argv[split.trailing_start..]);
        collect_argv(&expanded, depth + 1, analysis);
    }
}

fn split_static_env_string(payload: &str) -> Option<Vec<String>> {
    // GNU env -S owns a small split-string language, not a shell. Restrict
    // never-approvable inference to a byte set whose whitespace splitting is
    // exact in that language; quotes, escapes, substitutions, and env-specific
    // controls remain unresolved/consent-gated without speculative execution.
    if payload.is_empty()
        || !payload.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || ch.is_ascii_whitespace()
                || matches!(ch, '_' | '-' | '.' | '/' | ':' | '=' | '+' | ',')
        })
    {
        return None;
    }
    Some(
        payload
            .split_ascii_whitespace()
            .map(ToString::to_string)
            .collect(),
    )
}

fn trap_script_index(args: &[String]) -> Option<usize> {
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--" => index += 1,
            "-l" | "--list-signals" | "-p" | "--print" => return None,
            value if value.starts_with('-') => index += 1,
            _ => return Some(index),
        }
    }
    None
}

fn collect_find_exec(
    argv: &[String],
    dynamic: &[bool],
    depth: usize,
    analysis: &mut ShellAnalysis,
) {
    let mut index = 1;
    while index < argv.len() {
        if !matches!(
            argv[index].as_str(),
            "-exec" | "-execdir" | "-ok" | "-okdir"
        ) {
            index += 1;
            continue;
        }
        let start = index + 1;
        let end = argv[start..]
            .iter()
            .position(|arg| matches!(arg.as_str(), ";" | "+"))
            .map(|offset| start + offset)
            .unwrap_or(argv.len());
        if start >= end
            || argv[start].contains("{}")
            || dynamic
                .get(start..end)
                .unwrap_or_default()
                .iter()
                .any(|value| *value)
        {
            analysis.unresolved = true;
        } else {
            collect_argv(&argv[start..end], depth + 1, analysis);
        }
        index = end.saturating_add(1);
    }
}

fn shell_is_introspection_only(args: &[String]) -> bool {
    !args.is_empty()
        && args
            .iter()
            .all(|arg| matches!(arg.as_str(), "--help" | "--version"))
}

fn node_text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    node.utf8_text(source).ok()
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !value.is_empty() && !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_native_parser_reserves_syntax_error_exit() {
        assert_eq!(powershell_parse_result(Some(0)), Some(true));
        assert_eq!(powershell_parse_result(Some(2)), Some(false));
        assert_eq!(powershell_parse_result(Some(1)), None);
        assert_eq!(powershell_parse_result(None), None);
    }

    #[test]
    fn powershell_fallback_agrees_with_native_parser_when_available() {
        let cases = [
            "Write-Output 'literal value'",
            "Remove-Item -Recurse -LiteralPath .",
            "Write-Output before; Get-ChildItem | Select-Object Name",
            "Write-Output \"unterminated",
        ];
        for command in cases {
            let Some(native_accepts) = native_powershell_accepts(command) else {
                return;
            };
            let fallback_accepts = !analyze_powershell(command).unresolved;
            assert_eq!(
                fallback_accepts, native_accepts,
                "fallback/native syntax disagreement for {command:?}"
            );
        }
    }

    #[test]
    fn native_powershell_adapter_parses_input_without_executing_it() {
        let dir = tempfile::tempdir().expect("temp dir");
        let marker = dir.path().join("parser-must-not-execute.txt");
        let command = format!("Set-Content -Path '{}' -Value owned", marker.display());
        let Some(accepted) = native_powershell_accepts(&command) else {
            return;
        };
        assert!(accepted, "fixture should be valid PowerShell");
        assert!(
            !marker.exists(),
            "native parser adapter must never evaluate classified input"
        );
    }
}
