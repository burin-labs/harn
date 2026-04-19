use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use harn_parser::{DiagnosticSeverity, Node, SNode, TypeChecker};

use crate::cli::PlaygroundArgs;
use crate::commands::run::{
    connect_mcp_servers, install_cli_llm_mock_mode, persist_cli_llm_mock_recording, CliLlmMockMode,
};
use crate::package;
use crate::skill_loader::{
    emit_loader_warnings, install_skills_global, load_skills, SkillLoaderInputs,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct LlmOverride {
    provider: String,
    model: String,
}

#[derive(Clone, Debug)]
struct PlaygroundConfig {
    host: PathBuf,
    script: PathBuf,
    task: String,
    llm: Option<LlmOverride>,
    llm_mock_mode: CliLlmMockMode,
}

pub(crate) async fn run_command(
    args: PlaygroundArgs,
    llm_mock_mode: CliLlmMockMode,
) -> Result<(), String> {
    let config = PlaygroundConfig {
        host: canonicalize_or_err(&args.host)?,
        script: canonicalize_or_err(&args.script)?,
        task: args.task.unwrap_or_default(),
        llm: args.llm.as_deref().map(parse_llm_override).transpose()?,
        llm_mock_mode,
    };

    if args.watch {
        run_watch(&config).await
    } else {
        let output = execute_playground(&config).await?;
        if !output.is_empty() {
            io::stdout()
                .write_all(output.as_bytes())
                .map_err(|error| format!("failed to write playground output: {error}"))?;
        }
        Ok(())
    }
}

async fn run_watch(config: &PlaygroundConfig) -> Result<(), String> {
    use notify::{Event, EventKind, RecursiveMode, Watcher};

    eprintln!(
        "\x1b[2m[playground] running {} with host {}...\x1b[0m",
        config.script.display(),
        config.host.display()
    );
    emit_run_result(execute_playground(config).await);

    let roots = watch_roots(&config.host, &config.script);
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
    let _watcher = {
        let tx = tx.clone();
        let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
            if let Ok(event) = res {
                if matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                ) {
                    let has_harn = event
                        .paths
                        .iter()
                        .any(|path| path.extension().is_some_and(|ext| ext == "harn"));
                    if has_harn {
                        let _ = tx.blocking_send(());
                    }
                }
            }
        })
        .map_err(|error| format!("failed to create playground watcher: {error}"))?;

        for root in &roots {
            watcher
                .watch(root, RecursiveMode::Recursive)
                .map_err(|error| format!("failed to watch {}: {error}", root.display()))?;
        }
        watcher
    };

    eprintln!(
        "\x1b[2m[playground] watching {} (ctrl-c to stop)\x1b[0m",
        roots
            .iter()
            .map(|path| path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );

    loop {
        rx.recv().await;
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        while rx.try_recv().is_ok() {}

        eprintln!();
        eprintln!(
            "\x1b[2m[playground] change detected, re-running {}...\x1b[0m",
            config.script.display()
        );
        emit_run_result(execute_playground(config).await);
    }
}

fn emit_run_result(result: Result<String, String>) {
    match result {
        Ok(output) => {
            if !output.is_empty() {
                let _ = io::stdout().write_all(output.as_bytes());
            }
        }
        Err(error) => eprint!("{error}"),
    }
}

async fn execute_playground(config: &PlaygroundConfig) -> Result<String, String> {
    let (host_source, host_program) = crate::parse_source_file(&config.host.to_string_lossy());
    typecheck_program(&host_source, &host_program, &config.host, &HashSet::new())?;
    let host_exports = exported_host_functions(&host_program);

    let (script_source, script_program) =
        crate::parse_source_file(&config.script.to_string_lossy());
    typecheck_program(
        &script_source,
        &script_program,
        &config.script,
        &host_exports,
    )?;

    let chunk = harn_vm::Compiler::new()
        .compile(&script_program)
        .map_err(|error| format!("error: compile error: {error}\n"))?;

    let env_guard = ScopedEnv::apply(config);
    let source_parent = config
        .script
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let project_root = harn_vm::stdlib::process::find_project_root(&source_parent);
    let store_base = project_root.as_deref().unwrap_or(source_parent.as_path());
    let execution_cwd = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .to_string_lossy()
        .into_owned();
    let source_dir = source_parent.to_string_lossy().into_owned();

    let local = tokio::task::LocalSet::new();
    let result = local
        .run_until(async {
            install_cli_llm_mock_mode(&config.llm_mock_mode)
                .map_err(|error| format!("error: {error}\n"))?;
            let host_vm = configured_vm(
                &config.host,
                &host_source,
                project_root.as_deref(),
                store_base,
            )
            .await?;
            let bridge = Rc::new(
                harn_vm::bridge::HostBridge::from_harn_module(host_vm, &config.host)
                    .await
                    .map_err(|error| format!("error: {error}\n"))?,
            );

            let mut vm = configured_vm(
                &config.script,
                &script_source,
                project_root.as_deref(),
                store_base,
            )
            .await?;
            vm.set_bridge(bridge.clone());
            harn_vm::llm::install_current_host_bridge(bridge.clone());
            harn_vm::stdlib::process::set_thread_execution_context(Some(
                harn_vm::orchestration::RunExecutionRecord {
                    cwd: Some(execution_cwd),
                    source_dir: Some(source_dir),
                    env: std::collections::BTreeMap::new(),
                    adapter: None,
                    repo_path: None,
                    worktree_path: None,
                    branch: None,
                    base_ref: None,
                    cleanup: None,
                },
            ));
            let execution_result = match vm.execute(&chunk).await {
                Ok(_) => Ok(vm.output().to_string()),
                Err(error) => Err(vm.format_runtime_error(&error)),
            };
            harn_vm::llm::clear_current_host_bridge();
            harn_vm::stdlib::process::set_thread_execution_context(None);
            persist_cli_llm_mock_recording(&config.llm_mock_mode)
                .map_err(|error| format!("error: {error}\n"))?;
            execution_result
        })
        .await;
    drop(env_guard);
    result
}

