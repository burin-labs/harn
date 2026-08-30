#![allow(
    dead_code,
    reason = "one source module is compiled by both the Harn producer and private checker binary"
)]

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

const FORMAT: &[u8] = b"harn-freshness-manifest-v4\0";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    File,
    Directory,
    Symlink,
    Missing,
}

/// The semantic content authority for one manifest path.
///
/// Most inputs are exact file bytes. Git configuration is different: the
/// source inventory depends on a small set of resolved options, while normal
/// branch and remote bookkeeping in the same file is unrelated and churns in
/// every linked worktree. The native verifier owns this projection so hook
/// checks never rely on a shell-generated cache.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ContentKind {
    Exact,
    GitConfig,
}

impl ContentKind {
    fn code(self) -> u8 {
        match self {
            Self::Exact => 1,
            Self::GitConfig => 2,
        }
    }

    fn from_code(code: u8) -> Result<Self, String> {
        match code {
            1 => Ok(Self::Exact),
            2 => Ok(Self::GitConfig),
            _ => Err(format!("unknown freshness-manifest content kind {code}")),
        }
    }
}

#[derive(Debug)]
struct Authority {
    path: PathBuf,
    content_kind: ContentKind,
}

impl EntryKind {
    fn marker(self) -> &'static str {
        match self {
            Self::File => "f",
            Self::Directory => "d",
            Self::Symlink => "l",
            Self::Missing => "m",
        }
    }

    fn code(self) -> u8 {
        match self {
            Self::File => 1,
            Self::Directory => 2,
            Self::Symlink => 3,
            Self::Missing => 4,
        }
    }

    fn from_code(code: u8) -> Result<Self, String> {
        match code {
            1 => Ok(Self::File),
            2 => Ok(Self::Directory),
            3 => Ok(Self::Symlink),
            4 => Ok(Self::Missing),
            _ => Err(format!("unknown freshness-manifest entry kind {code}")),
        }
    }
}

#[derive(Debug)]
struct CapturedEntry {
    path: PathBuf,
    kind: EntryKind,
    content_kind: ContentKind,
    stat: blake3::Hash,
    content: Option<blake3::Hash>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum Verification {
    Fresh,
    InventoryChanged(PathBuf),
}

/// Write the exact content authority used by the no-build fast path.
///
/// Git determines the source inventory once, at the owning build/receipt
/// boundary. The manifest records content hashes plus platform file identities
/// for those paths. The checker re-reads content in bounded parallel batches;
/// metadata remains inventory and accidental-artifact evidence, never a
/// shortcut around exact source bytes.
pub(super) fn write_manifest(
    output: &Path,
    repo_root: &Path,
    git_covered: &BTreeSet<PathBuf>,
    dep_info: &Path,
    dependencies: &[(PathBuf, bool)],
    authority_list: &Path,
) -> Result<(), String> {
    let mut paths = BTreeMap::<PathBuf, ContentKind>::new();

    for relative in git_covered {
        let path = repo_root.join(relative);
        add_git_path(&mut paths, &path, repo_root)?;
    }
    add_file(&mut paths, dep_info)?;
    for (dependency, git_owned) in dependencies {
        if !git_owned {
            add_tree(&mut paths, dependency)?;
        }
    }
    for authority in read_authorities(authority_list)? {
        add_authority(&mut paths, &authority)?;
    }

    let mut paths = paths.into_iter().collect::<Vec<_>>();
    paths.sort_by_key(|(path, _)| os_bytes(path.as_os_str()));
    let entry_count = u64::try_from(paths.len())
        .map_err(|_| "freshness manifest contains too many paths".to_owned())?;
    let mut encoded = Vec::with_capacity(paths.len().saturating_mul(128));
    encoded.extend_from_slice(FORMAT);
    encoded.extend_from_slice(&entry_count.to_le_bytes());
    let mut content_buffer = vec![0_u8; 1024 * 1024];
    for (path, content_kind) in paths {
        let entry = capture(&path, content_kind, &mut content_buffer)?;
        let path_bytes = os_bytes(entry.path.as_os_str());
        let path_length = u32::try_from(path_bytes.len())
            .map_err(|_| format!("manifest path is too long: {}", entry.path.display()))?;
        encoded.push(entry.kind.code());
        encoded.push(entry.content_kind.code());
        encoded.extend_from_slice(&path_length.to_le_bytes());
        encoded.extend_from_slice(&path_bytes);
        encoded.extend_from_slice(entry.stat.as_bytes());
        if let Some(content) = entry.content {
            encoded.extend_from_slice(content.as_bytes());
        }
    }
    fs::write(output, encoded).map_err(|error| {
        format!(
            "cannot write freshness manifest {}: {error}",
            output.display()
        )
    })
}

fn add_authority(
    paths: &mut BTreeMap<PathBuf, ContentKind>,
    authority: &Authority,
) -> Result<(), String> {
    let metadata = match fs::symlink_metadata(&authority.path) {
        Ok(metadata) => metadata,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && authority.content_kind == ContentKind::GitConfig =>
        {
            return insert_path(paths, authority.path.clone(), authority.content_kind);
        }
        Err(error) => {
            return Err(format!(
                "cannot inspect manifest authority {}: {error}",
                authority.path.display()
            ));
        }
    };
    if authority.content_kind == ContentKind::GitConfig && !metadata.file_type().is_file() {
        return Err(format!(
            "Git configuration authority is not a regular file: {}",
            authority.path.display()
        ));
    }
    if metadata.file_type().is_dir() {
        insert_path(paths, authority.path.clone(), authority.content_kind)
    } else {
        add_file_with_kind(paths, &authority.path, authority.content_kind)
    }
}

fn insert_path(
    paths: &mut BTreeMap<PathBuf, ContentKind>,
    path: PathBuf,
    content_kind: ContentKind,
) -> Result<(), String> {
    if let Some(recorded_kind) = paths.get(&path) {
        if *recorded_kind != content_kind {
            return Err(format!(
                "manifest input has conflicting content authorities: {}",
                path.display()
            ));
        }
    }
    paths.insert(path, content_kind);
    Ok(())
}

fn add_git_path(
    paths: &mut BTreeMap<PathBuf, ContentKind>,
    path: &Path,
    repo_root: &Path,
) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            insert_path(paths, path.to_path_buf(), ContentKind::Exact)?;
        }
        Ok(_) => {
            return Err(format!("Git-owned path is a directory: {}", path.display()));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            insert_path(paths, path.to_path_buf(), ContentKind::Exact)?;
        }
        Err(error) => {
            return Err(format!(
                "cannot inspect Git-owned manifest input {}: {error}",
                path.display()
            ));
        }
    }
    let mut ancestor = path.parent();
    while let Some(directory) = ancestor {
        insert_path(paths, directory.to_path_buf(), ContentKind::Exact)?;
        if directory == repo_root {
            return Ok(());
        }
        ancestor = directory.parent();
    }
    Err(format!(
        "Git-owned path is outside repository root: {}",
        path.display()
    ))
}

