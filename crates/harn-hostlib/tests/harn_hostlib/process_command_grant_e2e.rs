//! A `for_command: "gh"` grant reaches the one `gh` process it names, through
//! the real `run_command` spawn path, and nothing that process or a shell
//! could start with its environment.
//!
//! The `gh` here is a stand-in script that reports whether the granted
//! variable arrived, and that reproduces the GitHub CLI's own exec paths: a
//! `!` alias from `$GH_CONFIG_DIR/config.yml` runs through `sh -c`, an
//! extension under `$XDG_DATA_HOME/gh/extensions` is executed, output goes
//! through `$GH_PAGER`, `pr checkout` and (to stand in for a read that touches
//! the work tree) `pr diff` run git, and `auth token` prints the
//! credential. The granted value is a probe held by a host secret provider,
//! never a token, and every call goes through the blocking-pool dispatch a
//! real tool call uses, so the provider must follow it there. Each leak
//! vector runs `env`; a leak shows as `GH_TOKEN=` in its output, and every
//! negative also checks that the read measured something.
//!
//! The user's files (home, the trusted `gh`, the Harn home) live outside the
//! host temp dir, which a granted spawn never trusts; the workspace is a temp
//! dir.

#![cfg(unix)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use harn_vm::security::{EnvironmentPolicyKind, GrantSourceSpec, GrantSpec, SessionEnvironment};
use harn_vm::value::VmDictExt as _;
use harn_vm::VmValue;

use super::support::*;

const GRANTED: &str = "GH_TOKEN";
const PROBE: &str = "harn-probe-gh-grant";
const PROBE_BASE64: &str = "aGFybi1wcm9iZS1naC1ncmFudA";
const SECRET_ACCOUNT: &str = "gh-grant-e2e";
const SECRET_KEY: &str = "token";
/// A second value, never granted to gh, so the process-wide redaction set
/// holds it only if the unprofiled grant registered it.
const OTHER_KEY: &str = "other";
const OTHER_PROBE: &str = "harn-probe-unprofiled-grant";
const PROBE_BASIC: &str = "eC1hY2Nlc3MtdG9rZW46aGFybi1wcm9iZS1naC1ncmFudA";

const FAKE_GH: &str = r#"#!/bin/sh
token=$(sed -n "s/^    oauth_token: '\(.*\)'\$/\1/p" "${GH_CONFIG_DIR:-/nonexistent}/hosts.yml" 2>/dev/null | head -n 1)
if [ -n "$token" ]; then echo "gh-grant=present"; else echo "gh-grant=absent"; fi
echo "env-token=[${GH_TOKEN:+set}${GITHUB_TOKEN:+set}] host=[$GH_HOST]"
echo "pager=[$GH_PAGER] editor=[$GH_EDITOR]"
echo "config=[$GH_CONFIG_DIR]"
config="${GH_CONFIG_DIR:-$HOME/.config/gh}/config.yml"
if [ -f "$config" ]; then sed 's/^/cfg: /' "$config"; fi
case "$1 $2" in
  "auth token") echo "$token"; exit 0 ;;
  "pr checkout") exec git checkout -q -B "$3" ;;
  "pr diff") git checkout -q -B "probe-$3" ;;
  "issue status") env; exit 0 ;;
  "issue view") echo "$GH_CONFIG_DIR" > gh.config; echo $$ > gh.pid; sleep 4; exit 0 ;;
  "pr status")
    other=$(cat gh.config)
    echo "own=$(grep -c oauth_token "$GH_CONFIG_DIR/hosts.yml" 2>/dev/null)"
    echo "other=$(cat "$other/hosts.yml" 2>/dev/null | grep -c oauth_token)"
    exit 0 ;;
  "api leak")
    printf 'token=%s' "$(printf '%s' "$token" | cut -c1-9)"; sleep 0.2
    printf '%s\n' "$(printf '%s' "$token" | cut -c10-)"
    printf 'b64=%s\n' "$(printf '%s' "$token" | base64)"
    printf 'basic=%s\n' "$(printf 'x-access-token:%s' "$token" | base64)"
    printf 'err=%s\n' "$token" >&2
    exit 0 ;;
esac
if [ -n "$1" ] && [ -f "$config" ]; then
  value=$(sed -n "s/^[[:space:]]*$1:[[:space:]]*//p" "$config" | head -n 1 | sed "s/^'//; s/'\$//")
  case "$value" in '!'*) exec sh -c "${value#!}" ;; esac
fi
extension="${XDG_DATA_HOME:-$HOME/.local/share}/gh/extensions/gh-$1/gh-$1"
if [ -n "$1" ] && [ -x "$extension" ]; then exec "$extension"; fi
if [ -n "$GH_PAGER" ]; then echo paged | $GH_PAGER; fi
"#;

const PLANTED_GH: &str = "#!/bin/sh\necho planted-gh\nenv\n";

