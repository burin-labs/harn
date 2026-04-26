//! File-system watch host capability.
//!
//! Wraps `notify` to deliver coalesced file-change batches into Harn's
//! session-scoped `AgentEvent` stream.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use globset::{Glob, GlobSet, GlobSetBuilder};
use harn_vm::agent_events::{AgentEvent, FsWatchEvent};
use harn_vm::VmValue;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use notify::event::{ModifyKind, RenameMode};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};
use crate::tools::args::{
    build_dict, dict_arg, optional_bool, optional_int, optional_string, str_value,
};

const SUBSCRIBE_BUILTIN: &str = "hostlib_fs_watch_subscribe";
const UNSUBSCRIBE_BUILTIN: &str = "hostlib_fs_watch_unsubscribe";
const DEFAULT_DEBOUNCE_MS: u64 = 50;

static NEXT_SUBSCRIPTION_ID: AtomicU64 = AtomicU64::new(1);

/// File-watch capability handle.
#[derive(Default)]
pub struct FsWatchCapability;

impl HostlibCapability for FsWatchCapability {
    fn module_name(&self) -> &'static str {
        "fs_watch"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        registry.register(RegisteredBuiltin {
            name: SUBSCRIBE_BUILTIN,
            module: "fs_watch",
            method: "subscribe",
            handler: subscribe_handler(),
        });
        registry.register(RegisteredBuiltin {
            name: UNSUBSCRIBE_BUILTIN,
            module: "fs_watch",
            method: "unsubscribe",
            handler: unsubscribe_handler(),
        });
    }
}

fn subscribe_handler() -> SyncHandler {
    std::sync::Arc::new(subscribe)
}

fn unsubscribe_handler() -> SyncHandler {
    std::sync::Arc::new(unsubscribe)
}