fn add_file(paths: &mut BTreeMap<PathBuf, ContentKind>, path: &Path) -> Result<(), String> {
    add_file_with_kind(paths, path, ContentKind::Exact)
}

fn add_file_with_kind(
    paths: &mut BTreeMap<PathBuf, ContentKind>,
    path: &Path,
    content_kind: ContentKind,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect manifest input {}: {error}", path.display()))?;
    if metadata.file_type().is_dir() {
        return Err(format!(
            "manifest file input is a directory: {}",
            path.display()
        ));
    }
    insert_path(paths, path.to_path_buf(), content_kind)
}

fn add_tree(paths: &mut BTreeMap<PathBuf, ContentKind>, path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect manifest input {}: {error}", path.display()))?;
    if path
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| {
            crate::path_policy::is_harn_internal_entry(
                name,
                crate::path_policy::PathEntryKind::from_is_directory(metadata.file_type().is_dir()),
            )
        })
    {
        return Ok(());
    }
    insert_path(paths, path.to_path_buf(), ContentKind::Exact)?;
    if !metadata.file_type().is_dir() {
        return Ok(());
    }
    let mut children = fs::read_dir(path)
        .map_err(|error| {
            format!(
                "cannot enumerate manifest input {}: {error}",
                path.display()
            )
        })?
        .map(|entry| {
            entry.map(|entry| entry.path()).map_err(|error| {
                format!(
                    "cannot enumerate manifest input {}: {error}",
                    path.display()
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|path| os_bytes(path.as_os_str()));
    for child in children {
        add_tree(paths, &child)?;
    }
    Ok(())
}

fn capture(
    path: &Path,
    content_kind: ContentKind,
    content_buffer: &mut [u8],
) -> Result<CapturedEntry, String> {
    let (kind, stat, metadata) = inspect(path)?;
    let content = content_identity(path, kind, content_kind, metadata.as_ref(), content_buffer)?;
    Ok(CapturedEntry {
        path: path.to_path_buf(),
        kind,
        content_kind,
        stat,
        content,
    })
}

fn inspect(path: &Path) -> Result<(EntryKind, blake3::Hash, Option<fs::Metadata>), String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((EntryKind::Missing, missing_stat_id(), None));
        }
        Err(error) => {
            return Err(format!(
                "cannot inspect manifest input {}: {error}",
                path.display()
            ));
        }
    };
    let kind = if metadata.file_type().is_file() {
        EntryKind::File
    } else if metadata.file_type().is_dir() {
        EntryKind::Directory
    } else if metadata.file_type().is_symlink() {
        EntryKind::Symlink
    } else {
        return Err(format!(
            "manifest input is not a regular file, directory, or symbolic link: {}",
            path.display()
        ));
    };
    let stat = metadata_identity(path, &metadata, kind)?;
    Ok((kind, stat, Some(metadata)))
}

fn content_identity(
    path: &Path,
    kind: EntryKind,
    content_kind: ContentKind,
    metadata: Option<&fs::Metadata>,
    content_buffer: &mut [u8],
) -> Result<Option<blake3::Hash>, String> {
    if content_kind == ContentKind::GitConfig {
        if kind == EntryKind::Missing {
            return Ok(None);
        }
        if kind != EntryKind::File {
            return Err(format!(
                "Git configuration authority is not a regular file: {}",
                path.display()
            ));
        }
        return git_config_projection_hash(path).map(Some);
    }
    let content = match kind {
        EntryKind::File => Some(hash_file(
            path,
            metadata
                .ok_or_else(|| format!("manifest file metadata is missing: {}", path.display()))?,
            content_buffer,
        )?),
        EntryKind::Symlink => {
            let target = fs::read_link(path).map_err(|error| {
                format!("cannot read manifest symlink {}: {error}", path.display())
            })?;
            Some(blake3::hash(&os_bytes(target.as_os_str())))
        }
        EntryKind::Directory => Some(directory_inventory_identity(path)?),
        EntryKind::Missing => None,
    };
    Ok(content)
}

