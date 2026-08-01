use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use crate::cli::{NewArgs, ProjectTemplate};
use crate::commands::run::RunSandboxOptions;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;
use crate::package::current_harn_range_example;

pub(crate) fn resolve_new_args(
    args: &NewArgs,
) -> Result<(Option<String>, ProjectTemplate), String> {
    let template = args.template.unwrap_or(ProjectTemplate::Basic);
    match (args.first.as_deref(), args.second.as_deref()) {
        (Some("package"), Some(name)) => Ok((Some(name.to_string()), ProjectTemplate::Package)),
        (Some("connector"), Some(name)) => Ok((Some(name.to_string()), ProjectTemplate::Connector)),
        (Some(kind @ ("package" | "connector")), None) => Err(format!(
            "`harn new {kind}` requires a package name, for example `harn new {kind} my-{kind}`"
        )),
        (Some(name), None) => Ok((Some(name.to_string()), template)),
        (None, None) => Ok((None, template)),
        (Some(_), Some(_)) => Err(
            "unexpected second positional argument; use `harn new package NAME` or `harn new NAME --template package`"
                .to_string(),
        ),
        (None, Some(_)) => unreachable!("clap cannot fill second positional without first"),
    }
}

/// `harn init` and `harn new` dispatch shim. Resolves the destination
/// directory in Rust, then delegates the template render + file-write loop
/// to `cli/scaffold/init.harn`.
pub(crate) async fn init_project(name: Option<&str>, template: ProjectTemplate) {
    let dir = match name {
        Some(n) => {
            let dir = PathBuf::from(n);
            if dir.exists() {
                eprintln!("Directory '{n}' already exists");
                process::exit(1);
            }
            fs::create_dir_all(&dir).unwrap_or_else(|e| {
                eprintln!("Failed to create directory: {e}");
                process::exit(1);
            });
            dir
        }
        None => PathBuf::from("."),
    };

    let project_name = name
        .and_then(|value| Path::new(value).file_name().and_then(|name| name.to_str()))
        .unwrap_or("my-project")
        .to_string();

    let exit = dispatch_to_script(name, &dir, &project_name, template).await;
    if exit != 0 {
        process::exit(exit);
    }
}

async fn dispatch_to_script(
    name: Option<&str>,
    dir: &Path,
    project_name: &str,
    template: ProjectTemplate,
) -> i32 {
    let dir_str = dir.display().to_string();
    let template_id = template_id(template);
    let harn_range = current_harn_range_example();
    let harn_version = env!("CARGO_PKG_VERSION");
    let name_str = name.unwrap_or("");
    let _name_env = ScopedEnvVar::set("HARN_INIT_NAME", name_str);
    let _project_env = ScopedEnvVar::set("HARN_INIT_PROJECT_NAME", project_name);
    let _dir_env = ScopedEnvVar::set("HARN_INIT_DIR", &dir_str);
    let _template_env = ScopedEnvVar::set("HARN_INIT_TEMPLATE", template_id);
    let _range_env = ScopedEnvVar::set("HARN_INIT_HARN_RANGE", &harn_range);
    let _version_env = ScopedEnvVar::set("HARN_INIT_HARN_VERSION", harn_version);
    let _mode_env = ScopedEnvVar::set(
        "HARN_INIT_MODE",
        if name.is_some() { "new" } else { "init" },
    );
    dispatch::dispatch_to_embedded_script_with_sandbox(
        "scaffold/init",
        Vec::new(),
        /* json_mode */ false,
        RunSandboxOptions::default().with_workspace_root(dir),
    )
    .await
}

fn template_id(template: ProjectTemplate) -> &'static str {
    match template {
        ProjectTemplate::Basic => "basic",
        ProjectTemplate::Agent => "agent",
        ProjectTemplate::Chat => "chat",
        ProjectTemplate::McpServer => "mcp-server",
        ProjectTemplate::Eval => "eval",
        ProjectTemplate::PipelineLab => "pipeline-lab",
        ProjectTemplate::Package => "package",
        ProjectTemplate::Connector => "connector",
    }
}

#[cfg(test)]
mod tests {
    use super::{dispatch_to_script, resolve_new_args, template_id};
    use crate::cli::{NewArgs, ProjectTemplate};
    use std::fs;

    #[test]
    fn new_package_kind_resolves_to_package_template() {
        let args = NewArgs {
            first: Some("package".to_string()),
            second: Some("sample".to_string()),
            template: None,
        };
        let (name, template) = resolve_new_args(&args).unwrap();
        assert_eq!(name.as_deref(), Some("sample"));
        assert_eq!(template, ProjectTemplate::Package);
    }

    #[test]
    fn template_ids_match_scaffold_script_contract() {
        assert_eq!(template_id(ProjectTemplate::Basic), "basic");
        assert_eq!(template_id(ProjectTemplate::McpServer), "mcp-server");
        assert_eq!(template_id(ProjectTemplate::PipelineLab), "pipeline-lab");
        assert_eq!(template_id(ProjectTemplate::Connector), "connector");
    }

    #[test]
    fn generated_connector_projects_typed_harness() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("typed-connector");
        fs::create_dir(&destination).expect("connector destination");
        let moved_destination = destination.clone();
        let exit = std::thread::Builder::new()
            .name("typed-connector-scaffold".to_string())
            .stack_size(16 * 1024 * 1024)
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("runtime");
                runtime.block_on(async {
                    let _guard =
                        crate::tests::common::harn_state_lock::lock_harn_state_async().await;
                    dispatch_to_script(
                        Some("typed-connector"),
                        &moved_destination,
                        "typed-connector",
                        ProjectTemplate::Connector,
                    )
                    .await
                })
            })
            .expect("scaffold thread")
            .join()
            .expect("scaffold thread completed");
        assert_eq!(exit, 0);

        let source =
            fs::read_to_string(destination.join("connectors/echo.harn")).expect("connector source");
        assert!(source.contains("pub fn normalize_inbound(_harness: Harness, raw)"));
        let formatted = harn_fmt::format_source(&source).expect("format connector source");
        assert_eq!(source, formatted, "connector source is not canonical");
    }
}
