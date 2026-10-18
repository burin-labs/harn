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
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::ast::Language;

use self::edits::{lower_edits, path_to_uri, Encoding, LineIndex, Position, WorkspaceEdit};
use self::servers::ServerSpec;
use self::transport::{Connection, RpcError};
use super::refactor_backend::{BackendOutcome, FileEdits, ServerRun, ServerStart};

#[cfg(test)]
pub(super) use self::servers::test_override;

const DEFAULT_STARTUP_MS: u64 = 30_000;
const DEFAULT_REQUEST_MS: u64 = 15_000;

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
    encoding: Encoding,
    prepare_rename: bool,
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
    let mut sessions = SESSIONS.lock().expect("lsp sessions poisoned");
    if let Some(existing) = sessions.get(&key) {
        if !existing.connection.exited() {
            return Ok((existing.clone(), ServerStart::Warm));
        }
        sessions.remove(&key);
    }
    let program = spec.resolve_program()?;
    let connection = Connection::spawn(spec, &program, root)?;
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
        .map_err(|err| format!("{} failed to initialize: {err}", spec.id))?;
    let rename = init
        .capabilities
        .rename_provider
        .unwrap_or(Value::Bool(false));
    if rename == Value::Bool(false) || rename.is_null() {
        return Err(format!("{} does not provide rename", spec.id));
    }
    connection
        .notify("initialized", json!({}))
        .map_err(|err| format!("{}: {err}", spec.id))?;
    let session = Arc::new(Session {
        encoding: Encoding::from_wire(init.capabilities.position_encoding.as_deref()),
        prepare_rename: rename
            .get("prepareProvider")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        connection,
        spec,
        op: Mutex::new(HashMap::new()),
    });
    sessions.insert(key, session.clone());
    Ok((session, ServerStart::Cold))
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
}

/// Whether a server is registered for `language`; no process is started.
pub(super) fn serves(language: Language) -> bool {
    servers::for_language(language).is_some()
}

pub(super) fn rename(request: &RenameRequest<'_>) -> (BackendOutcome, Option<ServerRun>) {
    let Some(spec) = servers::for_language(request.language) else {
        return (
            BackendOutcome::Unavailable(format!(
                "no language server is registered for {}",
                request.language.name()
            )),
            None,
        );
    };
    let started = Instant::now();
    let file = request.index_root.join(request.path);
    let Some(root) = spec.workspace_root(&file, request.index_root) else {
        return (
            BackendOutcome::Unavailable(format!(
                "{} found no project root for {}",
                spec.id, request.path
            )),
            None,
        );
    };
    let (session, start) = match session(
        spec,
        &root,
        started + budget("HARN_LSP_STARTUP_TIMEOUT_MS", DEFAULT_STARTUP_MS),
    ) {
        Ok(found) => found,
        Err(reason) => return (BackendOutcome::Unavailable(reason), None),
    };
    let ready_budget = match start {
        ServerStart::Cold => budget("HARN_LSP_STARTUP_TIMEOUT_MS", DEFAULT_STARTUP_MS),
        ServerStart::Warm => budget("HARN_LSP_REQUEST_TIMEOUT_MS", DEFAULT_REQUEST_MS),
    };
    let outcome = match session
        .connection
        .wait_ready(spec.readiness, started + ready_budget)
    {
        Ok(true) => session.rename(request, &file),
        Ok(false) => BackendOutcome::Unavailable(format!(
            "{} was still loading the workspace after {} ms; it keeps warming for the next call",
            spec.id,
            ready_budget.as_millis()
        )),
        Err(err) => BackendOutcome::Unavailable(format!("{}: {err}", spec.id)),
    };
    let run = ServerRun {
        server: spec.id,
        start,
        elapsed_ms: started.elapsed().as_millis() as u64,
    };
    (outcome, Some(run))
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
        let mut opened = Vec::new();
        let mut documents = vec![(file.to_path_buf(), request.source.to_string())];
        documents.extend(staged_documents(request, self.spec, file));
        for (path, text) in &documents {
            self.open(&mut versions, path, text, request.language);
            opened.push(path.clone());
        }
        let position = LineIndex::new(request.source).position(request.offset, self.encoding);
        let mut outcome = self.rename_at(request, file, position, timeout);
        if let Err(Stale(files)) = &outcome {
            // The server's view of some file is behind ours: hand it the
            // current text of every file it wanted to edit, then ask again.
            for path in files {
                if let Ok(text) = read_document(request, path) {
                    self.open(&mut versions, path, &text, request.language);
                    opened.push(path.clone());
                }
            }
            outcome = self.rename_at(request, file, position, timeout);
        }
        for path in opened {
            let _ = self.connection.notify(
                "textDocument/didClose",
                json!({"textDocument": {"uri": path_to_uri(&path)}}),
            );
        }
        match outcome {
            Ok(outcome) => outcome,
            Err(Stale(_)) => BackendOutcome::Unavailable(format!(
                "{} disagrees with the current file contents",
                self.spec.id
            )),
        }
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
        let id = self.spec.id;
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
                    self.spec.id,
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
        let id = self.spec.id;
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

/// Staged-but-uncommitted files the server should see instead of disk.
fn staged_documents(
    request: &RenameRequest<'_>,
    spec: &ServerSpec,
    seed: &Path,
) -> Vec<(PathBuf, String)> {
    let Some(session_id) = request.session_id else {
        return Vec::new();
    };
    let Ok(paths) = crate::fs::staged_pending_paths(session_id) else {
        return Vec::new();
    };
    paths
        .into_iter()
        .filter(|path| path != seed && path.starts_with(request.index_root))
        .filter(|path| {
            Language::detect(path, None)
                .and_then(servers::for_language)
                .is_some_and(|served| served.id == spec.id)
        })
        .filter_map(|path| {
            let text = crate::fs::read(&path, Some(session_id))?.ok()?;
            Some((path, String::from_utf8_lossy(&text).into_owned()))
        })
        .collect()
}

#[cfg(test)]
#[path = "rename_lsp_tests.rs"]
mod rename_lsp_tests;
