use std::fs;
use std::path::{Path, PathBuf};

use super::{find_harn_repo_root, DoctorCheck, DoctorStatus, ToolCheck};

/// Checks the load-bearing toolchain. Rust and Cargo block every code workflow
/// if missing; a repository checkout also requires its exact pinned compiler.
pub(super) fn check_toolchain() -> Vec<DoctorCheck> {
    const TOOLS: &[ToolCheck] = &[
        ToolCheck {
            id: "rustc",
            binary: "rustc",
            version_args: &["--version"],
            missing_status: DoctorStatus::Fail,
            install_hint:
                "https://rustup.rs (curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh)",
            docs_url: "https://www.rust-lang.org/tools/install",
            blocks: &["build", "test", "release", "publish"],
        },
        ToolCheck {
            id: "cargo",
            binary: "cargo",
            version_args: &["--version"],
            missing_status: DoctorStatus::Fail,
            install_hint: "https://rustup.rs",
            docs_url: "https://doc.rust-lang.org/cargo/",
            blocks: &["build", "test", "release", "publish"],
        },
    ];
    let mut checks: Vec<_> = TOOLS.iter().map(ToolCheck::run).collect();
    if std::env::var("HARN_ALLOW_TOOLCHAIN_MISMATCH").as_deref() != Ok("1") {
        let cwd = std::env::current_dir().unwrap_or_default();
        if let Some(repo) = find_harn_repo_root(&cwd) {
            enforce_repo_rustc_pin(&mut checks, &repo);
        }
    }
    checks
}

fn enforce_repo_rustc_pin(checks: &mut [DoctorCheck], repo: &Path) {
    let pin_path = repo.join("rust-toolchain.toml");
    let pinned = fs::read_to_string(&pin_path)
        .map_err(|error| format!("unable to read {}: {error}", pin_path.display()))
        .and_then(|text| {
            let parsed: toml::Value = toml::from_str(&text)
                .map_err(|error| format!("invalid {}: {error}", pin_path.display()))?;
            parsed
                .get("toolchain")
                .and_then(|toolchain| toolchain.get("channel"))
                .and_then(toml::Value::as_str)
                .filter(|channel| !channel.trim().is_empty())
                .map(str::to_string)
                .ok_or_else(|| format!("{} has no toolchain.channel", pin_path.display()))
        });
    let Some(rustc) = checks
        .iter_mut()
        .find(|check| check.id == "rustc" && check.status == DoctorStatus::Ok)
    else {
        return;
    };
    let pinned = match pinned {
        Ok(pinned) => pinned,
        Err(error) => {
            rustc.status = DoctorStatus::Fail;
            rustc.detail = error;
            return;
        }
    };
    let Some(resolved) = rustc.detail.split_whitespace().nth(1).map(str::to_string) else {
        rustc.status = DoctorStatus::Fail;
        rustc.detail = format!("could not read a version from `{}`", rustc.detail);
        return;
    };
    let rustc_path = which::which("rustc").unwrap_or_else(|_| PathBuf::from("rustc"));
    apply_rustc_pin(rustc, &pinned, &resolved, &rustc_path);
}

fn apply_rustc_pin(check: &mut DoctorCheck, pinned: &str, resolved: &str, rustc_path: &Path) {
    if pinned == resolved {
        return;
    }
    check.status = DoctorStatus::Fail;
    check.detail = format!(
        "rust-toolchain.toml pins rustc {pinned} but {} resolves to {resolved}",
        rustc_path.display()
    );
    check.fix_command = Some("put the rustup shim directory ahead of other Rust installs on PATH, then rerun harn doctor".to_string());
}

#[cfg(test)]
mod tests {
    use super::apply_rustc_pin;
    use crate::commands::doctor::{DoctorCheck, DoctorStatus};
    use std::path::Path;

    #[test]
    fn mismatch_is_a_failing_doctor_check() {
        let mut check = DoctorCheck {
            id: "rustc".to_string(),
            status: DoctorStatus::Ok,
            detail: "rustc 1.98.0 (example)".to_string(),
            blocks: vec!["build", "test"],
            ..Default::default()
        };
        apply_rustc_pin(
            &mut check,
            "1.95.0",
            "1.98.0",
            Path::new("/toolchain/bin/rustc"),
        );

        assert_eq!(check.status, DoctorStatus::Fail);
        assert!(check.detail.contains("pins rustc 1.95.0"));
        assert!(check.detail.contains("resolves to 1.98.0"));
        assert_eq!(check.blocks, vec!["build", "test"]);
    }

    #[test]
    fn matching_pin_remains_ok() {
        let mut check = DoctorCheck {
            id: "rustc".to_string(),
            status: DoctorStatus::Ok,
            detail: "rustc 1.95.0 (example)".to_string(),
            ..Default::default()
        };
        apply_rustc_pin(&mut check, "1.95.0", "1.95.0", Path::new("rustc"));
        assert_eq!(check.status, DoctorStatus::Ok);
    }
}
