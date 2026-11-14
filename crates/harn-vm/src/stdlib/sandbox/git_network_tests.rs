//! A granted git fetch and push against a local smart-HTTP server that wants
//! the login, with every hostile repository setting that could otherwise run
//! a program or route the request armed. Each hostile setting is also run
//! through plain git as its falsifier, so a quiet canary means "not run", not
//! "never reachable".

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use super::{parse, Command, TEST_ORIGIN};

const TOKEN: &str = "git-network-fixture-canary-5531";

fn git_program() -> PathBuf {
    let path = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path)
        .map(|dir| dir.join("git"))
        .find(|candidate| candidate.is_file())
        .and_then(|git| std::fs::canonicalize(git).ok())
        .expect("git on PATH")
}

fn git(cwd: &Path, args: &[&str]) -> std::process::Output {
    let output = std::process::Command::new(git_program())
        .args(args)
        .current_dir(cwd)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .output()
        .unwrap();
    output
}

fn ok(cwd: &Path, args: &[&str]) -> String {
    let output = git(cwd, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A smart-HTTP server over `git http-backend` that answers 401 until a
/// request carries the fixture login. It reports each request's
/// `Authorization` header, or `-` for none.
fn serve(root: PathBuf, listener: TcpListener) -> mpsc::Receiver<String> {
    let (seen, received) = mpsc::channel();
    let git = git_program();
    let expected = format!("Basic {}", base64_login());
    crate::runtime_stack::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(&stream);
            let mut request = String::new();
            if reader.read_line(&mut request).is_err() {
                continue;
            }
            let mut headers = BTreeMap::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
                }
            }
            let length = headers
                .get("content-length")
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            let mut body = vec![0; length];
            let _ = reader.read_exact(&mut body);
            let authorization = headers.get("authorization").cloned();
            let _ = seen.send(authorization.clone().unwrap_or_else(|| "-".into()));
            let mut out = &stream;
            if authorization.as_deref() != Some(expected.as_str()) {
                let _ = out.write_all(b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"fixture\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                continue;
            }
            let mut words = request.split_whitespace();
            let method = words.next().unwrap_or_default().to_string();
            let target = words.next().unwrap_or_default();
            let (path, query) = target.split_once('?').unwrap_or((target, ""));
            let mut cgi = std::process::Command::new(&git);
            cgi.arg("http-backend")
                .env_remove("GIT_TRACE")
                .env_remove("GIT_TRACE_CURL")
                .env("GIT_PROJECT_ROOT", &root)
                .env("GIT_HTTP_EXPORT_ALL", "1")
                .env("REMOTE_USER", "fixture")
                .env("REQUEST_METHOD", method)
                .env("PATH_INFO", path)
                .env("QUERY_STRING", query)
                .env("CONTENT_LENGTH", body.len().to_string())
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped());
            for (header, var) in [
                ("content-type", "CONTENT_TYPE"),
                ("content-encoding", "HTTP_CONTENT_ENCODING"),
                ("git-protocol", "GIT_PROTOCOL"),
            ] {
                if let Some(value) = headers.get(header) {
                    cgi.env(var, value);
                }
            }
            let mut child = cgi.spawn().unwrap();
            child.stdin.take().unwrap().write_all(&body).unwrap();
            let output = child.wait_with_output().unwrap();
            let split = output
                .stdout
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map_or(0, |at| at + 4);
            let (cgi_headers, cgi_body) = output.stdout.split_at(split);
            let cgi_headers = String::from_utf8_lossy(cgi_headers);
            let status = cgi_headers
                .lines()
                .find_map(|line| line.strip_prefix("Status: "))
                .unwrap_or("200 OK")
                .to_string();
            let _ = write!(
                out,
                "HTTP/1.1 {status}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n",
                cgi_headers
                    .trim_end()
                    .lines()
                    .filter(|line| !line.starts_with("Status:"))
                    .map(|line| format!("{line}\r\n"))
                    .collect::<String>(),
                cgi_body.len()
            );
            let _ = out.write_all(cgi_body);
        }
    });
    received
}

fn base64_login() -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(format!("{}:{TOKEN}", super::LOGIN_USER))
}

