//! Three-outcome result of locating and parsing the nearest `harn.toml`.
//!
//! Split out from `manifest.rs` so the distinction between "no manifest" and
//! "a manifest that would not parse" lives in one small, self-contained place
//! rather than buried in that module's bulk.

use std::path::{Path, PathBuf};

use super::{read_manifest_from_path, Manifest, PackageError};

/// Outcome of locating and parsing the nearest `harn.toml`.
///
/// The three cases are kept distinct on purpose. Collapsing `Malformed` into
/// `Missing` — as the previous hand-rolled walk did, by printing a warning and
/// returning `None` — makes a broken manifest indistinguishable from "no
/// manifest here" to every caller, so a user's config is silently ignored and
/// the failure surfaces only as absent behavior. Callers that legitimately
/// want a default when no manifest exists still get that via `Missing`, but
/// they must decide explicitly what to do with `Malformed`.
pub(crate) enum ManifestSearch {
    /// A `harn.toml` was found on the walk and parsed successfully. The
    /// manifest is boxed because it dwarfs the other variants (~1.7 KiB vs a
    /// handful of bytes), which would otherwise bloat every `ManifestSearch`.
    Found {
        manifest: Box<Manifest>,
        dir: PathBuf,
    },
    /// A `harn.toml` was found but failed to parse; the error names the path.
    Malformed(PackageError),
    /// No `harn.toml` governs the start path.
    Missing,
}

impl ManifestSearch {
    /// Collapse to `Result<Option<(Manifest, PathBuf)>, PackageError>`: a
    /// malformed manifest becomes a hard error (never silently "missing"),
    /// while a genuine absence is `Ok(None)`. Lets a `PackageError`-returning
    /// caller propagate parse failures with `?` while still treating absence
    /// as a soft default.
    pub(crate) fn into_result(self) -> Result<Option<(Manifest, PathBuf)>, PackageError> {
        match self {
            ManifestSearch::Found { manifest, dir } => Ok(Some((*manifest, dir))),
            ManifestSearch::Malformed(error) => Err(error),
            ManifestSearch::Missing => Ok(None),
        }
    }
}

/// Locate the nearest `harn.toml` via the shared project-root walk and parse
/// it into the CLI's full [`Manifest`]. See [`ManifestSearch`] — a manifest
/// that is found but fails to parse is reported as `Malformed`, never quietly
/// dropped as if it were absent.
pub(crate) fn load_nearest_manifest(start: &Path) -> ManifestSearch {
    let Some(found) = harn_modules::manifest_walk::find_nearest_manifest(start) else {
        return ManifestSearch::Missing;
    };
    match read_manifest_from_path(&found.path) {
        Ok(manifest) => ManifestSearch::Found {
            manifest: Box::new(manifest),
            dir: found.dir,
        },
        Err(error) => ManifestSearch::Malformed(error),
    }
}

/// Locate the directory of the nearest `harn.toml` without parsing it — the
/// project root. For callers that only need the location, so a malformed
/// manifest is irrelevant to them.
pub(crate) fn find_nearest_manifest_dir(start: &Path) -> Option<PathBuf> {
    harn_modules::manifest_walk::find_nearest_manifest(start).map(|found| found.dir)
}

/// For callers that cannot return an error (they yield a `Vec`, `Option`, or a
/// bare config): a malformed manifest is reported to stderr — never silently
/// dropped — and then treated as absent so the caller degrades to its default.
/// Callers that *can* surface an error should use [`ManifestSearch::into_result`]
/// instead and propagate `Malformed`.
pub(crate) fn nearest_manifest_or_warn(start: &Path) -> Option<(Manifest, PathBuf)> {
    match load_nearest_manifest(start) {
        ManifestSearch::Found { manifest, dir } => Some((*manifest, dir)),
        ManifestSearch::Malformed(error) => {
            eprintln!("warning: ignoring malformed harn.toml: {error}");
            None
        }
        ManifestSearch::Missing => None,
    }
}
