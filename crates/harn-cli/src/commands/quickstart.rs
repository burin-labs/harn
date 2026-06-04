use std::collections::BTreeSet;
use std::env;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::cli::QuickstartArgs;
use crate::commands::local_readiness;

const QUICKSTART_ALIAS: &str = "quickstart";
const PREFERRED_PROVIDERS: &[&str] = &[
    "ollama",
    "anthropic",
    "openai",
    "openrouter",
    "gemini",
    "together",
    "huggingface",
    "local",
    "mlx",
];

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderChoice {
    name: String,
    model: String,
    auth_envs: Vec<String>,
    auth_available: bool,
}

impl ProviderChoice {
    fn needs_key(&self) -> bool {
        !self.auth_envs.is_empty() && !self.auth_available
    }

    fn primary_auth_env(&self) -> Option<&str> {
        self.auth_envs.first().map(String::as_str)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderSelection {
    provider: String,
    model: String,
    auth_env: Option<String>,
    api_key: Option<String>,
    write_placeholder_key: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OllamaProbe {
    reachable: bool,
    status: String,
    message: String,
    available_models: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiskProbe {
    available_kib: Option<u64>,
    detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GpuProbe {
    detected: bool,
    detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct QuickstartPaths {
    cwd: PathBuf,
    harn_toml: PathBuf,
    env_file: PathBuf,
    providers_toml: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileAction {
    path: PathBuf,
    status: &'static str,
    detail: String,
}

pub(crate) async fn run_quickstart(args: &QuickstartArgs) -> Result<(), String> {
    let cwd = env::current_dir().map_err(|error| format!("failed to read cwd: {error}"))?;
    let paths = QuickstartPaths {
        harn_toml: cwd.join("harn.toml"),
        env_file: cwd.join(".env"),
        providers_toml: providers_config_path(),
        cwd,
    };

    let ollama = probe_ollama(args.model.as_deref()).await;
    let disk = detect_disk_space(&paths.cwd);
    let gpu = detect_gpu();
    let choices = provider_choices(args.model.as_deref(), &ollama);
    print_probe_report(&paths, &choices, &ollama, &disk, &gpu);

    let can_prompt = !args.non_interactive && std::io::stdin().is_terminal();
    let selection = if can_prompt {
        choose_interactive(args, &choices, &ollama)?
    } else {
        choose_non_interactive(args, &choices, &ollama)?
    };

    let actions = write_quickstart_files(&paths, &selection)?;
    print_completion(&selection, &actions);
    Ok(())
}

async fn probe_ollama(model: Option<&str>) -> OllamaProbe {
    let selected_model = model
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| harn_vm::llm_config::default_model_for_provider("ollama"));
    let mut options = harn_vm::llm::OllamaReadinessOptions::new(selected_model);
    options.tags_timeout = Duration::from_secs(2);
    let result = harn_vm::llm::ollama_readiness(options).await;
    let reachable = result.http_status.is_some()
        || matches!(
            result.status.as_str(),
            "ok" | "model_missing" | "warmup_failed"
        );
    OllamaProbe {
        reachable,
        status: result.status,
        message: result.message,
        available_models: result.available_models,
    }
}

fn provider_choices(model: Option<&str>, ollama: &OllamaProbe) -> Vec<ProviderChoice> {
    let mut names: BTreeSet<String> = PREFERRED_PROVIDERS
        .iter()
        .filter(|name| harn_vm::llm_config::provider_config(name).is_some())
        .map(|name| (*name).to_string())
        .collect();
    for name in harn_vm::llm_config::provider_names() {
        names.insert(name);
    }

    let mut choices: Vec<_> = names
        .into_iter()
        .filter_map(|name| provider_choice(&name, model, ollama))
        .collect();
    choices.sort_by_key(|choice| {
        PREFERRED_PROVIDERS
            .iter()
            .position(|name| *name == choice.name)
            .unwrap_or(PREFERRED_PROVIDERS.len())
    });
    choices
}

fn provider_choice(
    name: &str,
    model: Option<&str>,
    ollama: &OllamaProbe,
) -> Option<ProviderChoice> {
    let def = harn_vm::llm_config::provider_config(name)?;
    let auth_envs = harn_vm::llm_config::auth_env_names(&def.auth_env);
    let auth_available = auth_envs.is_empty()
        || auth_envs.iter().any(|name| {
            env::var(name)
                .ok()
                .is_some_and(|value| !value.trim().is_empty())
        });
    Some(ProviderChoice {
        name: name.to_string(),
        model: default_model_for_choice(name, model, ollama),
        auth_envs,
        auth_available,
    })
}

fn default_model_for_choice(provider: &str, model: Option<&str>, ollama: &OllamaProbe) -> String {
    if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
        return model.trim().to_string();
    }
    if provider == "ollama" {
        if let Some(model) = recommended_ollama_selector(&ollama.available_models) {
            return model;
        }
        if let Some(model) = ollama.available_models.first() {
            return ollama_selector_for_model(model);
        }
    }
    harn_vm::llm_config::default_model_for_provider(provider)
}

fn recommended_ollama_selector(available_models: &[String]) -> Option<String> {
    ordered_ollama_models(available_models)
        .first()
        .map(|model| ollama_selector_for_model(model))
}

fn ollama_selector_for_model(model: &str) -> String {
    local_readiness::selector_for_provider_model("ollama", model, None)
}

fn ordered_ollama_models(available_models: &[String]) -> Vec<String> {
    local_readiness::recommended_models_for_provider("ollama", available_models)
}

fn choose_non_interactive(
    args: &QuickstartArgs,
    choices: &[ProviderChoice],
    ollama: &OllamaProbe,
) -> Result<ProviderSelection, String> {
    let choice = if let Some(provider) = args.provider.as_deref() {
        let provider = provider.trim();
        choices
            .iter()
            .find(|choice| choice.name == provider)
            .ok_or_else(|| format!("unknown provider '{provider}'"))?
    } else {
        choices
            .iter()
            .find(|choice| !choice.auth_envs.is_empty() && choice.auth_available)
            .or_else(|| {
                ollama
                    .reachable
                    .then(|| choices.iter().find(|choice| choice.name == "ollama"))
                    .flatten()
            })
            .or_else(|| {
                env::var("LOCAL_LLM_BASE_URL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
                    .and_then(|_| choices.iter().find(|choice| choice.name == "local"))
            })
            .or_else(|| choices.iter().find(|choice| choice.name == "anthropic"))
            .ok_or_else(|| "no configurable providers found".to_string())?
    };
    Ok(selection_from_choice(choice, None))
}

fn choose_interactive(
    args: &QuickstartArgs,
    choices: &[ProviderChoice],
    ollama: &OllamaProbe,
) -> Result<ProviderSelection, String> {
    let choice = if let Some(provider) = args.provider.as_deref() {
        let provider = provider.trim();
        choices
            .iter()
            .find(|choice| choice.name == provider)
            .ok_or_else(|| format!("unknown provider '{provider}'"))?
    } else {
        println!();
        println!("Choose a provider:");
        for (idx, choice) in choices.iter().enumerate() {
            println!("  {}) {}", idx + 1, format_choice(choice, ollama));
        }
        let default = default_choice_index(choices, ollama);
        let answer = read_line(&format!("provider [{}]", default + 1))?;
        let selected = if answer.trim().is_empty() {
            default
        } else {
            answer
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|value| value.checked_sub(1))
                .filter(|idx| *idx < choices.len())
                .ok_or_else(|| format!("provider choice must be 1-{}", choices.len()))?
        };
        &choices[selected]
    };

    let mut model = choice.model.clone();
    if choice.name == "ollama" && args.model.is_none() && !ollama.available_models.is_empty() {
        let ollama_models = ordered_ollama_models(&ollama.available_models);
        println!();
        println!("Choose an Ollama model:");
        for (idx, model) in ollama_models.iter().enumerate() {
            let selector = ollama_selector_for_model(model);
            if selector == *model {
                println!("  {}) {model}", idx + 1);
            } else {
                println!("  {}) {model} ({selector})", idx + 1);
            }
        }
        let answer = read_line("model [1]")?;
        if !answer.trim().is_empty() {
            let selected = answer
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|value| value.checked_sub(1))
                .filter(|idx| *idx < ollama_models.len())
                .ok_or_else(|| format!("Ollama model choice must be 1-{}", ollama_models.len()))?;
            model = ollama_selector_for_model(&ollama_models[selected]);
        } else {
            model = ollama_selector_for_model(&ollama_models[0]);
        }
    } else if args.model.is_none() {
        let answer = read_line(&format!("model [{}]", choice.model))?;
        if !answer.trim().is_empty() {
            model = answer.trim().to_string();
        }
    }

    let mut api_key = None;
    if choice.needs_key() {
        if let Some(env_name) = choice.primary_auth_env() {
            println!();
            println!("{env_name} is not set.");
            let prompt = format!("Enter {env_name} for .env, or leave blank for a placeholder: ");
            let entered = rpassword::prompt_password(prompt)
                .map_err(|error| format!("failed to read API key: {error}"))?;
            if !entered.trim().is_empty() {
                api_key = Some(entered.trim().to_string());
            }
        }
    }

    let mut selection = selection_from_choice(choice, api_key);
    selection.model = model;
    Ok(selection)
}

fn selection_from_choice(choice: &ProviderChoice, api_key: Option<String>) -> ProviderSelection {
    let auth_env = choice.primary_auth_env().map(str::to_string);
    ProviderSelection {
        provider: choice.name.clone(),
        model: choice.model.clone(),
        auth_env,
        write_placeholder_key: choice.needs_key() && api_key.is_none(),
        api_key,
    }
}

fn default_choice_index(choices: &[ProviderChoice], ollama: &OllamaProbe) -> usize {
    choices
        .iter()
        .position(|choice| !choice.auth_envs.is_empty() && choice.auth_available)
        .or_else(|| {
            ollama
                .reachable
                .then(|| choices.iter().position(|choice| choice.name == "ollama"))
                .flatten()
        })
        .or_else(|| {
            env::var("LOCAL_LLM_BASE_URL")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .and_then(|_| choices.iter().position(|choice| choice.name == "local"))
        })
        .or_else(|| choices.iter().position(|choice| choice.name == "anthropic"))
        .unwrap_or(0)
}

fn read_line(prompt_label: &str) -> Result<String, String> {
    use reedline::{DefaultPrompt, DefaultPromptSegment, Reedline, Signal};

    let mut editor = Reedline::create();
    let prompt = DefaultPrompt::new(
        DefaultPromptSegment::Basic(prompt_label.to_string()),
        DefaultPromptSegment::Empty,
    );
    match editor.read_line(&prompt) {
        Ok(Signal::Success(line)) => Ok(line),
        Ok(Signal::CtrlC | Signal::CtrlD) => Err("quickstart cancelled".to_string()),
        Ok(_) => Err("quickstart input was not accepted".to_string()),
        Err(error) => Err(format!("failed to read input: {error}")),
    }
}

fn write_quickstart_files(
    paths: &QuickstartPaths,
    selection: &ProviderSelection,
) -> Result<Vec<FileAction>, String> {
    validate_existing_harn_toml(&paths.harn_toml)?;
    Ok(vec![
        write_providers_toml(&paths.providers_toml, selection)?,
        write_or_update_harn_toml(paths, selection)?,
        write_or_update_env_file(&paths.env_file, selection)?,
    ])
}

fn validate_existing_harn_toml(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let existing = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    toml::from_str::<toml::Value>(&existing)
        .map(|_| ())
        .map_err(|error| format!("{} is not valid TOML: {error}", path.display()))
}

fn write_providers_toml(path: &Path, selection: &ProviderSelection) -> Result<FileAction, String> {
    if path.exists() {
        return Ok(FileAction {
            path: path.to_path_buf(),
            status: "skip",
            detail: "already exists".to_string(),
        });
    }

    let content = render_providers_toml(selection);
    harn_vm::atomic_io::atomic_write(path, content.as_bytes())
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    Ok(FileAction {
        path: path.to_path_buf(),
        status: "create",
        detail: "starter provider overlay".to_string(),
    })
}

fn write_or_update_harn_toml(
    paths: &QuickstartPaths,
    selection: &ProviderSelection,
) -> Result<FileAction, String> {
    let path = &paths.harn_toml;
    if !path.exists() {
        let project_name = paths
            .cwd
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("harn-project");
        let content = render_new_harn_toml(project_name, selection);
        harn_vm::atomic_io::atomic_write(path, content.as_bytes())
            .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
        return Ok(FileAction {
            path: path.clone(),
            status: "create",
            detail: "starter project manifest".to_string(),
        });
    }

    let existing = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let parsed = toml::from_str::<toml::Value>(&existing)
        .map_err(|error| format!("{} is not valid TOML: {error}", path.display()))?;
    if parsed.get("llm").is_some() {
        return Ok(FileAction {
            path: path.clone(),
            status: "skip",
            detail: "already has [llm] config".to_string(),
        });
    }

    let mut content = existing;
    if !content.ends_with('\n') {
        content.push('\n');
    }
    content.push('\n');
    content.push_str(&render_harn_llm_section(selection));
    harn_vm::atomic_io::atomic_write(path, content.as_bytes())
        .map_err(|error| format!("failed to update {}: {error}", path.display()))?;
    Ok(FileAction {
        path: path.clone(),
        status: "update",
        detail: "added [llm] defaults".to_string(),
    })
}

fn write_or_update_env_file(
    path: &Path,
    selection: &ProviderSelection,
) -> Result<FileAction, String> {
    let existing = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("failed to read {}: {error}", path.display())),
    };
    let content = merge_env_file(&existing, selection);
    if content == existing {
        return Ok(FileAction {
            path: path.to_path_buf(),
            status: "skip",
            detail: "already has quickstart env keys".to_string(),
        });
    }
    harn_vm::atomic_io::atomic_write(path, content.as_bytes())
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    Ok(FileAction {
        path: path.to_path_buf(),
        status: if existing.is_empty() {
            "create"
        } else {
            "update"
        },
        detail: "starter environment file".to_string(),
    })
}

fn render_providers_toml(selection: &ProviderSelection) -> String {
    let alias = render_quickstart_alias(selection);
    format!(
        "# Generated by harn quickstart. Harn merges this file over built-in provider definitions.\n\
         default_provider = {}\n\n\
         [aliases]\n\
         {} = {}\n",
        toml_quote(&selection.provider),
        QUICKSTART_ALIAS,
        alias
    )
}

fn render_new_harn_toml(project_name: &str, selection: &ProviderSelection) -> String {
    format!(
        "[package]\n\
         name = {}\n\
         version = \"0.1.0\"\n\n\
         [dependencies]\n\n\
         {}",
        toml_quote(project_name),
        render_harn_llm_section(selection)
    )
}

fn render_harn_llm_section(selection: &ProviderSelection) -> String {
    let alias = render_quickstart_alias(selection);
    format!(
        "[llm]\n\
         default_provider = {}\n\n\
         [llm.aliases]\n\
         {} = {}\n",
        toml_quote(&selection.provider),
        QUICKSTART_ALIAS,
        alias
    )
}

fn render_quickstart_alias(selection: &ProviderSelection) -> String {
    let resolved = harn_vm::llm_config::resolve_model_info(&selection.model);
    let (id, provider, tool_format) = if resolved.alias.is_some() {
        (resolved.id, resolved.provider, resolved.tool_format)
    } else {
        let id = harn_vm::llm_config::normalize_model_id(&selection.model);
        let tool_format = harn_vm::llm_config::default_tool_format(&id, &selection.provider);
        (id, selection.provider.clone(), tool_format)
    };
    format!(
        "{{ id = {}, provider = {}, tool_format = {} }}",
        toml_quote(&id),
        toml_quote(&provider),
        toml_quote(&tool_format)
    )
}

fn merge_env_file(existing: &str, selection: &ProviderSelection) -> String {
    let mut content = existing.to_string();
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }

    let mut additions = Vec::new();
    push_env_if_missing(
        existing,
        &mut additions,
        "HARN_LLM_PROVIDER",
        &selection.provider,
        false,
    );
    push_env_if_missing(
        existing,
        &mut additions,
        "HARN_LLM_MODEL",
        &selection.model,
        false,
    );
    if let Some(env_name) = selection.auth_env.as_deref() {
        if let Some(api_key) = selection.api_key.as_deref() {
            push_env_if_missing(existing, &mut additions, env_name, api_key, false);
        } else if selection.write_placeholder_key {
            push_env_if_missing(existing, &mut additions, env_name, "replace-me", true);
        }
    }

    if additions.is_empty() {
        return content;
    }
    if !content.is_empty() {
        content.push('\n');
    }
    content.push_str("# Harn quickstart\n");
    for line in additions {
        content.push_str(&line);
        content.push('\n');
    }
    content
}

fn push_env_if_missing(
    existing: &str,
    additions: &mut Vec<String>,
    key: &str,
    value: &str,
    commented: bool,
) {
    if env_file_has_key(existing, key) {
        return;
    }
    let assignment = format!("{key}={}", shell_quote(value));
    if commented {
        additions.push(format!("# {assignment}"));
    } else {
        additions.push(assignment);
    }
}

fn env_file_has_key(existing: &str, key: &str) -> bool {
    existing.lines().any(|line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            return false;
        }
        trimmed
            .strip_prefix("export ")
            .unwrap_or(trimmed)
            .starts_with(&format!("{key}="))
    })
}