fn git_config_projection_hash(path: &Path) -> Result<blake3::Hash, String> {
    let source = fs::read_to_string(path).map_err(|error| {
        format!(
            "cannot read Git configuration authority {}: {error}",
            path.display()
        )
    })?;
    let projection = git_config_inventory_projection(&source).map_err(|error| {
        format!(
            "cannot project Git configuration authority {}: {error}",
            path.display()
        )
    })?;
    Ok(blake3::hash(&projection))
}

#[derive(Debug)]
struct GitConfigSection {
    name: String,
    identity: String,
}

/// Return the subset of one Git config file that can change Harn's source
/// inventory or its exact content semantics.
///
/// The build-time shell records the paths Git resolved from repository, global,
/// system, and included scopes. This byte-local parser deliberately does not
/// invoke Git again when a hook verifies a receipt. The projection excludes
/// branch tracking, remotes, identities, and other bookkeeping that cannot
/// affect the worktree while retaining `include` directives: adding an include
/// after a receipt invalidates its parent authority rather than becoming an
/// unobserved source of inventory settings.
///
/// Git only reports an included file once it contributes a setting. An include
/// target that is absent or empty when a receipt is written is therefore not a
/// manifest authority until the next build. The parent directive is still
/// projected, so changing that directive fails closed; creating or editing an
/// already-discovered included file is also covered.
fn git_config_inventory_projection(source: &str) -> Result<Vec<u8>, String> {
    let mut projection = b"harn-git-inventory-config-v1\0".to_vec();
    let mut section = None::<GitConfigSection>;
    let mut logical_line = String::new();
    let mut logical_line_start = 1_usize;

    for (index, raw_line) in source.split('\n').enumerate() {
        let line_number = index + 1;
        let raw_line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if logical_line.is_empty() {
            logical_line_start = line_number;
        }
        logical_line.push_str(raw_line);
        if has_unescaped_trailing_backslash(&logical_line) {
            logical_line.pop();
            continue;
        }
        parse_git_config_logical_line(
            &logical_line,
            logical_line_start,
            &mut section,
            &mut projection,
        )?;
        logical_line.clear();
    }
    if !logical_line.is_empty() {
        return Err(format!(
            "line {logical_line_start} ends with a continued value"
        ));
    }
    Ok(projection)
}

fn has_unescaped_trailing_backslash(line: &str) -> bool {
    line.bytes().rev().take_while(|byte| *byte == b'\\').count() % 2 == 1
}

fn parse_git_config_logical_line(
    line: &str,
    line_number: usize,
    section: &mut Option<GitConfigSection>,
    projection: &mut Vec<u8>,
) -> Result<(), String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
        return Ok(());
    }
    if line.starts_with('[') {
        *section = Some(parse_git_config_section(line, line_number)?);
        return Ok(());
    }
    let section = section
        .as_ref()
        .ok_or_else(|| format!("line {line_number} has a setting before its section"))?;
    let (key, value) = parse_git_config_setting(line, line_number)?;
    if !is_inventory_git_config_setting(&section.name, &key) {
        return Ok(());
    }
    write_projection_field(projection, &section.identity);
    write_projection_field(projection, &key);
    write_projection_field(projection, &value);
    Ok(())
}

fn parse_git_config_section(line: &str, line_number: usize) -> Result<GitConfigSection, String> {
    let inner = line
        .strip_prefix('[')
        .and_then(|line| line.strip_suffix(']'))
        .map(str::trim)
        .filter(|inner| !inner.is_empty())
        .ok_or_else(|| format!("line {line_number} has an invalid section header"))?;
    let name = inner
        .split(|character: char| {
            character == '.' || character == '"' || character.is_ascii_whitespace()
        })
        .next()
        .unwrap_or_default();
    if !is_git_config_name(name) {
        return Err(format!("line {line_number} has an invalid section name"));
    }
    Ok(GitConfigSection {
        name: name.to_ascii_lowercase(),
        // A filter subsection selects a distinct driver. Preserve the complete
        // header identity while normalizing the section name used for policy.
        identity: inner.to_owned(),
    })
}

fn parse_git_config_setting(line: &str, line_number: usize) -> Result<(String, String), String> {
    let (raw_key, raw_value) = line.split_once('=').unwrap_or((line, "true"));
    let key = raw_key.trim();
    if !is_git_config_name(key) {
        return Err(format!("line {line_number} has an invalid setting name"));
    }
    Ok((
        key.to_ascii_lowercase(),
        normalize_git_config_value(raw_value, line_number)?,
    ))
}

fn is_git_config_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn normalize_git_config_value(value: &str, line_number: usize) -> Result<String, String> {
    let mut normalized = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut previous_is_whitespace = true;
    for character in value.trim_start().chars() {
        if escaped {
            normalized.push(match character {
                '"' | '\\' => character,
                'n' => '\n',
                't' => '\t',
                'b' => '\u{0008}',
                _ => return Err(format!("line {line_number} has an invalid escape sequence")),
            });
            escaped = false;
            previous_is_whitespace = false;
            continue;
        }
        if quoted && character == '\\' {
            escaped = true;
            continue;
        }
        if character == '"' {
            quoted = !quoted;
            previous_is_whitespace = false;
            continue;
        }
        if !quoted && matches!(character, '#' | ';') && previous_is_whitespace {
            break;
        }
        previous_is_whitespace = character.is_ascii_whitespace();
        normalized.push(character);
    }
    if escaped || quoted {
        return Err(format!(
            "line {line_number} has an unterminated quoted value"
        ));
    }
    Ok(normalized.trim_end().to_owned())
}

