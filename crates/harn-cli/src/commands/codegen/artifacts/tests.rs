use super::*;

fn artifact(path: &str, content: &str) -> GeneratedArtifact {
    GeneratedArtifact {
        path: path.into(),
        content: content.into(),
        source_mappings: Vec::new(),
    }
}

fn identity() -> CodegenIdentity {
    CodegenIdentity {
        catalog_digest: "sha256:catalog".into(),
        binding_digest: "sha256:binding".into(),
        ir_digest: "sha256:ir".into(),
    }
}

#[test]
fn unsafe_and_aliasing_plugin_paths_are_refused_before_publication() {
    for name in [
        "",
        "/absolute",
        "../escape",
        "a/../escape",
        "a//b",
        "a/./b",
        "C:escape",
        "a\\b",
        ".harn-codegen.json",
        ".HARN-CODEGEN.JSON",
        ".harn-codegen.json/child",
        "a\nb",
    ] {
        assert_eq!(
            GeneratedArtifactSet::prepare(vec![artifact(name, "content")])
                .unwrap_err()
                .code,
            Code::UnsafeArtifactPath,
            "{name:?}"
        );
    }
    for paths in [
        ["same", "same"],
        ["Case", "case"],
        ["tree", "tree/child"],
        ["TREE/child", "tree"],
    ] {
        assert_eq!(
            GeneratedArtifactSet::prepare(
                paths
                    .into_iter()
                    .map(|name| artifact(name, "content"))
                    .collect()
            )
            .unwrap_err()
            .code,
            Code::DuplicateArtifactPath
        );
    }
}

#[test]
fn publication_checks_drift_and_removes_only_unmodified_owned_files() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("generated");
    let first = GeneratedArtifactSet::prepare(vec![
        artifact("nested/old.ts", "old"),
        artifact("keep.ts", "keep"),
    ])
    .unwrap();
    assert_eq!(
        first
            .publish(&root, &identity(), CodegenRole::Both, true)
            .unwrap_err()
            .code,
        Code::ArtifactDrift
    );
    assert!(!root.exists(), "check does not create the output directory");
    first
        .publish(&root, &identity(), CodegenRole::Both, false)
        .unwrap();
    first
        .publish(&root, &identity(), CodegenRole::Both, true)
        .unwrap();
    fs::write(root.join("handwritten.ts"), "retain").unwrap();
    let second = GeneratedArtifactSet::prepare(vec![
        artifact("next.ts", "next"),
        artifact("keep.ts", "keep"),
    ])
    .unwrap();
    assert_eq!(
        second
            .publish(&root, &identity(), CodegenRole::Both, true)
            .unwrap_err()
            .code,
        Code::ArtifactDrift
    );
    assert_eq!(
        fs::read_to_string(root.join("nested/old.ts")).unwrap(),
        "old"
    );
    second
        .publish(&root, &identity(), CodegenRole::Both, false)
        .unwrap();
    assert!(!root.join("nested/old.ts").exists());
    assert_eq!(
        fs::read_to_string(root.join("handwritten.ts")).unwrap(),
        "retain"
    );
    second
        .publish(&root, &identity(), CodegenRole::Both, true)
        .unwrap();
}

#[test]
fn conflicting_unowned_or_modified_files_preserve_every_preimage() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("z.ts"), "handwritten").unwrap();
    let next =
        GeneratedArtifactSet::prepare(vec![artifact("a.ts", "new"), artifact("z.ts", "generated")])
            .unwrap();
    let drift = next
        .publish(root, &identity(), CodegenRole::Both, true)
        .unwrap_err();
    assert_eq!(drift.code, Code::ArtifactDrift);
    assert_eq!(drift.structural_path, "z.ts");
    assert_eq!(
        next.publish(root, &identity(), CodegenRole::Both, false)
            .unwrap_err()
            .code,
        Code::ArtifactWriteFailed
    );
    assert!(
        !root.join("a.ts").exists(),
        "all preimages checked before first write"
    );
    assert_eq!(
        fs::read_to_string(root.join("z.ts")).unwrap(),
        "handwritten"
    );
    let owned = GeneratedArtifactSet::prepare(vec![artifact("owned.ts", "original")]).unwrap();
    owned
        .publish(root, &identity(), CodegenRole::Both, false)
        .unwrap();
    let manifest = fs::read(root.join(MANIFEST)).unwrap();
    fs::write(root.join("owned.ts"), "manual edit").unwrap();
    let replacement = GeneratedArtifactSet::prepare(vec![artifact("a.ts", "new")]).unwrap();
    let drift = replacement
        .publish(root, &identity(), CodegenRole::Both, true)
        .unwrap_err();
    assert_eq!(drift.code, Code::ArtifactDrift);
    assert_eq!(drift.structural_path, "owned.ts");
    assert_eq!(
        replacement
            .publish(root, &identity(), CodegenRole::Both, false)
            .unwrap_err()
            .code,
        Code::ArtifactWriteFailed
    );
    assert!(!root.join("a.ts").exists());
    assert_eq!(
        fs::read_to_string(root.join("owned.ts")).unwrap(),
        "manual edit"
    );
    assert_eq!(fs::read(root.join(MANIFEST)).unwrap(), manifest);
}

#[cfg(unix)]
#[test]
fn symlinked_destinations_and_parent_directories_cannot_escape_output() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("generated");
    let outside = temp.path().join("outside");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("target.ts"), "outside").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("nested")).unwrap();
    let nested =
        GeneratedArtifactSet::prepare(vec![artifact("nested/target.ts", "overwrite")]).unwrap();
    assert_eq!(
        nested
            .publish(&root, &identity(), CodegenRole::Both, false)
            .unwrap_err()
            .code,
        Code::UnsafeArtifactPath
    );
    std::os::unix::fs::symlink(outside.join("target.ts"), root.join("alias.ts")).unwrap();
    let alias = GeneratedArtifactSet::prepare(vec![artifact("alias.ts", "overwrite")]).unwrap();
    assert_eq!(
        alias
            .publish(&root, &identity(), CodegenRole::Both, false)
            .unwrap_err()
            .code,
        Code::UnsafeArtifactPath
    );
    assert_eq!(
        fs::read_to_string(outside.join("target.ts")).unwrap(),
        "outside"
    );
    assert!(!root.join(MANIFEST).exists());
}
