//! How a granted `git fetch`, `git push` or `git ls-remote` gets an HTTPS
//! login without running anything the repository names.
//!
//! A `for_command: "git"` grant gives git a github.com login. Git reads the
//! repository's config, runs its hooks, and starts the credential helpers,
//! proxies, filesystem monitors, and transports that config names, and the
//! agent writes that config. Blocking the keys known to run a program would
//! miss the next one, so a granted spawn never opens the repository's git
//! directory as its own:
//!
//! - `GIT_DIR` is the per-spawn directory, with a `config` Harn writes. Its
//!   `objects` and `FETCH_HEAD` link into the repository, and so does
//!   `refs/remotes/NAME` for each remote the command reaches, so fetched
//!   objects and that remote's tracking refs land there. A tracking directory
//!   that is or holds a link when the spawn starts is refused. `HEAD`, the
//!   branch and tag refs, `packed-refs` and `shallow` are copies of regular
//!   files, so the granted git cannot change any other repository ref, and it
//!   writes no repository config. No
//!   system or global config is read (`GIT_CONFIG_NOSYSTEM`, and
//!   `GIT_CONFIG_GLOBAL` is an empty file).
//! - Objects are read through the repository's alternates as the confined
//!   git's own sandbox allows; the per-spawn directory holds no object store.
//! - Fetches never write tags (`tagOpt = --no-tags`), and a fetch refspec
//!   must store into the remote's own tracking namespace.
//! - Of the repository's own config, only remotes whose every URL is a plain
//!   `https://github.com/OWNER/REPO`, their refspecs, and branch tracking are
//!   copied, read with `git config --file` and no includes. `insteadOf`,
//!   `pushurl`, proxies, helpers, hooks, and every other key are ignored.
//! - No file holds the login. Harn answers git's built-in `credential-cache`
//!   helper on a per-spawn socket (see `git_credential_socket`), for those
//!   repository URLs only (`credential.useHttpPath`). A repository that points
//!   a path git reads at the socket or at the per-spawn directory reads no
//!   login, and only this spawn's own sandbox profile may connect to it.
//! - Only `https` is allowed, and hooks, filesystem monitors, submodule
//!   recursion, automatic gc and prompts are off.
//! - Only macOS grants it: a same-user process's access to the granted git's
//!   memory is not yet measured on Linux, and Windows has no confinement.
//! - Only `fetch`, `push` and `ls-remote` with known options are granted. Any
//!   other subcommand (`pull`, whose merge runs filters, and `clone`, which
//!   writes a repository config git then reads), an option that names a
//!   program or writes config, and a remote that is not copied runs without
//!   the login.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::stdlib::process::{ExecProfile, SeedRequest, Seeded};

pub(crate) const GIT_PROFILE: ExecProfile = ExecProfile {
    program: "git",
    empty_env: &[],
    refusing_env: &[
        "GIT_ASKPASS",
        "SSH_ASKPASS",
        "GIT_EDITOR",
        "GIT_SEQUENCE_EDITOR",
        "EDITOR",
        "VISUAL",
        "GIT_PAGER",
        "PAGER",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
    ],
    dir_env: &[],
    credential_env: &["GH_TOKEN", "GITHUB_TOKEN"],
    fixed_env: &[("GIT_CONFIG_NOSYSTEM", "1"), ("GIT_TERMINAL_PROMPT", "0")],
    seed: seed_network_spawn,
    refusal: |_| None,
    grants: |args| parse(args).is_some(),
};