fn write_executable(path: &Path, body: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A scratch directory a granted spawn can trust, so neither the host temp dir
/// nor the current directory. The build's target dir qualifies, but an archived
/// test binary can run on a host where that path does not exist and is itself
/// extracted under the temp dir, so such a run uses the real home.
fn trusted_scratch_dir() -> tempfile::TempDir {
    let built = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let parent = if built.is_dir() {
        built.to_path_buf()
    } else {
        PathBuf::from(std::env::var_os("HOME").expect("HOME must name the real home"))
    };
    tempfile::Builder::new()
        .prefix("harn-grant-e2e-")
        .tempdir_in(parent)
        .expect("trusted scratch dir")
}

/// A grant reaches only a compiled `gh`, never a script. This one is a
/// compiled launcher that runs the `gh.sh` beside it, so the stand-in stays a
/// readable script.
const NATIVE_LAUNCHER: &str = r#"use std::os::unix::process::CommandExt;
fn main() {
    let script = std::env::current_exe().unwrap().with_file_name("gh.sh");
    let error = std::process::Command::new(script).args(std::env::args_os().skip(1)).exec();
    eprintln!("{error}");
    std::process::exit(127);
}
"#;

fn native_launcher() -> &'static [u8] {
    static LAUNCHER: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
    LAUNCHER.get_or_init(|| {
        let dir = trusted_scratch_dir();
        let source = dir.path().join("launcher.rs");
        let binary = dir.path().join("launcher");
        std::fs::write(&source, NATIVE_LAUNCHER).unwrap();
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let status = std::process::Command::new(rustc)
            .args(["--edition", "2021", "-o"])
            .arg(&binary)
            .arg(&source)
            .status()
            .expect("rustc must run to build the native gh launcher");
        assert!(status.success(), "building the native gh launcher failed");
        std::fs::read(&binary).unwrap()
    })
}

fn write_native_gh(bin: &Path, script: &str) {
    write_executable(&bin.join("gh.sh"), script);
    write_executable(&bin.join("gh"), native_launcher());
}

/// The user's gh setup the grant must not open (a `!` alias and an
/// extension, both running `env`), a workspace that plants its own `gh`, and
/// a granted session.
struct Fixture {
    user: tempfile::TempDir,
    workspace: tempfile::TempDir,
    previous: Vec<(&'static str, Option<OsString>)>,
}

struct Paths {
    /// Prepended to the session `PATH`, before the trusted `gh`.
    planted_path: Option<String>,
    harn_home: Option<PathBuf>,
}

impl Fixture {
    fn new() -> Self {
        Self::with(|_| Paths {
            planted_path: None,
            harn_home: None,
        })
    }

