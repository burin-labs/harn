use std::{fs, path::Path, process::Command};

pub(super) fn projection_hash(path: &Path) -> Result<blake3::Hash, String> {
    let git_dir = path
        .parent()
        .ok_or_else(|| "Git HEAD has no parent directory".to_owned())?;
    let output = Command::new("git")
        .arg("--git-dir")
        .arg(git_dir)
        .args(["rev-parse", "--verify", "HEAD"])
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .map_err(|error| {
            format!(
                "cannot resolve Git HEAD authority {}: {error}",
                path.display()
            )
        })?;
    let revision = output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout);
    let revision = revision.strip_suffix(b"\r").unwrap_or(revision);
    if !output.status.success()
        || !matches!(revision.len(), 40 | 64)
        || !revision.iter().all(u8::is_ascii_hexdigit)
        || revision.iter().all(|byte| *byte == b'0')
    {
        return Err(format!(
            "cannot resolve a complete Git HEAD revision: {}",
            path.display()
        ));
    }
    // Keep the symbolic selection too: includeIf "onbranch" can change the
    // source inventory even when two branches point to the same commit.
    let mut hasher = blake3::Hasher::new();
    hasher.update(
        &fs::read(path)
            .map_err(|error| format!("cannot read Git HEAD {}: {error}", path.display()))?,
    );
    hasher.update(revision);
    Ok(hasher.finalize())
}
