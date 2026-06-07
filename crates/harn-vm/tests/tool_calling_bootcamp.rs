#![recursion_limit = "256"]
//! Tool-calling boot camp: a deterministic, zero-live-call battery that proves
//! Harn abstracts away provider/model tool-calling quirks behind a single
//! `tool_format` knob, with the north-star invariant:
//!
//!   every provider/model config input is either REJECTED with a clear error,
//!   OR accepted and resolves to a format the model can actually serve —
//!   never silently half-supported.
//!
//! The battery exercises the REAL resolution layer harness authors hit:
//!   - `agent_tool_format_resolution(opts)` / `agent_tool_format(opts)`
//!     (`std/agent/options`), which the agent-loop preset machinery and
//!     preflight all route through.
//!   - `provider_capabilities(provider, model)` / `provider_capabilities_install`
//!     (the capability matrix), used to set up synthetic parity cells so the
//!     hard-reject path is covered without touching the shipped catalog.
//!
//! Design: a pairwise-covering sample over the axes
//!   {capability-profile × requested-format × config-source}
//! kept to a few dozen cases. Each case asserts exactly one of:
//!   (a) resolution REJECTS (throws) with a message naming `tool_format`, or
//!   (b) resolution ACCEPTS and returns a concrete `native`/`text` that the
//!       capability matrix says the model serves.
//! No case is allowed to resolve to a format the matrix marks impossible, and
//! no case is allowed to resolve to a non-`{native,text}` string.
//!
//! Run with:
//!   CARGO_TARGET_DIR=/tmp/harn-target-bootcamp \
//!     cargo test -p harn-vm --test tool_calling_bootcamp

use harn_vm::value::VmError;

/// Run one Harn snippet through a fresh VM with the full stdlib registered,
/// returning Ok(stdout) or Err(error-string). A `throw` inside the snippet
/// surfaces as Err here, which is how we observe a "rejected" config.
fn run(source: &str) -> Result<String, String> {
    harn_vm::reset_thread_local_state();
    let chunk = harn_vm::compile_source(source)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    rt.block_on(async {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let mut vm = harn_vm::Vm::new();
                harn_vm::register_vm_stdlib(&mut vm);
                vm.execute(&chunk)
                    .await
                    .map_err(|e: VmError| format!("{e:?}"))?;
                Ok(vm.output().to_string())
            })
            .await
    })
}

/// `log()` lines emitted by the snippet, stripped of the `[harn] ` prefix.
fn lines(source: &str) -> Result<Vec<String>, String> {
    run(source).map(|raw| {
        raw.lines()
            .filter_map(|l| l.strip_prefix("[harn] ").map(str::to_string))
            .collect()
    })
}