fn providers_config_path() -> PathBuf {
    env::var_os("HARN_PROVIDERS_CONFIG")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            harn_vm::user_dirs::home_dir()
                .map(|home| home.join(".config").join("harn").join("providers.toml"))
        })
        .unwrap_or_else(|| PathBuf::from(".config/harn/providers.toml"))
}

fn print_probe_report(
    paths: &QuickstartPaths,
    choices: &[ProviderChoice],
    ollama: &OllamaProbe,
    disk: &DiskProbe,
    gpu: &GpuProbe,
) {
    println!("Harn quickstart");
    println!();
    println!("Detected:");
    println!("  providers.toml  {}", paths.providers_toml.display());
    println!("  harn.toml       {}", paths.harn_toml.display());
    println!("  .env            {}", paths.env_file.display());
    println!("  ollama          {}", ollama.message);
    println!("  disk            {}", disk.detail);
    println!("  gpu             {}", gpu.detail);
    println!();
    println!("Credentials:");
    for choice in choices.iter().filter(|choice| !choice.auth_envs.is_empty()) {
        let status = if choice.auth_available {
            "set"
        } else {
            "missing"
        };
        println!(
            "  {:<12} {:<8} {}",
            choice.name,
            status,
            choice.auth_envs.join(", ")
        );
    }
}

