//! A `for_command: "gh"` grant reaches the one `gh` process it names, through
//! the real `run_command` spawn path, and nothing that process or a shell
//! could start with its environment.
//!
//! The `gh` here is a stand-in script that reports whether the granted
//! variable arrived, and that reproduces the GitHub CLI's own exec paths: a
//! `!` alias from `$GH_CONFIG_DIR/config.yml` runs through `sh -c`, an
//! extension under `$XDG_DATA_HOME/gh/extensions` is executed, output goes
//! through `$GH_PAGER`, `pr checkout` runs git, and `auth token` prints the
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
const PROBE_BASIC: &str = "eC1hY2Nlc3MtdG9rZW46aGFybi1wcm9iZS1naC1ncmFudA";

const FAKE_GH: &str = r#"#!/bin/sh
if [ -n "$GH_TOKEN" ]; then echo "gh-grant=present"; else echo "gh-grant=absent"; fi
echo "pager=[$GH_PAGER] editor=[$GH_EDITOR]"
echo "config=[$GH_CONFIG_DIR]"
config="${GH_CONFIG_DIR:-$HOME/.config/gh}/config.yml"
if [ -f "$config" ]; then sed 's/^/cfg: /' "$config"; fi
case "$1 $2" in
  "auth token") echo "$GH_TOKEN"; exit 0 ;;
  "pr checkout") exec git checkout -q -B "$3" ;;
  "api leak")
    printf 'token=%s' "$(printf '%s' "$GH_TOKEN" | cut -c1-9)"; sleep 0.2
    printf '%s\n' "$(printf '%s' "$GH_TOKEN" | cut -c10-)"
    printf 'b64=%s\n' "$(printf '%s' "$GH_TOKEN" | base64)"
    printf 'basic=%s\n' "$(printf 'x-access-token:%s' "$GH_TOKEN" | base64)"
    printf 'err=%s\n' "$GH_TOKEN" >&2
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

fn write_executable(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
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
        let user = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).expect("user root");
        let workspace = tempfile::tempdir().expect("workspace root");
        let home = user.path().join("home");
        write_executable(&user.path().join("bin/gh"), FAKE_GH);
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
        let set: [(&'static str, PathBuf); 5] = [
            ("PATH", PathBuf::from(search_path)),
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
            .with_secret(SecretId::new(SECRET_ACCOUNT, SECRET_KEY), PROBE),
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
fn a_granted_gh_cannot_hand_the_grant_to_an_alias_an_extension_or_a_pager() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();

    for (vector, command) in [("`!` alias", "gh leak"), ("extension", "gh ext")] {
        assert_granted_without_leak(&fixture.shell(command), vector);
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

    assert_granted_without_leak(&fixture.shell("gh triage"), "planted stand-in");
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
    assert_granted_without_leak(&fixture.shell("gh auth status"), "auth status");

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
    fixture
        .shell_in(&repo, "gh pr checkout control; true")
        .unwrap();
    let control = std::fs::read_to_string(&hook_env).expect("the hook must fire");
    assert!(control.contains("PATH="), "{control}");
    std::fs::remove_file(&hook_env).unwrap();

    let out = fixture.shell_in(&repo, "gh pr checkout feature").unwrap();
    assert_granted_without_leak(&out.stdout, "pr checkout");
    let granted = std::fs::read_to_string(&hook_env).unwrap_or_default();
    assert!(
        !granted.contains(&format!("{GRANTED}=")),
        "a workspace git hook saw the grant"
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
    for alias in ["gh leak", "gh triage"] {
        let out = fixture.shell_in(fixture.workspace.path(), alias).unwrap();
        assert!(
            !out.stdout.contains("leaked=") && !out.stderr.contains("leaked="),
            "{alias} ran a shell alias:\n{}\n{}",
            out.stdout,
            out.stderr
        );
        assert!(!out.stdout.contains(PROBE) && !out.stderr.contains(PROBE));
        // The control that gh itself ran: it rejects the unknown command.
        assert!(
            out.stderr.contains("unknown command"),
            "{alias}: {}",
            out.stderr
        );
    }
}