/// Resolve `tool_format` for a single `(provider, model, requested)` cell and
/// echo the outcome. `requested` of `"auto"`/`""` means "omit / auto".
fn resolve_snippet(provider: &str, model: &str, requested: &str) -> String {
    let requested = if requested.is_empty() {
        "auto"
    } else {
        requested
    };
    let opts =
        format!(r#"{{model: "{model}", provider: "{provider}", tool_format: "{requested}"}}"#);
    format!(
        r#"
import {{ agent_tool_format_resolution, agent_tool_format }} from "std/agent/options"
pipeline main(task) {{
  let r = agent_tool_format_resolution({opts})
  log("tool_format=" + to_string(r.tool_format))
  log("source=" + to_string(r.source))
  log("effective=" + to_string(agent_tool_format({opts})))
}}
"#
    )
}

/// Outcome of resolving one cell.
enum Outcome {
    /// Rejected: resolution threw. The carried string is the error text.
    Rejected(String),
    /// Accepted: resolution returned a concrete format string.
    Accepted { tool_format: String },
}

fn resolve(provider: &str, model: &str, requested: &str) -> Outcome {
    match lines(&resolve_snippet(provider, model, requested)) {
        Err(err) => Outcome::Rejected(err),
        Ok(out) => Outcome::Accepted {
            tool_format: accepted_field(&out, "tool_format"),
        },
    }
}

/// Pull a `key=value` line emitted by a resolve snippet.
fn accepted_field(out: &[String], key: &str) -> String {
    out.iter()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
        .unwrap_or("")
        .to_string()
}

// ---------------------------------------------------------------------------
// Invariant 1 — reject-or-work-well over the requested-format axis.
//
// A requested `tool_format` is one of: omitted/auto, native, text, or an
// invalid token (typo / wrong value). Every accepted resolution must yield a
// concrete `native` or `text`; every invalid token must be rejected.
// ---------------------------------------------------------------------------

/// The requested-format axis. `None` = omitted/auto.
const REQUESTED_FORMATS: &[&str] = &[
    "auto",     // explicit auto sentinel
    "",         // omitted (treated as auto)
    "native",   // valid
    "text",     // valid
    "NATIVE",   // valid but uppercase — must normalize, not reject
    " text ",   // valid but padded — must normalize, not reject
    "nativ",    // typo — must reject
    "json",     // wrong value — must reject
    "tool_use", // wrong value (Anthropic wire term) — must reject
    "xml",      // wrong value — must reject
];

/// Representative real catalog cells spanning the capability profiles:
///   - anthropic native-tools frontier
///   - ollama text-only local (devstral)
///   - llamacpp qwen3.6 native
const REAL_CELLS: &[(&str, &str)] = &[
    ("anthropic", "claude-sonnet-4-6"),
    ("ollama", "devstral-small-2:24b"),
    ("llamacpp", "qwen3.6-35b-a3b-ud-q4-k-xl"),
];

fn is_invalid_token(requested: &str) -> bool {
    let norm = requested.trim().to_lowercase();
    !matches!(norm.as_str(), "" | "auto" | "native" | "text")
}

#[test]
fn requested_format_axis_rejects_or_resolves_concretely() {
    for &(provider, model) in REAL_CELLS {
        let (native_ok, text_ok, parity) = capability_facts(provider, model);
        for &requested in REQUESTED_FORMATS {
            let outcome = resolve(provider, model, requested);
            let label = format!("{provider}:{model} requested={requested:?} (parity={parity})");
            let norm = requested.trim().to_lowercase();
            // A valid token can still be an impossible *side* for this cell:
            // requesting "native" on a native_tools=false model, or "text" on
            // a text_tool_wire_format_supported=false model, is a hard reject
            // (the matrix says that side does not work) — never a silent
            // degrade. The invalid-token cases reject on syntax alone.
            let requests_impossible_side =
                (norm == "native" && !native_ok) || (norm == "text" && !text_ok);
            if is_invalid_token(requested) {
                match outcome {
                    Outcome::Rejected(err) => assert!(
                        err.contains("tool_format"),
                        "{label}: rejection should name tool_format; got {err}"
                    ),
                    Outcome::Accepted { tool_format, .. } => panic!(
                        "{label}: invalid token silently accepted as {tool_format:?} \
                         (reject-or-work-well violation)"
                    ),
                }
            } else if requests_impossible_side {
                match outcome {
                    Outcome::Rejected(_) => {}
                    Outcome::Accepted { tool_format, .. } => panic!(
                        "{label}: requested an impossible side but was accepted as \
                         {tool_format:?} (silent half-support)"
                    ),
                }
            } else {
                match outcome {
                    Outcome::Rejected(err) => {
                        panic!("{label}: serviceable request unexpectedly rejected: {err}")
                    }
                    Outcome::Accepted { tool_format, .. } => assert!(
                        tool_format == "native" || tool_format == "text",
                        "{label}: accepted but resolved to non-concrete {tool_format:?}"
                    ),
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Invariant 2 — accepted format never contradicts the capability matrix.
//
// For each real cell, an accepted explicit request must match what the matrix
// reports as servable: a model whose matrix says native_tools=false and
// text_tool_wire_format_supported=true must NOT silently accept "native" as a
// working config when parity marks it impossible — and must always be able to
// serve "text". This is the "consistent across formats" gate.
// ---------------------------------------------------------------------------

/// Read the capability facts the resolver keys off for a cell.
fn capability_facts(provider: &str, model: &str) -> (bool, bool, String) {
    let src = format!(
        r#"
pipeline main(task) {{
  let c = provider_capabilities("{provider}", "{model}")
  log("native=" + to_string(c.native_tools))
  log("text=" + to_string(c.text_tool_wire_format_supported))
  log("parity=" + to_string(c.tool_mode_parity))
}}
"#
    );
    let out = lines(&src).expect("capability lookup");
    (
        accepted_field(&out, "native") == "true",
        accepted_field(&out, "text") == "true",
        accepted_field(&out, "parity"),
    )
}

#[test]
fn accepted_format_is_consistent_with_capability_matrix() {
    for &(provider, model) in REAL_CELLS {
        let (native_ok, text_ok, parity) = capability_facts(provider, model);
        // A text-only model (no native tools) must always accept and resolve
        // "text", and its auto resolution must land on "text".
        if text_ok && !native_ok {
            if let Outcome::Accepted { tool_format, .. } = resolve(provider, model, "text") {
                assert_eq!(
                    tool_format, "text",
                    "{provider}:{model}: text-capable model dropped explicit text request"
                );
            } else {
                panic!("{provider}:{model}: text-capable model rejected a text request");
            }
            // Auto must pick the servable side, not silently choose native.
            if let Outcome::Accepted { tool_format, .. } = resolve(provider, model, "auto") {
                assert_eq!(
                    tool_format, "text",
                    "{provider}:{model}: auto resolved to {tool_format:?} on a text-only model \
                     (parity={parity})"
                );
            }
        }
        // A native-capable model must accept and resolve "native".
        if native_ok {
            if let Outcome::Accepted { tool_format, .. } = resolve(provider, model, "native") {
                assert_eq!(
                    tool_format, "native",
                    "{provider}:{model}: native-capable model dropped explicit native request"
                );
            } else {
                panic!("{provider}:{model}: native-capable model rejected a native request");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Invariant 3 — hard parity (`*_only`) rejects the impossible side.
//
// Synthetic capability cells with `native_only` / `text_only` parity exercise
// the hard-reject path that no shipped model currently has. A request for the
// side the matrix marks impossible MUST be rejected, not degraded.
// ---------------------------------------------------------------------------

/// Install a synthetic capability rule with the given parity, then resolve
/// while forcing the requested side with an explicit override reason.
fn resolve_with_parity_and_override(
    model: &str,
    parity: &str,
    native_tools: bool,
    requested: &str,
) -> Outcome {
    resolve_with_parity_impl(model, parity, native_tools, requested, true)
}

/// Install a synthetic capability rule with the given parity, then resolve.
fn resolve_with_parity(model: &str, parity: &str, native_tools: bool, requested: &str) -> Outcome {
    resolve_with_parity_impl(model, parity, native_tools, requested, false)
}

/// Shared body: install a synthetic capability rule under provider `bootcamp`
/// with the given derived/declared parity, then resolve `requested` against it.
/// When `with_override` is set, a `tool_format_override_reason` is supplied to
/// exercise the deliberate-force escape hatch. The install + resolve share one
/// snippet so the override is live during resolution; the trailing
/// `provider_capabilities_clear()` resets process-wide capability state.
fn resolve_with_parity_impl(
    model: &str,
    parity: &str,
    native_tools: bool,
    requested: &str,
    with_override: bool,
) -> Outcome {
    let preferred = if native_tools { "native" } else { "text" };
    let install = format!(
        r#"
[[provider.bootcamp]]
model_match = "{model}*"
native_tools = {native_tools}
preferred_tool_format = "{preferred}"
text_tool_wire_format_supported = true
tool_mode_parity = "{parity}"
"#
    );
    let override_line = if with_override {
        r#"tool_format_override_reason: "probe: deliberately forcing the marked-impossible side","#
    } else {
        ""
    };
    let src = format!(
        r#"
import {{ agent_tool_format_resolution }} from "std/agent/options"
pipeline main(task) {{
  provider_capabilities_install({install:?})
  let r = agent_tool_format_resolution({{
    model: "{model}",
    provider: "bootcamp",
    tool_format: "{requested}",
    {override_line}
  }})
  log("tool_format=" + to_string(r.tool_format))
  log("source=" + to_string(r.source))
  provider_capabilities_clear()
}}
"#
    );
    match lines(&src) {
        Err(err) => Outcome::Rejected(err),
        Ok(out) => Outcome::Accepted {
            tool_format: accepted_field(&out, "tool_format"),
        },
    }
}

#[test]
fn native_only_parity_rejects_text_request() {
    // native_only: the model can only do native tool calling. Asking for text
    // must be rejected, while native must be accepted.
    match resolve_with_parity("bootcamp-native-only-model", "native_only", true, "text") {
        Outcome::Rejected(err) => assert!(
            err.contains("text") && err.contains("native_only"),
            "expected native_only rejection naming text; got {err}"
        ),
        Outcome::Accepted { tool_format, .. } => panic!(
            "native_only model accepted text request as {tool_format:?} \
             (silent half-support)"
        ),
    }
    match resolve_with_parity("bootcamp-native-only-model", "native_only", true, "native") {
        Outcome::Accepted { tool_format, .. } => assert_eq!(tool_format, "native"),
        Outcome::Rejected(err) => panic!("native_only model rejected its own native side: {err}"),
    }
}

#[test]
fn text_only_parity_rejects_native_request() {
    // text_only: the model can only do text tool calling. Asking for native
    // must be rejected, while text must be accepted.
    match resolve_with_parity("bootcamp-text-only-model", "text_only", false, "native") {
        Outcome::Rejected(err) => assert!(
            err.contains("native") && err.contains("text_only"),
            "expected text_only rejection naming native; got {err}"
        ),
        Outcome::Accepted { tool_format, .. } => panic!(
            "text_only model accepted native request as {tool_format:?} \
             (silent half-support)"
        ),
    }
    match resolve_with_parity("bootcamp-text-only-model", "text_only", false, "text") {
        Outcome::Accepted { tool_format, .. } => assert_eq!(tool_format, "text"),
        Outcome::Rejected(err) => panic!("text_only model rejected its own text side: {err}"),
    }
}

#[test]
fn override_reason_forces_marked_impossible_side() {
    // The escape hatch: a probe/matrix harness may deliberately force the
    // catalog-marked-impossible side by recording a reason. This stays within
    // "never SILENTLY half-supported" — the override is explicit and recorded,
    // not a silent quirk. Both directions must now ACCEPT.
    match resolve_with_parity_and_override(
        "bootcamp-text-only-forced",
        "text_only",
        false,
        "native",
    ) {
        Outcome::Accepted { tool_format, .. } => assert_eq!(
            tool_format, "native",
            "override reason should force the requested native side on a text_only model"
        ),
        Outcome::Rejected(err) => {
            panic!("override reason failed to unlock the forced native side: {err}")
        }
    }
    match resolve_with_parity_and_override(
        "bootcamp-native-only-forced",
        "native_only",
        true,
        "text",
    ) {
        Outcome::Accepted { tool_format, .. } => assert_eq!(tool_format, "text"),
        Outcome::Rejected(err) => {
            panic!("override reason failed to unlock the forced text side: {err}")
        }
    }
}

#[test]
fn unreliable_parity_warns_but_does_not_reject() {
    // *_unreliable is recoverable, not impossible: the requested side still
    // works (with a warning the harness author can act on), so resolution must
    // ACCEPT it rather than reject. This is the boundary between the warn path
    // and the hard-reject path.
    match resolve_with_parity(
        "bootcamp-native-unreliable-model",
        "native_unreliable",
        true,
        "native",
    ) {
        Outcome::Accepted { tool_format, .. } => assert_eq!(
            tool_format, "native",
            "native_unreliable should still honor an explicit native request"
        ),
        Outcome::Rejected(err) => {
            panic!("native_unreliable wrongly hard-rejected the recoverable side: {err}")
        }
    }
}

// ---------------------------------------------------------------------------
// Invariant 4 — config-source axis: the knob behaves identically whether the
// format is pinned via explicit option or resolved from the catalog.
// ---------------------------------------------------------------------------

#[test]
fn explicit_and_catalog_paths_agree_on_servable_cells() {
    for &(provider, model) in REAL_CELLS {
        let (native_ok, text_ok, _) = capability_facts(provider, model);
        let auto = match resolve(provider, model, "auto") {
            Outcome::Accepted { tool_format, .. } => tool_format,
            Outcome::Rejected(err) => panic!("{provider}:{model}: auto resolution failed: {err}"),
        };
        // Whatever auto picks, an explicit pin of the same value must resolve
        // identically and be a format the matrix says is servable.
        if !auto.is_empty() {
            let explicit = match resolve(provider, model, &auto) {
                Outcome::Accepted { tool_format, .. } => tool_format,
                Outcome::Rejected(err) => {
                    panic!("{provider}:{model}: explicit {auto:?} rejected though auto chose it: {err}")
                }
            };
            assert_eq!(
                auto, explicit,
                "{provider}:{model}: auto and explicit disagree ({auto} vs {explicit})"
            );
            let servable = match auto.as_str() {
                "native" => native_ok,
                "text" => text_ok,
                _ => false,
            };
            assert!(
                servable,
                "{provider}:{model}: auto chose {auto:?} which the matrix marks unservable"
            );
        }
    }
}