fn is_inventory_git_config_setting(section: &str, key: &str) -> bool {
    match section {
        "core" => matches!(
            key,
            "attributesfile"
                | "autocrlf"
                | "eol"
                | "excludesfile"
                | "filemode"
                | "ignorecase"
                | "precomposeunicode"
                | "safecrlf"
                | "symlinks"
                | "worktree"
        ),
        "filter" | "include" | "includeif" => true,
        _ => false,
    }
}

fn write_projection_field(projection: &mut Vec<u8>, value: &str) {
    projection.extend_from_slice(&(value.len() as u64).to_le_bytes());
    projection.extend_from_slice(value.as_bytes());
}

fn directory_inventory_identity(path: &Path) -> Result<blake3::Hash, String> {
    let mut children = fs::read_dir(path)
        .map_err(|error| {
            format!(
                "cannot enumerate manifest directory {}: {error}",
                path.display()
            )
        })?
        .map(|entry| {
            let entry = entry.map_err(|error| {
                format!(
                    "cannot enumerate manifest directory {}: {error}",
                    path.display()
                )
            })?;
            let file_type = entry.file_type().map_err(|error| {
                format!(
                    "cannot inspect manifest directory child {}: {error}",
                    entry.path().display()
                )
            })?;
            let kind = if file_type.is_file() {
                EntryKind::File
            } else if file_type.is_dir() {
                EntryKind::Directory
            } else if file_type.is_symlink() {
                EntryKind::Symlink
            } else {
                return Err(format!(
                    "manifest directory child has unsupported type: {}",
                    entry.path().display()
                ));
            };
            let name = entry.file_name();
            // Harn-owned runtime directories are not compiler inputs. Git-owned
            // paths beneath one are added and hashed separately by
            // `add_git_path`, so ignoring an untracked sibling here cannot hide
            // a tracked source change.
            if name.to_str().is_some_and(|name| {
                crate::path_policy::is_harn_internal_entry(
                    name,
                    crate::path_policy::PathEntryKind::from_is_directory(
                        kind == EntryKind::Directory,
                    ),
                )
            }) {
                return Ok(None);
            }
            Ok(Some((os_bytes(&name), kind)))
        })
        .collect::<Result<Vec<_>, String>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    children.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"harn-directory-inventory-v3\0");
    for (name, kind) in children {
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(&name);
        hasher.update(kind.marker().as_bytes());
    }
    Ok(hasher.finalize())
}

pub(super) fn verify_manifest(path: &Path) -> Result<Verification, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("cannot read freshness manifest {}: {error}", path.display()))?;
    let Some(mut input) = bytes.strip_prefix(FORMAT) else {
        return Err("freshness manifest has an unsupported format marker".into());
    };
    let entry_count = usize::try_from(read_u64(&mut input, "entry count")?)
        .map_err(|_| "freshness-manifest entry count does not fit this platform".to_owned())?;
    if entry_count == 0 || entry_count > 1_000_000 {
        return Err("freshness-manifest entry count is implausible".into());
    }
    let mut previous_path = None::<Vec<u8>>;
    let mut entries = Vec::with_capacity(entry_count);
    for offset in 0..entry_count {
        let kind = EntryKind::from_code(read_exact(&mut input, 1, "entry kind")?[0])?;
        let content_kind = ContentKind::from_code(read_exact(&mut input, 1, "content kind")?[0])?;
        let path_length = usize::try_from(read_u32(&mut input, "path length")?)
            .map_err(|_| "freshness-manifest path length does not fit this platform".to_owned())?;
        let path_bytes = read_exact(&mut input, path_length, "path")?.to_vec();
        if previous_path
            .as_ref()
            .is_some_and(|previous| previous >= &path_bytes)
        {
            return Err("freshness-manifest paths are duplicated or not strictly ordered".into());
        }
        previous_path = Some(path_bytes.clone());
        let stat = read_hash(&mut input, "stat identity")?;
        let content = if kind == EntryKind::Missing {
            None
        } else {
            Some(read_hash(&mut input, "content identity")?)
        };
        entries.push((
            path_from_os_bytes(path_bytes)?,
            kind,
            content_kind,
            stat,
            content,
        ));
        if input.is_empty() && offset + 1 != entry_count {
            return Err("freshness manifest ended before its declared entry count".into());
        }
    }
    if !input.is_empty() {
        return Err("freshness manifest contains trailing bytes".into());
    }

    // Git's index may avoid reading a worktree file when size and timestamps
    // appear unchanged. That shortcut is not an exact content authority on
    // every platform (notably when Windows LastWriteTime is restored), so the
    // checker reads every manifest file. Bounded native threads amortize the
    // 10k-file syscall surface without delegating correctness to metadata.
    let worker_count = std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(16)
        .min(entries.len());
    let chunk_size = entries.len().div_ceil(worker_count);
    std::thread::scope(|scope| {
        let handles = entries
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || {
                    let mut content_buffer = vec![0_u8; 1024 * 1024];
                    for (
                        entry_path,
                        recorded_kind,
                        content_kind,
                        _recorded_stat,
                        recorded_content,
                    ) in chunk
                    {
                        let (current_kind, _current_stat, metadata) = inspect(entry_path)?;
                        if *recorded_kind == EntryKind::Missing {
                            if current_kind == EntryKind::Missing {
                                continue;
                            }
                            return Ok(Verification::InventoryChanged(entry_path.clone()));
                        }
                        if current_kind != *recorded_kind {
                            return Err(format!(
                                "manifest input type changed: {}",
                                entry_path.display()
                            ));
                        }
                        if *recorded_kind == EntryKind::Directory {
                            if content_identity(
                                entry_path,
                                current_kind,
                                *content_kind,
                                metadata.as_ref(),
                                &mut content_buffer,
                            )? != *recorded_content
                            {
                                return Ok(Verification::InventoryChanged(entry_path.clone()));
                            }
                            continue;
                        }
                        if content_identity(
                            entry_path,
                            current_kind,
                            *content_kind,
                            metadata.as_ref(),
                            &mut content_buffer,
                        )? != *recorded_content
                        {
                            return Err(format!(
                                "manifest input content changed: {}",
                                entry_path.display()
                            ));
                        }
                    }
                    Ok(Verification::Fresh)
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            let verification = handle
                .join()
                .map_err(|_| "freshness-manifest worker panicked".to_owned())??;
            if !matches!(verification, Verification::Fresh) {
                return Ok(verification);
            }
        }
        Ok(Verification::Fresh)
    })
}

