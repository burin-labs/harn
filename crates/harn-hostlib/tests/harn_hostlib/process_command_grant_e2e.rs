//! A `for_command: "gh"` grant reaches the one `gh` process it names, through
//! the real `run_command` spawn path, and nothing that process or a shell
//! could start with its environment.
//!
//! The `gh` on `PATH` here is a stand-in script that reports whether the
//! granted variable arrived, and that reproduces the GitHub CLI's own exec
//! paths: a `!` alias from `$GH_CONFIG_DIR/config.yml` runs through `sh -c`,
//! an extension under `$XDG_DATA_HOME/gh/extensions` is executed, and output
//! goes through `$GH_PAGER`. The granted value is a literal probe, never a
//! token. Each leak vector runs `env`; a leak shows as `GH_TOKEN=` in stdout,
//! and every negative also checks that `env` actually ran, so an empty read
//! cannot pass for a closed one.

#![cfg(unix)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;

use harn_vm::security::{EnvironmentPolicyKind, GrantSourceSpec, GrantSpec, SessionEnvironment};
use harn_vm::value::VmDictExt as _;
use harn_vm::VmValue;

use super::support::*;

const GRANTED: &str = "GH_TOKEN";
const PROBE: &str = "harn-probe-gh-grant";

const FAKE_GH: &str = r#"#!/bin/sh
if [ -n "$GH_TOKEN" ]; then echo "gh-grant=present"; else echo "gh-grant=absent"; fi
echo "pager=[$GH_PAGER] editor=[$GH_EDITOR]"
echo "config=[$GH_CONFIG_DIR]"
config="${GH_CONFIG_DIR:-$HOME/.config/gh}/config.yml"
if [ -n "$1" ] && [ -f "$config" ]; then
  value=$(sed -n "s/^[[:space:]]*$1:[[:space:]]*//p" "$config" | head -n 1 | sed "s/^'//; s/'\$//")
  case "$value" in '!'*) exec sh -c "${value#!}" ;; esac
fi
extension="${XDG_DATA_HOME:-$HOME/.local/share}/gh/extensions/gh-$1/gh-$1"
if [ -n "$1" ] && [ -x "$extension" ]; then exec "$extension"; fi
if [ -n "$GH_PAGER" ]; then echo paged | $GH_PAGER; fi
"#;

