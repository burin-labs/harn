use std::fs;
use std::path::{Path, PathBuf};
use std::process;

use crate::cli::{NewArgs, ProjectTemplate};
use crate::commands::run::RunSandboxOptions;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;
use crate::package::{current_harn_range_example, generate_package_docs_impl, PackageError};

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
    if let Err(error) = generate_scaffolded_docs(&dir, template) {
        eprintln!("Failed to generate {SCAFFOLD_DOCS_PATH}: {error}");
        process::exit(1);
    }
}

/// Path the package and connector manifests declare as `docs_url`.
const SCAFFOLD_DOCS_PATH: &str = "docs/api.md";

/// Write `docs/api.md` through the generator that owns it.
///
/// The scaffold used to ship this file as a literal string, which made
/// `harn package docs` and the template two owners of one artifact. They drifted
/// the moment #5936 changed how signatures render, and every freshly scaffolded
/// package failed its own `harn package verify` on a stale-docs check. The
/// OpenAPI scaffold already generates rather than embeds; this matches it.
fn generate_scaffolded_docs(dir: &Path, template: ProjectTemplate) -> Result<(), PackageError> {
    if !matches!(
        template,
        ProjectTemplate::Package | ProjectTemplate::Connector
    ) {
        return Ok(());
    }
    generate_package_docs_impl(Some(dir), None, /* check */ false).map(|_| ())
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
            .stack_size(crate::CLI_RUNTIME_STACK_SIZE)
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
        assert!(source.contains("pub fn normalize_inbound(_harness: Harness, raw: dict) -> dict"));
        let formatted = harn_fmt::format_source(&source).expect("format connector source");
        assert_eq!(source, formatted, "connector source is not canonical");
    }

    /// A scaffolded package used to fail its own `harn package verify` because
    /// the template shipped `docs/api.md` as a literal string while
    /// `harn package docs` generated it — two owners of one artifact, which
    /// drifted the moment #5936 changed signature rendering.
    ///
    /// This lives in the fast lane on purpose. The only coverage that caught the
    /// drift was `harn_cli_e2e`, which runs nightly, so the break sat on main
    /// for three nights.
    #[test]
    fn scaffolded_package_docs_are_generated_fresh() {
        for (template, kind) in [
            (ProjectTemplate::Package, "package"),
            (ProjectTemplate::Connector, "connector"),
        ] {
            let temp = tempfile::tempdir().expect("tempdir");
            let destination = temp.path().join(format!("fresh-{kind}"));
            fs::create_dir(&destination).expect("destination");
            let scaffold_target = destination.clone();
            let exit = std::thread::Builder::new()
                .name(format!("{kind}-docs-scaffold"))
                .stack_size(crate::CLI_RUNTIME_STACK_SIZE)
                .spawn(move || {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .expect("runtime");
                    runtime.block_on(async {
                        let _guard =
                            crate::tests::common::harn_state_lock::lock_harn_state_async().await;
                        let exit = dispatch_to_script(
                            Some(&format!("fresh-{kind}")),
                            &scaffold_target,
                            &format!("fresh-{kind}"),
                            template,
                        )
                        .await;
                        if exit == 0 {
                            super::generate_scaffolded_docs(&scaffold_target, template)
                                .expect("generate scaffolded docs");
                        }
                        exit
                    })
                })
                .expect("scaffold thread")
                .join()
                .expect("scaffold thread completed");
            assert_eq!(exit, 0, "{kind} scaffold failed");

            let docs = destination.join(super::SCAFFOLD_DOCS_PATH);
            assert!(docs.is_file(), "{kind} scaffold did not write {docs:?}");

            // `check: true` is the same freshness comparison `harn package
            // verify` runs, so this fails for exactly the reason the nightly did.
            super::generate_package_docs_impl(Some(&destination), None, /* check */ true)
                .unwrap_or_else(|error| {
                    panic!(
                        "{kind} scaffold left {} stale: {error}",
                        super::SCAFFOLD_DOCS_PATH
                    )
                });
        }
    }
}