async fn configured_vm(
    path: &Path,
    source: &str,
    project_root: Option<&Path>,
    store_base: &Path,
) -> Result<harn_vm::Vm, String> {
    let mut vm = harn_vm::Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    harn_vm::register_store_builtins(&mut vm, store_base);
    harn_vm::register_metadata_builtins(&mut vm, store_base);
    let pipeline_name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("default");
    harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
    vm.set_source_info(&path.to_string_lossy(), source);
    if let Some(root) = project_root {
        vm.set_project_root(root);
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            vm.set_source_dir(parent);
        }
    }
    vm.set_global("argv", harn_vm::VmValue::List(Rc::new(Vec::new())));

    let loaded = load_skills(&SkillLoaderInputs {
        cli_dirs: Vec::new(),
        source_path: Some(path.to_path_buf()),
    });
    emit_loader_warnings(&loaded.loader_warnings);
    install_skills_global(&mut vm, &loaded);

    if let Some(manifest) = package::try_read_manifest_for(path) {
        package::install_capability_overrides(&manifest);
        if !manifest.mcp.is_empty() {
            connect_mcp_servers(&manifest.mcp, &mut vm).await;
        }
    }

    Ok(vm)
}

fn typecheck_program(
    source: &str,
    program: &[SNode],
    path: &Path,
    extra_names: &HashSet<String>,
) -> Result<(), String> {
    let graph = harn_modules::build(&[path.to_path_buf()]);
    let mut checker = TypeChecker::new();
    let mut imported = graph.imported_names_for_file(path).unwrap_or_default();
    imported.extend(extra_names.iter().cloned());
    if !imported.is_empty() {
        checker = checker.with_imported_names(imported);
    }

    let diagnostics = checker.check(program);
    let mut rendered = String::new();
    let mut had_error = false;
    for diagnostic in &diagnostics {
        let severity = match diagnostic.severity {
            DiagnosticSeverity::Error => {
                had_error = true;
                "error"
            }
            DiagnosticSeverity::Warning => "warning",
        };
        if let Some(span) = &diagnostic.span {
            rendered.push_str(&harn_parser::diagnostic::render_diagnostic(
                source,
                &path.to_string_lossy(),
                span,
                severity,
                &diagnostic.message,
                None,
                diagnostic.help.as_deref(),
            ));
        } else {
            rendered.push_str(&format!("{severity}: {}\n", diagnostic.message));
        }
    }

    if had_error {
        return Err(rendered);
    }
    if !rendered.is_empty() {
        eprint!("{rendered}");
    }
    Ok(())
}

fn exported_host_functions(program: &[SNode]) -> HashSet<String> {
    let mut public_names = HashSet::new();
    let mut all_names = HashSet::new();
    let mut has_pub_fn = false;

    for node in program {
        let inner = match &node.node {
            Node::AttributedDecl { inner, .. } => inner.as_ref(),
            _ => node,
        };
        let Node::FnDecl { name, is_pub, .. } = &inner.node else {
            continue;
        };
        all_names.insert(name.clone());
        if *is_pub {
            has_pub_fn = true;
            public_names.insert(name.clone());
        }
    }

    if has_pub_fn {
        public_names
    } else {
        all_names
    }
}

fn watch_roots(host: &Path, script: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for candidate in [
        host.parent().unwrap_or_else(|| Path::new(".")),
        script.parent().unwrap_or_else(|| Path::new(".")),
    ] {
        if !roots.iter().any(|existing| existing == candidate) {
            roots.push(candidate.to_path_buf());
        }
    }
    roots
}