    fn with(paths: impl FnOnce(&Path) -> Paths) -> Self {
        let user = trusted_scratch_dir();
        // Not `$TMPDIR`: other suites in this binary repoint it under their
        // own lock and remove the directory afterwards.
        let workspace = tempfile::tempdir_in("/tmp").expect("workspace root");
        let home = user.path().join("home");
        write_native_gh(&user.path().join("bin"), FAKE_GH);
        std::fs::create_dir_all(home.join(".config/gh")).unwrap();
        std::fs::write(
            home.join(".config/gh/config.yml"),
            "git_protocol: https\npager: env\naliases:\n    co: pr checkout\n    leak: '!env'\n",
        )
        .unwrap();
        write_executable(
            &user.path().join("user-data/gh/extensions/gh-ext/gh-ext"),
            "#!/bin/sh\nenv\n",
        );
        write_executable(&workspace.path().join("bin/gh"), PLANTED_GH);
        let paths = paths(workspace.path());
        let search_path = [
            paths.planted_path.unwrap_or_default(),
            user.path().join("bin").display().to_string(),
            std::env::var("PATH").unwrap_or_default(),
        ]
        .into_iter()
        .filter(|entry| !entry.is_empty())
        .collect::<Vec<_>>()
        .join(":");
        let set: [(&'static str, PathBuf); 6] = [
            ("PATH", PathBuf::from(search_path)),
            // The test lane's own config home sits under the host temp dir,
            // which a grant correctly refuses to trust.
            ("XDG_CONFIG_HOME", home.join(".config")),
            ("HOME", home),
            ("XDG_DATA_HOME", user.path().join("user-data")),
            (
                "HARN_HOME",
                paths
                    .harn_home
                    .unwrap_or_else(|| user.path().join("harn-home")),
            ),
            ("HARN_CACHE_DIR", user.path().join("harn-cache")),
        ];
        let previous = set
            .iter()
            .map(|(name, _)| (*name, std::env::var_os(name)))
            .collect();
        // SAFETY: every caller holds the shared environment lock, and the
        // previous values are restored on drop.
        for (name, value) in set {
            unsafe { std::env::set_var(name, value) };
        }
        let grant = GrantSpec {
            name: "gh_token".to_string(),
            source: GrantSourceSpec::SecretStore {
                account: SECRET_ACCOUNT.to_string(),
                key: SECRET_KEY.to_string(),
            },
            expose_as_env: Some(GRANTED.to_string()),
            for_command: Some("gh".to_string()),
            expose_to: Default::default(),
        };
        let environment =
            SessionEnvironment::launch(EnvironmentPolicyKind::Granted, vec![grant], &|name| {
                std::env::var(name).ok()
            })
            .expect("the granted policy must launch");
        harn_vm::stdlib::process::set_session_environment(Some(environment));
        Self {
            user,
            workspace,
            previous,
        }
    }

    fn workspace(&self, relative: &str) -> PathBuf {
        self.workspace.path().join(relative)
    }

    fn request(&self, cwd: &Path) -> harn_vm::value::DictMap {
        let mut request = harn_vm::value::DictMap::new();
        request.insert("cwd".into(), vm_string(&cwd.display().to_string()));
        request
    }

    fn shell_in(&self, cwd: &Path, command: &str) -> Result<Output, String> {
        let mut request = self.request(cwd);
        request.insert("mode".into(), vm_string("shell"));
        request.insert("command".into(), vm_string(command));
        let mut shell = harn_vm::value::DictMap::new();
        shell.insert("path".into(), vm_string("/bin/sh"));
        request.put("shell", VmValue::dict(shell));
        run(request)
    }

    fn shell(&self, command: &str) -> String {
        self.shell_in(self.workspace.path(), command)
            .unwrap()
            .stdout
    }

    fn argv(&self, argv: &[&str], env: &[(&str, String)]) -> String {
        let mut request = self.request(self.workspace.path());
        request.put(
            "argv",
            VmValue::list(argv.iter().map(|word| vm_string(word)).collect()),
        );
        if !env.is_empty() {
            let mut overlay = harn_vm::value::DictMap::new();
            for (key, value) in env {
                overlay.insert((*key).into(), vm_string(value));
            }
            request.put("env", VmValue::dict(overlay));
            // A supplied `env` otherwise defaults to `replace`, which drops
            // every grant; `patch` layers it over the session environment.
            request.insert("env_mode".into(), vm_string("patch"));
        }
        run(request).unwrap().stdout
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        harn_vm::stdlib::process::set_session_environment(None);
        for (name, previous) in &self.previous {
            unsafe {
                match previous {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

struct Output {
    stdout: String,
    stderr: String,
}

/// Dispatch `run_command` the way a VM tool call does: on Tokio's blocking
/// pool under the caller's ambient scope, inside a host secret provider scope.
fn run(request: harn_vm::value::DictMap) -> Result<Output, String> {
    use harn_vm::secrets::{MemorySecretProvider, SecretId, SecretProvider};
    let provider: std::sync::Arc<dyn SecretProvider> = std::sync::Arc::new(
        MemorySecretProvider::new(SECRET_ACCOUNT)
            .with_secret(SecretId::new(SECRET_ACCOUNT, SECRET_KEY), PROBE)
            .with_secret(SecretId::new(SECRET_ACCOUNT, OTHER_KEY), OTHER_PROBE),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let response = runtime
        .block_on(harn_vm::secrets::with_active_secret_provider(
            Some(provider),
            async move {
                harn_vm::orchestration::run_blocking_with_ambient(move || {
                    call_tool("hostlib_tools_run_command", request)
                })
                .await
            },
        ))
        .expect("the blocking dispatch must complete")
        .map_err(|error| error.to_string())?;
    let VmValue::Dict(dict) = response else {
        panic!("run_command must answer with a dict, got {response:?}");
    };
    let field = |name: &str| match dict.get(name) {
        Some(VmValue::String(text)) => text.to_string(),
        other => panic!("run_command carried no {name}: {other:?}"),
    };
    Ok(Output {
        stdout: field("stdout"),
        stderr: field("stderr"),
    })
}

/// `env` ran (the read measured something) and the grant was not in it.
fn assert_env_ran_without_grant(stdout: &str, vector: &str) {
    assert!(
        stdout.lines().any(|line| line.starts_with("PATH=")),
        "{vector}: env never ran, so nothing was measured; stdout:\n{stdout}"
    );
    assert!(
        !stdout.contains(&format!("{GRANTED}=")),
        "{vector}: the granted variable reached a process the grant never named"
    );
}

fn assert_granted_without_leak(out: &str, vector: &str) {
    assert!(
        out.contains("gh-grant=present"),
        "{vector}: the trusted gh must be granted; stdout:\n{out}"
    );
    assert!(
        out.contains("env-token=[] host=[github.com]"),
        "{vector}: the token must reach gh only as a github.com login; stdout:\n{out}"
    );
    assert!(
        !out.contains(&format!("{GRANTED}=")) && !out.contains("planted-gh"),
        "{vector}: the grant reached a program it never named; stdout:\n{out}"
    );
}

#[test]
fn a_plain_gh_shell_command_receives_the_grant_with_its_exec_paths_closed() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();

    let out = fixture.shell("gh issue list --limit 5");
    assert_granted_without_leak(&out, "shell");
    assert!(
        out.contains("pager=[] editor=[/usr/bin/false]"),
        "a granted gh must carry an empty pager and a refusing editor; stdout:\n{out}"
    );
    // The stand-in is private to this spawn, kept the plain alias, and
    // dropped the shell alias and the pager.
    let config_dir = out
        .lines()
        .find_map(|line| line.strip_prefix("config=[")?.strip_suffix(']'))
        .unwrap_or_default()
        .to_string();
    assert!(
        Path::new(&config_dir).starts_with(fixture.user.path().join("harn-home/grant-spawns")),
        "{config_dir}"
    );
    assert!(
        !Path::new(&config_dir).exists(),
        "the per-spawn directory outlived the spawn"
    );
    assert!(out.contains("cfg:     co: pr checkout"), "stdout:\n{out}");
    assert!(
        !out.contains("!env") && !out.contains("pager: env"),
        "stdout:\n{out}"
    );
    // The direction control: argv mode reaches the same grant.
    assert_granted_without_leak(&fixture.argv(&["gh", "issue", "list"], &[]), "argv");
}

#[test]
fn compound_shell_commands_keep_the_shell_and_receive_no_grant() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();

    assert_env_ran_without_grant(&fixture.shell("env"), "env");
    assert_eq!(
        fixture.shell("sh -c 'echo \"token=$GH_TOKEN\"'").trim(),
        "token=",
        "a nested shell must not see the grant"
    );
    let out = fixture.shell("gh issue list; env");
    assert!(out.contains("gh-grant=absent"), "stdout:\n{out}");
    assert_env_ran_without_grant(&out, "gh issue list; env");
    let out = fixture.shell("GH_PAGER=env gh issue list");
    assert!(out.contains("gh-grant=absent"), "stdout:\n{out}");
    assert_env_ran_without_grant(&out, "GH_PAGER=env gh issue list");
}

#[test]
fn an_alias_an_extension_or_a_caller_pager_never_runs_with_the_grant() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();

    for (vector, command) in [("`!` alias", "gh leak"), ("extension", "gh ext")] {
        let out = fixture.shell(command);
        assert!(out.contains("gh-grant=absent"), "{vector}: {out}");
        assert!(!out.contains(&format!("{GRANTED}=")), "{vector}: {out}");
    }
    // A caller variable that would reopen an exec path withholds the grant;
    // the pager it names does run, and sees nothing.
    let out = fixture.argv(&["gh", "issue", "list"], &[("GH_PAGER", "env".to_string())]);
    assert!(out.contains("gh-grant=absent"), "stdout:\n{out}");
    assert_env_ran_without_grant(&out, "caller pager");
}

#[test]
fn a_planted_stand_in_link_cannot_redirect_a_granted_gh() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();
    // What a confined child could do to the shared cache stand-in an earlier
    // design used: link it to a workspace directory holding a `!` alias.
    let attacker = fixture.workspace(".x");
    std::fs::create_dir_all(&attacker).unwrap();
    std::fs::write(
        attacker.join("config.yml"),
        "aliases:\n    triage: '!env'\n",
    )
    .unwrap();
    let stand_in = fixture.user.path().join("harn-cache/gh-grant");
    std::fs::create_dir_all(&stand_in).unwrap();
    std::os::unix::fs::symlink(&attacker, stand_in.join("config")).unwrap();

    let out = fixture.shell("gh triage");
    assert!(out.contains("gh-grant="), "gh never ran: {out}");
    assert!(
        !out.contains(&format!("{GRANTED}=")),
        "the planted alias saw the grant: {out}"
    );
    // A granted read is not redirected through the link either.
    let out = fixture.shell("gh issue list");
    assert_granted_without_leak(&out, "planted stand-in, read");
    assert!(!out.contains(&stand_in.display().to_string()), "{out}");
    // A racer that fills the private extension directory between its creation
    // and gh's lookup cannot: it is created exclusively, owner-only, and
    // outside every root a confined child can write.
}

#[test]
fn a_private_directory_inside_a_writable_root_refuses_the_grant() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::with(|workspace| Paths {
        planted_path: None,
        harn_home: Some(workspace.join(".harn")),
    });
    let error = fixture
        .shell_in(fixture.workspace.path(), "gh issue list")
        .err()
        .expect("a per-spawn directory inside the workspace must refuse the grant");
    assert!(error.contains("command_grant_refused"), "{error}");
    assert!(!fixture
        .workspace(".harn/grant-spawns")
        .read_dir()
        .is_ok_and(|mut entries| entries.next().is_some()));
}

#[test]
fn gh_auth_token_is_refused_and_printed_credentials_are_redacted() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();

    for command in [
        "gh auth token",
        "gh auth status --show-token",
        "gh auth git-credential get",
    ] {
        let error = fixture
            .shell_in(fixture.workspace.path(), command)
            .err()
            .unwrap_or_else(|| panic!("{command} must be refused"));
        assert!(
            error.contains("command_grant_refused"),
            "{command}: {error}"
        );
        assert!(!error.contains(PROBE), "{command}: {error}");
    }
    assert!(fixture.shell("gh auth status").contains("gh-grant=absent"));

    // A granted program that prints its credential anyway, split across
    // writes and encoded, has every form redacted from both streams.
    let out = fixture
        .shell_in(fixture.workspace.path(), "gh api leak")
        .unwrap();
    assert!(out.stdout.contains("gh-grant=present"), "{}", out.stdout);
    assert!(out.stdout.contains("token=[redacted]"), "{}", out.stdout);
    for form in [PROBE, PROBE_BASE64, PROBE_BASIC] {
        assert!(
            !out.stdout.contains(form) && !out.stderr.contains(form),
            "a granted value survived in output:\n{}\n{}",
            out.stdout,
            out.stderr
        );
    }
    assert!(out.stderr.contains("err=[redacted]"), "{}", out.stderr);
}

#[test]
fn only_the_trusted_gh_file_receives_the_grant() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::with(|workspace| Paths {
        planted_path: Some(format!("bin:{}", workspace.join("bin").display())),
        harn_home: None,
    });