#[cfg(unix)]
const LOGIN_USER: &str = "x-access-token";
/// The login socket's name in the per-spawn directory.
pub(crate) const CREDENTIAL_SOCKET: &str = "credential.sock";
const GLOBAL_CONFIG: &str = "global-config";
/// Repository entries the per-spawn git directory links to, from the shared
/// (common) git directory, and from the worktree's own. Missing files are
/// linked too, so git creates them in the repository.
#[cfg(unix)]
const COMMON_LINKS: &[&str] = &["objects"];
#[cfg(unix)]
const WORKTREE_LINKS: &[&str] = &["FETCH_HEAD"];
/// Ref namespaces copied in for a push to name and a fetch to negotiate from.
#[cfg(unix)]
const COPIED_REFS: &[&str] = &["heads", "tags"];
/// Repository files copied in when they are regular files, and refused when
/// they are anything else. A fetch's change to `shallow` stays in the copy.
#[cfg(unix)]
const COPIED_FILES: &[&str] = &["packed-refs", "shallow"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Command {
    Fetch,
    Push,
    LsRemote,
}

/// A granted invocation: its subcommand, the repositories it names (remote
/// names or URLs; empty for the default), and whether it fetches every remote.
#[derive(Debug, PartialEq)]
struct Invocation<'a> {
    command: Command,
    repositories: Vec<&'a str>,
    all: bool,
    /// A single-repository fetch's refspecs.
    refspecs: Vec<&'a str>,
}

/// Flags each subcommand may take with the login: none runs a program, names
/// another transport, or writes config the per-spawn directory would drop.
/// `-u`/`--set-upstream`, `--signed`, `--recurse-submodules`, `--filter`,
/// `--upload-pack`, `--receive-pack` and `--exec` are deliberately absent.
const COMMON_FLAGS: &[&str] = &[
    "-q",
    "--quiet",
    "-v",
    "--verbose",
    "--progress",
    "--no-progress",
    "-4",
    "--ipv4",
    "-6",
    "--ipv6",
];
const FETCH_FLAGS: &[&str] = &[
    "--all",
    "--multiple",
    "-a",
    "--append",
    "--atomic",
    "--dry-run",
    "-f",
    "--force",
    "-k",
    "--keep",
    "-p",
    "--prune",
    "--no-prune",
    "-n",
    "--no-tags",
    "--write-fetch-head",
    "--no-write-fetch-head",
    "--no-auto-gc",
    "--no-auto-maintenance",
    "--show-forced-updates",
    "--no-show-forced-updates",
    "--refetch",
];
const FETCH_VALUES: &[&str] = &["-j", "--jobs", "-o", "--server-option"];
const PUSH_FLAGS: &[&str] = &[
    "--all",
    "--branches",
    "--mirror",
    "--tags",
    "--follow-tags",
    "--no-follow-tags",
    "--atomic",
    "--no-atomic",
    "-n",
    "--dry-run",
    "--porcelain",
    "-d",
    "--delete",
    "-f",
    "--force",
    "--force-with-lease",
    "--no-force-with-lease",
    "--force-if-includes",
    "--no-force-if-includes",
    "--prune",
    "--no-verify",
    "--verify",
    "--thin",
    "--no-thin",
    "--no-signed",
];
const PUSH_VALUES: &[&str] = &["-o", "--push-option"];
const LS_REMOTE_FLAGS: &[&str] = &[
    "-h",
    "--heads",
    "-b",
    "--branches",
    "-t",
    "--tags",
    "--refs",
    "--symref",
    "--get-url",
    "--exit-code",
];
const LS_REMOTE_VALUES: &[&str] = &["--sort", "-o", "--server-option"];