/// A workspace repository whose `origin` is a fixture repository behind the
/// login, with one commit on the server.
struct Fixture {
    root: tempfile::TempDir,
    url: String,
    seen: mpsc::Receiver<String>,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let server = root.path().join("server/o");
        std::fs::create_dir_all(&server).unwrap();
        ok(&server, &["init", "-q", "--bare", "r.git"]);
        let seed = root.path().join("seed");
        ok(root.path(), &["init", "-q", "-b", "main", "seed"]);
        ok(
            &seed,
            &[
                "-c",
                "user.name=f",
                "-c",
                "user.email=f@f",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "a",
            ],
        );
        ok(&seed, &["tag", "v1"]);
        ok(
            &seed,
            &[
                "push",
                "-q",
                server.join("r.git").to_str().unwrap(),
                "main",
                "v1",
            ],
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let seen = serve(root.path().join("server"), listener);
        TEST_ORIGIN.with(|test| *test.borrow_mut() = Some(origin.clone()));
        let url = format!("{origin}/o/r.git");
        ok(root.path(), &["init", "-q", "-b", "main", "ws"]);
        ok(&root.path().join("ws"), &["remote", "add", "origin", &url]);
        Self { root, url, seen }
    }

    fn ws(&self) -> PathBuf {
        self.root.path().join("ws")
    }

    fn canary(&self) -> PathBuf {
        self.root.path().join("canary")
    }

    /// Run `git args` in the workspace the way the process seams do: the
    /// granted program and environment over this process's own environment,
    /// with every name the spawn lists removed. `ambient` is set in this
    /// process's environment for the call.
    fn granted(&self, args: &[&str], ambient: &[(&str, &str)]) -> Option<std::process::Output> {
        self.granted_planting(args, ambient, |_| {})
    }

    /// [`Fixture::granted`], with `plant` run on the per-spawn directory after
    /// it is seeded and before git starts; the directory's files are checked
    /// for the login while it still exists.
    fn granted_planting(
        &self,
        args: &[&str],
        ambient: &[(&str, &str)],
        plant: impl FnOnce(&Path),
    ) -> Option<std::process::Output> {
        let _env_lock = crate::runtime_paths::test_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for (key, value) in ambient {
            std::env::set_var(key, value);
        }
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        let output =
            crate::stdlib::process::granted_spawn("git", &args, &self.ws(), &BTreeMap::new())
                .expect("no refusal")
                .map(|spawn| {
                    let dir = spawn
                        .env
                        .iter()
                        .find(|(key, _)| key == "GIT_DIR")
                        .map(|(_, dir)| PathBuf::from(dir))
                        .expect("a granted git runs in its per-spawn directory");
                    plant(&dir);
                    let _launch = spawn.launching();
                    let mut command = std::process::Command::new(&spawn.program);
                    command.args(&args).current_dir(self.ws());
                    for key in &spawn.remove {
                        command.env_remove(key);
                    }
                    command.envs(spawn.env.iter().map(|(key, value)| (key, value)));
                    let output = command.output().unwrap();
                    assert_no_login_under(&dir);
                    output
                });
        for (key, _) in ambient {
            std::env::remove_var(key);
        }
        output
    }

    fn requests(&self) -> Vec<String> {
        self.seen.try_iter().collect()
    }
}

struct Granted(());

impl Granted {
    fn enter() -> Self {
        let git_dir = git_program().parent().unwrap().display().to_string();
        let lookup = move |name: &str| match name {
            "FIXTURE_LOGIN" => Some(TOKEN.to_string()),
            "PATH" => Some(git_dir.clone()),
            _ => None,
        };
        let environment = crate::security::SessionEnvironment::launch_from_snapshot(
            crate::security::EnvironmentPolicyKind::Granted,
            vec![crate::security::GrantSpec {
                name: "git_login".to_string(),
                source: crate::security::GrantSourceSpec::SecretEnv {
                    var: "FIXTURE_LOGIN".to_string(),
                },
                expose_as_env: Some("GH_TOKEN".to_string()),
                for_command: Some("git".to_string()),
                expose_to: Default::default(),
            }],
            BTreeMap::from([("PATH".to_string(), lookup("PATH").unwrap())]),
            &lookup,
        )
        .unwrap();
        crate::stdlib::process::set_session_environment(Some(environment));
        Self(())
    }
}