fn read_exact<'a>(input: &mut &'a [u8], length: usize, kind: &str) -> Result<&'a [u8], String> {
    if input.len() < length {
        return Err(format!("freshness manifest ended inside {kind}"));
    }
    let (value, remaining) = input.split_at(length);
    *input = remaining;
    Ok(value)
}

fn read_u32(input: &mut &[u8], kind: &str) -> Result<u32, String> {
    let bytes: [u8; 4] = read_exact(input, 4, kind)?
        .try_into()
        .expect("exact four-byte slice");
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64(input: &mut &[u8], kind: &str) -> Result<u64, String> {
    let bytes: [u8; 8] = read_exact(input, 8, kind)?
        .try_into()
        .expect("exact eight-byte slice");
    Ok(u64::from_le_bytes(bytes))
}

fn read_hash(input: &mut &[u8], kind: &str) -> Result<blake3::Hash, String> {
    let bytes: [u8; 32] = read_exact(input, 32, kind)?
        .try_into()
        .expect("exact 32-byte slice");
    Ok(blake3::Hash::from_bytes(bytes))
}

pub(super) fn file_content_hash(path: &Path) -> Result<blake3::Hash, String> {
    fs::read(path)
        .map(|bytes| blake3::hash(&bytes))
        .map_err(|error| format!("cannot read freshness input {}: {error}", path.display()))
}

// Platform identity catches ordinary mutation/replacement without re-reading a
// 400+ MiB debug binary. It is not a hostile-tamper signature: callers that do
// not trust the local filesystem need a full artifact digest or signature
// verification tier in addition to this developer-worktree receipt.
pub(super) fn artifact_stat_id(path: &Path) -> Result<blake3::Hash, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect executable {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err(format!(
            "executable is not a regular file: {}",
            path.display()
        ));
    }
    metadata_identity(path, &metadata, EntryKind::File)
}

pub(super) fn platform_build_id() -> Result<String, String> {
    buildid::build_id()
        .filter(|build_id| !build_id.is_empty())
        .map(hex::encode)
        .ok_or_else(|| "running executable has no platform build identity".into())
}

pub(super) fn canonical_path_id(path: &Path) -> Result<blake3::Hash, String> {
    let canonical = fs::canonicalize(path)
        .map_err(|error| format!("cannot resolve executable {}: {error}", path.display()))?;
    Ok(blake3::hash(&os_bytes(canonical.as_os_str())))
}

fn metadata_identity(
    path: &Path,
    metadata: &fs::Metadata,
    kind: EntryKind,
) -> Result<blake3::Hash, String> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"harn-file-stat-v2\0");
    hasher.update(kind.marker().as_bytes());
    hasher.update(&metadata.len().to_le_bytes());
    hash_platform_file_identity(&mut hasher, path, metadata)?;
    Ok(hasher.finalize())
}

fn missing_stat_id() -> blake3::Hash {
    blake3::hash(b"harn-file-stat-v2\0m")
}

#[cfg(unix)]
fn hash_platform_file_identity(
    hasher: &mut blake3::Hasher,
    _path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    for value in [
        metadata.dev(),
        metadata.ino(),
        metadata.mtime() as u64,
        metadata.mtime_nsec() as u64,
        metadata.ctime() as u64,
        metadata.ctime_nsec() as u64,
    ] {
        hasher.update(&value.to_le_bytes());
    }
    Ok(())
}

#[cfg(windows)]
fn hash_platform_file_identity(
    hasher: &mut blake3::Hasher,
    path: &Path,
    _metadata: &fs::Metadata,
) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FileBasicInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };

    // `std::os::windows::fs::MetadataExt` does not expose NTFS ChangeTime.
    // Without it, a same-size edit whose LastWriteTime is restored could hit
    // the unchanged fast path without re-hashing content. Query the owning OS
    // API directly for every manifest input, including directories and
    // reparse points, so the Windows optimization remains exact.
    const FILE_READ_ATTRIBUTES: u32 = 0x0080;
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(format!(
            "cannot open manifest input {} for Windows change identity: {}",
            path.display(),
            std::io::Error::last_os_error()
        ));
    }
    let mut identity = BY_HANDLE_FILE_INFORMATION::default();
    let identity_ok = unsafe { GetFileInformationByHandle(handle, &mut identity) };
    let identity_error = (identity_ok == 0).then(std::io::Error::last_os_error);
    let mut basic = FILE_BASIC_INFO::default();
    let basic_ok = unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileBasicInfo,
            std::ptr::from_mut(&mut basic).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    let basic_error = (basic_ok == 0).then(std::io::Error::last_os_error);
    unsafe {
        CloseHandle(handle);
    }
    if let Some(error) = identity_error.or(basic_error) {
        return Err(format!(
            "cannot query manifest input {} for Windows change identity: {error}",
            path.display()
        ));
    }
    for value in [
        u64::from(identity.dwVolumeSerialNumber),
        (u64::from(identity.nFileIndexHigh) << 32) | u64::from(identity.nFileIndexLow),
        (u64::from(identity.ftCreationTime.dwHighDateTime) << 32)
            | u64::from(identity.ftCreationTime.dwLowDateTime),
        (u64::from(identity.ftLastWriteTime.dwHighDateTime) << 32)
            | u64::from(identity.ftLastWriteTime.dwLowDateTime),
    ] {
        hasher.update(&value.to_le_bytes());
    }
    hasher.update(&basic.ChangeTime.to_le_bytes());
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn hash_platform_file_identity(
    hasher: &mut blake3::Hasher,
    _path: &Path,
    metadata: &fs::Metadata,
) -> Result<(), String> {
    let modified = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    hasher.update(&modified.to_le_bytes());
    Ok(())
}