    // A relative and a workspace `PATH` entry both come first; the grant
    // still runs the trusted gh.
    assert_granted_without_leak(&fixture.shell("gh issue list"), "planted PATH");
    let planted = fixture.workspace("bin/gh").display().to_string();
    for argv in [
        vec!["./bin/gh", "issue", "list"],
        vec![planted.as_str(), "issue"],
    ] {
        let out = fixture.argv(&argv, &[]);
        assert!(out.contains("planted-gh"), "{argv:?}: {out}");
        assert_env_ran_without_grant(&out, &format!("{argv:?}"));
    }
}

#[test]
fn git_hooks_of_the_workspace_never_see_the_grant() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();
    let repo = fixture.workspace("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [
        &["init", "-q"][..],
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "x",
        ],
    ] {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }
    write_executable(
        &repo.join(".git/hooks/post-checkout"),
        "#!/bin/sh\nenv > hook-env\n",
    );
    let hook_env = repo.join("hook-env");

    // Control: without the grant the hook fires in this repository.
    fixture.shell_in(&repo, "gh pr diff 1; true").unwrap();
    let control = std::fs::read_to_string(&hook_env).expect("the hook must fire");
    assert!(control.contains("PATH="), "{control}");
    std::fs::remove_file(&hook_env).unwrap();

    // A granted read that touched the work tree would still not run it.
    let out = fixture.shell_in(&repo, "gh pr diff 2").unwrap();
    assert_granted_without_leak(&out.stdout, "pr diff");
    let granted = std::fs::read_to_string(&hook_env).unwrap_or_default();
    assert!(
        !granted.contains(&format!("{GRANTED}=")),
        "a workspace git hook saw the grant"
    );
}