struct Subscription {
    _watcher: RecommendedWatcher,
    stop_tx: mpsc::Sender<WatchMessage>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let _ = self.stop_tx.send(WatchMessage::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

enum WatchMessage {
    Event(Event),
    Error(String),
    Stop,
}

#[derive(Clone)]
struct WatchFilter {
    session_id: String,
    subscription_id: String,
    root: PathBuf,
    globs: Option<GlobSet>,
    gitignore: Option<Gitignore>,
    kinds: BTreeSet<String>,
}

fn subscriptions() -> &'static Mutex<HashMap<String, Subscription>> {
    static SUBSCRIPTIONS: OnceLock<Mutex<HashMap<String, Subscription>>> = OnceLock::new();
    SUBSCRIPTIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn subscribe(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(SUBSCRIBE_BUILTIN, args)?;
    let dict = raw.as_ref();
    let request = SubscribeRequest::from_dict(dict)?;
    let subscription_id = next_subscription_id();
    let (tx, rx) = mpsc::channel();

    let filter = WatchFilter {
        session_id: request.session_id.clone(),
        subscription_id: subscription_id.clone(),
        root: request.root.clone(),
        globs: request.globs,
        gitignore: request.gitignore,
        kinds: request.kinds,
    };
    let debounce = Duration::from_millis(request.debounce_ms);
    let worker = thread::Builder::new()
        .name(format!("harn-fs-watch-{subscription_id}"))
        .spawn(move || watch_worker(rx, debounce, filter))
        .map_err(|err| HostlibError::Backend {
            builtin: SUBSCRIBE_BUILTIN,
            message: format!("failed to spawn watch worker: {err}"),
        })?;

    let notify_tx = tx.clone();
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<Event>| {
        let message = match result {
            Ok(event) => WatchMessage::Event(event),
            Err(err) => WatchMessage::Error(err.to_string()),
        };
        let _ = notify_tx.send(message);
    })
    .map_err(|err| HostlibError::Backend {
        builtin: SUBSCRIBE_BUILTIN,
        message: format!("failed to create watcher: {err}"),
    })?;

    let mode = if request.recursive {
        RecursiveMode::Recursive
    } else {
        RecursiveMode::NonRecursive
    };
    for path in &request.watch_paths {
        watcher
            .watch(path, mode)
            .map_err(|err| HostlibError::Backend {
                builtin: SUBSCRIBE_BUILTIN,
                message: format!("failed to watch {}: {err}", path.display()),
            })?;
    }

    subscriptions()
        .lock()
        .expect("fs_watch mutex poisoned")
        .insert(
            subscription_id.clone(),
            Subscription {
                _watcher: watcher,
                stop_tx: tx,
                worker: Some(worker),
            },
        );

    Ok(build_dict([(
        "subscription_id",
        str_value(subscription_id.as_str()),
    )]))
}

fn unsubscribe(args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(UNSUBSCRIBE_BUILTIN, args)?;
    let dict = raw.as_ref();
    let subscription_id = match dict.get("subscription_id") {
        Some(VmValue::String(value)) if !value.trim().is_empty() => value.to_string(),
        Some(other) => {
            return Err(HostlibError::InvalidParameter {
                builtin: UNSUBSCRIBE_BUILTIN,
                param: "subscription_id",
                message: format!("expected non-empty string, got {}", other.type_name()),
            });
        }
        None => {
            return Err(HostlibError::MissingParameter {
                builtin: UNSUBSCRIBE_BUILTIN,
                param: "subscription_id",
            });
        }
    };
    let removed = subscriptions()
        .lock()
        .expect("fs_watch mutex poisoned")
        .remove(&subscription_id)
        .is_some();
    Ok(build_dict([("removed", VmValue::Bool(removed))]))
}

struct SubscribeRequest {
    session_id: String,
    root: PathBuf,
    watch_paths: Vec<PathBuf>,
    recursive: bool,
    debounce_ms: u64,
    globs: Option<GlobSet>,
    gitignore: Option<Gitignore>,
    kinds: BTreeSet<String>,
}

impl SubscribeRequest {
    fn from_dict(dict: &BTreeMap<String, VmValue>) -> Result<Self, HostlibError> {
        let root_param = optional_string(SUBSCRIBE_BUILTIN, dict, "root")?;
        let raw_paths = optional_string_list(SUBSCRIBE_BUILTIN, dict, "paths")?;
        let raw_globs = optional_string_list(SUBSCRIBE_BUILTIN, dict, "globs")?;
        let session_id = optional_string(SUBSCRIBE_BUILTIN, dict, "session_id")?
            .or_else(harn_vm::agent_sessions::current_session_id)
            .ok_or(HostlibError::MissingParameter {
                builtin: SUBSCRIBE_BUILTIN,
                param: "session_id",
            })?;

        if session_id.trim().is_empty() {
            return Err(HostlibError::InvalidParameter {
                builtin: SUBSCRIBE_BUILTIN,
                param: "session_id",
                message: "must not be empty".to_string(),
            });
        }

        if root_param.is_none() && raw_paths.is_none() {
            return Err(HostlibError::MissingParameter {
                builtin: SUBSCRIBE_BUILTIN,
                param: "root",
            });
        }

        let root = match root_param.as_deref() {
            Some(root) => normalize_existing_path(SUBSCRIBE_BUILTIN, "root", root)?,
            None => std::env::current_dir().map_err(|err| HostlibError::Backend {
                builtin: SUBSCRIBE_BUILTIN,
                message: format!("failed to resolve current directory: {err}"),
            })?,
        };

        let raw_paths = raw_paths.unwrap_or_else(|| {
            root_param
                .as_ref()
                .map(|root| vec![root.clone()])
                .unwrap_or_default()
        });
        if raw_paths.is_empty() {
            return Err(HostlibError::InvalidParameter {
                builtin: SUBSCRIBE_BUILTIN,
                param: "paths",
                message: "must contain at least one path".to_string(),
            });
        }

        let mut watch_paths = Vec::with_capacity(raw_paths.len());
        for path in raw_paths {
            let path = PathBuf::from(path);
            let resolved = if path.is_relative() && root_param.is_some() {
                root.join(path)
            } else {
                path
            };
            watch_paths.push(normalize_existing_path_buf(
                SUBSCRIBE_BUILTIN,
                "paths",
                &resolved,
            )?);
        }

        let recursive = optional_bool(SUBSCRIBE_BUILTIN, dict, "recursive", true)?;
        let debounce_ms = optional_int(
            SUBSCRIBE_BUILTIN,
            dict,
            "debounce_ms",
            DEFAULT_DEBOUNCE_MS as i64,
        )?;
        if debounce_ms < 0 {
            return Err(HostlibError::InvalidParameter {
                builtin: SUBSCRIBE_BUILTIN,
                param: "debounce_ms",
                message: "must be >= 0".to_string(),
            });
        }
        let respect_gitignore = optional_bool(SUBSCRIBE_BUILTIN, dict, "respect_gitignore", false)?;

        Ok(Self {
            session_id,
            gitignore: if respect_gitignore {
                Some(build_gitignore(&root))
            } else {
                None
            },
            globs: build_globs(raw_globs.unwrap_or_default())?,
            kinds: parse_kinds(dict)?,
            root,
            watch_paths,
            recursive,
            debounce_ms: debounce_ms as u64,
        })
    }
}

fn watch_worker(rx: mpsc::Receiver<WatchMessage>, debounce: Duration, filter: WatchFilter) {
    let mut pending = Vec::new();
    loop {
        match rx.recv() {
            Ok(WatchMessage::Event(event)) => {
                pending.push(event);
                loop {
                    match rx.recv_timeout(debounce) {
                        Ok(WatchMessage::Event(event)) => pending.push(event),
                        Ok(WatchMessage::Error(error)) => emit_watch_error(&filter, error),
                        Ok(WatchMessage::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                            emit_pending(&filter, &mut pending);
                            return;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                    }
                }
                emit_pending(&filter, &mut pending);
            }
            Ok(WatchMessage::Error(error)) => emit_watch_error(&filter, error),
            Ok(WatchMessage::Stop) | Err(_) => return,
        }
    }
}

fn emit_pending(filter: &WatchFilter, pending: &mut Vec<Event>) {
    if pending.is_empty() {
        return;
    }
    let events = coalesce_events(std::mem::take(pending), filter);
    if events.is_empty() {
        return;
    }
    harn_vm::agent_events::emit_event(&AgentEvent::FsWatch {
        session_id: filter.session_id.clone(),
        subscription_id: filter.subscription_id.clone(),
        events,
    });
}

fn emit_watch_error(filter: &WatchFilter, error: String) {
    harn_vm::agent_events::emit_event(&AgentEvent::FsWatch {
        session_id: filter.session_id.clone(),
        subscription_id: filter.subscription_id.clone(),
        events: vec![FsWatchEvent {
            kind: "error".to_string(),
            paths: Vec::new(),
            relative_paths: Vec::new(),
            raw_kind: "error".to_string(),
            error: Some(error),
        }],
    });
}

fn coalesce_events(events: Vec<Event>, filter: &WatchFilter) -> Vec<FsWatchEvent> {
    let mut seen = BTreeSet::new();
    let mut output = Vec::new();
    for event in events {
        let kind = normalize_kind(&event.kind);
        if !filter.kinds.contains(kind) {
            continue;
        }
        let mut paths = Vec::new();
        let mut relative_paths = Vec::new();
        for path in &event.paths {
            if !filter.matches_path(path) {
                continue;
            }
            paths.push(path_to_string(path));
            relative_paths.push(filter.relative_path(path));
        }
        if paths.is_empty() {
            continue;
        }
        paths.sort();
        paths.dedup();
        relative_paths.sort();
        relative_paths.dedup();
        let raw_kind = format!("{:?}", event.kind);
        if !seen.insert((kind.to_string(), paths.clone(), raw_kind.clone())) {
            continue;
        }
        output.push(FsWatchEvent {
            kind: kind.to_string(),
            paths,
            relative_paths,
            raw_kind,
            error: None,
        });
    }
    output
}

impl WatchFilter {
    fn matches_path(&self, path: &Path) -> bool {
        if let Some(gitignore) = &self.gitignore {
            if gitignore.matched(path, path.is_dir()).is_ignore() {
                return false;
            }
        }
        if let Some(globs) = &self.globs {
            let relative = self.relative_path(path);
            return globs.is_match(relative);
        }
        true
    }

    fn relative_path(&self, path: &Path) -> String {
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        let value = path_to_string(relative);
        if value.is_empty() {
            ".".to_string()
        } else {
            value
        }
    }
}

fn normalize_kind(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::Create(_) => "create",
        EventKind::Remove(_) => "remove",
        EventKind::Modify(ModifyKind::Name(
            RenameMode::Any
            | RenameMode::To
            | RenameMode::From
            | RenameMode::Both
            | RenameMode::Other,
        )) => "rename",
        EventKind::Modify(_) | EventKind::Any => "modify",
        EventKind::Access(_) => "access",
        EventKind::Other => "other",
    }
}

fn parse_kinds(dict: &BTreeMap<String, VmValue>) -> Result<BTreeSet<String>, HostlibError> {
    let values = optional_string_list(SUBSCRIBE_BUILTIN, dict, "kinds")?.unwrap_or_else(|| {
        vec![
            "create".to_string(),
            "modify".to_string(),
            "remove".to_string(),
            "rename".to_string(),
        ]
    });
    let mut kinds = BTreeSet::new();
    for kind in values {
        match kind.as_str() {
            "create" | "modify" | "remove" | "rename" => {
                kinds.insert(kind);
            }
            _ => {
                return Err(HostlibError::InvalidParameter {
                    builtin: SUBSCRIBE_BUILTIN,
                    param: "kinds",
                    message: format!("unsupported event kind `{kind}`"),
                });
            }
        }
    }
    Ok(kinds)
}

fn build_globs(globs: Vec<String>) -> Result<Option<GlobSet>, HostlibError> {
    if globs.is_empty() {
        return Ok(None);
    }
    let mut builder = GlobSetBuilder::new();
    for glob in globs {
        let normalized = normalize_glob(&glob);
        builder.add(
            Glob::new(&normalized).map_err(|err| HostlibError::InvalidParameter {
                builtin: SUBSCRIBE_BUILTIN,
                param: "globs",
                message: format!("invalid glob `{glob}`: {err}"),
            })?,
        );
    }
    Ok(Some(builder.build().map_err(|err| {
        HostlibError::InvalidParameter {
            builtin: SUBSCRIBE_BUILTIN,
            param: "globs",
            message: format!("invalid glob set: {err}"),
        }
    })?))
}

fn build_gitignore(root: &Path) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root);
    let gitignore = root.join(".gitignore");
    if gitignore.exists() {
        let _ = builder.add(gitignore);
    }
    let exclude = root.join(".git").join("info").join("exclude");
    if exclude.exists() {
        let _ = builder.add(exclude);
    }
    builder.build().unwrap_or_else(|_| Gitignore::empty())
}

