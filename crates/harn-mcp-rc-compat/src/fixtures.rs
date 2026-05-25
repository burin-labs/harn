//! Shared wire fixtures used by both directions of the RC harness.
//!
//! Each fixture is a deterministic JSON document loaded from disk so the
//! same request/response shapes can be replayed by Rust tests, Burin
//! Code consumers, and harn-cloud test suites without diverging.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value as JsonValue;

use crate::FIXTURE_ROOT;

/// One wire fixture: a name, an exchange of one or more JSON-RPC
/// documents, and a short description of why the case exists. The
/// `documents` array is interpreted by the loading test — server tests
/// replay requests and assert responses, client tests do the inverse.
#[derive(Clone, Debug)]
pub struct WireFixture {
    pub name: String,
    pub description: String,
    pub kind: WireFixtureKind,
    pub documents: Vec<JsonValue>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireFixtureKind {
    /// Exchange of `request → response` (and optional notifications).
    /// Tests can assert the server returns the expected response when
    /// sent the request, or the client emits the expected request when
    /// driven into the matching state.
    Exchange,
    /// HTTP-only fixture: documents alternate `headers → body` so a
    /// fake server can assert the streamable HTTP routing headers are
    /// validated against the JSON-RPC body.
    HttpHeaderExchange,
    /// Schema-only fixture (used by the recursive `$defs` check).
    Schema,
}

/// Resolve a fixture path relative to the workspace root. Walks up from
/// the current crate manifest so tests can be run from any working
/// directory.
pub fn fixture_path(name: &str) -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // `crates/harn-mcp-rc-compat` → workspace root is two parents up.
    manifest
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join(FIXTURE_ROOT).join(name))
        .expect("workspace root")
}

/// Load and parse every fixture under `spec/protocol-artifacts/fixtures/mcp-rc/`.
/// Returns them sorted by name so iteration order is deterministic.
pub fn all_fixtures() -> Vec<WireFixture> {
    let root = fixture_path("");
    let mut entries: Vec<PathBuf> = fs::read_dir(&root)
        .unwrap_or_else(|err| panic!("read fixture root {}: {err}", root.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort();
    entries.into_iter().map(load_fixture).collect()
}

pub fn load_fixture(path: PathBuf) -> WireFixture {
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read fixture {}: {err}", path.display()));
    let value: JsonValue = serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("parse fixture {}: {err}", path.display()));
    let name = value
        .get("name")
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("fixture {} missing name", path.display()))
        .to_string();
    let description = value
        .get("description")
        .and_then(JsonValue::as_str)
        .unwrap_or_else(|| panic!("fixture {} missing description", path.display()))
        .to_string();
    let kind = match value.get("kind").and_then(JsonValue::as_str) {
        Some("exchange") => WireFixtureKind::Exchange,
        Some("http_header_exchange") => WireFixtureKind::HttpHeaderExchange,
        Some("schema") => WireFixtureKind::Schema,
        Some(other) => panic!("fixture {} has unknown kind {other:?}", path.display()),
        None => panic!("fixture {} missing kind", path.display()),
    };
    let documents = value
        .get("documents")
        .and_then(JsonValue::as_array)
        .unwrap_or_else(|| panic!("fixture {} missing documents", path.display()))
        .clone();
    WireFixture {
        name,
        description,
        kind,
        documents,
    }
}

/// Convenience accessor that panics if the named fixture does not load.
/// Tests call this for the small set of well-known fixtures.
pub fn load_named(file_name: &str) -> WireFixture {
    load_fixture(fixture_path(file_name))
}
