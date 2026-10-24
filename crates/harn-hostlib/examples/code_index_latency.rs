//! Release benchmark through the production code-index restore interface.
//! Run only against a disposable copy: this creates fixtures and snapshots.

use harn_vm::value::VmDictExt as _;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use harn_hostlib::code_index::{
    CodeIndexCapability, CodeIndexSnapshot, IndexState, NodeKind, SNAPSHOT_FORMAT_VERSION,
};
use harn_hostlib::{BuiltinRegistry, HostlibCapability};
use harn_vm::VmValue;
use sha2::{Digest, Sha256};
use tracing::{span, Event, Metadata, Subscriber};

#[derive(Clone, Default)]
struct Timings(
    Arc<Mutex<BTreeMap<String, (u64, u64)>>>,
    Arc<Mutex<Vec<String>>>,
);

#[derive(Default)]
struct Fields {
    phase: Option<String>,
    elapsed: Option<u64>,
    path: Option<String>,
}

impl tracing::field::Visit for Fields {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "phase" {
            self.phase = Some(value.to_owned());
        }
        if field.name() == "path" {
            self.path = Some(value.to_owned());
        }
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "elapsed_us" {
            self.elapsed = Some(value);
        }
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

impl Subscriber for Timings {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == "harn_hostlib::code_index::timing"
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        if let Some(path) = fields.path {
            self.1.lock().unwrap().push(path);
        }
        if let (Some(phase), Some(elapsed)) = (fields.phase, fields.elapsed) {
            let mut totals = self.0.lock().unwrap();
            let total = totals.entry(phase).or_default();
            total.0 += elapsed;
            total.1 += 1;
        }
    }
}

fn graph_digest(state: &IndexState) -> String {
    let mut keys = HashMap::new();
    let mut nodes = Vec::new();
    for node in state.symbols.iter_nodes() {
        let bytes = serde_json::to_vec(&(
            node.kind.as_str(),
            &node.path,
            &node.name,
            node.line,
            &node.container,
            &node.signature,
            &node.access_level,
            &node.language,
        ))
        .unwrap();
        let key: [u8; 32] = Sha256::digest(bytes).into();
        keys.insert(node.id, key);
        nodes.push(key);
    }
    nodes.sort_unstable();
    let mut edges = Vec::new();
    for node in state.symbols.iter_nodes() {
        for edge in state.symbols.outgoing(node.id) {
            let key: [u8; 32] = Sha256::digest(
                serde_json::to_vec(&(keys[&edge.from], edge.kind.as_str(), keys[&edge.to]))
                    .unwrap(),
            )
            .into();
            edges.push(key);
        }
    }
    edges.sort_unstable();
    let mut digest = Sha256::new();
    for key in nodes.into_iter().chain(edges) {
        digest.update(key);
    }
    hex::encode(digest.finalize())
}

fn postings_digest(state: &IndexState) -> String {
    let mut digest = Sha256::new();
    for posting in state.trigrams.snapshot_postings() {
        let mut paths: Vec<_> = posting
            .files
            .iter()
            .map(|id| &state.files[id].relative_path)
            .collect();
        paths.sort_unstable();
        digest.update(serde_json::to_vec(&("trigram", posting.trigram, paths)).unwrap());
    }
    for posting in state.words.snapshot_postings() {
        let mut hits: Vec<_> = posting
            .hits
            .iter()
            .map(|(id, line)| (&state.files[id].relative_path, line))
            .collect();
        hits.sort_unstable();
        digest.update(serde_json::to_vec(&("word", posting.word, hits)).unwrap());
    }
    hex::encode(digest.finalize())
}

