//! `@host_entry` freezes a signature the fixer cannot see the caller of.
//!
//! Every other runtime boundary is recognizable from something Harn owns — a
//! name, a trigger signature, a `handler:` field, a package manifest. A
//! function an embedding Rust host reaches through the runtime's
//! call-into-script path has none of those, so the body was the only evidence
//! and the migration narrowed the signature to whatever the body touched
//! (#6193).

use super::*;

/// The signature of `dispatch_audit_exports` in harn-cloud, verbatim in shape.
///
/// It uses exactly TWO capabilities, which is the case that broke: at one the
/// carrier ladder proposes a narrow handle, at three or more it keeps root
/// `Harness`, and only at two does it propose `{net: HarnessNet, postgres:
/// HarnessPostgres}` — a record the host bridge has no way to construct, so
/// the rewrite failed at dispatch rather than at `harn check`.
const TWO_CAPABILITY_DISPATCH: &str = concat!(
    "pub fn dispatch_audit_exports(harness: Harness, args: dict) -> dict {\n",
    "  const rows = harness.postgres.query(args.database_url, \"select 1\")\n",
    "  harness.net.post(args.sink_url, rows)\n",
    "  return {status: \"ok\"}\n",
    "}\n",
);

fn migrate(source: &str) -> String {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("audit_export.harn");
    fs::write(&script, source).unwrap();
    apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        super::FixOptions::capability_migrations(),
    )
    .unwrap();
    fs::read_to_string(&script).unwrap()
}

/// The falsifier for the fix: without the declaration the migration must still
/// narrow. If this stops holding, the assertion below passes vacuously and
/// proves nothing about `@host_entry`.
#[test]
fn capability_migration_narrows_an_undeclared_two_capability_entry_point() {
    let migrated = migrate(TWO_CAPABILITY_DISPATCH);
    assert!(
        migrated.contains("postgres: HarnessPostgres"),
        "undeclared entry point should still be narrowed to a record: {migrated}"
    );
    assert!(
        !migrated.contains("harness: Harness,"),
        "undeclared entry point should no longer take root Harness: {migrated}"
    );
}

#[test]
fn capability_migration_leaves_a_declared_host_entry_point_alone() {
    let declared = format!("@host_entry\n{TWO_CAPABILITY_DISPATCH}");
    let migrated = migrate(&declared);
    assert_eq!(
        migrated, declared,
        "a host-entered signature is a contract with a caller no Harn source can see"
    );
}

/// The record carrier is not merely over-tight for this call path — it is
/// unreachable. A host supplies one narrow capability or the root handle, so
/// the `Bundle` rung must never be proposed for a declared host entry, at any
/// capability count.
#[test]
fn capability_migration_never_proposes_a_record_carrier_for_a_host_entry() {
    for extra in ["", "  harness.fs.read_file(args.path)\n"] {
        let source = format!(
            "@host_entry\npub fn dispatch(harness: Harness, args: dict) -> dict {{\n\
             \x20 const rows = harness.postgres.query(args.database_url, \"select 1\")\n\
             \x20 harness.net.post(args.sink_url, rows)\n\
             {extra}\
             \x20 return {{status: \"ok\"}}\n\
             }}\n"
        );
        let migrated = migrate(&source);
        let signature = migrated
            .split("-> dict {")
            .next()
            .expect("signature precedes the body")
            .to_string();
        assert!(
            !signature.contains("HarnessFs")
                && !signature.contains("HarnessNet")
                && !signature.contains("HarnessPostgres"),
            "expected no record carrier in the signature, got: {signature}"
        );
        assert!(
            signature.contains("harness: Harness"),
            "expected root Harness to survive, got: {signature}"
        );
    }
}

/// The other half of the same contract. Narrowing a host-entered signature was
/// already refused; the ambient-capability migration changes it from the other
/// direction by *introducing* a parameter, and the host was never asked to
/// pass one (#6221).
const AMBIENT_HOST_ENTRY: &str = concat!(
    "@host_entry\n",
    "pub fn dispatch(args: dict) -> string {\n",
    "  return read_text(args.path)\n",
    "}\n",
    "\n",
    "pub fn summarize(path: string) -> string {\n",
    "  return read_text(path)\n",
    "}\n",
);

fn write_and_migrate(source: &str) -> String {
    let temp = tempfile::TempDir::new().unwrap();
    let script = temp.path().join("main.harn");
    fs::write(&script, source).unwrap();
    apply_repairs_with_options_at(
        temp.path(),
        RepairSafety::SurfaceChanging,
        false,
        super::FixOptions::capability_migrations(),
    )
    .unwrap();
    fs::read_to_string(&script).unwrap()
}

/// The falsifier: without the declaration the migration threads `dispatch` too.
#[test]
fn ambient_migration_threads_an_undeclared_entry_point() {
    let migrated = write_and_migrate(&AMBIENT_HOST_ENTRY.replace("@host_entry\n", ""));
    assert!(
        migrated.contains("pub fn dispatch(fs: HarnessFs, args: dict)"),
        "an undeclared entry point should still be threaded: {migrated}"
    );
}

#[test]
fn ambient_migration_leaves_a_declared_host_entry_signature_alone() {
    let migrated = write_and_migrate(AMBIENT_HOST_ENTRY);
    assert!(
        migrated.contains("pub fn dispatch(args: dict) -> string"),
        "a host-entered signature must not gain a parameter: {migrated}"
    );
}

/// Refusing one callable must not cost its neighbours their repair. The
/// migration is per-callable, so a file with one frozen entry point still
/// migrates everything else in it.
#[test]
fn ambient_migration_still_repairs_a_sibling_of_a_frozen_host_entry() {
    let migrated = write_and_migrate(AMBIENT_HOST_ENTRY);
    assert!(
        migrated.contains("pub fn summarize(fs: HarnessFs, path: string)"),
        "the sibling must still be migrated: {migrated}"
    );
    assert!(
        migrated.contains("fs.read_text(path)"),
        "the sibling's body must still be rewritten: {migrated}"
    );
}

/// The reason has to send the reader to the host, not to a closure wrapper.
#[test]
fn a_frozen_host_entry_is_reported_with_its_own_reason() {
    let temp = tempfile::TempDir::new().unwrap();
    fs::write(temp.path().join("main.harn"), AMBIENT_HOST_ENTRY).unwrap();

    let plan = super::build_plan_with_options_at(
        temp.path(),
        None,
        &super::FixOptions::capability_migrations(),
    )
    .expect("plan");

    let frozen = plan
        .frozen_callables
        .iter()
        .find(|frozen| frozen.name == "dispatch")
        .unwrap_or_else(|| {
            panic!(
                "the frozen host entry must be named; got {:?}",
                plan.frozen_callables
            )
        });
    assert!(
        frozen.reason.contains("`@host_entry`"),
        "the reason must name the declaration: {}",
        frozen.reason
    );
    assert!(
        !frozen.reason.contains("first-class reference"),
        "a host entry must not be explained as a value escape: {}",
        frozen.reason
    );
}
