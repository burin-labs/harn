//! Language-server refactor backend.
//!
//! Servers start on first use, one per (server, workspace root), and stay
//! cached for the life of the process. A cold start that outlasts its budget
//! returns `Unavailable` but leaves the server warming, so the next call in
//! the same workspace is answered by the server.

mod edits;
mod servers;
mod transport;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::ast::Language;

use self::edits::{lower_edits, path_to_uri, Encoding, LineIndex, Position, WorkspaceEdit};
use self::servers::{Launch, Readiness, ServerSpec};
use self::transport::{Connection, RpcError};
use super::refactor_backend::{BackendOutcome, FileEdits, ServerRun, ServerStart};

#[cfg(test)]
pub(super) use self::servers::test_override;

const DEFAULT_STARTUP_MS: u64 = 30_000;
const DEFAULT_REQUEST_MS: u64 = 15_000;
/// How long a first operation waits for a project load to begin before
/// concluding the server does not report one.
/// Upper bound on files opened beside the declaration for one rename.
const MAX_EXTRA_DOCUMENTS: usize = 500;
/// Servers kept running at once; each can hold gigabytes for a large
/// workspace.
const MAX_SESSIONS: usize = 4;
const PROJECT_LOAD_GRACE: Duration = Duration::from_secs(2);

fn budget(var: &str, default_ms: u64) -> Duration {
    Duration::from_millis(
        std::env::var(var)
            .ok()
            .and_then(|raw| raw.parse().ok())
            .unwrap_or(default_ms),
    )
}

struct Session {
    connection: Connection,
    spec: &'static ServerSpec,
    launch: &'static Launch,
    encoding: Encoding,
    prepare_rename: bool,
    /// Set once the first operation has waited for the project to load.
    loaded: AtomicBool,
    /// For evicting the least recently used server past [`MAX_SESSIONS`].
    last_used: Mutex<Instant>,
    /// Serializes operations so document open/close pairs never interleave.
    op: Mutex<HashMap<PathBuf, i32>>,
}

type SessionKey = (&'static str, PathBuf);

static SESSIONS: LazyLock<Mutex<HashMap<SessionKey, Arc<Session>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InitializeResult {
    capabilities: ServerCapabilities,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ServerCapabilities {
    #[serde(default)]
    position_encoding: Option<String>,
    #[serde(default)]
    rename_provider: Option<Value>,
}

fn session(
    spec: &'static ServerSpec,
    root: &Path,
    deadline: Instant,
) -> Result<(Arc<Session>, ServerStart), String> {
    let key = (spec.id, root.to_path_buf());
    if let Some(found) = cached(&key) {
        return Ok((found, ServerStart::Warm));
    }
    // Start outside the cache lock: a cold start can take the whole budget,
    // and renames in other workspaces must not wait on it.
    let mut reasons = Vec::new();
    let mut started = None;
    for launch in spec.launches {
        match start(spec, launch, root, deadline) {
            Ok(session) => {
                started = Some(Arc::new(session));
                break;
            }
            Err(reason) => reasons.push(reason),
        }
    }
    let Some(started) = started else {
        return Err(reasons.join("; "));
    };
    let mut sessions = SESSIONS.lock().expect("lsp sessions poisoned");
    if let Some(existing) = sessions
        .get(&key)
        .filter(|found| !found.connection.exited())
    {
        // A concurrent call won the race; dropping ours stops its server.
        return Ok((existing.clone(), ServerStart::Warm));
    }
    sessions.insert(key.clone(), started.clone());
    while sessions.len() > MAX_SESSIONS {
        let Some(oldest) = sessions
            .iter()
            .filter(|(other, _)| **other != key)
            .min_by_key(|(_, session)| *session.last_used.lock().expect("lsp clock poisoned"))
            .map(|(other, _)| other.clone())
        else {
            break;
        };
        sessions.remove(&oldest);
    }
    Ok((started, ServerStart::Cold))
}

fn cached(key: &SessionKey) -> Option<Arc<Session>> {
    let mut sessions = SESSIONS.lock().expect("lsp sessions poisoned");
    let found = sessions.get(key)?.clone();
    if found.connection.exited() {
        sessions.remove(key);
        return None;
    }
    *found.last_used.lock().expect("lsp clock poisoned") = Instant::now();
    Some(found)
}

fn start(
    spec: &'static ServerSpec,
    launch: &'static Launch,
    root: &Path,
    deadline: Instant,
) -> Result<Session, String> {
    let id = launch.id;
    let program = launch.resolve_program(root)?;
    let connection = Connection::spawn(spec, launch, &program, root)?;
    let root_uri = path_to_uri(root);
    let init: InitializeResult = connection
        .request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "workspaceFolders": [{"uri": root_uri, "name": root.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()}],
                "initializationOptions": spec.initialization_options(),
                "capabilities": {
                    "general": {"positionEncodings": ["utf-8", "utf-16"]},
                    "textDocument": {
                        "rename": {"prepareSupport": true},
                        "synchronization": {"didSave": false},
                    },
                    "workspace": {
                        "configuration": true,
                        "workspaceEdit": {"documentChanges": true},
                        "workspaceFolders": true,
                    },
                    "window": {"workDoneProgress": true},
                    "experimental": {"serverStatusNotification": true},
                },
            }),
            deadline.saturating_duration_since(Instant::now()),
        )
        .map_err(|err| format!("{id} failed to initialize: {err}"))?;
    let rename = init
        .capabilities
        .rename_provider
        .unwrap_or(Value::Bool(false));
    if rename == Value::Bool(false) || rename.is_null() {
        return Err(format!("{id} does not provide rename"));
    }
    connection
        .notify("initialized", json!({}))
        .map_err(|err| format!("{id}: {err}"))?;
    Ok(Session {
        encoding: Encoding::from_wire(init.capabilities.position_encoding.as_deref()),
        prepare_rename: rename
            .get("prepareProvider")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        connection,
        spec,
        launch,
        loaded: AtomicBool::new(false),
        last_used: Mutex::new(Instant::now()),
        op: Mutex::new(HashMap::new()),
    })
}

