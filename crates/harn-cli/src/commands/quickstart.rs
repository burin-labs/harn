use std::env;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::time::Duration;

use harn_vm::llm::ProviderCredentialStatus;

use crate::cli::QuickstartArgs;
use crate::commands::hardware::{self, GpuKind};
use crate::commands::local_readiness;

mod env_file;

const QUICKSTART_ALIAS: &str = "quickstart";
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderChoice {
    name: String,
    model: String,
    auth_envs: Vec<String>,
    auth_available: bool,
    credential: ProviderCredentialStatus,
}

impl ProviderChoice {
    fn needs_key(&self) -> bool {
        !self.auth_envs.is_empty() && !self.auth_available
    }

    fn primary_auth_env(&self) -> Option<&str> {
        self.auth_envs.first().map(String::as_str)
    }

    /// Plain words for the typed credential status. Deferred platform
    /// discovery is usable but has not found a credential, so it is never "set".
    fn credential_label(&self) -> String {
        crate::localization::translate(match self.credential {
            ProviderCredentialStatus::Ok => "set",
            ProviderCredentialStatus::Missing => "missing",
            ProviderCredentialStatus::NotRequired => "not required",
            ProviderCredentialStatus::Deferred => "deferred",
            ProviderCredentialStatus::NeedsUserApproval => "needs approval",
            ProviderCredentialStatus::RegionUnconfigured
            | ProviderCredentialStatus::CredentialsUnconfigured => "incomplete",
        })
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
    free_bytes: Option<u64>,
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
    if let Some(provider) = args.provider.as_deref() {
        check_route(provider, args.model.as_deref())?;
    }

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
    check_route(&selection.provider, Some(&selection.model))?;

    let (actions, saved) = write_quickstart_files(&paths, &selection)?;
    print_completion(&selection, &actions, saved);
    Ok(())
}

/// Refuse a provider the catalog does not know, or a model the catalog
/// assigns to a different provider, through the resolver dispatch uses.
/// Without a model only the provider name is checked: a missing default is
/// answered later by an explicit model or a prompt.
fn check_route(provider: &str, model: Option<&str>) -> Result<(), String> {
    let provider = provider.trim();
    let result = match model.map(str::trim).filter(|model| !model.is_empty()) {
        Some(model) => harn_vm::llm_config::resolve_model_request(model, Some(provider)).map(drop),
        None => match harn_vm::llm_config::default_model_for_provider(provider) {
            Err(error @ harn_vm::llm_config::ModelResolutionError::UnknownProvider { .. }) => {
                Err(error)
            }
            _ => Ok(()),
        },
    };
    result.map_err(|error| error.to_string())
}

async fn probe_ollama(model: Option<&str>) -> OllamaProbe {
    let selected_model = model
        .filter(|value| !value.trim().is_empty())
        .map(|value| value.trim().to_string())
        .or_else(|| harn_vm::llm_config::default_model_for_provider("ollama").ok())
        .unwrap_or_default();
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
    let automatic = harn_vm::llm_config::automatic_default_candidates();
    let featured = harn_vm::llm_config::featured_provider_names();
    let mut choices: Vec<_> = harn_vm::llm_config::provider_names()
        .into_iter()
        .filter_map(|name| provider_choice(&name, model, ollama))
        .collect();
    choices.sort_by_key(|choice| {
        let automatic_rank = automatic
            .iter()
            .position(|route| route.provider == choice.name)
            .unwrap_or(automatic.len());
        let featured_rank = featured
            .iter()
            .position(|name| *name == choice.name)
            .unwrap_or(featured.len());
        (automatic_rank, featured_rank)
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
    // An explicit model is offered only by the providers the catalog lets serve it.
    if let Some(model) = model {
        check_route(name, Some(model)).ok()?;
    }
    let auth = harn_vm::llm::provider_auth_status(name);
    let model = default_model_for_choice(name, model, ollama)?;
    Some(ProviderChoice {
        name: name.to_string(),
        model,
        auth_envs,
        auth_available: auth.available,
        credential: auth.credential_status,
    })
}

fn default_model_for_choice(
    provider: &str,
    model: Option<&str>,
    ollama: &OllamaProbe,
) -> Option<String> {
    if let Some(model) = model.filter(|value| !value.trim().is_empty()) {
        return Some(model.trim().to_string());
    }
    if let Some(route) = harn_vm::llm_config::automatic_default_candidates()
        .into_iter()
        .find(|route| route.provider == provider)
    {
        return Some(route.model_id);
    }
    if provider == "ollama" {
        if let Some(model) = recommended_ollama_selector(&ollama.available_models) {
            return Some(model);
        }
        if let Some(model) = ollama.available_models.first() {
            return Some(ollama_selector_for_model(model));
        }
    }
    harn_vm::llm_config::default_model_for_provider(provider).ok()
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

fn requested_choice<'a>(
    choices: &'a [ProviderChoice],
    provider: &str,
) -> Result<&'a ProviderChoice, String> {
    choices
        .iter()
        .find(|choice| choice.name == provider)
        .ok_or_else(
            || match harn_vm::llm_config::default_model_for_provider(provider) {
                Err(error) => error.to_string(),
                Ok(_) => format!("provider '{provider}' is unavailable"),
            },
        )
}

fn choose_non_interactive(
    args: &QuickstartArgs,
    choices: &[ProviderChoice],
    ollama: &OllamaProbe,
) -> Result<ProviderSelection, String> {
    let choice = if let Some(provider) = args.provider.as_deref() {
        let provider = provider.trim();
        requested_choice(choices, provider)?
    } else {
        preferred_choice_index(choices, ollama)
            .map(|idx| &choices[idx])
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
        requested_choice(choices, provider)?
    } else {
        println!();
        println!("{}", crate::localization::formatted!("Choose a provider:"));
        for (idx, choice) in choices.iter().enumerate() {
            println!("  {}) {}", idx + 1, format_choice(choice, ollama));
        }
        let default = preferred_choice_index(choices, ollama).unwrap_or(0);
        let answer = read_line(&crate::localization::formatted!(
            "provider [{}]",
            default + 1
        ))?;
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
        println!(
            "{}",
            crate::localization::formatted!("Choose an Ollama model:")
        );
        for (idx, model) in ollama_models.iter().enumerate() {
            let selector = ollama_selector_for_model(model);
            if selector == *model {
                println!("  {}) {model}", idx + 1);
            } else {
                println!("  {}) {model} ({selector})", idx + 1);
            }
        }
        let answer = read_line(&crate::localization::formatted!("model [1]"))?;
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
        let answer = read_line(&crate::localization::formatted!("model [{}]", choice.model))?;
        if !answer.trim().is_empty() {
            model = answer.trim().to_string();
        }
    }

    let mut api_key = None;
    if choice.needs_key() {
        if let Some(env_name) = choice.primary_auth_env() {
            println!();
            println!(
                "{}",
                crate::localization::formatted!("{env_name} is not set.", env_name = env_name)
            );
            let prompt = crate::localization::formatted!(
                "Enter {env_name} for .env, or leave blank for a placeholder: ",
                env_name = env_name
            );
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

/// Provider quickstart picks when the user names none: a keyed provider whose
/// credential is present, then reachable Ollama, then a configured local
/// endpoint, then the first catalog automatic route.
fn preferred_choice_index(choices: &[ProviderChoice], ollama: &OllamaProbe) -> Option<usize> {
    choices
        .iter()
        .position(|choice| {
            !choice.auth_envs.is_empty() && choice.credential == ProviderCredentialStatus::Ok
        })
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
        .or_else(|| {
            harn_vm::llm_config::automatic_default_candidates()
                .iter()
                .find_map(|route| {
                    choices
                        .iter()
                        .position(|choice| choice.name == route.provider)
                })
        })
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

/// Write every destination, then report whether all of them now record the
/// selection. A destination that keeps an existing different route makes the
/// selection unsaved.
fn write_quickstart_files(
    paths: &QuickstartPaths,
    selection: &ProviderSelection,
) -> Result<(Vec<FileAction>, bool), String> {
    let manifest = read_existing_harn_toml(&paths.harn_toml)?;
    validate_existing_providers_toml(&paths.providers_toml)?;
    let environment = env_file::prepare(&paths.cwd, &paths.env_file, selection)?;
    let env_saved = !environment.keeps_other_route;
    let mut actions = environment.write()?;
    actions.push(write_providers_toml(&paths.providers_toml, selection)?);
    let (manifest_action, manifest_saved) = write_or_update_harn_toml(paths, manifest, selection)?;
    actions.push(manifest_action);
    Ok((actions, env_saved && manifest_saved))
}

/// An existing project manifest is read as text plus whether it has `[llm]`.
/// That section is decoded through the package manifest's own provider schema
/// so quickstart refuses, before any write, a manifest the loader would reject.
struct ExistingManifest {
    content: String,
    has_llm: bool,
}

fn read_existing_harn_toml(path: &Path) -> Result<Option<ExistingManifest>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    let parsed = toml::from_str::<toml::Value>(&content)
        .map_err(|error| format!("{} is not valid TOML: {error}", path.display()))?;
    let has_llm = match parsed.get("llm") {
        None => false,
        Some(llm) => {
            llm.clone()
                .try_into::<harn_vm::llm_config::ProvidersConfig>()
                .map_err(|error| {
                    format!(
                        "{} [llm] is not valid provider configuration: {error}",
                        path.display()
                    )
                })?;
            true
        }
    };
    Ok(Some(ExistingManifest { content, has_llm }))
}

fn validate_existing_providers_toml(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let existing = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    harn_vm::llm_config::parse_config_toml(&existing)
        .map(|_| ())
        .map_err(|error| {
            format!(
                "{} is not valid provider configuration: {error}",
                path.display()
            )
        })
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
    existing: Option<ExistingManifest>,
    selection: &ProviderSelection,
) -> Result<(FileAction, bool), String> {
    let path = &paths.harn_toml;
    let llm_section = render_harn_llm_section(selection);
    let (content, status, detail) = match existing {
        None => {
            let project_name = paths
                .cwd
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("harn-project");
            (
                render_new_harn_toml(project_name, &llm_section),
                "create",
                "starter project manifest",
            )
        }
        Some(existing) if existing.has_llm => {
            // A rerun with the same selection finds its own section verbatim.
            let saved = existing.content.contains(&llm_section);
            let action = FileAction {
                path: path.clone(),
                status: "skip",
                detail: "already has [llm] config".to_string(),
            };
            return Ok((action, saved));
        }
        Some(existing) => {
            let mut content = existing.content;
            if !content.ends_with('\n') {
                content.push('\n');
            }
            content.push('\n');
            content.push_str(&llm_section);
            (content, "update", "added [llm] defaults")
        }
    };
    harn_vm::atomic_io::atomic_write(path, content.as_bytes())
        .map_err(|error| format!("failed to write {}: {error}", path.display()))?;
    let action = FileAction {
        path: path.clone(),
        status,
        detail: detail.to_string(),
    };
    Ok((action, true))
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

fn render_new_harn_toml(project_name: &str, llm_section: &str) -> String {
    format!(
        "[package]\n\
         name = {}\n\
         version = \"0.1.0\"\n\n\
         [dependencies]\n\n\
         {llm_section}",
        toml_quote(project_name),
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
    let placeholder_exists = commented
        && existing
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix('#'))
            .any(|line| env_assignment_key(line) == Some(key));
    if env_file_has_key(existing, key) || placeholder_exists {
        return;
    }
    let assignment = format!("{key}={}", shell_words::quote(value));
    if commented {
        additions.push(format!("# {assignment}"));
    } else {
        additions.push(assignment);
    }
}

fn env_file_has_key(existing: &str, key: &str) -> bool {
    existing
        .lines()
        .any(|line| env_assignment_key(line) == Some(key))
}

/// Whether `existing` assigns `key` a value other than `value`.
fn env_assigns_other_value(existing: &str, key: &str, value: &str) -> bool {
    existing.lines().any(|line| {
        env_assignment_key(line) == Some(key)
            && line
                .split_once('=')
                .and_then(|(_, raw)| shell_words::split(raw).ok())
                != Some(vec![value.to_string()])
    })
}

fn env_assignment_key(line: &str) -> Option<&str> {
    let line = line.trim_start();
    if line.starts_with('#') {
        return None;
    }
    line.strip_prefix("export ")
        .unwrap_or(line)
        .split_once('=')
        .map(|(key, _)| key)
}

fn providers_config_path() -> PathBuf {
    env::var_os("HARN_PROVIDERS_CONFIG")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(harn_vm::llm_config::user_providers_config_path)
        .unwrap_or_else(|| PathBuf::from(".config/harn/providers.toml"))
}

fn print_probe_report(
    paths: &QuickstartPaths,
    choices: &[ProviderChoice],
    ollama: &OllamaProbe,
    disk: &DiskProbe,
    gpu: &GpuProbe,
) {
    println!("{}", crate::localization::formatted!("Harn quickstart"));
    println!();
    println!("{}", crate::localization::formatted!("Detected:"));
    println!("  providers.toml  {}", paths.providers_toml.display());
    println!("  harn.toml       {}", paths.harn_toml.display());
    println!("  .env            {}", paths.env_file.display());
    println!("  ollama          {}", ollama.message);
    println!(
        "{}",
        crate::localization::formatted!("  disk            {}", disk.detail)
    );
    println!(
        "{}",
        crate::localization::formatted!("  gpu             {}", gpu.detail)
    );
    println!();
    println!("{}", crate::localization::formatted!("Credentials:"));
    for choice in choices.iter().filter(|choice| !choice.auth_envs.is_empty()) {
        println!(
            "  {:<12} {:<8} {}",
            choice.name,
            choice.credential_label(),
            choice.auth_envs.join(", ")
        );
    }
}

fn print_completion(selection: &ProviderSelection, actions: &[FileAction], saved: bool) {
    println!();
    let headline = if saved {
        crate::localization::formatted!(
            "Configured provider={} model={}",
            selection.provider,
            selection.model
        )
    } else {
        crate::localization::formatted!(
            "Kept existing configuration; requested provider={} model={} was not saved",
            selection.provider,
            selection.model
        )
    };
    println!("{headline}");
    for action in actions {
        println!(
            "  {:<6} {:<52} {}",
            crate::localization::translate(action.status),
            action.path.display(),
            crate::localization::translate(&action.detail)
        );
    }
    println!();
    println!("{}", crate::localization::formatted!("Next:"));
    println!("  set -a; . ./.env; set +a");
    println!("  harn doctor --no-network");
}

fn format_choice(choice: &ProviderChoice, ollama: &OllamaProbe) -> String {
    let auth = if choice.auth_envs.is_empty() {
        crate::localization::formatted!("no API key required")
    } else {
        format!(
            "{} {}",
            choice.auth_envs.join("/"),
            choice.credential_label()
        )
    };
    let extra = if choice.name == "ollama" {
        if ollama.reachable {
            crate::localization::translate(", daemon reachable")
        } else {
            crate::localization::translate(", daemon not reachable")
        }
    } else {
        String::new()
    };
    crate::localization::formatted!(
        "{} (model {}, {auth}{extra})",
        choice.name,
        choice.model,
        auth = auth,
        extra = extra
    )
}

fn detect_disk_space(path: &Path) -> DiskProbe {
    match hardware::detect_disk(path).free_bytes {
        Some(free_bytes) => DiskProbe {
            free_bytes: Some(free_bytes),
            detail: crate::localization::formatted!(
                "{:.1} GiB available at {}",
                hardware::bytes_to_gib_f64(free_bytes),
                path.display()
            ),
        },
        None => DiskProbe {
            free_bytes: None,
            detail: crate::localization::formatted!(
                "could not determine free space at {}",
                path.display()
            ),
        },
    }
}

fn detect_gpu() -> GpuProbe {
    // An explicit CUDA device selection is honored even when `nvidia-smi` is
    // absent — a quickstart-specific courtesy the shared detector does not
    // model (it reports memory-capacity facts, not env-var intent).
    if let Ok(value) = env::var("CUDA_VISIBLE_DEVICES") {
        let trimmed = value.trim();
        if !trimmed.is_empty() && trimmed != "-1" {
            return GpuProbe {
                detected: true,
                detail: format!("CUDA_VISIBLE_DEVICES={trimmed}"),
            };
        }
    }
    // Otherwise defer to the single owner so quickstart, doctor, and recommend
    // agree on what counts as a GPU.
    match hardware::detect_gpu().kind {
        GpuKind::None => GpuProbe {
            detected: false,
            detail: crate::localization::formatted!("no local GPU detected"),
        },
        kind => GpuProbe {
            detected: true,
            detail: kind.label().to_string(),
        },
    }
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
        // devstral dropped its stale heredoc `text` pin in the tool-format
        // harmonize; the quickstart overlay now inherits the global `json`
        // (fenced-JSON) text-channel default. Heredoc stays reachable via an
        // explicit `tool_format = "text"` pin.
        assert_eq!(
            parsed["aliases"][QUICKSTART_ALIAS]["tool_format"].as_str(),
            Some("json")
        );
    }

    #[test]
    fn appends_missing_env_values_without_overwriting_existing_keys() {
        let existing = "HARN_LLM_PROVIDER='openai'\n";
        let merged = merge_env_file(existing, &selection("anthropic", "claude-sonnet-4"));
        // Pre-existing lines are copied through verbatim, quotes and all;
        // newly appended ones are quoted only where `source .env` needs it.
        assert!(merged.contains("HARN_LLM_PROVIDER='openai'"));
        assert!(merged.contains("HARN_LLM_MODEL=claude-sonnet-4"));
        assert!(merged.contains("# ANTHROPIC_API_KEY=replace-me"));
        assert_eq!(merged.matches("HARN_LLM_PROVIDER=").count(), 1);
    }

    #[test]
    fn appended_env_values_are_quoted_when_the_shell_needs_it() {
        let merged = merge_env_file("", &selection("anthropic", "claude sonnet '4'"));
        assert!(merged.contains(r"HARN_LLM_MODEL='claude sonnet '\''4'\'''"));
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
    fn non_interactive_prefers_available_remote_provider() {
        let choices = vec![
            ProviderChoice {
                name: "ollama".to_string(),
                model: "llama3.2".to_string(),
                auth_envs: Vec::new(),
                auth_available: true,
                credential: ProviderCredentialStatus::NotRequired,
            },
            ProviderChoice {
                name: "anthropic".to_string(),
                model: "claude-sonnet-4-20250514".to_string(),
                auth_envs: vec!["ANTHROPIC_API_KEY".to_string()],
                auth_available: true,
                credential: ProviderCredentialStatus::Ok,
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
    fn non_interactive_uses_reachable_ollama_before_unprobed_keyless_providers() {
        let choices = vec![
            ProviderChoice {
                name: "ollama".to_string(),
                model: "devstral-small-2".to_string(),
                auth_envs: Vec::new(),
                auth_available: true,
                credential: ProviderCredentialStatus::NotRequired,
            },
            ProviderChoice {
                name: "anthropic".to_string(),
                model: "claude-sonnet-4-20250514".to_string(),
                auth_envs: vec!["ANTHROPIC_API_KEY".to_string()],
                auth_available: false,
                credential: ProviderCredentialStatus::Missing,
            },
            ProviderChoice {
                name: "local".to_string(),
                model: "gpt-4o".to_string(),
                auth_envs: Vec::new(),
                auth_available: true,
                credential: ProviderCredentialStatus::NotRequired,
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
