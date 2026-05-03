use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::ast::Node;
use super::error::TemplateError;
use super::parser::parse;

const TEMPLATE_CACHE_CAP: usize = 128;

#[derive(Debug, Clone)]
pub(crate) struct TemplateAsset {
    pub(crate) id: String,
    pub(crate) uri: String,
    pub(crate) source: Rc<str>,
    pub(crate) location: TemplateLocation,
}

#[derive(Debug, Clone)]
pub(crate) enum TemplateLocation {
    Inline {
        base: Option<PathBuf>,
        source_path: Option<PathBuf>,
        include_root: Option<PathBuf>,
    },
    Filesystem {
        path: PathBuf,
        base: Option<PathBuf>,
        include_root: Option<PathBuf>,
    },
    Stdlib {
        path: String,
        base: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct CacheKey {
    id: String,
    content_hash: String,
}

#[derive(Default)]
struct TemplateCache {
    entries: BTreeMap<CacheKey, Rc<Vec<Node>>>,
    order: VecDeque<CacheKey>,
}

thread_local! {
    static TEMPLATE_CACHE: RefCell<TemplateCache> = RefCell::new(TemplateCache::default());
}

impl TemplateAsset {
    pub(crate) fn inline(source: &str, base: Option<&Path>, source_path: Option<&Path>) -> Self {
        let source_path = source_path.map(Path::to_path_buf);
        let base = base.map(Path::to_path_buf);
        let include_root = base
            .as_ref()
            .map(|path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
        let uri = source_path
            .as_deref()
            .and_then(|path| path.to_str().map(str::to_string))
            .unwrap_or_default();
        let id = if uri.is_empty() {
            "inline".to_string()
        } else {
            format!("file:{uri}")
        };
        Self {
            id,
            uri,
            source: Rc::from(source),
            location: TemplateLocation::Inline {
                base,
                source_path,
                include_root,
            },
        }
    }

    pub(crate) fn render_target(path: &str) -> Result<Self, String> {
        if let Some(asset) = Self::stdlib(path) {
            return Ok(asset);
        }
        if crate::stdlib::asset_paths::stdlib_prompt_asset_path(path).is_some() {
            return Err(format!("unknown stdlib prompt asset '{path}'"));
        }
        let resolved = crate::stdlib::asset_paths::resolve_or_source_relative(path, None)?;
        Self::filesystem(resolved, "Failed to read template")
    }

    pub(crate) fn filesystem(path: PathBuf, read_prefix: &str) -> Result<Self, String> {
        let source = std::fs::read_to_string(&path)
            .map_err(|error| format!("{read_prefix} {}: {error}", path.display()))?;
        Ok(Self::from_filesystem_source(path, source))
    }

    pub(crate) fn from_filesystem_source(path: PathBuf, source: String) -> Self {
        let canonical_path = path.canonicalize().unwrap_or_else(|_| path.clone());
        let base = path.parent().map(Path::to_path_buf);
        let include_root = base
            .as_ref()
            .map(|path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
        let uri = path.display().to_string();
        Self {
            id: format!("file:{}", canonical_path.display()),
            uri,
            source: Rc::from(source),
            location: TemplateLocation::Filesystem {
                path,
                base,
                include_root,
            },
        }
    }

    pub(crate) fn stdlib(path: &str) -> Option<Self> {
        let rel = crate::stdlib::asset_paths::stdlib_prompt_asset_path(path)?;
        let source = crate::stdlib_modules::get_stdlib_prompt_asset(rel)?;
        Some(Self::from_stdlib_source(rel, source))
    }

    fn from_stdlib_source(path: &str, source: &'static str) -> Self {
        let base = Path::new(path)
            .parent()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        Self {
            id: format!("std:{path}"),
            uri: format!("std://{path}"),
            source: Rc::from(source),
            location: TemplateLocation::Stdlib {
                path: path.to_string(),
                base,
            },
        }
    }

    pub(crate) fn error_path(&self) -> Option<PathBuf> {
        match &self.location {
            TemplateLocation::Inline { source_path, .. } => source_path.clone(),
            TemplateLocation::Filesystem { path, .. } => Some(path.clone()),
            TemplateLocation::Stdlib { .. } => None,
        }
    }

    pub(crate) fn error_uri(&self) -> Option<String> {
        matches!(self.location, TemplateLocation::Stdlib { .. }).then(|| self.uri.clone())
    }
}

pub(crate) fn resolve_include(
    current: &TemplateAsset,
    path: &str,
    line: usize,
    col: usize,
) -> Result<TemplateAsset, TemplateError> {
    if let Some(asset) = TemplateAsset::stdlib(path) {
        return Ok(asset);
    }
    if crate::stdlib::asset_paths::stdlib_prompt_asset_path(path).is_some() {
        return Err(TemplateError::new(
            line,
            col,
            format!("unknown stdlib prompt asset '{path}'"),
        ));
    }
    match &current.location {
        TemplateLocation::Stdlib { base, path: parent } => {
            let joined = join_stdlib_relative(base, path).ok_or_else(|| {
                TemplateError::new(
                    line,
                    col,
                    format!("include path {path} is not a stdlib prompt asset"),
                )
            })?;
            TemplateAsset::stdlib(&format!("std/{joined}")).ok_or_else(|| {
                TemplateError::new(
                    line,
                    col,
                    format!("stdlib prompt asset std://{parent} includes missing std://{joined}"),
                )
            })
        }
        TemplateLocation::Inline {
            base,
            source_path,
            include_root,
        } => resolve_filesystem_include(
            path,
            source_path.as_deref(),
            base.as_deref(),
            include_root.as_deref(),
            line,
            col,
        ),
        TemplateLocation::Filesystem {
            path: current_path,
            base,
            include_root,
            ..
        } => resolve_filesystem_include(
            path,
            Some(current_path),
            base.as_deref(),
            include_root.as_deref(),
            line,
            col,
        ),
    }
}

pub(crate) fn parse_cached(asset: &TemplateAsset) -> Result<Rc<Vec<Node>>, TemplateError> {
    let key = CacheKey {
        id: asset.id.clone(),
        content_hash: blake3::hash(asset.source.as_bytes()).to_hex().to_string(),
    };
    if let Some(nodes) = TEMPLATE_CACHE.with(|cache| cache.borrow().entries.get(&key).cloned()) {
        return Ok(nodes);
    }
    let parsed = parse(&asset.source).map_err(|mut error| {
        if error.path.is_none() {
            error.path = asset.error_path();
        }
        if error.uri.is_none() {
            error.uri = asset.error_uri();
        }
        error
    })?;
    let nodes = Rc::new(parsed);
    TEMPLATE_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if !cache.entries.contains_key(&key) {
            if cache.order.len() >= TEMPLATE_CACHE_CAP {
                if let Some(oldest) = cache.order.pop_front() {
                    cache.entries.remove(&oldest);
                }
            }
            cache.order.push_back(key.clone());
        }
        cache.entries.insert(key, nodes.clone());
    });
    Ok(nodes)
}

#[cfg(test)]
pub(crate) fn reset_template_cache() {
    TEMPLATE_CACHE.with(|cache| *cache.borrow_mut() = TemplateCache::default());
}

#[cfg(test)]
pub(crate) fn template_cache_len() -> usize {
    TEMPLATE_CACHE.with(|cache| cache.borrow().entries.len())
}

fn resolve_filesystem_include(
    path: &str,
    current_path: Option<&Path>,
    base: Option<&Path>,
    include_root: Option<&Path>,
    line: usize,
    col: usize,
) -> Result<TemplateAsset, TemplateError> {
    let asset_ref_opt = crate::stdlib::asset_paths::parse(path);
    let resolved: PathBuf = if let Some(asset_ref) = &asset_ref_opt {
        let anchor = current_path
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(crate::stdlib::process::source_root_path);
        crate::stdlib::asset_paths::resolve(asset_ref, &anchor)
            .map_err(|msg| TemplateError::new(line, col, msg))?
    } else if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else if let Some(base) = base {
        base.join(path)
    } else {
        crate::stdlib::process::resolve_source_asset_path(path)
    };
    let canonical = resolved.canonicalize().unwrap_or_else(|_| resolved.clone());
    if asset_ref_opt.is_none() {
        if let Some(root) = include_root {
            if !canonical.starts_with(root) {
                return Err(TemplateError::new(
                    line,
                    col,
                    format!(
                        "include path {} escapes template root {}",
                        canonical.display(),
                        root.display()
                    ),
                ));
            }
        }
    }
    TemplateAsset::filesystem(resolved, "failed to read included template")
        .map_err(|message| TemplateError::new(line, col, message))
}

fn join_stdlib_relative(base: &str, rel: &str) -> Option<String> {
    if rel.is_empty() || rel.starts_with('/') || rel.contains('\\') {
        return None;
    }
    let mut parts = if base.is_empty() {
        Vec::new()
    } else {
        base.split('/').collect::<Vec<_>>()
    };
    for part in rel.split('/') {
        match part {
            "" | "." => {}
            ".." => return None,
            value => parts.push(value),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}
