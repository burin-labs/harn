#![cfg(target_os = "macos")]

use crate::test_util::process::harn_e2e_command;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

struct Agent {
    child: Child,
    socket: PathBuf,
}

impl Agent {
    fn start(socket: PathBuf) -> Self {
        let child = Command::new("ssh-agent")
            .args(["-D", "-a"])
            .arg(&socket)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut agent = Self { child, socket };
        let mut ready = String::new();
        BufReader::new(agent.child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert!(ready.starts_with("SSH_AUTH_SOCK="), "agent startup failed");
        agent
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn git(repo: &Path, home: &Path, socket: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("SSH_AUTH_SOCK", socket)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn exact_agent_grant_signs_without_exposing_keys_or_other_sockets() {
    let temp = tempfile::Builder::new()
        .prefix("harnssh-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path().join("home");
    let keys = home.join(".ssh");
    let repo = temp.path().join("repo");
    std::fs::create_dir_all(&keys).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    let key = keys.join("id_ed25519");
    assert!(Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&key)
        .status()
        .unwrap()
        .success());
    let agent = Agent::start(temp.path().join("agent.sock"));
    let other = Agent::start(temp.path().join("other.sock"));
    assert!(Command::new("ssh-add")
        .arg(&key)
        .env("SSH_AUTH_SOCK", &agent.socket)
        .output()
        .unwrap()
        .status
        .success());
    let empty_agent = Command::new("ssh-add")
        .arg("-l")
        .env("SSH_AUTH_SOCK", &other.socket)
        .output()
        .unwrap();
    assert_eq!(
        empty_agent.status.code(),
        Some(1),
        "other agent must be reachable and empty"
    );
    git(&repo, &home, &agent.socket, &["init", "-q", "--template="]);
    for args in [
        ["config", "user.name", "Socket proof"],
        ["config", "user.email", "socket@example.invalid"],
        ["config", "gpg.format", "ssh"],
    ] {
        git(&repo, &home, &agent.socket, &args);
    }
    let public_key = std::fs::read_to_string(key.with_extension("pub")).unwrap();
    std::fs::write(repo.join("signer.pub"), &public_key).unwrap();
    std::fs::write(
        repo.join("allowed-signers"),
        format!("socket@example.invalid {public_key}"),
    )
    .unwrap();
    git(
        &repo,
        &home,
        &agent.socket,
        &["config", "user.signingkey", "signer.pub"],
    );
    git(
        &repo,
        &home,
        &agent.socket,
        &["config", "gpg.ssh.allowedSignersFile", "allowed-signers"],
    );
    git(
        &repo,
        &home,
        &agent.socket,
        &[
            "commit",
            "--allow-empty",
            "-S",
            "-m",
            "Host signing control",
        ],
    );
    git(&repo, &home, &agent.socket, &["verify-commit", "HEAD"]);
    std::fs::write(
        repo.join("probe.harn"),
        include_str!("../fixtures/scoped_ssh_agent.harn"),
    )
    .unwrap();
    for granted in [false, true, false] {
        assert!(
            key.is_file(),
            "the private-key read control must name an existing file"
        );
        let before = git(&repo, &home, &agent.socket, &["rev-parse", "HEAD"]);
        let mut command = harn_e2e_command();
        command
            .current_dir(&repo)
            .env("HOME", &home)
            .env("SSH_AUTH_SOCK", &agent.socket)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["run", "--standalone", "--environment-policy", "isolated"]);
        if granted {
            command.arg("--allow-ssh-agent");
        }
        let output = command
            .args(["probe.harn", "--"])
            .arg(&key)
            .arg(&other.socket)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let proof: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(proof["confinement_fired"], true, "{proof}");
        assert_eq!(proof["key_read_exit"], 1, "{proof}");
        assert_eq!(proof["key_read_denied"], true, "{proof}");
        assert_eq!(proof["other_agent_exit"], 2, "{proof}");
        assert_eq!(proof["other_agent_denied"], true, "{proof}");
        let after = git(&repo, &home, &agent.socket, &["rev-parse", "HEAD"]);
        if granted {
            assert_eq!(proof["sign_exit"], 0, "{proof}");
            assert_ne!(before, after);
            git(&repo, &home, &agent.socket, &["verify-commit", "HEAD"]);
        } else {
            assert_ne!(proof["sign_exit"], 0, "{proof}");
            assert_eq!(before, after);
        }
    }
}