fn report(
    name: &str,
    trial: usize,
    start: Instant,
    timings: &Timings,
    state: &IndexState,
    root: &Path,
) {
    let elapsed_us = start.elapsed().as_micros();
    let phases = std::mem::take(&mut *timings.0.lock().unwrap());
    let operation_peak_rss_kib =
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find(|line| line.starts_with("VmHWM:"))
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|value| value.parse::<u64>().ok())
            });
    assert!(!phases.is_empty(), "phase timing path was not reached");
    let attempted = phases.get("ingest.total").map_or(0, |(_, count)| *count);
    let skipped = phases.get("ingest.skipped").map_or(0, |(_, count)| *count);
    let skipped_files = std::mem::take(&mut *timings.1.lock().unwrap());
    assert_eq!(skipped as usize, skipped_files.len());
    let ingested = attempted.checked_sub(skipped).unwrap();
    match name {
        "unchanged" => assert_eq!(ingested, 0, "unchanged restart did indexing work"),
        "body" | "declaration" | "resident_body" | "resident_declaration" => {
            assert_eq!(ingested, 1, "edit did not reach exactly one file");
        }
        "batch64" => assert_eq!(ingested, 64, "batch workload was not reached"),
        "head" | "cold" => assert_eq!(
            ingested as usize,
            state.files.len(),
            "full content check was not reached"
        ),
        _ => unreachable!(),
    }
    let targets = state
        .symbols
        .iter_nodes()
        .filter(|n| n.kind == NodeKind::CallSite)
        .map(|n| {
            state
                .symbols
                .outgoing(n.id)
                .iter()
                .filter(|e| e.kind.as_str() == "CALLS")
                .count()
        })
        .sum::<usize>();
    assert!(targets > 0, "the workload must contain resolved calls");
    let source_bytes: u64 = state.files.values().map(|file| file.size_bytes).sum();
    let mut file_facts: Vec<_> = state
        .files
        .values()
        .map(|file| (&file.relative_path, file.content_hash, file.size_bytes))
        .collect();
    file_facts.sort_unstable();
    let corpus_digest = hex::encode(Sha256::digest(serde_json::to_vec(&file_facts).unwrap()));
    println!(
        "{}",
        serde_json::json!({"scenario": name, "trial": trial, "elapsed_us": elapsed_us,
        "phases_us_and_count": phases, "files": state.files.len(), "nodes": state.symbols.node_count(),
        "edges": state.symbols.edge_count(), "resolved_calls": targets, "estimated_bytes": state.estimated_bytes(),
        "snapshot_bytes": std::fs::metadata(CodeIndexSnapshot::path_for(root)).ok().map(|m| m.len()),
        "operation_peak_rss_kib": operation_peak_rss_kib,
        "source_bytes": source_bytes, "corpus_digest": corpus_digest,
        "files_attempted": attempted, "files_skipped": skipped, "skipped_files": skipped_files,
        "graph_digest": graph_digest(state), "postings_digest": postings_digest(state)})
    );
}

