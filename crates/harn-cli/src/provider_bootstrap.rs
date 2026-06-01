use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use harn_parser::{BindingPattern, Node, SNode};

const OLLAMA_TAGS_URL: &str = "http://127.0.0.1:11434/api/tags";
const PROVIDER_BACKED_LLM_BUILTINS: &[&str] = &[
    "agent_loop",
    "agent_turn",
    "llm_call",
    "llm_call_safe",
    "llm_call_structured",
    "llm_call_structured_safe",
    "llm_call_structured_result",
    "llm_completion",
    "llm_healthcheck",
    "llm_stream",
    "llm_stream_call",
];
const OLLAMA_PROVIDERS_TOML: &str = r#"default_provider = "ollama"

[providers.ollama]
display_name = "Ollama"
base_url = "http://localhost:11434"
base_url_env = "OLLAMA_HOST"
auth_style = "none"
chat_endpoint = "/api/chat"
completion_endpoint = "/api/generate"

[providers.ollama.healthcheck]
method = "GET"
path = "/api/tags"
"#;

pub(crate) async fn maybe_seed_ollama_for_run_file(path: &Path, yes: bool, replay_llm: bool) {
    if replay_llm || !harn_file_tree_uses_llm(path) {
        return;
    }
    if let Err(error) = maybe_seed_ollama_config(path, yes).await {
        crate::command_error(&error);
    }
}

pub(crate) async fn maybe_seed_ollama_for_inline(source: &str, yes: bool, replay_llm: bool) {
    if replay_llm {
        return;
    }
    let anchor = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("harn-eval-inline.harn");
    let mut visited = HashSet::new();
    if !source_tree_uses_llm(&anchor, source, &mut visited) {
        return;
    }
    if let Err(error) = maybe_seed_ollama_config(&anchor, yes).await {
        crate::command_error(&error);
    }
}

pub(crate) async fn maybe_seed_ollama_for_playground(
    host: &Path,
    script: &Path,
    yes: bool,
    explicit_llm: bool,
    replay_llm: bool,
) {
    if explicit_llm || replay_llm {
        return;
    }
    if !harn_file_tree_uses_llm(script) && !harn_file_tree_uses_llm(host) {
        return;
    }
    if let Err(error) = maybe_seed_ollama_config(script, yes).await {
        crate::command_error(&error);
    }
}

async fn maybe_seed_ollama_config(anchor: &Path, yes: bool) -> Result<(), String> {
    if provider_override_env_present() || project_llm_configured(anchor) {
        return Ok(());
    }
    let Some(config_path) = default_providers_path() else {
        return Ok(());
    };
    if config_path.exists() {
        return Ok(());
    }
    if !ollama_tags_ready(OLLAMA_TAGS_URL).await {
        return Ok(());
    }
    if !yes && !confirm_write(&config_path)? {
        return Ok(());
    }
    write_ollama_config(&config_path)
}

fn provider_override_env_present() -> bool {
    env_has_value("HARN_PROVIDERS_CONFIG")
        || env_has_value("HARN_LLM_PROVIDER")
        || env_has_non_auto_value("HARN_DEFAULT_PROVIDER")
        || env_has_value("LOCAL_LLM_BASE_URL")
        || env_has_value("LOCAL_LLM_MODEL")
        || env_has_value("MLX_BASE_URL")
        || env_has_value("MLX_MODEL_ID")
        || env_has_value("VLLM_BASE_URL")
        || env_has_value("TGI_BASE_URL")
}

fn env_has_value(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .is_some_and(|value| !value.trim().is_empty())
}

fn env_has_non_auto_value(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|value| {
        let trimmed = value.trim();
        !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case("auto")
    })
}

fn project_llm_configured(anchor: &Path) -> bool {
    crate::package::find_nearest_manifest(anchor)
        .map(|(manifest, _)| !manifest.llm.is_empty())
        .unwrap_or(false)
}

fn default_providers_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .filter(|home| !home.trim().is_empty())
        .map(|home| PathBuf::from(home).join(".config/harn/providers.toml"))
}

async fn ollama_tags_ready(url: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(750))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    client
        .get(url)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