fn parse_llm_override(raw: &str) -> Result<LlmOverride, String> {
    let (provider, model) = raw
        .split_once(':')
        .ok_or_else(|| "playground --llm expects provider:model".to_string())?;
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() || model.is_empty() {
        return Err("playground --llm expects provider:model".to_string());
    }
    Ok(LlmOverride {
        provider: provider.to_string(),
        model: model.to_string(),
    })
}

fn canonicalize_or_err(path: &str) -> Result<PathBuf, String> {
    std::fs::canonicalize(path).map_err(|error| format!("failed to resolve {path}: {error}"))
}

struct ScopedEnv {
    previous: Vec<(String, Option<String>)>,
}

impl ScopedEnv {
    fn apply(config: &PlaygroundConfig) -> Self {
        let mut previous = Vec::new();
        Self::set("HARN_TASK", Some(config.task.as_str()), &mut previous);
        if let Some(llm) = &config.llm {
            Self::set(
                "HARN_LLM_PROVIDER",
                Some(llm.provider.as_str()),
                &mut previous,
            );
            Self::set("HARN_LLM_MODEL", Some(llm.model.as_str()), &mut previous);
        }
        Self { previous }
    }

    fn set(key: &str, value: Option<&str>, previous: &mut Vec<(String, Option<String>)>) {
        previous.push((key.to_string(), std::env::var(key).ok()));
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        for (key, previous) in self.previous.iter().rev() {
            match previous {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_file(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn exported_host_functions_prefers_pub_names() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("host_pub.harn");
        let source = r#"
fn helper() {}
pub fn run_shell(command) { return command }
pub fn request_permission(tool_name, request_args) { return true }
"#;
        write_file(&path, source);
        let (_, program) = crate::parse_source_file(path.to_string_lossy().as_ref());
        let names = exported_host_functions(&program);
        assert!(names.contains("run_shell"));
        assert!(names.contains("request_permission"));
        assert!(!names.contains("helper"));
    }

    #[test]
    fn parse_llm_override_splits_provider_and_model() {
        let parsed = parse_llm_override("ollama:qwen2.5-coder:latest").unwrap();
        assert_eq!(parsed.provider, "ollama");
        assert_eq!(parsed.model, "qwen2.5-coder:latest");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn playground_executes_host_backed_script() {
        let temp = tempfile::tempdir().unwrap();
        let host = temp.path().join("host.harn");
        let script = temp.path().join("pipeline.harn");
        write_file(
            &host,
            r#"
pub fn build_prompt(task) {
  return "prompt: " + task
}
"#,
        );
        write_file(
            &script,
            r#"
pipeline default(task) {
  llm_mock({text: "done"})
  let result = llm_call(build_prompt(env_or("HARN_TASK", "")), "You are concise.")
  println(result.text)
}
"#,
        );

        let output = execute_playground(&PlaygroundConfig {
            host,
            script,
            task: "ship it".to_string(),
            llm: Some(LlmOverride {
                provider: "mock".to_string(),
                model: "mock".to_string(),
            }),
            llm_mock_mode: CliLlmMockMode::Off,
        })
        .await
        .unwrap();

        assert!(output.contains("done"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn playground_reports_missing_capability_with_caller_context() {
        let temp = tempfile::tempdir().unwrap();
        let host = temp.path().join("host.harn");
        let script = temp.path().join("pipeline.harn");
        write_file(
            &host,
            r#"
pub fn helper() {
  return "ok"
}
"#,
        );
        write_file(
            &script,
            r#"
pipeline default(task) {
  run_shell("pwd")
}
"#,
        );

        let error = execute_playground(&PlaygroundConfig {
            host,
            script,
            task: String::new(),
            llm: None,
            llm_mock_mode: CliLlmMockMode::Off,
        })
        .await
        .unwrap_err();

        assert!(error.contains("run_shell"));
        assert!(error.contains("pipeline.harn:3:3"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn playground_replays_cli_llm_mock_fixtures() {
        let temp = tempfile::tempdir().unwrap();
        let host = temp.path().join("host.harn");
        let script = temp.path().join("pipeline.harn");
        let fixtures = temp.path().join("fixtures.jsonl");
        write_file(
            &host,
            r#"
pub fn build_prompt(task) {
  return "prompt: " + task
}
"#,
        );
        write_file(
            &script,
            r#"
pipeline default(task) {
  let result = llm_call(build_prompt(env_or("HARN_TASK", "")), "You are concise.")
  println(result.text)
}
"#,
        );
        write_file(
            &fixtures,
            r#"{"text":"fixture replay","model":"fixture-model"}
"#,
        );

        let output = execute_playground(&PlaygroundConfig {
            host,
            script,
            task: "ship it".to_string(),
            llm: Some(LlmOverride {
                provider: "anthropic".to_string(),
                model: "claude-sonnet".to_string(),
            }),
            llm_mock_mode: CliLlmMockMode::Replay {
                fixture_path: fixtures,
            },
        })
        .await
        .unwrap();

        assert!(output.contains("fixture replay"));
    }
}