/// The granted shape of `args`, or `None` when they run without the login.
fn parse(args: &[String]) -> Option<Invocation<'_>> {
    let (first, rest) = args.split_first()?;
    let (command, flags, values) = match first.as_str() {
        "fetch" => (Command::Fetch, FETCH_FLAGS, FETCH_VALUES),
        "push" => (Command::Push, PUSH_FLAGS, PUSH_VALUES),
        "ls-remote" => (Command::LsRemote, LS_REMOTE_FLAGS, LS_REMOTE_VALUES),
        _ => return None,
    };
    let is_flag = |name: &str| COMMON_FLAGS.contains(&name) || flags.contains(&name);
    let mut positionals = Vec::new();
    let mut words = rest.iter();
    let mut options_done = false;
    let (mut all, mut multiple) = (false, false);
    while let Some(word) = words.next() {
        if options_done || !word.starts_with('-') || word == "-" {
            positionals.push(word.as_str());
            continue;
        }
        if word == "--" {
            options_done = true;
            continue;
        }
        if let Some(long) = word.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (format!("--{name}"), Some(value)),
                None => (word.clone(), None),
            };
            let valued = values.contains(&name.as_str());
            let lease = name == "--force-with-lease" && command == Command::Push;
            if valued && inline.is_none() {
                words.next()?;
            } else if !valued && !lease && (inline.is_some() || !is_flag(&name)) {
                return None;
            }
            all |= name == "--all" && command == Command::Fetch;
            multiple |= name == "--multiple";
            continue;
        }
        // A short cluster: flag letters, then at most one valued letter whose
        // value is the rest of the word or the next one.
        let letters: Vec<char> = word.chars().skip(1).collect();
        for (index, letter) in letters.iter().enumerate() {
            let name = format!("-{letter}");
            if values.contains(&name.as_str()) {
                if index + 1 == letters.len() {
                    words.next()?;
                }
                break;
            }
            if !is_flag(&name) {
                return None;
            }
        }
    }
    let mut refspecs = Vec::new();
    let repositories = match (command, all, multiple) {
        (Command::Fetch, true, _) => {
            if !positionals.is_empty() {
                return None;
            }
            Vec::new()
        }
        (Command::Fetch, false, true) => positionals,
        (Command::Fetch, false, false) => {
            refspecs = positionals.iter().skip(1).copied().collect();
            positionals.into_iter().take(1).collect()
        }
        _ => positionals.into_iter().take(1).collect(),
    };
    Some(Invocation {
        command,
        repositories,
        all,
        refspecs,
    })
}

