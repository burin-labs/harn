use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
    process,
};

use super::freshness_manifest::{artifact_stat_id, platform_build_id, write_manifest};

const COMMAND: &str = "__internal-freshness-evidence-v4";
const FORMAT: &str = "harn-artifact-evidence-v4-depfile-0.1.1-manifest-3";

pub(super) fn handle(raw_args: &[String]) -> bool {
    let (command, dep_info, target, repo_root, git_covered_list, manifest) = match raw_args {
        [_, command, dep_info, target, repo_root, git_covered_list] => {
            (command, dep_info, target, repo_root, git_covered_list, None)
        }
        [_, command, dep_info, target, repo_root, git_covered_list, authority_list, manifest] => (
            command,
            dep_info,
            target,
            repo_root,
            git_covered_list,
            Some((Path::new(authority_list), Path::new(manifest))),
        ),
        _ => return false,
    };
    if command != COMMAND {
        return false;
    }

    match Evidence::collect(
        Path::new(dep_info),
        Path::new(target),
        Path::new(repo_root),
        Path::new(git_covered_list),
        manifest,
    ) {
        Ok(evidence) => print!("{evidence}"),
        Err(error) => {
            eprintln!("error: cannot collect Harn artifact freshness evidence: {error}");
            process::exit(1);
        }
    }
    true
}

#[derive(Debug)]
struct Evidence {
    build_freshness: &'static str,
    build_id: String,
    artifact_stat: blake3::Hash,
    dep_info: blake3::Hash,
    dependencies: blake3::Hash,
}

impl Evidence {
    fn collect(
        dep_info: &Path,
        target: &Path,
        repo_root: &Path,
        git_covered_list: &Path,
        manifest: Option<(&Path, &Path)>,
    ) -> Result<Self, String> {
        let target = absolute_existing_regular_file(target, repo_root, "binary")?;
        let dep_info = absolute_existing_regular_file(dep_info, repo_root, "Cargo dep-info")?;
        let repo_root = fs::canonicalize(repo_root).map_err(|error| {
            format!(
                "cannot resolve repository root {}: {error}",
                repo_root.display()
            )
        })?;
        if !repo_root.is_dir() {
            return Err(format!(
                "repository root is not a directory: {}",
                repo_root.display()
            ));
        }
        let git_covered = read_git_covered_paths(git_covered_list)?;
        let target_dir = target
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| format!("binary has no Cargo target root: {}", target.display()))?;

        let dep_info_bytes = fs::read(&dep_info).map_err(|error| {
            format!("cannot read Cargo dep-info {}: {error}", dep_info.display())
        })?;
        let dep_info_text = std::str::from_utf8(&dep_info_bytes).map_err(|error| {
            format!(
                "Cargo dep-info is not UTF-8 at byte {}",
                error.valid_up_to()
            )
        })?;
        let parsed = depfile::parse(dep_info_text)
            .map_err(|offset| format!("Cargo dep-info syntax is invalid at byte {offset}"))?;
        if parsed.len() != 1 {
            return Err(format!(
                "Cargo dep-info must contain exactly one target rule, found {}",
                parsed.len()
            ));
        }
        let (declared_target, dependencies) = parsed.iter().next().expect("one rule was required");
        let declared_target = absolute_existing_regular_file(
            Path::new(declared_target),
            &repo_root,
            "Cargo dep-info target",
        )?;
        if declared_target != target {
            return Err(format!(
                "Cargo dep-info target {} does not resolve to requested binary {}",
                declared_target.display(),
                target.display()
            ));
        }

