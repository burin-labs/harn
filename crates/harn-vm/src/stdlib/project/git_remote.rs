use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::value::VmValue;

use super::*;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct GitRemoteSignal {
    pub(super) name: String,
    pub(super) host: String,
    pub(super) slug: Option<String>,
    pub(super) redacted_url: String,
}

impl GitRemoteSignal {
    pub(super) fn into_vm_value(self) -> VmValue {
        let mut out = BTreeMap::new();
        out.insert("name".to_string(), VmValue::string(self.name));
        out.insert("host".to_string(), VmValue::string(self.host));
        out.insert(
            "slug".to_string(),
            self.slug.map(VmValue::string).unwrap_or(VmValue::Nil),
        );
        out.insert("url".to_string(), VmValue::string(self.redacted_url));
        VmValue::dict(out)
    }
}

pub(super) fn remote_signal_from_value(value: &VmValue) -> Option<GitRemoteSignal> {
    match value {
        VmValue::String(url) => remote_signal_from_url("origin", url),
        VmValue::Dict(dict) => {
            let name = optional_string_field(dict, "name").unwrap_or_else(|| "origin".to_string());
            let url = optional_string_field(dict, "url").unwrap_or_default();
            let host = optional_string_field(dict, "host")
                .or_else(|| remote_host(&url))
                .map(normalize_remote_host)
                .unwrap_or_default();
            let slug = optional_string_field(dict, "slug")
                .and_then(|slug| normalize_github_slug(&slug))
                .or_else(|| github_slug_from_remote(&url));
            if host.is_empty() && slug.is_none() && url.is_empty() {
                return None;
            }
            Some(GitRemoteSignal {
                name,
                host,
                slug,
                redacted_url: redact_remote_url(&url),
            })
        }
        _ => None,
    }
}

pub(super) fn detect_git_remote(dir: &Path) -> Option<GitRemoteSignal> {
    let git_path = find_git_path(dir)?;
    let mut remotes = Vec::new();
    for config in git_config_paths(&git_path) {
        let Some(text) = read_text_if_exists(config) else {
            continue;
        };
        remotes.extend(parse_git_config_remotes(&text));
    }
    remotes
        .iter()
        .find(|remote| remote.name == "origin")
        .cloned()
        .or_else(|| remotes.into_iter().next())
}

fn find_git_path(dir: &Path) -> Option<PathBuf> {
    let mut cursor = Some(dir);
    while let Some(path) = cursor {
        let git_path = path.join(".git");
        if git_path.exists() {
            return Some(git_path);
        }
        cursor = path.parent();
    }
    None
}

fn git_config_paths(git_path: &Path) -> Vec<PathBuf> {
    if git_path.is_dir() {
        return vec![git_path.join("config")];
    }
    let Some(git_dir) = read_gitdir_file(git_path) else {
        return Vec::new();
    };
    let mut out = vec![git_dir.join("config")];
    if let Some(common_dir) = read_commondir(&git_dir) {
        out.push(common_dir.join("config"));
    }
    out
}

fn read_gitdir_file(git_path: &Path) -> Option<PathBuf> {
    let text = read_text_if_exists(git_path.to_path_buf())?;
    let raw = text.trim().strip_prefix("gitdir:")?.trim();
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        Some(candidate)
    } else {
        Some(git_path.parent()?.join(candidate))
    }
}

fn read_commondir(git_dir: &Path) -> Option<PathBuf> {
    let raw = read_text_if_exists(git_dir.join("commondir"))?;
    let candidate = PathBuf::from(raw.trim());
    if candidate.is_absolute() {
        Some(candidate)
    } else {
        Some(git_dir.join(candidate))
    }
}

fn parse_git_config_remotes(config: &str) -> Vec<GitRemoteSignal> {
    let mut current_remote: Option<String> = None;
    let mut remotes = Vec::new();
    for line in config.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_remote = parse_remote_section(trimmed);
            continue;
        }
        let Some(name) = current_remote.as_deref() else {
            continue;
        };
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if key.trim() == "url" {
            if let Some(signal) = remote_signal_from_url(name, value.trim()) {
                remotes.push(signal);
            }
        }
    }
    remotes
}

fn parse_remote_section(section: &str) -> Option<String> {
    let inner = section.strip_prefix('[')?.strip_suffix(']')?.trim();
    let rest = inner.strip_prefix("remote")?.trim();
    let quoted = rest.strip_prefix('"')?;
    let (name, _) = quoted.split_once('"')?;
    Some(name.to_string())
}

fn remote_signal_from_url(name: &str, url: &str) -> Option<GitRemoteSignal> {
    let host = remote_host(url).map(normalize_remote_host)?;
    Some(GitRemoteSignal {
        name: name.to_string(),
        slug: github_slug_from_remote(url),
        host,
        redacted_url: redact_remote_url(url),
    })
}

fn remote_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.split_once("://").map(|(_, rest)| rest) {
        let authority = rest.split('/').next().unwrap_or_default();
        let host_port = authority.rsplit('@').next().unwrap_or(authority);
        let host = host_port.split(':').next().unwrap_or_default();
        return (!host.is_empty()).then(|| host.to_string());
    }
    if let Some((left, _path)) = trimmed.split_once(':') {
        let host = left.rsplit('@').next().unwrap_or(left);
        return (!host.is_empty()).then(|| host.to_string());
    }
    None
}

fn normalize_remote_host(host: String) -> String {
    let normalized = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if normalized == "github" {
        "github.com".to_string()
    } else {
        normalized
    }
}

pub(super) fn github_slug_from_remote(url: &str) -> Option<String> {
    let host = remote_host(url).map(normalize_remote_host)?;
    if host != "github.com" {
        return None;
    }
    let path = if let Some(rest) = url.split_once("://").map(|(_, rest)| rest) {
        rest.split_once('/')
            .map(|(_, path)| path)
            .unwrap_or_default()
            .to_string()
    } else {
        url.split_once(':')
            .map(|(_, path)| path)
            .unwrap_or_default()
            .to_string()
    };
    normalize_github_slug(&path)
}

fn normalize_github_slug(value: &str) -> Option<String> {
    let mut path = strip_url_suffix(value.trim())
        .trim_start_matches('/')
        .trim_end_matches('/')
        .to_string();
    if let Some(stripped) = path.strip_suffix(".git") {
        path = stripped.to_string();
    }
    let mut parts = path.split('/').filter(|part| !part.is_empty());
    let owner = parts.next()?;
    let repo = parts.next()?;
    Some(format!("{owner}/{repo}"))
}

pub(super) fn redact_remote_url(url: &str) -> String {
    let sanitized = strip_url_suffix(url.trim()).to_string();
    let Some((scheme, rest)) = sanitized.split_once("://") else {
        return sanitized;
    };
    let Some((userinfo, tail)) = rest.split_once('@') else {
        return sanitized;
    };
    if userinfo.is_empty() || tail.is_empty() {
        return sanitized;
    }
    format!("{scheme}://<redacted>@{tail}")
}

fn strip_url_suffix(value: &str) -> &str {
    let mut sanitized = value;
    for marker in ['?', '#'] {
        if let Some((head, _)) = sanitized.split_once(marker) {
            sanitized = head;
        }
    }
    sanitized
}