fn confirm_write(config_path: &Path) -> Result<bool, String> {
    if !io::stdin().is_terminal() {
        eprintln!(
            "Harn found local Ollama at {OLLAMA_TAGS_URL}, but no provider config is set. \
             Re-run with --yes to write {}.",
            config_path.display()
        );
        return Ok(false);
    }

    eprintln!(
        "Harn found local Ollama at {OLLAMA_TAGS_URL} and no provider config at {}.",
        config_path.display()
    );
    eprint!("Write providers.toml with Ollama as the default provider? [y/N] ");
    io::stderr()
        .flush()
        .map_err(|error| format!("failed to flush prompt: {error}"))?;

    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map_err(|error| format!("failed to read prompt response: {error}"))?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn write_ollama_config(config_path: &Path) -> Result<(), String> {
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    let mut file = match OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(config_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => {
            return Err(format!(
                "failed to create {}: {error}",
                config_path.display()
            ));
        }
    };
    file.write_all(OLLAMA_PROVIDERS_TOML.as_bytes())
        .map_err(|error| format!("failed to write {}: {error}", config_path.display()))?;
    eprintln!(
        "Configured Ollama as Harn's default provider at {}.",
        config_path.display()
    );
    Ok(())
}

fn harn_file_tree_uses_llm(path: &Path) -> bool {
    let Ok(source) = fs::read_to_string(path) else {
        return false;
    };
    let mut visited = HashSet::new();
    source_tree_uses_llm(path, &source, &mut visited)
}

fn source_tree_uses_llm(path: &Path, source: &str, visited: &mut HashSet<PathBuf>) -> bool {
    let key = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if !visited.insert(key) {
        return false;
    }
    if source_text_uses_provider_llm(source) {
        return true;
    }
    for import_path in import_paths(source) {
        let Some(resolved) = harn_modules::resolve_import_path(path, &import_path) else {
            continue;
        };
        let Some(imported_source) = harn_modules::read_module_source(&resolved) else {
            continue;
        };
        if source_tree_uses_llm(&resolved, &imported_source, visited) {
            return true;
        }
    }
    false
}

fn source_text_uses_provider_llm(source: &str) -> bool {
    let Ok(program) = harn_parser::parse_source(source) else {
        return false;
    };
    nodes_use_provider_llm(&program, &HashSet::new())
}

fn import_paths(source: &str) -> Vec<String> {
    let Ok(program) = harn_parser::parse_source(source) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for node in &program {
        collect_import_path(node, &mut paths);
    }
    paths
}

fn collect_import_path(node: &SNode, paths: &mut Vec<String>) {
    match &node.node {
        Node::ImportDecl { path, .. } | Node::SelectiveImport { path, .. } => {
            paths.push(path.clone());
        }
        Node::AttributedDecl { inner, .. } => collect_import_path(inner, paths),
        _ => {}
    }
}

fn nodes_use_provider_llm(nodes: &[SNode], inherited_shadows: &HashSet<String>) -> bool {
    let mut shadows = inherited_shadows.clone();
    for node in nodes {
        collect_function_like_shadow(node, &mut shadows);
    }

    for node in nodes {
        match &node.node {
            Node::LetBinding { pattern, value, .. } | Node::VarBinding { pattern, value, .. } => {
                if pattern_uses_provider_llm(pattern, &shadows)
                    || node_uses_provider_llm(value, &shadows)
                {
                    return true;
                }
                collect_binding_shadows(pattern, &mut shadows);
            }
            _ if node_uses_provider_llm(node, &shadows) => return true,
            _ => {}
        }
    }
    false
}

fn node_uses_provider_llm(node: &SNode, shadows: &HashSet<String>) -> bool {
    match &node.node {
        Node::FunctionCall { name, args, .. } => {
            (provider_backed_llm_builtin(name) && !shadows.contains(name))
                || nodes_use_provider_llm(args, shadows)
        }
        Node::AttributedDecl { inner, .. } => node_uses_provider_llm(inner, shadows),
        Node::Pipeline { body, .. }
        | Node::OverrideDecl { body, .. }
        | Node::FnDecl { body, .. }
        | Node::ToolDecl { body, .. }
        | Node::SpawnExpr { body }
        | Node::DeferStmt { body }
        | Node::MutexBlock { body, .. }
        | Node::TryExpr { body }
        | Node::Block(body)
        | Node::Closure { body, .. } => nodes_use_provider_llm(body, shadows),
        Node::ImplBlock { methods, .. } => nodes_use_provider_llm(methods, shadows),
        Node::LetBinding { pattern, value, .. } | Node::VarBinding { pattern, value, .. } => {
            pattern_uses_provider_llm(pattern, shadows) || node_uses_provider_llm(value, shadows)
        }
        Node::ConstBinding { value, .. } => node_uses_provider_llm(value, shadows),
        Node::IfElse {
            condition,
            then_body,
            else_body,
        } => {
            node_uses_provider_llm(condition, shadows)
                || nodes_use_provider_llm(then_body, shadows)
                || else_body
                    .as_ref()
                    .is_some_and(|body| nodes_use_provider_llm(body, shadows))
        }
        Node::ForIn {
            pattern,
            iterable,
            body,
        } => {
            if node_uses_provider_llm(iterable, shadows) {
                return true;
            }
            let mut body_shadows = shadows.clone();
            collect_binding_shadows(pattern, &mut body_shadows);
            nodes_use_provider_llm(body, &body_shadows)
        }
        Node::WhileLoop { condition, body } => {
            node_uses_provider_llm(condition, shadows) || nodes_use_provider_llm(body, shadows)
        }
        Node::Retry { count, body } => {
            node_uses_provider_llm(count, shadows) || nodes_use_provider_llm(body, shadows)
        }
        Node::DeadlineBlock { duration, body } => {
            node_uses_provider_llm(duration, shadows) || nodes_use_provider_llm(body, shadows)
        }
        Node::CostRoute { options, body } => {
            options
                .iter()
                .any(|(_, value)| node_uses_provider_llm(value, shadows))
                || nodes_use_provider_llm(body, shadows)
        }
        Node::Parallel {
            expr,
            body,
            options,
            ..
        } => {
            node_uses_provider_llm(expr, shadows)
                || options
                    .iter()
                    .any(|(_, value)| node_uses_provider_llm(value, shadows))
                || nodes_use_provider_llm(body, shadows)
        }
        Node::SelectExpr {
            cases,
            timeout,
            default_body,
        } => {
            cases.iter().any(|case| {
                node_uses_provider_llm(&case.channel, shadows)
                    || nodes_use_provider_llm(&case.body, shadows)
            }) || timeout.as_ref().is_some_and(|(duration, body)| {
                node_uses_provider_llm(duration, shadows) || nodes_use_provider_llm(body, shadows)
            }) || default_body
                .as_ref()
                .is_some_and(|body| nodes_use_provider_llm(body, shadows))
        }
        Node::MatchExpr { value, arms } => {
            node_uses_provider_llm(value, shadows)
                || arms.iter().any(|arm| {
                    node_uses_provider_llm(&arm.pattern, shadows)
                        || arm
                            .guard
                            .as_ref()
                            .is_some_and(|guard| node_uses_provider_llm(guard, shadows))
                        || nodes_use_provider_llm(&arm.body, shadows)
                })
        }
        Node::TryCatch {
            has_catch: _,
            body,
            catch_body,
            finally_body,
            ..
        } => {
            nodes_use_provider_llm(body, shadows)
                || nodes_use_provider_llm(catch_body, shadows)
                || finally_body
                    .as_ref()
                    .is_some_and(|body| nodes_use_provider_llm(body, shadows))
        }
        Node::ReturnStmt { value: Some(value) }
        | Node::ThrowStmt { value }
        | Node::EmitExpr { value }
        | Node::YieldExpr { value: Some(value) }
        | Node::Spread(value)
        | Node::TryOperator { operand: value }
        | Node::TryStar { operand: value }
        | Node::UnaryOp { operand: value, .. } => node_uses_provider_llm(value, shadows),
        Node::GuardStmt {
            condition,
            else_body,
        } => {
            node_uses_provider_llm(condition, shadows) || nodes_use_provider_llm(else_body, shadows)
        }
        Node::RequireStmt { condition, message } => {
            node_uses_provider_llm(condition, shadows)
                || message
                    .as_ref()
                    .is_some_and(|message| node_uses_provider_llm(message, shadows))
        }
        Node::HitlExpr { args, .. } => args
            .iter()
            .any(|arg| node_uses_provider_llm(&arg.value, shadows)),
        Node::MethodCall { object, args, .. } | Node::OptionalMethodCall { object, args, .. } => {
            node_uses_provider_llm(object, shadows) || nodes_use_provider_llm(args, shadows)
        }
        Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
            node_uses_provider_llm(object, shadows)
        }
        Node::SubscriptAccess { object, index }
        | Node::OptionalSubscriptAccess { object, index } => {
            node_uses_provider_llm(object, shadows) || node_uses_provider_llm(index, shadows)
        }
        Node::SliceAccess { object, start, end } => {
            node_uses_provider_llm(object, shadows)
                || start
                    .as_ref()
                    .is_some_and(|start| node_uses_provider_llm(start, shadows))
                || end
                    .as_ref()
                    .is_some_and(|end| node_uses_provider_llm(end, shadows))
        }
        Node::BinaryOp { left, right, .. }
        | Node::RangeExpr {
            start: left,
            end: right,
            ..
        } => node_uses_provider_llm(left, shadows) || node_uses_provider_llm(right, shadows),
        Node::Ternary {
            condition,
            true_expr,
            false_expr,
        } => {
            node_uses_provider_llm(condition, shadows)
                || node_uses_provider_llm(true_expr, shadows)
                || node_uses_provider_llm(false_expr, shadows)
        }
        Node::Assignment { target, value, .. } => {
            node_uses_provider_llm(target, shadows) || node_uses_provider_llm(value, shadows)
        }
        Node::EnumConstruct { args, .. } | Node::ListLiteral(args) | Node::OrPattern(args) => {
            nodes_use_provider_llm(args, shadows)
        }
        Node::StructConstruct { fields, .. } | Node::DictLiteral(fields) => {
            fields.iter().any(|entry| {
                node_uses_provider_llm(&entry.key, shadows)
                    || node_uses_provider_llm(&entry.value, shadows)
            })
        }
        Node::SkillDecl { fields, .. } => fields
            .iter()
            .any(|(_, value)| node_uses_provider_llm(value, shadows)),
        Node::EvalPackDecl {
            fields,
            body,
            summarize,
            ..
        } => {
            fields
                .iter()
                .any(|(_, value)| node_uses_provider_llm(value, shadows))
                || nodes_use_provider_llm(body, shadows)
                || summarize
                    .as_ref()
                    .is_some_and(|body| nodes_use_provider_llm(body, shadows))
        }
        Node::ImportDecl { .. }
        | Node::SelectiveImport { .. }
        | Node::EnumDecl { .. }
        | Node::StructDecl { .. }
        | Node::InterfaceDecl { .. }
        | Node::TypeDecl { .. }
        | Node::DurationLiteral(_)
        | Node::InterpolatedString(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::BoolLiteral(_)
        | Node::NilLiteral
        | Node::Identifier(_)
        | Node::ReturnStmt { value: None }
        | Node::YieldExpr { value: None }
        | Node::BreakStmt
        | Node::ContinueStmt => false,
    }
}

fn provider_backed_llm_builtin(name: &str) -> bool {
    PROVIDER_BACKED_LLM_BUILTINS.contains(&name)
}

fn collect_function_like_shadow(node: &SNode, shadows: &mut HashSet<String>) {
    let node = match &node.node {
        Node::AttributedDecl { inner, .. } => inner.as_ref(),
        _ => node,
    };
    let name = match &node.node {
        Node::Pipeline { name, .. }
        | Node::FnDecl { name, .. }
        | Node::ToolDecl { name, .. }
        | Node::OverrideDecl { name, .. } => name,
        _ => return,
    };
    if provider_backed_llm_builtin(name) {
        shadows.insert(name.clone());
    }
}

fn collect_binding_shadows(pattern: &BindingPattern, shadows: &mut HashSet<String>) {
    for name in binding_pattern_names(pattern) {
        if provider_backed_llm_builtin(&name) {
            shadows.insert(name);
        }
    }
}

fn binding_pattern_names(pattern: &BindingPattern) -> Vec<String> {
    match pattern {
        BindingPattern::Identifier(name) => vec![name.clone()],
        BindingPattern::Pair(left, right) => vec![left.clone(), right.clone()],
        BindingPattern::Dict(fields) => fields
            .iter()
            .filter(|field| !field.is_rest)
            .map(|field| field.alias.clone().unwrap_or_else(|| field.key.clone()))
            .collect(),
        BindingPattern::List(items) => items.iter().map(|item| item.name.clone()).collect(),
    }
}

fn pattern_uses_provider_llm(pattern: &BindingPattern, shadows: &HashSet<String>) -> bool {
    match pattern {
        BindingPattern::Identifier(_) | BindingPattern::Pair(_, _) => false,
        BindingPattern::Dict(fields) => fields.iter().any(|field| {
            field
                .default_value
                .as_ref()
                .is_some_and(|value| node_uses_provider_llm(value, shadows))
        }),
        BindingPattern::List(items) => items.iter().any(|item| {
            item.default_value
                .as_ref()
                .is_some_and(|value| node_uses_provider_llm(value, shadows))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_llm_calls_without_matching_strings() {
        assert!(!source_text_uses_provider_llm(
            r#"__io_println("llm_call(\"x\")")"#
        ));
        assert!(source_text_uses_provider_llm(
            r#"let response = llm_call("prompt", "system")"#
        ));
        assert!(source_text_uses_provider_llm(
            r#"agent_loop("task", "system", {})"#
        ));
    }

    #[test]
    fn detects_provider_backed_llm_surfaces() {
        for builtin in [
            "agent_turn",
            "llm_call_safe",
            "llm_call_structured",
            "llm_call_structured_safe",
            "llm_call_structured_result",
            "llm_completion",
            "llm_healthcheck",
            "llm_stream",
            "llm_stream_call",
        ] {
            let source = format!("let result = {builtin}(\"prompt\")");
            assert!(source_text_uses_provider_llm(&source), "{builtin}");
        }
    }

    #[test]
    fn ignores_provider_builtin_names_shadowed_by_user_code() {
        assert!(!source_text_uses_provider_llm(
            r#"
fn llm_call(prompt, system) {
  return "local"
}
__io_println(llm_call("prompt", "system"))
"#
        ));
        assert!(!source_text_uses_provider_llm(
            r#"
let llm_call = { prompt, system -> "local" }
__io_println(llm_call("prompt", "system"))
"#
        ));
    }

    #[test]
    fn detects_llm_calls_in_imports() {
        let temp = tempfile::tempdir().unwrap();
        let main = temp.path().join("main.harn");
        let imported = temp.path().join("agent.harn");
        fs::write(&main, r#"import "agent""#).unwrap();
        fs::write(
            &imported,
            r#"pub fn run() { return agent_loop("task", "system", {}) }"#,
        )
        .unwrap();

        assert!(harn_file_tree_uses_llm(&main));
    }

    #[test]
    fn seeded_ollama_config_is_minimal_and_parseable() {
        let config: harn_vm::llm_config::ProvidersConfig =
            toml::from_str(OLLAMA_PROVIDERS_TOML).unwrap();
        assert_eq!(config.default_provider.as_deref(), Some("ollama"));
        let ollama = config.providers.get("ollama").unwrap();
        assert_eq!(ollama.base_url, "http://localhost:11434");
        assert_eq!(ollama.base_url_env.as_deref(), Some("OLLAMA_HOST"));
        assert_eq!(ollama.auth_style, "none");
        assert_eq!(ollama.chat_endpoint, "/api/chat");
        assert_eq!(ollama.completion_endpoint.as_deref(), Some("/api/generate"));
        assert_eq!(
            ollama
                .healthcheck
                .as_ref()
                .and_then(|check| check.path.as_deref()),
            Some("/api/tags")
        );
    }

    #[test]
    fn write_ollama_config_creates_parent_and_never_overwrites() {
        let temp = tempfile::tempdir().unwrap();
        let config_path = temp.path().join(".config/harn/providers.toml");

        write_ollama_config(&config_path).unwrap();
        assert_eq!(
            fs::read_to_string(&config_path).unwrap(),
            OLLAMA_PROVIDERS_TOML
        );

        fs::write(&config_path, "sentinel").unwrap();
        write_ollama_config(&config_path).unwrap();
        assert_eq!(fs::read_to_string(&config_path).unwrap(), "sentinel");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn local_provider_env_counts_as_configured() {
        let _guard = crate::tests::common::env_lock::lock_env().lock().await;
        let previous_base = std::env::var("LOCAL_LLM_BASE_URL").ok();
        let previous_model = std::env::var("LOCAL_LLM_MODEL").ok();
        std::env::set_var("LOCAL_LLM_BASE_URL", "http://127.0.0.1:8000");
        std::env::set_var("LOCAL_LLM_MODEL", "qwen2.5-coder-32b");

        assert!(provider_override_env_present());

        match previous_base {
            Some(value) => std::env::set_var("LOCAL_LLM_BASE_URL", value),
            None => std::env::remove_var("LOCAL_LLM_BASE_URL"),
        }
        match previous_model {
            Some(value) => std::env::set_var("LOCAL_LLM_MODEL", value),
            None => std::env::remove_var("LOCAL_LLM_MODEL"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn ollama_probe_requires_success_status() {
        let ok_url = spawn_status_server("200 OK").await;
        assert!(ollama_tags_ready(&ok_url).await);

        let not_found_url = spawn_status_server("404 Not Found").await;
        assert!(!ollama_tags_ready(&not_found_url).await);
    }

    async fn spawn_status_server(status: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf).await;
            let response = format!("HTTP/1.1 {status}\r\ncontent-length: 2\r\n\r\n{{}}");
            let _ = stream.write_all(response.as_bytes()).await;
        });
        format!("http://{addr}/api/tags")
    }
}