        let mut resolved = Vec::with_capacity(dependencies.len());
        for dependency in dependencies {
            let dependency = Path::new(dependency.as_ref());
            let candidate = if dependency.is_absolute() {
                dependency.to_path_buf()
            } else {
                repo_root.join(dependency)
            };
            // The exact worktree fingerprint already owns existence, type,
            // path, and content for tracked and nonignored untracked files.
            // Trust that proof instead of canonicalizing and inspecting
            // thousands of Cargo prerequisites a second time on every hook.
            let git_owned = candidate
                .strip_prefix(&repo_root)
                .is_ok_and(|relative| git_covered.contains(relative))
                && fs::symlink_metadata(&candidate)
                    .is_ok_and(|metadata| metadata.file_type().is_file());
            let path = if git_owned {
                candidate
            } else {
                absolute_existing_path(&candidate, &repo_root, "Cargo dependency")?
            };
            resolved.push((path, git_owned));
        }
        resolved.sort_by(|(left, _), (right, _)| {
            os_bytes(left.as_os_str()).cmp(&os_bytes(right.as_os_str()))
        });
        if resolved.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(
                "Cargo dep-info contains duplicate dependencies after path normalization".into(),
            );
        }

        if let Some((authority_list, manifest)) = manifest {
            write_manifest(
                manifest,
                &repo_root,
                &git_covered,
                &dep_info,
                &resolved,
                authority_list,
            )?;
        }

        let mut dependency_hasher = blake3::Hasher::new();
        dependency_hasher.update(FORMAT.as_bytes());
        for (dependency, git_owned) in resolved {
            update_framed(&mut dependency_hasher, &os_bytes(dependency.as_os_str()));
            if git_owned {
                dependency_hasher.update(b"git-covered\0");
            } else {
                hash_path(
                    &mut dependency_hasher,
                    &dependency,
                    &dependency,
                    &repo_root,
                    target_dir,
                    &git_covered,
                )?;
            }
        }

        Ok(Self {
            build_freshness: env!("HARN_BUILD_FRESHNESS_ID"),
            build_id: platform_build_id()?,
            artifact_stat: artifact_stat_id(&target)?,
            dep_info: blake3::hash(&dep_info_bytes),
            dependencies: dependency_hasher.finalize(),
        })
    }
}

impl std::fmt::Display for Evidence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(formatter, "{FORMAT}")?;
        writeln!(formatter, "build-freshness={}", self.build_freshness)?;
        writeln!(formatter, "build-id={}", self.build_id)?;
        writeln!(formatter, "artifact-stat={}", self.artifact_stat)?;
        writeln!(formatter, "dep-info={}", self.dep_info)?;
        writeln!(formatter, "dependencies={}", self.dependencies)
    }
}

fn absolute_existing_regular_file(path: &Path, base: &Path, kind: &str) -> Result<PathBuf, String> {
    let path = absolute_existing_path(path, base, kind)?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("cannot inspect {kind} {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!("{kind} is not a regular file: {}", path.display()));
    }
    Ok(path)
}

fn absolute_existing_path(path: &Path, base: &Path, kind: &str) -> Result<PathBuf, String> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    let metadata = fs::symlink_metadata(&candidate)
        .map_err(|error| format!("cannot inspect {kind} {}: {error}", candidate.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "{kind} may not be a symbolic link: {}",
            candidate.display()
        ));
    }
    fs::canonicalize(&candidate)
        .map_err(|error| format!("cannot resolve {kind} {}: {error}", candidate.display()))
}

fn hash_path(
    hasher: &mut blake3::Hasher,
    path: &Path,
    root: &Path,
    repo_root: &Path,
    target_dir: &Path,
    git_covered: &BTreeSet<PathBuf>,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        format!(
            "cannot inspect Cargo dependency {}: {error}",
            path.display()
        )
    })?;
    let relative = path.strip_prefix(root).unwrap_or(path);
    update_framed(hasher, &os_bytes(relative.as_os_str()));
    if metadata.file_type().is_file() {
        hasher.update(b"file\0");
        if !path.starts_with(target_dir)
            && path
                .strip_prefix(repo_root)
                .is_ok_and(|relative| git_covered.contains(relative))
        {
            hasher.update(b"git-covered\0");
            return Ok(());
        }
        hash_file_into(hasher, path)?;
        return Ok(());
    }
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "Cargo dependency is neither a regular file nor directory: {}",
            path.display()
        ));
    }

    hasher.update(b"directory\0");
    let entries = fs::read_dir(path).map_err(|error| {
        format!(
            "cannot read Cargo dependency directory {}: {error}",
            path.display()
        )
    })?;
    let mut entries = entries
        .map(|entry| {
            entry.map(|entry| entry.path()).map_err(|error| {
                format!(
                    "cannot enumerate Cargo dependency directory {}: {error}",
                    path.display()
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|path| os_bytes(path.as_os_str()));
    for entry in entries {
        hash_path(hasher, &entry, root, repo_root, target_dir, git_covered)?;
    }
    Ok(())
}

fn read_git_covered_paths(path: &Path) -> Result<BTreeSet<PathBuf>, String> {
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "cannot read Git-covered path list {}: {error}",
            path.display()
        )
    })?;
    if bytes.last().is_some_and(|byte| *byte != 0) {
        return Err(format!(
            "Git-covered path list is not NUL-terminated: {}",
            path.display()
        ));
    }
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(git_path_from_bytes)
        .collect()
}

#[cfg(unix)]
fn git_path_from_bytes(bytes: &[u8]) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt;
    Ok(std::ffi::OsString::from_vec(bytes.to_vec()).into())
}