/// A rename the language server should answer.
pub(super) struct RenameRequest<'a> {
    pub index_root: &'a Path,
    /// Index-relative path of the declaration.
    pub path: &'a str,
    pub language: Language,
    pub source: &'a str,
    /// Byte offset of the declaration's name in `source`.
    pub offset: usize,
    pub old_name: &'a str,
    pub new_name: &'a str,
    pub session_id: Option<&'a str>,
    /// Index-relative files that mention `old_name`.
    pub mentions: &'a [String],
}

/// Whether one server answers for both languages (TypeScript and TSX do).
pub(super) fn same_server(a: Language, b: Language) -> bool {
    match (servers::for_language(a), servers::for_language(b)) {
        (Some(left), Some(right)) => left.id == right.id,
        _ => false,
    }
}

/// Whether a server is registered for `language`; no process is started.
pub(super) fn serves(language: Language) -> bool {
    servers::for_language(language).is_some()
}

/// A server that answered the readiness check for a rename's file.
pub(super) struct Warmed {
    session: Arc<Session>,
    run: ServerRun,
}

/// Start or reuse the server for `path` and wait until it can answer. Call
/// it before taking the code-index lock: a cold start can take the whole
/// startup budget. `Err` carries why the server cannot answer.
pub(super) fn warm(
    index_root: &Path,
    path: &str,
    language: Language,
) -> Result<Warmed, (String, Option<ServerRun>)> {
    let Some(spec) = servers::for_language(language) else {
        return Err((
            format!("no language server is registered for {}", language.name()),
            None,
        ));
    };
    let started = Instant::now();
    let file = index_root.join(path);
    let Some(root) = spec.workspace_root(&file, index_root) else {
        return Err((format!("no {} project root contains {path}", spec.id), None));
    };
    let (session, start) = session(
        spec,
        &root,
        started + budget("HARN_LSP_STARTUP_TIMEOUT_MS", DEFAULT_STARTUP_MS),
    )
    .map_err(|reason| (reason, None))?;
    let ready_budget = match start {
        ServerStart::Cold => budget("HARN_LSP_STARTUP_TIMEOUT_MS", DEFAULT_STARTUP_MS),
        ServerStart::Warm => budget("HARN_LSP_REQUEST_TIMEOUT_MS", DEFAULT_REQUEST_MS),
    };
    let ready = session
        .connection
        .wait_ready(session.launch.readiness, started + ready_budget);
    let run = ServerRun {
        server: session.launch.id,
        start,
        elapsed_ms: started.elapsed().as_millis() as u64,
    };
    match ready {
        Ok(true) => Ok(Warmed { session, run }),
        Ok(false) => Err((
            format!(
                "{} was still loading the workspace after {} ms; it keeps warming for the next call",
                session.launch.id,
                ready_budget.as_millis()
            ),
            Some(run),
        )),
        Err(err) => Err((format!("{}: {err}", session.launch.id), Some(run))),
    }
}