#[cfg(test)]
thread_local! {
    /// A test server's origin, granted in place of github.com over plain HTTP.
    pub(super) static TEST_ORIGIN: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

fn granted_origin() -> String {
    #[cfg(test)]
    if let Some(origin) = TEST_ORIGIN.with(|origin| origin.borrow().clone()) {
        return origin;
    }
    "https://github.com".to_string()
}

/// The repository path of a plain `https://github.com/OWNER/REPO[.git]` URL:
/// no user, port, query, fragment, or further path, and nothing a git URL
/// rewrite or transport prefix could change.
fn granted_url_path(url: &str) -> Option<&str> {
    let origin = granted_origin();
    let path = url.strip_prefix(origin.as_str())?.strip_prefix('/')?;
    let (owner, repo) = path.split_once('/')?;
    let plain = |part: &str| {
        !part.is_empty()
            && !part.starts_with('.')
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    (plain(owner) && plain(repo)).then_some(path)
}

/// Where the repository at or above `cwd` keeps its own files (`gitdir`) and
/// its shared ones (`common`), found by walking up for `.git` the way git
/// does, without running git.
struct Repository {
    gitdir: PathBuf,
    common: PathBuf,
}

impl Repository {
    fn discover(cwd: &Path) -> std::io::Result<Option<Self>> {
        for dir in cwd.ancestors() {
            let dot_git = dir.join(".git");
            let gitdir = if dot_git.is_dir() {
                dot_git
            } else if let Some(link) = read_regular(&dot_git) {
                let Some(target) = link.strip_prefix("gitdir: ") else {
                    return Ok(None);
                };
                dir.join(target.trim_end())
            } else if std::fs::symlink_metadata(&dot_git).is_ok() {
                // A `.git` that is neither: a FIFO, a device, a dangling link.
                return Ok(None);
            } else {
                continue;
            };
            let gitdir = std::fs::canonicalize(gitdir)?;
            let commondir = gitdir.join("commondir");
            let common = match read_regular(&commondir) {
                Some(common) => std::fs::canonicalize(gitdir.join(common.trim_end()))?,
                None if std::fs::symlink_metadata(&commondir).is_err() => gitdir.clone(),
                None => return Ok(None),
            };
            return Ok(Some(Self { gitdir, common }));
        }
        Ok(None)
    }

    /// The `HEAD` a granted git reads: a symbolic ref or an object name.
    fn head(&self) -> Option<String> {
        let head = read_regular(&self.gitdir.join("HEAD"))?;
        let head = head.trim_end();
        let valid = match head.strip_prefix("ref: ") {
            Some(target) => {
                target.starts_with("refs/")
                    && target.chars().all(|c| c.is_ascii_graphic() && c != '\\')
            }
            None => matches!(head.len(), 40 | 64) && head.chars().all(|c| c.is_ascii_hexdigit()),
        };
        valid.then(|| format!("{head}\n"))
    }
}

/// The copied part of a repository's config, keyed by section and
/// subsection, in file order.
type Projection = BTreeMap<(String, Option<String>), Vec<(String, String)>>;

/// Read a private copy of `config` with `git config --file`, which reads that
/// one file and no includes, and keep only what a granted fetch or push needs. `None` when
/// the repository uses an extension the per-spawn directory cannot honor.
/// The flag says whether every remote the config names was copied.
fn project_repository_config(
    program: &Path,
    scratch: &Path,
    config: &Path,
) -> std::io::Result<Option<(Projection, bool)>> {
    // The host reads the file once, as a bounded regular file, and git parses
    // that private copy: a config swapped for a FIFO or a link afterwards
    // cannot hang the host or point it elsewhere.
    let Some(contents) = read_regular(config) else {
        return Ok(None);
    };
    let copy = scratch.join("repository-config");
    std::fs::write(&copy, contents)?;
    let output = std::process::Command::new(program)
        .args(["config", "--file"])
        .arg(&copy)
        .args(["--null", "--list"])
        .env_clear()
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", scratch.join(GLOBAL_CONFIG))
        .env("GIT_CEILING_DIRECTORIES", scratch)
        .env("HOME", scratch)
        .current_dir(scratch)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()?;
    std::fs::remove_file(&copy)?;
    if !output.status.success() {
        return Err(std::io::Error::other(
            "the repository config could not be read",
        ));
    }
    let entries: Vec<(String, String)> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let entry = std::str::from_utf8(entry).ok()?;
            let (key, value) = entry.split_once('\n').unwrap_or((entry, "true"));
            Some((key.to_string(), value.to_string()))
        })
        .collect();
    let mut remote_urls: BTreeMap<String, Vec<&str>> = BTreeMap::new();
    for (key, value) in &entries {
        if let Some(remote) = key
            .strip_prefix("remote.")
            .and_then(|key| key.strip_suffix(".url"))
        {
            remote_urls
                .entry(remote.to_string())
                .or_default()
                .push(value);
        }
    }
    let mut projection = Projection::new();
    for (key, value) in &entries {
        let Some((section, rest)) = key.split_once('.') else {
            continue;
        };
        let (subsection, name) = match rest.rsplit_once('.') {
            Some((subsection, name)) => (Some(subsection), name),
            None => (None, rest),
        };
        let keep = match (section, subsection, name) {
            ("extensions", None, "refstorage" | "partialclone") => return Ok(None),
            ("core", None, "repositoryformatversion") => matches!(value.as_str(), "0" | "1"),
            ("extensions", None, "objectformat") => matches!(value.as_str(), "sha1" | "sha256"),
            ("remote", Some(remote), "url" | "fetch" | "push" | "prune" | "mirror") => {
                remote_urls
                    .get(remote)
                    .is_some_and(|urls| urls.iter().all(|url| granted_url_path(url).is_some()))
                    && !entries.iter().any(|(key, refspec)| {
                        key == &format!("remote.{remote}.fetch")
                            && !stores_into_tracking(refspec, Some(remote))
                    })
            }
            ("branch", Some(_), "remote" | "merge" | "pushremote") => true,
            ("remote", None, "pushdefault")
            | ("push", None, "default" | "followtags")
            | ("fetch", None, "prune") => true,
            _ => false,
        };
        if keep && !value.chars().any(char::is_control) {
            projection
                .entry((section.to_string(), subsection.map(str::to_string)))
                .or_default()
                .push((name.to_string(), value.clone()));
        }
    }
    let every_remote_copied = remote_urls
        .keys()
        .all(|remote| configured_remotes(&projection).contains(remote));
    Ok(Some((projection, every_remote_copied)))
}

