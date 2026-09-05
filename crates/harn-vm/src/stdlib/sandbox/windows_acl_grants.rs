//! Which host paths a confined child may read, what opening one costs, and how
//! long the grant lasts.
//!
//! This is a different subject from launching the process, which is what the
//! rest of the backend does. The token the backend builds decides *who* the
//! child is; the grants here decide *what that identity can see*, and every
//! rule below was settled by measuring a real Windows machine rather than by
//! reading the API contract. Start at [`Grantee`] for the lifetime rules,
//! [`READ_GRANT_ROOT_ENTRY_CEILING`] for the price, and
//! [`unaffordable_read_roots`] for how the two are traded off.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use super::system_roots::{
    broad_system_root, cached_tree_entry_count, hosts_an_executable, system_read_roots,
};
use super::{process_sandbox_preset_acl_roots, run_icacls, sandbox_trace};
use crate::orchestration::CapabilityPolicy;
use crate::stdlib::sandbox::{
    policy_allows_workspace_write, process_sandbox_policy_read_roots,
    process_sandbox_policy_write_roots, process_sandbox_readonly_roots, process_sandbox_roots,
};

pub(super) struct WorkspaceAclGrants {
    label: String,
    sid: String,
    /// Only the grants made to this spawn's own container SID. The persistent
    /// system read grants are deliberately absent: see [`Grantee`].
    paths: Vec<PathBuf>,
}

/// The well-known group every AppContainer token carries. Windows itself puts
/// it on `C:\Windows`, `C:\Program Files` and everything that inherits from
/// them, which is why a sandboxed child can already run `cmd.exe`.
const ALL_APPLICATION_PACKAGES_SID: &str = "S-1-15-2-1";

/// Who an ACL grant names, which is also what decides its lifetime.
///
/// The two answers are not interchangeable, and picking the wrong one is what
/// made this backend unusable. An ACL grant on Windows is a recursive rewrite
/// (inheritance is not retroactive, so an inheritable entry placed on a
/// directory does not reach the files already inside it), and a grant named
/// for one spawn has to be taken away again when that spawn ends. Measured on
/// a Windows 11 host, one such rewrite over a Node install of 2449 files costs
/// ~1s, and the matching removal costs the same again — per spawn, forever,
/// for every toolchain the agent might use.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Grantee {
    /// This spawn's own AppContainer SID. No other principal can use the
    /// grant and the SID dies with the spawn, so the grant is removed on
    /// drop. Correct for the workspace, whose contents are this run's alone.
    ThisContainer,
    /// [`ALL_APPLICATION_PACKAGES_SID`]. Correct for a host toolchain
    /// directory, and it is what makes the cost bounded: the grant is the
    /// read-execute entry the rest of `C:\Program Files` already carries, so
    /// it is neither per-spawn nor removed. Every later spawn's cheap
    /// non-recursive probe then sees the entry and skips the rewrite
    /// entirely, which on the same host is a 5ms read in place of a 1s
    /// rewrite.
    ///
    /// Two consequences worth stating plainly, because both outlive the
    /// spawn. The entry is readable by every sandboxed program on the
    /// machine, not only by ours; it is read-execute, and it is the
    /// permission the installer's own prefix already grants, but it is a
    /// widening. And `icacls /grant` clears the directory's protected-DACL
    /// flag, so a directory whose installer had detached it from
    /// `C:\Program Files` starts inheriting from that prefix again. Measured,
    /// not assumed: the Node installer detaches exactly this way, and
    /// granting reattached it.
    EveryAppContainer,
}

/// Whether a root has to be on disk for the spawn to be well-formed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MustExist {
    Yes,
    No,
}

/// What a failed grant costs, which is what decides whether the spawn can
/// continue without it. This is the one place the distinction lives, so a new
/// root source has to answer the question rather than inherit an answer.
#[derive(Clone, Copy)]
enum GrantIs {
    /// The child cannot do its job without this grant.
    LoadBearing,
    /// The grant only widens what the child can read.
    BestEffort,
}