pub(super) fn rename(request: &RenameRequest<'_>, warmed: &Warmed) -> (BackendOutcome, ServerRun) {
    let started = Instant::now();
    let file = request.index_root.join(request.path);
    let outcome = warmed.session.rename(request, &file);
    let mut run = warmed.run.clone();
    run.elapsed_ms += started.elapsed().as_millis() as u64;
    (outcome, run)
}

/// What the server resolves an identifier occurrence to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Resolution {
    /// A target starts at the renamed declaration's name.
    Seed,
    /// A target in the declaration's file spans its name without starting
    /// there (its module, an enclosing block): neither provably the
    /// declaration nor provably something else.
    Encloses,
    /// Some other declaration, located when the server said where.
    Elsewhere(Option<Target>),
    /// Nothing: code the server did not analyze (an unloaded module, an
    /// inactive `cfg`, an unexpanded macro).
    Unknown,
}

impl Resolution {
    pub(super) fn is_elsewhere(&self) -> bool {
        matches!(self, Self::Elsewhere(_))
    }
}

/// Where a definition answer points: canonical path and byte offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Target {
    pub path: PathBuf,
    pub offset: usize,
}

/// Ask the server what each `(index-relative path, byte offset)` site refers
/// to. `Err` carries why it could not answer.
/// With `stop_at_miss`, stops after the first site that is not
/// [`Resolution::Elsewhere`], so the result can be shorter than `sites`.
pub(super) fn resolve(
    request: &RenameRequest<'_>,
    warmed: &Warmed,
    sites: &[(String, usize)],
    stop_at_miss: bool,
) -> Result<Vec<Resolution>, String> {
    let file = request.index_root.join(request.path);
    warmed.session.resolve(request, &file, sites, stop_at_miss)
}

/// The first target's path and start, for [`Resolution::Elsewhere`].
type RawTarget = Option<(PathBuf, Position)>;

fn classify(answer: &Value, seed_file: &Path, seed: Position) -> (Resolution, RawTarget) {
    let targets: Vec<&Value> = match answer {
        Value::Array(items) => items.iter().collect(),
        Value::Null => Vec::new(),
        single => vec![single],
    };
    if targets.is_empty() {
        return (Resolution::Unknown, None);
    }
    let at = |position: Position| (position.line, position.character);
    let range = |target: &Value, pointer: &str| {
        target
            .pointer(pointer)
            .and_then(|range| serde_json::from_value::<edits::Range>(range.clone()).ok())
    };
    let mut encloses = false;
    let mut first = None;
    for target in &targets {
        let path = target
            .get("targetUri")
            .or_else(|| target.get("uri"))
            .and_then(Value::as_str)
            .and_then(|uri| edits::uri_to_path(uri).ok())
            .map(|path| path.canonicalize().unwrap_or(path));
        // The name's range: a link's selection range, a location's range.
        let name = range(target, "/targetSelectionRange").or_else(|| range(target, "/range"));
        if first.is_none() {
            first = path.clone().zip(name.map(|range| range.start));
        }
        if path.as_deref() != Some(seed_file) {
            continue;
        }
        if name.is_some_and(|range| range.start == seed) {
            return (Resolution::Seed, None);
        }
        encloses |= ["/targetRange", "/targetSelectionRange", "/range"]
            .iter()
            .filter_map(|pointer| range(target, pointer))
            .any(|range| at(range.start) <= at(seed) && at(seed) <= at(range.end));
    }
    if encloses {
        (Resolution::Encloses, None)
    } else {
        (Resolution::Elsewhere(None), first)
    }
}