/// Whether a fetch refspec stores only into `remote`'s tracking namespace
/// (`refs/remotes/REMOTE/`), or stores nothing (`FETCH_HEAD` only), as a
/// fetch from a URL must.
fn stores_into_tracking(refspec: &str, remote: Option<&str>) -> bool {
    let refspec = refspec.strip_prefix('+').unwrap_or(refspec);
    if refspec.starts_with('^') {
        return true;
    }
    match refspec.split_once(':') {
        None => true,
        Some((_, "")) => true,
        Some((_, destination)) => remote.is_some_and(|remote| {
            destination
                .strip_prefix("refs/remotes/")
                .and_then(|rest| rest.strip_prefix(remote))
                .is_some_and(|rest| rest.starts_with('/') && !rest.contains(".."))
        }),
    }
}

fn projected(
    projection: &Projection,
    section: &str,
    subsection: Option<&str>,
    name: &str,
) -> Option<String> {
    projection
        .get(&(section.to_string(), subsection.map(str::to_string)))?
        .iter()
        .rev()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.clone())
}

fn remote_urls(projection: &Projection, remote: &str) -> Vec<String> {
    projection
        .get(&("remote".to_string(), Some(remote.to_string())))
        .into_iter()
        .flatten()
        .filter(|(key, _)| key == "url")
        .map(|(_, url)| url.clone())
        .collect()
}

/// The repository URLs this invocation would send the login to and the
/// copied remotes it reaches by name, or `None` when any repository it
/// reaches is not a copied remote or a granted URL, or a refspec would store
/// outside a tracking namespace: it then runs without the login and with the
/// user's own remote config.
fn reached(
    invocation: &Invocation<'_>,
    projection: &Projection,
    head: Option<&str>,
) -> Option<(Vec<String>, Vec<String>)> {
    let branch = head
        .and_then(|head| head.trim_end().strip_prefix("ref: refs/heads/"))
        .map(str::to_string);
    let branch_key = |name: &str| {
        branch
            .as_deref()
            .and_then(|branch| projected(projection, "branch", Some(branch), name))
    };
    let names: Vec<String> = if invocation.all {
        let configured: Vec<String> = configured_remotes(projection);
        if configured.is_empty() {
            return None;
        }
        configured
    } else if invocation.repositories.is_empty() {
        let default = match invocation.command {
            Command::Push => branch_key("pushremote")
                .or_else(|| projected(projection, "remote", None, "pushdefault"))
                .or_else(|| branch_key("remote")),
            Command::Fetch | Command::LsRemote => branch_key("remote"),
        };
        vec![default.unwrap_or_else(|| "origin".to_string())]
    } else {
        invocation
            .repositories
            .iter()
            .map(|name| name.to_string())
            .collect()
    };
    let mut urls = Vec::new();
    let mut remotes = Vec::new();
    for name in names {
        if granted_url_path(&name).is_some() {
            urls.push(name);
            continue;
        }
        let remote = remote_urls(projection, &name);
        if remote.is_empty() {
            return None;
        }
        urls.extend(remote);
        remotes.push(name);
    }
    let target = match (&invocation.repositories[..], remotes.as_slice()) {
        ([_], [remote]) | ([], [remote]) => Some(remote.as_str()),
        _ => None,
    };
    if !invocation
        .refspecs
        .iter()
        .all(|refspec| stores_into_tracking(refspec, target))
    {
        return None;
    }
    urls.sort();
    urls.dedup();
    Some((remotes, urls))
}