fn hash_file(
    path: &Path,
    metadata: &fs::Metadata,
    buffer: &mut [u8],
) -> Result<blake3::Hash, String> {
    let mut hasher = blake3::Hasher::new();
    let mut file = File::open(path)
        .map_err(|error| format!("cannot read manifest input {}: {error}", path.display()))?;
    hasher.update(b"harn-file-content-v2\0");
    hash_platform_permissions(&mut hasher, metadata);
    loop {
        let read = file
            .read(buffer)
            .map_err(|error| format!("cannot read manifest input {}: {error}", path.display()))?;
        if read == 0 {
            return Ok(hasher.finalize());
        }
        hasher.update(&buffer[..read]);
    }
}

#[cfg(unix)]
fn hash_platform_permissions(hasher: &mut blake3::Hasher, metadata: &fs::Metadata) {
    use std::os::unix::fs::MetadataExt;
    hasher.update(&[u8::from(metadata.mode() & 0o111 != 0)]);
}

#[cfg(windows)]
fn hash_platform_permissions(hasher: &mut blake3::Hasher, _metadata: &fs::Metadata) {
    // Git does not project a portable executable bit from Windows attributes;
    // bytes and entry kind are the semantic content authority on this host.
    hasher.update(b"windows");
}

#[cfg(not(any(unix, windows)))]
fn hash_platform_permissions(hasher: &mut blake3::Hasher, _metadata: &fs::Metadata) {
    hasher.update(b"portable");
}