/// Tell every cached server under `index_root` that Harn rewrote `paths`.
pub(super) fn files_written(index_root: &Path, paths: &[String]) {
    let sessions: Vec<(PathBuf, Arc<Session>)> = SESSIONS
        .lock()
        .expect("lsp sessions poisoned")
        .iter()
        .map(|((_, root), session)| (root.clone(), session.clone()))
        .collect();
    for (root, session) in sessions {
        let changes: Vec<Value> = paths
            .iter()
            .map(|path| index_root.join(path))
            .filter(|path| path.starts_with(&root))
            .map(|path| json!({"uri": path_to_uri(&path), "type": 2}))
            .collect();
        if !changes.is_empty() {
            let _ = session.connection.notify(
                "workspace/didChangeWatchedFiles",
                json!({"changes": changes}),
            );
        }
    }
}

impl Session {
    fn rename(&self, request: &RenameRequest<'_>, file: &Path) -> BackendOutcome {
        let mut versions = self.op.lock().expect("lsp op poisoned");
        let timeout = budget("HARN_LSP_REQUEST_TIMEOUT_MS", DEFAULT_REQUEST_MS);
        let ends_before = self.connection.progress_ends();
        let mut opened = match self.open_all(&mut versions, request, file) {
            Ok(opened) => opened,
            Err(reason) => return BackendOutcome::Unavailable(reason),
        };
        let position = LineIndex::new(request.source).position(request.offset, self.encoding);
        let mut outcome = match self.project_loading(ends_before, timeout) {
            Some(unavailable) => Ok(unavailable),
            None => self.rename_at(request, file, position, timeout),
        };
        if let Err(Stale(files)) = &outcome {
            // The server's view of some file is behind ours: hand it the
            // current text of every file it wanted to edit, then ask again.
            for path in files {
                let Ok(text) = read_document(request, path) else {
                    continue;
                };
                if opened.contains(path) {
                    self.close(path);
                } else {
                    opened.push(path.clone());
                }
                let language = Language::detect(path, None).unwrap_or(request.language);
                self.open(&mut versions, path, &text, language);
            }
            outcome = self.rename_at(request, file, position, timeout);
        }
        for path in opened {
            self.close(&path);
        }
        match outcome {
            Ok(outcome) => outcome,
            Err(Stale(_)) => BackendOutcome::Unavailable(format!(
                "{} disagrees with the current file contents",
                self.launch.id
            )),
        }
    }

    /// Open the declaration's file and every extra document; returns what
    /// was opened so the caller closes it.
    fn open_all(
        &self,
        versions: &mut HashMap<PathBuf, i32>,
        request: &RenameRequest<'_>,
        file: &Path,
    ) -> Result<Vec<PathBuf>, String> {
        let mut documents = vec![(
            file.to_path_buf(),
            request.source.to_string(),
            request.language,
        )];
        documents.extend(extra_documents(request, self.spec, file)?);
        let mut opened = Vec::new();
        for (path, text, language) in &documents {
            self.open(versions, path, text, *language);
            opened.push(path.clone());
        }
        Ok(opened)
    }

