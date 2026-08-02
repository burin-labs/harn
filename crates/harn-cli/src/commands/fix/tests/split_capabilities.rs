use super::*;

#[test]
fn apply_preserves_separate_narrow_capability_parameters() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("split_capabilities.harn");
    let source = "fn read_key(secrets: HarnessSecrets) {\n  secrets.read(\"app-jwt\", nil)\n}\n\npub fn mint_app_jwt(clock: HarnessClock, secrets: HarnessSecrets, config) {\n  const issued_at = clock.now_ms()\n  return {issued_at: issued_at, key: read_key(secrets), config: config}\n}\n\nfn main(harness: Harness) {\n  mint_app_jwt(harness.clock, harness.secrets, {})\n}\n";
    fs::write(&script, source).unwrap();

    for pass in 1..=2 {
        let result = apply_repairs_with_options(
            &script,
            RepairSafety::SurfaceChanging,
            false,
            FixOptions {
                capability_migrations_only: true,
            },
        )
        .unwrap();
        assert!(result.applied.is_empty(), "pass {pass}: {result:#?}");
        assert_eq!(
            fs::read_to_string(&script).unwrap(),
            source,
            "pass {pass} must preserve an already-split capability boundary"
        );
    }
}

#[test]
fn apply_rewrites_ambient_call_through_root_first_split_boundary() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("root_first_split.harn");
    fs::write(
        &script,
        "fn helper(harness: Harness, clock: HarnessClock) {\n  println(\"hi\")\n  now_ms()\n}\n\nfn main(harness: Harness) {\n  helper(harness, harness.clock)\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert!(!result.applied.is_empty(), "{result:#?}");

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains("fn helper(harness: Harness, clock: HarnessClock)"),
        "the split signature must remain intact: {updated}"
    );
    assert!(
        updated.contains("harness.stdio.println(\"hi\")"),
        "the root carrier should own the ambient rewrite: {updated}"
    );
    assert!(
        updated.contains("clock.now_ms()") && !updated.contains("harness.clock.now_ms()"),
        "an existing narrow carrier should take precedence over root authority: {updated}"
    );
}

#[test]
fn apply_extends_narrow_first_split_boundary_with_missing_handle() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("narrow_first_split.harn");
    fs::write(
        &script,
        "fn helper(clock: HarnessClock, secrets: HarnessSecrets, config) {\n  println(\"hi\")\n  return {time: clock.now_ms(), key: secrets.read(\"key\", nil), config: config}\n}\n\nfn main(harness: Harness) {\n  helper(harness.clock, harness.secrets, {})\n}\n",
    )
    .unwrap();

    let first = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert!(!first.applied.is_empty(), "{first:#?}");

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated.contains(
            "fn helper(clock: HarnessClock, secrets: HarnessSecrets, stdio: HarnessStdio, config)"
        ),
        "the split boundary should gain only the missing handle: {updated}"
    );
    assert!(
        updated.contains("stdio.println(\"hi\")"),
        "the new narrow handle should own the ambient rewrite: {updated}"
    );
    assert!(
        updated.contains("helper(harness.clock, harness.secrets, harness.stdio, {})"),
        "the caller should project the added handle at the matching position: {updated}"
    );

    let second = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert!(second.applied.is_empty(), "{second:#?}");
}

#[test]
fn apply_preserves_lone_narrow_handle_when_adding_another_capability() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("lone_narrow_handle.harn");
    fs::write(
        &script,
        "fn helper(clock: HarnessClock, value) {\n  const day = clock.date_iso()\n  const token = harness.secrets.get(\"token\")\n  return [day, token, value]\n}\n\nfn main(harness: Harness) {\n  helper(harness.clock, \"input\")\n}\n",
    )
    .unwrap();

    let first = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert!(!first.applied.is_empty(), "{first:#?}");

    let updated = fs::read_to_string(&script).unwrap();
    assert_eq!(
        updated,
        "fn helper(clock: HarnessClock, secrets: HarnessSecrets, value) {\n  const day = clock.date_iso()\n  const token = secrets.get(\"token\")\n  return [day, token, value]\n}\n\nfn main(harness: Harness) {\n  helper(harness.clock, harness.secrets, \"input\")\n}\n",
    );

    let second = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert!(second.applied.is_empty(), "{second:#?}");
}