fn read_authorities(path: &Path) -> Result<Vec<Authority>, String> {
    let bytes = fs::read(path).map_err(|error| {
        format!(
            "cannot read authority path list {}: {error}",
            path.display()
        )
    })?;
    if bytes.last().is_some_and(|byte| *byte != 0) {
        return Err(format!(
            "authority path list is not NUL-terminated: {}",
            path.display()
        ));
    }
    bytes
        .split(|byte| *byte == 0)
        .filter(|value| !value.is_empty())
        .map(|value| {
            let (tag, path) = value
                .split_first()
                .ok_or_else(|| "authority list contains an empty record".to_owned())?;
            let content_kind = match tag {
                b'f' => ContentKind::Exact,
                b'g' => ContentKind::GitConfig,
                _ => return Err("authority list contains an unknown content kind".into()),
            };
            if path.is_empty() {
                return Err("authority list contains an empty path".into());
            }
            String::from_utf8(path.to_vec())
                .map(PathBuf::from)
                .map(|path| Authority { path, content_kind })
                .map_err(|_| "authority path list contains a non-UTF-8 shell path".into())
        })
        .collect()
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

#[cfg(unix)]
fn path_from_os_bytes(bytes: Vec<u8>) -> Result<PathBuf, String> {
    use std::os::unix::ffi::OsStringExt;
    Ok(OsString::from_vec(bytes).into())
}

#[cfg(windows)]
fn path_from_os_bytes(bytes: Vec<u8>) -> Result<PathBuf, String> {
    use std::os::windows::ffi::OsStringExt;
    if bytes.len() % 2 != 0 {
        return Err("Windows path encoding has an odd byte count".into());
    }
    let wide = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    Ok(OsString::from_wide(&wide).into())
}

#[cfg(not(any(unix, windows)))]
fn path_from_os_bytes(bytes: Vec<u8>) -> Result<PathBuf, String> {
    String::from_utf8(bytes)
        .map(PathBuf::from)
        .map_err(|_| "manifest path is not UTF-8".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_content_hash_ignores_metadata_churn_but_rejects_changed_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let input = temp.path().join("proof-checker");
        fs::write(&input, b"exact-proof").unwrap();
        let recorded = file_content_hash(&input).unwrap();

        File::options()
            .write(true)
            .open(&input)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH))
            .unwrap();
        assert_eq!(file_content_hash(&input).unwrap(), recorded);

        fs::write(&input, b"other-proof").unwrap();
        assert_ne!(file_content_hash(&input).unwrap(), recorded);
    }

    #[test]
    fn changed_content_with_recorded_mtime_and_size_is_not_fresh() {
        let temp = tempfile::tempdir().unwrap();
        let root = &temp.path().join("repo");
        fs::create_dir(root).unwrap();
        let input = root.join("input.harn");
        fs::write(&input, b"before").unwrap();
        let covered = BTreeSet::from([PathBuf::from("input.harn")]);
        let dep_info = temp.path().join("harn.d");
        fs::write(&dep_info, b"target:\n").unwrap();
        let authorities = temp.path().join("authorities");
        fs::write(&authorities, []).unwrap();
        let manifest = temp.path().join("manifest");
        write_manifest(&manifest, root, &covered, &dep_info, &[], &authorities).unwrap();

        let recorded_mtime = fs::metadata(&input).unwrap().modified().unwrap();
        fs::write(&input, b"after!").unwrap();
        File::options()
            .write(true)
            .open(&input)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(recorded_mtime))
            .unwrap();
        let error = verify_manifest(&manifest).unwrap_err();
        assert!(error.contains("content changed"));
    }

    #[test]
    fn directory_inventory_changes_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        let root = &temp.path().join("repo");
        fs::create_dir(root).unwrap();
        let input = root.join("input.harn");
        fs::write(&input, b"source").unwrap();
        let covered = BTreeSet::from([PathBuf::from("input.harn")]);
        let dep_info = temp.path().join("harn.d");
        fs::write(&dep_info, b"target:\n").unwrap();
        let authorities = temp.path().join("authorities");
        fs::write(&authorities, []).unwrap();
        let manifest = temp.path().join("manifest");
        write_manifest(&manifest, root, &covered, &dep_info, &[], &authorities).unwrap();

        #[cfg(unix)]
        let recorded_mtime = fs::metadata(root).unwrap().modified().unwrap();
        fs::write(root.join("new.harn"), b"new").unwrap();
        #[cfg(unix)]
        File::open(root)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(recorded_mtime))
            .unwrap();
        let verification = verify_manifest(&manifest);
        assert!(
            matches!(
                &verification,
                Ok(Verification::InventoryChanged(path)) if path == root
            ),
            "unexpected verification result: {verification:?}"
        );
    }

    #[test]
    fn harn_internal_directory_churn_is_not_source_inventory() {
        let temp = tempfile::tempdir().unwrap();
        let root = &temp.path().join("repo");
        fs::create_dir(root).unwrap();
        let input = root.join("input.harn");
        fs::write(&input, b"source").unwrap();
        let covered = BTreeSet::from([PathBuf::from("input.harn")]);
        let dep_info = temp.path().join("harn.d");
        fs::write(&dep_info, b"target:\n").unwrap();
        let authorities = temp.path().join("authorities");
        fs::write(&authorities, []).unwrap();
        let manifest = temp.path().join("manifest");
        write_manifest(&manifest, root, &covered, &dep_info, &[], &authorities).unwrap();

        for name in [".harn", ".harn-runs", ".harn-toolchain-cache"] {
            let internal = root.join(name);
            fs::create_dir(&internal).unwrap();
            fs::write(internal.join("runtime-artifact"), b"mutable").unwrap();
        }

        assert_eq!(verify_manifest(&manifest).unwrap(), Verification::Fresh);
    }

    #[test]
    fn dependency_tree_harn_internal_churn_is_not_source_input() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir(&root).unwrap();
        let dependency = temp.path().join("dependency");
        let internal = dependency.join(".harn/memory");
        fs::create_dir_all(&internal).unwrap();
        let source = dependency.join("source.harn");
        let runtime_state = internal.join("events.jsonl");
        fs::write(&source, b"source").unwrap();
        fs::write(&runtime_state, b"before").unwrap();
        let dep_info = temp.path().join("harn.d");
        fs::write(&dep_info, b"target:\n").unwrap();
        let authorities = temp.path().join("authorities");
        fs::write(&authorities, []).unwrap();
        let manifest = temp.path().join("manifest");
        write_manifest(
            &manifest,
            &root,
            &BTreeSet::new(),
            &dep_info,
            &[(dependency, false)],
            &authorities,
        )
        .unwrap();

        fs::write(&runtime_state, b"after!").unwrap();
        assert_eq!(verify_manifest(&manifest).unwrap(), Verification::Fresh);

        fs::write(&source, b"changed").unwrap();
        assert!(verify_manifest(&manifest)
            .unwrap_err()
            .contains("content changed"));
    }

    #[test]
    fn tracked_input_beneath_harn_internal_directory_remains_exact() {
        let temp = tempfile::tempdir().unwrap();
        let root = &temp.path().join("repo");
        let internal = root.join(".harn/fixtures");
        fs::create_dir_all(&internal).unwrap();
        let input = internal.join("input.harn");
        fs::write(&input, b"before").unwrap();
        let covered = BTreeSet::from([PathBuf::from(".harn/fixtures/input.harn")]);
        let dep_info = temp.path().join("harn.d");
        fs::write(&dep_info, b"target:\n").unwrap();
        let authorities = temp.path().join("authorities");
        fs::write(&authorities, []).unwrap();
        let manifest = temp.path().join("manifest");
        write_manifest(&manifest, root, &covered, &dep_info, &[], &authorities).unwrap();

        fs::write(&input, b"after!").unwrap();
        let error = verify_manifest(&manifest).unwrap_err();
        assert!(error.contains("content changed"));
    }

    #[test]
    fn restored_directory_inventory_is_fresh_despite_metadata_churn() {
        let temp = tempfile::tempdir().unwrap();
        let root = &temp.path().join("repo");
        fs::create_dir(root).unwrap();
        let input = root.join("input.harn");
        fs::write(&input, b"source").unwrap();
        let covered = BTreeSet::from([PathBuf::from("input.harn")]);
        let dep_info = temp.path().join("harn.d");
        fs::write(&dep_info, b"target:\n").unwrap();
        let authorities = temp.path().join("authorities");
        fs::write(&authorities, []).unwrap();
        let manifest = temp.path().join("manifest");
        write_manifest(&manifest, root, &covered, &dep_info, &[], &authorities).unwrap();

        let transient = root.join("transient.harn");
        fs::write(&transient, b"transient").unwrap();
        fs::remove_file(transient).unwrap();

        assert_eq!(verify_manifest(&manifest).unwrap(), Verification::Fresh);
    }

    #[test]
    fn missing_git_input_is_an_exact_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = &temp.path().join("repo");
        fs::create_dir(root).unwrap();
        let covered = BTreeSet::from([PathBuf::from("deleted.harn")]);
        let dep_info = temp.path().join("harn.d");
        fs::write(&dep_info, b"target:\n").unwrap();
        let authorities = temp.path().join("authorities");
        fs::write(&authorities, []).unwrap();
        let manifest = temp.path().join("manifest");
        write_manifest(&manifest, root, &covered, &dep_info, &[], &authorities).unwrap();
        assert_eq!(verify_manifest(&manifest).unwrap(), Verification::Fresh);

        let restored = root.join("deleted.harn");
        fs::write(&restored, b"restored").unwrap();
        let verification = verify_manifest(&manifest);
        assert!(
            matches!(
                &verification,
                Ok(Verification::InventoryChanged(path)) if path == root || path == &restored
            ),
            "unexpected verification result: {verification:?}"
        );
    }

    #[test]
    fn git_config_projection_ignores_bookkeeping_but_retains_inventory_settings() {
        let baseline = r#"
            [core]
                repositoryformatversion = 0
            [branch "topic"]
                remote = origin
            [remote "origin"]
                url = https://example.invalid/repository.git
            [user]
                email = contributor@example.invalid
        "#;
        let bookkeeping_churn = r#"
            [core]
                repositoryformatversion = 0
            [branch "topic"]
                remote = upstream
            [remote "origin"]
                url = ssh://example.invalid/repository.git
            [user]
                email = other@example.invalid
        "#;
        assert_eq!(
            git_config_inventory_projection(baseline).unwrap(),
            git_config_inventory_projection(bookkeeping_churn).unwrap()
        );

        let changed_core = format!("{baseline}\n[core]\n\texcludesFile = /tmp/ignored\n");
        assert_ne!(
            git_config_inventory_projection(baseline).unwrap(),
            git_config_inventory_projection(&changed_core).unwrap()
        );

        let changed_filter = format!("{baseline}\n[filter \"rewrite\"]\n\tclean = cat\n");
        assert_ne!(
            git_config_inventory_projection(baseline).unwrap(),
            git_config_inventory_projection(&changed_filter).unwrap()
        );
    }

    #[test]
    fn git_config_authorities_fail_closed_in_every_scope() {
        let temp = tempfile::tempdir().unwrap();
        let root = &temp.path().join("repo");
        fs::create_dir(root).unwrap();
        fs::write(root.join("input.harn"), b"source").unwrap();
        let covered = BTreeSet::from([PathBuf::from("input.harn")]);
        let dep_info = temp.path().join("harn.d");
        fs::write(&dep_info, b"target:\n").unwrap();
        let configs = [
            temp.path().join("repository.gitconfig"),
            temp.path().join("global.gitconfig"),
            temp.path().join("system.gitconfig"),
        ];
        for config in &configs {
            fs::write(config, b"[user]\n\temail = contributor@example.invalid\n").unwrap();
        }
        let authorities = temp.path().join("authorities");
        let mut authority_bytes = Vec::new();
        for config in &configs {
            authority_bytes.push(b'g');
            authority_bytes.extend_from_slice(config.to_str().unwrap().as_bytes());
            authority_bytes.push(0);
        }
        fs::write(&authorities, authority_bytes).unwrap();

        for config in &configs {
            let manifest = temp.path().join(format!(
                "{}.manifest",
                config.file_name().unwrap().to_string_lossy()
            ));
            write_manifest(&manifest, root, &covered, &dep_info, &[], &authorities).unwrap();
            fs::write(config, b"[user]\n\temail = other@example.invalid\n").unwrap();
            assert_eq!(
                verify_manifest(&manifest).unwrap(),
                Verification::Fresh,
                "unrelated configuration unexpectedly invalidated {}",
                config.display()
            );
            fs::write(config, b"[core]\n\texcludesFile = /tmp/inventory-ignore\n").unwrap();
            let error = verify_manifest(&manifest).unwrap_err();
            assert!(
                error.contains("content changed"),
                "scope {} unexpectedly verified: {error}",
                config.display()
            );
            fs::write(config, b"[user]\n\temail = contributor@example.invalid\n").unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn git_executable_bit_change_is_not_fresh() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let root = &temp.path().join("repo");
        fs::create_dir(root).unwrap();
        let input = root.join("script.sh");
        fs::write(&input, b"#!/bin/sh\n").unwrap();
        let covered = BTreeSet::from([PathBuf::from("script.sh")]);
        let dep_info = temp.path().join("harn.d");
        fs::write(&dep_info, b"target:\n").unwrap();
        let authorities = temp.path().join("authorities");
        fs::write(&authorities, []).unwrap();
        let manifest = temp.path().join("manifest");
        write_manifest(&manifest, root, &covered, &dep_info, &[], &authorities).unwrap();

        let mut permissions = fs::metadata(&input).unwrap().permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(&input, permissions).unwrap();
        let error = verify_manifest(&manifest).unwrap_err();
        assert!(error.contains("content changed"));
    }
}