fn print_completion(selection: &ProviderSelection, actions: &[FileAction]) {
    println!();
    println!(
        "Configured provider={} model={}",
        selection.provider, selection.model
    );
    for action in actions {
        println!(
            "  {:<6} {:<52} {}",
            action.status,
            action.path.display(),
            action.detail
        );
    }
    println!();
    println!("Next:");
    println!("  source .env");
    println!("  harn doctor --no-network");
}

fn format_choice(choice: &ProviderChoice, ollama: &OllamaProbe) -> String {
    let auth = if choice.auth_envs.is_empty() {
        "no API key required".to_string()
    } else if choice.auth_available {
        format!("{} set", choice.auth_envs.join("/"))
    } else {
        format!("{} missing", choice.auth_envs.join("/"))
    };
    let extra = if choice.name == "ollama" {
        if ollama.reachable {
            ", daemon reachable"
        } else {
            ", daemon not reachable"
        }
    } else {
        ""
    };
    format!("{} (model {}, {auth}{extra})", choice.name, choice.model)
}

fn detect_disk_space(path: &Path) -> DiskProbe {
    let output = Command::new("df").arg("-Pk").arg(path).output();
    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(kib) = parse_df_available_kib(&text) {
                DiskProbe {
                    available_kib: Some(kib),
                    detail: format!("{:.1} GiB available at {}", kib_to_gib(kib), path.display()),
                }
            } else {
                DiskProbe {
                    available_kib: None,
                    detail: "could not parse df output".to_string(),
                }
            }
        }
        Ok(output) => DiskProbe {
            available_kib: None,
            detail: format!("df exited with {}", output.status),
        },
        Err(error) => DiskProbe {
            available_kib: None,
            detail: format!("df not available: {error}"),
        },
    }
}