impl Drop for Granted {
    fn drop(&mut self) {
        crate::stdlib::process::set_session_environment(None);
        TEST_ORIGIN.with(|test| *test.borrow_mut() = None);
    }
}

/// No regular file under `dir` (links are not followed) holds the login.
fn assert_no_login_under(dir: &Path) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            assert_no_login_under(&entry.path());
        } else if kind.is_file() {
            let text = std::fs::read(entry.path()).unwrap();
            assert!(
                !String::from_utf8_lossy(&text).contains(TOKEN),
                "{} holds the login",
                entry.path().display()
            );
        }
    }
}

fn assert_succeeded(output: &std::process::Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    assert!(
        !stderr.contains(TOKEN) && !stderr.contains(&base64_login()),
        "{stderr}"
    );
    stderr
}

#[test]
fn a_granted_fetch_and_push_send_the_login_and_land_in_the_repository() {
    let _granted = Granted::enter();
    let fixture = Fixture::new();
    let ws = fixture.ws();
    assert_succeeded(
        &fixture
            .granted(&["fetch", "origin"], &[])
            .expect("fetch is granted"),
    );
    let remote_main = ok(&ws, &["rev-parse", "refs/remotes/origin/main"]);
    assert_eq!(
        ok(&ws, &["tag", "--list"]),
        "",
        "a granted fetch writes no tags"
    );
    ok(&ws, &["checkout", "-q", "-b", "feature", "origin/main"]);
    ok(
        &ws,
        &[
            "-c",
            "user.name=f",
            "-c",
            "user.email=f@f",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "b",
        ],
    );
    assert_succeeded(
        &fixture
            .granted(&["push", "origin", "HEAD:refs/heads/feature"], &[])
            .expect("push is granted"),
    );
    let pushed = ok(&ws, &["rev-parse", "HEAD"]);
    let server = fixture.root.path().join("server/o/r.git");
    assert_eq!(ok(&server, &["rev-parse", "refs/heads/feature"]), pushed);
    assert_eq!(
        ok(&ws, &["rev-parse", "refs/remotes/origin/feature"]),
        pushed,
        "the tracking ref lands in the repository"
    );
    assert_ne!(remote_main, pushed);
    let requests = fixture.requests();
    assert!(
        requests
            .iter()
            .any(|seen| seen == &format!("Basic {}", base64_login())),
        "{requests:?}"
    );
}

/// Records each connection to a would-be proxy and closes it.
fn record_connections(listener: TcpListener) -> mpsc::Receiver<()> {
    let (dialed, received) = mpsc::channel();
    crate::runtime_stack::spawn(move || {
        for _ in listener.incoming().flatten() {
            let _ = dialed.send(());
        }
    });
    received
}

