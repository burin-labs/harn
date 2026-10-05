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
use std::str::FromStr;
use url::Url;

const CONTENT_HASH_FILE: &str = harn_modules::package_execution::CONTENT_HASH_FILE;
const CACHE_METADATA_FILE: &str = harn_modules::package_execution::CACHE_METADATA_FILE;
const HARN_PACKAGE_REGISTRY_ENV: &str = "HARN_PACKAGE_REGISTRY";
const HARN_PACKAGE_REGISTRY_TOKEN_ENV: &str = "HARN_PACKAGE_REGISTRY_TOKEN";
const DEFAULT_PACKAGE_REGISTRY_URL: &str = "https://packages.harnlang.com/harn-package-index.toml";
const CACHE_METADATA_VERSION: u32 = 1;
const LOCK_FILE_VERSION: u32 = 5;
const REGISTRY_INDEX_VERSION: u32 = 2;
const PACKAGE_ARCHIVE_MAX_BYTES: u64 = 64 * 1024 * 1024;
const PACKAGE_ARCHIVE_MAX_UNPACKED_BYTES: u64 = 64 * 1024 * 1024;
const MANIFEST: &str = harn_modules::manifest_walk::MANIFEST_FILENAME;
const LOCK_FILE: &str = "harn.lock";
const TRIGGER_RETRY_MAX_LIMIT: u32 = 100;

mod credential_environment;
pub mod errors;
mod extensions;
mod extensions_connectors;
mod generations;
mod git_cwd;
mod input_policy;
mod lockfile;
mod manifest;
mod manifest_search;
mod maturity;
mod mutation;
mod package_ops;
mod persona_activation;
#[cfg(any(test, feature = "test-support"))]
pub use persona_activation::project_mutation_lock_test_probe;
mod persona_runtime;
mod registry;
mod skills;
mod validation;

#[allow(unused_imports)]
pub use errors::{PackageError, PackageResult};

pub use crate::path_policy::PathEntryKind;
pub use credential_environment::*;
pub use extensions::*;
pub(crate) use extensions_connectors::{
    dedupe_provider_connectors, installed_package_provider_connectors,
};
pub use extensions_connectors::{
    ensure_provider_connector_dependencies, load_provider_connector, try_load_provider_connectors,
    try_load_root_provider_connectors, ResolvedProviderConnectors,
};
pub(crate) use generations::*;
pub(crate) use git_cwd::Cwd;
pub use input_policy::should_exclude_package_entry;
#[cfg(test)]
pub use lockfile::add_package;
pub(crate) use lockfile::*;
pub use lockfile::{
    add_package_with_registry, declared_package_import_aliases,
    ensure_cached_dependencies_materialized, ensure_dependencies_materialized,
    ensure_reachable_dependencies_materialized, install_packages, lock_packages,
    reachable_dependency_lock_digest, remove_package, update_packages, PackageLockExport,
    PackageLockExports,
};
pub use manifest::*;
pub use manifest_search::{
    find_nearest_manifest_dir, load_nearest_manifest, nearest_manifest_or_warn, ManifestSearch,
};
pub use maturity::{
    artifacts_check_with, artifacts_manifest_with, audit_packages, check_artifact_manifest_from,
    outdated_packages, ArtifactDriftReport, AuditCode, AuditFinding, AuditReport, AuditSeverity,
    OutdatedEntry, OutdatedReport, OutdatedStatus,
};
pub use mutation::install_packages_in;
pub(crate) use mutation::*;
pub use package_ops::*;
pub use persona_activation::*;
pub(crate) use persona_runtime::*;
pub use persona_runtime::{
    installed_persona_trigger_configs, persona_runtime_binding, persona_runtime_callable,
    ResolvedRuntimePersona,
};
pub(crate) use registry::*;
pub use registry::{
    clean_package_cache, compute_content_hash, list_package_cache, path_source_uri,
    search_package_registry, search_rule_package_registry, show_package_registry_info,
    try_resolve_installed_package, verify_package_cache, verify_package_registry,
};
pub use skills::*;
pub(crate) use validation::*;
pub use validation::{
    build_manifest_provider_catalog, install_manifest_provider_schemas,
    manifest_module_source_path, manifest_trigger_location, parse_duration_millis,
    parse_local_trigger_ref, parse_trigger_handler_uri, validate_contributions,
    validate_orchestrator_budget, validate_static_trigger_configs,
};

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