fn parse_df_available_kib(output: &str) -> Option<u64> {
    output.lines().skip(1).find_map(|line| {
        let columns: Vec<&str> = line.split_whitespace().collect();
        columns.get(3).and_then(|value| value.parse::<u64>().ok())
    })
}

fn kib_to_gib(kib: u64) -> f64 {
    kib as f64 / 1024.0 / 1024.0
}

fn detect_gpu() -> GpuProbe {
    if let Ok(value) = env::var("CUDA_VISIBLE_DEVICES") {
        let trimmed = value.trim();
        if !trimmed.is_empty() && trimmed != "-1" {
            return GpuProbe {
                detected: true,
                detail: format!("CUDA_VISIBLE_DEVICES={trimmed}"),
            };
        }
    }
    if let Ok(output) = Command::new("nvidia-smi").arg("-L").output() {
        if output.status.success() {
            let detail = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("NVIDIA GPU detected")
                .to_string();
            if !detail.trim().is_empty() {
                return GpuProbe {
                    detected: true,
                    detail,
                };
            }
        }
    }
    if apple_silicon_detected() {
        return GpuProbe {
            detected: true,
            detail: "Apple Silicon GPU available".to_string(),
        };
    }
    GpuProbe {
        detected: false,
        detail: "no local GPU detected".to_string(),
    }
}

