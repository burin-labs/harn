use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TriggerTables {
    #[serde(default)]
    pub(crate) triggers: Vec<TriggerManifestEntry>,
}

pub(crate) fn test_vm() -> harn_vm::Vm {
    let mut vm = harn_vm::Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    vm
}

#[derive(Debug, Clone)]
pub(crate) struct TestWorkspace {
    env: PackageWorkspace,
}

impl TestWorkspace {
    pub(crate) fn new(root: &Path) -> Self {
        Self {
            env: PackageWorkspace::for_test(root, root.join(".cache")),
        }
    }

    pub(crate) fn with_registry_source(mut self, source: impl Into<String>) -> Self {
        self.env = self.env.with_registry_source(source);
        self
    }

    pub(crate) fn env(&self) -> &PackageWorkspace {
        &self.env
    }

    pub(crate) fn cache_dir(&self) -> PathBuf {
        self.env.cache_root().expect("test cache root")
    }
}

pub(crate) fn write_trigger_project(
    root: &Path,
    manifest: &str,
    lib_source: Option<&str>,
) -> PathBuf {
    std::fs::create_dir_all(root.join(".git")).unwrap();
    fs::write(root.join(MANIFEST), manifest).unwrap();
    if let Some(source) = lib_source {
        fs::write(root.join("lib.harn"), source).unwrap();
    }
    let harn_file = root.join("main.harn");
    fs::write(&harn_file, "pipeline main() {}\n").unwrap();
    harn_file
}

pub(crate) fn run_git(repo: &Path, args: &[&str]) -> String {
    let output = test_git_command(repo).args(args).output().unwrap();
    if !output.status.success() {
        panic!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub(crate) fn test_git_command(repo: &Path) -> process::Command {
    let mut command = process::Command::new("git");
    command
        // Package tests create disposable repos. Keep ambient developer Git
        // policy from invoking hooks or signing prompts inside nextest workers.
        .args([
            "-c",
            "commit.gpgSign=false",
            "-c",
            "tag.gpgSign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_ASKPASS");
    command
}

pub(crate) fn create_git_package_repo_with(
    name: &str,
    manifest_tail: &str,
    lib_source: &str,
) -> (tempfile::TempDir, PathBuf, String) {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join(name);
    fs::create_dir_all(&repo).unwrap();
    let init = test_git_command(&repo)
        .args(["init", "-b", "main"])
        .output()
        .unwrap();
    if !init.status.success() {
        let fallback = test_git_command(&repo).arg("init").output().unwrap();
        assert!(
            fallback.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&fallback.stderr)
        );
    }
    run_git(&repo, &["config", "user.email", "tests@example.com"]);
    run_git(&repo, &["config", "user.name", "Harn Tests"]);
    run_git(&repo, &["config", "core.hooksPath", "/dev/null"]);
    fs::write(
        repo.join(MANIFEST),
        format!(
            r#"
[package]
name = "{name}"
version = "0.1.0"
"#
        ) + manifest_tail,
    )
    .unwrap();
    fs::write(repo.join("lib.harn"), lib_source).unwrap();
    run_git(&repo, &["add", "."]);
    run_git(&repo, &["commit", "-m", "initial"]);
    run_git(&repo, &["tag", "v1.0.0"]);
    let branch = run_git(&repo, &["branch", "--show-current"]);
    (tmp, repo, branch)
}

pub(crate) fn create_git_package_repo() -> (tempfile::TempDir, PathBuf, String) {
    create_git_package_repo_with(
        "acme-lib",
        "",
        "pub fn value() -> string { return \"v1\" }\n",
    )
}

pub(crate) fn write_package_registry_index(
    path: &Path,
    registry_name: &str,
    git: &str,
    package_name: &str,
) {
    let harn_range = crate::package::current_harn_range_example();
    fs::write(
        path,
        format!(
            r#"
version = 1

[[package]]
name = "{registry_name}"
description = "Acme package for registry tests"
repository = "{git}"
license = "MIT OR Apache-2.0"
harn = "{harn_range}"
exports = ["lib"]
connector_contract = "v1"
docs_url = "https://docs.example.test/acme"
checksum = "sha256:index"
provenance = "https://provenance.example.test/acme"

[[package.version]]
version = "1.0.0"
git = "{git}"
tag = "v1.0.0"
package = "{package_name}"
checksum = "sha256:package"
provenance = "https://provenance.example.test/acme/1.0.0"
"#
        ),
    )
    .unwrap();
}

pub(crate) fn test_harn_connector_source(provider_id: &str) -> String {
    format!(
        r#"
pub fn provider_id() {{
  return "{provider_id}"
}}

pub fn kinds() {{
  return ["webhook"]
}}

pub fn payload_schema() {{
  return {{
harn_schema_name: "EchoEventPayload",
json_schema: {{
  type: "object",
  additionalProperties: true,
}},
  }}
}}
"#
    )
}

pub(crate) fn write_publishable_package(root: &Path) {
    fs::create_dir_all(root.join("lib")).unwrap();
    fs::create_dir_all(root.join("docs")).unwrap();
    let harn_range = crate::package::current_harn_range_example();
    fs::write(
        root.join(MANIFEST),
        format!(
            r#"[package]
name = "acme-lib"
version = "0.1.0"
description = "Acme helpers"
license = "MIT"
repository = "https://github.com/acme/acme-lib"
harn = "{harn_range}"
docs_url = "docs/api.md"

[exports]
lib = "lib/main.harn"

[dependencies]
"#
        ),
    )
    .unwrap();
    fs::write(
        root.join("lib/main.harn"),
        r#"/// Return a greeting.
pub fn greet(name: string) -> string {
  return "hi " + name
}
"#,
    )
    .unwrap();
    fs::write(root.join("README.md"), "# acme-lib\n").unwrap();
    fs::write(root.join("LICENSE"), "MIT\n").unwrap();
    fs::write(root.join("docs/api.md"), "").unwrap();
}