/// Every remote that was copied with its URLs.
fn configured_remotes(projection: &Projection) -> Vec<String> {
    projection
        .iter()
        .filter(|((section, subsection), entries)| {
            section == "remote"
                && subsection.is_some()
                && entries.iter().any(|(key, _)| key == "url")
        })
        .filter_map(|((_, subsection), _)| subsection.clone())
        .collect()
}

/// Prepare the per-spawn git directory for one granted network command, or
/// run the command without the login (`None`).
fn seed_network_spawn(request: &SeedRequest<'_>) -> std::io::Result<Option<Seeded>> {
    let Some(invocation) = parse(request.args) else {
        return Ok(None);
    };
    if request.token.chars().any(char::is_control) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "the granted token contains a control character",
        ));
    }
    let dir = request.dir;
    super::grant_spawn::write_private_file(&dir.join(GLOBAL_CONFIG), "")?;
    let repository = Repository::discover(request.cwd)?;
    // Linux is not granted until a same-user process's access to the granted
    // git's memory is measured; Windows has no confinement for it.
    if !cfg!(target_os = "macos") {
        return Ok(None);
    }
    let (projection, head, remotes, urls) = match &repository {
        Some(repository) => {
            let Some((projection, every_remote_copied)) =
                project_repository_config(request.program, dir, &repository.common.join("config"))?
            else {
                return Ok(None);
            };
            if invocation.all && !every_remote_copied {
                return Ok(None);
            }
            let Some(head) = repository.head() else {
                return Ok(None);
            };
            let Some((remotes, urls)) = reached(&invocation, &projection, Some(&head)) else {
                return Ok(None);
            };
            (projection, head, remotes, urls)
        }
        // Outside a repository only `ls-remote URL` has anything to reach.
        None if invocation.command == Command::LsRemote => {
            let urls: Vec<String> = invocation
                .repositories
                .iter()
                .map(|url| url.to_string())
                .collect();
            if urls.len() != 1 || granted_url_path(&urls[0]).is_none() {
                return Ok(None);
            }
            (
                Projection::new(),
                "ref: refs/heads/main\n".to_string(),
                Vec::new(),
                urls,
            )
        }
        None => return Ok(None),
    };
    match &repository {
        Some(repository) => link_repository(dir, repository, &remotes)?,
        None => {
            std::fs::create_dir(dir.join("objects"))?;
            std::fs::create_dir(dir.join("refs"))?;
        }
    }
    super::grant_spawn::write_private_file(&dir.join("HEAD"), &head)?;
    super::grant_spawn::write_private_file(&dir.join("config"), &render_config(dir, &projection))?;
    let socket = serve_login(dir, &urls, request.token)?;
    Ok(Some(Seeded {
        env: vec![
            ("GIT_DIR".to_string(), dir.display().to_string()),
            (
                "GIT_CONFIG_GLOBAL".to_string(),
                dir.join(GLOBAL_CONFIG).display().to_string(),
            ),
        ],
        hold: Some(socket),
    }))
}

/// Serve the login for the reached repository URLs on the per-spawn
/// credential socket until the returned value is dropped.
#[cfg(unix)]
fn serve_login(
    dir: &Path,
    urls: &[String],
    token: &str,
) -> std::io::Result<Box<dyn std::any::Any + Send>> {
    use super::git_credential_socket::{CredentialSocket, Login};
    let origin = granted_origin();
    let (protocol, host) = origin
        .split_once("://")
        .expect("the granted origin names its protocol");
    let paths = urls
        .iter()
        .map(|url| {
            granted_url_path(url)
                .expect("reached URLs are granted")
                .to_string()
        })
        .collect();
    Ok(Box::new(CredentialSocket::serve(
        dir,
        Login {
            protocol: protocol.to_string(),
            host: host.to_string(),
            paths,
            username: LOGIN_USER.to_string(),
            password: token.to_string(),
        },
    )?))
}

