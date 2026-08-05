//! Callables the package manifest declares the runtime enters (harn#6272).
//!
//! `@host_entry` exists because a host's registration lives in the host's own
//! source, where no static pass can see it (#6193). A manifest hook or trigger
//! is the same contract — the runtime invokes the handler at the arity the
//! declaration fixes — except that the registration is not invisible at all. It
//! is written down, in `harn.toml`, in a block this crate already parses:
//!
//! ```toml
//! [[hooks]]
//! event = "PreToolUse"
//! handler = "burin_code::enforce_stage_tool_gate"
//! ```
//!
//! Asking an author to also write `@host_entry` above that function would make
//! the manifest and the attribute two surfaces stating one fact, and the one
//! that drifts is the one nothing checks. So the fixer reads the manifest.
//!
//! # What the runtime actually supplies
//!
//! Not a fixed arity — the two dispatchers differ. A manifest tool hook goes
//! through `invoke_vm_hook_handler`, which calls the closure with
//! `[harness, event]`; a persona step hook goes through `call_lifecycle_hook`,
//! which passes the payload alone. What they share is the part that matters
//! here: when a capability argument is supplied at all it is the **root**
//! `Harness`. So the migration's carrier ladder is the problem, exactly as in
//! #6193 — it rewrote `enforce_stage_tool_gate(event)` to take
//! `{agent: HarnessAgent, runtime: HarnessRuntime}`, and no dispatcher builds a
//! record.
//!
//! # Why freeze rather than pin to root
//!
//! For a hook entered with `[harness, event]`, taking root `Harness` first is a
//! legal shape, so freezing refuses one rewrite that would have been correct in
//! order to refuse the several that are not. Choosing the conservative side
//! keeps this identical to what `@host_entry` already does for an entry point
//! with no capability parameter, and the refusal is reported as a frozen
//! callable rather than applied silently. Promoting a registered handler to
//! root `Harness` is a real improvement, but it is a migration with its own
//! correctness argument — per hook event — and not something to infer from a
//! body.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::package::{load_nearest_manifest, manifest_module_source_path, Manifest};

/// The manifest-declared entry points, indexed by the file that declares them.
///
/// Indexed by file rather than held as one name set because the manifest
/// resolves a handler to an exact module — `pkg::fn` to the package's
/// `lib.harn`, an export key to its mapped path. Freezing `run` everywhere
/// because some trigger handler is named `run` would block migrations the
/// manifest says nothing about.
#[derive(Debug, Default)]
pub(super) struct ManifestHostEntries {
    by_file: BTreeMap<PathBuf, BTreeSet<String>>,
    empty: BTreeSet<String>,
}

impl ManifestHostEntries {
    /// Read the nearest manifest above the fix targets.
    ///
    /// Best-effort by construction: no manifest, an unreadable one, or a
    /// handler naming a module that does not resolve all yield no entries. A
    /// fixer that refused to run because a manifest was malformed would be
    /// strictly worse than one that migrates what it can — `harn check` is
    /// where a broken manifest is supposed to be reported, and it says so
    /// there with a span.
    pub(super) fn load(targets: &[PathBuf]) -> Self {
        let anchor = targets
            .first()
            .cloned()
            .unwrap_or_else(|| PathBuf::from("."));
        let Ok(Some((manifest, manifest_dir))) = load_nearest_manifest(&anchor).into_result()
        else {
            return Self::default();
        };
        let mut entries = Self::default();
        for handler in manifest_handlers(&manifest) {
            entries.insert_handler(&manifest, &manifest_dir, handler);
        }
        entries
    }

    fn insert_handler(&mut self, manifest: &Manifest, manifest_dir: &Path, handler: &str) {
        // A trigger handler may address a remote surface (`a2a://`,
        // `worker://`, `persona://`, `eval_pack://`) instead of a local
        // module. Those name nothing in this source tree.
        if handler.contains("://") {
            return;
        }
        let Some((module_name, function_name)) = handler.rsplit_once("::") else {
            return;
        };
        let package_name = manifest
            .package
            .as_ref()
            .and_then(|package| package.name.as_deref());
        let Ok(path) = manifest_module_source_path(
            manifest_dir,
            package_name,
            &manifest.exports,
            Some(module_name),
        ) else {
            return;
        };
        self.by_file
            .entry(canonical(&path))
            .or_default()
            .insert(function_name.to_string());
    }

    pub(super) fn names_for(&self, file: &Path) -> &BTreeSet<String> {
        self.by_file.get(&canonical(file)).unwrap_or(&self.empty)
    }
}

/// Every `module::function` a manifest block names as a runtime entry point.
///
/// Hooks and triggers are one class here: both resolve a handler through
/// [`manifest_module_source_path`] and both are invoked by the runtime at the
/// arity the declaration fixes. A block that gains a handler field later joins
/// this list; nothing else has to change.
fn manifest_handlers(manifest: &Manifest) -> impl Iterator<Item = &str> {
    manifest
        .hooks
        .iter()
        .map(|hook| hook.handler.as_str())
        .chain(
            manifest
                .triggers
                .iter()
                .map(|trigger| trigger.handler.as_str()),
        )
}

/// Compare paths by their resolved form.
///
/// The fixer's file list and the manifest's resolved handler path reach the
/// same file by different routes — a relative target walked from the invocation
/// directory versus a join onto the manifest directory. Falling back to the
/// original path keeps a non-existent file comparable to itself.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