    /// What the server says each `(index-relative path, byte offset)` site
    /// refers to, with every relevant document open.
    fn resolve(
        &self,
        request: &RenameRequest<'_>,
        file: &Path,
        sites: &[(String, usize)],
        stop_at_miss: bool,
    ) -> Result<Vec<Resolution>, String> {
        let mut versions = self.op.lock().expect("lsp op poisoned");
        // One budget for the whole pass, not per site.
        let deadline = Instant::now() + budget("HARN_LSP_REQUEST_TIMEOUT_MS", DEFAULT_REQUEST_MS);
        let opened = self.open_all(&mut versions, request, file)?;
        let seed = LineIndex::new(request.source).position(request.offset, self.encoding);
        let seed_file = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
        let mut resolutions = Vec::with_capacity(sites.len());
        let mut failure = None;
        for (rel, offset) in sites {
            let path = request.index_root.join(rel);
            let text = match read_document(request, &path) {
                Ok(text) => text,
                Err(reason) => {
                    failure = Some(reason);
                    break;
                }
            };
            let position = LineIndex::new(&text).position(*offset, self.encoding);
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                failure = Some(format!(
                    "{} did not resolve {} remaining mentions in time",
                    self.spec.id,
                    sites.len() - resolutions.len()
                ));
                break;
            }
            let answer = self.connection.request::<_, Value>(
                "textDocument/definition",
                json!({"textDocument": {"uri": path_to_uri(&path)}, "position": position}),
                remaining,
            );
            match answer {
                Ok(answer) => {
                    let resolution = match classify(&answer, &seed_file, seed) {
                        (Resolution::Elsewhere(_), Some((path, position))) => {
                            let offset = read_document(request, &path).ok().and_then(|text| {
                                LineIndex::new(&text).offset(position, self.encoding)
                            });
                            Resolution::Elsewhere(offset.map(|offset| Target { path, offset }))
                        }
                        (resolution, _) => resolution,
                    };
                    let miss = !resolution.is_elsewhere();
                    resolutions.push(resolution);
                    if stop_at_miss && miss {
                        break;
                    }
                }
                Err(err) => {
                    failure = Some(format!("{} definition: {err}", self.spec.id));
                    break;
                }
            }
        }
        for path in opened {
            self.close(&path);
        }
        match failure {
            Some(reason) => Err(reason),
            None => Ok(resolutions),
        }
    }

    /// `Some` when the server is still loading the project the opened files
    /// belong to. Only the first operation expects a load to begin.
    fn project_loading(&self, ends_before: u64, timeout: Duration) -> Option<BackendOutcome> {
        if self.launch.readiness != Readiness::ProjectLoaded {
            return None;
        }
        let first = !self.loaded.swap(true, Ordering::Relaxed);
        let (expected, grace) = if first {
            (Some(ends_before), PROJECT_LOAD_GRACE)
        } else {
            (None, Duration::ZERO)
        };
        match self
            .connection
            .wait_progress(expected, grace, Instant::now() + timeout)
        {
            Ok(true) => None,
            Ok(false) => Some(BackendOutcome::Unavailable(format!(
                "{} was still loading the project after {} ms; it keeps loading for the next call",
                self.launch.id,
                timeout.as_millis()
            ))),
            Err(err) => Some(BackendOutcome::Unavailable(format!(
                "{}: {err}",
                self.launch.id
            ))),
        }
    }

    fn close(&self, path: &Path) {
        let _ = self.connection.notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": path_to_uri(path)}}),
        );
    }

    fn open(
        &self,
        versions: &mut HashMap<PathBuf, i32>,
        path: &Path,
        text: &str,
        language: Language,
    ) {
        let version = versions.entry(path.to_path_buf()).or_insert(0);
        *version += 1;
        let _ = self.connection.notify(
            "textDocument/didOpen",
            json!({"textDocument": {
                "uri": path_to_uri(path),
                "languageId": (self.spec.language_id)(language),
                "version": *version,
                "text": text,
            }}),
        );
    }

    fn rename_at(
        &self,
        request: &RenameRequest<'_>,
        file: &Path,
        position: Position,
        timeout: Duration,
    ) -> Result<BackendOutcome, Stale> {
        let id = self.launch.id;
        let document = json!({"textDocument": {"uri": path_to_uri(file)}, "position": position});
        if self.prepare_rename {
            // `null` and `{defaultBehavior: false}` both mean "not renameable".
            match self.call::<Value>("textDocument/prepareRename", document.clone(), timeout) {
                Ok(answer)
                    if answer.is_null()
                        || answer.get("defaultBehavior") == Some(&Value::Bool(false)) =>
                {
                    return Ok(BackendOutcome::Declined(format!(
                        "{id} reports nothing renameable at {}:{}",
                        request.path,
                        position.line + 1
                    )))
                }
                Ok(_) => {}
                Err(outcome) => return Ok(outcome),
            }
        }
        let mut params = document;
        params["newName"] = json!(request.new_name);
        let edit = match self.call::<Option<WorkspaceEdit>>("textDocument/rename", params, timeout)
        {
            Ok(Some(edit)) => edit,
            Ok(None) => {
                return Ok(BackendOutcome::Declined(format!(
                    "{id} found nothing to rename"
                )))
            }
            Err(outcome) => return Ok(outcome),
        };
        let per_file = match edit.into_file_edits() {
            Ok(per_file) => per_file,
            Err(detail) => return Ok(BackendOutcome::Unavailable(format!("{id}: {detail}"))),
        };
        self.changeset(request, per_file)
    }

    fn changeset(
        &self,
        request: &RenameRequest<'_>,
        per_file: BTreeMap<PathBuf, Vec<edits::TextEdit>>,
    ) -> Result<BackendOutcome, Stale> {
        let mut changeset = Vec::new();
        let mut stale = Vec::new();
        for (path, text_edits) in per_file {
            let Some(relative) = relative_to(request.index_root, &path) else {
                return Ok(BackendOutcome::Declined(format!(
                    "{} would edit {} outside the workspace",
                    self.launch.id,
                    path.display()
                )));
            };
            let source = match read_document(request, &path) {
                Ok(source) => source,
                Err(detail) => return Ok(BackendOutcome::Unavailable(detail)),
            };
            match lower_edits(&source, &text_edits, self.encoding) {
                // Every rename edit replaces the old spelling; anything else
                // means the server edited a different version of the file.
                Ok(edits) if edits.iter().all(|edit| edit.before == request.old_name) => {
                    if !edits.is_empty() {
                        changeset.push(FileEdits {
                            path: relative,
                            source,
                            edits,
                        });
                    }
                }
                _ => stale.push(path),
            }
        }
        if !stale.is_empty() {
            return Err(Stale(stale));
        }
        Ok(BackendOutcome::Changeset(changeset))
    }

    fn call<R: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<R, BackendOutcome> {
        let id = self.launch.id;
        let mut attempt = self.connection.request(method, params.clone(), timeout);
        if matches!(&attempt, Err(err) if err.retryable()) {
            attempt = self.connection.request(method, params, timeout);
        }
        attempt.map_err(|err| match err {
            RpcError::Server { ref message, .. } if !err.retryable() => {
                BackendOutcome::Declined(format!("{id}: {message}"))
            }
            other => BackendOutcome::Unavailable(format!("{id} {method}: {other}")),
        })
    }
}