#[cfg(not(unix))]
fn serve_login(
    _dir: &Path,
    _urls: &[String],
    _token: &str,
) -> std::io::Result<Box<dyn std::any::Any + Send>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "a granted git needs a Unix socket for its login",
    ))
}

/// Link the per-spawn git directory's objects, `FETCH_HEAD`, and each reached
/// remote's tracking refs into the repository, and copy in the branch and tag
/// refs, `packed-refs` and `shallow`. A remote's tracking directory is created
/// when missing, and refused when it, a directory above it within `refs`, or
/// anything under it is a link.
#[cfg(unix)]
fn link_repository(dir: &Path, repository: &Repository, remotes: &[String]) -> std::io::Result<()> {
    let objects = repository.common.join("objects");
    // A linked entry is written through, so it must be the repository's own:
    // `objects` a real directory, and `FETCH_HEAD` a regular file or absent.
    refuse_link(&objects)?;
    if !objects.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the repository has no objects directory",
        ));
    }
    let entries = COMMON_LINKS
        .iter()
        .map(|name| (name, &repository.common))
        .chain(WORKTREE_LINKS.iter().map(|name| (name, &repository.gitdir)));
    for (name, base) in entries {
        let target = base.join(name);
        if WORKTREE_LINKS.contains(name) {
            refuse_unless_regular(&target)?;
        }
        std::os::unix::fs::symlink(target, dir.join(name))?;
    }
    let refs = repository.common.join("refs");
    std::fs::create_dir_all(dir.join("refs/remotes"))?;
    for remote in remotes {
        // Every directory from `refs` down, and everything under the remote's
        // tracking refs, must be a real directory or file: a planted link
        // would send the fetch's tracking-ref writes elsewhere.
        let mut tracking = refs.clone();
        for part in std::iter::once("remotes").chain(remote.split('/')) {
            refuse_link(&tracking)?;
            tracking.push(part);
        }
        std::fs::create_dir_all(&tracking)?;
        refuse_link(&tracking)?;
        refuse_links_under(&tracking)?;
        let tracking = std::fs::canonicalize(&tracking)?;
        let link = dir.join("refs/remotes").join(remote);
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(tracking, link)?;
    }
    for namespace in COPIED_REFS {
        copy_refs(&refs.join(namespace), &dir.join("refs").join(namespace))?;
    }
    for name in COPIED_FILES {
        let from = repository.common.join(name);
        if std::fs::symlink_metadata(&from).is_err() {
            continue;
        }
        let Some(text) = read_regular(&from) else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("the repository's `{name}` is not a regular file"),
            ));
        };
        super::grant_spawn::write_private_file(&dir.join(name), &text)?;
    }
    Ok(())
}

/// Refuse a path that is a link (or anything but a directory).
#[cfg(unix)]
fn refuse_link(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "a repository ref directory is a link or not a directory",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Refuse a path that exists as anything but a regular file: a link, a FIFO,
/// a device or a directory.
#[cfg(unix)]
fn refuse_unless_regular(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(()),
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "the repository's `{}` is not a regular file",
                path.display()
            ),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Refuse a directory tree that holds a link or anything but directories and
/// regular files.
#[cfg(unix)]
fn refuse_links_under(path: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            refuse_links_under(&entry.path())?;
        } else if !kind.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "a remote's tracking refs hold a link",
            ));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn link_repository(
    _dir: &Path,
    _repository: &Repository,
    _remotes: &[String],
) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "a granted git needs symbolic links",
    ))
}

