use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use std::{fs, process};

use chrono_tz::Tz;
use fs2::FileExt;
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::str::FromStr;
use url::Url;

const CONTENT_HASH_FILE: &str = ".harn-content-hash";
const CACHE_METADATA_FILE: &str = ".harn-package-cache.toml";
const HARN_CACHE_DIR_ENV: &str = "HARN_CACHE_DIR";
const HARN_PACKAGE_REGISTRY_ENV: &str = "HARN_PACKAGE_REGISTRY";
const DEFAULT_PACKAGE_REGISTRY_URL: &str = "https://packages.harnlang.com/index.toml";
const CACHE_METADATA_VERSION: u32 = 1;
const LOCK_FILE_VERSION: u32 = 1;
const REGISTRY_INDEX_VERSION: u32 = 1;
const PKG_DIR: &str = ".harn/packages";
const MANIFEST: &str = "harn.toml";
const LOCK_FILE: &str = "harn.lock";
const TRIGGER_RETRY_MAX_LIMIT: u32 = 100;

mod extensions;
mod lockfile;
mod manifest;
mod package_ops;
mod registry;
mod skills;
mod validation;

pub use extensions::*;
#[cfg(test)]
pub use lockfile::add_package;
pub(crate) use lockfile::*;
pub use lockfile::{
    add_package_with_registry, ensure_dependencies_materialized, install_packages, lock_packages,
    remove_package, update_packages,
};
pub use manifest::*;
pub use package_ops::*;
pub(crate) use registry::*;
pub use registry::{
    clean_package_cache, list_package_cache, search_package_registry, show_package_registry_info,
    verify_package_cache,
};
pub use skills::*;
pub(crate) use validation::*;

#[cfg(test)]
pub(crate) mod test_support;