#[cfg(target_os = "macos")]
fn apple_silicon_detected() -> bool {
    Command::new("sysctl")
        .args(["-n", "hw.optional.arm64"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "1")
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn apple_silicon_detected() -> bool {
    false
}

fn toml_quote(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(provider: &str, model: &str) -> ProviderSelection {
        ProviderSelection {
            provider: provider.to_string(),
            model: model.to_string(),
            auth_env: Some("ANTHROPIC_API_KEY".to_string()),
            api_key: None,
            write_placeholder_key: true,
        }
    }

    #[test]
    fn renders_starter_provider_overlay() {
        let rendered = render_providers_toml(&selection("ollama", "devstral-small-2"));
        let parsed: toml::Value = toml::from_str(&rendered).unwrap();
        assert_eq!(parsed["default_provider"].as_str(), Some("ollama"));
        assert_eq!(
            parsed["aliases"][QUICKSTART_ALIAS]["id"].as_str(),
            Some("devstral-small-2:24b")
        );
        assert_eq!(
            parsed["aliases"][QUICKSTART_ALIAS]["tool_format"].as_str(),
            Some("text")
        );
    }

    #[test]
    fn appends_missing_env_values_without_overwriting_existing_keys() {
        let existing = "HARN_LLM_PROVIDER='openai'\n";
        let merged = merge_env_file(existing, &selection("anthropic", "claude-sonnet-4"));
        assert!(merged.contains("HARN_LLM_PROVIDER='openai'"));
        assert!(merged.contains("HARN_LLM_MODEL='claude-sonnet-4'"));
        assert!(merged.contains("# ANTHROPIC_API_KEY='replace-me'"));
        assert_eq!(merged.matches("HARN_LLM_PROVIDER=").count(), 1);
    }

    #[test]
    fn detects_env_keys_with_export_prefix() {
        assert!(env_file_has_key(
            "export OPENAI_API_KEY='sk-test'\n",
            "OPENAI_API_KEY"
        ));
        assert!(!env_file_has_key(
            "# OPENAI_API_KEY='sk-test'\n",
            "OPENAI_API_KEY"
        ));
    }

    #[test]
    fn parses_df_available_column() {
        let output = "Filesystem 1024-blocks Used Available Capacity Mounted on\n/dev/disk 100 10 90 10% /\n";
        assert_eq!(parse_df_available_kib(output), Some(90));
    }

    #[test]
    fn non_interactive_prefers_available_remote_provider() {
        let choices = vec![
            ProviderChoice {
                name: "ollama".to_string(),
                model: "llama3.2".to_string(),
                auth_envs: Vec::new(),
                auth_available: true,
            },
            ProviderChoice {
                name: "anthropic".to_string(),
                model: "claude-sonnet-4-20250514".to_string(),
                auth_envs: vec!["ANTHROPIC_API_KEY".to_string()],
                auth_available: true,
            },
        ];
        let args = QuickstartArgs {
            non_interactive: true,
            provider: None,
            model: None,
        };
        let selected = choose_non_interactive(
            &args,
            &choices,
            &OllamaProbe {
                reachable: true,
                status: "ok".to_string(),
                message: "ok".to_string(),
                available_models: vec!["llama3.2".to_string()],
            },
        )
        .unwrap();
        assert_eq!(selected.provider, "anthropic");
    }

    #[test]
    fn interactive_default_prefers_available_remote_provider() {
        let choices = vec![
            ProviderChoice {
                name: "ollama".to_string(),
                model: "llama3.2".to_string(),
                auth_envs: Vec::new(),
                auth_available: true,
            },
            ProviderChoice {
                name: "anthropic".to_string(),
                model: "claude-sonnet-4-20250514".to_string(),
                auth_envs: vec!["ANTHROPIC_API_KEY".to_string()],
                auth_available: true,
            },
        ];
        let default = default_choice_index(
            &choices,
            &OllamaProbe {
                reachable: true,
                status: "ok".to_string(),
                message: "ok".to_string(),
                available_models: vec!["llama3.2".to_string()],
            },
        );
        assert_eq!(choices[default].name, "anthropic");
    }

    #[test]
    fn non_interactive_uses_reachable_ollama_before_unprobed_keyless_providers() {
        let choices = vec![
            ProviderChoice {
                name: "ollama".to_string(),
                model: "devstral-small-2".to_string(),
                auth_envs: Vec::new(),
                auth_available: true,
            },
            ProviderChoice {
                name: "anthropic".to_string(),
                model: "claude-sonnet-4-20250514".to_string(),
                auth_envs: vec!["ANTHROPIC_API_KEY".to_string()],
                auth_available: false,
            },
            ProviderChoice {
                name: "local".to_string(),
                model: "gpt-4o".to_string(),
                auth_envs: Vec::new(),
                auth_available: true,
            },
        ];
        let args = QuickstartArgs {
            non_interactive: true,
            provider: None,
            model: None,
        };
        let selected = choose_non_interactive(
            &args,
            &choices,
            &OllamaProbe {
                reachable: true,
                status: "ok".to_string(),
                message: "ok".to_string(),
                available_models: vec!["qwen3.6:latest".to_string()],
            },
        )
        .unwrap();
        assert_eq!(selected.provider, "ollama");
    }

    #[test]
    fn ollama_selector_for_model_uses_alias_catalog() {
        assert_eq!(
            ollama_selector_for_model("devstral-small-2:24b"),
            "devstral-small-2"
        );
        assert_eq!(ollama_selector_for_model("gemma4:26b"), "ollama-gemma4");
    }
}
