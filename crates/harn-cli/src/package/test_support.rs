use super::*;
use serde::{Deserialize, Serialize};

pub(crate) fn create_test_package_generation(root: &Path) -> PathBuf {
    use harn_modules::package_snapshot::{
        generation_root, package_current_path, package_publication_lock_path,
        PackageGenerationManifest, PackageGenerationPointer, GENERATION_LEASE_FILE,
        GENERATION_LOCK_FILE, GENERATION_MANIFEST_FILE, GENERATION_PACKAGES_DIR,
    };

    let generation = "generation-test";
    let generation_root = generation_root(root, generation);
    let packages_root = generation_root.join(GENERATION_PACKAGES_DIR);
    fs::create_dir_all(&packages_root).unwrap();
    let lock_body =
        b"version = 4\ngenerator_version = \"test\"\nprotocol_artifact_version = \"test\"\n";
    fs::write(generation_root.join(GENERATION_LOCK_FILE), lock_body).unwrap();
    fs::write(generation_root.join(GENERATION_LEASE_FILE), []).unwrap();
    let digest = harn_modules::package_snapshot::package_lock_digest(lock_body);
    let manifest = PackageGenerationManifest::new(generation, digest).unwrap();
    fs::write(
        generation_root.join(GENERATION_MANIFEST_FILE),
        toml::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
    let pointer = PackageGenerationPointer::new(generation).unwrap();
    fs::write(
        package_current_path(root),
        toml::to_string_pretty(&pointer).unwrap(),
    )
    .unwrap();
    File::create(package_publication_lock_path(root)).unwrap();
    packages_root
}

pub(crate) fn current_packages_dir(root: &Path) -> PathBuf {
    harn_modules::package_snapshot::PackageSnapshot::acquire(root)
        .unwrap()
        .expect("published package generation")
        .packages_root()
        .to_path_buf()
}

pub(crate) fn write_test_generation_lock(root: &Path, body: &str) {
    use harn_modules::package_snapshot::{
        generation_root, package_lock_digest, PackageGenerationManifest, GENERATION_LOCK_FILE,
        GENERATION_MANIFEST_FILE,
    };

    let generation = "generation-test";
    let generation_root = generation_root(root, generation);
    fs::write(generation_root.join(GENERATION_LOCK_FILE), body).unwrap();
    let manifest =
        PackageGenerationManifest::new(generation, package_lock_digest(body.as_bytes())).unwrap();
    fs::write(
        generation_root.join(GENERATION_MANIFEST_FILE),
        toml::to_string_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

pub(crate) fn persona_manifest(name: &str, entry_workflow: &str) -> String {
    format!(
        r#"[[personas]]
name = "{name}"
version = "1.2.3"
description = "A package persona."
entry_workflow = "{entry_workflow}"
tools = ["filesystem", "shell"]
capabilities = ["workspace.read_text"]
autonomy_tier = "act_with_approval"
receipt_policy = "required"
model_policy = {{ default_model = "cheap", escalation_model = "frontier", fallback_models = ["backup"] }}
budget = {{ daily_usd = 10.0, hourly_usd = 4.0, run_usd = 2.0, frontier_escalations = 3, max_tokens = 4096, max_runtime_seconds = 600 }}
"#
    )
}

pub(crate) fn install_test_persona_package(
    root: &Path,
    alias: &str,
    locked_names: Vec<String>,
    manifest_names: &[&str],
) -> LockFile {
    create_test_package_generation(root);
    let mut lock = LockFile::default();
    add_test_persona_package(root, &mut lock, alias, locked_names, manifest_names);
    lock
}

pub(crate) fn add_test_persona_package(
    root: &Path,
    lock: &mut LockFile,
    alias: &str,
    locked_names: Vec<String>,
    manifest_names: &[&str],
) {
    let packages = current_packages_dir(root);
    let package_dir = packages.join(alias);
    fs::create_dir_all(&package_dir).unwrap();
    let mut manifest = format!("[package]\nname = \"{alias}\"\nversion = \"1.2.3\"\n\n");
    for name in manifest_names {
        manifest.push_str(&persona_manifest(name, "workflow.harn#run"));
        manifest.push('\n');
    }
    fs::write(package_dir.join(MANIFEST), manifest).unwrap();
    fs::write(
        package_dir.join("workflow.harn"),
        "pub pipeline run(task) -> dict { return {ok: true} }\n",
    )
    .unwrap();
    let content_hash = compute_content_hash(&package_dir).unwrap();
    lock.packages.push(LockEntry {
        name: alias.to_string(),
        source: format!("path+../{alias}"),
        content_hash: Some(content_hash),
        package_version: Some("1.2.3".to_string()),
        exports: PackageLockExports {
            personas: locked_names,
            ..PackageLockExports::default()
        },
        permissions: vec![
            "process:exec".to_string(),
            "workspace:read_text".to_string(),
        ],
        host_requirements: vec!["workspace.read_text".to_string()],
        ..LockEntry::default()
    });
    let body = toml::to_string_pretty(&lock).unwrap();
    fs::write(root.join(LOCK_FILE), &body).unwrap();
    write_test_generation_lock(root, &body);
}

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
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

pub(crate) fn test_git_command(repo: &Path) -> process::Command {
    let mut command = process::Command::new("git");
    // Keep disposable package repos independent of developer Git policy that can
    // make simple test commands invoke hooks, signing prompts, or editors.
    command
        .args([
            // Pin identity and default branch so the tests do not depend on the
            // host's user.name/user.email or init.defaultBranch.
            "-c",
            "user.email=harn-test@example.com",
            "-c",
            "user.name=harn-test",
            "-c",
            "init.defaultBranch=main",
            // Disable every flavour of host commit/tag signing policy.
            "-c",
            "commit.gpgSign=false",
            "-c",
            "tag.gpgSign=false",
            "-c",
            "tag.forceSignAnnotated=false",
            "-c",
            "gpg.format=openpgp",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.editor=true",
        ])
        .current_dir(repo)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_GLOBAL", repo.join(".gitconfig-test"))
        .env("GIT_CONFIG_SYSTEM", repo.join(".gitconfig-system-test"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_MERGE_AUTOEDIT", "no")
        .env("GIT_PAGER", "cat")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_ASKPASS")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true");
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