#[test]
fn only_gh_reads_receive_the_grant() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();

    for read in [
        "gh issue list",
        "gh -R o/r pr view 1",
        "gh pr view --repo=o/r 1",
        "gh api repos/o/r",
    ] {
        assert_granted_without_leak(&fixture.shell(read), read);
    }
    // Deny by default: every write, clone, checkout, fork, extension, alias,
    // and unrecognized spelling runs without the credential, whatever
    // repository selector precedes it.
    for other in [
        "gh pr checkout 7",
        "gh -R o/r pr checkout 7",
        "gh --repo=o/r pr checkout 7",
        "gh pr -R o/r checkout 7",
        "gh pr co 7",
        "gh co 7",
        "gh repo clone o/r",
        "gh -R o/r repo fork",
        "gh repo fork o/r --clone",
        "gh gist clone x",
        "gh extension install o/gh-x",
        "gh alias set co 'pr checkout'",
        "gh workflow run ci.yml",
        "gh codespace list",
        "gh ssh-key list",
        "gh secret list",
        "gh api -X POST repos/o/r/issues",
        "gh api repos/o/r/issues -f title=t",
        "gh pr view 1 --web",
    ] {
        let out = fixture.shell(other);
        assert!(
            out.contains("gh-grant=absent"),
            "{other} received the grant: {out}"
        );
    }
}

/// A granted read that names an output filter or passes arguments on after
/// `--` is refused: gh's jq filters read the environment and can print the
/// credential in a form redaction cannot match, and `--` hands the rest to
/// git. The same arguments on a non-read run without the credential.
#[test]
fn output_filters_and_passthrough_are_refused_on_a_granted_read() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();

    let not_refused = [
        "gh api user --jq env.GH_TOKEN",
        "gh api user --jq=env.GH_TOKEN",
        "gh pr list --json number -q env",
        "gh pr list --json number -qenv",
        "gh pr list -Lq 5",
        "gh api user --template x",
        "gh api user --template=x",
        "gh api user -t x",
        "gh api user -tx",
        "gh pr view 1 -- -c filter.a.smudge=env",
    ]
    .into_iter()
    .filter(|command| {
        !fixture
            .shell_in(fixture.workspace.path(), command)
            .is_err_and(|error| error.contains("command_grant_refused"))
    })
    .collect::<Vec<_>>();
    assert!(not_refused.is_empty(), "not refused: {not_refused:#?}");
    for other in [
        "gh repo clone o/r -- -c filter.a.smudge=env",
        "gh pr create -t title",
    ] {
        let out = fixture.shell(other);
        assert!(out.contains("gh-grant=absent"), "{other}: {out}");
    }
    assert_granted_without_leak(&fixture.shell("gh pr list --json number"), "--json");
}