/// The user's gh setup the grant must not open: a `!` alias and an extension,
/// both running `env`.
struct Fixture {
    root: tempfile::TempDir,
    previous: Vec<(&'static str, Option<OsString>)>,
}

fn write_executable(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("fixture root");
        let path = root.path();
        write_executable(&path.join("bin/gh"), FAKE_GH);
        std::fs::create_dir_all(path.join("home/.config/gh")).unwrap();
        std::fs::write(
            path.join("home/.config/gh/config.yml"),
            "git_protocol: https\naliases:\n    co: pr checkout\n    leak: '!env'\n",
        )
        .unwrap();
        write_executable(
            &path.join("user-data/gh/extensions/gh-ext/gh-ext"),
            "#!/bin/sh\nenv\n",
        );
        let search_path = format!(
            "{}:{}",
            path.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        // A granted policy's launcher snapshot keeps allowlisted names only,
        // so the stand-in seeds from `$HOME/.config/gh`, as it would for a user
        // with no GH_CONFIG_DIR override.
        let set: [(&'static str, String); 5] = [
            ("PATH", search_path),
            ("HOME", path.join("home").display().to_string()),
            (
                "GH_CONFIG_DIR",
                path.join("home/.config/gh").display().to_string(),
            ),
            (
                "XDG_DATA_HOME",
                path.join("user-data").display().to_string(),
            ),
            (
                "HARN_CACHE_DIR",
                path.join("harn-cache").display().to_string(),
            ),
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
            source: GrantSourceSpec::Literal {
                value: PROBE.to_string(),
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
        Self { root, previous }
    }

    fn path(&self, relative: &str) -> String {
        self.root.path().join(relative).display().to_string()
    }

    fn request(&self) -> harn_vm::value::DictMap {
        let mut request = harn_vm::value::DictMap::new();
        request.insert("cwd".into(), vm_string(&self.path("")));
        request
    }

    fn shell(&self, command: &str) -> String {
        let mut request = self.request();
        request.insert("mode".into(), vm_string("shell"));
        request.insert("command".into(), vm_string(command));
        let mut shell = harn_vm::value::DictMap::new();
        shell.insert("path".into(), vm_string("/bin/sh"));
        request.put("shell", VmValue::dict(shell));
        stdout(request)
    }

    fn argv(&self, argv: &[&str], env: &[(&str, String)]) -> String {
        let mut request = self.request();
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
        stdout(request)
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

fn stdout(request: harn_vm::value::DictMap) -> String {
    let response = call_tool("hostlib_tools_run_command", request).expect("run_command");
    let VmValue::Dict(dict) = response else {
        panic!("run_command must answer with a dict, got {response:?}");
    };
    match dict.get("stdout") {
        Some(VmValue::String(stdout)) => stdout.to_string(),
        other => panic!("run_command carried no stdout: {other:?}"),
    }
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

#[test]
fn a_plain_gh_shell_command_receives_the_grant_with_its_exec_paths_closed() {
    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();

    let out = fixture.shell("gh issue list --limit 5");
    assert!(
        out.contains("gh-grant=present"),
        "a shell-mode `gh` command must reach the gh grant; stdout:\n{out}"
    );
    assert!(
        out.contains("pager=[] editor=[/usr/bin/false]"),
        "a granted gh must carry an empty pager and a refusing editor; stdout:\n{out}"
    );
    // The direction control: argv mode always reached the grant.
    assert!(fixture
        .argv(&["gh", "issue", "list"], &[])
        .contains("gh-grant=present"));
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
    // The caller points gh at the user's own config and extensions, which
    // carry a `!` alias and an extension that both run `env`.
    let user_setup = [
        ("GH_CONFIG_DIR", fixture.path("home/.config/gh")),
        ("XDG_DATA_HOME", fixture.path("user-data")),
    ];

    for (vector, argv, extra) in [
        ("`!` alias", vec!["gh", "leak"], None),
        ("extension", vec!["gh", "ext"], None),
        (
            "pager",
            vec!["gh", "issue", "list"],
            Some(("GH_PAGER", "env".to_string())),
        ),
    ] {
        let mut env = user_setup.to_vec();
        env.extend(extra);
        let out = fixture.argv(&argv, &env);
        assert!(
            out.contains("gh-grant=present"),
            "{vector}: the direct gh spawn must still be granted; stdout:\n{out}"
        );
        assert!(
            !out.contains(&format!("{GRANTED}=")),
            "{vector}: gh handed the grant to a program the grant never named; stdout:\n{out}"
        );
    }
    let out = fixture.shell("gh leak");
    assert!(out.contains("gh-grant=present"), "stdout:\n{out}");
    assert!(!out.contains(&format!("{GRANTED}=")), "stdout:\n{out}");

    // The stand-in kept the plain alias and dropped the shell alias.
    let config_dir = out
        .lines()
        .find_map(|line| line.strip_prefix("config=[")?.strip_suffix(']'))
        .unwrap_or_default()
        .to_string();
    let stand_in = std::fs::read_to_string(Path::new(&config_dir).join("config.yml"))
        .unwrap_or_else(|error| panic!("no stand-in config at {config_dir:?}: {error}"));
    assert!(stand_in.contains("co: pr checkout"), "{stand_in}");
    assert!(!stand_in.contains("!env"), "{stand_in}");
}

/// The same proof through a confined spawn. Needs a live process sandbox.
#[test]
fn a_confined_plain_gh_shell_command_receives_only_the_grant() {
    use harn_vm::orchestration::{
        pop_execution_policy, push_execution_policy, CapabilityPolicy, SandboxProfile,
    };

    let _env_guard = super::process_tools_e2e::lock_env();
    let fixture = Fixture::new();
    push_execution_policy(CapabilityPolicy {
        sandbox_profile: SandboxProfile::Worktree,
        workspace_roots: vec![fixture.path("")],
        ..CapabilityPolicy::default()
    });
    struct PopPolicy;
    impl Drop for PopPolicy {
        fn drop(&mut self) {
            pop_execution_policy();
        }
    }
    let _policy = PopPolicy;

    let out = fixture.shell("gh issue list");
    assert!(out.contains("gh-grant=present"), "stdout:\n{out}");
    assert!(
        out.contains("pager=[] editor=[/usr/bin/false]"),
        "stdout:\n{out}"
    );
    let out = fixture.shell("gh issue list; env");
    assert!(out.contains("gh-grant=absent"), "stdout:\n{out}");
    assert_env_ran_without_grant(&out, "confined gh issue list; env");
}