/// A regular file's text, never through a link and never a large one.
#[cfg(unix)]
fn read_regular(path: &Path) -> Option<String> {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    // Opened without following a link and without blocking on a FIFO; the
    // opened file itself must be a regular file.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(64 << 20).read_to_string(&mut text).ok()?;
    Some(text)
}

#[cfg(not(unix))]
fn read_regular(path: &Path) -> Option<String> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    (metadata.is_file() && metadata.len() <= 64 << 20)
        .then(|| std::fs::read_to_string(path).ok())
        .flatten()
}

/// Copy the loose refs under `from` that hold an object name or a symbolic
/// ref, skipping links and anything else.
#[cfg(unix)]
fn copy_refs(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    let Ok(entries) = std::fs::read_dir(from) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let (path, name) = (entry.path(), entry.file_name());
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            copy_refs(&path, &to.join(&name))?;
        } else if let Some(value) = read_regular(&path).filter(|value| {
            let value = value.trim_end();
            value.starts_with("ref: refs/")
                || (matches!(value.len(), 40 | 64) && value.chars().all(|c| c.is_ascii_hexdigit()))
        }) {
            super::grant_spawn::write_private_file(&to.join(&name), &value)?;
        }
    }
    Ok(())
}

/// One config section Harn writes: name, subsection, and keys.
type ConfigSection<'a> = (&'a str, Option<&'a str>, Vec<(&'a str, String)>);

/// The per-spawn config: the copied repository keys, then the settings that
/// keep a granted git to the served login over https and running nothing.
fn render_config(dir: &Path, projection: &Projection) -> String {
    let http = granted_origin().starts_with("http://");
    let helper = format!("cache --socket={}", dir.join(CREDENTIAL_SOCKET).display());
    let fixed: Vec<ConfigSection<'_>> = vec![
        (
            "core",
            None,
            vec![
                ("bare", "false".into()),
                ("fsmonitor", "false".into()),
                ("hooksPath", dir.join("hooks").display().to_string()),
                ("askPass", String::new()),
                // The private directory is read-only to a confined git, and a
                // tracking ref's reflog would be written there.
                ("logAllRefUpdates", "false".into()),
            ],
        ),
        ("protocol", None, vec![("allow", "never".into())]),
        ("protocol", Some("https"), vec![("allow", "always".into())]),
        (
            "protocol",
            Some("http"),
            vec![("allow", if http { "always" } else { "never" }.into())],
        ),
        ("http", None, vec![("sslVerify", "true".into())]),
        (
            "credential",
            None,
            vec![("helper", helper), ("useHttpPath", "true".into())],
        ),
        (
            "fetch",
            None,
            vec![
                ("recurseSubmodules", "false".into()),
                ("writeCommitGraph", "false".into()),
            ],
        ),
        ("submodule", None, vec![("recurse", "false".into())]),
        ("gc", None, vec![("auto", "0".into())]),
        ("maintenance", None, vec![("auto", "false".into())]),
    ];
    let mut out = String::new();
    let section_header =
        |out: &mut String, section: &str, subsection: Option<&str>| match subsection {
            Some(subsection) => {
                let _ = writeln!(out, "[{section} \"{}\"]", escape(subsection));
            }
            None => {
                let _ = writeln!(out, "[{section}]");
            }
        };
    for ((section, subsection), entries) in projection {
        section_header(&mut out, section, subsection.as_deref());
        for (name, value) in entries {
            let _ = writeln!(out, "\t{name} = \"{}\"", escape(value));
        }
        if section == "remote" && subsection.is_some() {
            let _ = writeln!(out, "\ttagOpt = \"--no-tags\"");
        }
    }
    for (section, subsection, entries) in fixed {
        section_header(&mut out, section, subsection);
        for (name, value) in entries {
            let _ = writeln!(out, "\t{name} = \"{}\"", escape(&value));
        }
    }
    out
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(all(test, target_os = "macos"))]
#[path = "git_network_tests.rs"]
mod tests;
