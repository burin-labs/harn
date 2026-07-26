//! The `harn.lock` file: how it is built, read, and realized on disk.
//!
//! A manifest's dependencies are walked into a lock file ([`build`]) using the
//! entry-reuse and provenance rules in [`resolution`]; the resulting document
//! ([`document`], [`exports`]) is realized into `packages/` by [`materialize`].
//! [`add`], [`install`], and [`manifest_edit`] are the command surface that
//! drives all of it.

mod add;
mod build;
mod document;
mod exports;
mod install;
mod manifest_edit;
mod materialize;
mod resolution;

#[cfg(test)]
mod tests;

// Every consumer of these outside `add` is test-only, so the re-export is
// unused in a non-test build; same idiom as the `errors` re-export in
// `package/mod.rs`.
#[allow(unused_imports)]
pub(crate) use self::add::*;
pub(crate) use self::build::*;
pub(crate) use self::document::*;
pub(crate) use self::exports::*;
pub(crate) use self::install::*;
pub(crate) use self::manifest_edit::*;
pub(crate) use self::materialize::*;
pub(crate) use self::resolution::*;

#[cfg(test)]
pub use self::add::add_package;
pub use self::add::add_package_with_registry;
pub use self::exports::{PackageLockExport, PackageLockExports};
pub use self::install::{install_packages, lock_packages, remove_package, update_packages};
pub use self::materialize::ensure_dependencies_materialized;