#[test]
fn hostile_repository_config_runs_nothing_and_never_sees_the_login() {
    let _granted = Granted::enter();
    let fixture = Fixture::new();
    let ws = fixture.ws();
    let canary = fixture.canary();
    let mitm = TcpListener::bind("127.0.0.1:0").unwrap();
    let proxy = format!("http://{}", mitm.local_addr().unwrap());
    let dialed = record_connections(mitm);
    let record = format!(
        "!f() {{ echo helper >> '{0}'; cat >> '{0}'; }}; f",
        canary.display()
    );
    let hook = format!(
        "#!/bin/sh\n{{ echo hook; env; }} >> '{}'\n",
        canary.display()
    );
    let hooks = ws.join(".git/hooks");
    for name in ["pre-push", "reference-transaction"] {
        std::fs::write(hooks.join(name), &hook).unwrap();
        std::fs::set_permissions(
            hooks.join(name),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
    }
    let monitor = format!("printf 'monitor\\n' >> '{}'; false", canary.display());
    for (key, value) in [
        ("credential.helper", record.as_str()),
        ("core.fsmonitor", monitor.as_str()),
        ("alias.leak", "!env"),
        ("remote.origin.pushurl", "http://127.0.0.1:9/o/r.git"),
        ("http.proxy", proxy.as_str()),
        ("http.sslVerify", "false"),
        ("core.alternateRefsCommand", record.as_str()),
    ] {
        ok(&ws, &["config", key, value]);
    }
    // An alternate naming where per-spawn directories live reaches nothing
    // there: it holds no object store.
    std::fs::create_dir_all(ws.join(".git/objects/info")).unwrap();
    std::fs::write(
        ws.join(".git/objects/info/alternates"),
        format!(
            "{}\n",
            super::super::grant_spawn::grant_spawns_root().display()
        ),
    )
    .unwrap();
    let ambient = [
        ("GIT_TRACE_CURL", "1"),
        ("GIT_TRACE_REDACT", "0"),
        ("GIT_TRACE", "1"),
    ];

    assert_succeeded(
        &fixture
            .granted(&["fetch", "origin"], &ambient)
            .expect("fetch is granted"),
    );
    assert!(
        !canary.exists(),
        "{}",
        std::fs::read_to_string(&canary).unwrap_or_default()
    );
    ok(
        &ws,
        &[
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
            "checkout",
            "-q",
            "-b",
            "feature",
            "origin/main",
        ],
    );
    assert_succeeded(
        &fixture
            .granted(&["push", "origin", "HEAD:refs/heads/feature"], &ambient)
            .expect("push is granted"),
    );
    assert!(
        !canary.exists(),
        "{}",
        std::fs::read_to_string(&canary).unwrap_or_default()
    );
    assert!(
        dialed.try_recv().is_err(),
        "the repository proxy is never dialed"
    );
    assert!(
        fixture.granted(&["leak"], &[]).is_none(),
        "an alias never receives the login"
    );

    // Falsifier: plain git with the login stored for this repository runs
    // each hostile setting, and each one reaches the login.
    let store = fixture.root.path().join("plain-store");
    let stored = fixture
        .url
        .replacen("://", &format!("://{}:{TOKEN}@", super::LOGIN_USER), 1);
    std::fs::write(&store, format!("{stored}\n")).unwrap();
    let plain = |args: &[&str], extra: &[(&str, &str)]| {
        std::process::Command::new(git_program())
            .args([
                "-c",
                &format!("credential.helper=store --file={}", store.display()),
            ])
            .args(args)
            .current_dir(&ws)
            .envs(extra.iter().copied())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap()
    };
    assert!(!plain(&["fetch", "origin"], &[]).status.success());
    assert!(
        dialed.recv().is_ok(),
        "plain git dials the repository proxy"
    );
    ok(&ws, &["config", "--unset", "http.proxy"]);
    ok(&ws, &["config", "--unset", "remote.origin.pushurl"]);
    let traced = plain(&["push", "origin", "HEAD:refs/heads/plain"], &ambient);
    assert!(
        String::from_utf8_lossy(&traced.stderr).contains(&base64_login()),
        "plain tracing prints the login"
    );
    let leaked = std::fs::read_to_string(&canary).unwrap_or_default();
    assert!(
        leaked.contains("helper") && leaked.contains(TOKEN),
        "a repository helper receives the login"
    );
    assert!(
        leaked.contains("hook"),
        "plain git runs the repository hooks"
    );
    plain(&["status"], &[]);
    let monitored = std::fs::read_to_string(&canary).unwrap_or_default();
    assert!(
        monitored.contains("monitor"),
        "plain git runs the repository monitor"
    );
    let alias = plain(&["leak"], &[]);
    assert!(
        String::from_utf8_lossy(&alias.stdout).contains("GIT_"),
        "the alias runs a shell"
    );
}

#[test]
fn only_network_subcommands_with_known_options_are_granted() {
    let granted = |words: &[&str]| {
        let args: Vec<String> = words.iter().map(|word| word.to_string()).collect();
        parse(&args).map(|invocation| {
            (
                invocation.command,
                invocation
                    .repositories
                    .iter()
                    .map(|r| r.to_string())
                    .collect::<Vec<_>>(),
                invocation.all,
            )
        })
    };
    assert_eq!(granted(&["fetch"]), Some((Command::Fetch, vec![], false)));
    assert_eq!(
        granted(&["fetch", "-p", "origin", "main"]),
        Some((Command::Fetch, vec!["origin".into()], false))
    );
    // Shallow-history options change `shallow`, which is a private copy.
    for option in ["--depth=1", "--deepen=1", "--unshallow", "--update-shallow"] {
        assert_eq!(granted(&["fetch", option, "origin"]), None, "{option}");
    }
    assert_eq!(
        granted(&["fetch", "--all", "--prune"]),
        Some((Command::Fetch, vec![], true))
    );
    assert_eq!(
        granted(&["fetch", "--multiple", "a", "b"]),
        Some((Command::Fetch, vec!["a".into(), "b".into()], false))
    );
    assert_eq!(
        granted(&[
            "push",
            "-fq",
            "--force-with-lease=main:abc",
            "origin",
            "HEAD"
        ]),
        Some((Command::Push, vec!["origin".into()], false))
    );
    assert_eq!(
        granted(&["ls-remote", "--heads", "https://github.com/o/r"]),
        Some((
            Command::LsRemote,
            vec!["https://github.com/o/r".into()],
            false
        ))
    );
    for refused in [
        &["pull"][..],
        &["clone", "https://github.com/o/r"],
        &["-c", "credential.helper=!x", "fetch"],
        &["fetch", "--upload-pack=x"],
        &["fetch", "--recurse-submodules"],
        &["fetch", "--all", "origin"],
        &["fetch", "--set-upstream", "origin"],
        &["push", "-u", "origin", "main"],
        &["push", "--signed", "origin"],
        &["push", "--receive-pack", "x", "origin"],
        &["push", "--exec=x", "origin"],
        &["push", "-qu", "origin"],
        &["ls-remote", "--upload-pack", "x", "origin"],
        &["leak"],
    ] {
        assert_eq!(granted(refused), None, "{refused:?}");
    }
}

#[test]
fn a_remote_that_is_not_a_plain_granted_url_runs_without_the_login() {
    let _granted = Granted::enter();
    let fixture = Fixture::new();
    let ws = fixture.ws();
    ok(
        &ws,
        &["remote", "add", "mirror", "ssh://example.invalid/o/r.git"],
    );
    ok(&ws, &["remote", "add", "rewritten", "gh:o/r"]);
    ok(&ws, &["config", "url.http://127.0.0.1:9/.insteadOf", "gh:"]);
    for args in [
        &["fetch", "mirror"][..],
        &["fetch", "rewritten"],
        &["fetch", "--all"],
        &["fetch", "http://127.0.0.1:9/o/r.git"],
        &["push", "mirror", "main"],
        &["fetch", "origin", "main:main"],
        &["fetch", "origin", "+refs/heads/*:refs/remotes/other/*"],
        &[
            "fetch",
            "origin",
            "refs/heads/main:refs/remotes/origin/../../heads/main",
        ],
    ] {
        assert!(fixture.granted(args, &[]).is_none(), "{args:?}");
    }
    // A remote whose configured refspec stores outside its tracking
    // namespace is not copied, so it is not granted either.
    ok(
        &ws,
        &[
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/heads/*:refs/heads/*",
        ],
    );
    assert!(fixture.granted(&["fetch", "origin"], &[]).is_none());
    ok(
        &ws,
        &[
            "config",
            "--unset",
            "remote.origin.fetch",
            r"refs/heads/\*$",
        ],
    );
    assert!(fixture.granted(&["fetch", "origin", "main"], &[]).is_some());
    assert!(
        crate::stdlib::process::granted_spawn(
            "git",
            &["fetch".to_string(), "origin".to_string()],
            &ws,
            &BTreeMap::from([("GIT_TRACE_CURL".to_string(), "1".to_string())]),
        )
        .unwrap()
        .is_none(),
        "a caller tracing variable takes the grant away"
    );
}

/// The login is never a file: every path the repository can point git at
/// (an alternate, a tracking ref, `FETCH_HEAD`), aimed at the credential
/// socket or the per-spawn directory after seeding, reads no login, and no
/// file anywhere holds it.
#[test]
fn a_repository_that_points_git_at_the_login_channel_reads_nothing() {
    let _granted = Granted::enter();
    let fixture = Fixture::new();
    let ws = fixture.ws();
    let git_dir = ws.join(".git");
    assert_succeeded(&fixture.granted(&["fetch", "origin"], &[]).expect("granted"));
    ok(&ws, &["checkout", "-q", "-B", "main", "origin/main"]);
    let _ = fixture.requests();
    for args in [
        &["fetch", "origin"][..],
        &["push", "origin", "HEAD:refs/heads/planted"],
    ] {
        let output = fixture
            .granted_planting(args, &[], |dir| {
                let socket = dir.join(super::CREDENTIAL_SOCKET);
                assert!(socket.exists(), "the login is served on the socket");
                std::fs::create_dir_all(git_dir.join("objects/info")).unwrap();
                std::fs::write(
                    git_dir.join("objects/info/alternates"),
                    format!("{}\n{}\n", socket.display(), dir.display()),
                )
                .unwrap();
                std::fs::create_dir_all(git_dir.join("refs/remotes/origin")).unwrap();
                for (link, target) in [
                    ("refs/remotes/origin/to-socket", socket.clone()),
                    ("refs/remotes/origin/to-config", dir.join("config")),
                    ("FETCH_HEAD", socket),
                ] {
                    let _ = std::fs::remove_file(git_dir.join(link));
                    std::os::unix::fs::symlink(&target, git_dir.join(link)).unwrap();
                }
            })
            .expect("granted");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !text.contains(TOKEN) && !text.contains(&base64_login()),
            "{args:?}: {text}"
        );
        for link in [
            "refs/remotes/origin/to-socket",
            "refs/remotes/origin/to-config",
            "FETCH_HEAD",
        ] {
            std::fs::remove_file(git_dir.join(link)).unwrap();
        }
        std::fs::remove_file(git_dir.join("objects/info/alternates")).unwrap();
    }
    // Not vacuous: the planted runs did ask for, and send, the login.
    let requests = fixture.requests();
    assert!(
        requests
            .iter()
            .any(|seen| seen == &format!("Basic {}", base64_login())),
        "{requests:?}"
    );
}

/// `shallow` is copied in, never linked, and a link, FIFO or link planted in
/// a tracking-ref path is refused before git starts.
#[test]
fn a_linked_shallow_or_tracking_ref_is_refused() {
    let _granted = Granted::enter();
    let fixture = Fixture::new();
    let ws = fixture.ws();
    let git_dir = ws.join(".git");
    let spawn = |args: &[&str]| {
        let args: Vec<String> = args.iter().map(|arg| arg.to_string()).collect();
        crate::stdlib::process::granted_spawn("git", &args, &ws, &BTreeMap::new())
    };
    let secret = fixture.root.path().join("secret");
    std::fs::write(&secret, "outside").unwrap();
    std::os::unix::fs::symlink(&secret, git_dir.join("shallow")).unwrap();
    assert!(spawn(&["fetch", "origin"]).is_err(), "a linked shallow");
    std::fs::remove_file(git_dir.join("shallow")).unwrap();

    // Entries git writes through a link must be the repository's own.
    let fetch_head = git_dir.join("FETCH_HEAD");
    let _ = std::fs::remove_file(&fetch_head);
    std::os::unix::fs::symlink(&secret, &fetch_head).unwrap();
    assert!(spawn(&["fetch", "origin"]).is_err(), "a linked FETCH_HEAD");
    std::fs::remove_file(&fetch_head).unwrap();
    let fifo = std::ffi::CString::new(fetch_head.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(spawn(&["fetch", "origin"]).is_err(), "a FIFO FETCH_HEAD");
    std::fs::remove_file(&fetch_head).unwrap();
    std::fs::rename(git_dir.join("objects"), git_dir.join("objects-real")).unwrap();
    std::os::unix::fs::symlink("objects-real", git_dir.join("objects")).unwrap();
    assert!(spawn(&["fetch", "origin"]).is_err(), "a linked objects");
    std::fs::remove_file(git_dir.join("objects")).unwrap();
    std::fs::rename(git_dir.join("objects-real"), git_dir.join("objects")).unwrap();

    std::fs::create_dir_all(git_dir.join("refs/remotes/origin")).unwrap();
    std::os::unix::fs::symlink("../../heads", git_dir.join("refs/remotes/origin/sub")).unwrap();
    assert!(
        spawn(&["fetch", "origin"]).is_err(),
        "a tracking directory holding a link"
    );
    std::fs::remove_file(git_dir.join("refs/remotes/origin/sub")).unwrap();
    std::fs::remove_dir(git_dir.join("refs/remotes/origin")).unwrap();
    std::os::unix::fs::symlink("../heads", git_dir.join("refs/remotes/origin")).unwrap();
    assert!(
        spawn(&["fetch", "origin"]).is_err(),
        "a linked tracking directory"
    );
    std::fs::remove_file(git_dir.join("refs/remotes/origin")).unwrap();

    // A FIFO where the repository config belongs refuses rather than hangs.
    let config = git_dir.join("config");
    let saved = std::fs::read(&config).unwrap();
    std::fs::remove_file(&config).unwrap();
    let fifo = std::ffi::CString::new(config.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(
        matches!(spawn(&["fetch", "origin"]), Ok(None)),
        "a FIFO config runs ungranted"
    );
    std::fs::remove_file(&config).unwrap();
    std::fs::write(&config, saved).unwrap();

    // A regular shallow file is copied in, and the fetch is granted.
    let main = ok(
        &fixture.root.path().join("server/o/r.git"),
        &["rev-parse", "main"],
    );
    std::fs::write(git_dir.join("shallow"), format!("{main}\n")).unwrap();
    assert!(matches!(spawn(&["fetch", "origin"]), Ok(Some(_))));
}

/// The granted fetch and push work under the real sandbox profile, whose
/// per-spawn root denial reaches every confined child and which re-allows
/// only this spawn's own directory and socket. Run unconfined, the tests
/// above cannot see a profile that leaves git unable to open its own git
/// directory or ask its socket.
#[test]
fn a_confined_granted_fetch_and_push_land() {
    use crate::orchestration::{
        pop_execution_policy, push_execution_policy, CapabilityPolicy, ProcessSandboxPolicy,
        ProcessSandboxPreset, SandboxProfile,
    };
    let _granted = Granted::enter();
    let fixture = Fixture::new();
    let ws = fixture.ws();
    push_execution_policy(CapabilityPolicy {
        workspace_roots: vec![std::fs::canonicalize(fixture.root.path())
            .unwrap()
            .display()
            .to_string()],
        side_effect_level: Some("network".to_string()),
        sandbox_profile: SandboxProfile::Worktree,
        process_sandbox: Box::new(ProcessSandboxPolicy {
            presets: Some(vec![
                ProcessSandboxPreset::SystemRuntime,
                ProcessSandboxPreset::DeveloperToolchains,
            ]),
            ..ProcessSandboxPolicy::default()
        }),
        ..CapabilityPolicy::default()
    });
    struct PopPolicy;
    impl Drop for PopPolicy {
        fn drop(&mut self) {
            pop_execution_policy();
        }
    }
    let _policy = PopPolicy;
    let confined = |words: &[&str]| {
        let args: Vec<String> = words.iter().map(|word| word.to_string()).collect();
        let spawn = crate::stdlib::process::granted_spawn("git", &args, &ws, &BTreeMap::new())
            .expect("no refusal")
            .expect("granted");
        let _launch = spawn.launching();
        let config = crate::stdlib::sandbox::ProcessCommandConfig {
            cwd: Some(ws.clone()),
            env: spawn.env.clone(),
            env_remove: spawn.remove.clone(),
            closed_env: true,
            ..Default::default()
        };
        crate::stdlib::sandbox::command_output(&spawn.program, &args, &config).expect("spawned")
    };
    assert_succeeded(&confined(&["fetch", "origin"]));
    ok(&ws, &["checkout", "-q", "-b", "confined", "origin/main"]);
    assert_succeeded(&confined(&["push", "origin", "HEAD:refs/heads/confined"]));
    let server = fixture.root.path().join("server/o/r.git");
    assert_eq!(
        ok(&server, &["rev-parse", "refs/heads/confined"]),
        ok(&ws, &["rev-parse", "HEAD"])
    );
}