#[cfg(not(unix))]
fn git_path_from_bytes(bytes: &[u8]) -> Result<PathBuf, String> {
    String::from_utf8(bytes.to_vec())
        .map(PathBuf::from)
        .map_err(|_| "Git-covered path list contains non-UTF-8 bytes".into())
}

fn hash_file_into(hasher: &mut blake3::Hasher, path: &Path) -> Result<(), String> {
    let mut file = File::open(path)
        .map_err(|error| format!("cannot read Cargo dependency {}: {error}", path.display()))?;
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("cannot read Cargo dependency {}: {error}", path.display()))?;
        if read == 0 {
            return Ok(());
        }
        hasher.update(&buffer[..read]);
    }
}

fn update_framed(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(unix)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    value.as_bytes().to_vec()
}

#[cfg(windows)]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().flat_map(u16::to_le_bytes).collect()
}

#[cfg(not(any(unix, windows)))]
fn os_bytes(value: &OsStr) -> Vec<u8> {
    value.to_string_lossy().as_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn collect(
        temp: &tempfile::TempDir,
        dep_info: &Path,
        target: &Path,
    ) -> Result<Evidence, String> {
        let covered = temp.path().join("git-covered");
        fs::write(&covered, []).unwrap();
        Evidence::collect(dep_info, target, temp.path(), &covered, None)
    }

    fn depfile_path(path: &Path) -> std::borrow::Cow<'_, str> {
        depfile::escape(path.to_str().expect("temporary test path must be UTF-8"))
    }

    #[test]
    fn depfile_parser_decodes_make_and_windows_escaping_without_loss() {
        let input = concat!(
            "C\\:\\\\build\\ dir\\\\harn.exe: ",
            "C\\:\\\\source\\ dir\\\\embedded\\ file.harn ",
            "D\\:\\\\generated\\\\schema.rs\\\n",
        );
        let parsed = depfile::parse(input).unwrap();
        let (target, dependencies) = parsed.iter().next().unwrap();
        assert_eq!(target, r"C:\build dir\harn.exe");
        assert_eq!(
            dependencies.iter().map(AsRef::as_ref).collect::<Vec<_>>(),
            [
                r"C:\source dir\embedded file.harn",
                r"D:\generated\schema.rs"
            ]
        );
    }

    #[test]
    fn evidence_rejects_multiple_rules_and_non_regular_dependencies() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("harn");
        File::create(&target).unwrap().write_all(b"binary").unwrap();
        let dep_info = temp.path().join("harn.d");
        fs::write(&dep_info, format!("{}:\nother:\n", depfile_path(&target))).unwrap();
        let error = collect(&temp, &dep_info, &target).unwrap_err();
        assert!(error.contains("exactly one target rule"));

        fs::write(
            &dep_info,
            format!("{}: {}\n", depfile_path(&target), depfile_path(temp.path())),
        )
        .unwrap();
        assert!(collect(&temp, &dep_info, &target).is_ok());
    }

    #[test]
    fn directory_dependency_digest_catches_same_mtime_content_changes() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("harn");
        fs::write(&target, b"binary").unwrap();
        let watched = temp.path().join("generated");
        fs::create_dir(&watched).unwrap();
        let child = watched.join("asset.harn");
        fs::write(&child, b"before").unwrap();
        let dep_info = temp.path().join("harn.d");
        fs::write(
            &dep_info,
            format!("{}: {}\n", depfile_path(&target), depfile_path(&watched)),
        )
        .unwrap();
        let before = collect(&temp, &dep_info, &target).unwrap();
        fs::write(&child, b"after!").unwrap();
        let after = collect(&temp, &dep_info, &target).unwrap();
        assert_ne!(before.dependencies, after.dependencies);
    }

    #[test]
    fn running_binary_exposes_a_nonempty_platform_build_id() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("harn");
        fs::write(&target, b"binary-one").unwrap();
        let dep_info = temp.path().join("harn.d");
        fs::write(&dep_info, format!("{}:\n", depfile_path(&target))).unwrap();
        let evidence = collect(&temp, &dep_info, &target).unwrap();
        assert!(!evidence.build_id.is_empty());
        assert!(evidence
            .build_id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn artifact_stat_rejects_a_byte_identical_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("harn");
        let replacement = temp.path().join("replacement");
        fs::write(&target, b"byte-identical").unwrap();
        fs::copy(&target, &replacement).unwrap();
        let before = artifact_stat_id(&target).unwrap();
        fs::remove_file(&target).unwrap();
        fs::rename(&replacement, &target).unwrap();
        let after = artifact_stat_id(&target).unwrap();
        assert_ne!(before, after);
    }
}
