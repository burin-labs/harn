use serde::{Deserialize, Serialize};
use std::{fs, path::Path, process};

const COMMAND: &str = "__internal-source-gate-receipt-v1";
const SCHEMA: &str = "harn.source_gate_receipt.v1";

pub(super) fn handle(raw_args: &[String]) -> bool {
    let Some(command) = raw_args.get(1).filter(|value| value.as_str() == COMMAND) else {
        return false;
    };
    let result = match raw_args.get(2).map(String::as_str) {
        Some("write") => write_from_args(&raw_args[3..]),
        Some("verify") => verify_from_args(&raw_args[3..]),
        _ => Err(format!(
            "usage: {command} {{write <receipt> <head> <remote-pr-head-or-dash> <binary> <build-freshness> <runtime-mode> <subtask-placement> <audit-jobs> <conformance-jobs> <terminal-utc> <summary> -- <gate-command...>|verify <receipt> <head> <binary> <build-freshness>}}"
        )),
    };
    if let Err(error) = result {
        eprintln!("error: {error}");
        process::exit(1);
    }
    true
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourceGateReceipt {
    schema: String,
    git: GitEvidence,
    binary: BinaryEvidence,
    runtime: RuntimeEvidence,
    gate: GateEvidence,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GitEvidence {
    head: String,
    clean_before: CleanEvidence,
    clean_after: CleanEvidence,
    remote_pr_head: Option<String>,
    matches_remote_pr_head: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CleanEvidence {
    tracked: bool,
    index: bool,
    untracked: bool,
}

impl CleanEvidence {
    fn clean() -> Self {
        Self {
            tracked: true,
            index: true,
            untracked: true,
        }
    }

    fn is_clean(&self) -> bool {
        self.tracked && self.index && self.untracked
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BinaryEvidence {
    path: String,
    build_freshness_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimeEvidence {
    mode: String,
    subtask_placement: String,
    audit_jobs: Option<u32>,
    conformance_jobs: Option<u32>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GateEvidence {
    command: Vec<String>,
    terminal_utc: String,
    summary: String,
}

fn write_from_args(args: &[String]) -> Result<(), String> {
    let separator = args
        .iter()
        .position(|value| value == "--")
        .ok_or_else(|| "source gate receipt write is missing `--`".to_owned())?;
    if separator != 11 || args.len() == separator + 1 {
        return Err("source gate receipt write has the wrong argument shape".into());
    }
    let receipt = SourceGateReceipt {
        schema: SCHEMA.into(),
        git: GitEvidence {
            head: full_object_id(&args[1], "Git head")?,
            clean_before: CleanEvidence::clean(),
            clean_after: CleanEvidence::clean(),
            remote_pr_head: optional_object_id(&args[2])?,
            matches_remote_pr_head: (args[2] != "-").then_some(args[1] == args[2]),
        },
        binary: BinaryEvidence {
            path: args[3].clone(),
            build_freshness_id: full_object_id(&args[4], "build freshness ID")?,
        },
        runtime: RuntimeEvidence {
            mode: nonempty(&args[5], "runtime mode")?,
            subtask_placement: nonempty(&args[6], "subtask placement")?,
            audit_jobs: optional_positive_u32(&args[7], "audit jobs")?,
            conformance_jobs: optional_positive_u32(&args[8], "conformance jobs")?,
        },
        gate: GateEvidence {
            terminal_utc: nonempty(&args[9], "terminal timestamp")?,
            summary: nonempty(&args[10], "summary")?,
            command: args[separator + 1..].to_vec(),
        },
    };
    if receipt.git.matches_remote_pr_head == Some(false) {
        return Err("tested Git head does not match the named remote PR head".into());
    }
    let output = Path::new(&args[0]);
    let parent = output
        .parent()
        .ok_or_else(|| "source gate receipt path has no parent".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "cannot create receipt directory {}: {error}",
            parent.display()
        )
    })?;
    let temporary = output.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&receipt)
        .map_err(|error| format!("cannot encode source gate receipt: {error}"))?;
    fs::write(&temporary, bytes).map_err(|error| {
        format!(
            "cannot write source gate receipt {}: {error}",
            temporary.display()
        )
    })?;
    fs::rename(&temporary, output).map_err(|error| {
        format!(
            "cannot publish source gate receipt {}: {error}",
            output.display()
        )
    })
}

fn verify_from_args(args: &[String]) -> Result<(), String> {
    let [receipt, head, binary, build_freshness] = args else {
        return Err("source gate receipt verify has the wrong argument shape".into());
    };
    let bytes = fs::read(receipt)
        .map_err(|error| format!("cannot read source gate receipt {receipt}: {error}"))?;
    let parsed: SourceGateReceipt = serde_json::from_slice(&bytes)
        .map_err(|error| format!("malformed source gate receipt {receipt}: {error}"))?;
    validate(&parsed)?;
    if parsed.git.head != *head {
        return Err(format!(
            "source gate receipt tested {}, but checkout is {head}",
            parsed.git.head
        ));
    }
    if parsed.binary.path != *binary {
        return Err("source gate receipt names a different Harn binary".into());
    }
    if parsed.binary.build_freshness_id != *build_freshness {
        return Err("source gate receipt names a different Harn build freshness ID".into());
    }
    Ok(())
}

fn validate(receipt: &SourceGateReceipt) -> Result<(), String> {
    if receipt.schema != SCHEMA
        || !receipt.git.clean_before.is_clean()
        || !receipt.git.clean_after.is_clean()
    {
        return Err("source gate receipt does not certify one clean source state".into());
    }
    full_object_id(&receipt.git.head, "Git head")?;
    full_object_id(&receipt.binary.build_freshness_id, "build freshness ID")?;
    if receipt.git.remote_pr_head.is_some() != receipt.git.matches_remote_pr_head.is_some()
        || receipt.git.matches_remote_pr_head == Some(false)
        || receipt.gate.command.is_empty()
        || (receipt.runtime.audit_jobs.is_none() && receipt.runtime.conformance_jobs.is_none())
    {
        return Err("source gate receipt contains inconsistent evidence".into());
    }
    Ok(())
}

fn optional_object_id(value: &str) -> Result<Option<String>, String> {
    (value != "-")
        .then(|| full_object_id(value, "remote PR head"))
        .transpose()
}

fn full_object_id(value: &str, label: &str) -> Result<String, String> {
    if matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(value.to_owned())
    } else {
        Err(format!("{label} must be a full lowercase object ID"))
    }
}

fn positive_u32(value: &str, label: &str) -> Result<u32, String> {
    value
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("{label} must be a positive integer"))
}

fn optional_positive_u32(value: &str, label: &str) -> Result<Option<u32>, String> {
    (value != "-")
        .then(|| positive_u32(value, label))
        .transpose()
}

fn nonempty(value: &str, label: &str) -> Result<String, String> {
    (!value.is_empty())
        .then(|| value.to_owned())
        .ok_or_else(|| format!("{label} must not be empty"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_validation_rejects_a_stale_remote_head() {
        let receipt = SourceGateReceipt {
            schema: SCHEMA.into(),
            git: GitEvidence {
                head: "a".repeat(40),
                clean_before: CleanEvidence::clean(),
                clean_after: CleanEvidence::clean(),
                remote_pr_head: Some("b".repeat(40)),
                matches_remote_pr_head: Some(false),
            },
            binary: BinaryEvidence {
                path: "/tmp/harn".into(),
                build_freshness_id: "c".repeat(40),
            },
            runtime: RuntimeEvidence {
                mode: "full_io".into(),
                subtask_placement: "worker".into(),
                audit_jobs: Some(4),
                conformance_jobs: Some(4),
            },
            gate: GateEvidence {
                command: vec!["make".into(), "conformance".into()],
                terminal_utc: "2026-08-25T00:00:00Z".into(),
                summary: "passed".into(),
            },
        };
        assert!(validate(&receipt).is_err());
    }
}
