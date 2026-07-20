use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fs, process};

use chrono_tz::Tz;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::str::FromStr;
use url::Url;

const CONTENT_HASH_FILE: &str = harn_modules::package_execution::CONTENT_HASH_FILE;
const CACHE_METADATA_FILE: &str = harn_modules::package_execution::CACHE_METADATA_FILE;
const HARN_CACHE_DIR_ENV: &str = "HARN_CACHE_DIR";
const HARN_PACKAGE_REGISTRY_ENV: &str = "HARN_PACKAGE_REGISTRY";
const HARN_PACKAGE_REGISTRY_TOKEN_ENV: &str = "HARN_PACKAGE_REGISTRY_TOKEN";
const DEFAULT_PACKAGE_REGISTRY_URL: &str = "https://packages.harnlang.com/harn-package-index.toml";
const CACHE_METADATA_VERSION: u32 = 1;
const LOCK_FILE_VERSION: u32 = 4;
const REGISTRY_INDEX_VERSION: u32 = 1;
const PACKAGE_ARCHIVE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const PACKAGE_ARCHIVE_MAX_UNPACKED_BYTES: u64 = 64 * 1024 * 1024;
const MANIFEST: &str = harn_modules::manifest_walk::MANIFEST_FILENAME;
const LOCK_FILE: &str = "harn.lock";
const TRIGGER_RETRY_MAX_LIMIT: u32 = 100;

pub(crate) mod errors;
mod extensions;
mod generations;
mod git_cwd;
mod lockfile;
mod manifest;
mod manifest_search;
mod maturity;
mod mutation;
mod package_ops;
mod persona_activation;
mod persona_runtime;
mod registry;
mod skills;
mod validation;

#[allow(unused_imports)]
pub use errors::{PackageError, PackageResult};

pub use extensions::*;
pub(crate) use generations::*;
pub(crate) use git_cwd::Cwd;
#[cfg(test)]
pub use lockfile::add_package;
pub(crate) use lockfile::*;
pub use lockfile::{
    add_package_with_registry, ensure_dependencies_materialized, install_packages, lock_packages,
    remove_package, update_packages, PackageLockExport, PackageLockExports,
};
pub use manifest::*;
pub(crate) use manifest_search::*;
pub use maturity::{
    artifacts_check, artifacts_manifest, audit_packages, outdated_packages, ArtifactDriftReport,
    AuditCode, AuditFinding, AuditReport, AuditSeverity, OutdatedEntry, OutdatedReport,
    OutdatedStatus,
};
pub(crate) use mutation::*;
pub use package_ops::*;
pub use persona_activation::*;
pub(crate) use persona_runtime::*;
pub(crate) use registry::*;
pub use registry::{
    clean_package_cache, list_package_cache, search_package_registry, search_rule_package_registry,
    show_package_registry_info, verify_package_cache,
};
pub use skills::*;
pub(crate) use validation::*;

#[cfg(test)]
pub(crate) mod test_support;