struct Stale(Vec<PathBuf>);

fn relative_to(root: &Path, path: &Path) -> Option<String> {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path.strip_prefix(&root)
        .ok()
        .map(|rel| rel.to_string_lossy().replace('\\', "/"))
}

fn read_document(request: &RenameRequest<'_>, path: &Path) -> Result<String, String> {
    let bytes = match crate::fs::read(path, request.session_id) {
        Some(result) => result,
        None => std::fs::read(path),
    }
    .map_err(|err| format!("read `{}`: {err}", path.display()))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Files the server should see as Harn does besides the declaration's:
/// staged-but-uncommitted files and every file mentioning the name. A server
/// renames only files it has loaded, and its picture of an unopened file can
/// lag the disk or miss a file outside its project.
fn extra_documents(
    request: &RenameRequest<'_>,
    spec: &ServerSpec,
    seed: &Path,
) -> Result<Vec<(PathBuf, String, Language)>, String> {
    let staged: Vec<PathBuf> = request
        .session_id
        .and_then(|session_id| crate::fs::staged_pending_paths(session_id).ok())
        .into_iter()
        .flatten()
        .collect();
    let mut paths = staged;
    paths.extend(
        request
            .mentions
            .iter()
            .map(|rel| request.index_root.join(rel)),
    );
    paths.sort();
    paths.dedup();
    let served: Vec<(PathBuf, Language)> = paths
        .into_iter()
        .filter(|path| path != seed && path.starts_with(request.index_root))
        .filter_map(|path| {
            let language = Language::detect(&path, None)?;
            let served = servers::for_language(language)?;
            (served.id == spec.id).then_some((path, language))
        })
        .collect();
    // Opening only some would let the server rename from a stale view of
    // the rest, which no later check can see.
    if served.len() > MAX_EXTRA_DOCUMENTS {
        return Err(format!(
            "{} files mention the name; more than the {MAX_EXTRA_DOCUMENTS} a language-server rename opens",
            served.len()
        ));
    }
    Ok(served
        .into_iter()
        .filter_map(|(path, language)| {
            let text = read_document(request, &path).ok()?;
            Some((path, text, language))
        })
        .collect())
}

#[cfg(test)]
#[path = "rename_lsp_tests.rs"]
mod rename_lsp_tests;