fn normalize_glob(glob: &str) -> String {
    let glob = glob.replace('\\', "/");
    if glob == "*" || glob.starts_with("**/") || glob.contains('/') {
        glob
    } else {
        format!("**/{glob}")
    }
}

fn optional_string_list(
    builtin: &'static str,
    dict: &BTreeMap<String, VmValue>,
    key: &'static str,
) -> Result<Option<Vec<String>>, HostlibError> {
    let Some(value) = dict.get(key) else {
        return Ok(None);
    };
    match value {
        VmValue::Nil => Ok(None),
        VmValue::List(items) => items
            .iter()
            .enumerate()
            .map(|(idx, item)| match item {
                VmValue::String(value) => Ok(value.to_string()),
                other => Err(HostlibError::InvalidParameter {
                    builtin,
                    param: key,
                    message: format!("item {idx} must be a string, got {}", other.type_name()),
                }),
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        other => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected list of strings, got {}", other.type_name()),
        }),
    }
}

fn normalize_existing_path(
    builtin: &'static str,
    param: &'static str,
    path: &str,
) -> Result<PathBuf, HostlibError> {
    normalize_existing_path_buf(builtin, param, &PathBuf::from(path))
}

fn normalize_existing_path_buf(
    builtin: &'static str,
    param: &'static str,
    path: &Path,
) -> Result<PathBuf, HostlibError> {
    path.canonicalize()
        .map_err(|err| HostlibError::InvalidParameter {
            builtin,
            param,
            message: format!(
                "{} does not resolve to an existing path: {err}",
                path.display()
            ),
        })
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn next_subscription_id() -> String {
    let seq = NEXT_SUBSCRIPTION_ID.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("fsw-{millis}-{seq}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: EventKind, path: impl Into<PathBuf>) -> Event {
        Event::new(kind).add_path(path.into())
    }

    fn filter(root: PathBuf, globs: Option<Vec<&str>>) -> WatchFilter {
        WatchFilter {
            session_id: "session".to_string(),
            subscription_id: "sub".to_string(),
            root,
            globs: globs.map(|patterns| {
                build_globs(patterns.into_iter().map(str::to_string).collect())
                    .unwrap()
                    .unwrap()
            }),
            gitignore: None,
            kinds: parse_kinds(&BTreeMap::new()).unwrap(),
        }
    }

    #[test]
    fn coalesce_deduplicates_same_kind_and_path() {
        let root = std::env::current_dir().unwrap();
        let path = root.join("src/lib.rs");
        let filter = filter(root, None);
        let events = coalesce_events(
            vec![
                event(EventKind::Modify(ModifyKind::Any), &path),
                event(EventKind::Modify(ModifyKind::Any), &path),
            ],
            &filter,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "modify");
    }

    #[test]
    fn glob_filter_uses_relative_paths() {
        let root = std::env::current_dir().unwrap();
        let filter = filter(root.clone(), Some(vec!["*.rs"]));
        let events = coalesce_events(
            vec![
                event(
                    EventKind::Create(notify::event::CreateKind::Any),
                    root.join("src/lib.rs"),
                ),
                event(
                    EventKind::Create(notify::event::CreateKind::Any),
                    root.join("README.md"),
                ),
            ],
            &filter,
        );
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].relative_paths, vec!["src/lib.rs"]);
    }

    #[test]
    fn kind_filter_drops_unrequested_events() {
        let root = std::env::current_dir().unwrap();
        let mut filter = filter(root.clone(), None);
        filter.kinds = BTreeSet::from(["remove".to_string()]);

        let events = coalesce_events(
            vec![
                event(
                    EventKind::Create(notify::event::CreateKind::Any),
                    root.join("src/lib.rs"),
                ),
                event(
                    EventKind::Remove(notify::event::RemoveKind::Any),
                    root.join("src/lib.rs"),
                ),
            ],
            &filter,
        );

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "remove");
    }

    #[test]
    fn gitignore_filter_drops_ignored_paths() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(".gitignore"), "ignored.txt\n").unwrap();
        let mut filter = filter(temp.path().to_path_buf(), None);
        filter.gitignore = Some(build_gitignore(temp.path()));

        let events = coalesce_events(
            vec![
                event(
                    EventKind::Modify(ModifyKind::Any),
                    temp.path().join("allowed.txt"),
                ),
                event(
                    EventKind::Modify(ModifyKind::Any),
                    temp.path().join("ignored.txt"),
                ),
            ],
            &filter,
        );

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].relative_paths, vec!["allowed.txt"]);
    }
}
