use std::process::Command;

use ed25519_dalek::SigningKey;
use harn_vm::flow::{Atom, Provenance, ShadowGitBackend, TextOp, VcsBackend};
use tempfile::TempDir;
use time::OffsetDateTime;

fn git_available() -> bool {
    Command::new("git").arg("--version").output().is_ok()
}

fn run_git(root: &TempDir, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(root.path())
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_PREFIX")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_repo() -> TempDir {
    let repo = tempfile::tempdir().expect("tempdir");
    run_git(&repo, &["init"]);
    repo
}

fn signing_key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn atom(index: usize, parent: Option<harn_vm::flow::AtomId>) -> Atom {
    let principal = signing_key(1);
    let persona = signing_key(2);
    Atom::sign(
        vec![TextOp::Insert {
            offset: index as u64,
            content: format!("atom-{index}\n"),
        }],
        parent.into_iter().collect(),
        Provenance {
            principal: "user:alice".to_string(),
            persona: "ship-captain".to_string(),
            agent_run_id: "run-flow-backend".to_string(),
            tool_call_id: Some(format!("tool-{index}")),
            trace_id: format!("trace-{index}"),
            transcript_ref: "transcript:test".to_string(),
            timestamp: OffsetDateTime::from_unix_timestamp(1_777_000_000 + index as i64).unwrap(),
        },
        None,
        &principal,
        &persona,
    )
    .expect("sign atom")
}

#[test]
fn shadow_git_emits_atoms_as_sidecar_commits() {
    if !git_available() {
        return;
    }

    let repo = init_repo();
    let backend = ShadowGitBackend::new(repo.path()).expect("backend");
    let mut atoms = Vec::new();
    let mut parent = None;

    for index in 0..10 {
        let next = atom(index, parent);
        let atom_ref = backend.emit_atom(&next).expect("emit atom");
        assert_eq!(atom_ref.atom_id, next.id);
        assert_eq!(atom_ref.ref_name, format!("refs/flow/atoms/{}", next.id));
        let commit = run_git(
            &repo,
            &[
                "rev-parse",
                &format!("refs/flow/atoms/{}^{{commit}}", next.id),
            ],
        );
        assert_eq!(commit, atom_ref.commit);
        let payload = run_git(&repo, &["show", &format!("{commit}:atom.json")]);
        let decoded = Atom::from_json_slice(payload.as_bytes()).expect("atom payload");
        assert_eq!(decoded.id, next.id);
        parent = Some(next.id);
        atoms.push(next);
    }

    let listed = backend.list_atoms().expect("list atoms");
    assert_eq!(listed.len(), 10);

    let atom_ids = atoms.iter().map(|atom| atom.id).collect::<Vec<_>>();
    let slice = backend
        .derive_slice(&[*atom_ids.last().expect("ten atom ids")])
        .expect("derive slice");
    assert_eq!(slice.atoms, atom_ids);
    let replayed = backend.replay_slice(&slice).expect("replay slice");
    assert_eq!(
        replayed.iter().map(|atom| atom.id).collect::<Vec<_>>(),
        atom_ids
    );

    let receipt = backend.ship_slice(&slice).expect("ship slice");
    assert_eq!(receipt.ref_name, format!("refs/flow/slices/{}", slice.id));
    let exported = backend
        .export_git(&slice, "refs/heads/flow-export")
        .expect("export git");
    assert_eq!(exported.commit, receipt.commit);

    let imported = backend
        .import_git("refs/heads/flow-export")
        .expect("import git");
    assert_eq!(imported.atoms, atom_ids);
}