/// Redaction covers every grant's value, not only gh's: a command-bound
/// grant to a program without an exec profile prints nothing it was given.
#[test]
fn a_grant_to_an_unprofiled_program_is_redacted_from_its_output() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();
    let grant = GrantSpec {
        name: "printenv_probe".to_string(),
        source: GrantSourceSpec::SecretStore {
            account: SECRET_ACCOUNT.to_string(),
            key: OTHER_KEY.to_string(),
        },
        expose_as_env: Some("PRINTENV_PROBE".to_string()),
        for_command: Some("printenv".to_string()),
        expose_to: Default::default(),
    };
    let environment =
        SessionEnvironment::launch(EnvironmentPolicyKind::Granted, vec![grant], &|name| {
            std::env::var(name).ok()
        })
        .expect("the granted policy must launch");
    harn_vm::stdlib::process::set_session_environment(Some(environment));
    let out = fixture.argv(&["printenv", "PRINTENV_PROBE"], &[]);
    assert!(out.contains("[redacted]"), "the grant never arrived: {out}");
    assert!(
        !out.contains(OTHER_PROBE),
        "an unprofiled grant was printed: {out}"
    );
}

/// A script `gh` on a trusted `PATH` entry, such as a version-manager shim
/// that picks the real program from files in the working directory, gets no
/// grant.
#[test]
fn a_script_gh_on_a_trusted_path_receives_no_grant() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();
    write_executable(&fixture.user.path().join("bin/gh"), FAKE_GH);
    let out = fixture.shell("gh issue list");
    assert!(out.contains("gh-grant=absent"), "{out}");
}

/// A write root over the user's SSH or git configuration refuses the grant:
/// a confined child could plant a `ProxyCommand` or a filter driver there.
#[test]
fn a_write_root_over_the_users_git_or_ssh_config_refuses_the_grant() {
    use harn_vm::orchestration::{pop_execution_policy, push_execution_policy, CapabilityPolicy};

    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();
    let home = fixture.user.path().join("home");
    for protected in [home.join(".ssh"), home.join(".config/git")] {
        std::fs::create_dir_all(&protected).unwrap();
        let mut policy = CapabilityPolicy::default();
        policy
            .process_sandbox
            .write_roots
            .push(protected.display().to_string());
        push_execution_policy(policy);
        let result = fixture.shell_in(fixture.workspace.path(), "gh issue list");
        pop_execution_policy();
        let error = result
            .err()
            .unwrap_or_else(|| panic!("a write root at {} must refuse", protected.display()));
        assert!(error.contains("command_grant_refused"), "{error}");
    }
    assert_granted_without_leak(&fixture.shell("gh issue list"), "no write root");
}

/// The token is a github.com login sent only over https: a granted read that
/// names any other host, or any URL scheme but https, in any spelling, is
/// refused before gh runs.
#[test]
fn a_granted_read_naming_another_host_is_refused() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();

    let not_refused = [
        "gh api --hostname github.localhost user",
        "gh api --hostname=github.localhost user",
        "gh -R github.localhost/o/r issue list",
        "gh -Rgithub.localhost/o/r issue list",
        "gh issue list --repo=github.localhost/o/r",
        "gh issue list --repo github.localhost/o/r",
        "gh repo view github.localhost/o/r",
        "gh run list -R github.localhost/o/r",
        "gh -R zz-probe-tenant.ghe.com/o/r issue list",
        "gh repo view https://github.localhost/o/r",
        "gh repo view git@github.localhost:o/r",
        "gh api http://api.github.localhost/user",
        "gh api http://api.github.com/user",
        "gh api http://github.com/api/v3/user",
        "gh api HTTP://api.github.com/user",
        "gh api //api.github.com/user",
        "gh repo view --repo=http://github.com/o/r",
    ]
    .into_iter()
    .filter(|command| {
        !fixture
            .shell_in(fixture.workspace.path(), command)
            .is_err_and(|error| error.contains("command_grant_refused"))
    })
    .collect::<Vec<_>>();
    assert!(not_refused.is_empty(), "not refused: {not_refused:#?}");
    for read in [
        "gh -R o/r issue list",
        "gh -R github.com/o/r issue list",
        "gh repo view https://github.com/o/r",
        "gh api --hostname github.com user",
        "gh pr view feature/a/b",
    ] {
        assert_granted_without_leak(&fixture.shell(read), read);
    }
}

/// Run `body` under a confined Worktree policy that can read the user's files.
fn confined<T>(fixture: &Fixture, body: impl FnOnce() -> T) -> T {
    use harn_vm::orchestration::{
        pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
    };
    let mut policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![fixture.workspace.path().display().to_string()],
        ..CapabilityPolicy::default()
    };
    policy
        .process_sandbox
        .read_roots
        .push(fixture.user.path().display().to_string());
    push_execution_policy(policy);
    struct PopPolicy;
    impl Drop for PopPolicy {
        fn drop(&mut self) {
            pop_execution_policy();
        }
    }
    let _policy = PopPolicy;
    body()
}

