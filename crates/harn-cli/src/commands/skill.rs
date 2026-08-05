use std::path::Path;
use std::process;

use crate::cli::{
    SkillEndorseArgs, SkillKeyGenerateArgs, SkillSignArgs, SkillTrustAddArgs, SkillTrustListArgs,
    SkillVerifyArgs, SkillWhoSignedArgs,
};
use crate::skill_provenance;

use serde::Serialize;

pub(crate) fn run_key_generate(args: &SkillKeyGenerateArgs) {
    match skill_provenance::generate_keypair(Path::new(&args.out)) {
        Ok(outcome) => {
            println!("private_key: {}", outcome.private_key_path.display());
            println!("public_key: {}", outcome.public_key_path.display());
            println!("fingerprint: {}", outcome.fingerprint);
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub(crate) fn run_sign(args: &SkillSignArgs) {
    match skill_provenance::sign_skill(Path::new(&args.skill), Path::new(&args.key)) {
        Ok(outcome) => {
            println!("signature: {}", outcome.signature_path.display());
            println!("fingerprint: {}", outcome.signer_fingerprint);
            println!("skill_sha256: {}", outcome.skill_sha256);
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub(crate) fn run_endorse(args: &SkillEndorseArgs) {
    match skill_provenance::endorse_skill(Path::new(&args.skill), Path::new(&args.key)) {
        Ok(outcome) => {
            println!("signature: {}", outcome.signature_path.display());
            println!("endorser_fingerprint: {}", outcome.endorser_fingerprint);
            println!("skill_sha256: {}", outcome.skill_sha256);
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub(crate) fn run_verify(args: &SkillVerifyArgs) {
    let registry_url = skill_provenance::configured_registry_url(Some(Path::new(&args.skill)));
    let options = skill_provenance::VerifyOptions {
        registry_url,
        ..Default::default()
    };
    match skill_provenance::verify_skill(Path::new(&args.skill), &options) {
        Ok(report) if report.is_verified() => {
            if args.json {
                print_json(&verification_output(report, Vec::new()));
            } else {
                print_verified_report(&report);
            }
        }
        Ok(report) => {
            if args.json {
                print_json(&verification_output(report.clone(), Vec::new()));
            }
            eprintln!("error: {}", report.human_summary());
            process::exit(1);
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub(crate) async fn run_who_signed(args: &SkillWhoSignedArgs) {
    let registry_url = skill_provenance::configured_registry_url(Some(Path::new(&args.skill)));
    let options = skill_provenance::VerifyOptions {
        registry_url,
        ..Default::default()
    };
    match skill_provenance::verify_skill(Path::new(&args.skill), &options) {
        Ok(report) => {
            let scores = signer_scores(&report).await;
            let output = verification_output(report, scores);
            if args.json {
                print_json(&output);
            } else {
                print_who_signed_report(&output);
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

pub(crate) fn run_trust_add(args: &SkillTrustAddArgs) {
    match skill_provenance::trust_add(&args.from) {
        Ok(record) => {
            println!("fingerprint: {}", record.fingerprint);
            println!("path: {}", record.path.display());
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}

#[derive(Debug, Serialize)]
struct SkillVerificationOutput {
    skill_path: String,
    signature_path: String,
    skill_sha256: String,
    status: &'static str,
    signed: bool,
    trusted: bool,
    author: Option<SignerOutput>,
    endorsements: Vec<SignerOutput>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct SignerOutput {
    role: &'static str,
    fingerprint: String,
    signed_at: Option<String>,
    trusted: bool,
    status: &'static str,
    error: Option<String>,
    trust_score: Option<harn_vm::TrustScore>,
}

fn verification_output(
    report: skill_provenance::VerificationReport,
    mut scores: Vec<(String, harn_vm::TrustScore)>,
) -> SkillVerificationOutput {
    let mut score_for = |fingerprint: &str| {
        scores
            .iter()
            .position(|(candidate, _)| candidate == fingerprint)
            .map(|index| scores.remove(index).1)
    };
    let author = report.signer_fingerprint.as_deref().map(|fingerprint| {
        let author_verified = report.status == skill_provenance::VerificationStatus::Verified
            || report.status == skill_provenance::VerificationStatus::MissingEndorsement
            || !report.endorsements.is_empty();
        SignerOutput {
            role: "author",
            fingerprint: fingerprint.to_string(),
            signed_at: report.signed_at.clone(),
            trusted: author_verified,
            status: if author_verified {
                skill_provenance::VerificationStatus::Verified.as_str()
            } else {
                report.status.as_str()
            },
            error: None,
            trust_score: score_for(fingerprint),
        }
    });
    let endorsements = report
        .endorsements
        .iter()
        .map(|endorsement| SignerOutput {
            role: "endorser",
            fingerprint: endorsement.endorser_fingerprint.clone(),
            signed_at: Some(endorsement.signed_at.clone()),
            trusted: endorsement.trusted,
            status: endorsement.status.as_str(),
            error: endorsement.error.clone(),
            trust_score: score_for(&endorsement.endorser_fingerprint),
        })
        .collect();
    SkillVerificationOutput {
        skill_path: report.skill_path.display().to_string(),
        signature_path: report.signature_path.display().to_string(),
        skill_sha256: report.skill_sha256,
        status: report.status.as_str(),
        signed: report.signed,
        trusted: report.trusted,
        author,
        endorsements,
        error: report.error,
    }
}

async fn signer_scores(
    report: &skill_provenance::VerificationReport,
) -> Vec<(String, harn_vm::TrustScore)> {
    let mut fingerprints = Vec::new();
    if let Some(fingerprint) = report.signer_fingerprint.as_deref() {
        fingerprints.push(fingerprint.to_string());
    }
    fingerprints.extend(
        report
            .endorsements
            .iter()
            .map(|endorsement| endorsement.endorser_fingerprint.clone()),
    );
    fingerprints.sort();
    fingerprints.dedup();
    let Ok(log) = open_trust_log() else {
        return Vec::new();
    };
    let mut scores = Vec::new();
    for fingerprint in fingerprints {
        if let Ok(score) =
            harn_vm::trust_score_for(&log, &fingerprint, Some("skill.provenance")).await
        {
            scores.push((fingerprint, score));
        }
    }
    scores
}

fn open_trust_log() -> Result<std::sync::Arc<harn_vm::event_log::AnyEventLog>, String> {
    harn_vm::reset_thread_local_state();
    let cwd = std::env::current_dir().map_err(|error| format!("failed to read cwd: {error}"))?;
    let workspace_root = harn_vm::stdlib::process::find_project_root(&cwd).unwrap_or(cwd);
    harn_vm::event_log::install_default_for_base_dir(&workspace_root)
        .map_err(|error| format!("failed to open event log: {error}"))
}

fn print_verified_report(report: &skill_provenance::VerificationReport) {
    println!("verified: {}", report.skill_path.display());
    println!(
        "author_fingerprint: {}",
        report.signer_fingerprint.clone().unwrap_or_default()
    );
    for endorsement in &report.endorsements {
        println!("endorser_fingerprint: {}", endorsement.endorser_fingerprint);
    }
    println!("skill_sha256: {}", report.skill_sha256);
}

fn print_who_signed_report(output: &SkillVerificationOutput) {
    println!("skill: {}", output.skill_path);
    println!("status: {}", output.status);
    println!("trusted: {}", output.trusted);
    if let Some(author) = &output.author {
        println!(
            "author: {} trusted={} score={}",
            author.fingerprint,
            author.trusted,
            format_score(author.trust_score.as_ref())
        );
    }
    for endorsement in &output.endorsements {
        println!(
            "endorsement: {} trusted={} status={} score={}",
            endorsement.fingerprint,
            endorsement.trusted,
            endorsement.status,
            format_score(endorsement.trust_score.as_ref())
        );
    }
}

fn format_score(score: Option<&harn_vm::TrustScore>) -> String {
    score
        .map(|score| {
            format!(
                "{:.3} ({}/{})",
                score.success_rate, score.successes, score.total
            )
        })
        .unwrap_or_else(|| "n/a".to_string())
}

fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(rendered) => println!("{rendered}"),
        Err(error) => {
            eprintln!("error: failed to encode JSON: {error}");
            process::exit(1);
        }
    }
}

pub(crate) fn run_trust_list(_args: &SkillTrustListArgs) {
    match skill_provenance::trust_list() {
        Ok(records) => {
            if records.is_empty() {
                println!("No trusted skill signers installed.");
                return;
            }
            for record in records {
                println!("{} {}", record.fingerprint, record.path.display());
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            process::exit(1);
        }
    }
}
