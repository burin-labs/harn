//! The package registry and the on-disk package cache.
//!
//! A dependency spec becomes a resolved registry version ([`resolve`]) backed
//! by an index document ([`index`]) fetched over a source URI ([`source`]);
//! that source is cloned or unpacked ([`git_source`], [`populate`]) into a
//! cache directory whose layout and locking live in [`cache_layout`] and whose
//! contents are hashed and materialized by [`content_hash`]. [`inspect`] reads
//! the cache back, and [`commands`] is the CLI surface over all of it.

mod cache_layout;
mod commands;
mod content_hash;
mod git_source;
mod index;
mod inspect;
mod populate;
mod resolve;
mod source;

#[cfg(test)]
mod tests;

pub(crate) use self::cache_layout::*;
pub(crate) use self::content_hash::*;
pub(crate) use self::git_source::*;
pub(crate) use self::index::*;
pub(crate) use self::inspect::*;
pub(crate) use self::populate::*;
pub(crate) use self::resolve::*;
pub(crate) use self::source::*;

pub use self::commands::{
    clean_package_cache, list_package_cache, search_package_registry, search_rule_package_registry,
    show_package_registry_info, verify_package_cache,
};