/// A caller variable that would route, intercept or log gh's requests never
/// reaches a granted gh: everything but a short allowlist is dropped, and the
/// proxy and debug variables are set empty. Needs a live process sandbox.
#[test]
fn caller_proxy_tls_and_debug_variables_never_reach_a_granted_gh() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();
    let blocked = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
        "NO_PROXY",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
        "GODEBUG",
        "CURL_CA_BUNDLE",
        "GITHUB_TOKEN",
    ];
    confined(&fixture, || {
        for var in blocked {
            let injected = format!("harn-injected-{}", var.to_ascii_lowercase());
            let out = fixture.argv(&["gh", "issue", "status"], &[(var, injected.clone())]);
            assert_granted_without_leak(&out, var);
            assert!(
                !out.contains(&injected),
                "{var} reached the granted gh:\n{out}"
            );
        }
        let out = fixture.argv(
            &["gh", "issue", "status"],
            &[("LANG", "C.UTF-8".to_string())],
        );
        assert!(
            out.contains("LANG=C.UTF-8"),
            "an allowlisted variable was dropped:\n{out}"
        );
        for empty in ["HTTP_PROXY=", "HTTPS_PROXY=", "GODEBUG=", "GH_DEBUG="] {
            assert!(
                out.lines().any(|line| line == empty),
                "{empty} is not set empty:\n{out}"
            );
        }
    });
}

/// While a granted gh runs, its credential sits in its private `hosts.yml`
/// and nowhere in its environment. A confined sibling can read neither the
/// file nor, on Linux, the process's environment. An unconfined reader in
/// the same window is the control: it reads the file and finds the
/// environment free of the token. Needs a live process sandbox.
#[test]
fn a_confined_sibling_cannot_read_a_running_granted_gh_credential() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();
    let hosts = fixture
        .user
        .path()
        .join("harn-home/grant-spawns/*/config/hosts.yml")
        .display()
        .to_string();
    let reader = format!(
        "echo file=$(cat {hosts} 2>/dev/null | grep -c oauth_token); \
         pid=$(cat gh.pid); \
         if [ -r /proc/self/environ ]; then \
           echo environ=$(tr '\\0' '\\n' < /proc/$pid/environ 2>/dev/null | grep -c -e '^PATH=' -e harn-probe-gh-gran[t]); \
           echo token=$(tr '\\0' '\\n' < /proc/$pid/environ 2>/dev/null | grep -c harn-probe-gh-gran[t]); \
         fi"
    );
    let session = harn_vm::stdlib::process::current_session_environment();
    let (granted, confined_read, unconfined_read, peer) = std::thread::scope(|scope| {
        let fixture = &fixture;
        let granted = scope.spawn(move || {
            harn_vm::stdlib::process::set_session_environment(session);
            confined(fixture, || fixture.shell("gh issue view 1"))
        });
        let started = std::time::Instant::now();
        while !fixture.workspace("gh.pid").exists() {
            assert!(
                started.elapsed() < std::time::Duration::from_secs(20),
                "the granted gh never started"
            );
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let confined_read = confined(fixture, || fixture.shell(&reader));
        let unconfined_read = fixture.shell(&reader);
        // A second granted gh reads its own login and not the running one's.
        let peer = confined(fixture, || fixture.shell("gh pr status"));
        (
            granted.join().unwrap(),
            confined_read,
            unconfined_read,
            peer,
        )
    });
    assert!(
        peer.contains("own=2") && peer.contains("other=0"),
        "a granted gh must read its own login and no concurrent spawn's:\n{peer}"
    );
    assert_granted_without_leak(&granted, "sleeping read");
    assert!(
        unconfined_read.contains("file=2"),
        "control: the unconfined reader must see the live hosts.yml:\n{unconfined_read}"
    );
    assert!(
        confined_read.contains("file=0"),
        "a confined sibling read the granted credential file:\n{confined_read}"
    );
    if cfg!(target_os = "linux") {
        assert!(
            unconfined_read.contains("token=0") && !unconfined_read.contains("environ=0"),
            "control: the unconfined reader must read the environment and find no token:\n{unconfined_read}"
        );
        assert!(
            confined_read.contains("environ=0"),
            "a confined sibling read the granted gh's environment:\n{confined_read}"
        );
    }
}