impl WorkspaceAclGrants {
    pub(super) fn grant(label: &str, sid: &str, policy: &CapabilityPolicy) -> io::Result<Self> {
        // Read-execute for the entire profile when writes are denied;
        // otherwise Modify on the writable roots. Read-only roots always
        // get read-execute regardless of the workspace-write capability.
        let workspace_permission = if policy_allows_workspace_write(policy) {
            "(OI)(CI)M"
        } else {
            "(OI)(CI)RX"
        };
        let mut paths = Vec::new();
        let writable = process_sandbox_roots(policy).into_iter().map(|root| {
            (
                root,
                workspace_permission,
                MustExist::Yes,
                GrantIs::LoadBearing,
                Grantee::ThisContainer,
            )
        });
        let read_only = process_sandbox_readonly_roots(policy)
            .into_iter()
            .map(|root| {
                (
                    root,
                    "(OI)(CI)RX",
                    MustExist::Yes,
                    GrantIs::BestEffort,
                    Grantee::ThisContainer,
                )
            });
        let process_read = process_sandbox_policy_read_roots(policy)
            .into_iter()
            .map(|root| {
                (
                    root,
                    "(OI)(CI)RX",
                    MustExist::Yes,
                    GrantIs::BestEffort,
                    Grantee::ThisContainer,
                )
            });
        let preset_roots = process_sandbox_preset_acl_roots(policy)
            .into_iter()
            .map(|root| {
                (
                    root,
                    "(OI)(CI)RX",
                    MustExist::No,
                    GrantIs::BestEffort,
                    Grantee::ThisContainer,
                )
            });
        // Host toolchains on PATH that the container cannot already read.
        // Unlike every other entry here this set is not preset-gated: the
        // product contract is reads-open on every profile, and a child that
        // cannot read the interpreter its command names fails with a message
        // that blames PATH rather than the sandbox.
        //
        // The write roots are what a PATH root can already be covered by
        // without being granted itself. The read roots are deliberately NOT
        // listed here: whether each of those is really granted is decided
        // under the cost budget, so the selection tracks them as it accepts
        // them rather than assuming them.
        let write_roots: Vec<PathBuf> = process_sandbox_roots(policy)
            .into_iter()
            .chain(process_sandbox_policy_write_roots(policy))
            .collect();
        // One cost discipline for every read-only grant this spawn would make,
        // computed in the same order the loop below grants them.
        let unaffordable = unaffordable_read_roots(policy, &write_roots);
        let system_read = system_read_roots().into_iter().map(|root| {
            (
                root,
                "(OI)(CI)RX",
                MustExist::No,
                GrantIs::BestEffort,
                Grantee::EveryAppContainer,
            )
        });
        let process_write = if policy_allows_workspace_write(policy) {
            process_sandbox_policy_write_roots(policy)
                .into_iter()
                .map(|root| {
                    (
                        root,
                        workspace_permission,
                        MustExist::Yes,
                        GrantIs::LoadBearing,
                        Grantee::ThisContainer,
                    )
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        for (root, permission, must_exist, grant_is, grantee) in writable
            .chain(read_only)
            .chain(process_read)
            .chain(preset_roots)
            .chain(system_read)
            .chain(process_write)
        {
            if !root.exists() {
                if must_exist == MustExist::No {
                    continue;
                }
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("sandbox workspace root '{}' does not exist", root.display()),
                ));
            }
            // Read grants the cost discipline ruled out. Checked after the
            // existence handling above so a missing load-bearing root still
            // fails the spawn rather than being quietly skipped.
            if unaffordable.contains(&root) {
                continue;
            }
            let grantee_sid = match grantee {
                Grantee::ThisContainer => sid,
                Grantee::EveryAppContainer => ALL_APPLICATION_PACKAGES_SID,
            };
            // A durable grant another process made since this spawn priced
            // its roots makes this rewrite pure duplicated work. Test runners
            // start many processes at once, and they price the same closed
            // roots in the same instant, so without this re-read every one of
            // them repeats every rewrite. The read costs milliseconds against
            // the second it saves, and it deliberately bypasses the cache,
            // whose whole job is to remember the answer from before.
            if grantee == Grantee::EveryAppContainer && recheck_reads_open(&root) {
                sandbox_trace(
                    label,
                    format!(
                        "icacls grant skipped path={} reason=granted-concurrently-by-another-process",
                        root.display()
                    ),
                );
                continue;
            }
            sandbox_trace(
                label,
                format!(
                    "icacls grant begin path={} grantee={}",
                    root.display(),
                    match grantee {
                        Grantee::ThisContainer => "this-container",
                        Grantee::EveryAppContainer => "every-app-container",
                    }
                ),
            );
            let granted = run_icacls(
                &root,
                [
                    "/grant",
                    &format!("*{grantee_sid}:{permission}"),
                    "/T",
                    "/C",
                ],
            );
            match (granted, grant_is) {
                (Ok(()), _) => {
                    sandbox_trace(label, "icacls grant ok");
                    // Only a grant named for this spawn's own container SID is
                    // recorded for removal. A read-execute entry for every
                    // AppContainer is shared state that outlives this spawn by
                    // design, and taking it away again would both restore the
                    // per-spawn cost and race any concurrent spawn relying on
                    // it.
                    match grantee {
                        Grantee::ThisContainer => paths.push(root),
                        Grantee::EveryAppContainer => {
                            remember_reads_open(&root);
                            report_durable_host_grant(&root);
                        }
                    }
                }
                // A write grant the child depends on. Without it the child
                // cannot write its own workspace, which is not a usable
                // sandbox, so the spawn fails rather than running crippled.
                (Err(error), GrantIs::LoadBearing) => return Err(error),
                // A read grant. Failing it leaves the child seeing that
                // directory as read-closed, which is exactly the behavior
                // before this root was ever attempted — a narrower sandbox,
                // not a broken one. An unelevated caller cannot rewrite a
                // system directory's ACL, and that must not take every
                // command on the machine down with it.
                (Err(error), GrantIs::BestEffort) => sandbox_trace(
                    label,
                    format!(
                        "icacls grant failed, continuing read-closed for this root: path={} error={error}",
                        root.display()
                    ),
                ),
            }
        }
        Ok(Self {
            label: label.to_string(),
            sid: sid.to_string(),
            paths,
        })
    }
}

impl Drop for WorkspaceAclGrants {
    fn drop(&mut self) {
        for path in &self.paths {
            sandbox_trace(
                &self.label,
                format!("icacls remove begin path={}", path.display()),
            );
            match run_icacls(path, ["/remove:g", &format!("*{}", self.sid), "/T", "/C"]) {
                Ok(()) => sandbox_trace(&self.label, "icacls remove ok"),
                Err(error) => sandbox_trace(&self.label, format!("icacls remove failed: {error}")),
            }
        }
    }
}

/// The largest tree one read root may be before it is not worth opening.
///
/// Affordability is a property of the root, not a race between roots. A shared
/// budget spent in some order makes the toolchain a command actually names
/// compete against ones it never will, and it loses on the ordering rather
/// than on the merits: measured on a Windows runner, a shared 8,192-entry
/// budget was consumed by cheaper directories and the Node install was refused
/// with 2,822 left against the 2,865 it needed, so `node` stayed unreadable.
///
/// Entries are the price (the rewrite runs at roughly 2,500 per second on a
/// Windows 11 host), so this is the price of the most expensive root worth
/// paying for. It admits every real language toolchain measured on a build
/// runner, a Node install at 2,865 entries and a Python installation at 5,978,
/// and refuses the 9,307-entry cargo build tree, whose executables are test
/// binaries no agent command names.
const READ_GRANT_ROOT_ENTRY_CEILING: usize = 6144;

/// Backstop on the total a single spawn will rewrite, across every root.
///
/// The per-root ceiling above is what decides the normal case. This exists so
/// a pathological host, one whose `PATH` carries dozens of closed toolchain
/// directories, cannot turn many individually reasonable grants into one
/// unreasonable spawn. It is deliberately far above what a real host reaches:
/// the build runner that motivated all of this needs about 9,000 entries in
/// total, once, because the grants are durable.
const READ_GRANT_TOTAL_ENTRY_CEILING: usize = 65536;

/// Every read-only root this spawn should NOT grant, decided once for all
/// four read sources under one budget.
///
/// Read grants are the only ones with a cost problem, and they all have the
/// same one: each is a recursive ACL rewrite whose price is the size of the
/// tree. Deciding that per source is how the backend ended up unusable, so the
/// decision lives here and nowhere else. Load-bearing workspace write grants
/// are deliberately not routed through this: the spawn needs them, so their
/// cost is not optional and skipping one would produce a sandbox that cannot
/// do its job.
///
/// The order matters, because the budget is finite and spent in order. It
/// matches the order the grant loop uses, so what this rules out is exactly
/// what that loop would otherwise have paid for.
///
/// Two measurements from a Windows 11 developer machine shaped this:
///
/// * The home toolchain roots (`.cargo`, `.rustup`, `.cache`) are enormous on
///   any machine that has actually built something. Granting them per spawn
///   and removing them again afterwards is what made every sandboxed command
///   time out at two minutes — not the `PATH` roots this module was written
///   for (harn#7993, harn#8004).
/// * `PATH` on a build host is mostly cargo build output directories, which
///   hold object files and no executable. [`hosts_an_executable`] is what
///   stops them crowding out the Node install they were hiding.
///
/// A root ruled out here is simply not opened to the child, which is a
/// narrower sandbox rather than a broken one. A root that does not exist is
/// never ruled out, so the caller's own existence handling still decides
/// whether a missing load-bearing root fails the spawn.
fn unaffordable_read_roots(
    policy: &CapabilityPolicy,
    write_roots: &[PathBuf],
) -> BTreeSet<PathBuf> {
    let mut skip = BTreeSet::new();
    // What a `PATH` root can be covered by. A proposed root is not a granted
    // one: `~/.cargo` is a read root on every developer machine and is far too
    // large to grant, so treating it as covering `~/.cargo\bin` would leave
    // the child unable to read either.
    let mut covered_by: Vec<PathBuf> = write_roots.to_vec();

    let candidates = process_sandbox_readonly_roots(policy)
        .into_iter()
        .chain(process_sandbox_policy_read_roots(policy))
        .chain(process_sandbox_preset_acl_roots(policy))
        .map(|root| (root, false))
        .chain(system_read_roots().into_iter().map(|root| (root, true)));

    // First pass: rule out everything that is wrong in shape or already open,
    // and price what remains. Nothing is granted here, so the order of this
    // pass does not decide anything.
    let mut priced: Vec<(usize, PathBuf)> = Vec::new();
    for (root, from_path) in candidates {
        if skip.contains(&root) || priced.iter().any(|(_, seen)| *seen == root) || !root.exists() {
            continue;
        }
        // These three rules apply only to roots discovered from `PATH`. A root
        // the policy names is there because an embedder asked for it, so it is
        // not ours to second-guess on shape — only on cost.
        if from_path {
            if broad_system_root(&root) {
                read_root_decision(&root, "action=skipped reason=broad-system-prefix");
                skip.insert(root);
                continue;
            }
            if !hosts_an_executable(&root) {
                read_root_decision(&root, "action=skipped reason=no-executable-in-directory");
                skip.insert(root);
                continue;
            }
        }
        if app_container_can_already_read(&root) {
            read_root_decision(
                &root,
                "probe=already-open action=skipped reason=admits-all-application-packages",
            );
            skip.insert(root);
            continue;
        }
        let Some(entries) = cached_tree_entry_count(&root, READ_GRANT_ROOT_ENTRY_CEILING) else {
            read_root_decision(
                &root,
                &format!(
                    "probe=closed action=skipped reason=tree-exceeds-root-ceiling-{READ_GRANT_ROOT_ENTRY_CEILING}"
                ),
            );
            skip.insert(root);
            continue;
        };
        priced.push((entries, root));
    }

    // Second pass: cheapest first, under the spawn backstop.
    //
    // Position on `PATH` is not a measure of worth, and spending in that order
    // is what kept the Node install unreadable: on a Windows build runner a
    // 9,307-entry build tree sat ahead of it and took everything. Ordering no
    // longer decides whether a root is granted, since the per-root ceiling
    // already settled that; it decides only who reaches the backstop first,
    // and cheapest first opens the most toolchains before anything does.
    priced.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let mut remaining = READ_GRANT_TOTAL_ENTRY_CEILING;
    for (entries, root) in priced {
        if covered_by.iter().any(|already| root.starts_with(already)) {
            read_root_decision(
                &root,
                "action=skipped reason=already-granted-by-another-root",
            );
            skip.insert(root);
            continue;
        }
        if entries > remaining {
            read_root_decision(
                &root,
                &format!(
                    "probe=closed action=skipped reason=entries-{entries}-exceed-remaining-spawn-ceiling-{remaining}"
                ),
            );
            skip.insert(root);
            continue;
        }
        remaining -= entries;
        covered_by.push(root.clone());
        read_root_decision(
            &root,
            &format!(
                "probe=closed action=will-grant entries={entries} remaining_budget={remaining}"
            ),
        );
    }
    skip
}

/// One line per read root, naming what was decided and why. A spawn that takes
/// too long, or a toolchain the child cannot see, is unreadable without it,
/// and both were diagnosed from exactly these lines.
fn read_root_decision(root: &Path, outcome: &str) {
    sandbox_trace(
        "read-roots",
        format!("decision path={} {outcome}", root.display()),
    );
}

/// Whether `path`'s DACL already admits every AppContainer, i.e. carries an
/// `ALL APPLICATION PACKAGES` (`S-1-15-2-1`) entry. Read through `icacls`
/// rather than `GetNamedSecurityInfoW` so the check uses the same mechanism
/// the grants do and stays free of hand-rolled ACL walking. This read is
/// non-recursive and costs milliseconds, unlike the `/T` grant it avoids.
///
/// Cached per path: host permissions do not change under us, and the same
/// roots are re-examined on every spawn.
///
/// A DACL that cannot be read counts as already-open. That is the cheap
/// direction and it matches what this backend did before the probe existed:
/// an unreadable DACL never adds a recursive ACL rewrite.
/// Re-read `path`'s permissions, ignoring and then refreshing the cache.
///
/// [`app_container_can_already_read`] answers from a cache on purpose: within
/// one spawn the answer cannot change under it. Across spawns it can, because
/// a durable grant is exactly the kind of change another process makes, and
/// that is the one case worth paying a fresh read for.
fn recheck_reads_open(path: &Path) -> bool {
    let Ok(dacl) = read_icacls(path) else {
        return false;
    };
    let dacl = dacl.to_ascii_uppercase();
    let readable = dacl.contains("ALL APPLICATION PACKAGES") || dacl.contains("S-1-15-2-1:");
    if readable {
        remember_reads_open(path);
    }
    readable
}

/// Tell the operator, once per path, that a durable change was made to their
/// machine.
///
/// Every other trace here is behind `HARN_WINDOWS_SANDBOX_TRACE`, which is the
/// right default for per-spawn detail an operator has no reason to read. This
/// one is not, because it is the only thing the sandbox does that outlives the
/// run: the entry stays on the directory after the process exits, it is
/// readable by every sandboxed program rather than only this one, and applying
/// it clears the directory's protected-permissions flag. A change with those
/// three properties should not be discoverable only by someone who already
/// suspected it happened.
///
/// It is quiet in practice. The grant is skipped whenever the path already
/// reads open, so a machine prints this once per toolchain root and then never
/// again.
fn report_durable_host_grant(root: &Path) {
    if !first_report_of(root) {
        return;
    }
    eprintln!(
        "harn: opened '{}' for reading by sandboxed programs. This is a lasting \
         change to this machine: the entry stays after harn exits, applies to \
         every sandboxed program rather than this run alone, and re-enables \
         permission inheritance on that directory. Undo it with: icacls \
         \"{}\" /remove:g *{} /T /C",
        root.display(),
        root.display(),
        ALL_APPLICATION_PACKAGES_SID
    );
}

/// Whether this process has yet reported a durable grant for `root`.
///
/// Separate from [`reads_open_cache`] on purpose: that cache is about what the
/// container can read, and is written by paths that made no change at all.
fn first_report_of(root: &Path) -> bool {
    static REPORTED: std::sync::OnceLock<std::sync::Mutex<BTreeSet<PathBuf>>> =
        std::sync::OnceLock::new();
    let reported = REPORTED.get_or_init(|| std::sync::Mutex::new(BTreeSet::new()));
    match reported.lock() {
        Ok(mut seen) => seen.insert(root.to_path_buf()),
        // A poisoned lock means another thread panicked mid-report. Saying it
        // twice is better than a machine change nobody was told about.
        Err(_) => true,
    }
}

/// Answers remembered by [`app_container_can_already_read`], shared with
/// [`recheck_reads_open`] so a fresh read can correct a stale "closed".
fn reads_open_cache() -> &'static std::sync::Mutex<std::collections::HashMap<PathBuf, bool>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<PathBuf, bool>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

fn remember_reads_open(path: &Path) {
    if let Ok(mut map) = reads_open_cache().lock() {
        map.insert(path.to_path_buf(), true);
    }
}

fn app_container_can_already_read(path: &Path) -> bool {
    let cache = reads_open_cache();
    if let Ok(map) = cache.lock() {
        if let Some(known) = map.get(path) {
            return *known;
        }
    }
    let Ok(dacl) = read_icacls(path) else {
        sandbox_trace(
            "system-read-roots",
            format!("DACL unreadable, treated as open path={}", path.display()),
        );
        return true;
    };
    let dacl = dacl.to_ascii_uppercase();
    // The friendly name is localized, so match the raw SID too. `icacls`
    // renders an unresolved SID as `*S-1-15-2-1:(...)`; the trailing colon
    // keeps `S-1-15-2-1` from matching a longer sibling SID.
    let readable = dacl.contains("ALL APPLICATION PACKAGES") || dacl.contains("S-1-15-2-1:");
    if let Ok(mut map) = cache.lock() {
        map.insert(path.to_path_buf(), readable);
    }
    readable
}

fn read_icacls(path: &Path) -> io::Result<String> {
    let output = std::process::Command::new("icacls").arg(path).output()?;
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("icacls read failed for '{}'", path.display()),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
