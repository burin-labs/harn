use std::path::Path;
use std::process;

use crate::cli::{
    SkillKeyGenerateArgs, SkillSignArgs, SkillTrustAddArgs, SkillTrustListArgs, SkillVerifyArgs,
};
use crate::skill_provenance;

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

pub(crate) fn run_verify(args: &SkillVerifyArgs) {
    let registry_url = skill_provenance::configured_registry_url(Some(Path::new(&args.skill)));
    let options = skill_provenance::VerifyOptions {
        registry_url,
        ..Default::default()
    };
    match skill_provenance::verify_skill(Path::new(&args.skill), &options) {
        Ok(report) if report.is_verified() => {
            println!("verified: {}", report.skill_path.display());
            println!(
                "fingerprint: {}",
                report.signer_fingerprint.unwrap_or_default()
            );
            println!("skill_sha256: {}", report.skill_sha256);
        }
        Ok(report) => {
            eprintln!("error: {}", report.human_summary());
            process::exit(1);
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