/// A crashed or killed spawn's directory, which holds its login, is removed by
/// the next granted spawn: when its process is gone, or when it is older than
/// the age bound. A link, an entry another mode or name, and a live spawn's
/// directory are left alone, and a link is never followed.
#[test]
fn a_granted_spawn_sweeps_directories_a_crashed_spawn_left_behind() {
    use std::os::unix::fs::DirBuilderExt as _;
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();
    let base = fixture.user.path().join("harn-home/grant-spawns");
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(&base).unwrap();
    let dead = {
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        pid
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let me = std::process::id();
    let leaf = |name: String, mode: u32| {
        let dir = base.join(name);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(mode)
            .create(dir.join("config"))
            .unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).unwrap();
        std::fs::write(dir.join("config/hosts.yml"), "oauth_token: x\n").unwrap();
        dir
    };
    let crashed = leaf(format!("{dead}-0-{now}"), 0o700);
    let expired = leaf(format!("{me}-1-0"), 0o700);
    let live = leaf(format!("{me}-2-{now}"), 0o700);
    let shared = leaf(format!("{dead}-3-{now}"), 0o755);
    let unrelated = leaf("keep-me".to_string(), 0o700);
    let outside = fixture.user.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("marker"), "x").unwrap();
    let link = base.join(format!("{dead}-4-{now}"));
    std::os::unix::fs::symlink(&outside, &link).unwrap();

    assert_granted_without_leak(&fixture.shell("gh issue list"), "sweeping read");
    assert!(!crashed.exists(), "a dead process's directory survived");
    assert!(!expired.exists(), "a directory past the age bound survived");
    for kept in [&live, &shared, &unrelated] {
        assert!(
            kept.join("config/hosts.yml").exists(),
            "{} was removed",
            kept.display()
        );
    }
    assert!(
        std::fs::symlink_metadata(&link).is_ok() && outside.join("marker").exists(),
        "the sweep followed or removed a link"
    );
}

/// The same proof through a confined spawn. Needs a live process sandbox.
#[test]
fn a_confined_plain_gh_shell_command_receives_only_the_grant() {
    use harn_vm::orchestration::{
        pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
    };

    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();
    let mut policy = CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![fixture.workspace.path().display().to_string()],
        ..CapabilityPolicy::default()
    };
    policy
        .process_sandbox
        .read_roots
        .push(fixture.user.path().display().to_string());
    push_execution_policy(policy);
    struct PopPolicy;
    impl Drop for PopPolicy {
        fn drop(&mut self) {
            pop_execution_policy();
        }
    }
    let _policy = PopPolicy;

    let out = fixture.shell("gh issue list");
    assert_granted_without_leak(&out, "confined");
    assert!(
        out.contains("pager=[] editor=[/usr/bin/false]"),
        "stdout:\n{out}"
    );
    let out = fixture.shell("gh issue list; env");
    assert!(out.contains("gh-grant=absent"), "stdout:\n{out}");
    assert_env_ran_without_grant(&out, "confined gh issue list; env");
}

/// The GitHub CLI itself, when the host has it: `gh auth token` is refused,
/// and a planted `!` alias never runs. Uses a probe value, never a token.
#[test]
fn the_real_github_cli_neither_prints_nor_leaks_the_grant() {
    let Some(real) = ["/usr/local/bin/gh", "/opt/homebrew/bin/gh", "/usr/bin/gh"]
        .into_iter()
        .find(|path| Path::new(path).is_file())
    else {
        eprintln!("skipped: no GitHub CLI installed on this host");
        return;
    };
    let _env_guard = super::process_tools_e2e::lock_env();
    let real_dir = Path::new(real).parent().unwrap().display().to_string();
    let fixture = Fixture::with(|_| Paths {
        planted_path: Some(real_dir),
        harn_home: None,
    });
    let home = fixture.user.path().join("home/.config/gh/config.yml");
    std::fs::write(&home, "aliases:\n    leak: '!echo leaked=$GH_TOKEN'\n").unwrap();
    let attacker = fixture.workspace(".x");
    std::fs::create_dir_all(&attacker).unwrap();
    std::fs::write(
        attacker.join("config.yml"),
        "aliases:\n    triage: '!echo leaked=$GH_TOKEN'\n",
    )
    .unwrap();
    let stand_in = fixture.user.path().join("harn-cache/gh-grant");
    std::fs::create_dir_all(&stand_in).unwrap();
    std::os::unix::fs::symlink(&attacker, stand_in.join("config")).unwrap();

    let error = fixture
        .shell_in(fixture.workspace.path(), "gh auth token")
        .err()
        .expect("gh auth token must be refused");
    assert!(error.contains("command_grant_refused"), "{error}");
    // An alias is never a read, so gh runs it without the credential: the
    // user's own `!` alias prints an empty value, and the planted one is
    // unknown because the link is never read.
    let out = fixture
        .shell_in(fixture.workspace.path(), "gh leak")
        .unwrap();
    assert!(
        out.stdout.contains("leaked="),
        "the alias never ran: {}",
        out.stdout
    );
    let out_triage = fixture
        .shell_in(fixture.workspace.path(), "gh triage")
        .unwrap();
    assert!(
        out_triage.stderr.contains("unknown command"),
        "{}",
        out_triage.stderr
    );
    for out in [out, out_triage] {
        assert!(
            !out.stdout.contains(PROBE) && !out.stderr.contains(PROBE),
            "{}\n{}",
            out.stdout,
            out.stderr
        );
    }
}
