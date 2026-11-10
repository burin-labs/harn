use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BUILD_REVISION_ENV: &str = "HARN_BUILD_REVISION";
const BUILD_FRESHNESS_ENV: &str = "HARN_BUILD_FRESHNESS_ID";

pub struct SourceRevision {
    pub revision: String,
    pub watched_paths: Vec<PathBuf>,
}

pub fn emit() {
    println!("cargo:rerun-if-env-changed={BUILD_REVISION_ENV}");
    println!("cargo:rerun-if-env-changed={BUILD_FRESHNESS_ENV}");

    let raw = std::env::var(BUILD_REVISION_ENV).ok();
    let override_revision = normalize(raw.as_deref())
        .unwrap_or_else(|message| panic!("{BUILD_REVISION_ENV} {message}"));
    let revision = if let Some(revision) = override_revision {
        revision.to_owned()
    } else {
        let manifest_dir =
            PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
        match derive(&manifest_dir) {
            Ok(source) => {
                for path in source.watched_paths {
                    println!("cargo:rerun-if-changed={}", path.display());
                }
                source.revision
            }
            Err(message) => {
                println!("cargo:warning=Harn source revision unavailable: {message}; set {BUILD_REVISION_ENV} to an attested revision");
                String::new()
            }
        }
    };
    println!("cargo:rustc-env={BUILD_REVISION_ENV}={revision}");
    let raw_freshness = std::env::var(BUILD_FRESHNESS_ENV).ok();
    let freshness = normalize(raw_freshness.as_deref())
        .unwrap_or_else(|message| panic!("{BUILD_FRESHNESS_ENV} {message}"));
    println!(
        "cargo:rustc-env={BUILD_FRESHNESS_ENV}={}",
        freshness.unwrap_or_default()
    );
}

pub fn derive(manifest_dir: &Path) -> Result<SourceRevision, String> {
    let vcs_path = manifest_dir.join(".cargo_vcs_info.json");
    match fs::read_to_string(&vcs_path) {
        Ok(text) => {
            #[derive(serde::Deserialize)]
            struct CargoVcsInfo {
                git: CargoGit,
            }
            #[derive(serde::Deserialize)]
            struct CargoGit {
                sha1: String,
            }
            let info: CargoVcsInfo = serde_json::from_str(&text)
                .map_err(|error| format!("invalid Cargo source metadata: {error}"))?;
            return Ok(SourceRevision {
                revision: measured_revision(&info.git.sha1)?,
                watched_paths: vec![vcs_path],
            });
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot read Cargo source metadata: {error}")),
    }

    // A vendored crate must not inherit an enclosing consumer checkout's identity.
    let manifest_dir = manifest_dir
        .canonicalize()
        .map_err(|error| format!("cannot resolve crate source: {error}"))?;
    let root = PathBuf::from(git_text(&manifest_dir, &["rev-parse", "--show-toplevel"])?)
        .canonicalize()
        .map_err(|error| format!("cannot resolve Git source root: {error}"))?;
    if manifest_dir != root && manifest_dir != root.join("crates/harn-cli") {
        return Err("crate is outside the Harn Git source layout".to_owned());
    }
    let manifest_dir = manifest_dir.as_path();
    git_text(
        manifest_dir,
        &["ls-files", "--error-unmatch", "--", "Cargo.toml"],
    )?;
    let revision = measured_revision(&git_text(manifest_dir, &["rev-parse", "--verify", "HEAD"])?)?;
    let mut watched_paths = vec![git_path(manifest_dir, "HEAD")?];
    let symbolic = git(manifest_dir, &["symbolic-ref", "--quiet", "HEAD"])?;
    if symbolic.status.success() {
        let reference = String::from_utf8(symbolic.stdout)
            .map_err(|_| "Git reference is not UTF-8".to_owned())?;
        let reference_path = git_path(manifest_dir, reference.trim())?;
        let watch = if reference_path.is_file() {
            reference_path
        } else {
            reference_path
                .ancestors()
                .find(|path| path.is_dir())
                .ok_or_else(|| "Git reference directory unavailable".to_owned())?
                .to_path_buf()
        };
        watched_paths.push(watch);
    } else if symbolic.status.code() != Some(1) {
        return Err("cannot read Git HEAD selection".to_owned());
    }
    let packed = git_path(manifest_dir, "packed-refs")?;
    if packed.is_file() {
        watched_paths.push(packed);
    }
    if root.join(".git").is_file() {
        watched_paths.push(root.join(".git"));
    }
    Ok(SourceRevision {
        revision,
        watched_paths,
    })
}

fn measured_revision(raw: &str) -> Result<String, String> {
    normalize(Some(raw))
        .map_err(str::to_owned)?
        .filter(|revision| !revision.bytes().all(|byte| byte == b'0'))
        .map(str::to_owned)
        .ok_or_else(|| "source metadata has no complete Git revision".to_owned())
}

fn git(manifest_dir: &Path, args: &[&str]) -> Result<Output, String> {
    let mut command = Command::new("git");
    command.current_dir(manifest_dir).args(args);
    for key in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_PREFIX",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        command.env_remove(key);
    }
    command
        .output()
        .map_err(|error| format!("cannot inspect Git source: {error}"))
}

fn git_text(manifest_dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = git(manifest_dir, args)?;
    if !output.status.success() {
        return Err("crate manifest is not in an available Git source checkout".to_owned());
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim().to_owned())
        .map_err(|_| "Git source metadata is not UTF-8".to_owned())
}

fn git_path(manifest_dir: &Path, name: &str) -> Result<PathBuf, String> {
    git_text(
        manifest_dir,
        &["rev-parse", "--path-format=absolute", "--git-path", name],
    )
    .map(PathBuf::from)
}

pub fn normalize(raw: Option<&str>) -> Result<Option<&str>, &'static str> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let revision = raw.trim();
    if revision.is_empty() {
        return Ok(None);
    }
    if !matches!(revision.len(), 40 | 64)
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("must be a full lowercase 40- or 64-character hexadecimal object ID");
    }
    Ok(Some(revision))
}