#[allow(clippy::assertions_on_constants)] // Keep debug builds compilable, but refuse their timings.
fn main() {
    let args: Vec<_> = std::env::args_os().collect();
    let root = Path::new(args.get(1).expect("disposable corpus root"));
    let scenario = args.get(2).unwrap().to_str().unwrap();
    let trial: usize = args.get(3).unwrap().to_str().unwrap().parse().unwrap();
    assert!(
        root.is_absolute() && root.starts_with("/tmp"),
        "use a disposable OS-temp corpus"
    );
    assert_eq!(
        std::fs::read(root.with_extension("harn-code-index-benchmark"))
            .expect("mark the disposable corpus"),
        b"disposable\n",
        "the benchmark rewrites snapshots, its fixture, and Git HEAD"
    );
    assert!(
        !cfg!(debug_assertions),
        "this benchmark requires a release build"
    );
    let timings = Timings::default();
    tracing::subscriber::set_global_default(timings.clone()).unwrap();
    let fixture = root.join("harn_index_latency_fixture.rs");
    let original = "pub fn harn_latency_probe() -> u32 { 1 }\npub fn harn_latency_caller() -> u32 { harn_latency_probe() }\n";
    let base = CodeIndexSnapshot::path_for(root).with_file_name("release-base.json");
    if scenario != "cold" && args.get(4).is_some_and(|arg| arg == "prepare") {
        std::fs::write(&fixture, original).unwrap();
        let mut snapshot: CodeIndexSnapshot =
            serde_json::from_slice(&std::fs::read(&base).unwrap()).unwrap();
        // The JSON seed carries the facts, not a layout; persist it in this
        // build's format.
        snapshot.meta.format_version = SNAPSHOT_FORMAT_VERSION;
        let head = snapshot
            .meta
            .git_head
            .as_ref()
            .expect("initialize the disposable corpus as a Git repository");
        assert!(std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["update-ref", "HEAD", head])
            .status()
            .unwrap()
            .success());
        // Rewriting identical fixture bytes may change its stat; refresh just the
        // saved stat so the no-op case actually performs zero indexing work.
        let row = snapshot
            .files
            .iter_mut()
            .find(|f| f.relative_path == "harn_index_latency_fixture.rs")
            .unwrap();
        let metadata = std::fs::metadata(&fixture).unwrap();
        row.mtime_ms = metadata
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        row.size_bytes = metadata.len();
        match scenario {
            "unchanged" | "resident_body" | "resident_declaration" => {}
            "body" => {
                std::fs::write(&fixture, original.replace("{ 1 }", "{ 123456 }")).unwrap();
            }
            "declaration" => {
                std::fs::write(
                    &fixture,
                    original.replace("harn_latency_probe", "harn_latency_replacement"),
                )
                .unwrap();
            }
            "head" => {
                assert!(std::process::Command::new("git")
                    .arg("-C")
                    .arg(root)
                    .args([
                        "-c",
                        "commit.gpgsign=false",
                        "commit",
                        "--allow-empty",
                        "-m",
                        "benchmark-head-change"
                    ])
                    .output()
                    .unwrap()
                    .status
                    .success());
            }
            "batch64" => {
                let selected: Vec<(String, usize)> = serde_json::from_reader(
                    std::fs::File::open(base.with_file_name("benchmark-selected.json")).unwrap(),
                )
                .unwrap();
                assert_eq!(selected.len(), 64);
                for (path, _) in selected {
                    let file = snapshot
                        .files
                        .iter_mut()
                        .find(|f| f.relative_path == path)
                        .unwrap();
                    file.mtime_ms = -1;
                    file.content_hash = 0;
                }
            }
            _ => panic!("unknown scenario"),
        }
        snapshot.save(root).unwrap();
        drop(snapshot);
        return;
    }
    let capability = CodeIndexCapability::new();
    let mut start = Instant::now();
    if scenario == "cold" {
        std::fs::write(&fixture, original).unwrap();
        start = Instant::now();
        // `cold <trial> <workers>` compares fact-worker counts on one binary.
        let (state, outcome) = match args.get(4) {
            Some(workers) => IndexState::build_from_root_with_workers(
                root,
                workers.to_str().unwrap().parse().unwrap(),
            ),
            None => IndexState::build_from_root(root),
        };
        assert!(outcome.files_indexed > 8000);
        state.save_snapshot().unwrap();
        *capability.shared().lock().unwrap() = Some(state);
    } else {
        assert!(capability.restore_from_disk(root).unwrap());
    }
    let mut registry = BuiltinRegistry::new();
    capability.register_builtins(&mut registry);
    if scenario.starts_with("resident_") {
        timings.0.lock().unwrap().clear();
        timings.1.lock().unwrap().clear();
        let source = if scenario == "resident_declaration" {
            original.replace("harn_latency_probe", "harn_latency_replacement")
        } else {
            original.replace("{ 1 }", "{ 123456 }")
        };
        std::fs::write(&fixture, source).unwrap();
        let mut request = harn_vm::value::DictMap::new();
        request.put_str("path", fixture.to_str().unwrap());
        start = Instant::now();
        let result = (registry
            .find("hostlib_code_index_reindex_file")
            .unwrap()
            .handler)(&[VmValue::dict(request)])
        .unwrap();
        let VmValue::Dict(dict) = result else {
            panic!("reindex did not return a record");
        };
        assert!(matches!(dict.get("indexed"), Some(VmValue::Bool(true))));
    }
    let ready_us = start.elapsed().as_micros();
    let handler = &registry.find("hostlib_code_index_cypher").unwrap().handler;
    let mut request = harn_vm::value::DictMap::new();
    let expected_name = if scenario.ends_with("declaration") {
        "harn_latency_replacement"
    } else {
        "harn_latency_probe"
    };
    request.put_str(
        "query",
        format!("MATCH (f:Function {{name: '{expected_name}'}}) RETURN f.name"),
    );
    let query_start = Instant::now();
    let result = handler(&[VmValue::dict(request)]).unwrap();
    let query_us = query_start.elapsed().as_micros();
    let VmValue::Dict(dict) = result else {
        panic!("query did not return a record");
    };
    let Some(VmValue::List(rows)) = dict.get("rows") else {
        panic!("query did not return rows");
    };
    assert_eq!(rows.len(), 1, "the fresh declaration must be queryable");
    let mut request = harn_vm::value::DictMap::new();
    request.put_str("word", expected_name);
    let word_start = Instant::now();
    let word_result = (registry
        .find("hostlib_code_index_word_get")
        .unwrap()
        .handler)(&[VmValue::dict(request)])
    .unwrap();
    let word_query_us = word_start.elapsed().as_micros();
    let VmValue::List(hits) = word_result else {
        panic!("word query must return hits");
    };
    assert_eq!(hits.len(), 2, "both fixture occurrences must be present");
    let mut request = harn_vm::value::DictMap::new();
    request.put_str("query", expected_name);
    let trigrams = (registry
        .find("hostlib_code_index_extract_trigrams")
        .unwrap()
        .handler)(&[VmValue::dict(request)])
    .unwrap();
    let mut request = harn_vm::value::DictMap::new();
    request.insert(harn_vm::value::intern_key("trigrams"), trigrams);
    let trigram_start = Instant::now();
    let trigram_result = (registry
        .find("hostlib_code_index_trigram_query")
        .unwrap()
        .handler)(&[VmValue::dict(request)])
    .unwrap();
    let trigram_query_us = trigram_start.elapsed().as_micros();
    let VmValue::List(files) = trigram_result else {
        panic!("trigram query must return files");
    };
    let fixture_id = {
        let shared = capability.shared();
        let guard = shared.lock().unwrap();
        guard
            .as_ref()
            .unwrap()
            .lookup_path("harn_index_latency_fixture.rs")
            .unwrap()
    };
    assert!(
        files
            .iter()
            .any(|file| matches!(file, VmValue::Int(id) if *id == i64::from(fixture_id))),
        "trigram candidates must include the fresh fixture"
    );
    let mut request = harn_vm::value::DictMap::new();
    request.put_str("needle", expected_name);
    let literal_start = Instant::now();
    let literal_result =
        (registry.find("hostlib_code_index_query").unwrap().handler)(&[VmValue::dict(request)])
            .unwrap();
    let literal_query_us = literal_start.elapsed().as_micros();
    let VmValue::Dict(result) = literal_result else {
        panic!("literal query must return a record");
    };
    let Some(VmValue::List(results)) = result.get("results") else {
        panic!("literal query must return results");
    };
    assert_eq!(
        results.len(),
        1,
        "literal search must filter candidate false positives"
    );
    let VmValue::Dict(result) = &results[0] else {
        panic!("literal result must be a record");
    };
    assert!(
        matches!(result.get("match_count"), Some(VmValue::Int(2))),
        "both fixture occurrences must match"
    );
    // A restore publishes before rewriting its snapshot. Record when the
    // write lands too, so persistence cost stays visible.
    capability.wait_until_idle();
    let persisted_us = start.elapsed().as_micros();
    eprintln!(
        "ready scenario={scenario} trial={trial} ready_us={ready_us} persisted_us={persisted_us} query_us={query_us} word_query_us={word_query_us} trigram_query_us={trigram_query_us} literal_query_us={literal_query_us} rows={}",
        rows.len()
    );
    let shared = capability.shared();
    let guard = shared.lock().unwrap();
    let state = guard.as_ref().unwrap();
    // The actual host interface publishes a fresh index only after restoration.
    report(scenario, trial, start, &timings, state, root);
    if scenario == "cold" {
        // Preserve the next scenario's seed outside the measured operation.
        // The seed stays JSON whatever the persisted format is, so builds
        // with different snapshot layouts prepare from identical facts.
        let writer = std::io::BufWriter::new(std::fs::File::create(base).unwrap());
        serde_json::to_writer(writer, &state.snapshot()).unwrap();
    }
}