#[test]
fn apply_does_not_guess_domain_argument_after_lone_narrow_handle() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("lone_narrow_domain_gap.harn");
    fs::write(
        &script,
        "fn helper(clock: HarnessClock, value) {\n  const token = harness.secrets.get(\"token\")\n  return [clock.date_iso(), token, value]\n}\n\nfn main(harness: Harness) {\n  helper(harness.clock)\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert!(!result.applied.is_empty(), "{result:#?}");

    assert_eq!(
        fs::read_to_string(&script).unwrap(),
        "fn helper(clock: HarnessClock, secrets: HarnessSecrets, value) {\n  const token = secrets.get(\"token\")\n  return [clock.date_iso(), token, value]\n}\n\nfn main(harness: Harness) {\n  helper(harness.clock, harness.secrets)\n}\n",
        "the fixer must migrate authority without inventing the missing domain value",
    );
}

#[test]
fn apply_completes_split_capability_prefix_before_extension() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("partial_split_call.harn");
    fs::write(
        &script,
        "fn mint(clock: HarnessClock, secrets: HarnessSecrets) {\n  println(\"hi\")\n  return {time: clock.now_ms(), key: secrets.read(\"key\", nil)}\n}\n\nfn main(harness: Harness) {\n  mint(harness.clock)\n}\n",
    )
    .unwrap();

    let first = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert!(!first.applied.is_empty(), "{first:#?}");

    let updated = fs::read_to_string(&script).unwrap();
    assert!(
        updated
            .contains("fn mint(clock: HarnessClock, secrets: HarnessSecrets, stdio: HarnessStdio)"),
        "the split signature should gain the missing handle: {updated}"
    );
    assert!(
        updated.contains("mint(harness.clock, harness.secrets, harness.stdio)"),
        "the partial call should converge in parameter order: {updated}"
    );

    let second = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert!(second.applied.is_empty(), "{second:#?}");
}

#[test]
fn apply_defers_extension_across_missing_ordinary_parameter() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("ordinary_gap.harn");
    fs::write(
        &script,
        "fn mint(clock: HarnessClock, config, secrets: HarnessSecrets) {\n  println(\"hi\")\n  return {time: clock.now_ms(), key: secrets.read(\"key\", nil), config: config}\n}\n\nfn main(harness: Harness) {\n  mint(harness.clock)\n}\n",
    )
    .unwrap();

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert!(!result.applied.is_empty(), "{result:#?}");

    let updated = fs::read_to_string(&script).unwrap();
    assert_eq!(
        updated,
        "fn mint(clock: HarnessClock, config, secrets: HarnessSecrets, stdio: HarnessStdio) {\n  stdio.println(\"hi\")\n  return {time: clock.now_ms(), key: secrets.read(\"key\", nil), config: config}\n}\n\nfn main(harness: Harness) {\n  mint(harness.clock)\n}\n",
        "the signature and ambient call should migrate without guessing the missing config value"
    );
}

#[test]
fn apply_does_not_widen_narrow_caller_of_split_boundary() {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("split_callee.harn");
    let source = "fn mint(clock: HarnessClock, secrets: HarnessSecrets) {\n  return {time: clock.now_ms(), key: secrets.read(\"key\", nil)}\n}\n\nfn wrap(clock: HarnessClock, secrets) {\n  mint(clock, secrets)\n}\n\nfn main(harness: Harness) {\n  wrap(harness.clock, harness.secrets)\n}\n";
    fs::write(&script, source).unwrap();

    let result = apply_repairs_with_options(
        &script,
        RepairSafety::SurfaceChanging,
        false,
        FixOptions {
            capability_migrations_only: true,
        },
    )
    .unwrap();
    assert!(result.applied.is_empty(), "{result:#?}");
    assert_eq!(fs::read_to_string(&script).unwrap(), source);
}
