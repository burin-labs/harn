#![cfg(unix)]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use harn_hostlib::sandbox::{
    ExecRequest, FilesystemAccess, FilesystemMount, LocalSandbox, SandboxBackend, SandboxSpec,
};

const PACKAGE_NAME: &str = "harn-offline-dep";
const PACKAGE_VERSION: &str = "1.0.0";
const TARBALL_NAME: &str = "harn-offline-dep-1.0.0.tgz";

#[tokio::test]
async fn sandboxed_npm_install_resolves_file_tarball_dependency_offline() {
    let Some(npm) = find_on_path("npm") else {
        eprintln!("skipping: npm was not found on PATH");
        return;
    };
    #[cfg(target_os = "macos")]
    if !Path::new("/usr/bin/sandbox-exec").exists() {
        eprintln!("skipping: /usr/bin/sandbox-exec is not available");
        return;
    }

    let temp = tempfile::tempdir().expect("create temp npm fixture");
    let workspace = temp.path().join("workspace");
    let vendor = workspace.join("vendor");
    fs::create_dir_all(&vendor).expect("create vendored tarball dir");

    let package = temp.path().join("dep-src");
    write_dependency_package(&package);
    pack_dependency(&npm, &package, &vendor);
    let tarball = vendor.join(TARBALL_NAME);
    assert!(tarball.is_file(), "npm pack did not create {tarball:?}");

    write_consumer_project(&workspace);

    let backend = LocalSandbox::default();
    let session = backend
        .provision(SandboxSpec {
            mounts: vec![FilesystemMount {
                source: workspace.clone(),
                target: "/workspace".to_string(),
                access: FilesystemAccess::ReadWrite,
            }],
            ..Default::default()
        })
        .await
        .expect("provision local sandbox");

    let result = backend
        .exec(
            &session.id,
            ExecRequest {
                command: npm.display().to_string(),
                args: vec![
                    "install".to_string(),
                    "--offline".to_string(),
                    "--ignore-scripts".to_string(),
                    "--no-audit".to_string(),
                    "--no-fund".to_string(),
                ],
                cwd: Some("/workspace".to_string()),
                env: BTreeMap::from([
                    ("NO_UPDATE_NOTIFIER".to_string(), "1".to_string()),
                    ("NPM_CONFIG_PROGRESS".to_string(), "false".to_string()),
                ]),
                timeout: Some(Duration::from_secs(30)),
                ..Default::default()
            },
        )
        .await
        .expect("run sandboxed npm install");
    let _ = backend.terminate(&session.id).await;

    assert!(
        result.success(),
        "npm install --offline failed in sandbox\nstdout:\n{}\nstderr:\n{}",
        result.stdout,
        result.stderr
    );

    let installed = workspace
        .join("node_modules")
        .join(PACKAGE_NAME)
        .join("package.json");
    assert!(
        installed.is_file(),
        "expected sandboxed npm install to create {installed:?}"
    );
    let installed_manifest = fs::read_to_string(&installed).expect("read installed manifest");
    assert!(
        installed_manifest.contains(&format!("\"name\":\"{PACKAGE_NAME}\""))
            || installed_manifest.contains(&format!("\"name\": \"{PACKAGE_NAME}\"")),
        "installed manifest did not belong to {PACKAGE_NAME}: {installed_manifest}"
    );
}

fn write_dependency_package(package: &Path) {
    fs::create_dir_all(package).expect("create dependency package");
    fs::write(
        package.join("package.json"),
        format!(
            r#"{{
  "name": "{PACKAGE_NAME}",
  "version": "{PACKAGE_VERSION}",
  "main": "index.js",
  "files": ["index.js"]
}}
"#
        ),
    )
    .expect("write dependency package.json");
    fs::write(package.join("index.js"), "module.exports = \"offline\";\n")
        .expect("write dependency entrypoint");
}

fn pack_dependency(npm: &Path, package: &Path, destination: &Path) {
    let output = Command::new(npm)
        .arg("pack")
        .arg("--pack-destination")
        .arg(destination)
        .arg("--ignore-scripts")
        .current_dir(package)
        .output()
        .expect("run npm pack");

    assert!(
        output.status.success(),
        "npm pack failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_consumer_project(workspace: &Path) {
    fs::write(
        workspace.join("package.json"),
        format!(
            r#"{{
  "name": "harn-offline-consumer",
  "version": "1.0.0",
  "private": true,
  "dependencies": {{
    "{PACKAGE_NAME}": "file:vendor/{TARBALL_NAME}"
  }}
}}
"#
        ),
    )
    .expect("write consumer package.json");
    fs::write(
        workspace.join(".npmrc"),
        "registry=http://127.0.0.1:9/\n\
         offline=true\n\
         cache=.npm-cache\n\
         audit=false\n\
         fund=false\n\
         update-notifier=false\n\
         progress=false\n",
    )
    .expect("write project npmrc");
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|dir| dir.join(program))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}
