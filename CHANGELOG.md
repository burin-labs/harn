# Changelog

Release notes for Harn v0.8 and newer are documented in this file.

Detailed pre-v0.8 release notes (v0.6.0 – v0.7.62) live in
[changelog/archive/CHANGELOG-pre-0.8.md](changelog/archive/CHANGELOG-pre-0.8.md).
Condensed pre-v0.6 highlights live in
[changelog/archive/CHANGELOG-pre-0.6.md](changelog/archive/CHANGELOG-pre-0.6.md).
Harn had no external users before 0.6.0, so that archive intentionally
keeps condensed series summaries instead of full per-patch history.

## v0.10.3

### Changed

- **Session store crate.** Durable session event, store, signing, SQLite,
  memory, and retention primitives now live in a reusable
  `harn-session-store` crate, with `harn-serve` reduced to the HTTP adapter
  and compatibility reexports (#4295).
- **GitHub trigger payloads.** Normalized GitHub events now preserve
  connector-promoted `reaction_topics` across the typed runtime payload
  boundary, including an empty list for events with no semantic reactions.

### Fixed

- **Agent loop same-resource fail-fast receipts are now typed (#4134).** Skipped
  sibling tool calls after a mutating same-resource failure include structured
  partial-apply metadata for the executed prefix, skipped dependent suffix,
  blocking prior call, mutation status, and next recovery action.
- Block textual git-destructive commands in Harn's universal catastrophic command floor.
- Give no-progress monologue hard stops one bounded tool-call recovery turn with structured recovery receipts before stopping.
- Warn on unknown provider catalog fields, including removed `fast_mode` rows,
  and fail generated catalog builds when source fragments would otherwise be
  silently ignored.
- Made agent judge-decision events carry an explicit `source` and stopped model-visible no-progress nudges
  from falling back to raw numeric counters.
- Warn on unknown provider-config fields in package `[llm]` sections and
  `[patch.models.*.batch]` tables instead of silently ignoring them.
- Forward `env_remove` through stdlib command helpers, agent command tools, and
  command-policy rewrites so host integrations can strip caller-selected child
  environment variables without replacing the rest of the inherited environment.
- `harn models lora` and `harn local` now canonicalize local runtime provider aliases such as `local-vllm`
  to the catalog-backed provider id before planning LoRA launches.
- Normalize local provider aliases in `harn models lora` export, manifest,
  preflight, and train receipts.

## v0.10.2

### Added

- `tools.run_command` now honors `env_remove`: a list of variable names stripped
  from the child's inherited environment before explicit `env` overrides apply
  (an explicit `env` entry for the same key still wins; no effect with
  `env_mode: "replace"`). Previously the key was silently ignored, and the
  v0.10.1 strict request validation rejected it outright.

### Changed

- Add first-class LoRA promotion probe cases to the evidence contract and CLI renderers.

### Fixed

- Make `std/command::command_run_streaming` enforce process timeouts through
  the background command wait/cancel contract and wait for timeout cancellation
  results before returning.
- Recover Harmony-framed text tool calls without dropping valid multi-call batches.
- **Python boundary checks now reject stale MCP helper shims.** The boundary allowlist no longer
  permits MCP helper filenames that were already cut over to Harn, and the remaining proxy fixture
  documents its real TCP-listener constraint instead of pointing at closed cutover work.

## v0.10.1

### Added

- Added `harn provider tool-scorecard` to aggregate saved tool-probe
  reports into deterministic provider/model tool-call quality scorecards.
- Add `harn mcp call` for one-shot stdio MCP tool probes and use it to replace
  Python conformance MCP driver helpers.
- **`std/agent/stall` gains two additive, default-off enrichment seams for the
  burin stuck-detector subsumption (#4228).** A per-turn `action_signal` fact
  callback lets the host report a boolean "this turn performed a flailing action"
  verdict (e.g. a desperation shell write, a truncated read, or an edit-oscillation
  result) that trips the stall on a SINGLE occurrence — no streak needed — with the
  distinct `flailing_action` warning pattern; consecutive flailing turns accumulate
  a dedicated counter (independent of the core consecutive-trip escalation, which
  resets every non-tripping turn) and escalate to a hard stop at
  `hard_stop_after_trips`. A `turns_since_clean_verify` int knob adds an
  elapsed-turns hard floor — a truly independent axis, active whether or not the
  repair-aware/post-edit-reverify modes are on — that counts every folded turn since
  the last clean verification (edit/read storms included, unlike the failing-verify
  `no_net_progress_hard_cap_after`) and forces a terminal stuck stop once the
  caller-set floor is crossed. Both knobs default to nil/off; when neither is
  configured the detector's decisions are byte-identical to before, so the live
  eval fleet's measurements are unchanged.
- **`std/agent/lanes` gains a per-stage tool-gate and `std/agent/pins` gains
  taxonomy/policy seams (#4230).** `stage_gate_label`/`stage_gate_parse`/
  `stage_gate_from_session`/`stage_gate_allows`/`stage_gate_denial` encode a
  lane/stage tool-gate into a task label so a downstream PreToolUse seam can
  recover the active workflow scope from the prompt itself; the gate fails OPEN
  (no kind resolver ⇒ never blocks) and an ungated node's label is byte-identical
  to before. `stage_gate_label` rejects any component containing a marker
  delimiter (space, `=`, `,`, `]`) loudly rather than silently truncating on the
  parse round-trip. `pin_register_kinds`, the extended `pin_reminder` options, and
  `pin_compaction_policy` merge/preserve-labels behavior extend the pin taxonomy;
  every new option collapses to its pre-seam expression when unset. These unblock
  the burin-code tool-gating and structural-compaction subsumptions.
- Added `harn models lora train` to render deterministic LoRA trainer receipts,
  optional backend launch argv, structural dataset audit counters, and
  post-training manifest commands.
- Added Harn-owned `std/coordination` schema/report helpers for coordination
  messages, receipts, inboxes, and wait-reply envelopes, plus `std/agent/events`
  schemas for captured events, tool lifecycle events, tool audit events, and
  typed checkpoints.
- Added `mcp_client_roots()` / `harn.mcp.client_roots()` for Harn-served MCP handlers to issue
  client `roots/list` requests without fixture-specific protocol code.
- Added a pure Harn remote eval fanout dry-run planner for stable shard,
  concurrency, artifact, and ledger-ingest receipts.
- Added `path_status(path, access?)` and `harness.fs.status(path, access?)` for
  structured filesystem visibility probes that distinguish missing paths from
  sandbox scope denial, read-only denial, and stat errors.
- Seam adoption enablers so hosts can adopt the rearch agent seams without adapters: a configurable
  stage-gate marker `prefix` on `stage_gate_label`/`stage_gate_parse`/`stage_gate_from_session`,
  independently-armable no-net-progress predicates (`stall_diagnostics.no_net_progress_predicates`), a
  cumulative-count return form for the `remediation_delivered` callback, and per-class completion-gate
  veto counters read via `agent_completion_gate_veto_counts`. All new options are byte-identical to prior
  behavior when unset.
- Added a pure `std/agent/governors` convergence guard contract for post-green
  finalization runaway detection, with typed fact vectors, issue-shaped
  verdicts, and auditable receipts.
- Added Harn-owned remote eval fanout trial receipts, artifact path rewrite
  manifests, and fail-closed rejoin receipts for missing, duplicate, unknown, or
  failed shards.
- Added a typed `prompt_cache_ttl` LLM/agent option so Anthropic routes can
  request the 1-hour prompt-cache TTL while preserving the default 5-minute cache behavior.
- Added `std/schema::schema_validator`, a reusable validator object for
  schema-backed `is`, `check`, `parse`, `report`, `expect`, error, issue, and
  JSON/OpenAPI schema operations.
- Added Harn stdlib helpers for one-step JSON parse plus schema validation.

### Changed

- Delete unused provider Python mock servers and classify remaining helper Python as Harn cutover debt.
- Model catalog provider metadata now distinguishes asynchronous Batch APIs from synchronous
  discounted or premium serving tiers such as Flex, Priority, and fast mode.
- Replaced the HTTP MCP echo conformance helper with a Harn-served MCP fixture.
- Replaced the HTTP MCP elicitation conformance helper with a Harn-served MCP fixture.
- `harn models lora` receipts now record PEFT `modules_to_save` and embedding/head
  save policy as part of the hashed LoRA contract.

### Fixed

- Command policy deny patterns now evaluate quote-aware shell command segments, including `sh -c`
  payloads and pipelines, so prefix-style denied commands cannot hide behind allowed compound
  commands.
- **Agent-loop hard-stop recovery receipts.** Thrash hard stops that have no
  matching recovery rung now emit a skipped `harn.agent_loop_recovery_receipt.v1`
  receipt with the uncovered stall pattern, so mechanism-liveness can
  distinguish an intentionally skipped ladder from an inert one (#4222).
- Normalize provider-facing tool and structured-output JSON schemas before strict provider request validation.
- **Git hooks now validate rebased PR branches against the base branch (#4246).**
  Workflow-only branches no longer inherit unrelated Rust or Harn changes from
  stale upstream ranges after a rebase, avoiding unnecessary broad Cargo and
  registry checks during pre-push.
- Fix stdlib metadata stripping for Harn v0.10 const-bound generated examples,
  add an audit guard for syntax-sensitive Harn keyword scans/examples, and keep
  highlight keyword hooks on the branch-source Harn binary.
- The `prefer-const` autofix (`mutable-never-reassigned`) no longer proposes
  tightening a destructuring `let` to `const` when only some of its bound
  names are never reassigned, which could have frozen a reassigned sibling.
  It now applies only to simple `let x = …` bindings. Two migration-stale
  messages that still said `var` (the shadow-variable lint suggestion and the
  redeclare-immutable runtime error) now say `let`.
- Reject streaming request rows in `harn models batch` manifest and prepare flows, and document OpenAI
  `/v1/responses` endpoint overrides for batch requests.
- Agent-orchestration seams now validate their config LOUDLY at config time
  instead of failing deep inside a run (or silently doing nothing).
  `agent_completion_gate` rejects a non-callable `facts` / `classify_write` /
  `verify_command` / `veto_combine`, a `classify_write` set without `facts`, and
  a `judge_seam` other than `"verify_completion_judge"` / `"done_judge"`. `goal`
  rejects a criterion `check` that is set but not callable, and `goal_reloop`
  rejects a non-callable `facts_fn` (both were silently dropped). A governor
  `policy.signal` outside `iterations` / `tokens` / `cost` now throws instead of
  silently metering iterations. Added `pace_action_of` to map
  `governor_pace_decision`'s native `proceed`/`extend`/`pace_check`/`cut`
  vocabulary onto the shared `proceed`/`warn`/`abort` governance vocabulary.
  `agent_completion_gate` also accepts a `feedback_templates` dict to override the
  coding-domain veto feedback prose per ladder rule (validated at config time;
  defaults unchanged).
- Local Makefile and package/measurement scripts now pair isolated Cargo target
  directories with isolated Cargo build directories, avoiding shared build-script
  output races during parallel validation.
- **Merge-queue Rust tests now use a lower build parallelism cap (#4302).**
  The queue lane halves `CARGO_BUILD_JOBS` after capped hosted runners still
  OOMed during Rust compile, improving release queue reliability on the
  standard 16 GB runner.
- Added a hook-wide no-local-build mode so eval-window commits and pushes can keep cheap guards
  without spawning local Cargo or Harn builds.
- Hardened the Linux Rust CI disk reclaim step so large affected-crate test closures have more room to link.
- **Hostlib command floor.** Scoped build-directory cleanups wrapped in
  `sh -c` are no longer mistaken for project-root deletes when a later command
  in the same shell script references `.`.
- Allow annotated mutating tools to declare dependency key arguments so same-file
  edit batches skip only genuinely dependent siblings after an applied failure.
- Added a reusable `agent_done_contract` helper so Harn agents can require a completion claim and
  receive judge `specific_gaps` repair feedback before yielding.
- The coding-agent eval harness and stdlib file writers now avoid redundant `mkdir` calls on an
  already-mounted sandbox workspace root, restoring mock-matrix release audit coverage under strict
  workspace-root write enforcement.
- Agent feedback-injection events now report the injected feedback text as `content` and carry no-progress streak counts separately.
- Validate hostlib request payloads against Harn-owned JSON Schemas before dispatching registered VM builtins.
- Ad hoc Makefile Harn CLI fallbacks now isolate Cargo's intermediate build
  directory under the active target directory, avoiding stale shared build
  artifacts when generated-artifact checks source-build the CLI.
- Added `tool_names` to the live agent post-turn payload so attempted tool use
  has a canonical Harn-owned contract instead of being inferred downstream.
- Reap escaped subprocess descendants on command timeout/cancel instead of only signalling the original process group.
- Fixed release/package audit hermeticity when temporary directories are
  redirected inside the checkout, and prevented project-profile Git/GitHub
  detection from inheriting parent scratch-checkout metadata.
- Release audit now runs Harn/script lanes through a stable copied `harn` binary
  instead of Cargo's relinked target path, avoiding `(deleted)` self-spawn
  failures while Rust audit lanes rebuild in parallel.
- Made Harn VM event-log and serving-tier unit tests independent of ambient release harness state.
- Fixed local type aliases used as runtime schema values in user-defined wrapper calls, matching
  imported public aliases and schema-aware builtins.
- Accept named function callbacks in agent stage-gate `kind_of` and `suggest` options.
- Fixed tree-sitter Harn parsing for multiline function and tool parameter lists with default values and trailing commas.

### Security

- Harden scoped filesystem writes on Windows: the parent-directory walk now
  opens each component with `FILE_FLAG_OPEN_REPARSE_POINT` and refuses any
  junction or symlink reparse point mid-walk, substantially narrowing the
  junction-traversal bypass that `O_NOFOLLOW` cannot cover on Windows. (A
  concurrent-swap TOCTOU window remains on Windows pending a handle-relative
  walk; the unix fd-walk is not affected.)
- Add a recurrence-guard test that forbids raw path-based `create_dir_all` /
  `File::create` / `OpenOptions` (and other path-resolving `std::fs`/`libc`
  calls) inside the scoped-walk and content-open helpers, and asserts every
  scoped leaf open keeps `O_NOFOLLOW`.

## v0.10.0

### Breaking

- **Variable-binding keywords now follow the TypeScript/Swift convention.**
  `const` is the immutable default binding (formerly `let`) and `let` is the
  mutable binding (formerly `var`); `var` is removed and now produces a
  migration diagnostic. `const` is a normal immutable binding that accepts any
  initializer — the old strict compile-time-constant rejection is gone (pure
  initializers are still folded as a transparent optimization). Every `.harn`
  source must be migrated; see `docs/src/migrations/const-let.md` for the
  automated `harn codemod` rules.

### Added

- Added opt-in raw provider request/response sidecars for LLM transcript debugging via `HARN_LLM_TRANSCRIPT_RAW=1`.

### Changed

- Move `std/llm/tool_binder` and `std/llm/structural_validator` public option validation
  onto the Harn schema runtime, and preserve `std/schema` required-field markers through schema
  builder helpers.
- Replace the MCP file-upload conformance Python helper with a Harn-served fixture.

### Fixed

- `harn run <bare-filename>` from a project root now resolves top-level
  `@asset`/relative prompt paths against the project even when a
  `[dependencies]` provider connector is installed. The entry pipeline's source
  dir is now always established (falling back to the working directory) and
  re-asserted immediately before execution, so a dependency provider-connector
  contract load during startup can no longer leave the resting source dir
  pointing at `.harn/packages/<dep>/`.

## v0.9.22

### Added

- **Agent transcript normalizers now expose schema-backed diagnostics (#4211).**
  `std/agent/transcript` can return normalized rows with structured validation
  issues for malformed records and publishes reusable row/report schemas for
  downstream eval and analyzer code.
- **Harn-served MCP scripts can now declare server metadata (#4219).** The new
  `mcp_server_metadata({name?, version?, instructions?})` builtin lets
  script-driven `harn serve mcp` endpoints advertise stable identity and
  client instructions without custom helper servers.
- **`harn codemod` rules can now rewrite a single sub-node of a match.** A raw
  `query` atomic matcher (used verbatim, must bind `@__match`) plus a `fixTarget`
  field splice the `fix` over only a named capture's span, leaving the rest of the
  node byte-for-byte intact — closing the data-loss gap where a whole-match
  `pattern`+`fix` dropped un-captured parts (e.g. a binding's `: Type` annotation).
- **Sandbox write roots and portable document PDF rendering.** `harn run` and
  `harn time run` now accept repeatable `--write-root` / `--writable-root`
  flags for sandboxed writes to declared external output folders, and
  `std/document` adds dependency-free text, HTML, and Markdown-to-PDF helpers
  backed by Harn's built-in `document_render_pdf` primitive.
- `std/testing` gained `assert_snapshot(name, actual, options?)`, a golden-file
  snapshot assertion in the style of Jest's `toMatchSnapshot` / insta. Goldens
  live at `__snapshots__/<name>.harn.snap` next to the test by default (override
  with `options.dir`); running with `HARN_UPDATE_SNAPSHOTS=1` writes them and
  drift fails with a unified diff. In CI (`CI`/`HARN_CI` set) the
  `HARN_UPDATE_SNAPSHOTS` trigger is ignored — the primitive compares only and
  never writes, so a leaked update flag can't turn the gate into a silent no-op.
  The explicit `options.update = true` seam stays honored (deliberate in-source
  code, used to drive the write path in tests). `options.redact` accepts
  `{pattern, replacement?}` regex scrubs applied before write/compare.

### Changed

- **LoRA serving contracts are now machine-readable (#4214).** `harn models
  lora plan --json`, `harn models lora manifest --json`, and `harn models lora
  inspect --json` expose `serving.serving_requirements` entries for parser
  ownership, vLLM tool-parser flags, chat-template ids, manifest metadata, and
  promotion gates.
- Promote the settled re-architecture orchestration stdlib surfaces from
  `@api_stability: experimental` to `stable`: `std/agent/{pins,goal,lanes,overlays,host_tools}`
  (including `agent_edit_tools`) and `std/workflow/repair`. The governor / stall /
  judge / agent-loop-options surfaces stay experimental while #3943 (editless-stop
  eval convergence) is in flight.
- **Connector secrets and inline runner ergonomics.** `harn run` now installs
  the configured secret provider chain for package-scoped scripts by default,
  connector OAuth secrets share canonical `namespace/name` parsing and exported
  token-name constants, and `harn run -e` accepts complete Harn programs with
  pipeline entrypoints instead of always wrapping inline source as a snippet.
- **Workspace-local scratch directories.** `harness.fs.workspace_temp_dir()` and
  `harness.fs.mkdtemp_in_workspace(prefix?)` let sandboxed workflows create
  intermediate files inside the active workspace instead of escaping to host
  temp paths.
- **String scanning ergonomics.** `string.rfind(substr)` is now a typed alias
  for `string.last_index_of(substr)`.
- **`release_ship.sh` now refuses to ship unfolded changelog fragments.** A new
  `require_no_unfolded_fragments` preflight (run first in every release mode,
  before the audit) fails loud with the exact remediation if
  `changelog.d/<id>.<category>.md` fragments remain, instead of silently cutting
  a release whose notes omit them. Closes the gap where invoking `release_ship`
  directly (bypassing the `release_harn` fold) produced empty release notes.

### Fixed

- Register `harn models batch ... --json` commands in the `harn --json-schemas`
  catalog.
- **Agent loop hard-stop recovery receipts (#4231).** Repeated-tool hard stops now get
  one bounded recovery turn with a typed `harn.agent_loop_recovery_receipt.v1`
  checkpoint, and final-wrapup tool-call leaks are reported instead of being silent.
- **OpenAI-compatible text-tool wrapper recovery (#4235).** Harn now recovers
  complete text-tool calls when a provider returns a generic `tool_call`
  wrapper with a malformed provider-boundary close suffix.
- Dependency package loads no longer leak the thread-local source dir, so
  top-level `@asset`/relative prompt resolution anchors on the project root even
  when `[dependencies]` are present.

### Security

- Harden scoped content writes so parent directory creation carries the final
  parent file descriptor into the write path and rejects excessive path depth.

## v0.9.21

### Added

- **Model batch planning now exposes provider operational notes (#3872).**
  `provider_capabilities`, `llm_catalog`, and `harn models batch plan --json`
  now report provider-specific submit/retry/rejoin constraints so offline
  batch runners can stay provider-neutral.
- **Optional subscript access now supports `?.[...]` syntax.** Harn code can use
  `obj?.["content-type"]` and `items?.[0]` as the canonical optional subscript
  spelling; the older `obj?[key]` form remains accepted for compatibility.

### Changed

- **Connector secrets and inline runner ergonomics.** `harn run` now installs
  the configured secret provider chain for package-scoped scripts by default,
  connector OAuth secrets share canonical `namespace/name` parsing and exported
  token-name constants, and `harn run -e` accepts complete Harn programs with
  pipeline entrypoints instead of always wrapping inline source as a snippet.

### Fixed

- **Project fingerprint language ranking.** `project_fingerprint` now ranks
  actual source-file languages ahead of manifest-only tooling hints, so
  frontend build files no longer make PHP or Ruby projects look TypeScript-first
  and C++ projects with `.h` headers no longer look C-first (#4195).
- Restored the `mkdir -p` contract for content-producing filesystem writes.
  The scoped-write hardening first shipped in v0.9.18 (#4147) resolved parent
  directories at open time via a symlink-safe parent-fd walk, but that walk
  required every ancestor directory to already exist. As a result, `write_file`,
  `write_file_bytes`, `append_file`, `harness.fs.write_text`, and `http_download`
  could fail with "No such file or directory" when writing into a not-yet-created
  directory. These writes now recreate missing ancestor directories in the
  sandboxed path, unrestricted path, and test overlay fallback. Structural
  operations (copy, move, remove, single `mkdir`) keep their "parent must already
  exist" semantics.

## v0.9.20

### Fixed

- Salvage harmless tagged-response envelope drift by canonicalizing stray prose,
  default done-sentinel text, single recovered bare tool calls, and call-shaped
  native tool names into repairable transcript state.

## v0.9.19

### Added

- Documented the canonical `harn serve acp` launch path for external ACP editor hosts and the Homebrew install channel for registry distribution.
- Carried caller-provided verification snapshot bindings through background `tools.run_command` handles, progress items, and `tools.wait_command` results.
- Added a config-declared `tools.toolchain_facts` hostlib builtin for verification profiles to capture toolchain version, cache environment, and state-path facts without hardcoded language adapters.
- Added `harn models batch cancel`, producing durable provider-neutral cancellation receipts for submitted batch jobs.
- Added typed provider-catalog batch lifecycle metadata for asynchronous model Batch APIs, including limits, result ordering, partial-failure semantics, cancellation support, retention, and safe storage notes.
- Added normalized batch receipt lifecycle objects to prepare, submit, status, and download receipts so eval/corpus runners can consume one provider-neutral state-machine contract.
- **Project scanning now recognizes Composer PHP projects (#4170).** Composer scripts, Laravel, Pest, and PHPUnit facts are surfaced for downstream harnesses and coding-agent workflows.

### Changed

- **Provider catalog route rows.** The provider catalog now exposes Harn-owned `routing_routes` rows for provider/model/family/capability/timeout routing decisions, giving Cloud and Burin a shared catalog contract instead of duplicating route schemas (burin-labs/harn-cloud#1120).
- Port the PR affected-crate selector from Python to Harn and remove the last repo-owned Python bootstrap exception.
- Align the binary-size guard's default budget with the release workflow budget.

### Fixed

- Fixed provider tool-probe request shaping and liveness classification so Anthropic and other non-OpenAI dialects use provider-compatible tool schemas and tool choices, and successful Anthropic `tool_use` responses count as parseable native tool calls.
- **Hook Harn builds now keep Cargo build artifacts inside the isolated hook target directory (#4160).** Local hooks default `CARGO_BUILD_BUILD_DIR` under `CARGO_TARGET_DIR` so warm Harn CLI builds no longer spill into the shared cargo build directory.
- Reduced the Harn CI audit tail by warming one cached `harn` binary and reusing it across Harn-backed script gates.
- Clarify same-resource edit fanout feedback so skipped sibling edits point agents toward one current-file recovery edit instead of replaying a stale same-file batch.
- Cap text-tool dispatch for wrapper-corrupted responses that recover multiple top-level bare calls.
- Pin intra-Harn workspace crate dependencies exactly in published manifests so downstream crates cannot resolve stale Harn siblings from the same semver line.
- Let `std/testing` host mocks intercept request-dict hostlib builtins, including `hostlib_tools_run_command` and compatible `process.exec` command mocks.
- Tighten the artifact manifest JSON Schema to reject unknown manifest and file-spec fields, matching `artifact_emit` validation.
- **Command artifact retention.** Harn now sweeps old `harn-command-*` temp directories by count as well as age, preventing long agent/eval runs from exhausting temp-dir quotas with thousands of small stdout artifacts.
- Registered reminder providers and session/lifecycle hooks now resolve sibling module `pub fn`s from inside their closures even after the VM that registered them is torn down. Previously the closure's module function table was held only via a `Weak` owned by that VM's module cache, so a provider/hook fired from a later VM misdispatched the sibling call to the host bridge (`host bridge tool '<fn>' is not implemented`), killing the agent turn. The registered closure now pins its module scope for its retained lifetime.
- Release tooling now rewrites intra-Harn crate dependencies to exact `=X.Y.Z` pins on every version bump, matching the `check-crate-sibling-versions` gate. Previously it emitted caret `X.Y` pins, so a published crate could resolve an older sibling from the same minor line.
- A streamed tool call whose native arguments are cut off mid-stream (a `{"__parse_error": "..."}` carrier) is now named as a truncated or malformed call and coached to re-issue smaller, instead of the misdiagnosing "missing required parameter: path" that sent models (observed on llama.cpp qwen3.6-35b) into a 20+ call re-try spin with no visible reply.

### Security

- **Agent subprocess sandboxing now defaults to a required OS sandbox carrier, and Linux seccomp uses a default-deny syscall allowlist (#4127).** Top-level agent loops install an `os_hardened` carrier by default, and Linux subprocesses omit process-introspection, `io_uring`, and addressable socket syscalls unless policy permits them.
- **Provider auth secret references (#4163).** Provider API-key env vars may now point at Harn secret references such as `harn-secret://provider/anthropic-api-key`; Harn resolves the raw key only inside provider auth construction and catalog availability checks understand the same reference form.

## v0.9.18

### Added

- Added `std/agent/guardrails::agent_input_guardrail`, a pre-loop input guardrail bookend for stopping agent loops before the first main model turn when a cheap classifier trips.
- The canonical model-ladder step (`ModelLadderStepDef` and the `.harn` `ModelLadderStep` alias) now reach full parity: alongside `model`, `provider?`, and `label?` they carry `when?`, `options?`, `family?`, and `capabilities?`. All added fields are optional and serde-absent-when-unset, so existing catalog bundles and records serialize byte-identically. Catalog `[model_ladders.*]` steps now honor per-step `options` overrides identically to inline `models:` steps (previously the catalog path silently discarded them), and `family`/`capabilities` let downstreams such as harn-cloud's `FreeTierRoute` adopt the canonical ladder-step type instead of maintaining their own copy.

### Changed

- Path-gate reporting-only merge queue workflows so irrelevant speculative diffs skip expensive Rust setup and checks.
- **ACP/A2A artifact manifest surfaces.** Adapter coverage now pins `artifact_manifest` session and task artifact updates so document bundles keep their manifest specs, MIME type, fallback text, provenance, and metadata intact across protocol bridges (#4134).
- **Agent stdlib**: consolidated duplicated prompt-fragment folds (`__with_prompt_fragment`), judge checkpoint preambles (`__judge_run_checkpoint`), stall observations/diagnostic trips (`__agent_stall_observation` / `__agent_stall_emit_diagnostic_trip`), and the `verify` node builder shared by the workflow pattern graphs (`__patterns_verify_node`). The prompt-nudge overlay fold is now `with_overlay(agent_options, rows, mode, options?)` — options-first, matching `with_goal` / `with_governance`; `overlay_policy` remains as a deprecated alias with the old argument order. Behavior is unchanged.

### Fixed

- Preserve `repair_prompt_builder` callbacks across `workflow_graph(...)` normalization so `workflow_run_repair` custom retry prompts execute instead of silently falling back to blind retries.
- Classify GitHub Actions hosted-runner shutdowns separately from code failures and auto-recover first-attempt CI preemptions by rerunning PR jobs or re-arming merge-queue PRs.
- Fixed a cancellation panic in ambient LLM prompt rendering when an async `llm_call` is aborted while its task-local render context is swapped out.
- `agent_preset` now treats provider/model routing as an atomic override group, so configured local routes no longer inherit a conflicting built-in provider or model ladder from the preset pack.
- Fixed generated chat projects to use the structured std/io read_line helper and made Ctrl-C interrupt idle stdin reads during harn run.
- Honor `output_schema` on `agent_loop`/`agent_turn`: it now gates the loop's final answer instead of being silently ignored. The schema is applied only to the terminal answer (never forced on every mid-loop turn, where it fought tool-calling); off-shape answers are re-asked once through the `llm_caller` seam, and the parsed value is surfaced on `run.output` with `run.output_valid` recording whether validation passed.
- **LLM caller middleware**: `default_llm_caller` (`std/llm/handlers`) now attaches the underlying `error` on the `budget_exhausted` envelope branch, matching `safe_call` (`std/llm/safe`); both share one `__wrap_llm_result` wrapper so the budget context is no longer dropped on the handlers path. The default retry predicate now also treats `context_overflow` as an alias of `context_window_exceeded` (never retried), and the `with_retry` docs list the full never-retry set.

### Security

- Deny agent-scoped secret access to runtime-reserved provenance namespaces so signed receipt Ed25519 seeds remain Harn-internal signing material.
- Hardened Harn filesystem and HTTP download writes so restricted workspace mutations resolve parent paths at open time without following symlinks.
- **Guard all outbound HTTP clients against SSRF.** The rebind-proof connect-time SSRF resolver is now installed on every outbound client — the shared LLM streaming/blocking/utility clients, connector clients, MCP discovery/OAuth/card/HTTP-transport clients, the provider healthcheck, and the remote provider-catalog fetcher — not just the `http_*` builtins. A base URL, connector endpoint, or MCP server URL whose hostname resolves (or DNS-rebinds) to a private / loopback / link-local / metadata (`169.254.169.254`) / denied-CIDR address is now unreachable at connect time on these paths.
- **Redaction no longer passes secrets in values larger than 256 KB.** Oversized string values were previously returned unredacted; a secret embedded in a large tool result, transcript, or base64 blob under a non-sensitive field name could leak verbatim. Oversized inputs are now scanned in overlapping windows, so a secret anywhere in the value — including one straddling a window boundary — is redacted, while legitimate large content is not over-redacted.
- **Corrected the `std/session-store` integrity claims.** The keyless SHA-256 hash chain is documented as tamper-EVIDENT (detects accidental corruption, truncation, reordering, and naive edits) rather than tamper-RESISTANT: a writer with filesystem access can rewrite history and recompute the chain, and `actor` / `tenant_id` / `ts_ms` are not part of the digest. Use signed harn-serve session receipts / Ed25519 run-receipt provenance for attribution. The record-hash formula is unchanged (cross-language contract with Burin).
- **Universal catastrophic-command floor at the process chokepoint.** The never-approvable catastrophic-command floor is now enforced unconditionally at the shared `spawn_process` chokepoint that every hostlib process tool funnels through (`run_command`, `run_test`, `run_build_command`, `manage_packages`, and the long-running background path) — closing a gap where a bare `run_command` (the agent's real shell tool) could run machine/disk/data-destroying commands with no floor. Catastrophic reasons are now split into two categories: the **universal** destruction set (`rm -rf` escaping the workspace, fork bombs, `mkfs`, `dd of=<device>`, `chmod -R 000`, `truncate -s 0` of a source file, redirect-over-source, project-root delete) is blocked everywhere, including with no `command_policy` on the stack, while the recoverable git **workflow** family (`git reset --hard`, `git clean -fd`, force-push) stays enforced only when a policy is pushed — so the stdlib `git.push --force-with-lease` flow and standalone scripts keep working. The same universal backstop also now guards the `process.exec` no-policy path, and the shared classifier is exposed as `harn_vm::orchestration::universal_catastrophic_reason` so embedders no longer need to re-plumb the floor. Policy-present behavior is unchanged.

## v0.9.17

### Added

- Added Harn verification HUD model and snapshot-staleness helpers so hosts can
  render verification status from structured stdlib facts instead of duplicating
  prompt-text logic.
- **LoRA promotion evidence contracts (#4111).** `harn models lora plan`,
  `export`, and `manifest` now include a machine-readable promotion evidence
  contract with paired base/adapter routes, required receipts, and a
  batch-ready eval manifest handoff.
- Added first-class `artifact_emit("artifact_manifest", ...)` validation for
  `harn.artifacts.v1` document/media bundles.
- Added a public `harn.artifacts.v1` JSON Schema for document/media artifact manifests and file artifact references.

### Fixed

- **Release gate Cargo isolation (#4108).** Direct `scripts/release_gate.sh`
  invocations now default to release-local Cargo target/build directories while
  preserving explicit maintainer overrides.
- Merge-queue CI skips heavy Rust lanes for stacked docs-only queue tails while preserving the full backstop for code changes.
- Release container publishing now verifies that the version, major.minor, and
  latest GHCR tags are publicly pullable without workflow credentials.
- Run the runtime's own registered reminder-provider and session-hook closures
  as trusted bridge calls. Under an active execution policy the agent loop
  previously killed every turn with
  `tool_rejected: bridged builtin '...' exceeds execution policy` the moment a
  registered closure's body invoked a host-provided builtin; the trusted-bridge
  guard is now held across each provider/hook closure invocation so first-party
  closures the runtime chose to fire are no longer mistaken for model-issued
  tool calls. Model-issued bridged builtins remain gated.

## v0.9.16

### Added

- Add std/verification warm-state lifecycle helpers so verification profiles can start, reuse, and tear down
  hostlib-owned background verifier processes from data-declared rows.
- **Local Agents API artifacts.** File artifacts emitted by Harn sessions can now
  be indexed and served through workspace-scoped artifact metadata and content
  endpoints (#4096).
- Added a never-approvable **catastrophic command floor** to `command_policy`.
  The deterministic command-risk scanner now emits a distinct `catastrophic`
  label for irreversible destruction — fork bomb; `git reset --hard`;
  `git clean -fd`; `git push --force`/`-f`/`--force-with-lease`; `rm -rf`
  escaping the workspace root or wiping it in place; `dd of=…`; `mkfs`;
  `chmod -R 000`; `truncate -s 0` of a source file; and `>`/`>>` redirection
  onto a source file — detected through adversarial quoting, chained-command
  splitting, `bash -c` recursion, and the `sudo`/`env`/`nice`/`nohup`/`time`/
  `timeout`/`command`/`builtin` wrapper family. A `catastrophic` command is
  always hard-denied (a `status: "blocked"` envelope, no child spawned)
  regardless of policy configuration and is never routed to the consent gate —
  it cannot be approved. Policies may promote other scanner labels to the same
  never-approvable tier with a new `command_policy({deny_labels: […]})` set.
  Also fixed the `destructive` heuristic to key on `dd of=` (a raw overwrite)
  instead of the read-only `dd if=`.
- Add a live Fireworks Batch API adapter for `harn models batch submit`,
  `status`, and `download`, including Fireworks-native request JSONL and output
  dataset download receipts.
- Added a Gemini Batch API live adapter for `harn models batch` submit, status, and download.
- Added `std/agent/hypothesis`: a keyed store of human/agent hypotheses (id,
  statement, a mutable free-form `status`, and provenance) layered on
  `std/session-store`. `hypothesis_open` / `hypothesis_set_status` /
  `hypothesis_get` / `hypothesis_list` (with an optional status filter) /
  `hypothesis_delete` project the append-only stream; events carry the special
  `{kind: "hypothesis"}` tag.
- Added `memory_reminders(namespace, options?)` to `std/memory`: a
  deterministic callable returning the active records flagged to auto-surface
  as reminders (default flag `auto_surface`, overridable via `options.flag`) —
  a filtered enumeration safe to call every turn.
- Extended `std/memory` with mutable record metadata. `memory_store` now accepts
  `options.status`, `options.scope`, and `options.flags` (a `{name: bool}` map); records
  written without them stay byte-identical to existing logs. New `memory_update(namespace,
  id, patch, options?)` appends an in-place, projection-time overlay by record id (value,
  tags, status, scope, flags, provenance) while keeping the log append-only, and
  `memory_list(namespace, options?)` enumerates active records newest-first with
  `status` / `scope` / `tag` / `flag` filters. "Rejected" and similar states are just a
  `status`, so soft-retired records stay queryable.
- Add live `harn models batch` submit/status/download adapter support for
  Parasail's OpenAI-compatible Batch API.
- Added a persona `output_style` field. A persona manifest (or `.harn`
  `@persona(...)`) can now declare how the persona shapes its prose — either a
  bare style name (`output_style = "concise"`) or a `{ name, instructions }`
  table. The new `persona_output_style(function?)` accessor (from
  `std/personas/prelude`) returns the active — or a named — persona's style as
  `{name, instructions}`, or nil when none is declared.
- Added live `harn models batch` submit/status/download adapter support for Together's Batch API.

### Fixed

- **Nested workflow runs no longer clobber the caller's trace.** Running
  `workflow_execute` / `workflow_run_repair` inside an enclosing timing span
  (e.g. a run-level `start_timing` / `timed(...)` held across the run) no longer
  resets the tracing collector out from under the caller. Previously the
  enclosing span was stranded — its `end_timing` threw
  `__timing_end: unknown timing handle` — and already-completed sibling spans
  were erased. Tracing is now reset only when the collector is idle, so the
  standalone case is unchanged while nested runs preserve the caller's spans.

## v0.9.15

### Added

- **`std/agent/stall` gains a host verify-state progress axis.** A new optional
  `stall_diagnostics.progress_signal` callback (`{payload -> float?}`) lets the
  host report a monotone "best verification state" scalar so
  `agent_stall_no_net_progress` keys on whether the verifier actually advanced
  ("writes are not progress") instead of edit/tool signatures. It expresses both
  a long-stall cut (`verify_state_streak` + `verify_state_stall_turns`, or a
  `verify_state_recurrence_hard` recurrence) and a short write-axis cut
  (`no_verifier_progress_limit` non-advancing turns with a write landed). Absent
  the callback, behavior is byte-identical to before.
- **`std/agent/stall` gains a delivered-fix-not-landing trigger.** A new optional
  `stall_diagnostics.remediation_delivered` callback
  (`{ {session_id, signature, prev_dispatch} -> bool }`) lets the host report
  that a repair was delivered for the active failure signature; when it still
  recurs, the detector escalates one turn sooner via the new
  `delivered_fix_not_landing` warning pattern. Absent the callback, the plain
  `stuck_same_diagnostic` nudge is unchanged.
- **`std/agent/governors` gains `governor_pace_decision(policy, obs)`.** A pure
  smart-timeout pace core (proceed / extend / pace_check / cut) that decides
  progress-based *extend-inside-a-time-bound*, returning `new_budget_ms` for
  host-side wall-budget re-stamping. Bounded by `extend_max` /`pace_check_max`
  (default the existing `GOVERNOR_PACE_EXTEND_MAX` / `GOVERNOR_PACE_CHECK_MAX_INJECTIONS`).
  It reads no clock, store, or flags — every input is passed in.
- Added `std/agent/canon` helpers for routing agent iteration events into harn-canon feedback from changed
  paths and observed-symbol evidence.
- **Command policy can now REQUEST CONSENT instead of only allow/deny.** A
  `command_policy({...})` may carry a `consent` closure — the
  `std/llm/tool_middleware::with_consent` prompt_fn contract (`true` /
  `{decision: "approved"}` to allow, `false` / `{decision: "denied", reason?}`
  to deny). When a command lands on a `require_approval` risk class (a
  deterministic risk label listed in `require_approval`, or a pre-hook
  `require_approval` decision), it now routes through the consent gate instead
  of hard-denying: an approval lets the command run, a denial returns a
  `status: "consent_denied"` envelope without ever spawning a child process.
  The consent closure receives the command context enriched with
  `consent.reason` and `consent.risk_labels` and may call `request_approval` /
  `ask_user` to block on a human. Policies without a `consent` closure keep the
  legacy hard-block behavior byte-for-byte, so the default path is unchanged.
- Added Groq to `harn models batch` planning, submit/status/download dry-runs,
  and the OpenAI-compatible live batch adapter path.
- Workflow stages can now run a caller-supplied `executor` closure as their leaf instead of spawning a delegated
  worker. Pass `executor: { ctx -> ... }` to `workflow_run_repair` (or set `executor` on any stage node) to wrap
  harn's retry-with-feedback / verify / attempt-recording machinery around a bespoke in-process agent loop. The
  closure receives `{task, attempt, prior_findings, prior_verification, prior_text, artifacts}` and returns
  `{result | text, artifacts?, transcript?, verification?}`; failing attempts thread their findings into the next
  call exactly like the delegated path. Omitting `executor` keeps the existing delegated-worker leaf unchanged.
- Added the `std/session-store` primitive: an append-only, SHA-256 hash-chained session event store at
  `.harn/session-store/<session_id>.jsonl`. `session_store_append` writes events (mirroring the harn-serve
  `StoredEvent` shape) with a canonical `record_hash` over `{session_id, event_id, payload, prev_hash}`;
  `session_store_project` / `session_store_project_value` fold `upsert`/`delete`/`replace`/`clear` mutation
  payloads to the latest-by-id records or a single value; and `session_store_verify` proves chain integrity.
  This is the durable substrate agent memory, hypothesis, and learned-context stores layer on.
- **`std/agent/stall` verify-state hard-recurrence now also trips on TOTAL
  (non-consecutive) failure recurrence.** The `verify_state_recurrence_hard` cut
  (active only when a `progress_signal` callback is supplied) previously keyed
  only on a consecutive same-diagnostic streak, which resets whenever an
  off-signature failure interposes — missing interleaved churn (the same dead
  API / wrong error re-proposed N times with different failures churning between
  recurrences). A per-signature TOTAL count (`verify_signature_counts`) that
  never resets on a signature change now trips the same cut on total
  occurrences, so interleaved-churn stalls are caught. Maintained cheaply on the
  default path but consulted only when a `progress_signal` is supplied, so
  behavior is byte-identical without the callback.

### Changed

- Record Together's provider batch discount in the built-in capability catalog.

### Fixed

- **OpenAI-compatible tool-call parsing.** Harn now unwraps native
  `tool_call` wrapper functions whose `arguments` string contains a Harn
  text-tool call, preventing providers from dispatching a bogus literal
  `tool_call` when they nest `<tool_call>look(...)` inside the native
  arguments field.

## v0.9.14

### Added

- `std/agent/canon` now discovers installed single-pack and manifest-backed
  `harn.canon` package contributions under `.harn/packages` when no explicit
  canon root is configured.
- `harn package check` now accepts contribution-only packages as publishable
  package surfaces instead of requiring a module export or rule pack.
- Added `harn models batch download` for provider-normalized batch result receipts.
- Add a live xAI adapter to `harn models batch` for JSONL file submit, status polling, and result download receipts.

### Changed

- **GitHub Actions spend reports now run through Harn.** The spend report
  helper keeps its existing shell entrypoint, but aggregation, sorting, repo
  filtering, JSON parsing, and table rendering now live in a Harn script with
  focused Harn tests instead of inline Python snippets.
- `harn models batch plan` now reports whether Harn has a live
  submit/status/download adapter for each provider route instead of only failing
  at submit time, and the CLI reference/JSON contract document the
  `batch.harn_live_adapter` field.

### Fixed

- **Agent `on_delta` streaming now uses the canonical `llm_call` wrapper (#4036).**
  Streaming agent turns inherit routing policies, schema retries, budget checks,
  and provider observability instead of bypassing them through a raw provider
  call.
- Reject arguments passed to `list.sort` instead of silently ignoring custom sort callbacks.
- Accept `stall_diagnostics.repair_diagnostics` as the documented nested repair-diagnostics config shape.

## v0.9.13

### Added

- Added code-index `Field` and `EnumCase` graph nodes with normalized `access_level` metadata.
- Surface completion-gate `judge_decision` reason and confirm fields for fire-rate telemetry.
- **Model batch manifests.** `harn models batch manifest` now turns JSONL
  request ledgers into durable provider-neutral batch manifests with grouped
  requests, stable custom ids, row hashes, and catalog-backed provider/model
  metadata for offline eval, judge, corpus-refresh, and distillation jobs.
- Add `harn models batch plan` to discover provider Batch API eligibility and offline workload constraints.
- Add `harn models batch prepare` to turn model batch manifests into provider-native request files and
  deterministic prepare receipts without live provider calls.
- Added `harn models batch status` to validate submission receipts and poll provider batch lifecycle state.
- Add `harn models batch submit` to validate prepared batch receipts, submit supported provider jobs,
  and write durable submission receipts.
- **Model ladders.** `llm_call` (and the `llm_call_structured*` variants) gain
  a first-class `models:` option — an ordered fallback ladder of
  `{model, provider?, options?}` steps (plain `["model-a", "model-b"]` strings
  are sugar) — plus `ladder: "<name>"` to resolve a named `[model_ladders.<name>]`
  ladder from the catalog. A ladder lowers onto the existing routing chain, so
  it advances to the next step **only** on transport-class failures
  (connection/timeout/429/5xx/throttled-empty, via the same failover
  classifier) and never on schema-validation or 4xx policy errors. Schema
  retries re-ask the same step's model. Each advance emits an
  `llm_models_advance` trace event (`agent_trace()`), and the winning model is
  surfaced on the existing `routing` result block. `models:`/`ladder:` cannot be
  combined with each other, with an explicit `model:`/`provider:` pin, or with an
  explicit `routing:` policy — any second model-selection surface is a hard error.
  The typed `LlmCallOptions` / `AgentLoopOptions` aliases and the `ModelLadderStep`
  alias (`{model, provider?, options?, label?}`) accept the option too, so ladders
  compose through `agent_loop`.
- **`std/agent/governors` and a unified detector surface in `std/agent/stall`.**
  New composable pace/budget GOVERNORS and a one-vocabulary DETECTOR subsystem,
  generalizing the guardrails a host would otherwise hand-roll on top of
  `agent_loop`. `governor_post_turn(policy)` returns a `post_turn_callback` that
  watches a monotone consumption signal (iterations / tokens / cost) against a
  budget ceiling and steers with a shared `proceed` / `warn` / `abort`
  vocabulary; `compose_post_turn([...])` chains several callbacks and
  `with_governance(opts, {governor, detectors})` folds a governor and a
  `DetectorSpec` into an options dict through existing seams only (no new
  `agent_loop` hooks). `DetectorSpec` lowers loop / no-progress / stuck rows onto
  native `stall_diagnostics` and adds a token-runaway overlay
  (`token_runaway_decision` / `token_runaway_post_turn`) that emits the same
  `agent_loop_stall_warning` event. `governors_selftest()` plus a live-firing
  conformance test assert the callback fires on the real `agent_compute_post_turn`
  payload shape, so a payload drift fails CI instead of silently disabling the
  governor.
- Workflow stages gain three ergonomics on top of the retry-with-feedback
  machinery. A stage's `verify` may now be a **function** (fn-verify): it
  receives the settled attempt result and returns `{ok, findings?}` (or a
  bool), gates the retry branch on *any* stage kind, and threads its findings
  into the next attempt's repair prompt. `workflow_stages`
  (`std/workflow/patterns`) expands a concise linear `WorkflowStagesSpec` into
  the `{entry, nodes, edges}` graph `workflow_execute` consumes — pure sugar
  whose output is byte-identical to the hand-authored `workflow_graph`. And
  `workflow_run_repair` (new `std/workflow/repair`) runs the
  run→validate→repair loop as a first-class helper: one agent stage, a
  caller-supplied verifier (callable / `{command}` / `{assert_text}`), and
  automatic re-prompting with the findings up to `max_attempts`. All three
  reuse the embedded PR-I2 attempt loop (`std/workflow/stage.harn`); none adds
  a loop of its own.
- **JSON salvage helpers in `std/llm/safe`.** New `extract_first_json_object(text)`,
  `extract_first_json_value(text)`, and `parse_first_json(text)` promote the
  balanced-brace scanners downstream repos hand-rolled for pulling the first
  JSON value out of sloppy freeform LLM text (leading prose, trailing garbage,
  code fences, braces inside strings, escaped quotes). `parse_first_json`
  composes with `strip_code_fences` and skips balanced-but-invalid candidates,
  returning the first span `json_parse` accepts, or nil. These are the
  salvage path for text you did not produce — prefer `llm_call_structured` /
  `safe_structured_call` when you control the request. The workflow and
  pipeline docs also gain a pipeline-vs-workflow glossary box (the `pipeline`
  keyword vs. the `workflow_execute` stage-graph runtime, plus the
  llm_call < agent_loop < workflow ladder one-liner).
- **Added `agent_completion_gate` (`std/agent/judge`)** — a configured done-time
  completion gate that composes the existing `verify_completion` and bounded
  `verify_completion_judge` / `done_judge` seams (no new loop seam). It
  generalizes burin-code's completion-verification policy: an ordered veto ladder,
  a source-write **evidence** requirement (only source writes count as progress
  toward "done" — cosmetic/test-scaffold writes do not), a per-session veto budget
  (`max_vetoes`, default 3) with strict post-write classes (a red verifier, or a
  source write whose verifier has not gone green) the budget never releases, and
  AND-of-oracles verify composition (`veto_combine`-configurable). Every domain
  fact stays a host callback (`facts`, `classify_write`, `verify_command`); with
  no callbacks the gate degrades to judge-only mode and surfaces the degraded state
  on the returned bundle (`_completion_gate.facts_available = false`) instead of
  fabricating a pass. The gate never keys on a done-sentinel string. Presets can
  carry a default via the new `completion_gate` pack row.
- **`std/agent/goal`: a typed long-running goal object.** `goal(spec)` normalizes
  `{objective, success_criteria, constraints, budget}`, where each success
  criterion may carry a host-fact `check` callback that makes it machine-checkable.
  `goal_check(goal, facts?)` evaluates that deterministic floor;
  `goal_judge(goal, opts?)` returns a `done_judge` config (the semantic ceiling)
  that composes with the existing `agent_verify_or_continue` seam; and
  `with_goal(opts, goal)` renders the objective/criteria/constraints into every
  outbound request through the existing per-turn context-profile fragment channel
  (#2631) — no new hook surface. `goal_reloop(goal, opts?)` returns `agent_loop`
  options that drive the bounded "not yet met, re-enter with findings" re-loop
  through `agent_loop`'s own completion loop (a `verify_completion` gate over
  `goal_check` vetoes an unmet goal, threads the unmet criteria into the
  transcript, and re-runs the agent up to `max_attempts`, default 3) rather than
  a hand-written loop, and `goal_pin(goal)` bridges a goal into a self-replacing
  `std/agent/pins` pin.
- **`std/agent/lanes` and `std/agent/overlays`: tool-surface lanes and
  prompt-nudge overlays.** `lane_policy(rows, task, opts?)` classifies a task
  once into a named lane from a data table (`default_lane_rows()` ports
  burin-code's `agent_lane_for_task` decision ladder: 1-4 explicit file
  targets narrows to a `look`/`search`/`edit`/`run`/... `explicit_patch` lane,
  otherwise the unrestricted `general` lane) and narrows `opts.tools`/
  `opts.policy` down to that lane's allowed tools for the run, reusing the
  same tool-surface-narrowing primitives already shared with
  `std/agent/stance` — no new Rust, no new hook surface. Narrowing via
  `policy.tools` means a hidden tool is never *named* to the model as an
  alternative when the model attempts it: harn-vm's native tool-ceiling denial
  path reports only the tool actually attempted. `lane_scope_classifier(rows)`
  optionally lowers the same rows onto the existing `pre_turn_scope_classifier`
  seam for per-turn lane telemetry (never narrows or skips a turn itself).
  `overlay_policy(rows, mode, opts?)` layers data-driven, mode/lane-specific
  prompt-nudge lines onto the outbound system prompt through the existing
  `context_profile.prompt_fragments` channel (#2631) — it only ever adds a
  fragment alongside the caller's explicit `system`/fragments, never replaces
  them, and an `options.overrides` entry fills (and wins over) a row's slot.
  `agent_preset`/`agent_preset_register` gain `lane_policy`/`overlay_policy`
  pack keys (fill-nil, explicit caller input always wins); the built-in
  `repair` preset ships a default lane table and `review_captain` ships a
  default overlay row.
- **`std/agent/pins`: compaction pins as data.** A new module generalizes
  Burin's structural working-set into a typed pin taxonomy — `pin(kind, content,
  opts?)` / `unpin(pins, id)` over the kinds `artifact_ref`, `constraint`,
  `decision`, `goal`, and `no_compact`. Pins survive compaction by construction:
  `pin_reminder(pin)` lowers to a `preserve_on_compact` system reminder and
  `pin_compaction_policy(pins)` emits `compaction_policy` preserve directives.
  The same pins double as reachability-GC roots — `with_pin_roots(opts, pins)`
  feeds pin content into the existing `reachability_gc` projection `roots` config
  so any referenced stale tool result is kept, not reclaimed.
  `recognize_no_compact(text)` is a documented ingestion adapter for Burin's
  literal `[no-compact]` heading marker. Agent presets can carry a default
  `pin_policy` pack row (the long-running captains do). Additive stdlib only — no
  new host surface.
- Workflow stages support retry-with-feedback. `retry_policy.feedback`
  (`true` or `{max_chars}`) appends the prior attempt's verification findings
  to the next attempt's task, and `retry_policy.repair_prompt_builder` is a
  closure that returns the full replacement task from the retry context
  (`{task, attempt, findings, verification, error, prior_text, stage}`). With
  neither set, retries stay byte-identical to before. The per-stage attempt
  loop now runs in embedded Harn (`std/workflow/stage.harn`); Rust keeps only
  the enforcement/attestation leaves. Added `workflow_repair_stage_graph`
  (`std/workflow/patterns`) — one-stage sugar over the stage retry policy for
  validate→repair loops.

### Changed

- `harn models lora export` now stamps exported tool-calling rows and manifests
  with deterministic provenance fields for source ids, split/license defaults,
  teacher and target route metadata, tool-schema hashes, and prompt-template
  hashes.
- `harn models lora plan` now accepts `--trainer` and emits
  trainer-backend-specific SFT contract notes for TRL, Unsloth, and external
  trainers.
- Model capability reports now surface provider Batch API support, input format,
  published discount, and turnaround metadata for offline orchestration planning.
- **Promoted `agent_preset` to the stable agent-cell surface** (dropped the
  stale `@api_stability: experimental` tag): `agent_preset(kind, options?)`
  is how you build `agent_loop` options. Kinds now live in a registry —
  `agent_preset_register(kind, {family?, pack?})` makes user-defined kinds
  first-class (validated through the same path as the built-ins) and
  `agent_preset_kinds()` discovers them. Each kind carries fill-nil pack rows
  (per-kind `provider`, `timeout_ms`, session-cumulative `budget`, and
  `model_ladder` defaults) that fill only nil/absent keys and never override
  explicit caller input. Presets also bake a bounded default transport retry
  onto the effective `llm_caller:` seam (`with_retry`, `max_attempts: 3`,
  transport-class failures only — never schema/auth/budget/policy),
  restoring the resilience the removed `llm_retries: 2` profile default used
  to provide; `retry: false` opts out, `retry: {...}` tunes it. Typed alias
  follow-ups: `AgentPresetOptions` gains `retry` / `model_ladder`, and
  `AgentLoopOptions` gains `history` (the #4030 caller-managed history
  seeding option).
- **Documentation: consolidated the R1 orchestration-primitive guidance.**
  [Choosing an agent abstraction](docs/src/concepts/abstraction-ladder.md) now
  opens with the one-line ladder — `llm_call` (one request) < `agent_loop` (one
  goal, run to completion) < `workflow` (more than one goal, attempt, or model) —
  spells out the "never hand-write a `while` around `llm_call`" rule, and states
  explicitly that `agent_preset` and model ladders are *not* rungs. It adds a
  **placement contract** table naming the canonical home for every cross-cutting
  mechanism (completion gate → `std/agent/judge`, governors → `std/agent/governors`,
  unified detectors → `std/agent/stall`, lanes → `std/agent/lanes`, overlays →
  `std/agent/overlays`, compaction → `std/agent/autocompact`, scratchpad →
  `std/agent/scratchpad`, default mutation tools → `agent_edit_tools`, and preset
  packs → `std/agent/presets`). The `harn-orchestration` skill gained the same
  ladder framing plus the `models:` / `ladder:`, `agent_edit_tools`,
  retry-with-feedback (`repair_prompt_builder` / `feedback`), fn-verify,
  `workflow_stages`, and `workflow_run_repair` surfaces. The `llm_call` options
  reference now documents `models:` / `ladder:` and points to the 0.10 migration
  for the removed `llm_retries` / `llm_backoff_ms` / `transcript_policy` options.
- **Stage policy flattening moved to Harn.** The per-stage collapse of the
  ~15 workflow policy structs (model policy, auto-compaction, tool spec,
  capability + approval policy, skill registry, nested-execution attribution)
  into the `agent_loop` options dict now lives in
  `std/workflow/stage.workflow_flatten_agent_loop_options` instead of Rust
  (design D5: Harn decides *what options the loop gets*). Rust keeps only the
  enforcement leaf: it re-derives the capability ceiling
  (`tool spec ∩ stage capability_policy`) and, when the flattened dict re-enters
  the host, rejects any result whose `policy` *widens* that ceiling — a buggy or
  hostile flattener can narrow a capability / budget / permission ceiling but can
  never widen one (`CapabilityPolicy::assert_within_ceiling`, surfaced as a
  `tool_rejected` error). Flattening output is byte-compatible with the prior
  Rust path, so replay records are unchanged.
- **Dev setup now emits a machine-shared Cargo `build-dir`** (`$TMPDIR/cargo-build-shared`)
  alongside the per-worktree `target-dir`, so intermediate artifacts are deduped across
  worktrees (sccache's Rust hash is target-path-dependent and never was). Also fixed
  `scripts/prune_stale_targets.sh` dying silently under `set -euo pipefail` before it
  could GC stale per-worktree target dirs; it now always prints its summary line.

### Fixed

- Deduplicate Anthropic sampling-parameter stripping across primary, stream,
  completion, Bedrock Claude, and provider-override request paths.
- Break the actionless-200 empty-completion re-dispatch storm. When a
  route returns HTTP 200 with an empty visible+tool channel
  (`completion_tokens=8 ... delivered no content` — e.g. an Anthropic
  escalation target fed a huge cross-provider-bridged context), the
  agent loop used to re-dispatch the full context and continue, 18-43x
  per run. Terminal unproductive completions now feed the always-on
  per-route circuit breaker as a dedicated streak (and no longer reset
  it as a success); after a small cap the breaker opens and the next
  dispatch fails fast with `circuit_open` before re-sending the context,
  so the loop degrades onto the primary/cheap-model result instead of
  hemorrhaging full-context re-sends. Provider-general and independent of
  the `llm.rate_governor` flag.
- Agent escalations now attach catalog-backed equivalent failover routes for
  non-primary model calls, including no-dispatch failover for actionless
  completions.
- **Stage flatten ceiling re-check now covers every widenable dimension.** The
  Rust re-check that rejects a Harn stage flattener from widening the capability
  ceiling (`CapabilityPolicy::assert_within_ceiling`) previously guarded only 7
  of the 10 `CapabilityPolicy` fields, leaving `process_sandbox` (subprocess
  host-FS read/write roots + presets), `tool_arg_constraints` (per-argument path
  scoping), and `tool_annotations` (which drive constraint resolution and
  per-tool side-effect classification) unchecked — a flattener could widen any of
  them undetected. All three are now enforced with the same
  narrowing-allowed / widening-rejected semantics and a `tool_rejected` error
  naming the dimension. The side-effect-level comparison also now ranks through
  the canonical `SideEffectLevel::rank_str` ladder (fail-closed on unknown
  levels, and grows at the top) instead of a hand-rolled fail-open rank.
- The egress SSRF classifier now also blocks the remaining reserved IPv4
  ranges: IETF protocol assignments `192.0.0/24`, 6to4 relay anycast
  `192.88.99/24`, and class E `240/4`.
- Recover provider tool-call dialects that previously looked actionless to the
  agent loop: OpenAI-compatible responses that misplaced complete Harn text-tool
  syntax into native `function.name` or `function.arguments`, and DeepSeek DSML
  `tool_calls` blocks that were already recoverable but still injected
  parse-error feedback every turn.

### Security

- Block outbound HTTP redirects that downgrade a public HTTPS request to HTTP;
  loopback downgrade targets remain available only through the explicit
  `allow_loopback` egress-policy hatch.

## v0.9.12

### Breaking

- **Removed the deprecated `llm_retries` / `llm_backoff_ms` options and the
  in-call transient retry budget.** `llm_call` and `agent_loop` are fail-fast
  on transient provider errors (the `agent_loop` profiles no longer inject
  `llm_retries: 2`); compose retry policy on the caller seam with
  `with_retry(default_llm_caller(), {...})` from `std/llm/handlers` — note the
  off-by-one, `llm_retries: K` → `with_retry(..., {max_attempts: K + 1})`. The
  `deprecated_llm_options` lint is now a hard error carrying that hint. The
  built-in empty-completion retry is a fixed single silent retry for
  provider-shaped routes and can no longer be widened per call.
- **Deleted `std/agent/stack` and the preset wrapper fns.** `agent_stack`,
  `agent_llm_caller`, `agent_tool_stack`, `agent_stack_audit_line`,
  `agent_stack_model_policy`, `agent_budget`, and the eight `*_agent` wrappers
  (`audit_agent` … `release_captain_agent`) are gone; `agent_preset(kind,
  options?)` + `agent_loop` is the single preset surface. The survivors
  `agent_model_options` / `agent_sanitize_model_options` moved to
  `std/agent/options`. See `docs/src/migrations/v0.10.md` for side-by-side
  rewrites.

### Added

- `agent_loop` gained a `history` option: a caller-managed conversation history
  (a list of `{role, content, ...}` messages in the canonical `llm_call` shape)
  prepended to the transcript as real conversational turns ahead of the task
  message. The seeded turns are visible to the model exactly as `llm_call`'s
  `messages` array would be, and are treated as ordinary transcript turns by
  done_judge, compaction, and projection. This is transient seeding (the caller
  owns the history), distinct from session persistence, and unblocks
  chat-shaped harnesses that manage their own history from using the full agent
  loop instead of raw `llm_call`.
- **`std/llm/safe` gains free-text cleanup helpers `strip_think_blocks` and
  `strip_code_fences` (#4021).** `strip_think_blocks` removes inline `<think>` /
  `<thinking>` reasoning blocks from raw model output; `strip_code_fences` strips
  whole-line Markdown fence delimiters (with or without a language hint) for when a
  model wraps an entire document in a fence. These are the manual escape hatch for
  free text that did not flow through the capability-gated `llm_call` envelope (a
  hand-assembled string, a cached blob, an uncatalogued provider).
- **Default mutation toolset.** `std/agent/host_tools` now ships
  `agent_edit_tools(...)`, the canonical root-scoped `write_file`, `edit_file`,
  `create_directory`, and `delete_path` tools every embedder previously
  hand-rolled. They wrap the existing hostlib filesystem primitives, reuse the
  same root resolution and path-scope enforcement as `agent_read_tools`, and are
  annotated as mutating (`kind: edit|delete`, `side_effect_level:
  workspace_write`) so the read-only stance hides them. Compose explicitly over
  the read/command surface (`agent_edit_tools(agent_host_tools(nil, opts),
  opts)`); mutation stays opt-in.
- **`agent_loop` gains an `on_delta` streaming seam (#4020).** Pass an
  `on_delta: { delta -> ... }` closure and each per-turn model call is issued
  through the streaming transport, firing the callback once per streamed chunk of
  the assistant's visible text — so chat-shaped harnesses can render or transform
  the token stream without abandoning `agent_loop` for a raw `llm_stream_call`.
  The callback is observational (return value ignored); the turn still returns a
  complete result, so native tool calls and usage are preserved and tool dispatch
  is unaffected. Providers that do not stream fall back to a single delta carrying
  the full visible text. A custom `llm_caller` short-circuits the default path, so
  a non-streaming caller simply never fires `on_delta`. Tool-call-fragment
  streaming is intentionally out of scope for v1. The `llm_mock` testing surface
  learns a `stream_chunks` field (helper: `llm_stream_text([...])`) for scripting
  deterministic streaming responses.
- Add computer use (screenshot + mouse/keyboard control) as an opt-in host capability. `harn-hostlib`
  gains a cross-platform `computer` module (Cargo features `computer` / `computer-local`) exposing
  `hostlib_computer_{screenshot,execute,ui_tree,permissions}` over pluggable local (`xcap` capture +
  `enigo` input), helper, and remote (TCP) socket backends that all share one wire protocol. `harn-vm`
  projects a single neutral computer tool onto each provider's native surface — Anthropic
  `computer_20251124` (with the `computer-use-2025-11-24` beta header), OpenAI Responses `computer`, or a
  portable function-schema fallback for other vision models — and carries screenshot tool results back as
  image content blocks. Off by default; gated by model capability (`computer_use_style`) and
  `BURIN_COMPUTER_USE_TRANSPORT`.
- Typed option aliases are now the single documented path for building
  orchestration options. `std/llm/options` gains `LlmCallOptions`, `LlmBudget`,
  and `ModelLadder` (plus `llm_options(...)` / `model_ladder(...)`
  constructors); `std/agent/options` gains `AgentLoopOptions`,
  `AgentPresetOptions`, `IterationBudget`, `TurnPolicy`, `StallDiagnostics`,
  `CompactionPolicy`, and `JudgeConfig` (plus `agent_options(...)` /
  `agent_preset_options(...)`); `std/workflow/options` gains `StageSpec`,
  `WorkflowRetryPolicy` (including the staged `repair_prompt_builder` /
  `feedback` retry-with-feedback keys), `ModelPolicySpec`, `StageContract`,
  and `WorkflowExecuteOptions` (plus `workflow_stage_spec(...)` /
  `workflow_execute_options(...)`). Each alias carries a cross-reference to
  its Rust policy twin and a serde-defaults key-parity test pins the two
  surfaces together. A new info-level `unnormalized-options` lint
  (`HARN-LNT-060`) flags inline option dict literals passed directly to
  `agent_loop` / `workflow_execute` and points at the typed constructors —
  raw dicts still execute unchanged. Docs examples across `llm_call`,
  `agent_loop`, the workflow runtime chapter, and the LLM quickref now build
  options through the typed aliases.

### Changed

- `harn models lora plan` now calls out Gemma 4 native-tool serving risk on vLLM and records parser/template
  pinning guidance in the plan.
- **LoRA planning.** `harn models lora plan` now reports a model-aware corpus
  selection and refinement contract for tool-calling adapters.
- **LoRA planning now emits post-training receipt and probe commands.** `harn
  models lora plan` includes the `harn models lora manifest` handoff step and a
  served-route `harn provider tool-probe` command so external trainers can return
  auditable adapter metadata to Harn before inspection, launch, and promotion
  evals.
- Internal (pure refactor, no behavior change): extracted the workflow stage
  attempt loop's mechanisms into four runtime-only host builtins —
  `__host_stage_select_artifacts`, `__host_stage_execute_once`,
  `__host_stage_record_attempt`, and `__host_llm_usage_snapshot` /
  `__host_llm_usage_delta` — as inversion pre-work for moving the retry loop
  itself into `std/workflow/stage.harn` (design D5 step 1). The Rust loop in
  `stage.rs::execute_stage_attempts` now drives through the same internal
  functions the builtins wrap; replay, records, and all stage outcomes are
  byte-identical.

### Fixed

- **`llm_call` now excludes inline `<think>` reasoning from `text`, `prose`, and
  `visible_text` for local/open-weight routes that emit reasoning inline (#4021).**
  Routes whose capability matrix marks them as inline-reasoning emitters (Qwen3 via
  vLLM, local Ollama / llama.cpp reasoning models, Kimi) have their `<think>...</think>`
  blocks split out of the visible answer channels and folded into the `thinking`
  reasoning channel, mirroring how hosted-provider thinking is already surfaced. The
  split is capability-gated on `emits_inline_reasoning` (derived from
  `thinking_block_style = "inline"`, data-driven — never a provider-name match), so a
  provider that never emits inline think (Anthropic, OpenAI) passes any literal
  `<think>` in its output through untouched. An unclosed `<think>` with no matching
  `</think>` consumes the remainder as reasoning, the safe reading for a truncated
  trace with no committed answer.
- **Agent read-only stance.** Read-only tool filtering now honors policy-level
  tool annotations when host-provided registry entries omit direct metadata.
- Advertise `stance_transition` in ACP/Harn protocol artifacts so hosts can render read-only stance lifecycle
  updates through generated bindings.

## v0.9.11

### Added

- Added a git-hook duration instrument: `.githooks/lib.sh` now appends one
  NDJSON line per pre-commit/pre-push invocation to `~/.burin/hook-timings.ndjson`
  (zero-dep, never changes the hook's exit code, degrades silently if the log
  directory is unavailable). Added `scripts/hook_timings_report.sh` to print
  p50/p95/max duration per (repo, hook), and `scripts/gha_spend_report.sh` to
  print estimated GitHub Actions spend per repo/workflow.
- **LLM provider rate/concurrency governor — Layer 0 detection + Layer 1 adaptive
  governor, behind the default-off `llm.rate_governor` flag (`HARN_LLM_RATE_GOVERNOR=1`).**
  When armed, Harn now governs its own concurrency/rate per `(provider, org_key)`
  against provider throttling instead of retrying blindly into an org-wide wall. Detection
  classifies each provider outcome into a structured `provider_throttle` transcript
  record (HTTP 429, 529/503 overload, Anthropic overloaded/rate-limit body, and the
  empty-completion-under-load heuristic). The governor runs an AIMD concurrency
  limiter (additive-increase on sustained success, halve-to-floor on a throttle
  signal), optional RPM/TPM token buckets, and a circuit breaker
  (CLOSED → OPEN with exponential backoff + full jitter honoring `Retry-After`
  → HALF-OPEN single probe → CLOSED) so retries wait behind the governor. Limits
  live in the catalog as `[provider_limits.<provider>]` rows (Anthropic seeded;
  every provider generic), never at call sites. New `harn provider limits`
  reports resolved limits and live governor state deterministically, and a
  `governor_state` record plus a `circuit_is_open` query seam expose whether a
  run was infra-throttled rather than a capability failure. Byte-identical
  behavior when the flag is off. Layer 2 (shared-local Harn leases) and
  Layer 3 (Harn Cloud quota authority) are follow-ups; the `(provider, org_key)`
  key and serializable governor state leave clean seams for both.
- Perf pre-work for the VM-heavier re-architecture (measurement only): recorded
  the pre-migration CLI cold-start baseline in `perf/cli/baselines/main.json`
  (cold + warm medians, Apple Silicon macOS host), added a criterion benchmark
  for the Rust↔`.harn` `harn_entry` boundary crossing (`call_harn_export_typed`
  vs `call_harn_export_by_name`, warm vs cold parent module cache), added a
  transcript-projection benchmark at ~10k/50k/100k-token transcripts, and added
  `perf/README.md` documenting these suites as the regression gates for the
  stage-loop inversion wave.
- Extracted the canonical secret pattern catalog into a new dependency-free
  `harn-secret-catalog` crate so off-runtime host consumers can share the single
  source of truth instead of forking their own detector lists. `harn-vm`'s
  redaction and `secret_scan` paths now re-export it with byte-identical
  behavior.

### Changed

- Ported the protocol bindings round-trip checker to Harn while preserving Python
  execution for the generated Python SDK artifact under test.

### Fixed

- **Git hooks now build their own Harn binary without inherited Rust compiler
  wrappers (#4015).** Hook-internal Harn builds clear `RUSTC_WRAPPER` and
  `CARGO_BUILD_RUSTC_WRAPPER`, avoiding sccache/cache-wrapper contention when
  developer or CI environments set global Rust wrappers.
- **Agent-loop post-turn policy stops no longer run completion verification by
  default.** Callback stop verdicts now terminate authoritatively unless they
  explicitly set `needs_verify: true`, preventing graceful-recovery ripcords
  from being converted back into repair feedback.
- Make terminal empty completions under an active provider throttle/circuit
  failover-eligible instead of returning a final empty response on the same
  provider.
- Corrected a stale `harn-secret-catalog` version pin in `Cargo.lock` (0.9.9 →
  0.9.10) so the workspace lockfile matches the current crate version and cargo
  no longer re-dirties the tree on build.
- LLM provider dispatch: `resolve_api_key` and the `harn providers` status
  builtin no longer special-case Bedrock/Vertex by hardcoded provider name to
  skip the generic `auth_env` check. Providers now declare
  `credential_resolution = "platform_managed"` in `providers.toml` when their
  shim resolves credentials through a multi-step chain (AWS SigV4 credential
  chain, GCP ADC / service-account JSON) instead of a simple env-var lookup.
  This also fixes a latent gap where Vertex's declared `auth_env` list (which
  does not include every valid ADC path) could make `resolve_api_key` report
  a false "missing API key" outside the code paths that had the hardcoded
  bypass.

## v0.9.10

### Added

- Added `harn models lora manifest` for writing canonical LoRA training-run manifests that share the existing
  Harn route, tool-call, serving, and promotion contracts.
- Added a Harn-based Python boundary guard for repository tooling so new Python files must be explicit
  bootstrap, platform, generated-binding, or fixture code.

### Changed

- Port the Burin protocol artifact drift check from Python to Harn.
- Port the CLI cold-start benchmark controller from Python to Harn and keep the measured subprocess isolated.
- Port the tree-sitter parse sweep release-gate helper from Python to Harn.

## v0.9.9

### Added

- **LoRA preflight now validates the target tool-call format.** `harn models
  lora preflight` accepts `--tool-format`, checks that the corpus source tool
  calls can export into the selected Harn/native route, and `harn models lora
  plan` now prints a matching preflight command before dataset export.
- Added a `precision` class (`"high"` for self-identifying token shapes,
  `"heuristic"` for keyword/context matches like `Bearer <b64>` or
  `password = "..."`) to every `secret_scan` finding and to the canonical
  secret catalog. Consumers can now hard-block only high-precision findings
  without hard-coding detector names.
- Added an optional `{audit: false}` second argument to `secret_scan` so a
  hot-path caller (e.g. a per-edit or per-command guard) can get catalog-backed
  findings without appending an `audit.secret_scan` event on every call. The
  one-argument form still audits, unchanged.

### Fixed

- **Agent loop and provider routing hygiene (#3999).** Harn now hides debug-only
  nested-execution policy events by default, strips internal verdict envelopes
  from visible assistant text, requires real verification before ending repair
  loops, and routes the mid-tier preset to Qwen3-Coder-Next with refreshed
  OpenRouter/Z.AI catalog metadata.
- Added `includes()` as a membership alias for Harn strings and collections.
- Preserve host helper builtins when composition dispatcher closures run inside embedded hosts.

## v0.9.8

### Added

- Add `skills_activation_evidence(registry, options?)` and a Harn-owned
  activation-evidence payload (schema v1) so hosts can read which skill cards
  were shown or omitted — and why, plus source, budget cost, and body
  lifecycle (`eligible`/`shown`/`omitted`/`loaded`/`used`) — without parsing
  the catalog prompt text. The `skill.loaded` event now carries matching
  `lifecycle`, `source`, `disable_model_invocation`, and `token_estimate`
  fields. Documented in `docs/src/skill-activation-evidence.md`, with the
  migration path from Burin's `skill_activation_report`.
- `harn provider cache-probe --usage-fixture <path>` classifies a saved
  repeat-run prompt-cache usage fixture into one cache verdict. It resolves
  prompt-cache support and cache-control requirements (breakpoint style,
  minimum useful prefix, TTL notes, usage-field mapping) from the single
  provider capability path, normalizes each run's usage keeping fresh-input /
  cache-read / cache-write / output / unknown-missing separate, and buckets
  every run (`cache_effective`, `cache_supported_miss`, `unsupported_zero`,
  `support_unknown_zero`, `no_prompt_tokens`, `provider_field_inconsistent`).
  A missing provider usage field is recorded as an observation, never
  re-classified as "unsupported". The `provider_capabilities` builtin now also
  projects a `cache_control` profile for Burin dogfood and Harn Cloud receipts.
- Added `harn models lora preflight` for CPU-only tool-calling corpus readiness checks before LoRA training.

### Changed

- **Typechecker and parser cutovers tighten callable, generic-bound, and
  control-flow behavior (#3922).** Harn now supports annotated
  `fn(...) -> T { ... }` closures, parses full type expressions in generic
  interface `where` bounds, allows callbacks to ignore surplus user-function
  arguments while keeping minimum arity enforced, widens branch narrowings after
  reassignment, and rejects `return`/`yield` from `defer` blocks.
- Relocated the `agents-protocol-receipts/` and `agents-protocol-replay/`
  artifact directories under `spec/agents-protocol/{receipts,replay}/`, and
  folded the hidden `.experiments/` manual-eval scripts into
  `experiments/manual-evals/`. The public artifact URLs now live under
  `spec/agents-protocol/`.
- Split the Harn LLM configuration internals into focused modules without
  changing provider configuration behavior.
- Split the Harn LLM capabilities internals into focused modules without
  changing provider capability behavior.
- Run the generated-artifact registry guard through its Harn implementation.
- `harn models lora plan` now includes train/serve precision invariants in
  the plan and export metadata hints, and uses Gemma 4 LoRA target modules
  that cover both attention and MLP projections.
- **LoRA planning.** `harn models lora plan` now emits method-specific adapter
  target modules, including PEFT `all-linear` targets for QLoRA plans.
- Port the burin-mini qmode inspector from Python to Harn and run the
  experiment inspector through `harn run`.

### Removed

- **Removed the never-wired `[security]` config-file section.** Prompt-injection
  posture is a runtime directive, not a persisted config field: the only code
  that builds a `SecurityPolicy` reads the `security_policy(...)` pipeline dict
  (via `std/security`'s `spotlight()`/`strict()`/`local_ml()` helpers), and
  nothing ever read `HarnConfig.security`. A persisted `[security]` section
  therefore silently did nothing — a documented-but-inert surface that read as a
  security fail-open (write `mode = "strict"` in config, still get `spotlight`).
  The field, its JSON-schema block, the published `harn-config.schema.json`
  entry, and the misleading config docs are gone; posture is now configured only
  through `security_policy(...)`. Byte-identical at runtime (the field was never
  consumed). `HarnConfig` uses `deny_unknown_fields`, so a config that still
  carries a `[security]` section now fails to load instead of ignoring it.

### Fixed

- **CLI LLM mock replay now survives provider worker threads.** `--llm-mock`
  replay and recording scopes are carried on each request, so off-thread
  provider dispatch cannot silently fall through to a real provider.
- Fixed OpenAI `gpt-5.x` dispatch: reasoning models now send
  `max_completion_tokens` instead of the rejected legacy `max_tokens`, so
  `gpt-5.5`/`gpt-5.4`/`gpt-5.2`/`gpt-5.1`/`gpt-5` serve through the chat
  completions path again.
- Fixed OpenAI `*-codex` models (responses-endpoint only): they now
  auto-route through the Responses API instead of returning a silent HTTP 404
  on `/v1/chat/completions`.
- Fixed the Z.AI base URL (`https://api.z.ai/api/paas/v4`; the previously
  catalogued `.../v1` returned 404) and refreshed the GLM catalog to the live
  lineup (`glm-4.5`, `glm-4.5-air`, `glm-4.6`, `glm-4.7`, `glm-5-turbo`).
- Keep non-user-visible LLM progress deltas and internal completion-control
  verdicts out of visible assistant transcript text.

## v0.9.7

### Added

- Add `harn canon check` to evaluate harn-canon invariant packs against changed files through the Harn stdlib.
- **Read-only stance (experimental, default-off).** `agent_loop` gains a
  `read_only_stance` option: tasks classified as read-only get a
  least-privilege tool window (read-only-annotated tools only; unannotated
  tools count as mutating) plus an auto-registered `request_write_access`
  escape hatch whose consent check verifies — agentically, against the
  session's recent user messages — that the user expressed or implied consent
  before mutating tools return. Transitions emit typed `stance_transition`
  events (armed / write_access_granted / write_access_denied / disarmed) on
  the agent event stream and the ACP session-update channel.
- Add a self-contained `resolved_dispatch` transcript record emitted per
  agent-loop LLM call: the final resolved provider, model, wire format
  (`anthropic_native` vs `openai_compat`), base URL host, thinking config, tool
  format, per-field provenance (including `inherited_from_primary`), and a
  normalized outcome that distinguishes `served`,
  `empty_completion_transient_recovered`, `empty_completion_terminal`,
  `usage_limit`, and `provider_error`.

  A new deterministic
  `harn provider dispatch-explain <provider> <model> [--thinking] [--tool-format ...] [--json]`
  command reports the same wire-format/tool-format/thinking resolution
  statically, with no network or LLM call.
- Added `std/verification::verification_gate_input`, a structured reducer that
  combines stale-diagnostic classification with diagnostic-delta progress credit
  for loop gate policy. Harn's agent stall detector now uses that reducer so
  stale or unbound diagnostics stay advisory instead of feeding no-progress
  streaks.

### Changed

- Raised the Linux x86_64 release binary-size ratchet to 192 MiB after the
  v0.9.6 build measured 190.13 MiB, keeping the gate narrow while unblocking
  release asset recovery.
- `std/agent/canon` now infers harn-canon packs only from `canon-packs.json`, keeping manifest
  routing as the single source of truth.
- `harn models lora inspect` can compare adapters against LoRA export manifests and surface contract drift before promotion.
- `harn models lora plan` and `export` now report a reusable adapter promotion
  contract with minimum trial count, base-vs-adapter baseline, required metrics,
  and contract-id drift gates.
- `make check-generated-registry` now runs through a buildless Python auditor, so
  hook-only pushes and release recovery paths no longer compile Harn just to check
  Makefile/workflow/hook registry drift.

### Fixed

- **Typechecker call checking is now rest-aware and consolidated (#3922).** Generic
  return inference now binds type parameters from every variadic argument, and
  user functions that shadow builtin names report their own arity diagnostics.
- **A thinking-enabled Anthropic (Claude) model routed over the OpenAI-compatible
  transport is now rejected at dispatch with a clear error instead of silently
  serving a billed-but-empty completion (#3956).** Anthropic's OpenAI-compatibility
  surface bills the thinking budget but never streams extended thinking, so this
  pairing — usually caused by a dropped or mis-scoped provider on an escalation
  path — used to fail far downstream with no structured cause. A new typed
  `Route::resolve` validates the `(provider, model, thinking)` triple before any
  HTTP call and errors loudly, pointing at the likely upstream provider-drop.
  Valid routes (native Anthropic, non-thinking compat calls, and legitimate
  non-Anthropic reasoning models over compat) are unaffected.
- `harn-hostlib` tool results now emit forward-slash-separated paths on every
  platform. Search matches, staged/committed/discarded file labels, command
  artifact paths (`output_path`/`stdout_path`/`stderr_path`), git repo roots,
  code-index roots, filesystem snapshot paths, and directory-watch events
  previously leaked OS-native backslashes on Windows (`crates\foo\bar.rs`),
  breaking the path invariant the model and every path-consuming pipeline
  assume. All agent-facing path strings now route through a single
  `to_agent_path` normalizer, guarded by `check_agent_path_normalization.sh`.
- Fix the streaming empty-completion error at the shared SSE parser hardcoding
  "openai-compatible model" even for native Anthropic streams. The throw now
  names the actual wire style (`anthropic-native` vs `openai-compatible`) and the
  concrete `provider:model`, so a native Anthropic empty-stream flake no longer
  prints a misleading "openai-compatible" label.
- **`harn models lora export` now preserves grouped tool results.** Structured
  LoRA exports convert multiple `[result of ...]` blocks in one user message
  into ordered tool-role messages instead of collapsing the group into prose.
- Added storage-scoped OAuth refresh locking so std/oauth clients re-read tokens inside
  a single-flight transaction before spending refresh grants.

### Security

- **Command-argument provenance (opt-in).** Under `taint_command_reads`,
  untrusted-origin file provenance extends from structured `read_file` calls to
  the command surface: an `Execute`-kind tool whose command string names a
  tainted-origin file (`cat vendor/dep/README`) is classified untrusted by the
  same file origin, so a payload laundered back into context outside a
  structured read still arms the taint / lethal-trifecta gate. This closes the
  `tool_result` residual — the fetch-to-disk-then-`cat` laundering path that
  evaded lexical file provenance. It fires only on paths already recorded
  untrusted (via taint-on-write), so a first-party `cat src/main.rs` stays
  trusted and no new confirmations land on ordinary command use. Default OFF
  (byte-identical behaviour when disabled). With this on alongside directive
  authentication and file provenance, the containment battery reaches full
  coverage of its worst-case corpus (every modelled ingress — fetch/MCP
  provenance, cross-agent channel, on-disk read, and laundered command read — is
  contained).
- **Hygiene passes require spotlight framing.** `SecurityPolicy::from_config`
  now gates `neutralize_special_tokens` and `destyle_untrusted` on
  `spotlight_external`. Both passes run only inside `spotlight_wrap`, which the
  agent host invokes solely under `if policy.spotlight_external`, so "hygiene on,
  spotlight off" was an inert combination that additionally made `policy_summary`
  misreport the active posture. Gating them on their framing prerequisite
  (mirroring the file/command-provenance and precise/trifecta gates) removes the
  nonsensical subset while preserving the meaningful granularity — toggling a
  hygiene pass off *within* spotlight. Default posture is byte-identical.
- **Precise exfil gate requires the trifecta gate.** `SecurityPolicy::from_config`
  now gates `precise_exfil_gate` on `trifecta_gate` structurally. The precise
  gate only narrows the coarse trifecta gate — its logic runs solely inside
  `trifecta_gate_reason`, which is called only when the trifecta gate is armed —
  so it is inert on its own. Gating it on its prerequisite (mirroring the
  existing file/command-provenance gate) means the nonsensical "precise gate, no
  trifecta gate" configuration can no longer arise from config or a future
  caller. The live install path routes through `from_config`, so the invariant
  holds end-to-end. Default posture is byte-identical.
- **Secret-read gate requires the trifecta gate.** `SecurityPolicy::from_config`
  now gates `gate_secret_reads` on `trifecta_gate`. The secret-read arm is
  evaluated only inside `trifecta_gate_reason`, which runs solely when the
  trifecta gate is armed, so it is inert on its own. Gating it on its
  prerequisite (mirroring the precise-exfil gate) removes the dead
  "secret-read gate on, trifecta gate off" configuration. Default posture is
  byte-identical.
- The `strict` and `local-ml` security tiers now bundle the origin-provenance
  defenses — directive authentication, untrusted-origin file taint, command-read
  taint, and the precise (destination-aware) exfil gate — on from the mode alone.
  Previously these opt-in flags had no runtime install path (`policy_from_dict`
  dropped three of them and no caller set them), so the defenses were reachable
  only from `#[cfg(test)]`. Command-read taint is now structurally gated on file
  taint, so the inert "command reads without file provenance" combination can no
  longer be configured. The default `spotlight` posture is unchanged.

## v0.9.6

### Added

- **`harn usage` — LLM spend/usage analytics.** Aggregates the
  `provider_call_response` records Harn already emits into cost, token, and
  prompt-cache-efficiency rollups by provider, model, or a day/week/month
  cumulative time series. Reuses the runtime-computed `cost_usd` (no pricing is
  recomputed) and the same event-log reader `harn portal` uses. Supports
  `--since`/`--until`, `--provider`/`--model` filters, `--all` cross-project
  discovery, and `--json`/`--csv` output; `mock`-provider rows are excluded by
  default.
- **`harn lint --strict` promotes lint warnings to a non-zero exit.** The flag
  overrides `[check] strict` in `harn.toml`, so a single invocation can deny
  warning noise (e.g. in CI) instead of leaving every finding advisory. The
  bundled demo scenarios and repo `scripts/*.harn` lint clean under it, and
  `make lint-harn` now runs those surfaces with `--strict` so lint noise cannot
  regress.

### Changed

- Archived the pre-v0.8 changelogs under `changelog/archive/`
  (`CHANGELOG-pre-0.8.md`, `CHANGELOG-pre-0.6.md`) to keep the repository
  root focused on the active `CHANGELOG.md`. Links in `CHANGELOG.md`,
  `CONTRIBUTING.md`, and the markdownlint ignore glob were updated to the
  new paths.
- **The CI "Harn conformance + audit" lane runs its gate battery in
  parallel.** `scripts/audit_gates.sh` builds the harn CLI + runs the
  conformance suite once, exports the warm `target/debug/harn` as `HARN_BIN`
  so no downstream gate re-walks cargo's build graph, then hands the ~25
  independent gates to `make -j -k`. The serial check-`*`/lint tail collapses
  from `sum(gates)` to `max(gate)` (measured ~116s → ~41s, 2.83x, on a warm
  binary) with identical verdicts, and `-k` reports every gate's result
  instead of stopping at the first failure. Mirrors the proven
  `release_gate.sh audit` fan-out.
- `harn local launch` now lets provider catalog rows choose a LoRA module value
  shape, with vLLM using lineage-preserving JSON module specs while the public
  `--lora-adapter NAME=PATH_OR_REPO` flag stays portable.
- **Generic inference joins conflicting candidates to a union.** `keep(1, "x")`
  against `fn keep<T>(a: T, b: T) -> T` now infers `T = int | string` (matching
  heterogeneous list-literal inference and TypeScript) instead of hard-erroring
  "type parameter 'T' was inferred as both int and string". Explicit type
  arguments (`identity<int>("oops")`) remain a frozen contract checked
  per-argument — they no longer run arg-driven re-inference at all.
- **`match` on a `bool` scrutinee must be exhaustive.** `match b { true -> … }`
  with no `false`/wildcard arm now errors like enum and union matches do.

### Removed

- Removed the orphaned `test_fixtures/execute_response_raw.txt` sample
  transcript (no reader anywhere in the tree; last touched in an unrelated
  v0.5.34 release commit) and dropped the now-empty `test_fixtures/` from the
  changelog-fragment gate's ignore list.

### Fixed

- **Hostlib search now bounds oversized line payloads.** `hostlib_tools_search`
  clips long matched/context lines at UTF-8 boundaries, keeps matched-line
  snippets centered on the hit, exposes `max_line_bytes` for presets/APIs, and
  marks the response `truncated` when either match count or line content is
  clipped.
- **Ternary branch merging matches if/else expressions.** `cond ? 1 :
  unreachable(…)` infers `int` (the `never` arm collapses) and nested unions
  flatten/dedup instead of producing `Union[Union[…]]` shapes that defeated
  downstream narrowing.
- **Aliased collection receivers keep their element/value types across all
  methods.** With `type Env = dict<string, string>`, methods like
  `.map_values()`, `.merge()`, `.window()`, and `.iter()` now see through the
  alias the way `.values()`/`.keys()` already did.
- **The falsy branch of `schema_is(x, S)` no longer over-narrows.** Subtracting
  a literal schema (`"a"`) from `string | int` kept only `int`, wrongly
  dropping the whole `string` member; members are now subtracted only when
  every value of the member matches the schema.
- `hostlib_tools_search` now reports match paths with forward-slash separators on
  every platform, matching the rest of the agent tool surface. Previously Windows
  emitted OS-native backslash paths (`crates\foo\bar.rs`), which shipped
  non-portable paths to the model and broke path-suffix matching in downstream
  tooling and tests.

### Security

- **Cross-agent zero-trust (opt-in).** Under `authenticate_directives`,
  `classify_result_trust` now distrusts a result returned over a delegation /
  A2A channel by ORIGIN — a tool annotated with an `agent_channel` capability —
  rather than by a forged-authority keyword vocabulary. A peer agent's output
  may itself have ingested untrusted content, so it is quarantined as untrusted
  data and cannot smuggle authority regardless of phrasing; provenance-stamped
  hand-offs still authenticate. The containment battery shows this lifts
  cross-agent-poisoning containment from 1/10 (keyword authenticator) to 10/10
  and overall exfil-sink containment from 0.49 to 0.63 under the opted-in
  posture, with the default posture byte-identical.
- **Precise exfil gate (opt-in).** Under `precise_exfil_gate`, the
  lethal-trifecta exfil axis fires only on the real attack signature — the
  untrusted content controls the destination (an endpoint it named, recovered
  even from a steganographic payload), the payload ships a secret, or the
  untrusted content was flagged as a likely injection — instead of on any
  exfil-capable tool while any untrusted content is in context. Benign
  research-and-synthesis to a user-named or configured destination (a doc, a
  connector) is no longer confirmed. Destinations are matched after de-cloaking
  Unicode tag smuggling (ASCII smuggling) and zero-width / bidi host splitting,
  so a hidden exfil destination cannot slip the narrowed gate. The multi-step
  "structuring" case — a danger triangle assembled from individually innocent
  steps — is already covered: the taint ledger is context-global and persists
  for the session, so the gate fires when the exfil leg runs no matter how many
  benign steps separate it from the untrusted ingress. Default OFF (the coarse
  gate is byte-identical when disabled). The new exfil-precision battery pins
  the effect: the coarse gate confirms every benign workflow; the precise gate
  confirms none while containing every attack, including the hidden-destination
  ones.
- **Untrusted-origin file taint (opt-in).** Under `taint_file_provenance`, a
  file written while untrusted content is in the session's context — or by a
  fetch / clone / MCP step — is recorded in a session-scoped provenance ledger,
  and a later read of that path is classified untrusted so it flows into the
  same lethal-trifecta gate as a live external ingress. This quarantines a
  deferred on-disk injection (a cloned dependency's `README`, a downloaded
  dataset) that a plain first-party file read would otherwise carry straight to
  an exfil sink. First-party file reads stay trusted (a file you authored is not
  an injection vector). The containment battery shows this lifts overall
  exfil-sink containment by exactly the on-disk file-read attack count; the
  default posture is byte-identical.

## v0.9.5

### Added

- Added `std/verification::verification_diagnostic_delta` for row-normalized
  diagnostic-set comparison with advisory/stale suppression and deterministic
  progress credit.
- Added `std/verification::verification_warm_state_facts` and warm-state-aware
  ladder planning so slow verifier rows can choose warm or cold timing per
  profile row.
- **Lethal-trifecta containment battery.** `security::battery::run_containment_battery`
  drives the malicious ASR corpus through the lethal-trifecta gate, model-free
  and deterministic, and reports per-class containment: does each attack's
  ingress register taint so a fully-obeyed exfiltration attempt is forced to
  confirm? It measures the product-level guarantee — *even a fooled model is
  contained* — that the detection tier alone cannot show. The pinned baseline
  (default posture) contains network-boundary ingress (`web_fetch`, mounted MCP)
  but exposes the honest residual: subagent/A2A channel messages register no
  taint, so cross-agent poisoning is uncontained unless directive
  authentication is enabled — and even then the current marker vocabulary
  catches only canonically-framed forged authority.
- **Bare enum-variant match patterns.** `match r { Ok(v) -> { … } Err(e) -> { … } }`
  now works without the `Result.` qualifier — a call-shaped pattern resolves to
  its enum whenever the variant name is declared by exactly one visible enum
  (ambiguity is a compile error asking for the qualified form; non-variant call
  patterns keep expression-equality semantics). Bare patterns bind payloads,
  count toward exhaustiveness, and work on user-declared enums.

### Changed

- **`std/agent/canon` root discovery.** Canon helpers now resolve harn-canon
  roots from explicit options, `HARN_CANON_ROOT`, or workspace-local `.harn/canon`
  before evaluating packs.
- Slimmed push-to-main CI by relying on merge-queue Rust coverage and keeping main pushes on cheap hygiene lanes.
- Wrap `harn models lora plan|inspect|export --json` output in the canonical CLI JSON envelope and
  register the commands in `harn --json-schemas`.
- Factor shared CLI render helpers for `harn models lora` plan, inspect, and export into `std/cli/render`.
- `harn models lora plan` and `harn models lora export` now surface a
  machine-readable training contract for assistant loss masks, packing, parser
  ownership, and split policy in reports and export manifests.

### Fixed

- Route agent stall repair/no-net-progress diagnostic accounting through the
  stdlib verification diagnostic-delta helper so stale, unbound, and advisory
  diagnostics no longer advance hard repair gates.
- Honor exported `HARN_BIN` across git hook Harn checks so hook-spawned Make
  targets reuse the same binary instead of rebuilding it.
- Fixed `harn models lora inspect` so it no longer prints a local LoRA launch
  command for providers that do not declare LoRA launch support.
- Fixed `agent_loop` runtime feedback labels so ordinary post-turn callback messages use
  `post_turn` and terminal callback rescue messages use `terminal_callback`.
- Keep `hostlib_tools_search` glob filters inside the normal ignore-aware file
  walk so broad globs no longer re-include gitignored build output such as
  `target/`.
- **Generic enum payloads bind their instantiated types in match patterns.**
  Matching a `Result<int, string>` with `Result.Ok(v)` used to bind `v` as the
  raw declaration parameter `T` (so `return v` from a typed fn errored with
  "expected int, found T"); the scrutinee's type arguments are now substituted,
  and statically-unknown instantiations degrade to gradual instead of leaking
  phantom parameter names.
- **Container writes are type-checked.** `xs[0] = v`, `d["k"] = v`, and
  `s.field = v` now validate the value against the element/value/field type,
  check subscript index types (`list` → `int`, `dict<K, V>` → `K`), and emit
  the same receiver diagnostics as reads (nilable receiver, unknown field on
  annotated shapes/structs). The unannotated dict-literal idiom stays lenient.
- **Flow narrowing is scope-chain aware.** Assigning inside a nested block or
  loop no longer produces spurious "assignment to `x`: expected string, found
  nil" errors on `string?` vars (the check target is the declared type), loop
  bodies invalidate narrowing for variables they reassign (both inside the loop
  and after it — `while` conditions re-narrow soundly), and path-narrowing
  invalidation now masks ancestor-scope entries instead of only local ones.
- **`type_of` narrowing recognises the full runtime tag vocabulary.**
  `type_of(x) == "duration"` (and `set`, `decimal`, `channel`, `range`, `pair`,
  …) now narrows like `list`/`dict` always did; the canonical tag list lives in
  `harn-builtin-meta` and a VM unit test keeps `VmValue::type_name` in lockstep.
- The HARN-OWN-001 immutable-assignment repair hints now say `var`/`let`
  instead of the nonexistent `mut` keyword.

### Security

- **High-resolution ASR battery.** `security/fixtures/asr-battery.json` grows
  from 14 fixtures (1–2 per class) to 94 (≥10 *distinct* mechanisms per
  role-confusion class + 11 false-positive controls), so per-class attack-success
  rate resolves a small effect instead of quantizing to 0/1. New `battery.rs`
  invariants make the corpus self-guarding: unique ids, exactly-one `{CANARY}`
  per coupled behavioural payload, no duplicate payloads (trial independence),
  reserved-token presence for special-token attacks, and a ≥10-per-class floor.

## v0.9.4

### Added

- **Verification facts now expose seq-bound file-hash snapshots (#3818).**
  `hostlib_code_index_file_hash_snapshot`,
  `std/verification::verification_file_hash_snapshot`, and
  `std/code_librarian::code_librarian_file_hash_snapshot` capture batched
  current file hashes with index metadata so stale diagnostics and background
  checks can bind results to explicit file snapshots without duplicating
  hashing logic.
- Added verification profile matching and data-driven ladder planning helpers in `std/verification`.
- Added `std/verification::verification_affected_targets` for config-declared
  affected-target facts with Cargo, JS workspace graph, generic JSON, generic
  line, and code-index fallback adapters.
- Added `std/verification` toolchain identity facts from config-declared probe
  rows, including version extraction, cache identity, and non-throwing unavailable
  facts for missing tools.
- Added a semantic *stance* tier to the behavioral ASR probe
  (`security::stance_judge`): a judge that reads the framed attack turn and the
  model's reply and classifies obeyed-vs-resisted, run as a post-processor over
  the `BEHAVIORAL_PROBE_DUMP` transcripts. The deterministic canary metric alone
  conflates a model that *obeyed* an injection with one that *refused but quoted*
  the canary while describing it; the judge separates them, surfacing
  narrate-and-quote false alarms (canary hit, judged resisted) and subtle
  obedience the canary missed (canary absent, judged obeyed). The judging logic
  is unit-tested against a mock; the live judge is an on-demand `#[ignore]` run.
- Added `std/agent/canon` helpers that build harn-canon slices from changed paths and inject feedback for those paths.
- `harn models lora export` now includes a stable LoRA contract id in reports,
  exported row metadata, and provenance manifests.
- **`harn models lora plan` now prints the matching export command.** The plan
  includes a manifest-backed `harn models lora export` invocation so LoRA
  dataset export, trainer inputs, evals, and serving all share the same resolved
  model/tool-call contract.
- Added `std/verification` helpers for converting command results into
  verification profile observations, recording check timing, running one
  snapshot-bound check through the profile store, and starting/finishing
  background checks with the same snapshot-bound receipt shape.

### Changed

- `harn models lora plan` now records LoRA rank, alpha, and dropout in the training
  contract and propagates the planned rank into local launch hints when the runtime
  supports max-rank configuration.
- `harn models lora plan` now reports the tool-calling SFT trainer contract,
  including assistant-only loss masks, required tool-calling columns, and
  packing boundaries.
- Update OpenRouter Qwen3.6 Flash and GLM-5.2 catalog cache-pricing metadata from the live model API.
- **Provider matrix evidence.** The generated provider matrix now shows catalog
  parity notes as the evidence fallback for unsampled model/tool-format rows,
  so documented GLM/Qwen/MiniMax/Kimi provider quirks point at the probe record
  instead of reading as uncollected data.

### Fixed

- **Pattern learning in read-only agent modes.** Learned-context lookup now skips
  legacy migration writes, and post-run observation degrades to explicit
  unavailable metadata when storage is denied, so read-only/headless agent runs
  no longer abort before the first model call.
- Fixed release publishing retry classification by capturing cargo output
  synchronously before matching retryable errors.
- `release_gate.sh prepare` no longer reruns a full workspace cargo check after
  release audit and post-bump protocol artifact generation have validated the
  release branch.
- Release version-bump tooling now rewrites local path dependency requirements
  for every Cargo workspace member, including root-level members such as
  `tree-sitter-harn`.
- Made SpawnToPool trigger conformance fixtures use per-run pool names so repeated or adjacent runs
  cannot share stale pool state.
- Z.AI GLM reasoning now lowers through the Harn capability matrix to
  `thinking: {type: ...}` plus `reasoning_effort`, matching the current GLM-5.2
  API instead of relying on generic OpenAI-compatible reasoning fields.

## v0.9.3

### Added

- `std/agent/canon` now infers harn-canon packs from `canon-packs.json` routing metadata when a manifest is available.
- Added `std/coordination::coord_with_dir_lock` for scoped directory-lock critical
  sections that release on success or thrown errors.
- **Mid-conversation MCP mounting for skill-declared servers (default-off).**
  MCP servers were only bootstrapped once, at agent-loop entry, so a skill that
  activated mid-conversation could never surface its MCP tools. A SKILL.md
  frontmatter field `mcp` (alias `mcp-servers`) now carries opaque MCP server
  specs, and — when the new default-off `mid_conversation_mcp_mount` loop opt is
  set — the loop mounts any server an active skill declares that is not already
  active. `agent_mcp_mount_additional` bootstraps only the delta (tracked via
  the running `_mcp_server_info` list), reusing the same catalog merge and
  `__with_mcp_tool_ceiling` admission as the initial bootstrap so the new
  `server__tool` entries become visible AND callable without re-connecting a
  live server or duplicating tools/ceiling entries. `install_session_mcp_clients`
  now merges (rather than replaces) the session MCP client map so an incremental
  bootstrap never drops live handles. With the flag off the one-time bootstrap
  path is byte-identical to before.
- Added an opt-in transcript dump to the behavioral ASR probe
  (`security::behavioral`): set `BEHAVIORAL_PROBE_DUMP=<path>` and every probe
  appends its full transcript (framed user turn, raw reply, canary, scored
  outcome) as JSONL. A live A/B — base vs. a LoRA-adapted model — can then be
  root-caused from the actual replies instead of aggregate counts, which is what
  distinguishes a model that *obeyed* an injection from one that merely *narrated*
  it and happened to quote the canary. Env unset is a byte-for-byte no-op, so CI
  (mock models, no env) is unchanged. The on-demand baseline doc also records
  that `mlx_lm.server` 0.31.3 ignores per-request temperature, so a local "N=5"
  read degenerates to N=1 — confirm variance before claiming a bootstrap CI on a
  local surface.
- Added a degenerate-variance guard to the behavioral ASR baseline: after the
  trials loop, if every trial produced an identical outcome signature it warns
  that the effective N is 1 and a bootstrap CI must not be claimed. This is
  provider-agnostic — it catches any temperature-ignoring serving surface (the
  confirmed `mlx_lm.server` 0.31.3 `mx.compile` RNG bug, a misconfigured server,
  or simply `temp=0`) at runtime, so the harness detects the quirk instead of
  hardcoding a brittle per-provider capability list.

### Changed

- `harn models lora plan` now includes serving notes that keep parser ownership
  and native tool strictness aligned with the selected model route and tool-call
  format.

### Fixed

- Prevented main-push release checks from treating in-progress binary asset uploads
  as a cargo publish retry signal.

### Security

- **Origin-authenticated cross-agent directives (default-OFF `authenticate_directives`).**
  Defends the measured `cross_agent_poison` weak class — a forged
  `Orchestrator directive:` / `Coordinator override:` planted inside a subagent's
  untrusted result that the model obeys as if it were a real orchestration
  directive (arXiv:2504.16902 / arXiv:2506.23260). The new
  `security::provenance` module stamps a legitimate directive with a
  process-scoped HMAC over `(emitter, body)` — reusing the same per-process
  signing pattern the channel journal already uses, not a new PKI — and
  authenticates directive-looking spans on the read/ingest path: a marker with a
  stamp that verifies is `Authenticated` and passes through; a marker with no /
  invalid stamp is `Forged` and is classified `TrustLevel::Untrusted`, flowing
  into the existing `TaintRecord` ledger and lethal-trifecta gate so it is
  quarantined as DATA and can never reach an egress/write sink without approval.
  Wired into `agent_session_host` behind the default-OFF `[security]` flag
  (byte-identical when disabled); the existing MCP/fetch taint tagging and the
  trifecta gate already cover mounted-untrusted-connector quarantine, now proven
  by tests. Adds the `security_stamp_directive` / `security_verify_directive`
  builtins and a `trigger_directive_provenance_gate` conformance fixture.

## v0.9.2

### Added

- Added the behavioral tier of the ASR (attack-success-rate) battery
  (`security::behavioral`): a deterministic, judge-free probe that runs each
  role-confusion attack case through a model as a framed untrusted document and
  scores obedience by a per-case canary token. Where the static battery measures
  detection and containment, this measures the outcome that protects the user —
  whether the model actually obeys an injected directive under the shipped
  `spotlight_wrap` framing. Model access is behind a `BehavioralModel` trait so
  the aggregation is unit-tested with mocks (no network in CI); the live baseline
  is run on demand and is the pre-LoRA number role-robustness training must beat.
- Added `std/agent/canon` helpers that resolve harn-canon packs, evaluate Flow slices, render bounded
  feedback, and optionally inject it into agent sessions.
- Added a default-OFF `code_mode` agent tool (the CodeAct pattern): the model
  authors a short Harn script that composes the session's other tools as a typed
  API via `call_tool(name, args)`, keeping intermediate connector data out of the
  model context and returning only the composed result. The script runs in a
  restricted sandbox VM whose only egress routes through the same policy +
  approval + MCP-credential gate as the model's own tool calls, so a code-mode
  script's capability is provably ≤ the model's own and connector credentials
  never enter the script. Enable per session with `code_mode: true` or by listing
  `"code_mode"` in `enabled_tools`.
- `std/coordination` now exposes filesystem-backed directory lease helpers so Harn
  scripts can serialize cross-process work without shell-specific lock glue. Stale
  lease recovery is guarded by a second atomic cleanup directory to avoid
  cross-process delete/reacquire races.
- **`harn models lora export` can now turn tool-calling corpora into
  trainer-ready LoRA datasets.** The command resolves the target model route,
  exports Harn text/json or native tool-call rows, and can write a provenance
  manifest with corpus/output hashes and conversion stats.
- `harn models lora plan` and `harn models lora export` now include portable
  serving and adapter-binding metadata for LoRA promotion.
- **Role-hygiene ingress: special-token neutralization + destyling inside the
  spotlight frame.** `spotlight_wrap` now runs two structural passes on an
  untrusted body before framing it: `neutralize_special_tokens` rewrites reserved
  chat-template tokens (`<|im_start|>`, `[INST]`, `<|eot_id|>`, …) to
  `⟦special-token:…⟧` so they cannot re-open turns or inject a system message
  (ChatBug / ChatInject / MetaBreak), and `destyle_untrusted` neutralizes
  line-leading `User:`/`Assistant:`/`System:` labels and `<think>` reasoning tags
  (arXiv:2603.12277) so injected content cannot read as a real turn or
  chain-of-thought. Both are idempotent, surgical (benign look-alikes untouched),
  and default on for every non-`off` mode; new `[security]` knobs
  `neutralize_special_tokens` / `destyle_untrusted` toggle them via
  `std/security::configure`. The ASR battery now proves the delta in one run:
  special-token survival drops from **1.00** (framing only) to **0.00** under the
  default posture, and role-style survival is **0.00** for the tagged/prefixed
  attacks. String-level containment; a tokenizer-level guarantee over rendered
  token IDs is a planned follow-up.

### Changed

- `flow_invariant_feedback` now includes capped finding locations from Flow and
  harn-canon predicate reports by default, with `include_findings` and
  `max_findings_per_item` options for terse callers.

### Fixed

- **Cross-provider escalation from an OpenAI/Ollama-dialect primary to
  Anthropic no longer dies with `messages: Unexpected role "tool"` (HTTP 400).**
  A cheap OpenAI-dialect primary (e.g. Fireworks gpt-oss escalating to Claude
  Sonnet) records tool results as top-level `role:"tool"` messages. When
  escalation switched the provider to Anthropic and replayed that history,
  Anthropic rejected `role:"tool"` — it represents a tool result as a
  `role:"user"` message carrying a `tool_result` content block keyed by
  `tool_use_id`, never a top-level `role:"tool"`. The Anthropic request builder
  now translates any `role:"tool"` message into that shape at the egress
  boundary (before the canonical-key retain that would otherwise strip the
  source `tool_call_id`, and before tool-result adjacency enforcement so the
  real observation pairs with its `tool_use` block instead of being masked by an
  interrupted-before-dispatch placeholder). It also translates the ASSISTANT
  half of the same boundary: the primary's OpenAI-style top-level `tool_calls`
  array is rendered as Anthropic `tool_use` content blocks with the same ids
  (name + parsed `input`, preserving any accompanying assistant text), so every
  translated `tool_result` has its corresponding `tool_use` — closing the third
  stacked 400 (`unexpected tool_use_id found in tool_result blocks ... Each
  tool_result block must have a corresponding tool_use`). The quirk lives in the
  Anthropic adapter, so homogeneous-Anthropic and homogeneous-OpenAI/Ollama runs
  are byte-identical — only the cross-dialect escalation case changes.
- `harness.fs.mkdir(path, false)` now performs non-recursive, exclusive directory creation so
  Harn workflows can use directory creation as an atomic cross-process lock primitive.
- Release publishing now streams `cargo publish` output live while preserving retry
  classification, making crates.io/index waits visible in GitHub Actions logs.
- Restore the Linux x86_64 release-asset path by raising the release binary-size budget to match
  the current stripped Harn binary and adding a workflow/default drift guard.
- A `<tool_call>` block whose body is a JSON array of calls (`[{ "name": …, "arguments": … }]`) is no
  longer silently swallowed as prose. A single-element array now dispatches its one call identically
  to the bare and object-envelope forms; a multi-element array surfaces the actionable "one call per
  `<tool_call>` block" error instead of vanishing with no feedback. The array body was being intercepted
  by the narration-recovery path, which guarded `<`- and `{`-leading bodies from the prose fallback but
  omitted `[`.

## v0.9.1

### Added

- **Static ASR battery for the prompt-injection substrate.**
  `harn_vm::security::battery` measures `crate::security` against the
  role-confusion attack classes (arXiv:2603.12277 and the ChatBug / ChatInject /
  MetaBreak lineage) with no model call: `run_static_battery(mode)` reports the
  classifier's under-detection rate, the false-positive rate on benign controls,
  and the special-token survival rate through `spotlight_wrap`. The embedded
  corpus (`security/fixtures/asr-battery.json`) carries CoT-forgery, role-tag
  forgery, special-token smuggling, spotlight breakout, concealment, exfil, and
  cross-agent-poisoning attacks plus benign false-positive controls, each with a
  `injected_directive` / `success_signal` the Burin behavioural tier consumes.
  Baseline pinned 2026-07-02 (heuristic classifier, threshold 50%):
  undetected 0.82, false-positive 0.33, special-token survival 1.00 — the
  quantified headroom for the neural `local-ml` classifier and the
  token-neutralization work.
- **Canonical manifest-producing redaction for whole transcripts and run
  records.** `RedactionPolicy` gains `redact_json_manifest` — a single walk that
  scrubs an arbitrary JSON structure (a transcript, a serialized `RunRecord`, a
  session bundle) in place while returning an auditable `RedactionEntry` manifest
  for every value it touched — and `find_unredacted_secret`, the symmetric
  share/ingest gate that refuses a payload still carrying a high-confidence
  secret. These were previously private helpers inside `session_bundle`; they now
  live in `harn_vm::redact` so every export surface (session bundles today;
  portal transcript download, TUI export, and harn-cloud tape ingest next) calls
  one engine instead of reimplementing the walk and drifting from the
  leaf-scrubbing policy. `session_bundle` now consumes the canonical functions
  with identical behavior; `RedactionEntry` moved to `harn_vm::redact`.
- **Agent-run traces now carry first-class per-LLM-call token and cost
  attribution, plus tool-selection span kinds.** Each `llm_call` span records
  structured token usage keyed by canonical metadata constants
  (`input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`,
  `model`, `provider`) built through the typed `LlmCallUsage` helper, and its
  `cost_usd` is now `None` (honest "unpriced") rather than a misleading `0.0`
  when the (provider, model) pair has no catalog entry. `RunTraceSpanRecord`
  gains an optional, first-class `cost_usd` field so downstream viewers (Burin
  portal, harn-cloud dashboard) can build token/cost flame graphs without
  reconstructing them from cumulative-usage diffs; the field defaults to `None`
  so records persisted before it existed still load. Three point-in-time marker
  span kinds are added for the tool surface — `model_route`
  (`from_model`/`to_model`/`reason`), `tool_mount`
  (`tool_names`/`tool_count`/`source`/`detail`), and `deferred_tool_load`
  (`tool_name`/`query`/`score`) — emitted via typed `emit_*` helpers. `tool_mount`
  is wired at MCP bootstrap; `model_route` and `deferred_tool_load` ship the
  emission API for the escalation and `tool_search`-promotion sites to call.
  Telemetry only — no agent behavior changes.

### Changed

- `harn models lora plan` now accepts `--teacher` and `--corpus-strategy` to plan
  synthetic distillation or corpus-refresh LoRA data lanes with provenance fields,
  hard-negative coverage, and holdout-contamination gates.

### Fixed

- Reduced local hook rebuild churn by giving hooks a deterministic per-worktree
  Cargo target directory when none is configured and by running prompt-prose
  checks through one resolved `harn` binary instead of repeated `cargo run`
  invocations.
- **Internal engine bugs surfaced during tool dispatch now abort the agent
  loop instead of being swallowed as a recoverable tool error.** The loop's
  tool-dispatch catch sites only ever distinguished `cancelled` errors; every
  other failure — including a `VmError::UndefinedBuiltin` (a `#[harn_builtin]`
  def missing from its install array), corrupt bytecode, or another VM
  invariant violation — was folded into a synthetic tool-error observation and
  the run marched on to a `done`/`stuck` status with no log, no non-zero exit,
  and no test failure. That is what let a mis-wired builtin ship silently inert.
  A new `ErrorCategory::Internal` classifies these faults (both the structured
  `VmError::UndefinedBuiltin`/`InvalidInstruction` variants and the stringly
  `"Undefined builtin: …"` message form), the Rust tool-dispatch retry loop no
  longer wastes retries on them, and every agent-loop tool/classifier catch site
  re-raises them through a shared `__agent_error_must_propagate` predicate —
  exactly like `cancelled`. `error_category(err)` now returns `"internal"` for
  these so Harn middleware can react to them too.
- Update release smoke package fixtures and package-authoring examples to target the Harn v0.9 compatibility line.
- Hardened release audits against retry drag by keeping demo tests from copying
  ignored `.harn` runtime state, printing release-audit lane log paths
  immediately, narrowing the audit warm prebuild to the CLI binary, and reusing
  one stable package-check target for extracted crate verification.
- **The default-enabled reminder-provider set is now derived from a single
  source of truth instead of a parallel hand-maintained list.** Previously
  `canonical_providers()` (the provider objects) and `canonical_provider_ids()`
  (the default-enabled ids) were two separate arrays kept in sync only by
  convention. Adding a default-on provider but forgetting its id in the second
  list left it registered yet never fired — and that silent miss was
  indistinguishable from the intentional opt-in case (the burin compass, which
  is deliberately registered-but-off). `ReminderProvider` gained a
  `default_enabled()` method (defaulting to `true`; the compass overrides it to
  `false`), and the default id set is derived from `canonical_providers()`
  filtered by that flag, so a new provider fires by default automatically and
  the drift class is gone.

## v0.9.0

### Breaking

- `parallel` and `parallel each` are now fail-fast: the first branch that
  throws cancels all in-flight siblings (in-flight LLM/host calls are dropped,
  queued branches never start) and its error propagates out of the construct.
  Previously every branch ran to completion and the first error in source
  order was raised only after all branches finished. When several branches
  have already failed by the time the cancellation lands, the lowest-index
  branch's error is reported, so the propagated error stays deterministic.
  Cancelled siblings are still joined before the construct returns.
  **Migration:** if you need every branch to run regardless of failures,
  switch to `parallel settle`, which is unchanged and still runs everything,
  collecting per-branch `Ok`/`Err` outcomes.
- **Subprocesses now die with their invoking scope.** Foreground `run_command` /
  `run_test` / `run_build_command` / `manage_packages` tool commands and the
  VM-side `process.exec` / `shell` / `exec_opts` builtins spawn their child in
  its own process group and, when the invoking scope is cancelled, a `deadline`
  expires, or the VM is dropped, terminate the whole group — SIGTERM, a 2s grace
  period, then SIGKILL (Unix; best-effort direct-child kill on Windows).
  Previously such children (and their grandchildren) kept running as orphans
  until they exited on their own. Scripts that relied on an orphaned survivor
  should use the existing background form instead:
  `run_command({..., background: true})` children are exempt from scope
  cancellation and are reaped only via `cancel_handle` or agent-session-end
  cleanup. As part of the same change, `deadline`/host-cancel now preempt a
  *blocking* command mid-wait (the command returns `status: "killed"` and the
  scope error surfaces immediately) instead of waiting for the child to finish.

### Added

- **Capability rules support `extends = true` field-wise fall-through.** A
  matching `[[provider.<name>]]` capability rule that sets `extends = true`
  now contributes ONLY the fields it explicitly sets and lets resolution
  continue to later matching rules (user rules before built-in rules, then
  the `provider_family` chain) and ultimately to provider / built-in defaults
  to fill the rest. A rule without `extends` (or with `extends = false`)
  terminates resolution exactly as before, so every existing catalog and
  overlay is unchanged. This lets an overlay tweak one field of a shipped row
  without copying the whole row verbatim (which silently freezes the rest of
  the row against catalog updates). The capability matrix (`harn` audit /
  matrix surfaces) reports an `extends` row's own fields and, for a matched
  model, the full precedence chain of absorbed rule patterns.
- Added `flow_invariant_feedback(report, options?)` to turn Flow invariant
  reports into compact agent-feedback text.

### Changed

- Agent tool dispatch takes a fast path when no policy/permission machinery is
  configured: the session policy guard, execution-policy enforcement, and the
  dynamic-permission check are skipped (each is a provable no-op without a
  configured policy, permission scope, or cached session grant), and the JSON
  form of tool arguments is no longer deep-cloned twice per call.
  `perf/vm/agent_tool_dispatch` improves from ~72.5ms to ~65.5ms per run
  (3,000 dispatches; ~10% faster, ~2.3us/dispatch of avoided policy/permission
  setup; settled-min A/B on the same machine, ~1.5ms run-to-run noise). Any
  configured policy, approval, command policy, permissions option, ambient
  policy scope, or session grant routes through the unchanged slow path.
- `harn models lora plan` now reports the template convention to train
  against, including distinct guidance for native Gemma 4, FunctionGemma, and
  Harn text/json tool-call adapters.
- Release PR CI now skips redundant PR-head Rust, macOS, and Windows lanes
  when the release branch diff contains only generated release metadata, while
  preserving full merge-queue and post-merge backstops.
- Local git hooks no longer treat every Makefile-only change as a Rust
  workspace compile trigger; Makefile changes still run workflow lint and
  generated-artifact registry checks.
- The advisory CLI cold-start budget now skips release PRs, avoiding a
  version-bump-only release binary build that does not gate cold-start changes.

### Fixed

- **Agent-loop transcript integrity: no more orphaned tool_use / tool_result
  pairs.** Skip paths that persist an assistant tool_use turn without
  dispatching (pre-dispatch user interrupt, `agent_await_resumption`
  suspension and its parallel siblings, invalid await arguments) now record
  synthesized placeholder tool_results (`interrupted` /
  `awaiting_resumption` / `skipped`), so Anthropic-native sessions no longer
  400 with "tool_use ids were found without tool_result blocks" after an
  interrupt or resume. The Anthropic egress normalizer additionally
  backfills a deterministic placeholder tool_result for any orphaned
  tool_use id as a safety net, and auto-compaction never splits the kept
  window between an assistant tool-use message and its tool_result
  message(s).
- **Escalation no longer orphans a `tool_use` block into an Anthropic HTTP
  400.** When a text-format primary model escalated to a native-format model
  (e.g. Fireworks → Anthropic), the escalated model would emit a real
  `tool_use` block that the loop then declined to dispatch (native-format
  fallback reject, all-blank-name drop, parse-error, or no-progress nudge) and
  followed with a bare user-feedback message. That left the assistant
  `tool_use` with no matching `tool_result`, which Anthropic rejects with a
  non-retryable HTTP 400 (`tool_use ids were found without tool_result blocks
  immediately after`), killing the run before the escalated fix was applied.
  Every such inject path now first synthesizes a matching `tool_result` for
  each orphaned block (carrying the same corrective feedback as its
  observation) via the shared `agent_session_pair_orphaned_tool_use` repair, so
  the pairing invariant holds across the native / OpenAI / Gemini wire shapes.
  The repair is a strict no-op for homogeneous text-format runs (whose calls
  stay inline in `content`) and for blocks the loop already dispatched, so
  converging runs are unaffected.
- **Escalation tool-result pairing is now actually effective on text-primary
  runs (both the declined-dispatch AND the dispatched path).** The orphan
  repair, and the sibling `record_tool_results` dispatch path, both synthesized
  their tool-result using the session-locked `tool_format`. That lock is pinned
  to the PRIMARY model's format (`text`) at session init and is never re-claimed
  when the run escalates to a native model — so on the exact scenario the repair
  targets (a text-format primary escalating to Anthropic/OpenAI), the tool-result
  took the text-channel branch and was emitted as a bare `role:"user"` message,
  leaving the escalated model's native `tool_use` block orphaned and
  re-triggering the same non-retryable Anthropic HTTP 400. A structured native
  `tool_use`/`tool_call` block is native by definition (text/json channels carry
  calls inline in `content` and produce no structured blocks), so both paths now
  synthesize the tool-result in the provider's native shape (anthropic
  `tool_result`+`tool_use_id`, openai `tool`+`tool_call_id`) when the assistant
  turn carries native blocks, regardless of the session lock. Homogeneous
  text-channel runs and already-dispatched blocks remain strict no-ops.
- Sanitize nested OpenAI-compatible assistant `tool_calls` history before
  provider dispatch so strict OpenRouter/Fireworks routes do not receive
  storage-only or telemetry fields.
- **CI now catches `#[harn_builtin]` defs that are declared but never
  installed on a live VM.** A builtin annotated with `#[harn_builtin]` is
  auto-added to the linkme `ALL_BUILTIN_DEFS` slice, but *installing* it onto a
  running VM still runs through hand-maintained `register_*` functions (the
  `LLM_RUNTIME_PRIMITIVE_BUILTINS` array, `register_agent_session_host_primitives`,
  the per-module `register_*_builtins`, …). A def could therefore exist — and
  pass every parser-alignment test — yet never be wired into runtime dispatch,
  so any call threw `Undefined builtin` at runtime and got silently swallowed by
  the agent loop's outer `try` (this is how `__host_agent_undispatched_tool_results`
  shipped inert before the transcript-integrity fix). A new alignment test,
  `every_runtime_handler_builtin_is_installed_on_a_full_vm`, walks
  `ALL_BUILTIN_DEFS` against the fully-configured stdlib VM and fails the build
  if any runtime-handler def is missing from every `register_*` path, naming the
  builtin and the array to add it to.

## v0.8.169

### Added

- Verification profile store: `verification_profiles_get`/`verification_profiles_set`
  persist a versioned (`schemaVersion: 1`) record set of check rows through the
  hierarchical project metadata store, `verification_profile_resolve` picks the
  most-specific row for a `{repo?, path?, language?, task?}` scope query, and
  `verification_profile_record_run` folds run observations into per-row timing
  percentiles and the `lastRun` file→hash snapshot binding. Rows are pure data
  (no hardcoded languages/toolchains) and unknown fields round-trip untouched.
- Stale-diagnostic contract primitive: `verification_diagnostic_classify`
  classifies a diagnostic envelope `{rung, rowId?, at, snapshot}` against
  current file hashes as `bound_fresh` / `bound_stale` / `unbound`; only
  `bound_fresh` diagnostics may feed no-progress detectors, escalation streaks,
  verifier signatures, or completion gates.
- **`[patch.models]` field-wise catalog overlay patches.** Provider config
  overlays can now tweak individual model-row fields
  (`[patch.models."<id>"] stream_timeout = 1200.0`) instead of copying the
  whole baseline row verbatim and freezing its other fields against catalog
  updates. Tables merge recursively, scalars and arrays replace, patches win
  over same-overlay whole-row replacement, and they stay sticky across later
  layers' whole-row refreshes. Works at every overlay layer, including
  `harn.toml` `[llm.patch.models]`; dangling patches are held until the row
  arrives and reported via `dangling_model_patches()`.
- Added harn-hostlib helpers for normalized host-env custody metadata envelopes.
- Added `harn models lora plan` to emit portable LoRA/QLoRA tool-calling
  training, validation, eval, inspect, and launch recipes from Harn's provider
  capability matrix.

### Changed

- Internal cleanup: collapsed the per-module copy-paste hostlib builtin
  registration helpers into shared `BuiltinRegistry::register_fn` /
  `register_gated_fn`, migrated hand-rolled stdlib argument helpers
  (`bytes`, `files`, `multipart`, `observability`, `timing`) onto the
  canonical `stdlib/options.rs` layer, and deleted dead test-only Rust
  twins of the self-hosted `trace import` / `explain` CLI handlers
  (their coverage now exercises the shipping `.harn` scripts
  end-to-end). No user-facing behavior or error-message changes.
- Documented the online source and public issue cross-references behind the
  Qwen/GLM OpenRouter provider evidence.
- Correct Qwen 3.7 Max OpenRouter catalog tiering and document the live model
  metadata/probe evidence behind the Qwen hosted presets.

### Fixed

- **MCP client robustness: no more unbounded hangs.** All MCP OAuth HTTP
  requests (token exchange, refresh, discovery, dynamic registration) now use
  a client with a 30s request timeout and a 10s connect timeout — a token
  endpoint that accepts TCP but never responds can no longer wedge a refresh
  (and the single-flight refresh lock behind it) forever. The refresh lock
  itself is also bounded: the in-process mutex wait times out with a clear
  error naming the stuck holder, and the cross-process file lock uses a
  non-blocking try-lock with backoff instead of pinning a blocking thread
  indefinitely. On the stdio transport, response lines are capped at 64 MiB
  (protocol error instead of unbounded memory growth), and request writes now
  drain server output concurrently so a large request racing a flood of
  server notifications can no longer deadlock on full pipe buffers.
- **Worker snapshots no longer silently corrupt non-serializable values or
  persist secrets verbatim.** A new strict persistence serializer
  (`vm_value_to_json_strict`) rejects closures, channels, and other
  runtime-only handles at save time with a path-annotated error (e.g.
  `options.custom_compactor: closure is not serializable`) instead of
  writing a display-string that rehydrates as a plain string and fails long
  after resume. Workflow worker options fail loud; live sub-agent suspend
  options (which legitimately carry callbacks like `tool_caller`) strip the
  offending entry with a WARN event naming the dropped path. Every persisted
  worker snapshot is now scrubbed with the unified redaction policy, so
  `Authorization`/api-key-shaped fields and high-confidence token patterns in
  options, headers, and transcripts land on disk as `[redacted]`.
- **Pre-commit release-workflow edits no longer compile Harn unnecessarily.**
  The generated-artifact registry hook now runs for the registry, Makefile,
  CI workflow, and hook surfaces it actually audits, rather than every
  workflow file.
- **Anthropic streaming no longer dispatches tools with silently-empty
  arguments.** When accumulated streamed tool-argument JSON is malformed or
  truncated, the finalizer now emits the same recoverable `__parse_error`
  carrier the OpenAI paths build (the agent loop asks the model to re-issue the
  call) instead of running the tool with `{}`. A genuinely argument-less tool
  call still maps to `{}`.
- **Gemini request lowering stops dropping sampling params.** `seed`,
  `frequency_penalty`, `presence_penalty`, and `logprobs`/`top_logprobs` now
  map into `generationConfig` (capability-gated, matching the OpenAI-compat
  builder).
- **Provider overload (HTTP 529/503, `overloaded_error`) now feeds the
  per-route circuit breaker and the shared cooldown**, so parallel agents back
  off together instead of hammering an overloaded provider. Overload responses
  without a Retry-After header get a default 5s shared cooldown; 429 and
  generic 500/502 semantics are unchanged.
- **Vertex requests now delegate body shaping to the Gemini builder** (as
  Azure delegates to the OpenAI builder), fixing dropped multimodal parts and
  tool-call history and inheriting the sampling-param fix, while preserving
  Vertex-specific auth, model routing, `responseSchema` naming, prefill
  emulation, and the legacy `response_format: "json"` mirror.
- **Release binary builds no longer cancel healthy platform legs.** The
  release-binary workflow now lets every target finish even when one target
  fails, preserving incrementally uploaded archives and making recovery reruns
  fill only the genuinely missing assets. Recovery skip checks, publish
  self-healing, and release smoke now also require `SHA256SUMS`, so a release
  missing its checksum sidecar is not treated as complete.
- Release binary recovery now rebuilds only missing platform archives, regenerates
  missing release metadata without rebuilding binaries, and finalizes complete
  prereleases without rerunning the five-target matrix.

## v0.8.168

### Added

- **Stdlib coordination request/reply helpers.** `std/coordination` now includes
  `coord_request` and `coord_wait_reply` so harnesses can build durable
  addressed request/decision protocols with request-scoped acknowledgement
  cursors instead of product-local mailbox glue.
- Add `harn-hostlib::host_env_custody::HostEnvCustodyContract` for reusable
  named environment custody metadata with class-name-only validation.
- Add catalog-driven local runtime LoRA launch flags, vLLM launch metadata, and `harn models lora inspect`
  for adapter/base compatibility checks.

### Changed

- Move Harn's hosted `mid` preset to OpenRouter Qwen 3.6 Flash, add canonical
  OpenRouter Qwen 3.6 catalog rows, update GLM 5.2 OpenRouter pricing and
  reasoning-effort metadata, and keep Qwen/GLM provider quirks centralized in
  the capability matrix.

### Fixed

- Claude-family models now default to `native` tool calling at the family level on OpenRouter and on
  prefixed direct-Anthropic ids: catalog catch-all rules cover ids the versioned capability rows miss
  (new family names, unparseable version segments, dated slugs, pre-4.x models), which previously fell
  through to the global text-channel `json` default. Hosts no longer need per-alias `tool_format`
  pins for Claude routes; explicit pins still win.
- Preserve non-shell `run` command arrays as structured `argv` during tool-call
  compatibility normalization.
- `harn install --locked` / `--frozen` / `--offline` no longer fails with
  "harn.lock would need to change" after a Harn release bump when the resolved
  dependency set is unchanged: the frozen comparison now checks the resolution
  content and ignores the `generator_version` / `protocol_artifact_version`
  provenance stamps (still refreshed by non-frozen installs and audited by
  `harn package audit`). Frozen installs also now correctly fail when the
  manifest dropped all dependencies but the lock still pins packages.
- **`harn models lora inspect` now applies provider overrides consistently.**
  The report recomputes the effective tool format for the selected provider and
  model route, so LoRA launch hints no longer mix an alias's original provider
  metadata with an explicit `--provider` override.
- Include adapter rank in `harn models lora inspect` launch hints when the
  cataloged local runtime supports a max LoRA rank flag.
- **Release binary size gate recovery.** The x86_64 Linux release binary-size
  gate no longer depends on the checked-out tag's `check_binary_size.harn`
  script for the hard release check, so workflow-dispatch recovery can rebuild
  immutable older tags even when the tagged helper script fails to type-check.
  The release binary budget is ratcheted to 189.25 MiB after v0.8.167 measured
  189.07 MiB, keeping the guard tight without blocking a valid patch release on
  a 0.07 MiB threshold miss.

### Security

- **Linear connector conformance now exercises the signed, fail-closed package path (#2950).** The
  embedded contract fixture is synced to the current `harn-linear-connector`
  implementation so stale unsigned webhook deliveries and invalid signatures are
  rejected instead of documented as verified.

## v0.8.167

### Added

- Added `harn session checkpoint <worker-snapshot>` to export suspended worker
  snapshots as local resumable session bundles without hand-building an
  intermediate run record.
- **Coordination:** Added channel consumer cursor/ack builtins and addressed
  `std/coordination` inbox helpers for durable multi-agent coordination.
- `pub type` exports a type alias from a module. Importers can name it in
  selective imports (`import { SmartTarget, pick } from "./targets"`), use it
  in annotations, and pass it in schema positions (`output_schema:`,
  `schema_is`) — the loader binds the imported name to the alias's JSON-Schema
  lowering, and `pub import` re-exports it through facades. Type aliases
  without `pub` stay module-private and error on import, matching `pub fn` /
  `pub struct` visibility.
- `round(x, digits)` rounds to a number of decimal places (half away from
  zero, matching the 1-arg form): `round(2.567, 2)` is `2.57`. Negative
  digits round to power-of-ten buckets (`round(1250, -2)` is `1300`), ints
  stay ints when they fit, decimals keep the decimal type, and the 2-arg form
  const-folds in `const` initializers.
- Added `std/command` helpers for converting provider-style argv and command
  values into safe shell text without whitespace or quote corruption.
- Added `std/coordination`, a durable multi-agent coordination ledger facade over Harn channels, EventLog, and memory.

### Changed

- **Unused-variable lint now prefers true discard locals.** Simple unused
  Harn locals such as `_cleanup` now warn and autofix to `let _ = ...`, while
  underscore-prefixed parameters and pattern bindings remain exempt when the
  name carries intent (#3786).
- Ported the seven remaining queued Bash validation checks to self-hosted Harn
  scripts with byte-identical output parity: `lint_test_patterns`,
  `check_docs_cli_flags`, `check_binary_size`, `check_docs_snippets`,
  `check_docs_workflow_quickstart`, `check_docs_model_refs`, and
  `check_site_snippets`. Each has a paired `scripts/tests/<name>_test.harn`
  suite (104 new tests) and the `.sh` originals are deleted.
- `normalize_tool_call_shape` now folds a top-level `create` call into `edit({action: "create"})` like the
  other edit-action verbs, and a new negative test pins that shell listing `run` commands (`ls -R`, `tree`)
  pass through untouched — structured-listing ergonomics belong to the host.
- Clarified unused-binding diagnostic repair summaries to point at the `_`
  discard binding instead of underscore-prefixed names.
- **Unused-variable lint fixes now use the Harn discard binding (#3783).** The linter
  and editor quick fix suggest replacing unused bindings with `_` instead of
  inventing underscore-prefixed names, matching the language's existing discard
  parameter and binding semantics.
- Refreshed the bundled provider/model catalog against provider docs verified
  2026-07-02:
  - **Anthropic**: added Claude Sonnet 5 (direct + OpenRouter rows, intro
    pricing through 2026-08-31) and new `sonnet5`/`sonnet46` aliases; bumped
    the `sonnet`/`frontier` aliases to `claude-sonnet-5`; 4.6+ rows now carry
    their 1M-token context window (the long-context beta graduated to standard
    pricing) and `vision`; marked Claude Opus 3 retired (2026-01-05) and dated
    the Opus 4.1 retirement (2026-08-05); removed the erroneous
    `claude-sonnet-4-7` row (no such model — only the Opus line had a 4.7).
  - **OpenAI**: added the GPT-5.4 tier family (base/mini/nano) and
    GPT-5.3-Codex; deprecated o1/o1-mini/o3/o3-mini with their announced
    shutdown dates; `mid` tier alias and the openai QC default moved off
    gpt-4o-mini to gpt-5.4-mini / gpt-5.4-nano.
  - **Gemini**: fixed gemini-2.5-flash pricing (it carried Flash-Lite's
    $0.10/$0.40 rate; real rate $0.30/$2.50) and gemini-2.5-pro output/cache
    pricing and context window; added gemini-2.5-flash-lite,
    gemini-3.1-pro-preview, and gemini-3.1-flash-lite with capability rules;
    `small` tier alias moved off the stale OpenRouter Qwen3.5-9B route to
    gemini-2.5-flash-lite.
  - **Mistral**: added Codestral 25.08 and Devstral 2 Medium/Small rows,
    `codestral-*`/`magistral-*` inference rules, and a codestral capability
    rule.
  - **Open-weight hosts**: added DashScope first-party Qwen rows
    (`dashscope/<wire-id>` keys), OpenRouter Qwen3-Coder-Next and
    Qwen3.5-397B-A17B, Groq Qwen3.6-27B (with capability rule), Together and
    Fireworks MiniMax M3 / Kimi K2.7-Code, DeepInfra DeepSeek V4 Flash, and
    Z.AI GLM-4.7-Flash (free tier, now the zai QC default).
  - **Pricing corrections**: Moonshot Kimi K2.7-Code ($0.95/$4.00),
    DeepInfra DeepSeek V4 Pro / Kimi K2.7-Code, Fireworks DeepSeek V4 Pro,
    Z.AI GLM-5 ($1.00/$3.20), MiniMax M3 billed rate ($0.30/$1.20 permanent
    50%-off list) with weights now open (HF) and SWE-bench Pro 59.0.
  - **Deprecations**: Groq llama-3.3-70b-versatile (retires 2026-08-16);
    Together GLM-5.2 context corrected to its 262K per-host cap.
  - QC defaults: `local` moved off the sunset hosted `gpt-4o` id to
    `gemma-4-26b-a4b-it`; added a `gemini` QC default.
  - Capability fragments migrated the legacy `json_schema` field to the
    canonical `structured_output` name.

### Fixed

- Tool-ceiling denials no longer use permission framing: an unknown/excluded
  tool name now gets action-oriented "not one of the available tools" feedback
  (listing the callable tools), and a call named `tool_call` whose arguments
  smuggle one valid text-format call gets parse-repair feedback that names the
  embedded call and shows the direct invocation. Previously both fell into the
  generic "tell the user what you need permission for" denial body, which sent
  headless models into permission-request spirals with no user to ask.
  Genuinely permission-gated denials (capability/side-effect ceilings, approval
  and host rejections) keep the existing wording.
- Durable rate-limit concurrency tests now pin mock time so slow release-audit
  workers cannot expire the first queued request row mid-test.
- `harn fmt` no longer orphans `//` comments that sit between the segments of
  a multi-line method chain (they were relocated to the end of the program at
  column 0). Chain-segment comments now stay anchored above the segment they
  precede, for both `.method(...)` and `?.method(...)` chains.
- **OpenAI-compatible native streaming tool calls.** Added regression coverage
  ensuring streamed `container.exec` argv chunks finalize as canonical `run`
  tool calls instead of surfacing provider-native arguments.
- Corpus-driven text tool-call parser tolerance, grounded in 526 mined eval runs: back-to-back
  `</tool_call><tool_call>` blocks on one line no longer shred into stray-text violations; `<invoke>` markup
  tolerates extra `<parameter ...>` attributes (`string="true"`) instead of misdiagnosing complete calls as
  truncated; `<function_calls>` wrapper tags are swallowed silently; an unclosed terminal
  `<user_response>`/`<assistant_prose>` is accepted as the block body instead of killing the final answer;
  compat aliases (`replace_range`, `bash`, ...) now fold in the bare TEXT channel like they already did
  natively; `<|...|>` provider tokens are stripped from unresolvable tool names; and dispatch coerces
  `"True"`→bool and JSON-array strings→list on unambiguous schema expectations. Each observed failing
  emission shape is pinned as a conformance fixture in `tools/tests/corpus_conformance.rs`.
- Prevent parallel test resets from wiping LLM rate-limit state while another LLM test is running.
- **Release audit tests are deterministic under parallel load.** Harn VM
  waitpoint and action-graph regression tests now filter persisted event-log
  records by the run or waitpoint they created instead of depending on shared
  topic ordering or thread-local test signals.
- Hardened release-gate checks against parallel rate-limit state resets,
  external nested `harn` binaries under process sandboxes, and comment-anchored
  tree-sitter method chains.
- Lists now compare lexicographically (element by element, shorter prefix
  first), so multi-key sorts like `xs.sort_by({ x -> [x.a, x.b] })` order by
  the first key then the second instead of silently comparing equal. The same
  order backs `sort`, `min`/`max`, and the relational operators; a `NaN`
  element keeps the pair-style "unordered" semantics.
- Harden waitpoint replay tests against parallel `cargo test` by replacing the
  process-wide `HARN_REPLAY` toggle with a scoped in-process replay override.
- `while true { ... }` with no `break` binding to the loop now types as
  `never`: a function whose tail is such a loop no longer demands an
  unreachable trailing `return`, and statements after the loop are flagged
  unreachable. Adding a `break` at the loop's level restores the fall-through
  return requirement.

## v0.8.166

### Fixed

- Canonical native `run` tool calls now normalize `command`/`cmd` argv arrays into
  Harn command strings, matching the existing `container.exec` alias recovery path.

## v0.8.165

### Added

- Teach the Anthropic provider catalog and request builder about Claude Sonnet 5,
  including default-on adaptive thinking, `thinking: disabled`, native
  `output_config.effort`, and the provider-neutral `max` reasoning effort level.
- Added `harn pack unpack` and `harn pack repack` for native `.harnpack`
  inspection, mutation, and release-audit workflows without requiring host
  `tar` or `zstd` binaries.

### Fixed

- A2A streaming tasks now attach their worker-event sink directly to the active
  dispatch, so progress status updates keep streaming even if the process-global
  agent-event registry is reset by sibling work.
- A2A streaming tasks keep progress-event sinks registered until terminal task publication,
  preventing final progress updates from being dropped at dispatch shutdown.
- Session-scoped agent event sinks now deliver across worker threads in test builds,
  matching production registry semantics and stabilizing A2A progress streaming
  coverage under full-suite execution.
- Fixed escalated provider-error fallback so it restores the primary provider,
  model, and tool format instead of leaking the escalated route's tool-calling
  mode into the retry.
- Normalize Codex-style shell argv tool calls into Harn run command strings.
- Tool-call parse errors for an unquoted multi-line value (e.g. a raw code body
  pasted after `content:`/`new_text:`) now name the heredoc recovery
  (`key: <<BODY … BODY`) instead of dead-ending on "unexpected character starting
  a value", so a weak value model can self-heal instead of looping on the same
  malformed edit.

## v0.8.164

### Added

- **MCP HTTP transport now step-ups OAuth on a `403 insufficient_scope`
  challenge, not just a `401`.** Per RFC 6750 §3.1, a `403` whose
  `WWW-Authenticate: Bearer` header carries `error="insufficient_scope"` means
  the presented token is valid but lacks a required scope. Harn now treats it
  like a `401`: it emits `mcp_auth_required` (carrying the challenge's elevated
  `scope`) and re-runs the authorization flow, so a tool call that needs an
  additional scope recovers in place instead of dead-ending. A plain `403`
  without an `insufficient_scope` challenge is still a hard denial and falls
  through unchanged.
- MCP servers that announce a changed tool/resource/prompt list
  (`notifications/*/list_changed`) now emit an `mcp_catalog_changed` agent event,
  so a connected client re-fetches the catalog and surfaces newly added tools
  within the same session — no restart.

### Fixed

- **Release binary publishing now treats Actions run-artifact uploads as
  best-effort.** Official GitHub Release archives still publish through the
  hard `gh release upload` path, so transient artifact-store outages no longer
  block otherwise valid signed release artifacts before checksum and smoke gates
  can run.

## v0.8.163

### Added

- **Postgres query helpers can now compose trusted SQL fragments structurally.**
  `std/postgres/query` adds `sql_fragment`, `sql_and`, `sql_or`, `sql_not`, and
  `jsonb_object` so Harn data-access modules can build reusable predicates and
  JSON envelope projections without ad hoc string concatenation.

### Fixed

- **Verify/build/test subprocesses now run under a deterministic English message
  locale.** Spawned tool commands inherited the user's shell locale, so a
  non-Anglosphere user whose environment set `LC_ALL`/`LANG` to a localized value
  got translated (non-English) compiler and test output — silently breaking every
  downstream matcher that keys on English diagnostics (deterministic syntax
  repair, error-signature grounding, completion/pass-fail classification). Both
  spawn paths (`process.exec` builder and the `harn-hostlib` real spawner) now
  strip an inherited `LC_ALL` and pin `LC_MESSAGES=C` plus `DOTNET_CLI_UI_LANGUAGE=en`
  (the .NET CLI ignores `LC_*`), while deliberately leaving `LC_CTYPE`/`LANG`
  untouched so UTF-8 handling of non-ASCII source is preserved. An explicit
  caller-supplied `env`/`env_remove` still wins, matching the `TMPDIR` overlay.

## v0.8.162

### Fixed

- Narrowed the visible-text sanitizer's bare JSON control-message filter so
  legitimate JSON-only assistant answers with keys such as `tasks`, `steps`, or
  `reasoning` remain visible while leaked internal verdict envelopes are still
  hidden.

## v0.8.161

### Added

- Added opt-in `poll_command`, `wait_command`, and `kill_command` lifecycle tools to
  `agent_command_tools`, backed by the existing deterministic command handle builtins.

### Fixed

- **Visible assistant text sanitization now strips orphan protocol residue in the
  VM (#3757).** Final messages no longer surface truncated tool/prose markers
  or bare internal verdict JSON as user-visible assistant text.
- Fan-out agent workers now preserve and isolate the active host bridge across
  cooperative awaits, so child `host_call` operations keep using the intended
  host bridge when sibling tasks interleave.
- Isolate `harn test conformance` runtime state per case so ignored `.harn/metadata`
  output from earlier runs cannot make metadata fixtures flaky.
- Prevent the project-metadata demo test from reusing persisted metadata across flake-detection iterations.
- When a restricted policy declares no explicit `workspace_roots`, the file
  write/read jail now falls back to the active session's workspace anchor and
  the host-declared `HARN_PROJECT_ROOT` project before the process execution
  cwd. Dispatched sub-agent workers running where the process cwd differs from
  the project (the eval pattern) can now write into the project instead of
  being rejected with a `HARN-CAP-201` sandbox violation rooted at the cwd.
- Add a bounded terminal callback hook so agent loops can recover iteration-cap or stuck exits.

## v0.8.160

### Fixed

- Fixed sub-agent worker write scopes so unanchored child agents inherit the parent
  workspace anchor as their default execution cwd.
- **Orchestrator pump startup readiness.** The in-process orchestrator harness now
  waits for pending, inbox, cron, and waitpoint pumps to subscribe before it
  reports startup readiness, preventing immediately accepted trigger requests
  from racing ahead of the pump cursor and being skipped under CI scheduling.

## v0.8.159

### Added

- **Session-bundle liveness.** Exporting a session bundle from an event stream
  now classifies the session's liveness: a stream without a terminal
  `SessionClosed` event — a loop frozen mid-turn for cross-compute migration, or
  a time-traveled replay prefix — is labeled `suspended` (with a machine-readable
  `session_liveness` metadata tag and a null `finished_at`) instead of being
  silently reported as `completed`. This is the discriminator a resume host needs
  to continue a pending turn rather than replay a finished run.
`harn provider tool-probe` accepts `--repeat` for live provider reliability checks and reports repeated summaries conservatively.

### Fixed

- **`files_written` on the fan-out path.** A sub-agent's edits are no longer
  dropped from its receipt when the agent loop dispatches the turn's tool calls
  through `parallel` / `parallel settle`. Those subtasks now run under an
  isolated copy of the spawning agent's full ambient scope (session, execution
  context, mutation session, policies), so a dispatched tool's write attributes
  to the agent's session even while a fan-out worker's scope is swapped out.
  Previously the receipt reported `files_written: []` for a child that really
  did edit files, which a downstream host renders as "wrote 0 file(s)" /
  "0/N units completed" and can trigger wasteful parent re-work.
- **Host-side edits now feed `files_written`.** Added the
  `agent_session_record_changed_path(path, session_id?)` builtin so a product
  host whose edit/write funnel goes through workspace capabilities (rather than
  the hostlib write chokepoint `auto_capture_for_write`) can report the path it
  mutated into the active agent session's changed-path set. Without this, a
  sub-agent's edits performed via the host write path never reached the set the
  receipt drains, so its `files_written` came back empty — surfaced downstream
  as "wrote 0 file(s)" / "0/N units completed" for a child that really did edit.
Split concatenated streamed OpenAI-compatible tool-call argument objects into separate
tool calls instead of surfacing a parse error.

## v0.8.158

### Fixed

- **Background command output.** Running command handles now expose their live
  output artifacts before completion, so hosts can poll or read in-progress
  build and test diagnostics instead of spinning blind.

## v0.8.157

### Added

- **Session bundles now carry run observability records explicitly (#3684).**
  Verification outcomes and transcript pointers travel in the replay envelope
  even when consumers import from bundle-level replay data.
- Added direct session-bundle replay verification outcomes so verify results
  survive portable replay/import without requiring the full observability
  snapshot.
Session bundles now carry embedded suspended worker snapshots and materialize them
on import, so portable checkpoint bundles no longer depend on source-host snapshot
paths.
Added a site-wide pre-release notice (banner above the navbar on every page, plus a README callout) that links to the GitHub release notes so readers know the language, standard library, and CLI may change between releases.
Added a capability-matrix consistency gate that forbids any route from pinning
the provider-native tool channel for a model family whose native channel is
unreliable as a weight-intrinsic property (starting with GLM-5.x, which leaks
`<tool_call>` markup into assistant content instead of returning native tool
calls). Fixed the NVIDIA GLM-5 route — the lone outlier that pinned `native`
while the rest of the family uses the clean text channel — so a value model can
no longer silently thrash on a single mis-pinned provider.
- **Session bundle import reports (#3682).** `harn session import --json` now
  prints the imported run-record path, materialized worker snapshot paths, and
  argv-form `harn run --resume` commands for portable checkpoint restore
  workflows.

### Changed

- **Pre-push hooks can now run in fast-only mode.** Set
  `HARN_PREPUSH_FAST_ONLY=1` to keep signature, merge-queue, and cheap drift
  guards while deferring expensive local build checks to remote CI.

### Removed

- Removed stale Anthropic Opus 4.6 fast-mode catalog metadata after Anthropic's June 29, 2026 removal date;
  Harn no longer advertises `speed = "fast"` for `claude-opus-4-6`.

### Fixed

- Preserve sub-agent workspace anchors in persisted worker snapshots so
  suspended workers retain placement boundaries after cold restore.
Fixed fan-out child `files_written` attribution so each child agent's own session breadcrumb
survives awaits without inheriting the parent session.
- **OpenTelemetry tracing exporters now use one compatible dependency family
  (#3711).** The OTLP setup paths no longer mix Harn's direct `reqwest`
  version with the HTTP client expected by the upgraded OpenTelemetry stack.
Fixed local pre-push coverage for session-bundle schema drift.
Arg/path-scoped dynamic-permission denials are now coached as recoverable
instead of terminal: when a route is permitted but a specific argument value or
path is out of policy, the agent is told to retry with an allowed value (the
same treatment the argument allow-list gate already gets, harn#3670). Hard
capability ceilings, approval-unavailable, and explicit user rejections stay
terminal — a retry there yields the same answer.
- **Model capability matrix.** GLM-5 family routes now fail the capability
  audit if a provider pins the unreliable native tool channel, and the NVIDIA
  GLM-5 row is corrected to the text tool format (#3729).
Surface real `harn.toml` read errors (permission denied, bad symlink) instead of silently falling back to default config, and make `ed25519_verify` throw on malformed public-key input (wrong length / non-hex) instead of reporting it as a failed signature.
- Standalone `host_call("project.metadata_*", ...)` now routes metadata get,
  inspect, set, save, stale, and refresh calls to Harn's built-in metadata
  store when no host bridge handles the call, and `harn check` recognizes the
  inspect operation by default. This preserves project metadata learning in
  CLI/debug runs.
- **Orchestrator watch-mode reload coverage is now deterministic.** The admin
  reload test no longer races the manifest file-watch debounce.

## v0.8.156

### Added

- Added lifecycle hooks to `std/edit`'s `edit_safe_text_patch` commit path so
  hosts can reject, rewrite, advise, and invalidate rollback state without
  reimplementing the edit primitive.

### Fixed

- **Agent loop truncation recovery.** Length-truncated turns that contain only
  hidden reasoning now auto-continue with a raised output cap instead of
  flowing into empty-turn parse or stall handling (#3645).
- **Flake detection CI now fails loudly on broken nextest filter discovery
  (#3699).** The touched-path mapper is portable to Bash 3, ignores non-Rust
  paths itself, and scheduled reruns are bounded by a 180-minute job timeout.
- **Harmony text tool calls.** Recover leaked
  `tool_call to=... code<|message|>{...}` text-format tool calls instead of
  rejecting the response as stray text.
- **Harn CLI Windows paths.** Windows drive-letter paths are now treated as
  filesystem paths instead of URL schemes in package registry, archive, and
  connector/OpenAPI flows, and CLI JSON/TOML artifacts normalize path output
  deterministically across platforms.
- **Release protocol artifacts.** Release preparation now regenerates protocol
  artifacts with a post-bump Harn binary so their artifact version matches the
  new workspace version.
- **Worker lifecycle metadata now exposes millisecond timing fields.** Worker
  summaries and `worker_update` bridge events include decoded `created_at_ms`,
  `started_at_ms`, `finished_at_ms`, and `wall_ms` values instead of forcing
  clients to parse UUIDv7 timestamp IDs themselves.

## v0.8.155

### Added

Added `agent_session_inject_reminder(session_id, options)` — inject a single typed
system reminder directly into a live session's transcript event stream,
bridge-free. The in-process sibling of the
`push_bridge_injection`/`drain_bridge_injections` reminder path, for hosts that
drive the agent loop without an ACP `HostBridge`. `options` mirrors
`transcript.inject_reminder` (`body` required; optional `tags`, `dedupe_key`,
`ttl_turns`, `preserve_on_compact`, `propagate`, `role_hint`); the loop's
existing `apply_reminder_post_turn` pass evicts the reminder once its `ttl_turns`
reaches zero.
Sub-agent result envelopes (and therefore `agent_fanout` per-child results) now
carry `files_written` — the authoritative set of workspace paths the child
actually mutated through the deterministic hostlib write surface, collected at
the single `fs_snapshot::auto_capture_for_write` chokepoint so capability-denied
out-of-scope writes are excluded — plus a `usage` object
(`input_tokens`/`output_tokens`/`total_tokens`). This lets a fan-out parent
attribute writes to each child and detect a child that claimed completion but
wrote nothing or wrote outside its scope, without re-parsing transcripts, and
works headless. Exposed via new
`harn_vm::agent_sessions::{record,session,take,clear}_session_changed_path(s)`
helpers fed from the hostlib write chokepoint.

### Fixed

- `std/agent/host_tools` command policies now accept `require_approval` verdicts
  and return structured approval requests instead of throwing or executing.
Anthropic requests now drop whitespace-only text blocks at provider egress,
avoiding strict validator 400s without changing stored transcripts.
OpenAI-compatible chat-completions handling now avoids strict-provider failures by
omitting invalid request combinations, dropping orphaned native tool results, and
splitting concatenated JSON tool-argument objects into dispatchable calls.
- **Corrected the DeepInfra GLM-5.2 catalog pricing to its published rate.**
  The `deepinfra/zai-org/GLM-5.2` row carried `1.40/4.40/0.26` — the
  together/baseten placeholder copied onto the sibling DeepInfra row, not
  DeepInfra's own rate. DeepInfra's published GLM-5.2 pricing, verified against
  their pricing API and reconciled exactly against a live `chat/completions`
  `estimated_cost` (and corroborated by OpenRouter's `z-ai/glm-5.2` listing),
  is `$0.95` in / `$3.00` out / `$0.18` cached read per MTok. Catalog data only,
  no behavior change; this fixes cost accounting for the DeepInfra GLM-5.2 route.
- **Runtime errors now name the source file, not just the line.** Error
  enrichment appended a bare `(line N)` drawn from the innermost stack frame,
  which is ambiguous across 100+ stdlib `.harn` files and forced a manual hunt
  to locate a crash. When the frame carries a source path the suffix is now
  `(<file>:N)` (e.g. `(stall.harn:497)`); the bare `(line N)` form is kept
  as the fallback when no path is known.
- **Failure-evidence snippets no longer amputate the tail.** `agent/stall`'s
  diagnostic snippet and `agent/sitrep`'s message truncation both did a
  head-only clip, silently dropping the end of the text — which for tool and
  command output is usually where the decisive error lives (failing assertion,
  last compiler error). Both now preserve a generous head **and** tail with an
  explicit `…[N chars elided]…` marker, so the model never loses the part of a
  tool-call error that pinpoints the fix.
- Fixed two SEV-1 fan-out concurrency cross-wires: the per-worker VM execution
  context (cwd/env/source-dir + capability path-scope root) and mutation session
  (audit/run_id/approval/secret-scope) are now captured into
  `AmbientExecutionScope` and swapped per-poll, so cooperatively-scheduled
  `spawn_local` children no longer read a sibling's worktree root, environment,
  or audit/secret attribution across an `.await`. A drift guard now fails CI if a
  new ambient-shape thread-local is added without classifying it.
- **`agent_fanout` no longer loses a whole batch when one child fails to spawn.**
  A background `sub_agent_run` validates and builds its request synchronously
  (e.g. parsing `allowed_tools`) and can also hit a host worker-spawn fault, so
  it can `throw` before any handle exists. The per-wave spawn loop had no
  per-child guard, so a single malformed unit aborted the entire wave *and*
  every later wave — silently dropping every sibling result. Each spawn is now
  caught individually: a spawn-time throw becomes that unit's own `ok: false`
  result (`status: "failed"`, the fault in `error`) at its correct offset, the
  surviving children are still joined, and results stay 1:1 and positionally
  aligned with `requests`. One bad unit can no longer nuke a parallel fan-out.
OpenAI-compatible providers now strip transcript-only and provider-private
message fields before sending chat completions requests, avoiding strict-provider
rejections of stored reasoning or cache metadata.
OpenAI-compatible requests now clamp emitted temperature and top_p values to
provider-safe ranges before send.
OpenAI-compatible provider errors now surface numeric codes, request ids, and
upstream failure tails from varied provider error envelopes.
- **Strings now support `.slice(start, end?)` as an alias for `.substring`.**
  `slice` was a list-only method, so calling it on a string raised a runtime
  `string has no method \`slice\`` that aborted the whole agent loop. Harness
  authors (and JS/Python muscle memory) reach for `.slice` on strings
  constantly; it is now char-based and negative-index aware, mirroring
  `list.slice` exactly, which structurally removes the crash class rather than
  chasing individual call sites. This was crashing `agent/stall`'s failure-
  snippet path whenever a failing tool result carried a >240-char error body
  (e.g. a long compiler error), terminating the loop mid-fix.

## v0.8.154

### Breaking

- **`harn model-info` is folded into `harn models info`.** The standalone
  top-level `model-info` command is removed; the same model-metadata lookup now
  lives under the `models` noun (`harn models info <model> [--verify] [--warm]`),
  matching the `provider`/`skill` consolidations. No alias is kept.

### Fixed

- Give each spawned agent/worker task its own ambient execution scope. Capability
  context (execution/approval/command/autonomy policy, dynamic permissions, the
  bridge-trust + command-hook depths, the runtime-context overlay, the LLM
  render context, and the active connector context) lived in thread-local LIFO
  stacks whose guards were held across `.await`. Because workers run interleaved
  on a `spawn_local` LocalSet, a child reading its policy after an await could
  observe a *sibling's* top-of-stack — cross-wiring per-child file scoping, tool
  ceilings, approval, autonomy tier, template render context, and event
  attribution between concurrent fan-out children. Each worker future now swaps
  its own scope in and out around every poll (the `tracing::Instrument`
  technique), so only the currently-polling task's context is ever live on a
  thread. Correct under both cooperative and work-stealing multi-thread runtimes;
  O(1) per poll.
- Coach a retry instead of a give-up when a tool call is refused by the argument
  allow-list. A path/command outside an agent's scoped `tool_arg_constraints`
  (e.g. a fan-out child scoped to `test/users.*` that tried to edit the shared
  reference file) is now a SOFT, retryable denial: the model is told its allowed
  pattern(s) and to re-issue with a matching argument — and to read reference
  files with `look` rather than editing them — rather than reading the terminal
  "permission denied / do not retry" body and abandoning the turn. Hard
  capability/side-effect/tool ceilings stay terminal.

## v0.8.153

### Breaking

- **CLI naming cleanup.** The `harn trust-graph` alias is removed — use
  `harn trust` (its canonical name). The shell-completion command is renamed
  from `harn completions <shell>` to `harn completion <shell>` for consistency
  with the rest of the CLI. No aliases are kept; both are direct cutovers.
- **Provider commands are consolidated under a single `harn provider` noun.**
  The six top-level commands `providers`, `provider`, `provider-catalog`,
  `provider-ready`, `provider-probe`, and `provider-tool-probe` are gone.
  Use `harn provider capabilities`, `harn provider catalog <refresh|validate|
  build-config|build-capabilities|export|matrix|support|recommend|show>`
  (`show` prints the loaded catalog JSON, formerly `provider-catalog`),
  `harn provider ready`, `harn provider probe`, and `harn provider tool-probe`.
- **Skill commands are unified under a single `harn skill` noun.** The
  separate `harn skills` (corpus discovery) command is gone; its verbs now live
  alongside the provenance verbs under `harn skill`: `list`, `get`, `dump`,
  `resolved`, `inspect`, `match`, `install`, `new`, plus `key`, `sign`,
  `endorse`, `verify`, `who-signed`, and `trust`. No aliases — direct cutover.
- **Module exports now require explicit `pub`.** A module's import surface is exactly the functions it marks
  `pub` (plus `pub import` re-exports); a module with no `pub` functions exports nothing. The previous
  "a module with no `pub` exports everything" fallback is removed — it made adding the first `pub` a silent
  breaking change for a module's importers. Both `harn check` (`HARN-IMP-002`) and the runtime loader enforce
  the rule. To migrate, mark intended exports `pub`; the diagnostic points at any selective import that needs it.

### Added

Added `agent_fanout(requests, options)` to `std/agent/workers` — a general parallel sub-agent fan-out primitive. It
maps a list of independent units onto concurrent background `sub_agent_run` children in bounded waves (`max_parallel`,
default 8), joins them, and returns one normalized `{label, index, status, ok, result, error}` per request in input
order. Composes the existing worker primitives (no new host surface); the caller owns each child's tool surface,
capability policy, model, and prompt via per-request `options`. Two integration tests lock the contract:
`worker_overlap` proves the children's LLM turns overlap in wall-clock time (A/B serial-vs-concurrent), and
`agent_fanout` proves order/label preservation, per-child isolation, ok/error normalization, and wave chunking. See
`docs/src/agent-pools.md` (“Parallel sub-agent fan-out”).
- Windows PowerShell installer: `irm https://harnlang.com/install.ps1 | iex` downloads, checksum-verifies, and
  installs the Windows release archive and adds the install directory to the user PATH. The POSIX `install.sh`
  now points Windows shells at it instead of erroring.

### Changed

- Bumped `wasmtime` / `wasmtime-wasi` 45 → 46 (testbench WASI sandbox
  backend) and `tower-http` 0.6 → 0.7 (`harn serve` compression + CORS
  layers). No behavior change; both upgrades are API-compatible with our
  usage.
- Genericized documentation, spec, persona, and example references to specific
  downstream products into role terms (an IDE host, a cloud platform), so the
  public repo describes integration surfaces rather than naming closed products.
  Company attribution and `burin-labs` GitHub URLs/release links are unchanged.
- The minimum supported Rust version (MSRV) is now declared as `rust-version = "1.95"` across the workspace, so
  `cargo install harn-cli` on an older toolchain fails with a clear message instead of a confusing build error.
- The default package index now resolves from `https://packages.harnlang.com/harn-package-index.toml`, and
  `harn publish` opens its index PR against the public `burin-labs/harn-packages` repo (previously the index
  was served from a private repo's GitHub Pages). Override per-command with `--registry` / `HARN_PACKAGE_REGISTRY`
  for resolution, or `--index-repo` / `--index-path` for publishing.
- When no provider is configured (`HARN_DEFAULT_PROVIDER` and `default_provider` both unset), Harn now
  auto-selects a provider instead of silently assuming Anthropic: it prefers a configured cloud provider whose
  API key is present, then a local/auth-free provider (Ollama, `harn local`), and only then falls back to the
  documented Anthropic default — warning once (with how to configure one) so non-Anthropic adopters get a clear
  nudge rather than a raw auth failure. Detection reads catalog `auth_env`/`local_runtime` metadata only, with
  no hardcoded paths or ports.
- The provider/model catalog now refreshes from `https://harnlang.com/provider-catalog/provider-catalog.json`
  (served from Harn's own site) instead of a private repo's GitHub Pages. The catalog bundled in the binary
  remains the offline default; set `HARN_PROVIDER_CATALOG_URL` to point at a different catalog.

### Fixed

- Let narrowed sub-agent fan-out workers stat paths and carry their real session
  id: subsume `workspace.exists` under a `read_text`/`list` grant (so `look`,
  `read_file`, and edit preflight stop hard-failing under a tool-derived child
  policy), and stamp a sub-agent's audit with its real `sub_agent_session_*` id
  instead of a fresh random one that joins to nothing.
- **Provider catalog corrections from the cross-provider footgun pre-mortem (harn#3645 §4).**
  Added two reliable Together serverless sample routes
  (`Qwen/Qwen2.5-7B-Instruct-Turbo`, `meta-llama/Llama-3.3-70B-Instruct-Turbo`)
  to replace representative routes that were unusable as one-click samples
  (`Qwen/Qwen3-Coder-Next-FP8` is dedicated-only; the Together Gemma route is a
  reasoning model with an empty-content footgun). Marked the dead
  `MiniMax-Text-01` route deprecated (HTTP 500 / absent from
  `GET /v1/models`; use `MiniMax-M2`). Added three live-catalog contract tests
  that guard the wire-id conventions the audit relied on: no `wire_model` retains
  its own `<provider>/` route prefix, no DeepInfra wire id regains the
  `deepinfra/` prefix, and `nvidia/minimax-m2.7` still dispatches the NIM wire id
  `minimaxai/minimax-m2.7` (#3645).
- Normalize single-tool-call provider history so Fireworks GPT-OSS runs do not
  replay parallel native tool calls into a provider-side 400.
- The `curl -fsSL https://harnlang.com/install.sh | sh` one-liner is now published by the docs-site build,
  which previously returned 404 because `install.sh` was never copied into the rendered site.
MCP list-change notifications now refresh manifest-derived prompt state through
the same in-process path used by the file watcher, making prompt and package
metadata updates deterministic under load.

### Security

- The default sandbox `NetworkPolicy` is now deny-all instead of unrestricted: a `SandboxSpec` constructed
  without an explicit network policy gets no egress, so embedders are secure-by-default and must opt into network
  access with a host allowlist (or `NetworkPolicy::Unrestricted`). The wire variants are unchanged, and the
  `harn-serve` permission lowering already denied egress for an empty allowlist; this aligns the type's default
  with that posture.

## v0.8.152

### Added

- `harn demo` scenarios can now ship sibling files — `.harn.prompt` templates,
  imported modules, and fixtures — alongside `scenario.harn`. The runner
  materializes the whole scenario directory into a tempdir and executes it
  hermetically, so a demo is laid out like a real Harn project. The bundled
  `review-captain` demo now renders its prompts from `review.harn.prompt` /
  `clarification.harn.prompt` via `render_prompt`.

### Fixed

- Classify transient transport failures during escalated agent turns as explicit
  provider aborts instead of budget circuit-breaker exhaustion.
- Keep runtime feedback adjacent-safe for strict OpenAI-compatible Moonshot and MiniMax tool-call routes.

## v0.8.151

### Changed

- **Protocol artifacts now publish Harn's complete ACP handled-method surface
  (#3642).** The generated manifest and Rust binding include
  `session/set_budget` transport control plus the `harn.session_timeline.*`
  extension methods, with drift guards tied to the live adapter code paths.

### Fixed

- **Anthropic provider requests.** Runtime feedback injected between an
  assistant `tool_use` and its matching `tool_result` is now deferred until
  after the result before sending Anthropic Messages API requests, avoiding
  non-retryable 400 responses on strict tool-result adjacency validation.

## v0.8.150

### Fixed

- Normalize `harn pack` logical SBOM and skipped-asset paths to `/` separators
  across platforms, make Windows nightly path/process tests platform-aware, and
  remove the local sandbox's dependency on GNU `timeout` for macOS nightly runs.

## v0.8.149

### Fixed

- Make the Qwen3.6 llama.cpp local profile choose context length from host
  memory facts instead of a fixed 65k cap, and remove the stale hybrid-cache
  reprocess gate.
- Strip unsupported thinking options when agent loops switch provider routes,
  so OpenRouter Claude/Sonnet escalations do not inherit Anthropic-native
  reasoning options the target route rejects.
- **LLM stream transport fallback.** Agent LLM calls that hit a mid-stream
  response-body/read failure now retry once through non-streaming
  request/response transport when the selected route does not require
  streaming, preventing provider stream glitches from masquerading as agent
  convergence failures.
- **`std/agent`: no-net-progress stall detection now has a hard failing-verify
  floor.** When `stall_diagnostics.no_net_progress_extend_guard` is enabled,
  Harn counts failing verification turns across changing diagnostic signatures
  and successful edit calls, resets only on a clean verification pass, and emits
  a terminal `no_net_progress_hard_cap` stuck warning once the hard cap is
  crossed. This prevents slow-draining red-build loops from evading the existing
  same-diagnostic no-net-progress guard.

## v0.8.148

### Added

- Added a default-OFF reserved-budget terminal-verify guard
  (`stall_diagnostics.reserved_terminal_verify`): when an agent loop would
  terminate on a budget / token / wall-clock boundary with an unverified source
  write, it spends a small held-back iteration reserve on a final
  verify(+bounded repair) instead of ending blind on a red build. Closes the
  budget-exit half of declared-done-with-red-build (the loop-cap half landed in
  the previous release). A new `reserved_terminal_verify` agent event surfaces
  the guard's grant / verify_passed / verify_failed phases.
- Added a Harn Flow `flow_evaluate_invariants` builtin for executing Harn
  `@invariant` predicates against a slice, including harn-canon-style result
  adaptation and semantic-predicate skip reporting.

### Fixed

- **MCP and local runtime edge cases.** MCP connects now reject unsupported
  protocol versions locally and tolerate padded version strings, bytecode cache
  writes avoid same-process temp-file collisions, and `harn local launch`
  deduplicates default flags supplied as `--flag=value`.
- Agent loops now enforce post-edit verification after a live failure
  independently of repair-aware nudges, including edits made on the final
  iteration.
- Fixed macOS process TMPDIR assertions to handle `/var` and `/private/var`
  path aliases.
- Ensure `agent_loop` runs configured completion verification when
  `stop_after_successful_tools` ends a turn.

## v0.8.147

### Added

- Added `std/agent/pattern_knowledge` for cross-session pattern recall and
  promotion over durable Harn memory.

### Changed

- Added MCP `2025-06-18` compatibility, `/.well-known/mcp.json` discovery and
  publication, and stricter OAuth refresh-token rotation handling that clears
  stored MCP OAuth state on terminal `invalid_grant` while keeping token
  endpoint bodies redacted.
- Memoized the per-file read+import-scan and `canonicalize` work inside the
  bytecode cache's transitive import-graph hash (`CacheKey::from_source`). A cold
  `harn run` over a large pipeline calls `from_source` once per module load --
  the Burin 286-file pipeline does ~175 of them -- and each call previously
  re-read, re-scanned, and re-`realpath`'d every shared library file on the import
  graph. The walk now reads and canonicalizes each file at most once per stat
  identity, so the import-graph hash drops from ~3.6s to ~0.4s on a warm process
  and the whole pre-execution module-load phase falls from ~10s to ~0.6s
  steady-state (~2x faster even on a single-shot cold process). The memo is keyed
  by `(path, len, mtime_ns)`, so any on-disk edit busts it and a long-lived warm
  process still recompiles edited pipelines correctly; the folded hash bytes are
  byte-identical to the un-memoized path, so cache keys are unchanged.

### Fixed

- Bootstrapped MCP tools are now admitted into a non-empty agent policy tool
  ceiling when `agent_mcp_bootstrap_if_needed` adds them to the tool catalog, so
  an agent that can see a runtime-discovered MCP tool can also dispatch it.
  Empty `policy.tools` lists still mean "no ceiling" and remain open.
- **Changelog-fragment gate no longer treats nested source files as pip
  manifests.** The dependency-metadata allowlist matched `requirements.*\.txt` /
  `constraints.*\.txt`, where `.*` crossed `/`, so a non-dependency path such as
  `crates/.../requirements_helpers/seed.txt` matched and could slip a
  source-only change past the changelog gate without a fragment. Tightened both
  to a single path segment (`requirements[^/]*\.txt`, `constraints[^/]*\.txt`);
  genuine `requirements*.txt` / `constraints*.txt` manifests are unaffected.
- **`i64::MIN / -1` now promotes to float instead of silently wrapping.**
  Integer division wrapped this lone overflow back to `i64::MIN` -- a wrong sign
  and magnitude -- while `+`/`-`/`*`/negation already promote to float on
  overflow (the true value is `i64::MAX + 1`). The VM now promotes it, and the
  native code generator deopts to the VM for it (like the other overflowing ops)
  so the interpreter and JIT stay in agreement. `i64::MIN % -1` is `0` and is
  unchanged; division by zero still traps.
- **`harn fmt` keeps parentheses around a nested range.** A range (`a to b`)
  binds looser than ternary, comparison, and another range, so it can only
  appear bare at the top of an expression. The formatter dropped the parens when
  a range was nested as a ternary condition/branch, a binary operand, or another
  range's bound, producing output that either failed to re-parse
  (`c ? (a to b) : d` -> `c ? a to b : d`) or silently changed meaning
  (`(a to b) < c` -> `a to b < c`, i.e. `a to (b < c)`). Such ranges are now
  parenthesized at those sites.
- **Release packaging guard works on BSD/macOS grep again.** The
  `verify_crate_packages.sh` checks that fail when a packaged `harn-stdlib` /
  `harn-modules` crate still contains workspace-relative consumer includes used
  GNU-only BRE alternation (`\(vm\|modules\)`). On BSD/macOS grep `\|` matches
  literally, so the guards silently never matched and passed even on a broken
  package -- a no-op on the primary dev platform where `release_gate.sh` is run.
  Switched both to portable ERE (`grep -RE '(vm|modules)'` /
  `'(vm|stdlib)'`); behavior on GNU grep is unchanged.
- **`harn local launch` no longer duplicates flags that a runtime's
  `default_args` and a dedicated CLI flag both supply.** The llama.cpp runtime
  ships `default_args = [--jinja, --reasoning off, --reasoning-format deepseek,
  --metrics, --flash-attn on]`; passing the matching dedicated flags
  (`--jinja`, `--flash-attn on`, ...) appended each one a second time, so the
  launched argv carried `--jinja ... --jinja` and `--flash-attn on ...
  --flash-attn on`. The builder now folds in only the `default_args` entries the
  caller did not override, and the explicit value wins (e.g. `--flash-attn auto`
  replaces the default `on`). Harmless to llama.cpp, but it made the persisted
  launch record and logs misleading; deduped output is now exact.
- **Sandboxed builds get a writable, workspace-local `TMPDIR`.** Compiler
  linkers (`rustc`/`cc`/`ld`, Go, Swift, ...) and other toolchains write
  intermediate object/temp files to `$TMPDIR`, defaulting to the system `/tmp`
  when it is unset -- which is outside the sandbox's writable workspace roots, so
  those writes were denied and a build that should pass FALSE-FAILED with
  `could not write output to /tmp/rustcXXXX/...: Cannot create temporary file in
  /tmp/: Permission denied`. The process command-runner (both the
  `host_call("process", ...)` exec/spawn path and the `process.exec`/`shell`
  builtins) now points a sandboxed child's `TMPDIR`/`TMP`/`TEMP` at a lazily
  created `.harn-tmp/` inside the first writable workspace root, which the OS
  sandbox already grants. This fixes any TMPDIR-honoring toolchain without
  widening the sandbox; a `TMPDIR` the caller sets explicitly is respected, and
  the temp dir self-`.gitignore`s so its churn never leaks into a diff or eval
  grading.
- **Linux sandbox no longer denies `socketpair` below the network ceiling.** The
  seccomp blocklist conflated the anonymous, unaddressable local-IPC `socketpair`
  with egress sockets, so `cargo build`/`cargo test` could not even spawn `rustc`
  (Cargo's jobserver is `socketpair`-backed) -- it failed with `(never executed)`
  / `Operation not permitted`. `socketpair` is now allowed while
  `socket`/`connect`/`bind`/`listen`/`accept` stay denied, so local IPC works
  without opening any egress path. The `send*`/`recv*` family the jobserver drives
  that pair with is un-denied in the companion fix below.
- **Linux sandbox no longer denies the `send*`/`recv*` family below the network
  ceiling, so Cargo's socketpair-backed jobserver works.** Un-denying
  `socketpair` was necessary but not sufficient: Cargo acquires/releases build
  tokens over that pair with `recvfrom`/`sendto`, and those stayed seccomp-denied
  so the parent's token read returned `EPERM`, surfacing as a worker-thread
  `the CLOEXEC pipe failed: Operation not permitted` panic that aborted
  `cargo build`/`cargo test` before any `rustc`/link step. `recvfrom`, `recvmsg`,
  `sendmsg`, and `sendto` are now allowed below the network ceiling. They open no
  egress: with `socket`/`connect`/`bind`/`listen`/`accept` still denied, a
  sandboxed process can hold only anonymous `socketpair` pairs and pipes, so the
  send/recv family can only drive local IPC. Reproduced and verified under the
  exact seccomp filter -- a trivial `cargo build` panics with the family denied
  and completes with it allowed, while `socket(AF_INET)` and `connect` stay
  `EPERM`.

## v0.8.146

### Fixed

- Narrowed the same-turn dependent-edit skip guard so a later edit to the same
  file is only skipped when the earlier failure could actually have mutated the
  file. The dominant intra-turn failure — a pre-apply rejection (`old_string not
  found`) — leaves the file byte-identical, so a sibling edit is no longer stale
  and now runs, eliminating a wasted recovery round-trip. Post-apply diagnostics
  failures, opaque errors, and non-edit mutating tools still poison the resource
  (safety preserved). (#3611)

## v0.8.145

### Added

- Add `code_index.repo_map`, a read-only hostlib query that ranks typed symbol
  definitions with personalized PageRank and returns a prompt-budgeted
  structural map for agent grounding.
- `code_index.rename_symbol` now accepts an optional `replacement_text` field.
  When supplied, the builtin overwrites every true-identifier occurrence of the
  symbol (still skipping strings and comments) with arbitrary text instead of
  renaming it, turning the one-shot atomic, syntax-validated, all-or-nothing
  cross-file primitive into a symbol-grounded find/replace.

### Changed

- Generalized internal documentation comments to vendor-neutral language,
  removing references to a specific downstream host's private Swift source files
  and product name. No behavior, wire identifiers, or fixtures changed.
- Make the canonical reasoning effort -> token budget mapping the single source
  of truth. A new `llm_reasoning_effort_budget(level)` builtin exposes the
  mapping, and the structured-floor fallback now delegates to it instead of
  duplicating the constants.

### Fixed

- Preserve pipeline-authored `loop_stuck` event payloads emitted through `agent_emit_event`.
- **Provider catalog Gemma 4 capability tags now stay consistent (#3585).** The
  local Gemma 4 rows now declare the same tools, vision, thinking, and
  structured-output tags that Harn derives from structured capabilities.
- **`std/agent`: the adaptive loop-control extension policy is now outcome-aware,
  so a thrash can no longer extend its own iteration budget without bound.**
  `agent_loop_is_progressing` keyed "progress" on `progress.changed`, an
  *activity* signal that fires merely because the model issued tool calls — so a
  run thrashing through the same compile/test failure every turn (lots of
  edits/verifies, no error draining toward a green build) read as "progressing"
  every turn and kept hitting the `extend` rule at the budget boundary (observed
  24→32→…→72 on a run that should have been cut). A new default-OFF guard
  (`stall_diagnostics.no_net_progress_extend_guard`, after
  `no_net_progress_extend_after` turns, default 3) sets `progress.no_net_advance`
  when the verify-bearing failing path is *not advancing* — the same failure
  signature has recurred past threshold (the stall detector's
  `same_diagnostic_streak`) — and `agent_loop_is_progressing` then declines to
  extend. Outcome-aware and conservative: a productive edit changes the error
  signature (streak resets → still progress), a passing test clears the failure
  model (→ still progress), and read-only/explore turns with no failing
  verification never arm it, so genuinely-advancing and exploring runs keep their
  budget. New pub `agent_stall_no_net_progress`. Pairs with Burin's product-side
  no-net-progress ripcord: the loop now neither extends a thrash nor lets it run
  unaided.
- `std/llm/safe`: the structured-output **repair** side-call now floors its
  output budget reasoning-awarely (`repair_min_max_tokens`), the twin fix to the
  #3598 judge floor. A flat 600-token repair budget was billed against the same
  `max_tokens` as a reasoning route's hidden analysis channel, truncating the
  repaired JSON verdict to empty (a silent dead-repair) on reasoning models such
  as gpt-oss. Non-reasoning routes are unchanged (600 baseline); reasoning routes
  are raised to `reasoning_budget + verdict headroom`. Applied in
  `safe_structured_call`'s judge defaults and the `with_repair` caller-seam
  handler, reusing the same floor as the judge path so the two never drift.
- `load_skill` and `skill_who_signed` now resolve a bare-name collision
  deterministically by `source` layer precedence (project > user > host,
  matching the `Layer` priority table) instead of failing the turn with an
  "ambiguous" error.
- `std/lifecycle/pool`: terminal pipeline-scope pool tasks are now durably
  appended before waiters can observe completion, closing a restart race where a
  fast `pool_wait` could see `completed` in memory but reload the older
  `running` record after `pool_simulate_restart`.

## v0.8.144

### Fixed

Make the minimum output budget for structured side-calls (completion judge, verifier, router, classifier) reasoning-aware. The flat `STRUCTURED_MIN_MAX_TOKENS = 512` floor was billed against the same `max_tokens` budget as a reasoning route's hidden analysis channel, so on a reasoning model (e.g. gpt-oss at `low` effort, ~1024 reasoning tokens) the thinking consumed the whole budget and the JSON verdict truncated to empty — a silent dead-judge abstention. New `structured_min_max_tokens` resolves the route's reasoning budget and reserves verdict headroom on top (clamped to 32768), only ever raising the floor; non-reasoning routes keep the flat 512.

## v0.8.143

### Added

- Add a parser-agreement contract test (`harn-hostlib` `parser_agreement_corpus`) that drives a small checked-in polyglot fixture corpus through the bundled tree-sitter `extract_imports` / `parse_errors` facts and fails CI when those facts diverge from declared ground truth — so a grammar-bump regression that mis-lexes valid source into phantom facts is caught before it can ship to an agent. Seeded with the zig `\\` multiline-string mislex.
- The native scanner now extracts declared package-manifest dependencies for the project root and each detected sub-project, surfaced as `project.available_dependencies` and `sub_projects[].dependencies` in the `scanner.scan_project` / `scanner.scan_incremental` response. Coverage spans 14 ecosystems (npm, Composer, PyPI/Poetry, Cargo, Go modules, RubyGems, Dart pub, sbt, Gradle Groovy/Kotlin, Maven, SwiftPM, Mix, MSBuild/`.csproj`), porting Burin Code's Swift `ManifestDependencyParser` into cross-platform Rust so the fact is computed once in `harn-hostlib`.

### Fixed

Declare vision on the local Gemma 4 capability route so its emitted `capability_tags` and structured caps agree with the model's real multimodality and its hosted siblings (gemini/openrouter/together/nvidia). Previously the local OpenAI-compat `gemma-4*` rule dropped `vision`, giving hosts an inconsistent, route-dependent capability view.
Stop a malformed `\x`/`\u` escape from dropping a whole tool call in the `name({ key: "value" })`
text channel. A double/single-quoted string value containing Perl/PCRE `\x{1F600}`, a Windows path
`C:\users\me`, a trailing `\x`, a short `\uAB`, or `\uABCG` previously errored with `invalid \x/\u
escape` and the entire `edit`/`run` call was discarded — the model authored nothing and thrashed
(the same class as the surrogate-pair drop, generalized to every other malformed-known-escape
shape). The parser now degrades any escape that is not a complete `\xHH` / `\uHHHH` / `\u{...}` to
its literal `\x`/`\u` bytes, byte-identical to the heredoc and template-literal channels, so the
call still dispatches with the model's content. A complete-but-invalid escape (an unpaired
surrogate) is still rejected, and well-formed `\xHH`/`\u{...}` escapes still decode.
Agent loop: the depth>=4 no-progress nudge no longer accuses a tool-active model of being "stuck in a narration loop." When the model is issuing real tool calls but the loop's progress signal has not advanced, the nudge now steers it to re-read the failing output and target a different location instead of telling it to stop narrating.

## v0.8.142

### Fixed

- Stop a leaked tool-call heredoc wrapper from corrupting written files. When a model delivers a
  `content: <<EOF\n...\nEOF` value through a channel that never runs the heredoc grammar — a native
  JSON string `"<<EOF\n...\nEOF"`, or chat-template `<parameter=content>`/DSML markup — the `<<TAG`
  opener and closing sentinel previously leaked verbatim into the file, so the first line became a
  literal `<<EOF` (e.g. Zig: `expected type expression, found '<<'`) and the build failed. The
  dispatch normalizer (`normalize_tool_args`) now strips a value that is *entirely* one well-formed
  heredoc, recursing into nested `ops` arrays. A value that merely contains `<<` (a shift operator, a
  real mid-file `<<EOF`) or a partially-wrapping heredoc is left byte-identical.

## v0.8.141

### Fixed

- Fixed four agent-loop convergence bugs found by audit: `strip_thinking_tags` no longer corrupts
  tool-call arguments that contain `<think>`/`</think>` inside strings or heredoc bodies; completion
  and step verdict classification now reads the leading word (so `done.` and `done — all pass` are
  recognized) and drops the reflexive `yes`/`true` done-tokens; a single-message compaction archive is
  no longer discarded as a false-terminal `context_overflow`; and provider-catalog fragment
  leading-key bleed (which mislabelled `openai/gpt-oss-120b`) is fixed with a build-time validator.
- Skill discovery now recognizes the optional `targets:` `SKILL.md`
  frontmatter field (version-aware grounding) instead of flagging it as an
  unknown field and logging a warning.
- The stall detector now location-normalizes diagnostic text (strips
  `file:line:col` coordinates and bare numbers) before hashing its
  same-error signature, so the same compile/test error re-firing at a
  shifting line no longer resets the repeat streak and the
  `stuck_same_diagnostic` / hard-stop trip fires as intended.
- The step judge now marks a fail-open pass with `judge_error: true` /
  `reason: "judge_unavailable"` (surfaced on the `step_judge_decision`
  event as `judgeError`) when the judge model itself errors, so a swallowed
  judge failure is observable in telemetry instead of being
  indistinguishable from a genuine approval.

### Security

- `run`/`command_run` no longer leak secret-bearing environment variables to
  spawned child processes (and thus to the model, which reads child stdout as
  the tool result). Under the default `inherit_clean`/`patch` env modes the
  child environment now strips provider `*_API_KEY`s, `*_TOKEN`/`*_SECRET`/
  `*_KEY` variables, and explicit names like `GITHUB_TOKEN`,
  `HARN_CLOUD_API_KEY`, `BURIN_ADMIN_TOKEN`, and `AWS_SECRET_ACCESS_KEY`. Benign
  build/toolchain vars (`PATH`, `HOME`, `LANG`, `CARGO_*`, …) are preserved, and
  an explicit caller-supplied `env` is left untouched.
- The file-backed OAuth/MCP token store now writes its sealed token file with
  `0o600` permissions on Unix so a wide umask can't leave it group/world
  readable.
- Closed a filesystem write/delete symlink-swap TOCTOU in the deterministic
  `tools/{write_file, delete_file}` builtins (audit finding F5). The
  workspace-scope check canonicalizes a copy of the path, but the subsequent
  `write`/`remove_*` ran on the raw path and followed a symlink at the final
  component at op time, so an in-workspace attacker could swap a check-passed
  path for a symlink pointing outside the allowed roots and escape the
  workspace. `write_file` now opens the final component with `O_NOFOLLOW` on
  Unix (with an `lstat`-reject fallback elsewhere) so a symlink-final path is
  rejected at open time rather than followed; `delete_file` re-validates an
  escaping final-component symlink under the active policy and refuses to
  remove through it. Normal in-root writes, overwrites, and deletes of real
  files are unaffected.

## v0.8.140

### Added

- **Eval run inspection now exposes a read-only MCP debugger and VM event-log
  introspection.** `harn.eval.inspect_run` builds a chain-of-custody dossier for
  eval artifacts, and the `event_log` stdlib now supports deterministic
  `describe`, `topics`, and bounded `read` calls for debugger workflows.
- `harn test --coverage` reports per-file line coverage for the Harn source a
  user test suite executes, and `--coverage-out <path>` writes an LCOV tracefile
  consumable by Codecov, `genhtml`, and the VS Code Coverage Gutters extension.
  Coverage reuses the per-instruction source-line table the VM already carries,
  so it needs no separate instrumentation pass; recording is opt-in and adds a
  single predictable branch to the dispatch loop when no session is active.

### Changed

- **Tool-calling north-star RFC (#3576).** Documents the target tool-calling
  architecture and the staged path from today's runtime toward it.
- **Refreshed provider tool-call wire-format pins from a real-spend
  forced-format sweep (2026-06-24, N=5).** MiniMax-M2.7 (Together, SambaNova),
  Kimi-K2.7-Code (OpenRouter), and Qwen3.6-35B-A3B (DeepInfra) corrupt
  backslash-heavy file bodies on the provider-native and fenced-JSON channels
  but round-trip them byte-clean on the escape-free heredoc `text` channel;
  those four routes are now pinned `preferred_tool_format = "text"` /
  `tool_mode_parity = "native_unreliable"`. Evidence:
  `docs/eval/provider-tool-mode-sweep-2026-06-24.md`.
- **Release binary size gate now allows the v0.8.139 x86_64 Linux binary
  (#3561).** The release workflow and local size-check script now share a
  187 MiB ratchet, matching the 186.11 MiB stripped binary produced by the
  v0.8.139 release build.

### Fixed

- **Agent loop now gives actionable feedback on empty/unrecoverable tool
  arguments and fails fast on repeated same-resource failures (#3573).** When a
  tool call arrives with empty or unrecoverable arguments the loop surfaces a
  cause-naming nudge instead of silently churning, and repeated identical
  same-resource failures within a turn are cut early. Adds conformance coverage
  (`agent_loop_empty_args_cause_feedback`,
  `agent_loop_intra_turn_resource_fail_fast`,
  `agent_loop_native_text_markup_rescue`,
  `agent_loop_no_progress_feedback_modes`).
- Agent loops now fail-fast later same-resource mutating tool calls in the same
  assistant response after an earlier sibling fails, using tool annotations and
  path arguments instead of host-specific heuristics.
- Treat product-level `Error:` tool results as failures for intra-turn
  same-resource fail-fast scheduling.
- **gpt-oss (Harmony) and GLM-5.x native tool-call footguns are now closed at
  the capability layer (#3574).** DeepInfra and SambaNova `gpt-oss` and the
  zai-direct `glm-5.x` routes are pinned to the TEXT tool channel with
  `tool_mode_parity = "native_unreliable"`, so a `native` pin (alias or
  `--tool-format native`) auto-corrects to `text` with an explanatory
  `correction` instead of silently emitting an empty tool stream. The
  provider-native Harmony channel on these pay-per-token routes drops tool calls
  into the private reasoning/commentary channel (empty `tool_calls` /
  billed-noncommittal), matching the Fireworks `#3505` precedent and the same
  failure class reported on vLLM, SGLang, and the OpenAI Harmony repo.
- **New first-class "no viable tool channel" fail-fast guard
  (`capabilities::no_viable_tool_channel`).** When a `(provider, model)` route
  has neither a trusted native nor a trusted text tool channel, a tool-bearing
  `llm_call` now fails before dispatch with an actionable error naming the bad
  combo and a suggested alternative, instead of billing a noncommittal
  completion with no dispatchable tool call.
- **Native tool-channel degrade now also fires on a vanishing tool call and a
  function-call protocol refusal (#3577).** The runtime native→text tool-format
  degrade now also fires on a billed-noncommittal vanishing tool call (the
  upstream finished cleanly, billed output, and committed no tool call — the
  action stranded in a private reasoning channel) and on a native function-call
  protocol refusal (e.g. SambaNova's HTTP 400 "Model started a function call but
  did not complete it"), not only on the 5xx/EOF server-side parser choke it
  already handled. These signatures meant a native-channel route that vanished
  its tool call previously retried the same broken native channel until the
  budget drained, then surfaced; now it degrades once to the text channel and
  recovers. The degrade stays a one-way last resort: it remains gated to native
  channels, fires at most once per call, and never triggers for a
  `length`/`max_tokens` truncation (continue-on-truncation stays above
  channel-switch in the remedy order).
- **OpenAI-compatible streamed tool calls now preserve non-empty argument
  payloads.** Clean `tool_calls` finishes that stream Harn text-format or
  otherwise malformed multiline arguments no longer silently dispatch `{}`;
  complete text-format payloads are recovered and unrecoverable payloads carry
  an explicit parse error while length-truncation behavior is unchanged.
- **Text tool-call parser diagnostics now classify unclosed `<user_response>`
  blocks correctly (#3563).** An accepted opening tag without its closing
  `</user_response>` is reported as an unclosed block instead of the
  contradictory unknown-tag fallback.
- Normalize bare tool-choice names for OpenAI-compatible providers so a specific
  tool request cannot be forwarded as an invalid scalar value.
- Pinned unsafe GLM and Moonshot alias tool-format defaults to safe formats.
- Normalize marker and generic pseudo-tool wrappers with clear read/search/run
  intent before tool dispatch, reducing recoverable value-model tool-call
  denials.
- `harn test --coverage-out <path>` now always writes the LCOV tracefile, even
  when no on-disk source executed. An explicitly requested artifact is no longer
  silently skipped on an empty report (which would break a CI step that consumes
  the file); the empty report renders to a valid zero-record LCOV.
- The `harn.eval.inspect_run` dossier now reports sample-scoped event stats
  accurately. `first_sampled_id`/`last_sampled_id` and the provenance
  `chain_breaks_in_sample`/`sample_chain_ok` cover only the sampled prefix
  window instead of leaking full-scan values when a JSONL topic has more records
  than `limit`, and `agent_event_topics` is deduplicated when a topic surfaces
  from both the JSONL dir and the sqlite log.
- **The experimental native compiler (`harn-codegen`) no longer disagrees with
  the VM on integer overflow.** Integer `+`/`-`/`*`/negation now guard against
  `i64` overflow and deopt (`NativeOutcome::Deopt`) exactly where the VM
  promotes the result to `float`, instead of silently wrapping — so a
  JIT-compiled kernel is always bit-identical to the interpreter or signals a
  fall-back, never a quietly wrong answer. Adds a `tests/vm_fidelity.rs`
  differential suite that runs the same functions on the real `harn-vm`
  interpreter to prove the value/deopt/trap boundaries match.
- Avoid redacting source declarations such as `pub const Token = struct` as
  secret assignments while preserving generic token assignment redaction.

## v0.8.139

### Added

- **Provider catalog and Baseten Model APIs.** Added Baseten as a first-class
  OpenAI-compatible provider with current GLM/Kimi/DeepSeek/GPT-OSS/Nemotron
  routes, rate-limit and serving-performance metadata in the exported catalog
  contract, live catalog refresh hooks for additional SOTA OpenAI-compatible
  providers, cross-platform llama.cpp setup guidance, and provider-tool-probe
  alias handling for catalog rows that need provider-native `wire_model` IDs.
- **Agent and workflow stdlib now include reusable harness-building blocks.**
  `std/agent/stack` centralizes provider/model option resolution, capability
  cleanup, healthcheck-safe option stripping, LLM caller middleware, and tool
  middleware; `std/agent/stream` adds split-safe private-span filtering for
  streaming chat UIs; `std/workflow/patterns` adds common graph builders and
  typed route failover helpers.

- **Workflow retry policies now execute explicit stage attempts.**
  `retry_policy.max_attempts` now retries VM-executed workflow stage paths,
  including deterministic command verifiers, records every attempt, and stops on
  first success.

### Changed

- **Release gate audit reuses the warm Harn binary (#3540).** Make targets now
  honor `HARN_BIN`, and `release_gate.sh audit` exports the debug binary built
  during warm prebuild so Harn script/conformance/lint lanes avoid repeated
  `cargo run` entrypoints while Cargo-heavy package and Rust lanes keep their
  existing coverage.
- **VM inline-cache dispatch now avoids per-op hash lookups.** Call frames cache
  the VM-local inline-cache set for their chunk once at entry, so adaptive
  binary, property, method, and direct-call cache reads/writes index directly
  into VM-local feedback during hot dispatch.

### Fixed

- Split trigger inbox observability from raw dispatch storage so MCP-facing
  inbox records no longer expose raw webhook payloads.
- `harn serve mcp` exported-function tools can now use deterministic hostlib
  helpers such as `std/command` when the hostlib feature is enabled (#3550).
- Workflow crystallization now rejects traces whose actions are explicitly
  marked non-deterministic (e.g. rejected tool calls, human-approval steps)
  even when they are not flagged `fuzzy`, closing a hole where such actions
  carrying a side effect could be mined into a "deterministic" workflow.
- **Release metadata now verifies the ACP registry manifest.** The release
  audit fails when `spec/acp-registry/harn/agent.json` or any binary archive URL
  drifts from the Cargo package version, preventing stale editor-install
  entries after release bumps.
- Agent loop: `intra_turn_failure_fanout_cap` now collapses EVERY distinct
  identical-failure group in one response, not just the first. A single
  `collapsed_emitted` latch let the first capped fan-out group suppress the
  collapse marker for every later group in the same batch, silently dropping
  those groups' tail calls from the result set with no entry at all. The latch
  now resets whenever a new call signature trips the cap, so each group emits
  exactly one synthetic collapsed result (regression-guarded by a two-group
  scenario in `agent_loop_intra_turn_failure_fanout_cap`).
- Avoid noisy SQLite event-log startup failures when concurrent processes open
  an already-WAL database.
- **Streaming text tool-call promotion now reports parsed arguments as raw
  input.** Promoted candidate events populate `rawInput` instead of mislabeling
  arguments as `rawOutput`, preserving event-log provenance for tool-call
  forensics.

## v0.8.138

### Fixed

- **Surface escalation `provider_error` instead of masquerading it as success
  (#3543).** When smart-escalation switched to the frontier provider mid-trial
  and that escalated `llm_call` failed fast pre-dispatch, the agent loop broke
  silently and ACP replayed the prior cheap-model text as a completed turn — an
  escalation failure looked like success. The non-tracked-failure branch now
  emits the `iteration_end`/`provider_error` event and a primary-retry fallback
  so an inert escalation is observable.
- **Collapse every distinct intra-turn fan-out group, not just the first
  (#3544).** The `intra_turn_failure_fanout_cap` lever tracked emission with a
  single boolean, so only the first identical-failure fan-out group was
  collapsed and later groups leaked through uncapped. Emission is now tracked
  per group.
- **Clamp durable rate-limit backoff so one rate-limited provider can't eat the
  trial wall (#3546).** A single sustained-quota route (e.g.
  `cerebras/gpt-oss-120b`) accumulated ~50s durable rate-limit waits that summed
  to ~89% of a trial's wall budget. The backoff is now clamped so one
  rate-limited provider cannot consume the trial wall.

### Testing

- **Mechanism-contract onramp tier (#3545).** Adds the first required rung below
  the N≥5 convergence gauntlet: a deterministic, mock-provider mini-eval that
  proves a new termination / escalation / judge / guard / routing mechanism
  engages in isolation (fires on its trigger, emits its effect, stays quiet on
  the negative case) before any convergence measurement. A green contract is a
  precondition to a meter run, never a replacement for it.

## v0.8.137

### Security

- Redact run records, action-graph updates, and agent-event durable sinks with
  the shared redaction policy before writing operational artifacts.

## v0.8.136

### Fixed

- **YAML-backed skill and project enrichment readers now support serde_yml
  0.0.13 (#3534).** SKILL.md frontmatter, GitHub workflow scanning,
  pre-commit hooks, and Lefthook command collection use the crate's new
  string-keyed mapping API.
- Anthropic provider: strip storage-only message keys from outgoing Messages
  API requests. A durable assistant turn persists a top-level `reasoning` key;
  echoing it back into `messages[]` triggered a non-retryable HTTP 400
  (`messages.N.reasoning: Extra inputs are not permitted`) that bricked
  thinking-enabled direct-Anthropic runs. Only canonical message-level fields
  (`role`, `content`, `cache_control`) survive the egress boundary; the
  persisted transcript shape is unchanged, so replay and other providers'
  adapters still see `message.reasoning`.
- **PR gates.** Dependency-only manifest and lockfile updates no longer need
  manual changelog fragments, while mixed dependency and source changes still
  trigger the changelog gate.

## v0.8.135

### Fixed

- Parser: recover an unclosed `<tool_call>` wrapper around a structurally
  complete bare `name({ ... <<EOF ... EOF })` call (heredoc sentinel-closed,
  `})` present, `stop_reason: stop`). It was discarded with a false "TOOL CALL
  TRUNCATED" diagnostic; a genuinely cut-off body still reports truncation.
- Agent loop: add `intra_turn_failure_fanout_cap` (default OFF). When one model
  response fans out a batch of byte-identical *failing* tool calls, collapse the
  tail after the Kth identical failure into a single synthetic result instead of
  dispatching every call — the intra-turn analog of the no-progress terminator.
- Replaced the agent-loop text-mode named-tool heuristic with default `missing_tool_call_recovery`.
  Missing tool calls can now be classified across languages and typos without English substring lists.

## v0.8.134

### Added

- **Connector operator runbook.** Added a repo-local first-slice runbook for
  connector administrators covering credential inventory, dogfood checks,
  status commands, and placeholder policy for GitHub, Slack, CircleCI,
  Buildkite, Actions runners, and Harn Cloud gates (#2952).

### Changed

- Updated the ACP Agent Registry submission manifest to the current release,
  added a drift test for the published binary launch targets, and taught release
  preparation to keep the manifest version aligned.

### Fixed

- **Command policy write-intent scanning now recognizes compact shell output
  redirects (#3513).** The deterministic scanner now catches file writes such
  as `cmd>out`, `1>out`, and `2>err` while leaving descriptor duplication and
  output sinks such as `2>&1`, `>/dev/null`, and `>NUL` unflagged.
- **Grounded-review reminders no longer mint phantom `[verified:parse_errors]`
  signals from innocent substrings in correct code.** A `look`/`read`/`search`/
  `glob` of a file whose bytes merely contain `"Parse error"` (a string literal
  or `///` doc comment) is no longer admitted as verifier output — file-display
  tools render bytes, they do not run a build/test/compiler, so they can never
  contribute a grounded review finding on a substring match alone. A passing
  test line whose descriptive name embeds an error phrase (e.g.
  `parser.test.parse error: unclosed section...OK`) is now skipped because it
  carries a trailing pass marker. Genuine verifier output — a real compiler
  `error:` parse-error line or a structured `parse_errors` array — still
  produces the grounded signal.
- Hostlib scanner now pairs source files with common prefix/suffix test naming
  conventions such as `foo_test.go`, `test_foo.py`, `FooTest.java`, and
  `FooSpec.scala`.
- **Release bump PRs now satisfy the changelog-fragment gate automatically.**
  The legacy `release_ship.sh --bump` recovery path labels pure version-bump
  PRs with `no-changelog-needed` before enabling auto-merge, matching the
  gate's documented bypass for version-only release paperwork.
- Made `release_ship.sh --finalize` create release tags with an explicit message
  so signed-tag Git configurations cannot block on an interactive editor.
- Fixed release preparation's ACP Agent Registry manifest bump by importing the
  JSON module used to rewrite the checked-in manifest.

## v0.8.133

### Added

- Added the ACP `mcp/status` request so clients can read active MCP host
  status, including authenticated display identity, through the same Harn-owned
  status source as `harn.mcp.status()`.
- Added `std/identity` for Harn-native ActorChain validation, summaries, compact
  formatting, and structured provenance reports. `std/disclosure` now reuses
  those helpers for traversal and subject parsing.
- Extended `harn.mcp.status()` and `mcp_registry_status()` entries with server
  `transport`/`url` metadata, and added `display_identity` to
  `harn.mcp.status()` for connected OAuth-backed MCP servers with vetted
  identity descriptors.
- **Stdlib slug names, structured schema reports, and clearer `??` formatting.**
  `std/slug` now provides Harn-written random and deterministic memorable name
  helpers, `schema_report(...)` exposes non-throwing path-aware validation
  issues, `std/schema` wraps those reports ergonomically, and `harn fmt`
  parenthesizes mixed `??`/comparison or logical expressions so the parser's
  grouping is visible.
- **Runtime tool_format fallback and equivalence-group catalog guards.** A
  native-tool-format request whose failure fingerprint says the provider's
  server-side tool-call parser choked (the Ollama 500 / EOF leak, or any serving
  stack that 500s/EOFs on the native assumption) now degrades once to the text
  channel and retries there instead of parse-looping or hard-failing — keyed on
  the failure signature, never a model name. The provider catalog also gains two
  build-time invariants: every active row in an `equivalence_group` must declare
  the same `tier` (a capability of the logical model, not of who hosts it), and a
  local-runtime row may not carry `strengths` beyond its group's conservative
  baseline (so a local route cannot inherit a cloud peer's decoration and read as
  already-capable). Both invariants pass on the shipping catalog and only fail the
  build if a future change reintroduces the divergence.

### Fixed

- **Release Harn-format gates now skip materialized package dependencies (#3502).**
  The `fmt-harn` target still checks repo-owned Harn fixtures, but no longer
  recurses into ignored `.harn/packages` dependency installs under examples.
- Pin Fireworks-hosted `gpt-oss-*` to the `text` (heredoc) tool channel instead
  of `json`. An empirical A/B (real Fireworks calls, 3 samples per arm, task =
  author a backslash-heavy Zig file) showed the `json` channel corrupts source
  bodies in every sample — gpt-oss double-escapes the backslashes a JSON string
  arg requires (`\\` becomes `\\\\` Zig multiline prefixes, escaped quotes, and
  one run leaked literal `\n`/`\"` for the whole file) — while the escape-free
  heredoc body stayed byte-clean in every sample. Tool-call dispatch succeeded on
  both channels (no heredoc-wrapper regression).
- Agent loops now emit a typed `llm_call_start` checkpoint before each
  blocking model call so thin hosts can keep liveness timers and run monitors
  honest during long prompt/model phases.
- Fix the Apple-Silicon MLX local route so `auto`/presets land on real weights.
  The MLX aliases (`mlx-qwen3.6-27b`, `mlx-qwen36-27b`, …) still pointed at
  `unsloth/Qwen3.6-27B-UD-MLX-4bit` — the dense vision model that never finished
  downloading (HF cache held only zero-byte `.incomplete` blobs) — even though
  burin #2717 switched the launcher to the coding-tuned Qwen3.6-35B-A3B MoE served
  via `mlx_lm.server`. Repoint every MLX alias (plus the `MLX_MODEL_ID` defaults
  and the install guidance) to `unsloth/Qwen3.6-35B-A3B-UD-MLX-4bit` (q4) /
  `-8bit` (q8), add the model rows with the shared `qwen3.6-35b-a3b`
  logical_model / equivalence_group so eval aggregation compares the MLX and
  llama.cpp runtimes directly, drop the stale `vision` capability (the MoE is
  text-only), and carry `reserved_tool_call_token = true` on the MLX `*qwen3.6*`
  capability row to match the Qwen3.6 tokenizer's reserved `<tool_call>` tokens.
  The MLX runtime profile still requires a `tool_probe` before native is trusted;
  if `mlx_lm.server` returns empty OpenAI tool_calls the safe pin is fenced-json.
- Pin OpenRouter-hosted `openai/gpt-oss-*` to the `text` (heredoc) tool channel
  instead of `json`. The provider-native channel bills noncommittal on this
  aggregate route, so it already rode a TEXT channel; between the two text
  grammars, an empirical A/B (real OpenRouter calls, task = author a
  backslash-heavy Zig file) showed `text` beats `json` on both dispatch (3/3 vs
  2/3) and byte-fidelity (3/3 clean vs 0/3) — gpt-oss double-escapes the
  backslashes a JSON string arg requires and corrupts `\\`-heavy source bodies,
  while the escape-free heredoc carries them verbatim. Same class as the Fireworks
  GPT-OSS flip; direct Cerebras/Groq/DeepInfra GPT-OSS rows keep `native`.

### Security

- **Command-risk scanner quoted workspace wipes.** Recursive workspace-wipe
  detection now treats shell quotes and `$PWD`/`$(pwd)` workspace-root targets
  the way the shell does, so quoted forms such as `rm -rf "."`,
  `rm -rf "$PWD"/*`, `find "." -delete`, and quoted `sh -c`/PowerShell/cmd
  payloads are denied without blocking scoped cleanup like `rm -rf "build/"`.
  PowerShell `-EncodedCommand` payloads are decoded as UTF-16LE before the same
  destructive-command policy is applied.
- **Token redaction covers additional AI-provider key shapes.** Harn's shared
  redaction and `secret_scan` catalog now covers Hugging Face `hf_...`,
  Cerebras `csk-...`, Together `tgp_v1_...`, and Google `AIza...` keys across
  transcripts, receipts, event logs, and stdlib token-redaction calls.
- **Redaction-sensitive orchestration surfaces now share Harn's provider-aware
  secret policy.** Crystallization bundles and friction/context-pack records
  reuse the central redaction catalog for provider tokens, JWTs, private keys,
  and sensitive key/value assignments while still preserving logical secret
  references such as `github/webhook-secret`.

## v0.8.132

### Added

Add vetted identity metadata for bundled MCP presets, including documented source URLs and drift tests for descriptor shape.

### Fixed

Prune Harn/Burin runtime artifact directories from the hostlib code index.
Startup indexing now ignores eval transcripts and generated run state.

## v0.8.131

### Added

Added an `embed` hostlib capability: a cross-platform, fully-offline
text-similarity / embedding core exposing `hostlib_embed_similarity`,
`hostlib_embed_top_k`, `hostlib_embed_vector`, and `hostlib_embed_info`.
The default backend is an always-available, zero-asset lexical hashing
embedder (deterministic across macOS/Linux/Windows, microsecond latency); a
Model2Vec/"potion"-style static token-pooled backend is selected
automatically when a vendored asset is resolvable (sandbox/settings-aware,
no network), degrading cleanly to lexical when absent. A higher-accuracy
candle/ONNX transformer tier can slot in behind a future Cargo feature
without changing the surface.

- `agent_session_push_user_message(session_id, options)` (Harn stdlib
  `std/agent/state`): the in-VM, loop-driver equivalent of the ACP
  `session/inject` method. Pushes a user-role message onto the running
  session; `options.mode: "steer"` delivers it at the next tool/iteration
  break-point (after the in-flight tool result, before the next model call),
  `options.mode: "queue"` defers it to loop exit. Lets in-process hosts (e.g.
  the Burin TUI) steer a turn without the ACP wire. Refs: rfd/session-inject.

### Fixed

A structured/`llm_call`-with-schema retry that failed because the response was
truncated by `max_tokens` mid-JSON now doubles the output-token budget (capped
at 32,768) before the retry instead of replaying the same under-budget call.
Reasoning models (gpt-oss/Harmony, DeepSeek-R, o-series) bill their analysis
channel against the same output budget while it stays invisible in the parsed
text, so a budget that comfortably fits a non-reasoning model's JSON gets
consumed entirely by reasoning — truncating the visible JSON to empty and, once
schema-retry slots are exhausted, returning a DEAD `length_truncation` envelope
(an empty judge verdict that silently falls through to the deterministic
grader). The escalation is provider-agnostic, keyed off the existing
`is_length_truncation` truncation marker.

Fixed SSE and websocket receive limits so timeout, EOF, and open-control polls
no longer consume the configured event/message budget.

- **VM compiler list append optimization.** `x = x.push(item)` on local list
  accumulators now uses the same fused append bytecode as `x = x + [item]`,
  avoiding accidental quadratic cloning while preserving immutable aliasing.

## v0.8.130

### Changed

Provider catalog exports now keep `provider-catalog.json` as the only checked-in
catalog data artifact; TypeScript and Swift provider-catalog bindings are
type-only schema contracts for hosts that load the JSON directly.

## v0.8.129

### Added

- **Provider catalog artifacts now expose provider healthcheck metadata
  (#3484).** The catalog includes provider extra headers and free
  healthcheck probes, and `harn providers export` emits a type-only
  `harn-provider-catalog.d.ts` artifact for hosts that load the JSON catalog
  directly.

## v0.8.128

### Changed

- **Provider catalog and agent model defaults.** Refreshed the built-in
  provider/model catalog for current Anthropic, OpenAI, Gemini, Mistral, xAI,
  Groq, MiniMax, Z.AI, Moonshot, DeepInfra, SambaNova, Together, Fireworks,
  OpenRouter, and NVIDIA NIM models; added empirical tool-mode defaults for
  newly cataloged agent models; unified provider readiness probing; rounded
  model-test cost output to human-scale significant digits; and cut the
  self-hosted CLI model/provider rendering path over to `.harn` scripts.

### Fixed

- **`wait_command` no longer falsely reports a still-running handle when a
  sibling background command completes during the wait.** The per-`handle_id`
  wait is layered on a per-`session_id` completion inbox, and concurrent
  background handles can share one inbox bucket (notably the empty session id
  under `harn test`/headless). The old code parked once, re-drained once, and
  requeued any foreign sibling completion — falsely returning `status:
  "running"` for the handle the caller asked about, which then got cancelled.
  `wait_command::handle` now loops until either its own handle's result arrives
  or the timeout deadline elapses, re-parking for the remaining budget after
  each foreign wakeup. The `timeout_ms == 0` non-blocking poll is unchanged.

## v0.8.127

### Changed

- **User-function calls allocate less on the call hot path.** Entering a closure
  frame previously performed two `VmEnv` (scope-stack) heap clones plus a
  reallocation when the cloned callee env was grown by the empty scope every
  call pushes. The caller-env snapshot is now a move (`std::mem::replace`)
  rather than a clone, and the callee-env clone reserves room for the pushed
  scope so it no longer reallocates. A measured user-function call drops from 5
  heap allocations / 81 bytes to 3 / 41 bytes, with identical scoping,
  recursion, and closure-capture semantics and no `harn` language behavior
  change (#3474).

### Fixed

- **Provider `context_overflow` no longer kills the agent loop.** When a
  provider rejects a request because the prompt exceeds its context window,
  the loop now performs an emergency compaction and retries instead of
  recording a terminal `provider_call_error` and ending the run. The
  Fireworks `gpt-oss-120b` route gained a real catalog context window so its
  auto-compaction budget resolves correctly rather than falling back to a
  placeholder (#3475). Context-overflow classification
  (`is_context_overflow`) is now provider-agnostic: it recognizes overflow
  phrasings from providers that do not use the literal word "context" (e.g.
  Gemini's "input token count … exceeds the maximum"), so the recovery path
  generalizes to every provider, with transparent single-shot surfacing
  rather than misclassification as a generic `invalid_request` (#3479).
- **`tool_define`'d native tool schemas now surface in nested agent loops.**
  A nested `agent_loop` (e.g. an agentic rubric judge or sub-agent) that pins
  `tool_format: "native"` for a route whose capability matrix marks the native
  channel unreliable (`tool_mode_parity = native_unreliable`) previously lost
  every tool call — the model emitted the call as prose instead of a real
  invocation. The stdlib `tool_format` resolution and the wire-level capability
  gate now agree for parity-forbidden explicit pins, so cheap-model graders
  and sub-agents emit real tool calls (#3476).
- **`run` tool accepts an argv vector under the `command` field.** Models
  frequently conflate the `command` (string) and `argv` (array) fields and
  pass `run({command: ["bash", "-lc", "…"]})`. The host `run_command` request
  builder now coerces a list under `command` into argv mode instead of
  throwing `argv must be a non-empty list of strings` (#3477).
- **Security (SB-3): cwd-scoped recursive deletes are flagged as
  destructive.** The deterministic destructive-command detector previously
  only matched root-anchored wipes (`rm -rf /`, `mkfs`, `dd if=`). A
  prompt-injected workspace-relative wipe (`rm -rf .`, `rm -rf ./*`,
  `rm -rf *`, `find . -delete`, `find . -exec rm`) got no `destructive`
  label and was recommended for auto-approval — a data-loss-from-injection
  vector. A new token-based `has_cwd_wipe_tokens` check now flags these for
  UNIX shells, Windows `cmd.exe`, and PowerShell, insensitive to flag order
  and whitespace and catching wrapped forms (`sh -c rm -rf .`,
  `cd x && rm -rf .`) (#3478).

## v0.8.126

### Added

- **Package registry archive sources.** Registry entries can now resolve to
  checksum-verified `.tar.gz` package archives, and private HTTP(S) registries
  can use `HARN_PACKAGE_REGISTRY_TOKEN` for both index and archive fetches.
- Provider-side prompt caching now defaults ON for routes whose capability
  matrix declares `prompt_caching`. The stable system-prompt + tool-definitions
  prefix re-sent on every turn of a multi-turn agent loop (and across the rubric
  grader's turns×trials) is marked cacheable, so supporting providers discount
  it heavily — Anthropic ephemeral caching (~90% off cached input), OpenRouter
  `cache_control` passthrough, and implicit DeepSeek / gpt-oss caching. The win
  is largest for the cheap value models the product steers toward. Routes that
  do not advertise prompt caching are unaffected: the resolved `cache` flag
  defaults to `false` for them, leaving the outgoing request byte-identical. An
  explicit `cache:` option is always honoured verbatim — `cache: false` opts out
  anywhere, and an explicit `cache: true` on a non-caching route still errors
  loudly via the capability gate.

### Changed

- **Synchronous builtin calls now dispatch on the fast (sync) interpreter
  path.** A bare builtin call such as `abs(x)` / `len(xs)` previously fell
  through `Op::CallBuiltin`'s sync handler to the async handler, which re-ran
  name resolution (a second local-slot scan, env walk, and — inside imported
  modules — the `module_functions` + `module_state` mutexes) and spun up the
  async state machine only to reach the same synchronous builtin. The sync
  handler now dispatches synchronous builtins directly once it has confirmed the
  name is not a user closure, eliminating the redundant resolution and the async
  hop; asynchronous builtins are unchanged. Resolution semantics are identical —
  a user `fn` that shadows a builtin name still wins — and the change holds one
  fewer lock per call inside imported modules, so it is friendly to the
  multi-threaded runtime. No `harn` language behavior changes.
- **Dict keys are now interned, refcounted strings instead of owned `String`s.**
  The map backing every `VmValue::Dict` changed from `OrdMap<String, VmValue>` to
  `OrdMap<HarnStr, VmValue>` (the thin one-word `arcstr::ArcStr` from the value
  shrink), and keys flow through a bounded interner (`harn_vm::value::intern_key`).
  Agent workloads are dict-heavy — the same field names (`role`, `content`,
  `arguments`, …) recur across thousands of message/JSON dicts — so each recurring
  key now shares a single allocation (a refcount bump) instead of allocating a
  fresh `String` per key, and dict tree nodes hold an 8-byte key instead of a
  24-byte one. The interner is bounded (keys up to 64 bytes, at most 8192 distinct
  entries) so high-cardinality or adversarial keys fall back to a plain allocation
  and can never grow it without bound. `VmValue::dict(...)` still accepts the
  `BTreeMap<String, _>` / `DictMap` maps callers already build (it interns on the
  way in). No `harn` language behavior changes.

### Fixed

- Cheap-model tool-call dialect: three fixes that stop advertised-only-tool
  lanes (e.g. the Burin eval lane: `look`/`search`/`edit`/`run`/
  `read_command_output`) from denying calls models emit in another harness's
  vocabulary. (1) The tool-calling contract (both the text and fenced-JSON
  prompts) now states under `## Available tools` that these are the ONLY callable
  tools and that any unlisted name is rejected — pick the closest listed tool;
  the JSON contract's worked `## Example` no longer primes the unlisted
  `write_file` name (it now uses an illustrative `<tool>` placeholder). (2) The
  tool-name normalizer resolves SEMANTIC aliases to canonical Harn tools so the
  gate, dispatch, and telemetry all see a real name: `repo_browser.*` /
  `repository_browser.*` / `workspace_browser.*` / `file_browser.*` file/list
  verbs → `look`, their search/find/grep verbs → `search`; `container.exec` /
  `container_exec` / `exec` / `sh` / `shell` / `bash` → `run` (remapping a
  `script` / `cmd` arg onto `command`); and edit-action verbs called as
  top-level tools (`replace_range`, `replace_body`, `insert_after`,
  `insert_function`, `delete_range`, `exact_patch`, `add_import`) →
  `edit({ action: <verb>, … })`. Raw-write/whole-file tools (`write_file` /
  `delete_file` / `patch_file`) are deliberately NOT aliased to `edit` — they are
  semantically lossy — and the symbol-level edit tools (`replace_symbol` /
  `remove_symbol`) are NOT folded into `edit` either, since `replace_symbol` is a
  hard-kept standalone tool in the default surface; all fall through to the
  denial feedback instead. (3) The permission-denial feedback now NAMES the active
  policy's allowed tools (`… Available tools: look, search, edit, run,
  read_command_output.`) so the model can self-correct in one turn. Sibling to
  the `tool.`/`functions.` namespace-prefix strip.
- Tool-name normalization now strips a leading `tool.` / `tools.` / `functions.`
  / `function.` namespace prefix that cheap OpenAI-compatible hosts (notably
  `gpt-oss-120b`) prepend to native tool calls — `tool.look` → `look`,
  `functions.search` → `search` — so the call resolves to a real tool instead of
  being denied as an unknown name (which previously sent the model into give-up /
  thrash loops). The strip is guarded against the generic-wrapper names
  (`tool.call` / `tool.exec` / `function.call`), which still unwrap their inner
  `{ name, args }` payload rather than collapsing to `call` / `exec`. The
  unknown-tool feedback also recognizes cross-harness edit aliases
  (`apply_patch`, `str_replace`, `str_replace_editor`, `edit_file`, `create_file`)
  and points the model at the `edit` tool instead of issuing a bare denial. This
  is the tool-name-normalization sibling to the tool-format dialect gate.

## v0.8.125

### Added

- Tool-call dialect validity gate: the provider/model capability registry now
  *enforces* its tool-call dialect facts instead of leaving them advisory. A
  requested `tool_format` whose wire channel a route is known not to return
  parseable tool calls on — by `tool_mode_parity` (e.g. a `native` pin on the
  `native_unreliable` DeepSeek V3.2 route, which silently drops to unparsed DSML
  text) or by an explicit `text_tool_wire_format_supported = false` (e.g. the
  native-only local Ollama Qwen3 route) — is auto-corrected to a working channel,
  preferring the route's `preferred_tool_format`, with an actionable message
  naming the bad combo and the working alternative. A route with no working
  channel at all passes through untouched rather than rewriting to another broken
  format. Enforced at `resolve_model_info` and at the tool-bearing `llm_call`
  seam, so a harness can no longer silently get vanishing tool calls from an
  invalid model×provider×tool_format combination.

### Changed

- **`assert(cond)` and `require cond` now narrow types like a guard.** Both
  diverge (throw) when `cond` is falsy, so code after them may rely on the
  truthy refinement: `assert(x != nil, ...)` (or `require x != nil`) followed
  by `x + 1` now type-checks without a `??`, matching how an `if x == nil`
  guard already narrows. This is the TypeScript "assertion function" model and
  removes friction for the idiomatic `assert(value != nil)` test/precondition
  pattern.
- **Entering a lexical block no longer allocates an empty binding map.** Every
  block pushes a scope, but inside a function body its bindings compile to local
  slots rather than env writes, so the pushed scope is almost always empty — yet
  it used to `Arc::new(BTreeMap::new())`-allocate (and free) one map per entry, a
  per-iteration cost in any loop whose body is a block. Empty scopes now share a
  single process-wide immutable map (a refcount bump), and the first real
  binding copies-on-write away from it, so scopes that never bind anything never
  allocate. No behavior change.
- **The hosted language specification is now navigable chapter-by-chapter.**
  `scripts/sync_language_spec.harn` generates a per-chapter page under
  `docs/src/spec/language/` for each section after the overview and rewrites the
  chapter list in `docs/src/SUMMARY.md`, so the site nav and search index cover
  every chapter instead of one ~7k-line page. `docs/src/language-spec.md` is now
  a landing page (the overview plus a table of contents); deep links to the old
  monolithic anchors were repointed to their chapter pages. The single-file
  `spec/HARN_SPEC.md` assembly is unchanged, and the per-chapter
  `spec/chapters/*.md` sources remain the one place to edit.
- **The core `VmValue` runtime value is now 16 bytes (down from 24).** Every
  value the interpreter pushes, pops, clones, and writes to a local slot — plus
  every element of a `list` and every entry on the stack — is a third smaller,
  improving cache density across the whole VM. The shrink boxes the four
  oversized payloads (`Decimal`, `StructInstance`, and the `Range`/`BuiltinRefId`
  already-boxed cases) behind a shared pointer and replaces the 16-byte
  `Arc<str>` fat pointer behind the string-shaped variants (`String`,
  `BuiltinRef`, `TaskHandle`) with a one-word thin string (`arcstr::ArcStr`,
  re-exported as `harn_vm::value::HarnStr`). String-literal loads and enum/struct
  field reads stay zero-copy refcount bumps. No `harn` language behavior changes;
  this is an internal representation change (the unsafe pointer work lives in the
  vetted `arcstr` crate, not in Harn).

## v0.8.124

### Breaking

- **Selective imports now respect `pub` visibility.** `import { name } from "m"`
  could previously bind a non-`pub` function of `m`, even though a wildcard
  `import "m"` would not see it — selective imports silently bypassed
  visibility. Now both forms expose the same surface: a module that marks any
  function `pub` exports only its `pub` functions (and `pub import`
  re-exports); a module that marks nothing `pub` still exports everything
  (the zero-ceremony fallback is unchanged). Importing a non-`pub` name from a
  module that has opted into explicit exports is rejected at `harn check` time
  with `HARN-IMP-002` (pointing at the import, suggesting `pub`) and at load
  time. **Migration:** mark the symbol `pub` if it is meant to be importable;
  to test a private helper, co-locate the test in the same file (it sees
  module-private functions directly). This matches TypeScript, Rust, and Go.

### Added

- **The `harn-diagnostics` skill now indexes the HARN-* error codes.** `harn
  skills get harn-diagnostics` points at how to look up any `HARN-<CAT>-<NNN>`
  code — `harn explain <CODE>` (and `--json`), the full `harn explain --catalog`
  index, and the committed `docs/src/diagnostics.md` / `docs/diagnostics-catalog.json`.
- **Arithmetic on a possibly-nil operand is now a compile-time error, with
  control-flow narrowing for assignments.** `x + 1` where `x: int?` is flagged
  (`operand of '+' may be nil`) instead of throwing `nil + 1` at runtime, for
  `+ - * / % **`. A binding proven non-nil by an earlier assignment
  (`x = 5`), a `!= nil` guard, or `??` is narrowed and not flagged — assignment
  now participates in nil-narrowing (vars and `obj.field` paths), matching
  TypeScript/Flow control-flow narrowing. This also sharpens the existing
  nilable property-access diagnostics after an assignment.
- **`harn test` now supports duration-aware user-test sharding.** User test
  suites can pass `--shard-index` and `--shard-total` (or the matching
  `HARN_TEST_SHARD_INDEX` / `HARN_TEST_SHARD_TOTAL` environment variables) to
  split CI matrix work after discovery and balance shards using the existing
  timing cache.

### Changed

- **Running a compiled chunk no longer deep-copies its bytecode.** `Vm::execute`
  used to clone the entire `Chunk` (bytecode + constant pool + side tables) on
  every top-level run. The internal run path now threads the shared
  `Arc<Chunk>` straight into the call frame, and a new `Vm::execute_arc(ChunkRef)`
  entry point lets callers that re-run the same chunk (the `harn serve` request
  path, ACP, record filters) pay a refcount bump instead of an `O(code)` copy.
  `Vm::execute(&Chunk)` is unchanged for one-shot callers.
- **In-place list/dict concat now also speeds up dynamically-typed
  accumulators.** The `out = out + [item]` / `out += [item]` loop already
  extended the accumulator's buffer in place (amortized O(n)) when its type was
  statically known to be a list or dict. A new fused `ConcatAssignLocal` opcode
  gates that in-place extend on the *runtime* value instead, so untyped (`any`)
  accumulators get the same O(n²) → O(n) win — and a throwing `+=` on a scalar
  now reliably leaves the binding at its previous value.
- **Set membership is now O(1).** `set` values previously stored only a plain
  list and rebuilt a hash index from every element on each `contains` /
  `union` / `intersect` / `difference` / subset/superset/disjoint call — O(n)
  work (plus an allocation) per query. They now carry a resident structural-key
  index alongside the items, so membership is O(1) and the set-algebra builtins
  and methods drop from rebuild-per-call to a single probe per element.
  Observable semantics (insertion-ordered iteration, structural dedup,
  order-independent equality and hashing) are unchanged.

### Fixed

- **Generic type parameters now support multiple `where` bounds.** A
  constraint written as repeated clauses (`where T: A, T: B`) no longer lets
  the second bound clobber the first — both apply, so a method guaranteed by
  the first interface is resolved correctly in the function body. The additive
  spelling `where T: A + B` now parses as well; the two forms are equivalent.
  Method resolution on a multiply-bound `T` accepts a method declared on any
  bound interface, and call-site checking enforces every bound.
- **Imports across a module cycle now bind reliably.** A plain `import "m"`
  or `import { name } from "m"` that resolved to a module still mid-load (an
  import cycle) used to silently skip binding the name, so calling it later
  failed with `Undefined builtin: <name>` — and which module got starved
  depended on load order, making the failure look nondeterministic. Cyclic
  imports are now bound late, once every module in the cycle finishes loading,
  for both bare references and calls. A `pub import` re-export across a cycle
  remains unsupported but now fails with a message that names the cycle
  instead of the misleading "imported module was not loaded".
- **The formatter no longer drops or relocates comments on multi-line binary
  and pipe expressions.** A trailing comment on the first line of an
  expression that breaks across lines (e.g. `let r = aaa // note` followed by
  `+ bbb`) was silently dropped at the top level, or moved out of its
  enclosing block to the end of the file inside a function. Each broken
  operand's trailing comment is now preserved in place.
- **`json_stringify` now preserves the decimal point on whole-number floats.**
  A `float` like `2.0` serialized to `"2"` (so `json_parse` read it back as an
  `int`) and disagreed with `json_stringify_pretty`, which emitted `"2.0"`.
  Compact output now routes finite floats through the same serde `Number`
  formatter as the pretty printer, so floats round-trip as floats.
- **`to_int` / `to_float` now trim surrounding whitespace before parsing.**
  `to_int("  42  ")` and `to_float(" 1.5\n")` returned `nil`; they now parse to
  `42` / `1.5`, matching the sibling `decimal(...)` builtin and Python/JS
  numeric coercion. Non-numeric strings still return `nil`.

## v0.8.123

### Added

- **`harness.secrets` now exposes host-owned secret management primitives
  (#499).** Embedders can attach a `SecretProvider` that backs scoped
  read/write/rotate/lease operations while Harn supplies the typed VM surface,
  audit context propagation, and binary-safe variants.

### Changed

- **Scalar integer arithmetic now promotes to float on `i64` overflow instead
  of silently wrapping.** `a + b`, `a - b`, `a * b`, `a ** b`, and unary `-a`
  previously wrapped two's-complement (e.g. `i64::MAX + 1` became a large
  negative number); they now promote to `float`, matching the language's own
  aggregate policy — `sum`/`abs` already promote on overflow. In-range integer
  arithmetic is unchanged. The compile-time constant folder defers the same
  overflow cases to the runtime so folded and unfolded expressions agree.
- **Regex builtin docs and conformance now pin flag and capture semantics.**
  `regex_match`, `regex_replace`, and `regex_captures` document and test
  inline `(?is)` flags, trailing `i`/`s` flags, newline-spanning lazy captures,
  and the exact `regex_captures` result shape.

### Fixed

- **OpenRouter GPT-OSS now defaults to JSON text-channel tools.** The provider
  capability matrix no longer advertises native tool calls for
  `openrouter/openai/gpt-oss-*`, while direct Cerebras, DeepInfra, and Groq
  GPT-OSS routes stay on native tools.
- **`harn precompile` now resolves imports before type-checking, so an
  imported symbol that shares a name with a builtin no longer reports phantom
  type errors.** Calling `render` imported from `std/disclosure` (or any
  stdlib/user export colliding with a builtin such as the `render` template
  helper) compiled and ran fine under `harn run` but failed `harn precompile`,
  because the precompiler checked the call against the builtin's signature
  instead of the import. Precompile now derives the import graph like
  `harn run`/`harn check`, and an imported name shadows a same-named builtin in
  the type checker.
- **Exhaustive `match` on a `bool` no longer reports a false "can fall
  through" error.** A `match` over a `bool` scrutinee that covers both `true`
  and `false` with returning arms is now recognized as exhaustive and
  terminating, matching how Rust and Swift treat a `match`/`switch` over a
  boolean — no wildcard arm required.
- **`harn check` now type-checks expressions inside string interpolation.**
  Holes in `"... ${expr} ..."` are re-parsed and run through the normal
  checker, so undefined calls, argument-type mismatches, and other static
  errors inside `${...}` are caught at check time instead of slipping through
  to a runtime crash.

## v0.8.122

### Fixed

- **Provider-native tool-call normalization.** Harmony marker-wrapper tool names
  such as `<|constrain|>json` now normalize command-shaped calls before policy
  checks, preventing valid provider-native tool calls from tripping tool-ceiling
  enforcement before dispatch.

## v0.8.121

### Added

- Carry RFC 8693 actor-chain metadata across A2A dispatch, serve, and
  push-trigger flows, and add draft-tracking OAuth token-exchange capability
  rows for ID-JAG, transaction tokens, and WIMSE WIT/WPT.
- **Slack outbound disclosure now carries actor-chain bylines and AI markers (#3340).**
  `std/disclosure` exposes `slack_message_disclosure(...)`, and
  `std/connectors/slack.post_message` attaches that artifact from an
  `actor_chain` option so Slack connector packages can fall back to textual
  bylines unless `chat:write.customize` is granted and can emit
  machine-readable AI metadata where the surface supports it.

### Changed

- **Release binary size gate now allows the current stripped Linux binary (#3432).**
  The release artifact budget was raised to 186 MiB so v0.8.120 binary builds can
  publish without failing just above the previous threshold.

### Fixed

Normalize wrapped GPT-OSS tool-call envelopes before policy and dispatch.

## v0.8.120

### Added

- **Disclosure stdlib Git trailer helpers (#3338).** `std/disclosure` now
  exposes `git_trailers` and `append_git_trailers` so commit messages and PR
  bodies can append actor-chain `Co-Authored-By` / `Assisted-by` attribution
  from the configured disclosure templates while filtering non-human
  `Signed-off-by` lines.
- Added `std/disclosure` GitHub `author_mode` output so commit and PR
  adapters can choose between human-authored commits with actor-chain trailers
  and GitHub App bot-authored writes.
- Added RFC 8693 OAuth token exchange to `std/oauth/client`, including
  data-backed provider capability rows, delegation/impersonation validation,
  and nested `act` claim helpers.
- Added opt-in RFC 8693 token exchange for HTTP MCP clients so servers can
  receive transient delegated bearer tokens for active actor-chain sessions.
- Added `std/agent/completions` helpers for completion context, policy,
  proposal, telemetry, usage, and TrustGraph projection.

### Fixed

- **Fireworks GPT-OSS provider capabilities (#3426).** Fireworks-hosted GPT-OSS
  routes now default to fenced-JSON text tools instead of native tool calls, so
  agent loops avoid billed empty native completions with no dispatchable tool
  calls.

## v0.8.119

### Added

- Added OpenTrustGraph actor-chain lineage metadata on trust records and
  verifier checks that nested actor chains stay aligned with `parent_record_id`
  lineage.
- Added actor-chain scope attenuation policy, validation, conformance coverage,
  and OpenTrustGraph alerts for widened delegation scopes.
- Added `std/disclosure` for rendering Git trailers, Slack bylines, and GitHub
  author choices from ActorChain values with layered TOML configuration.
- Add provenance and compaction recommendation options to
  `agent_session_seed_from_jsonl` for explicit external coding-agent transcript
  imports.
- **Agents API trigger-run observation.** `harn serve api` now exposes recent
  trigger-dispatched workflow runs at `/v1/workflow-trigger-runs`, including
  trigger outbox metadata joined with matching action-graph observations for
  Burin's local comment-trigger workflow surfaces (burin-code#2232).

### Changed

- Make task-plan `human_gate`, `deterministic_command`, and `compact` nodes
  execute as first-class workflow stages instead of falling through to LLM
  stages.
- Upgrade the `agent` project scaffold to an opinionated `agent/` app layout
  with instructions, skills, tools, subagent, channel, sandbox, and schedule
  convention folders.

## v0.8.118

### Added

- **Agent lifecycle tools can now skip the self-suspend surface (#3413).**
  `agent_lifecycle_tools` and `agent_loop` accept
  `agent_await_resumption_enabled: false` to avoid exposing
  `agent_await_resumption` when a host does not want model-initiated
  suspension in the active tool schema.

## v0.8.117

### Added

- Threaded RFC 8693 actor chains through agent-session context and exposed them with `agent_session_actor_chain(id?)`.

### Changed

- **ACP websocket control attribution now references `ActorChain` (#3334).**
  Forwarded controls and control-outcome audit events keep the legacy
  `clientId`/`connectionId`/`role`/`source` fields while carrying the canonical
  RFC 8693 actor chain for downstream provenance.

### Fixed

- Resume interactive HTTP MCP tool calls after a mid-loop OAuth 401 once the host
  completes authorization, while headless runs now fail with a clear auth error
  instead of hanging.
- **`harn check` now recognizes `@stream` route attributes (#3403).** Stream
  routes no longer emit an `unknown attribute @stream` warning during static
  checking.
- **Route attribute checking now recognizes `@raw` (#3404).** Static
  typechecking no longer reports declaration-only raw HTTP routes as unknown
  attributes.
- **Hostlib command artifacts now expire stale temp directories.** Command output
  artifact creation now performs a throttled best-effort sweep of old
  `harn-command-*` temp directories while preserving fresh artifacts, live-PID
  artifacts, malformed names, and symlinks.
- **JSONL agent event logs are now line-durable during live runs.** The flat
  `JsonlEventSink` flushes after each appended event so replay/eval consumers
  can read terminal tail records such as `iteration_end`, `typed_checkpoint`,
  and `judge_decision` before the sink is dropped.
- **Harn target-dir cleanup now recognizes nested Codex worktrees.** The
  `prune_stale_targets.sh` helper scans bounded nested repo roots such as
  `$HOME/.codex/worktrees` and uses portable mtimes, so per-worktree Cargo
  targets are kept or pruned based on the live worktree set instead of only
  direct children of `$HOME/projects`.

## v0.8.116

### Added

- **HTTP response stdlib.** Added `http_reply_from(result)` to convert host or
  adapter response records into tagged HTTP response envelopes while preserving
  headers, status, bytes, stream, SSE, JSON, and no-body semantics (#3400).

### Fixed

- Fixed token-pressure reminders to use the current prompt/context token count
  when available instead of cumulative session token totals, preventing false
  "compact or summarize" warnings in long agent loops.

## v0.8.115

### Fixed

Enable SQLite WAL mode (with `busy_timeout` + `synchronous=NORMAL`) on the LLM read cache
(`llm.sqlite`) and the durable rate limiter (`llm-rate-limits.sqlite`) so multiple concurrent
Harn sessions on one machine no longer hit `SQLITE_BUSY` (silently dropped usage rows / blocked
LLM calls). Matches the WAL pattern already used by `events.sqlite`.
Fix `matching_paren_len` (stdlib public-function signature parser) so a top-level `]` or `}`
terminates the scan when bracket depth returns to zero, matching its symmetric opener arm and the
sibling `split_top_level_params`. Previously only `)` triggered the return, so a mismatched closing
bracket made the scan run to the end and yield `None`.
- **Provider capability audits now catch native-tool contradictions.** The
  OpenRouter Claude capability rows also track direct Anthropic thinking,
  prefill, structured-output, and tool-format breakpoints for the covered
  Claude 4.6/4.7 and Mythos-class routes.

## v0.8.114

### Added

- **harn-serve route policies and embedded-agent facade.** `std/harness/policy`
  now supports typed missing-auth, scope, principal-kind, resource, tenant, and
  method-specific denials for route handlers; `harn-serve` exports matching
  policy metadata and a stable `EmbeddedAgentClient` facade over ACP lifecycle
  calls and run/session views; public examples now prefer argv-style command
  execution and stable `harn runs view` projections (#3323, #3324, #3325).
- **Actor chains.** `harn-vm` now exposes an RFC 8693-compatible
  `ActorChain`/`Principal` type with nested `act` JSON serde, optional
  `may_act`, current/origin helpers, and plain `VmValue` dictionary conversion
  for Harn scripts (#3332).
- **Equivalent LLM failover can now opt into no-dispatch upstream contract violations.**
  `equivalent_failover: {on_no_dispatch: true}` lets same-logical-model
  fallback routes advance after the normal empty-completion retry is exhausted
  for billed no-dispatch provider responses.

## v0.8.113

### Added

- **`@policy(kinds: "...")` — a declarative route auth policy for
  `harn serve site`.** A routed `pub fn` can now declare the principal
  kinds permitted to invoke it, composing with `@scopes` rather than
  replacing it: `@policy(kinds: "operator platform_admin")` admits a
  request only when the embedder-resolved principal `kind` (see
  `harness.auth.kind()`) is in the allow-set. Enforcement runs at site
  admission immediately after the scope check and **fails closed** — a
  principal the embedder did not classify can never satisfy a non-empty
  allow-set. Denials render the tenant-safe `forbidden_principal_kind`
  403 envelope, which names the route's allowed kinds (route
  configuration) but never echoes the caller's own kind. The parsed policy
  is carried on the route's export entry (`ExportedFunction.policy`) so
  audit tooling can see which routes declare a principal-kind guard. A
  malformed argument (unknown key, positional, or non-string value) is
  dropped with a `HARN-SRV-017` diagnostic and a typechecker warning,
  leaving any host-side defense-in-depth check in place. Together with the
  `harness.auth` handle this is the declarative half of the Harn-side route
  auth policy (issue #3323); the imperative `require_policy(...)` helper for
  method-specific / resource-match cases is a follow-up.

- **`std/harness/policy.require_policy` — imperative route auth-policy guard.**
  The second half of the route-policy toolkit (the declarative
  `@policy(kinds: ...)` annotation is the first): a `.harn` handler can call
  `require_policy({kinds: [...], scopes: [...]})` to enforce a principal-kind
  / scope policy that depends on runtime data the annotation cannot see (a
  path or body field, resource ownership). It composes the ambient
  `harness.auth` principal and returns `nil` when the policy is satisfied, or
  a ready-to-return tenant-safe HTTP 403 envelope (`http_error`) when it is
  not — `let denial = require_policy({...}); if denial != nil { return denial }`.
  Denials name the route's requirement (`allowed_kinds`, the missing scope)
  but never echo the caller's own kind, matching the `@policy` denial, and
  fail closed: an unauthenticated or unclassified principal never satisfies a
  non-empty `kinds` allow-set.

- **ACP `mcp/authorize_batch` + streamed status.** Added an ACP method that
  begins OAuth for many MCP servers at once over the bulk-auth driver, returning
  `{ flows, skipped, failed }` and streaming per-server progress as
  `mcp/authorize_status` notifications. Captured callbacks posted to
  `mcp/oauth_callback` now route through the active batch (streaming
  `Exchanging`/`Connected`) when their `state` matches; the single-URL path is
  unchanged (#3357).

- **`mcp.reauth_expired()` bulk re-auth builtin.** Added a harn-script builtin
  (`harn.mcp.reauth_expired` / `mcp_reauth_expired`) that enumerates the declared
  OAuth-backed MCP servers, drives the bulk-auth driver in `Expired` mode
  (silently refreshing what it can), and returns one outcome per server
  (`reauth_required` / `skipped` / `failed`) so triggers and workers can satisfy
  401-mid-loop re-auth declaratively rather than failing one call at a time
  (#3358).

### Changed

- **`llm_cost` returns an exact `decimal` instead of a binary `float`.** The
  per-call cost is money, so it is now computed and returned as a `decimal`:
  summing many calls no longer drifts, and the value compares exactly. Each
  catalog rate is recovered to its *authored* decimal value (the short literal
  in `providers.toml`, e.g. `0.15`) via shortest-round-trip recovery, so the
  result is genuinely exact rather than `float`-rounding laundered into
  false precision. `llm_format_usd` now accepts a `decimal` amount (alongside
  `float`/`int`), so `llm_format_usd(llm_cost(...))` keeps working. This is a
  breaking type change for scripts that compared `llm_cost(...)` against a
  `float` literal — `decimal` is a clean island and never compares
  equal/ordered with `float`, so compare against `decimal("…")` instead. The
  `@budget` enforcement accumulator and `llm_session_cost`/`llm_pricing`/
  `llm_compare_costs` continue to report `float` for now; migrating that
  family to `decimal` is tracked separately.

## v0.8.112

### Added

- **Run/session view compatibility fixtures.** Added a shared fixture corpus
  and drift check for `harn.run_view.v1` and `harn.session_view.v1` projections
  so downstream clients can detect intentional view-contract changes (#3322).

- **`harness.auth` — a read-only authenticated-principal handle for `.harn`
  routes.** A `harn-serve` dispatch now threads the principal it
  authenticated at admission — subject, scheme, granted scopes, and an
  optional embedder-assigned principal `kind` — to the `.harn` callee as the
  ambient `harness.auth` sub-handle, alongside the existing `harness.tenant`.
  Routes can read identity and compose their own authorization without a
  host-side dispatch guard: `harness.auth.is_authenticated()`,
  `harness.auth.subject()` / `try_subject()`, `harness.auth.scheme()` /
  `try_scheme()`, `harness.auth.kind()`, `harness.auth.scopes()`, and
  `harness.auth.has_scope(scope)`. `subject()`/`scheme()` raise a typed
  `Auth` error when no principal is bound (mirroring `harness.tenant.id()`);
  the `try_*`/`kind` getters return `nil` and `scopes()`/`has_scope()`/
  `is_authenticated()` degrade to empty/false so an unauthenticated route can
  branch without try/catch. The handle is identity-only: it carries no
  credentials or secrets, never the tenant (that stays the single-sourced
  `harness.tenant` ambient), and never the opaque embedder auth context (that
  stays the host-call-bridge channel). The synthetic anonymous principal
  harn-serve admits under allow-all binds nothing, so `is_authenticated()` is
  `false` when no credential authenticated the request. Foundation for
  Harn-side route auth policies (issue #3323); unblocks harn-cloud's adoption
  of declarative route policies in place of duplicated Rust dispatch guards.

### Changed

- **Lazy manifest-hook install for `harn test`.** A hook's handler closure is
  resolved (loading its module's whole import graph) on first fire against the
  firing VM, instead of eagerly during every test's setup. Pure-logic unit
  tests that never fire a hook no longer pay that cost — for a large manifest
  like burin-code this cut per-test setup from ~1s to single-digit ms (suite
  wall 840s -> 550s). Hook semantics are unchanged: closures still resolve
  against the firing VM, preserving per-test module-state isolation. Production
  callers (`harn run`, agent loops) stay eager via `install_manifest_hooks`, so
  a misconfigured handler still fails fast at startup; the lazy path is opt-in
  via `install_manifest_hooks_with_mode(.., lazy = true)` (#3370).

### Fixed

- **`read_range` reads a raw path again when the code index is unbuilt.** The
  read-only secondary-roots work (#3352) routed `read_range` through a resolver
  that returned no path when the primary index slot was `None` (never rebuilt),
  so reads erred with "path must stay within the indexed workspace root". This
  broke callers that read a file before any rebuild — `agent_run` scanning a
  process-output temp file to surface buried test-failure lines, and eval/verify
  reads over arbitrary shell output. Restored the pre-#3352 fallback: with an
  unbuilt primary index, resolve the raw path so the read still succeeds (a
  genuinely missing path still fails with "file not found") (#3372).

## v0.8.111

### Added

- **MCP authenticated-identity resolution (#3349).** New `harn_vm::mcp_identity`
  turns the per-server identity recipes from the preset catalog into a
  human-readable "logged in as …" string. The OAuth engine now captures the
  non-credential extras a token endpoint returns (Notion, for one, inlines the
  workspace + authorizing user) onto the stored token, so the headline
  `token_response` source resolves with no extra network call. The bundled
  Notion preset ships the first vetted descriptor; the preset-catalog schema is
  now version 2.

- **`harn mcp status` now shows who you're logged in as (#3350).** The status
  report carries a `display_identity` field — "Jane Doe <jane@acme.com> — Acme",
  for example — for connected servers with a vetted identity recipe and a
  captured token payload (the `mcp status --json` schema is now version 3, and
  the human output appends `as=…`). The ACP `mcp/oauth_callback` response gains a
  `displayIdentity` so an embedding GUI can show it the moment a server connects.

- **Bulk MCP OAuth driver + per-server status stream (#3355).** New
  `harn_vm::mcp_bulk_auth` orchestrates authenticating *all* pending OAuth-backed
  MCP servers at once on top of the unchanged per-server engine, and publishes a
  per-server `McpAuthStatus` event stream (`discovering → awaiting_consent →
  exchanging → connected/failed/skipped`). `prepare()` begins every selected
  flow concurrently and returns the authorize URLs (keyed by OAuth `state`) for
  a surface to open against one shared loopback listener; `complete()` routes
  each captured callback back by `state`. Modes cover first-auth (`Missing`),
  re-auth of stale tokens (`Expired`), and forced re-auth (`All`); concurrency
  and per-server timeout are overlayable via `[bulk_auth]`
  (`~/.config/harn/mcp_bulk_auth.toml`). The keystone for `mcp login --all`
  (#3356), ACP `authorize_batch` (#3357), and bulk re-auth (#3358).

- **`harn mcp login --all` / `--reauth` — bulk OAuth login (#3356).** Authenticate
  every OAuth-backed MCP server in the nearest `harn.toml` in one command instead
  of one `harn mcp login <server>` at a time. `--all` first-auths every
  unconnected server; `--reauth` re-authenticates servers whose stored token has
  expired or been revoked (`--all --reauth` force-reauths everything); `--only`
  narrows the set and `--concurrency` overrides the prepare fan-out. One shared
  loopback listener captures every redirect (demuxed by the OAuth `state`),
  browser consents open serially to avoid a popup storm, and per-server progress
  streams live (`--json` emits one status object per line). Built on the bulk
  driver (#3355); single-server `harn mcp login <server>` is unchanged.

- Added a `serving_precision` provider capability field (`trusted` / `degraded` /
  `throttled` / `unverified`) so the capability matrix can label routes that serve
  a model at degraded quality or unusable timing. Seeded the known gpt-oss-120b
  verdicts (Fireworks + OpenRouter = trusted, SambaNova = degraded/quantized,
  Cerebras = throttled) and exposed the field on `harn check --provider-matrix
  --json`, giving the Burin meter precision canary a data-driven signal instead of
  trusting provider liveness alone.

## v0.8.110

### Added

- Added shared `harn.run_view.v1` and `harn.session_view.v1` projections for
  persisted run records, CLI JSON output, portal details, and live session APIs.

- **MCP server presets are now overlayable data, not Rust constants (#3348).**
  The well-known preset catalog (Notion, Linear, GitHub, Filesystem) moved from
  a compiled-in table to bundled TOML (`mcp_presets.toml`) that can be overridden
  or extended at runtime without a recompile — point `HARN_MCP_PRESETS_CONFIG` at
  a file or drop one at `~/.config/harn/mcp_presets.toml`; overlays merge
  last-writer-wins by preset `id`, then append new presets. The catalog also
  gains an optional, inert `identity` descriptor schema (`IdentityProbeDescriptor`)
  describing how to fetch a human-readable "logged in as …" string per server;
  the probe runner that consumes it ships separately (#3349). The serialized JSON
  catalog contract is unchanged.

- **`code_index` gains additive, read-only secondary roots so dependency/SDK
  symbols are discoverable without clobbering the project index (#3352).**
  `hostlib_code_index_rebuild({root})` still owns exactly one writable root and
  flips its slot wholesale, so indexing a dependency root through it would
  destroy the project index. The new `hostlib_code_index_add_readonly_roots({roots, replace?})`
  builds each extra root into a parallel, read-only `IndexState` that lives
  beside the primary. `hostlib_code_index_query` now merges hits from every
  read-only root (each tagged with a `root` field; primary hits carry
  `root: null`), and `hostlib_code_index_read_range` falls back to the
  read-only roots so a symbol discovered in a dependency root can be read back.
  No mutating builtin (`version_record`, `reindex_file`, `rename_symbol`,
  locks) ever touches the read-only set — writes to a dependency-root path stay
  rejected exactly as before. Adding the same root twice is idempotent.
  Enables the deferred burin dependency-grounding wiring (burin #2403 follow-up).

- **gpt-oss reasoning capability rows for fireworks, deepinfra, and sambanova
  (#3318).** Added declarative reasoning-capability matrix rows so gpt-oss
  served through these providers correctly advertises and negotiates its
  reasoning channel.

### Changed

- **`scripts/release_gate.sh audit` is faster and surfaces failures up front.**
  `sync_language_spec` (the `spec/HARN_SPEC.md` -> `docs/src/language-spec.md`
  mirror writer) was being run in *both* the `docs-audit` and `grammar-audit`
  lanes, which run in parallel — duplicating ~72s of work and racing two writers
  on the same mirror file. It now runs only in `docs-audit`; `grammar-audit`'s
  `verify_language_spec` reads the canonical spec source directly and does not
  depend on the mirror. On failure, the gate now prints a `RELEASE AUDIT FAILED`
  summary at the TOP of the output naming the failing lane *and the specific
  failing sub-step* (e.g. `grammar-audit / verify_tree_sitter_parse`), derived
  from the unmatched `time_phase` banner, plus the last 40 log lines — instead
  of forcing a maintainer to scroll thousands of lines into the full per-lane
  log dump (which is still emitted afterward for deep debugging).

- **LLM retry/throttle layer is more robust under concurrency and network loss
  (#3360).** Three deterministic, unit-tested hardenings to the outbound-LLM
  retry path:
  - **Equal-jitter backoff.** Transient-error backoff was a fixed exponential
    (`250/500/1000/2000/4000ms`) with zero jitter, so concurrent same-key
    callers (e.g. `eval --concurrency K` alongside a live session) retried in
    lockstep and re-stampeded the provider. Backoff now uses AWS "equal jitter"
    (`wait = ceil/2 + rand(0, ceil/2)`, `ceil = 250 * 2^min(attempt, 4)`), which
    desynchronizes retries while avoiding the near-zero waits of full jitter. A
    small additive jitter is also layered on top of an honored provider
    `Retry-After` so identical `Retry-After` values do not resume in unison.
  - **Typed non-streaming send errors.** Streaming `req.send()` failures were
    already classified by reqwest error kind, but non-streaming send failures
    became a bare `"{provider} API error: {e}"` string that the retry layer
    had to re-classify by substring. Both paths now share one reqwest-kind
    classifier that tags timeouts/connection failures as typed
    `ErrorCategory::Timeout` / `TransientNetwork` at the source.
  - **Network-only circuit breaker.** A per-route, per-process breaker now opens
    after sustained `NetworkError`/`Timeout` failures (laptop disconnect, DNS
    failure, dropped link), fails fast for a short window, then admits a single
    half-open probe and closes on success. It deliberately does **not** react to
    429 (handled by the existing rate-limiter cooldown + `Retry-After`) or 5xx,
    so it only stops the retry budget from burning against a truly dead link.

### Fixed

- **gpt-oss / Harmony channel leaks no longer pollute conversation history
  (#3359).** On ~23% of gpt-oss-120b turns the provider fails to split its
  harmony channels and collapses the analysis reasoning plus the inline tool
  call into the assistant `content` (empty `reasoning` field, empty native
  `tool_calls`). The bare `{"tool":..,"arguments":..}` dialect emitted in that
  case is now recovered (the native-JSON parser accepts `tool` as a `name`
  alias, in both the acceptance gate and the extractor — previously the call was
  dropped and the loop saw a stall), and the persisted assistant turn is rebuilt
  into the canonical shape a non-leaked turn produces: structured `tool_calls`,
  the leaked trace moved to the private `reasoning` field (stripped from the wire
  by the prior-turn reasoning fix), and empty `content`. This stops the model's
  raw chain-of-thought — including "game the verifier" plans — from being
  re-serialized into every later request and wasting input tokens.

- **Strip prior-turn assistant `reasoning` from the openai-compat wire (#3319).**
  Echoing a previous turn's `reasoning` field back on the next request made
  Fireworks reject the call with a 400; the reasoning is now stripped from
  outbound openai-compat history so multi-turn loops do not break.

- **The tree-sitter-harn grammar now parses an attribute followed by a doc
  comment before a declaration**, e.g. `@complexity(allow)` on one line, a
  `/** ... */` (or `//`) comment on the next, then `pub fn ...`. Previously the
  external scanner emitted a line separator after the attribute's newline, the
  comment was lexed as `extras`, and the *second* newline (after the comment)
  arrived in a parser state that no longer accepted a separator before the
  declaration — producing a hard parse `ERROR` and failing
  `scripts/verify_tree_sitter_parse.py --strict`. The canonical lexer treats
  comments as trivia and `parse_attributed_decl` `skip_newlines()` swallows the
  whole run, so the construct is valid Harn; this was a tree-sitter grammar gap,
  not a malformed source file. Fixed by changing `attributed_declaration` to
  accept `repeat($._line_sep)` (rather than `optional`) after each attribute so
  the trailing separator on either side of the comment is absorbed. This was the
  v0.8.109 release `grammar-audit` blocker (it tripped on a vendored
  `.harn/packages/harn-slack-connector/src/lib.harn` whose `normalize_inbound`
  carried exactly this `@attr` + doc-comment shape). Added a corpus regression
  test.

- **`harn-vm` compiles again after the persistent-`imbl::OrdMap` dict migration.**
  The iterative value-teardown worklist (`value/recursion.rs::dismantle_values`,
  added to bound native-stack depth when dropping deeply nested values) called
  `into_values()` on an owned `DictMap` — but `DictMap` is now an
  `imbl::OrdMap`, which (unlike `BTreeMap`/`HashMap`) has no `into_values`.
  This was a hard `error[E0599]` that broke every Rust compile of `harn-vm`
  (the `Audit scripts`, `Harn conformance + audit`, and crate-packaging release
  gates all failed on it). Fixed by moving owned values out via the map's
  owning `IntoIterator` (`into_iter().map(|(_, v)| v)`), preserving the iterative
  teardown semantics.

- **Portal input validation (#3317).** Runs-page URL params now fall back to
  valid filters, sort order, page, and row-count values, and launch env
  overrides now reject non-object JSON and non-string values before submitting.

- **Structured judge/router calls no longer silently truncate into a dead-judge
  fall-through, and a truncation is now its own integrity category.** A
  `safe_structured_call` (judge / router / cheap-classifier) that went out with
  a tiny `max_tokens` budget truncated mid-object on a reasoning model — gpt-oss
  on Cerebras emits its structured output *inside* the reasoning channel, so the
  reasoning and the JSON share the same output budget — and the unparseable
  result was classified as a generic `missing_json` miss indistinguishable from
  a model that just returned prose. Two provider-generic fixes: (1)
  `safe_structured_call` now floors a structured call's `max_tokens` to 512 (it
  only RAISES an unset/too-small budget; an explicit larger value such as a
  1200-token rubric judge is untouched), so a small verdict object always has
  room to finish; live-probed against `cerebras gpt-oss-120b`, a historical
  `max_tokens: 180` call (which produced zero JSON, 180/180 tokens spent on
  reasoning) now returns clean bounded JSON at ~207 tokens with `stop_reason=
  stop`. (2) A token-limit truncation gets its own `error_category:
  "length_truncation"` on the result envelope (kept even after a failed repair
  pass) so a caller can detect a DEAD structured call — one that fell through to
  a deterministic grader without ever rendering a verdict — instead of laundering
  it as an ordinary abstention.

## v0.8.109

### Added

- **Exact `decimal` type for money and precise arithmetic.** A new `decimal`
  value type (96-bit base-10, up to 28–29 significant digits) backed by
  `rust_decimal`, so `decimal("0.1") + decimal("0.2")` is exactly `0.3` instead
  of the binary-float `0.30000000000000004`. Construct via the `decimal(value)`
  builtin (string/int/float/decimal; throws on un-parseable input rather than
  returning `nil`). Arithmetic (`+ - * / %`, unary `-`) promotes `int` operands
  exactly but refuses to mix with `float` (a compile-time error — binary float
  would corrupt exact values); `to_int`/`to_float`/`to_string` convert out.
  Equality and ordering are a clean island: `decimal` compares only against
  `decimal` (scale-insensitive, so `1.5 == 1.50`), and `decimal("1") == 1` is
  `false`. Decimals serialize across the host/JSON boundary as strings to
  preserve precision and bind natively to Postgres `NUMERIC`/`DECIMAL` columns.

- **Opinionated provider-route policy: gpt-oss on OpenRouter is now pinned to
  clean sub-providers, plus a compile-time footgun-validation gate.** OpenRouter
  routes `openai/gpt-oss-120b` across a ~17-upstream sub-provider lottery, and
  some upstreams mis-serialize the Harmony tool call even with reasoning ON
  (billed-noncommittal: 0 tool_calls), so the route was a runtime footgun even
  after the reasoning fix. Two declarative pieces close it: (1) a new
  `openrouter_provider_order` capability field (the allowlist counterpart to
  `provider_route_denylist`) materializes to the OpenRouter request body's
  `provider: { order: [...], allow_fallbacks: false }`, and the
  `openai/gpt-oss-*` OpenRouter row pins it to `["Cerebras", "Groq"]` — the
  upstreams that served Harmony tool calls cleanly in a live 2026-06-13 probe
  (order-pinned requests gave 0 billed-noncommittal; Together was flaky 1/3);
  (2) a data-driven footgun gate (`harn providers build-capabilities --check` /
  `make check-provider-capabilities`) that FAILS the build on known-footgun
  provider/model/config combos — a `reasoning_required_for_tools` route that
  also forces a tool task to reasoning-off, and a `reasoning_required_for_tools`
  OpenRouter route with no clean-sub-provider pin. The gate reads the
  capability matrix's own invariants (no hard-coded model-name patterns), so a
  new footgun route is caught the moment it forgets a pin. The blessed-vs-
  forbidden policy is documented in the `capabilities.toml` base shard.

### Changed

- **Documented the rate limiter's gross-token accounting.** The LLM rate
  limiter counts GROSS prompt tokens against TPM by design — prompt-cached
  tokens are intentionally NOT netted out, because provider TPM enforcement is
  on gross tokens regardless of cache hits (verified live against Cerebras
  gpt-oss-120b: with 6400/6482 prompt tokens served from cache, the per-minute
  token budget still decremented by the full gross prompt). Comment-only at the
  charge site in `rate_limit.rs`; no behavior change.

- **Gemini thinking-budget quirks moved from hard-coded Rust branches into the
  `capabilities.toml` declarative matrix.** `gemini.rs` previously decided
  whether a Gemini model supported a thinking budget, whether thinking could be
  disabled, and what the high/xhigh budget ceiling was via inline
  `model.contains("gemini-2.5")` / `model.contains("flash")` branches. Those
  facts are now declared alongside each model's other wire capabilities: a new
  `max_thinking_budget` capability field (Gemini 2.5 Flash 24576, Pro 32768)
  plus the existing `reasoning_disable_supported` (Flash can disable thinking,
  Pro cannot) and `thinking_modes` (effort support gates thinkingConfig). The
  provider now reads `capabilities::lookup("gemini", model)` and the
  per-model patterns live in the matrix, matching the
  `auto_reasoning_overrides` precedent. Behavior is identical (the unreachable
  speculative `robotics` branch — no such catalogued model exists — is the only
  dropped path, folded into the declared per-row flags). Verified by the
  existing `gemini_thinking_config_maps_from_typed_thinking` golden test.

- **VM dicts are now persistent.** `VmValue::Dict` is backed by a structurally
  shared `imbl::OrdMap` instead of a `BTreeMap`. Copy-on-write dict mutation —
  performed on every `dict[key] = value` / property assignment when the value is
  aliased (on the stack, in another local, or captured by a closure) — drops
  from an O(n) deep clone of every key and entry to an O(log n) path copy,
  removing the dominant allocation cost in mutation-heavy scripts. Dict
  ordering, equality, identity (`===`), iteration, and the full read/write API
  are unchanged.

### Fixed

- **gpt-oss (Harmony) now keeps reasoning ON for tool calls — kills the
  billed-noncommittal failure at its root.** gpt-oss performs tool calls
  *inside* the Harmony chain-of-thought channel, so disabling reasoning breaks
  tool calling entirely (live OpenRouter probe of `openai/gpt-oss-120b`:
  `reasoning {enabled:false}` → 0 tool_calls + null completion_tokens; `effort:
  low` / provider default → clean native tool calls). This is the *opposite* of
  the Qwen3 quirk (Qwen narrates tool intent in the reasoning trace and emits
  zero `tool_calls`, so Qwen needs reasoning OFF for tools), and #3303's retry
  was masking this self-inflicted misconfig. The fix is declarative, in the
  `capabilities.toml` family: a new `reasoning_required_for_tools` capability
  flag is set on every gpt-oss row (Together, Groq, Cerebras, and a newly-added
  OpenRouter `openai/gpt-oss-*` row that previously fell through to a
  reasoning-less catch-all), and `reasoning_policy` now refuses to resolve a
  tool-bearing task (agent/code/verify) to reasoning-off when that flag is set —
  flooring to the lowest supported effort instead — so no future auto default,
  capability override, or session pin can re-introduce the failure. The Qwen3
  reasoning-off-for-tools behavior is unchanged. Both quirks are now documented
  side by side in the capability matrix.

- **macOS-gated `#[cfg(target_os = "macos")]` warnings can no longer slip to
  `main` and break the release `prepare` build.** The Linux per-PR CI lanes
  never compile macOS-only code, so a stray unused import / dead_code /
  deprecation in a `target_os = "macos"` (or `cfg(any(macos, windows))`) path
  only surfaced on a contributor's Mac — historically at release `prepare`
  time under `-D warnings`, one error at a time (the v0.8.109 blocker was an
  unused `BTreeMap` import in `crates/harn-hostlib/tests/secret_store_os_native.rs`,
  a `#![cfg(any(macos, windows))]` test file Linux CI skips). Removed that
  import and added a path-routed `macos` CI lane (analogue of the existing
  `windows` lane) that runs `cargo clippy --workspace --all-targets -D
  warnings` on `macos-latest` for PRs touching macOS-gated process/sandbox/
  secret-store/CLI paths, plus unconditionally on push/merge — compiling even
  the cfg-gated *test* targets so the class fails the PR instead of the
  release.

### Security

- **Deeply nested values can no longer abort the process with a native stack
  overflow.** A script could build a value nested far deeper than the call
  stack tolerates (`x = [x]` in a loop adds no VM frames, so `max_vm_frames`
  never fired), then crash the whole host — `SIGABRT`, bypassing every runtime
  limit — simply by comparing (`==`), printing, `json_stringify`-ing, sorting,
  hashing (set/dict de-dup), or even just dropping it. The recursive value
  walks (equality, ordering, structural hashing, display, JSON) now grow the
  native stack on demand (the approach serde/rustc/syn take, via `stacker`), so
  deep-but-finite data completes instead of crashing; value teardown across the
  VM's slot/scope lifecycle is now iterative; and the `serde`-backed
  pretty-JSON / YAML encoders reject values past `max_value_depth` (1024) with a
  catchable error rather than overflowing. Mirrors `serde_json`'s
  deserialization recursion limit and CPython's recursion guards in its C-level
  `json`/comparison paths.

## v0.8.108

### Added

- `std/agent/best_of_n` adds `best_of_n_candidates(opts)` — a generic best-of-N candidate selector with an optional
  pre-generation spec-contract gate. The host supplies `generate` / `select_callback` / `spec_contract_callback`
  closures (it owns candidate generation, including any workspace reset, and the selection signal); Harn owns the
  advance / fallthrough policy. Host-callback errors degrade gracefully — a non-true spec contract aborts to a single
  candidate, a failing `generate` skips it, and a nil/out-of-range/erroring `select_callback` falls through to a
  deterministic candidate — so best-of-N is a safe overlay over plain single-attempt behavior.
- `std/context` gains `context_artifact_select_one(artifacts, options)` — a single-best context-artifact selector that
  hard-suppresses stale artifacts, scores candidates with a depth/altitude-aware path term (a deeper, more-specific
  directory scope outranks a shallower ancestor), and abstains on a score tie by returning
  `{reason: "ambiguous_context_artifact", candidates}` rather than injecting an arbitrary sibling. Complements the
  existing list-budgeting `context_artifact_select`.
- `std/fs` gains `fs_snapshot(paths, opts?)`, `fs_restore(snapshot_id, paths?, opts?)`, `fs_list_snapshots(opts?)`, and
  `fs_drop_snapshot(snapshot_id, opts?)` — thin pipeline-facing wrappers over the existing host
  `hostlib_fs_*` snapshot builtins. Pipelines can now checkpoint the workspace and roll a mutation back between
  independent retry attempts (the substrate for a verify-gated best-of-N agent loop). Snapshots are session-scoped:
  the session id defaults to the active agent session (`agent_session_current_id()`) so each conversation's snapshots
  stay isolated and are cleaned up on session close.
- Added three OpenAI-compatible LLM providers to the bundled catalog:
  `moonshot` (first-party Kimi K2 host), `deepinfra` (open-weight host), and
  `sambanova` (fast RDU inference). Each ships catalog rows, capability matrix
  entries, and short aliases (`kimi-direct`/`moonshot-kimi-k2.7-code`,
  `deepinfra-deepseek`/`deepinfra-qwen3.6`, `sambanova-deepseek`/`sambanova-llama`).
- `std/postgres/query` gains `nullable_uuid_text(name)` — the nullable counterpart to `uuid_text(name)`, rendering
  `CASE WHEN name IS NULL THEN NULL ELSE name::text END AS name` as a trusted projection fragment. It preserves SQL
  NULLs as JSON `null` instead of casting them to the string `"null"`, mirroring `nullable_timestamptz_json`, and
  accepts table-qualified names (`sessions.forked_from_session_id`). This replaces the hand-rolled `CASE WHEN … END`
  string concatenation that data-access modules wrote for nullable UUID/foreign-key columns.

### Changed

- **VM hot paths.** Local property and subscript assignment now compile to
  slot-addressed bytecode, builtin ID dispatch uses a hash-indexed side table,
  and ASCII string `len` / `count` paths avoid unnecessary scalar iteration
  while preserving Unicode behavior (#3295).
- **Allocator.** The `harn` binary now links [mimalloc](https://github.com/microsoft/mimalloc)
  as its global allocator by default, lowering per-allocation latency and
  fragmentation on the runtime's allocation-heavy copy-on-write workload.
  Opt out with `--no-default-features` (or omit the new default-on `mimalloc`
  Cargo feature) to fall back to the system allocator.
- **VM value size.** `VmValue` is now 24 bytes (down from 32). The two
  oversized inline payloads — `Range` (a `start`/`end`/`inclusive` triple) and
  `BuiltinRefId` (an id plus an `Arc<str>` name) — are boxed behind a shared
  pointer, so no variant inflates the common `Int` / `Float` / `List` / `Dict` /
  `String` shapes the interpreter copies on every push, pop, clone, and
  local-slot write.

### Removed

- Removed three dead codepaths: the unused `std/collections` helpers `store_stale` / `store_refresh` (zero call
  sites), and the dead `"adapter_shim"` `callsite_strategy` branch in `std/edit`'s `add_parameter` (it only ever
  returned "not yet supported"). Internal: `harn-cli`'s bundle signer now uses `hex::encode` instead of a duplicate
  local `hex_encode` helper.

### Fixed

- **LLM-layer reliability and correctness wave.**
  - *Cerebras/Groq/Together gpt-oss now use native tools.* The prior
    `native_tools=false`/json pin was a stale workaround for an
    "empty native streaming payload" defect that no longer reproduces; under
    json/text gpt-oss emits a `{"tool","arguments"}` dialect the fenced parser
    rejected, yielding zero parsed calls. Native is the measured-best (and only
    working) channel; verified live end-to-end through the streaming reassembler.
  - *Cerebras rate limits corrected to the Developer tier* (250 rpm / 250k tpm)
    so the proactive sliding-window limiter no longer throttles a paid key to
    the 5 rpm Free-Trial pin.
  - *429 retries now engage by default.* `DEFAULT_LLM_CALL_RETRIES` was `0`, so
    the existing `Retry-After` + per-route cooldown + bounded backoff never ran
    and a single 429 was a hard failure; the default is now `2`.
  - *Fenced-JSON tool parser accepts the `tool`/`arguments` dialect* as aliases
    for `name`/`args` (canonical wins), recovering text-route calls from models
    that emit that shape.
  - *Tool-body failures are surfaced as failures, not ok.* A host-bridge
    `{ok:false}` / `{status:"error"}` / `{error:...}` result was laundered into
    success; the terminal-status reader now classifies these as failures.
  - *LLM response cache key now includes tools, structured-output schema, and
    stop sequences.* Two calls differing only in those fields previously
    collided and returned the wrong cached response.
  - *Char-boundary-safe tool-argument error previews.* Malformed-JSON previews
    sliced by byte index and could panic mid-UTF8-codepoint.
  - *Agent stall/loop test suite now runs in CI* (`make test-agent-scripts`),
    and two stall tests that had rotted (un-gated by any CI target) are fixed.
- **`parse_set_cookie` now trims whitespace around the cookie name and value
  before validation (#3297).** A `Set-Cookie: name = value` header was
  previously dropped entirely (the un-trimmed `name` failed `valid_cookie_name`)
  and parsed values kept a stray leading space, diverging from the
  `Cookie`-header parser, which already trims both sides.
- **Billed-noncommittal provider completions are now retried instead of
  terminating the agent loop (#3303).** When an upstream bills output but
  serializes the tool call onto the reasoning channel only — finishing *clean*
  yet committing no dispatchable tool call or answer (`upstream contract
  violation`) — the response/transport parsers *throw* that error. The agent
  loop's `Err` arm routed it through the generic terminal-error classifier,
  which did not match its signature, so it was never retried and the loop
  hard-broke as a silent `provider_error`, bypassing all completion/verify
  machinery. The thrown shape now folds onto the **same bounded
  empty-completion retry budget** the zero-token empty-completion path already
  uses (floored at one retry for real providers, unaffected by the global
  fail-fast `DEFAULT_LLM_CALL_RETRIES`) — one unified retry path, no new
  parallel mechanism. Once that budget is exhausted on a chronically-broken
  upstream, the loud thrown error is surfaced unchanged and still names the
  `upstream contract violation` so the eval layer can classify it as infra, not
  model capability. Complements #3219 (zero-token empty retry) and #3292
  (errored-but-actionless `Ok` retry), closing the remaining *thrown*
  unproductive-completion shape.
- **Agent session redo checkpoints survive rejected transcript-budget writes.** Failed
  transcript mutations that leave the transcript unchanged no longer discard redo
  state captured by the previous rollback.
- **Errored / tool-call-less generations are retried instead of advancing the
  loop on a broken turn.** A cheap model sometimes returns a generation that
  ends with a provider error (`stop_reason == "error"`) after only *narrating*
  an intended tool call in its text/reasoning (e.g. "We need to make edit to
  create tests/...test.cpp...") while emitting ZERO parsed tool calls. The agent
  loop used to advance on that turn and reply with a generic
  `no_progress`/`stall_diagnostics` nag that never told the model its turn
  errored, so after a few such turns the model gave up having written nothing.
  This is distinct from the zero-token empty-completion retry (those have
  non-zero reasoning tokens, so the zero-token predicate misses them).
  `observed_llm_call` now treats an errored-but-actionless `Ok` generation as a
  transient provider hiccup and retries it within the same bounded
  empty-completion budget (no change to the global retry default). When the
  budget is exhausted and the loop does advance, the stall detector emits
  cause-specific feedback ("your previous turn ended with a provider error and
  emitted no tool call — re-emit the intended tool call") instead of the generic
  no-progress nag, while genuine no-tool-intent stalls keep the existing nag.

## v0.8.107

### Added

- `complementary_reviewer` now returns a stable, machine-readable
  `fallback_code` whenever it falls back to the author model: one of
  `unknown_author_family`, `no_diff_family_within_price`,
  `no_diff_family_serverless`, or `all_diff_family_excluded`. Callers that
  require a structurally independent reviewer can branch on `fallback` plus
  `fallback_code` to hard-fail a degraded self-review instead of parsing the
  human-readable reason prose.

### Fixed

- `rules.search` and related path-based rule builtins now skip non-UTF8 files
  such as `.DS_Store` instead of failing the whole run; scanner tests also pin
  Scala sealed-trait symbol discovery.

## v0.8.106

### Added

- `harn providers export` / `harn providers validate` accept `--capabilities-overlay <path>` (capabilities.toml
layout): overlay-declared private/local models can claim structured capabilities (native tools, vision, prompt
caching, reasoning modes) in the exported artifact instead of relying on legacy `capabilities` tags or post-export
patching. The serve runtime honors the same data through the manifest `[capabilities]` section, so exported and
served catalogs agree; new `harn_vm::llm::capabilities::parse_capabilities_toml` parses without mutating thread
state (#3267)
- Add Moonshot Kimi K2.7 Code to the OpenRouter provider catalog and
  normalize its fixed-parameter/tool-choice quirks in the OpenAI-compatible
  transport.
- Added current Qwen 3.7, KAT-Coder-Pro V2, Step 3.7 Flash, Together DeepSeek
  V4 Pro, Together GLM 5.1, and Together MiniMax M2.7 routes to the provider
  catalog, including live pricing, aliases, and capability rows for
  provider-specific streaming/tool behavior.
- Agent loop: evidence-aware repair loop in the stall detector
  (`stall_diagnostics.repair_aware`, default off). Tracks a signature-keyed
  current-failure model instead of a blind repeat counter: the same failure
  signature advancing across repair turns — even across intervening edits that
  do not change the error — trips a strategy-shift nudge grounded in the actual
  diagnostic after `stuck_same_diagnostic_after` turns, while a productive edit
  that changes the error resets the streak (so the `fail, edit, fail, edit,
  fail` edit-between-retest thrash is caught, and legitimate progress is never
  flagged). Forces a post-edit re-verify through the existing
  `verify_completion` entrypoint before continuing or stopping, carries a
  `current_failure` summary on a stuck hand-back, and clears it on a successful
  termination so a clean `done` never reports a stale failure. Surfaced
  (default-off) on the tool-using defaults preset's `repair_diagnostics` block.

### Changed

- Corrected the MiniMax M2.7 OpenRouter mirror price to the current
  $0.25/Mtok input / $1.00/Mtok output, and added the MiniMax M2.5 OpenRouter
  mirror (`minimax/minimax-m2.5`, 205K context, tools/streaming). The M2.5
  mirror inherits native-tool capabilities from the existing
  `minimax/minimax-m2*` capability rule, so no new capability row was needed.

### Fixed

- **Cross-cutting bug sweep, round 2 (13 root-cause fixes).**
  - *Compiler — nested assignment wrote to the wrong target.* Every
    multi-segment assignment path collapsed to its root variable and final
    accessor: `a.b.c = v` silently wrote `a.c`, `a["b"]["c"] = v` wrote
    `a["c"]`, `xs[0][1] = v` replaced `xs[1]`, `m.list[0] = v` wrote
    `m["0"]`, and `p[0].x = v` threw. Nested paths are now desugared into a
    scope-contained chain of temporaries that reads down, assigns the leaf,
    and writes back up — preserving `let` immutability at the root.
  - *Compiler — compound subscript indices evaluated twice.*
    `xs[idx()] += 1` called `idx()` once for the read and again for the
    write; side-effecting indices are now hoisted into a temp and evaluated
    exactly once.
  - *Compiler — invalid assignment targets compiled as silent no-ops.*
    `get_thing().x = 99` compiled and did nothing; it is now a compile
    error naming the unsupported target form.
  - *Compiler — module-level statements were silently dropped in pipeline
    mode.* Assignments and expression statements between a binding and a
    pipeline declaration never executed (`var n = 1` then `n = 2` left the
    pipeline seeing 1, and a module-level `log(...)` never printed). All
    module statements now run in source order before declarations, matching
    script mode.
  - *Runtime — index assignment on unsupported types was a silent no-op.*
    `s[0] = "Z"` on a string and `xs["a"] = 9` on a list left the value
    untouched and reported nothing; both now raise the same class of type
    error the read path and property path already raised.
  - *Codegen security — `${…}` interpolation injection.* The `harn try`
    scaffolding and the composition/crystallize code generators escaped
    quotes and backslashes when embedding values in generated Harn source,
    but not `${`, so a value containing `${host_call(...)}` executed as
    code when the generated program compiled. All generators (and
    `harn fmt`) now share one round-trip-tested
    `harn_lexer::escape_string_literal`.
  - *ACP — `session/cancel` skipped authentication.* The only session
    method without the `reject_unauthenticated` gate, letting any
    unauthenticated peer cancel a running session. Guard added, plus a
    drift test asserting every `session/*` dispatch arm checks auth.
  - *LLM providers — `Retry-After` dropped on errors.* Gemini, Azure
    OpenAI, Bedrock, and Vertex hardcoded `None` for the rate-limit
    backoff hint that Anthropic/OpenAI/Ollama already threaded through;
    all providers now share one header-extraction helper.
  - *LSP — find-references returned whole declarations.* References for a
    function, parameter, or binding highlighted the entire declaration
    span; results now narrow to the identifier token, sharing the same
    refinement rename already used.
  - *Typechecker — subscript inference ignored intersections.* `x["a"]` on
    an intersection type inferred gradual (accepting any assignment) while
    `x.a` resolved the member type; the subscript path now mirrors the
    property path.
  - *CLI — divergent TOML escaping produced invalid TOML.* The connector
    scaffolding's escape helper skipped `\n`/`\t`/`\r` (and all other
    control characters), so generated specs with multi-line values did not
    parse; consolidated with `harn rules`' copy into one shared helper that
    escapes the full control range. Three identical `html_escape` copies
    and a JUnit `xml_escape` were also unified.
  - *Tests — one flaky test failed as sixteen.* A scheduler-sensitive
    rate-limit assertion poisoned the shared LLM env lock and cascaded into
    15 unrelated failures. The env lock now recovers from poisoning and the
    flaky yield-count assertions are deterministic diagnostic timeouts.
  - *Tests — the two rate-limit reset scopes are now decoupled.* The
    rate-limiter registry is process-global, but reset ran at a single
    level: `reset_llm_state` wiped the whole registry, which (a) corrupted a
    sibling rate-limit test's counters when an unrelated parallel test reset
    mid-assertion, yet (b) was *required* for the sequential CLI test runner
    to clear a leaked retry-after cooldown between cases. The two needs now
    have two functions: `reset_llm_state` (parallel-safe; scrubs only leaked
    runtime overrides) and a full-wipe `reset_rate_limit_registry` reached
    via `reset_thread_local_state`, which only runs in sequential /
    separate-process contexts. This fixes a conformance fidelity test that
    hung for its full per-test timeout when a prior test's mock-429 cooldown
    leaked onto the paused-clock LLM call. Two contract tests pin each half.

## v0.8.105

### Added

- **Providers overlay: `[suppress] routes` section (#3267).** New
  `[suppress] routes = ["provider:model_id", ...]` section hides broken or
  product-unsupported routes from the exported/served catalog artifact — the
  model row, its aliases, and any recommendation variant derived from it —
  without forking the baseline. Selectors split on the first colon (model ids
  may contain colons, e.g. Ollama image tags). Combined with whole-row
  `models` replacement this also expresses route renames (define the new id,
  suppress the old), letting embedders drop post-export catalog patching.

### Changed

- **One glob matcher for the whole workspace (#3265).** Seven divergent
  `glob_match` implementations (invariant globs, metadata scan, model
  config/capability patterns, hook routing, mock prompt patterns, skills
  triggers) consolidated into the new `harn-glob` crate with three explicit
  contracts (`match_path`, `match_name`, `match_prose`). Model and hook
  patterns now share full glob syntax (`*`, `?`, `[..]`), and path globs
  uniformly treat `**/` as matching zero directories.

### Fixed

- **One wire id per streamed tool call (#3270).** The streaming
  `ToolCall(Pending)` announcement and partial-arg updates from the SSE
  transport used a `tool-{provider_id}` id while the executed
  `Pending → InProgress → Completed/Failed` lifecycle ran under the bare
  provider id, so every executed call appeared under two ids on the ACP
  wire and id-keyed consumers double-counted tool calls. The announcement
  now uses the provider id verbatim, and when a provider omits the id the
  synthesized fallback is written into the dispatched call so the
  lifecycle keeps the same id.
- **`metadata` directory-scan `**` patterns match again (#3265).** The
  scanner's hand-rolled matcher compared the second `*` in `**/*.rs`
  literally, so recursive `**` scan patterns never matched; scan patterns now
  go through the shared glob matcher (bare substring patterns keep their
  historical behavior).
- **Cross-cutting validation fixes (#3269).** Glob `**/` patterns now stay on
  directory boundaries and name globs honor brace alternates; schema validation
  and export now enforce enum/`uniqueItems` collection constraints and emit
  valid JSON/OpenAPI for `any`, `set`, and nullable unions; MCP file upload
  `accept` matching honors wildcards and extensions; OAuth dynamic registration
  accepts loopback IPv6 and 127/8 redirect hosts; and MCP OAuth resource
  audiences are matched exactly.
- **Cross-cutting bug sweep (10+ root-cause fixes).**
  - *Compiler:* a function or program body whose bytecode exceeded 64 KiB
    silently truncated its `u16` jump operands and jumped somewhere wild at
    runtime; every finalized chunk is now guarded and oversized bodies fail
    compilation with a clear "split it into smaller functions" error.
  - *Typechecker:* optional subscript access on a shape (`x?["k"]`) dropped
    the optional flag, so an unknown key inferred gradual instead of `nil`
    and mismatched assignments passed silently.
  - *Stdlib:* the regex builtins (`regex_match`, `regex_replace`,
    `regex_captures`, `regex_split`) stringified an explicit `nil`
    pattern/text/replacement to the literal string `"nil"` and compiled —
    and matched — it as a real regex; `nil` now takes the same fallback as a
    missing argument (mirroring the existing flags guard).
  - *Formatter:* `harn fmt` relocated comments to the end of the file from
    three positions — trailing comments on inline match arms
    (`1 -> { x } // c`), trailing comments on dict/struct-literal entries,
    and full-line comments inside multiline dict/list literals. All three now
    stay anchored, and a literal with interior comments keeps its multiline
    shape.
  - *Linter:* `comparison-to-bool` flagged optional-chained presence tests
    (`d?.enabled == false`) as "redundant" and its autofix rewrote them to
    `!d?.enabled`, which flips the branch when the chain is nil; the rule now
    skips operands that can be nil via optional chaining. Also,
    `prefer-optional-shorthand` diagnostics reported byte-based columns,
    overshooting on lines with multibyte characters; columns are now
    character-based.
  - *Spec:* `spec/HARN_SPEC.md` claimed `if`/`else` and `match` execute
    without creating a child scope, contradicting both the docs and actual
    behavior (branch/arm bodies are block-scoped); the scope-creation section
    now matches the implementation.
  - *MCP test fixtures:* `FakeServerBehavior::HeaderMismatch` was declared
    but never implemented; the fake RC server now validates `Mcp-Method` /
    `Mcp-Name` headers via the production negotiation helper and answers
    `-32600` with the spec error shape.
  - *Docs/release:* `docs/src/embedding-rust.md` pinned `tag = "v0.8.57"`
    (~46 releases stale); the pins are refreshed and
    `release_gate.sh prepare` now bumps them automatically each release.

## v0.8.104

### Added

- **`region` field on LLM routing chain links + catalog introspection.** Routes
  can now carry a `region` so region-aware providers (e.g. Bedrock, Vertex,
  Azure OpenAI) resolve to the correct regional endpoint, and the value is
  surfaced through catalog introspection alongside the other route fields.

### Fixed

- **Agent loop no longer terminates silently on provider empty-name tool calls
  (burin-code#2120).** When a provider returns a malformed tool call with an
  empty / whitespace-only name alongside valid sibling calls in the same turn
  (observed live as a turn carrying `[look, "", look]`), the loop now drops only
  the nameless call, keeps and dispatches the valid siblings, and injects
  parse-guidance so the model re-emits a named call next turn — instead of
  passing the empty-name call through unguarded and ending the loop, which read
  as a model give-up (`result=INCOMPLETE, outcome_kind=null`) but was a pure
  harness dispatch failure that corrupted eval-meter accounting.

## v0.8.103

### Added

- **Non-blocking `host_call("process", ...)` primitive.** `spawn` returns a
  handle immediately; `poll`/`wait`/`kill`/`release` observe progress, drain
  captured stdout/stderr, signal-kill, and free retained output. The spawned
  child goes through the same sandbox-gated command builder and command-policy
  preflight as `process.exec`. The handle registry is capped, evicts terminal
  entries first, and never silently drops a still-running child.
- **Project scanner fingerprints more languages as `primary_language`.** Scala,
  Kotlin, Swift, Elixir, Zig and friends are now recognized by extension and
  build-tool signals instead of being dropped as `unknown`, so downstream
  consumers (e.g. per-language authoring grounding) can key off an accurate
  `primary_language`.
- **One `harn serve site` route can now be both a WebSocket upgrade and an
  SSE/stream route.** A routed `pub fn` may carry `@ws` *and* `@stream`
  together: after the route runs auth + `@scopes` admission once, the site
  adapter sniffs the request head's `Upgrade: websocket` / `Connection:
  upgrade` headers (before any extractor or the body is consumed) — a genuine
  handshake takes the `SiteStreamProvider::upgrade` path, while every other
  request falls through to the `SiteStreamProvider::open` (SSE/stream) path
  instead of being refused with a 4xx. This is the seam the harn-cloud gateway
  `/acp` carve-out needs (one route, two transports, one admission). The
  previous `@ws`∧`@stream` conflict diagnostic (`HARN-SRV-016`) now fires only
  for `@ws`∧`@raw` (a handshake carries no request body, but `@raw` exists to
  buffer one). `@ws`-only and `@stream`-only routes are unchanged: a non-upgrade
  request to a `@ws`-only route is still refused with the correct 4xx by axum's
  extractor, and the `SiteStreamProvider::upgrade` default impl still returns
  `426 Upgrade Required` so embedders that implement only `open` keep compiling.

### Fixed

- **`ast.parse_errors` now flags tree-sitter grammar-limitation cascades.** When
  an `ERROR` node starts on line 1 and spans essentially the whole file — the
  fingerprint of well-formed source the grammar simply can't model, e.g.
  tree-sitter-scala 0.26 on Scala 3 indentation-based `match`/`case` — the
  response sets a top-level `cascade: true` and marks the offending error
  `spans_full_source: true`. Edit-validation gates use this to stop
  hard-rejecting correct creates/replaces on a grammar blind spot, instead of
  reporting a misleading `syntax error: line 1: ...`. Localized syntax errors are
  unaffected.

### Security

- **The ACP `code` (ActAuto) coding mode now honors an embedder-supplied
  sandbox config instead of discarding it.** Previously, an embedder's
  `AcpSandboxConfig` (e.g. from `sandbox.json` / `BURIN_SANDBOX_CONFIG`) was
  loaded and validated but silently dropped in `code` mode, because the ActAuto
  tier short-circuited to "no per-turn policy" before the config was applied —
  so the default coding agent ran with no filesystem scoping, no OS
  confinement, and no egress guard even when the embedder asked for them.
  ActAuto's *approval* semantics (no human approval gate) are now decoupled
  from *OS confinement*: when the embedder provides a non-default sandbox
  config, `code` mode applies it as a `Worktree`-level OS sandbox (seatbelt on
  macOS, Landlock on Linux) seeded from the config's read-only roots and
  process presets, while keeping ActAuto's `network` side-effect ceiling. **No
  change to the no-config default** — a session with no sandbox config behaves
  exactly as before (ambient policy, no per-turn ceiling).
- **The SSRF private-address egress guard is now installed on the ACP
  agent/serve path** (not just `harn run`) whenever the embedder opts into
  sandboxing. While active it blocks outbound requests to private / loopback /
  link-local / cloud-metadata addresses (e.g. `169.254.169.254`, `127.0.0.1`,
  `10.x`, `192.168.x`) while leaving legitimate public traffic — model API
  calls, `web_search` / `web_fetch` to public hosts — fully allowed. The
  metadata endpoint stays blocked even with the loopback escape hatch. Local
  model servers on loopback are reached via the documented
  `HARN_EGRESS_ALLOW_LOOPBACK=1` / `egress_policy({block_private:"off"})` opt
  out. With no sandbox config the guard is not installed, so default egress is
  unchanged.
- **The `DeveloperToolchains` sandbox preset now covers JVM/iOS toolchain
  caches** so a `Worktree`-confined build does not break Gradle, Maven,
  CocoaPods, Xcode, or Kotlin/Native. Read+write access is granted to
  `~/.gradle`, `~/.m2`, `~/.konan`, `~/Library/Caches/CocoaPods`, and
  `~/Library/Developer/Xcode/DerivedData` when the policy allows workspace
  writes (read-only otherwise), mirroring the existing `UserTemp` cache-write
  pattern.

## v0.8.102

### Fixed

- **OpenRouter route-around for broken upstreams plus a billed-no-op contract
  guard.** Capability rows gained a data-driven `provider_route_denylist`
  (`Vec<String>`) that the OpenAI-compatible request builder materializes into
  the OpenRouter request body's `provider.ignore` array (merged and deduped,
  preserving existing entries). The `qwen/qwen3.6*` openrouter row denies the
  `Ambient` upstream, which billed reasoning tokens and then finished with
  `finish_reason: "stop"` and empty `tool_calls` — narrating the intended tool
  call on the reasoning channel and serializing it to no wire field — while
  `Parasail` / `AtlasCloud` / `AkashML` serve the identical request natively.
  Native tools stay on; only the broken upstream is routed around (the
  `require_parameters` knob does not help). As a deterministic backstop across
  every OpenAI-compatible route (streaming and non-streaming), a clean-finish
  turn that billed output, offered tools, captured no tool call or tool-search
  block, and produced fewer than 24 committed answer characters now fails loudly
  as an "upstream contract violation" instead of returning a silent empty
  success. A `length`/truncation finish, a normal tool call, a substantive text
  answer, and a tool-less prompt are all left untouched.

## v0.8.101

### Added

- **`@ws` WebSocket-upgrade marker for `harn serve site`.** A `pub fn`
  carrying the bare `@ws` attribute alongside its HTTP route is answered
  by the embedder's `SiteStreamProvider` instead of dispatching into the
  VM — the WebSocket sibling of `@stream`. After the same admission (the
  `SiteAuth` hook plus the route's `@scopes`, enforced *before* the
  connection is handed off), the site adapter extracts the
  `WebSocketUpgrade` and calls the new `SiteStreamProvider::upgrade`,
  which completes the handshake and drives the socket; the `101` is
  forwarded verbatim. The trait method has a default `426 Upgrade
  Required` implementation, so existing providers keep compiling. A
  non-WebSocket request to a `@ws` route is refused with the correct 4xx,
  and a `@ws` route declared without a provider fails the router build
  loudly. New diagnostics `HARN-SRV-014..016` cover a `@ws` marker with
  arguments, without a route, or conflicting with `@stream`/`@raw`.

### Fixed

- **Linux process sandbox no longer fails to spawn when a read-root resolves
  to a regular file.** `process.exec` under the `worktree`/`os_hardened`
  profiles built a Landlock `PATH_BENEATH` rule with directory-only access
  rights (`READ_DIR`, the `MAKE_*`/`REMOVE_*` family, `REFER`) even when the
  root was a *file* — e.g. the package-manager-config preset's `~/.gitconfig`,
  `~/.cargo/config.toml`, or `~/.npmrc`. On a kernel with Landlock support the
  kernel rejects such a rule with `EINVAL`, surfacing as
  `host_call process.exec: Invalid argument (os error 22)`. The directory-only
  bits are now stripped for non-directory rule targets, so the remaining
  file-applicable rights (`READ_FILE`/`EXECUTE`/…) are still enforced and the
  spawn succeeds.

## v0.8.100

### Added

- **`exec_opts` / `exec_at_opts` — an options form for the convenience exec
  builtins (#3240).** `.harn` callers that need to pass `env`, `cwd`, or a
  `timeout` no longer have to drop to the verbose `host_call("process.exec",
  {mode: "argv", argv: [...], env, env_mode})`. The new builtins take an argv
  list plus an options dict: `exec_opts(["git", "clone", a, b], {env: {...},
  timeout: 30000})` and `exec_at_opts(dir, ["git", ...], {env_mode: "replace"})`.
  Options are `{env?, env_mode?, cwd?, timeout?}` (`timeout`/`timeout_ms` in
  milliseconds), and the result is the same `{stdout, stderr, status, success}`
  shape as `exec` plus `timed_out`/`duration_ms`. The positional
  `exec("ls", "-la")` / `exec_at(dir, ...)` forms are unchanged.
- **`std/postgres/query` gains `raw_sql(template, params?)` and
  `named_raw_sql(name, mode, template, params?)` (#3238).** These build query
  records from literal SQL with **no** `{name}` scanning, so brace-heavy SQL —
  JSON paths (`#>>'{}'`), array literals (`'{a,b}'::text[]`), and
  `jsonb_set(.., '{path}', ..)` — no longer needs `{{`/`}}` doubling. Parameters
  are positional (`$1`, `$2`, ...). The existing `sql(...)` / `named_sql(...)`
  named-placeholder behavior, including `{{`/`}}` escaping and `unsafe_sql(...)`
  fragments, is unchanged.
- **Server embedders can install a process-lifetime shared Postgres pool
  registry (#3234).** `harn_vm::install_shared_pool_registry()` (re-exported as
  `harn_serve::install_shared_pool_registry()` under the `vm-postgres` feature)
  opts a long-lived server into reusing one `sqlx` connection pool per distinct
  connection identity across requests and worker threads — instead of opening a
  fresh pool on every `harn serve` dispatch. Pools are keyed on the **resolved**
  connection identity (host/port/database/credentials, SSL mode, application
  name, replica set, and every pool-shaping option), not on a caller-supplied
  alias, so two callers never share a pool across different credentials,
  databases, or pool shapes. Safe across tenants because harn scopes RLS
  per-transaction, never per-pool/per-connection. Strictly opt-in: the CLI
  one-shot path never installs the registry and behaves exactly as before.
  (Shipped in the v0.8.99 series; documented here.)
- **One-shot `@job` drivers can override or disable the retry policy (#3242).**
  `harn_serve::run_job_once_with_options(..)` accepts a new
  `harn_serve::JobRunOptions` whose `retry_override` replaces the `@job`'s
  declared `@retry`/`retry:` policy for that run only. `JobRunOptions::fail_fast()`
  runs a single attempt with no backoff sleep — the natural choice for one-shot
  CLI and failure-path test drivers, which previously inherited the `@job`'s
  multi-hour `svix` backoff and could hang for an hour-plus on an erroring job.
  Strictly opt-in: `run_job_once` / `run_job_once_with` (and the server path)
  are unchanged and still honour the `@job`'s declared policy when no override
  is given.

### Changed

- **`@retry(...)` and `@job(retry: {...})` now share one validator (#3236).**
  The standalone job modifier and its compact dict alias are documented as
  equivalent, so their recognized keys and backoff strategies
  (`svix`/`linear`/`exponential`) are now a single source of truth — the two
  surfaces can no longer drift apart in what they accept or reject.
- **`process.exec` no longer clears the child environment by default when `env`
  is supplied (#3240).** Previously, passing an `env` dict without an explicit
  `env_mode` defaulted to `env_mode: "replace"`, which called `env_clear()` and
  silently dropped PATH/HOME/etc. — so `env: {ONE_VAR: "x"}` wiped the rest of
  the environment. The default is now `env_mode: "merge"`: the provided keys are
  overlaid on the inherited parent environment. Full replacement is still
  available by passing `env_mode: "replace"` explicitly. An unrecognized
  `env_mode` is now rejected instead of being treated as a non-replace mode. No
  in-tree caller relied on the clear-by-default behavior.

### Fixed

- **Harn's git stdlib is now non-interactive by default (#3241), so a `.harn`
  git operation can no longer hang on a credential or host-key prompt in a
  TTY-less context (`harn serve`, `@job`, CI).** Every git subprocess the stdlib
  spawns — the receipt builtins (`git.fetch`/`git.push`/`git.rebase`/
  `git.worktree_create`/…) and the `std/worktree` helpers — now runs with
  `GIT_TERMINAL_PROMPT=0`, an empty `GIT_ASKPASS`/`SSH_ASKPASS`, and an
  `ssh -oBatchMode=yes` transport, so git fails fast instead of blocking on an
  interactive prompt. The guard is merged (`env_mode: "merge"`), so inherited
  `PATH`/`HOME` and credentials supplied via env, `.netrc`, credential helpers,
  or a pre-loaded ssh-agent continue to authenticate push/clone/fetch exactly as
  before — only *interactive prompting* is disabled. `std/worktree` helpers take
  an optional trailing `options` dict to re-enable interactive git when wanted.
- **`run()` toolchain-PATH normalizer is now Windows-correct (#3239).** The
  `HARN_RUN_TOOLCHAIN_PATH` normalizer builds the child `PATH` with
  `std::env::join_paths`/`split_paths`, so prepend/override/replace use the
  platform separator (`;` on Windows, `:` on unix) instead of a hardcoded `:`,
  and the PATH env key is matched case-insensitively on Windows (`Path`/`PATH`)
  so an existing caller-supplied key is updated in place rather than duplicated.
  The toolchain-PATH unit-test fixtures were also made platform-correct so the
  "Rust on Windows" CI job is green.
- **`publish-release` no longer fails on the transient release-commit
  ahead-of-tag state on a `main` push (#3237).** The guard that errors when
  `Cargo.toml` is ahead of the latest tag now tolerates the brief window between
  a Release PR landing on `main` and the `vX.Y.Z` tag being pushed, instead of
  surfacing a spurious red run.

### LLM

- **`llm(structured)` degrades to text transport on a route-specific
  structured-output 400 (#3244).** When a provider/route rejects a native
  structured-output request with a 400 that is specific to that route's
  structured-output support, the call now falls back to the text transport for
  that request instead of failing, recovering the response without losing the
  result.

### Tooling

- **`pub fn` docstring lint is now SOTA-default (#3245).** The `missing-harndoc`
  rule (HARN-LNT-024) is **opt-in** via `[lint] require_docstrings = true` in
  `harn.toml` instead of warning on every undocumented `pub fn`, and the stdlib
  metadata contract (HARN-STD-101) shrank from five required tags to two —
  `@effects` + `@errors`. `@allocation` is retired entirely (parser ignores it;
  the field is gone from `StdlibMetadata` and `harn graph --json`),
  `@api_stability` is optional (absent ⇒ stable), and `@example` is optional
  everywhere: LSP hover and `harn graph --json` now synthesize a usage example
  from the type signature when no hand-written one exists (`derived_example` in
  graph JSON, a labeled "derived from signature" block in hover). ~2,900 lines
  of boilerplate tags were stripped from the embedded stdlib. **Breaking:** no
  compatibility shims; projects relying on the old default docstring requirement
  must set `require_docstrings = true` to keep it.

## v0.8.99

### Added

- Added `harn serve worker` for `.harn` `@job` files, including
  scheduled job activation, durable queue consumption, and standalone
  `@retry(...)` job modifiers.
- **`run()` can prepend the repo-declared toolchain to the child-process
  `PATH` (mise/asdf path normalizer).** When `HARN_RUN_TOOLCHAIN_PATH` is set,
  the `run_*` tools detect a repo's declared interpreter versions
  (`.tool-versions`, `.mise.toml`, `.ruby-version`, `.nvmrc`) at or above the
  command's cwd, resolve them via `mise where` / `asdf where`, and prepend the
  resolved bin dir to the child process's `PATH` only — so the command sees the
  version the repo declares instead of a stale system interpreter. Strictly
  declaration-gated (no version file ⇒ `PATH` byte-identical), process-scoped,
  delegates all version resolution to mise/asdf (no per-language table in harn),
  never prepends a path that does not exist, and logs a one-line override notice.
  Generalizes burin-code #2136's keg-only-Ruby hardcode; default OFF and
  per-session disable-able.

### Fixed

- **Fenced-JSON tool parsing now recovers recognizable near-miss tool-call
  shapes (#3176).** Tool-like fence drift such as `tool_code`, `tool python`,
  and tilde `tool` fences now dispatches valid `{ "name": ..., "args": { ... } }`
  calls with a protocol violation, while bare JSON calls and legacy
  `<tool_call>` markup surface actionable guidance instead of disappearing into
  prose.
- **OpenRouter DeepSeek aliases.** `deepseek/deepseek-chat` and tool-capable
  DeepSeek R1 routes now accept native tools instead of falling through to the
  unmatched model default (#3177).
- **OpenAI-compatible prompt-cache requests now honor `cache: true` on explicit-cache
  routes (#3197).** OpenRouter-routed Claude emits a top-level `cache_control`
  breakpoint, Alibaba-backed Qwen/DeepSeek explicit-cache routes mark the last
  message content block, Gemini explicit-cache routes do the same, and existing
  message or tool cache markers survive request normalization without duplicate
  insertion.
- **harn-serve hosted site handlers now preserve binary HTTP bodies through
  dispatch (#3214).** Request `body` is no longer UTF-8-lossy for binary
  payloads; handlers use `body_base64` when `body_kind` is `base64`, and
  `http_reply(..., bytes, headers)` now returns byte-exact responses with
  caller-controlled headers.
- Unsupported sampling-param warnings (`"top_k" is not supported by provider …,
  ignoring` and the seed/penalty/cache siblings) now emit once per
  `(param, provider, model)` instead of on every LLM call, so they no longer
  flood agent and eval logs.
- **SSRF egress policy controls documented (#3172).** The HTTP builtin
  reference and language spec now document the `egress_policy` axes
  (`block_private`, `allow_loopback`) and their environment hatches; the
  resolved-IP `NetPolicy` runtime already shipped in earlier patches.

## v0.8.98

### Added

- **`harn serve site` now takes an embedder auth hook with tenant/claims plumbing (#3212).** New
  `harn_serve::SiteAuth` trait (`async fn authenticate(&self, parts, route) -> SiteAuthOutcome`),
  installed via `SiteServerConfig::with_auth(Arc<dyn SiteAuth>)`, runs on every matched route before
  the body is read. `Allow(SiteAuthContext { tenant_id, scopes, context })` threads the tenant into
  the dispatch (trust records, spans, `harness.tenant.id()`), checks the route's `@scopes` against
  the hook-granted scopes (refusing with the canonical `forbidden` envelope), and installs the
  opaque embedder `context` as an ambient scope a `HostCallBridge` can read via
  `harn_serve::current_auth_context()` for the duration of the dispatch; `Deny(response)` returns
  the embedder-shaped response verbatim. `AuthRequest` gains a `granted_scopes` field so transports
  that own credential resolution compose with the dispatch-level scope check, and `CallRequest`
  gains the `auth_context` carrier. Without a hook, behavior is unchanged. Fixes #3212.
- **`harn serve site` can now host-stream response bodies (SSE/chunked) on `@stream` routes
  (#3213).** The dispatch boundary is JSON in/out, so a `.harn` handler's response is always
  buffered — wrong for SSE. A routed `pub fn` carrying the new bare `@stream` attribute therefore
  never buffers the request body and never dispatches into the VM: after admission (the `SiteAuth`
  hook plus the route's `@scopes`, which the adapter now enforces itself for stream routes since no
  `CallRequest` exists to back-stop them) the adapter calls the embedder's new
  `harn_serve::SiteStreamProvider` (`async fn open(&self, route, auth: Option<&SiteAuthContext>,
  request: Value) -> Response`), installed via `SiteServerConfig::with_stream_provider`, and
  forwards its streaming `Response` unbuffered — keep-alive and client-disconnect propagation come
  from the returned `Sse`/stream body. The provider receives the same request-head dict a `.harn`
  handler would see (`method`, `path`, `route`, `path_params`, `query`, `headers`, `client_ip`,
  `remote_addr`), minus any body. Declaring a `@stream` route without a provider fails at
  router-build time; a malformed `@stream(...)` or one without an HTTP route is diagnosed
  (`HARN-SRV-009` / `HARN-SRV-010`). Non-stream routes are unchanged. Fixes #3213.
- **`harn serve site` now carries raw/binary request and response bodies losslessly on
  provider-answered routes (#3214).** The plain dispatch boundary is a JSON envelope (a utf8-lossy
  `body` plus `body_base64`), which is wrong for binary surfaces like `.harnpack` downloads, CAS
  blobs, and multipart pack publishes. The response half needed no new machinery: a `@stream`
  route's `SiteStreamProvider` may return *any* axum `Response` — including a buffered binary body
  with its own `Content-Type`/`Content-Disposition` — and the adapter forwards it verbatim,
  byte-exact (now documented and proven by test). The request half is the new bare `@raw` route
  marker: like `@stream` it skips the VM and is answered by the same provider behind the same
  admission (the `SiteAuth` hook plus the route's `@scopes`, enforced before the body is read), but
  the request body *is* buffered — up to the configured body limit, larger payloads get the
  canonical 413 — and handed to the provider as exact bytes. `SiteStreamProvider::open` accordingly
  gains a `body: Option<Bytes>` parameter (`None` on `@stream` routes, `Some(bytes)` on `@raw`
  routes); the provider parses multipart itself from the head dict's `content-type` boundary.
  Declaring a `@raw` route without a provider fails at router-build time; a malformed `@raw(...)`,
  one without an HTTP route, or one combined with `@stream` is diagnosed (`HARN-SRV-011` /
  `HARN-SRV-012` / `HARN-SRV-013`). Plain routes are unchanged. Fixes #3214.
Claude Fable 5 (`claude-fable-5`, released 2026-06-09) in the provider catalog: model entry
($10/$50 per MTok, 1M context, SWE-bench Pro 80.3), a `fable` alias, `claude-fable-*` /
`claude-mythos-*` capability rules (adaptive-only thinking, no assistant prefill), and
generation parsing for the fable/mythos families so the Opus 4.7+ request guards
(sampling-param strip, adaptive-thinking rewrite, prefill removal, always-on thinking) apply.

### Fixed

- **Native tool_format: tool calls emitted as chat-template text markup are no longer silently
  dropped (#3220).** Cheap native-format models (observed live: qwen3.6 under long context) sometimes
  fall back to their chat template's TEXT rendering of a tool call —
  `<tool_call><function=edit><parameter=action>...</parameter></function></tool_call>` or the
  `<invoke name="edit">` attribute spelling — which the native parse path ignored entirely, so the
  turn read as a natural completion and the run ended with the call lost. The tagged text parser now
  rescue-parses this markup into real calls (schema-typed parameter values: string params keep raw
  bytes verbatim, non-string params parse as JSON; unknown tools and truncated `<parameter>` blocks
  surface precise parse errors instead of dispatching partial values; prose mentions and fenced
  examples never fire — the opener must be line-anchored outside a markdown fence). In native
  sessions the existing `native_tool_fallback` contract then takes over: the default `reject` policy
  injects "re-issue through the native tool channel" feedback and the loop continues; `allow`
  dispatches the rescued call. A zero-call turn whose markup could not be promoted (parse error,
  feedback queued) no longer reads as a native natural completion — the loop holds the turn open so
  the parse guidance reaches the model.
- **harn-serve:** the `@budget(pg_queries)` integration test
  (`pg_query_budget_rejects_third_query_as_429_via_dispatch`) now registers the
  `pg.*` builtins it exercises via a `harn-vm`/`postgres` dev-dependency, so
  `cargo nextest run -p harn-serve` is green in isolation. Previously the test
  only passed under a `--workspace` run that happened to unify `harn-vm/postgres`
  on; running the crate alone hit `Undefined builtin: pg_mock_pool`. The library
  build stays lean (`default-features = false`, no `sqlx` in the non-dev graph).
- **Empty-args native tool calls now get cause-named feedback, and the
  provider `stop_reason` rides the observability transcript.** Two halves of
  the same blind spot (burin-code#2121, observed live on the OpenRouter
  native route: 13/165 edit calls arrived with literally `{}` arguments while
  the model authored 549–5,056 output tokens those turns): (1) the
  `provider_call_response` record in `llm_transcript.jsonl` dropped
  `LlmResult.stop_reason` — the transport layer captured `finish_reason` on
  both the streaming and non-streaming OpenAI-compatible paths, but transcript
  mining saw `stop_reason=None` on every provider response, so truncation
  analysis was blind; the record now carries it. (2) A tool call that arrives
  with empty (`{}`/null) arguments and fails required-parameter validation was
  misdiagnosed as `"missing required parameter(s): path"`, sending the model
  into re-call loops. The agent loop now threads the turn's provider stop
  reason into dispatch, and the feedback names the actual cause: on a length
  truncation the model is told its arguments were TRUNCATED by the output
  limit (re-issue shorter / split the change); on a clean stop it is told the
  provider dropped the arguments (re-issue the same call in full). The
  dispatch envelope and inner tool result also carry a machine-readable
  `cause` (`empty_arguments_truncated` / `empty_arguments_dropped`) so host
  harnesses can classify the fault without string-matching. Calls that did
  deliver (incomplete) arguments keep the precise missing-parameter message.
- **Judge verdicts no longer carry trailing JSON junk.** When a structured completion/step judge
  emits sloppy JSON (double commas, run-on key/value pairs) that the structured-call repair layer
  salvages, the captured `verdict` string could include trailing junk — observed live in
  `judge_decision` events as `continue",,` and `continue",  "reasoning":`. `__judge_classify_verdict`
  now normalizes the captured verdict to its leading token (cut at the first JSON structural
  character), so stored/emitted `judge_decision` / `step_judge_decision` verdicts are clean tokens
  and a mangled PASS token (`done",,`) classifies as a pass instead of being wrongly vetoed.
  Multi-word prose verdicts without JSON junk pass through unchanged.
- **The OpenAI-compatible transport now honors the model catalog's
  `stream_timeout`, surfaces mid-stream failures, and retries zero-token empty
  completions.** Three gaps turned provider stalls into silent empty agent
  turns (observed live in Burin Code eval-meter work: an OpenRouter call hung
  133s and returned `output_tokens=0` as a "success"): (1) the catalog's
  `stream_timeout` (seconds) was projected into config dicts but consumed by
  no transport — it now feeds the shared whole-request deadline
  (`explicit timeout option > HARN_LLM_TIMEOUT > stream_timeout > 120s
  default`) for every provider on the common `resolve_timeout` seam, both
  streaming and non-streaming, so slow local models with `stream_timeout =
  900.0` get their budget and hung remote calls are bounded; (2) a mid-body
  SSE read failure (including that deadline firing mid-stream) was silently
  swallowed, returning a truncated zero-token success — it now surfaces as
  the same transient stream-error class other timeouts use, so the existing
  retry machinery picks it up; (3) a wire-level "success" carrying zero output
  tokens, no content, no thinking, and no tool calls is now retried once
  built-in (more with `llm_retries`) as a transient provider hiccup, with an
  `empty_completion_retry` observability entry and `EmptyCompletionRetry`
  trace event; if it stays empty after the budget, the result is returned
  unchanged. Token-cap truncations (`stop_reason` length/max_tokens) are
  excluded from the retry, and mock/fake providers only retry on explicit
  opt-in so scripted tests stay deterministic.

## v0.8.97

### Added

- **The `@job` one-shot driver can now register embedder-defined host builtins (#3207.)** New
  `harn_serve::run_job_once_with(.., configure: impl FnOnce(&mut Vm))` hands the embedder the
  fully-built job VM after the standard stdlib + store/metadata registration and before the job
  entrypoint runs, so a host builtin (e.g. a `sandbox_exec` bridge) can coexist with — or override —
  the standard ones and be callable from the `@job` `.harn` code. `run_job_once` is unchanged and
  delegates to the new variant with a no-op closure. Fixes #3207.
Added `std/eval/agreement`: deterministic, I/O-free cross-checked-success math for eval ledgers — the reusable
counterpart to `std/eval/stats`. Exposes `agreement_decision` (the ">=2 independent judges must agree, with at least one
independent re-execution among them" rule) and `cohen_kappa` (inter-judge agreement statistic), so eval clients can drop
their own hand-rolled agreement math.
Added `estimate_cost_usd` and `realized_trial_cost_usd` to `std/eval/stats`: cache-aware token→USD cost estimation
(cache-read/write tokens are billed at their own rates and not re-charged at the full input rate) plus cached-replay
realized-cost accounting. Lets eval harnesses drop their own hand-rolled LLM cost math.

### Fixed

Release tooling no longer reflows already-published `CHANGELOG.md` sections.
`make lint-md` previously linted the assembled `CHANGELOG.md` (and `CHANGELOG-pre-*.md`
archives) under MD013 line-length, so long lines in published `## vX.Y.Z` sections were
flagged and rewrapped during a release — tripping the retroactive-edit guard. Those
machine-assembled, append-only files are now excluded from markdownlint; the
`changelog.d/*` fragments are still linted at the source.
- **Agent-loop no-progress feedback now respects native tool mode.** Native-tool
  runs are nudged to use the provider tool channel instead of receiving
  text-mode `<tool_call>` and `<user_response>` syntax.

## v0.8.96

### Fixed

- Agent tool feedback: a recoverable schema/argument-validation rejection (a tool
  call missing a required parameter, or a malformed empty tool name) now returns a
  retry-positive `invalid_arguments` result that coaches the model to re-call the
  tool with the named correction, instead of the `permission_denied` "Do not retry
  the same call" denial body. The denial body is now reserved for true
  policy/permission denials. Cheap models were giving up after one fixable mistake
  — observed across ~26 recent eval transcripts as a false-FAIL pass-rate
  deflation.

## v0.8.95

### Removed

- `std/agent/prompts`: removed the unused `action_required_feedback`,
  `action_turn_nudge`, and `protocol_violation_feedback` prompt entrypoints
  (their `*_prompt` functions, registry/catalog/override entries, and
  `.harn.prompt` exemplar files). They had no in-tree caller — the live tool-
  call repair path uses the parametric `parse_guidance` prompt — and the
  `protocol_violation_feedback` exemplar hardcoded a text-format `<tool_call>
  name({...})` shape that does not apply to json/native tool-format sessions.

### Fixed

- `harn-vm` tool-call parser feedback is now precise about what went wrong and
  how to fix it, so cheap coding models stop re-emitting the same broken turn:
  - Source/test code emitted where a tool call was expected (`it(...)`,
    `expect(...)`, `describe(...)`, `assertServiceCount(...)`, …) no longer
    reports a misleading `Unknown tool 'it'`. The feedback now names the real
    cause — code outside a heredoc/`content` envelope — and tells the model to
    wrap it.
  - The "Unknown tool" available-tools list is no longer capped at 20 names
    (which could hide the very tool the model needed). It lists every tool, and
    appends an explicit `…and N more` only for a pathologically large registry —
    never silently truncating. The highest-frequency misses (`read`, `write`,
    `list`, `search`, …) now carry a canonical alias hint, e.g. `read` →
    `look({ intent: "read" })`. Genuine close-miss typos still get the
    `Did you mean '<tool>'?` suggestion. Applies to both the bare-TS and
    native-JSON tool-call parsers.
  - A denied/permission-gated tool result now carries an actionable `next_step`
    ("do not retry the same call; make progress with allowed tools, or ask for
    permission") instead of a bare `{"error":"permission_denied"}`.
  - Object-literal tool-call parse errors now include a short `Raw:` preview of
    the offending span (mirroring the native-JSON parser), so the model can tell
    which of several on-screen calls failed.
- `harn-vm` observation-mask compaction no longer shreds structured failure
  detail. Masking a large tool output (`default_mask_tool_result`) used a
  weaker, divergent filter than the microcompact path and dropped assertion-
  value lines (`left:`/`right:`/`expected:`/`actual:`/`got`/`want`), rustc
  continuation lines (`-->`, `= help:`, numbered source rows, `^` carets), and
  `Lnnn:` failing-line markers — so the model re-read a summary with the
  actual-vs-expected values removed. There is now ONE shared failure-signal
  filter (`is_failure_signal_line`) used by both the microcompact and
  observation-mask paths; the mask preserves those failure lines (bounded)
  alongside the first-line preview.
- **Egress NetPolicy CIDR/IP allow & deny rules now match resolved host IPs,
  not just URL literals (#3174).** A rule like `deny 203.0.113.0/24` or
  `allow 10.0.0.0/8` previously applied only when the request URL contained a
  literal IP, so a CIDR denylist could be bypassed with a DNS name and a CIDR
  allowlist wrongly rejected hostnames that resolve into the allowed range. The
  IP/CIDR rules are now evaluated against the host's resolved addresses in the
  off-runtime egress pre-check (clean typed `EgressBlocked`) and re-enforced at
  connect time by the `GuardedResolver`, which pins the connection to the same
  checked address — closing the DNS-rebinding window. Literal-IP, hostname, and
  `*.suffix` rules are unchanged.

## v0.8.94

### Fixed

- Lint: `cyclomatic-complexity` (HARN-LNT-002) and `naming-convention`
  diagnostics now underline just the keyword + declaration name
  (`fn run_agent`, `struct Bad`) instead of carpeting carets across the
  entire function/type body.
- **Agent tool-call parser no longer silently drops valid calls in two cases.**
  A single stray/unmatched backtick in model prose used to flip the bare-call
  scanner's inline-code flag for the rest of the response, suppressing every
  later bare `name({ ... })` tool call and stalling the agent loop; the flag now
  resets at each newline (Markdown inline code never spans lines). And the
  native-JSON fallback now accepts the flat OpenAI on-the-wire envelope whose
  `arguments`/`parameters` value is a JSON *string*
  (`{"name":"read","arguments":"{\"path\":\"a\"}"}`, common from local
  llama.cpp/vLLM/Ollama OpenAI-mimic templates) — the acceptance gate previously
  required an object and dropped the call even though the extractor already
  decoded the string.
- `harn-vm` agent loop: an empty or malformed tool-call name no longer aborts
  the whole run. `host_agent_dispatch_tool_call` previously threw a runtime
  error on a blank `tool_name`, terminating `agent_loop` — so a single malformed
  function-call element from a model (common on some native-tool/MoE providers)
  could wipe an entire run. The empty/malformed-name case now routes through the
  existing recoverable denied-tool path, re-injecting actionable retry feedback,
  exactly as an unknown-but-named tool already does.
- `harn-vm` stdlib `write_file`/text edits: the primary (no-overlay) write
  path is now crash-safe. It previously used `std::fs::write`, which opens the
  destination `O_CREAT|O_TRUNC` and truncates it before any byte is written, so
  a failure or process kill mid-write (ENOSPC/EDQUOT/EIO) left the file empty
  or partial and the original content unrecoverable. Writes now go through a
  sibling temp file that is flushed, fsynced, and atomically renamed over the
  target, leaving the original untouched on any failure.
- AST edit builtins (`ast.apply_node`, `ast.batch_apply`,
  `ast.insert_at_anchor`) no longer corrupt invalid-UTF-8 bytes. The read path
  decoded the whole file with `String::from_utf8_lossy` and the callers wrote
  the decoded buffer back, so any non-UTF-8 byte anywhere in the file (e.g. a
  Latin-1 byte or a `\x80` in a comment or byte-string) was silently rewritten
  to the 3-byte U+FFFD encoding — even in regions the edit never touched. The
  edit pipeline now reads, parses (tree-sitter over raw bytes), splices, and
  writes raw bytes, so bytes outside the edited span pass through verbatim.
  Lossy decoding is retained only for display-only previews/diagnostics and the
  read-only `ast.search` text bindings.
- Structured-output truncation is now detected for Gemini/Vertex responses,
  which report `MAX_TOKENS` (uppercase). `llm_call`'s structured-output error
  path previously used a case-sensitive `length`/`max_tokens` literal match and
  mislabeled a truncated Gemini response as "did not contain parseable JSON".
  It now reuses the canonical, case-insensitive `is_length_truncation`
  classifier (single source of truth shared with the agent/ACP path).

## v0.8.93

### Added

- **Eval packs can now be discovered from installed packages (#3124).** Package
  eval discovery includes the root package plus materialized dependencies under
  `.harn/packages/<alias>/`, so `harn test package --evals` and root
  `eval_pack://...` trigger handlers can run published eval packs through the
  existing package-manager install path.
- **`.harn` worker/job execution surface (#3171).** A `pub fn` annotated
  with `@job("name")` is now a worker entrypoint that runs on harn-serve.
  `harn run --as-job <file.harn> --job <name> --request <req.json>
  [--result-out <out.json>]` runs it once against a JSON request, delivers
  the request to the handler as `event.provider_payload.raw`, and prints
  the value the job returns as a JSON report — the drop-in shape for
  read-request → do-work → emit-report workers. The job is lowered into a
  trigger binding and dispatched through the existing trigger dispatcher,
  so retry (`@job("name", retry: { max: 3, backoff: "exponential" })`),
  dead-letter, per-dispatch `@budget(...)`, scopes, and cancellation are
  inherited from the dispatcher rather than re-implemented. `@schedule` and
  `@queue` attributes are parsed for the forthcoming `harn serve worker`
  daemon. Malformed forms surface `HARN-SRV-004..008` diagnostics.

### Changed

- **Compiler constant-pool building.** The bytecode compiler now indexes
  constants while emitting chunks, avoiding quadratic duplicate scans in
  generated scripts with many literals.
- Harmonized `gpt-oss` to a single tool-format default and dropped the stale
  heredoc `text` pins left on the local devstral / llamacpp-qwen rows after the
  fenced-json default flip. Previously the same `gpt-oss` model resolved three
  ways: cerebras and groq pinned `preferred_tool_format = "native"` while
  together inherited the new global `json` default — a correctness bug. All
  three `gpt-oss` capability rows now inherit `json` (the cerebras/groq rows drop
  their `native_tools`/`preferred_tool_format = "native"` pins so they match
  together). native is the evidenced-bad direction here: `gpt-oss` native
  streaming tool-calls have returned empty payloads in evals; `json` is
  structurally delimiter-safe and beats heredoc `text`.
- Dropped the explicit `preferred_tool_format = "text"` pins on the llamacpp
  `*qwen3.6*` / `*qwen3*` and the llamacpp + ollama `devstral-small-2*`
  capability rows so they inherit the global `json` default like their siblings.
  The qwen rows keep `reserved_tool_call_token = true` (the remap still applies
  whenever a heredoc `text` pin re-selects the tagged format; json's ` ```tool `
  fence already sidesteps the reserved `<tool_call>` token). devstral has no
  reserved-token constraint, so there was never a structural reason for the
  heredoc pin. Confirmed live on the local-qwen3.6 `:8001` route (json parses
  delimiter-soup `write_file` content clean; heredoc leaks the `<<EOF`
  delimiter); the devstral rows apply the same structural fix (not locally
  reachable, backed by the audit's local-qwen3.6 json 3/3 vs heredoc 0/3).

### Fixed

- **Debugging and test-result edge cases.** Conditional breakpoint fallback
  evaluation now reports unknown or non-numeric conditions instead of silently
  firing, JUnit duration parsing saturates hostile timestamps instead of
  panicking, HTTP download byte counts no longer wrap, and `to_int` now follows
  the documented bool and invalid-float behavior.
- **`pg_migrate` now recycles prepared-statement caches after DDL, and a few
  `std/postgres` bind/setting edge cases are tightened.** (1) After a migration
  runs DDL, a pooled connection could reuse a cached query plan whose result
  type the DDL changed and fail with `cached plan must not change result type`
  (SQLSTATE `0A000`). `pg_migrate` now clears the pool's cached statements (and
  the per-slot OID describe cache) once after applying any migration, so the
  next query re-prepares cleanly. (2) An out-of-range integer bound into a
  narrow (`int4`/`int2`) column now surfaces a stable `numeric_out_of_range`
  (SQLSTATE `22003`) diagnostic instead of a raw message; in-range integers bind
  and round-trip correctly. (3) A `nil` value in `pg_transaction(settings)` is
  rejected instead of being silently set as the literal text `"nil"`.

### Security

- **Outbound HTTP now has a DNS-resolving SSRF/egress guard, on by default
  under `harn run` (#3172).** A new private-address classifier blocks loopback,
  RFC 1918, link-local (including the `169.254.169.254` cloud-metadata
  endpoint), broadcast, unspecified, multicast, documentation, CGNAT
  (`100.64/10`), and benchmark (`198.18/15`) ranges, plus the IPv6 analogues
  and IPv4-mapped addresses. URL pre-checks classify literal IPs synchronously
  and resolve host names off the runtime, and a connect-time `reqwest` DNS
  resolver re-validates every resolved address so DNS rebinding cannot reach a
  private target after the check. `http_*`, `http_download`, `http_stream_open`,
  and `harness.net.*` all inherit the guard through the shared client path.
  Configure via `egress_policy({block_private: "private"|"off", allow_loopback:
  bool})` or `HARN_EGRESS_BLOCK_PRIVATE` / `HARN_EGRESS_ALLOW_LOOPBACK`; the
  private-block always wins over `allow` rules, and block reasons name only the
  host so request secrets are never echoed.
- **`std/postgres` transaction settings are now allowlisted, and raw Postgres
  error detail no longer leaks to `.harn` callers.** Two hardening fixes to the
  Postgres hostlib. (1) `pg_transaction(settings)` previously ran `set_config`
  for *any* GUC key, so `.harn` code could set a privileged backend GUC
  (`role`, `session_authorization`, `is_superuser`, `search_path`, …) to escape
  row-level security at the Postgres level. Settings are now restricted to the
  application's own `app.*` namespace (which RLS policies are written against)
  plus the benign `statement_timeout` / `lock_timeout` /
  `idle_in_transaction_session_timeout` GUCs; any other key is rejected with a
  clear error. (2) A failing query/execute previously surfaced the raw Postgres
  message — which embeds schema, relation, column, and constraint names — to the
  caller. The hostlib boundary now maps database errors to stable, schema-free
  categories (e.g. `unique_violation (SQLSTATE 23505)`) and keeps the full
  detail in server-side tracing only.

## v0.8.92

### Added

- Added an `eval-suite-regression-notifier` trigger example that runs a scheduled
  eval pack, gates it with `std/eval/stats::regression_gate`, and posts Slack
  verdicts only on regression or improvement flips.

### Fixed

- **Fixed nil-bearing Postgres queries whose params Postgres cannot infer
  failing on the new describe-then-bind path.** The describe-then-bind fix learns
  per-slot OIDs via `prepare_with(sql, &[])`, which forces Postgres to infer
  *every* `$n` from query structure alone. For a query with a genuinely
  ambiguous slot (e.g. `SELECT $1 IS NULL`, real failure
  `could not determine data type of parameter $3`), the describe probe itself
  errored and the whole query failed — even though the pre-describe-then-bind
  behavior (bind the `nil` as a `text` NULL) worked. Inside a `pg_transaction`
  the failed probe also aborted the transaction (`current transaction is
  aborted`), so even a same-connection fallback could not recover. The describe
  probe is now **best-effort**: when `prepare_with` fails, Harn no longer
  propagates the error — it returns no per-slot OIDs (cached, since the failure
  is deterministic per SQL structure) and the bind path falls back to the legacy
  `text` NULL, which is strictly ≥ the old behavior. When the probe runs inside a
  caller transaction it is wrapped in a `SAVEPOINT _harn_describe_probe`
  (released on success, rolled back on failure) so a failed probe never taints
  the caller's transaction — the ambiguous query succeeds via the fallback and
  the transaction stays usable for subsequent statements and commit. The
  all-non-null fast path and the successful-describe path are unchanged (no extra
  round-trips when describe succeeds; the savepoint is added only around the
  in-transaction probe).

## v0.8.91

### Added

- Added cron-bindable eval-pack trigger handlers via `eval_pack://...`,
  including trigger budget, retry, replay, DLQ, cancellation, ledger, docs, and
  conformance coverage.
- Added a third tool-calling format, `tool_format = "json"` (fenced-JSON), a
  delimiter-safe peer of `text` (tagged/heredoc) and `native`. Each call is one
  ```` ```tool ```` fenced block wrapping a single
  `{ "name": ..., "args": { ... } }` JSON object (N blocks for N calls); the
  body channel is a JSON string, so backticks, `<<EOF`, `}`, and `</tool>` ride
  inside file content with no escaping and the line-anchored close fence never
  collides with a content ```` ``` ````. This root-cause-fixes the
  native/text `<<EOF` heredoc-leak class (`syntax error: line 0: <<`) by
  deleting the heredoc body channel entirely. New parser
  `crates/harn-vm/src/llm/tools/parse/fenced_json.rs` (selected when
  `tool_format == "json"`), new `agent.tool_contract_json` prompt and json
  paradigm/body-hint, format plumbing across the parity gates and capability
  resolution with a compile-time exhaustive `tool_format_channel` guard, and a
  conformance classifier that recognizes a fenced-JSON emission as
  `parseable_harn_text_tool_call`. (A follow-up change promotes `json` to the
  global default text tool-calling format; see the separate changelog entry.)

### Changed

- **Internal: unified the Markdown table-cell pipe escaping used by the CLI
  report commands.** The eval summary, provider-matrix, provider-support, and
  diagnostics-catalog commands each carried a private copy of the same
  `|`-escaping helper; they now share `crate::format::escape_md`. No behavior
  change to any rendered report.
- Made fenced-JSON (`tool_format = "json"`) the GLOBAL DEFAULT text tool-calling
  format, replacing heredoc (`text`). A text-channel model with no
  `preferred_tool_format` pin — and the `auto`/omitted resolution path — now
  resolves to `json` in both the runtime (`llm_config::default_tool_format`) and
  the agent stdlib (`std/agent/options` fallback). NATIVE-channel models are
  unchanged. The flip is STRUCTURAL, not just measured: a JSON string can't
  carry a raw newline, so a content delimiter like `<<EOF` never collides with
  the call wrapper, deleting the heredoc `line 0: <<` leak class — so it
  generalizes to unmeasured models, not only the local-qwen3.6 /
  gemini-2.5-flash / deepseek rows that swept a clean 1.0/1.0/1.0
  compliance/parse-determinism/expressiveness bench. Heredoc (`text`) remains a
  selectable format and a per-model `preferred_tool_format = "text"` override
  (the reverse safety valve) for any model that later regresses below baseline.
  `json` is now also a first-class alias `tool_format` (validated against
  text-channel tool support), the structural validator enforces text-protocol
  well-formedness for `json` identically to `text`, and the local-qwen3.6 ollama
  route drops its `text` pin to inherit json (json's ```tool fence sidesteps the
  reserved `<tool_call>` token that forced the heredoc pin).

### Fixed

- Fixed the code-index symbol graph accumulating duplicate Module→Module
  `IMPORTS` edges on every incremental reindex. `link_imports` re-runs over the
  whole workspace after each per-file reindex, but `rebuild_file` only clears the
  reindexed file's edges, so every reindex appended another copy of every
  still-valid import edge between unchanged files — despite the documented
  "idempotent" / "add-only" contract. The edge set grew without bound and Cypher
  `IMPORTS`/`IMPORTED_BY` traversals returned duplicate rows, wasting the row
  budget and polluting code-index grounding. The relink is now idempotent.
- Fixed named per-tool `tool_budgets` (e.g. `{edit: 1}`) being silently
  unenforced for live coding-agent eval packs. The in-process executor reports
  tool usage only as a per-call `sequence` array (no `by_tool` map), so the
  budget checker could never resolve a named tool's count and skipped the limit
  entirely. The checker now falls back to counting occurrences in `sequence`, so
  a configured per-tool budget is enforced regardless of executor summary shape.
- **Tool middleware optional parameter injection.** `tool_inject_param(...,
  {required: false})` now also marks the injected parameter fragment optional,
  preventing provider-facing tool schemas from accidentally requiring stripped
  middleware-only fields such as `_nl_intent`.
- Fixed `pg_advisory_xact_lock` / `pg_with_advisory_lock` failing for string and
  `{class, instance}` keys: the blocking path bound the two-part key as `int8`,
  asking Postgres for a nonexistent `pg_advisory_xact_lock(int8, int8)` overload
  (`function ... does not exist`). The key halves are now bound as `int4` to hit
  the real `(int4, int4)` overload, matching the already-correct
  `pg_try_advisory_xact_lock` path. String keys (`pg_with_advisory_lock(db,
  "migrations", ...)`) and dict keys were the common lock-by-name path, so this
  affected most advisory-lock callers.
- **Fixed dynamic `nil` Postgres binds in all contexts via describe-then-bind.**
  A dynamic `nil` has no static Rust type, so the long-stable fallback bound it
  as `None::<String>` (Postgres TEXT). That poisoned sqlx's per-connection,
  SQL-keyed prepared-statement cache (a later non-text value at the same `$n`
  slot failed with `invalid byte sequence for encoding "UTF8": 0x00`) and was
  rejected outright against non-text typed columns/casts (`column is of type
  integer but expression is of type text`). The previously-attempted OID-0
  ("let the server infer") alternative broke mixed nil + non-null queries with
  `incorrect binary data format in bind parameter N`. Now, **only when a query
  actually contains a `nil`**, Harn binds every `nil` as a typed NULL carrying
  the server-described OID for that slot while non-null params keep their natural
  binary encodings. The per-slot OIDs are obtained by describing the SQL once and
  **caching the result keyed by the SQL string** — Postgres infers each slot from
  the query structure, so the OID list is stable per SQL and never needs
  invalidation. After the first nil-bearing execution of a given SQL there are
  **zero** extra round-trips: subsequent nil-queries hit the OID cache (no
  describe) and execute as a **non-persistent** statement, so they never poison
  sqlx's SQL-keyed prepared-statement cache and never need to clear it. The
  all-non-null fast path (and its warm statement cache) is completely unchanged,
  and a representative nil-query's steady-state p99 latency is within ~1.02x of
  the same query bound without a nil. This fixes `nil` into typed columns, cache
  poisoning across NULL-then-non-null on a pooled connection, and mixed
  nil/non-null params in `INSERT`/`WHERE`/`COALESCE`/`CASE`/multi-row `VALUES`,
  without the per-query describe + full-cache-clear of the initial fix.

## v0.8.90

### Added

- Added live-verify eval pack cases with a generic executor command interface,
  verify-command checks, expected output validation, and trial-level ledger
  resume.

### Changed

- Refactored `harn eval coding-agent` fixtures into portable live-verify eval-pack cases
  with trial ledger resume while preserving existing coding-agent reports.

### Fixed

- Fixed the direct Anthropic provider sending `tool_choice` as a bare string
  (the OpenAI wire shape), which Anthropic rejected with HTTP 400
  (`tool_choice: Input should be an object`) and broke tool-using agent loops on
  `--provider anthropic`. Harn's tool-choice modes are now mapped to Anthropic's
  object form (`auto`/`any`/`none`/specific-tool), and OpenAI-style
  `{"type":"function",...}` and bare-name inputs are normalized too.
- **Reverted the v0.8.89 "untyped NULL (OID 0)" Postgres `nil` bind.** Binding
  `nil` as an unspecified-type NULL (OID 0) so Postgres infers the slot type
  from context is incompatible with sqlx's binary wire protocol: when a query
  mixes a `nil` param with non-null typed params, Postgres re-infers the whole
  parameter-type list from the OID-0 slot during `Parse`, and the inferred
  types no longer match the client-declared OIDs sqlx encodes the non-null
  params with — yielding `incorrect binary data format in bind parameter N` /
  `insufficient data left in message` (and `could not determine data type` in
  genuinely ambiguous contexts). `nil` once again binds as `None::<String>`
  (Postgres TEXT), the long-stable behavior. The narrow cache-poisoning /
  typed-column cases the OID-0 change targeted are handled at the query layer by
  binding a concrete cast (`$n::text::int`, a `::text` cast, or a non-null
  sentinel) — the correct place to disambiguate a NULL's intended type.
- Fixed the Postgres hostlib binding a non-finite `Float` (NaN/Infinity) raw:
  a direct `float8` bind stored NaN/Infinity (which breaks downstream JSON), and
  a non-finite float on the jsonb path serialized to a silent JSON `null`.
  `pg_query`/`pg_execute` (and the introspection/advisory helpers) now reject a
  non-finite float — directly bound or nested in a list/dict — with a clear
  error before it reaches the database. Finite-float binds are unchanged.

## v0.8.89

### Added

- **Eval packs now have a durable trial-level ledger (#3119).** `eval_pack_run`
  resumes exact `(suite, model, split, commit, case, case_fingerprint,
  harness_config_fingerprint, trial)` matches from the sqlite event log,
  reports all-skip reruns honestly, refuses fingerprint-mismatched resume rows,
  and exposes `eval_ledger_*` builtins for reading, appending, prior-commit
  lookup, and resume planning.
- **`pg_migrate` gained an SQLx-compatible ledger mode (`ledger: "sqlx"`).**
  The Postgres builtin can now read and write SQLx's own `_sqlx_migrations`
  table byte-for-byte: it keys migrations off the integer version prefix of
  each filename, sorts ascending by that numeric version, applies only
  forward files (`*.up.sql` / `*.sql`, skipping `*.down.sql`), records the
  same `version, description, success, checksum (SHA-384), execution_time`
  rows SQLx does, and takes SQLx's per-database advisory lock
  (`0x3d32ad9e * crc32(current_database())`) so a Harn migration and a
  concurrent `sqlx migrate run` serialize against each other. It is
  idempotent against a SQLx-migrated database (applies zero rows, checksums
  byte-identical), refuses to run on a dirty ledger, errors on checksum
  drift naming the version, and warn-and-skips duplicate versions. The
  default `ledger: "harn"` path (the native `harn_migrations` SHA-256
  ledger) is unchanged. This lets harn-cloud retire its bespoke Rust
  `run_migrations()` in favor of `pg_migrate`.

### Fixed

- **Incremental project scans now detect same-instant edits that the
  modification-time heuristic alone misses.** `scan_incremental`'s automatic
  delta computation (the path taken when no explicit `changed_paths` signal is
  supplied) compared only `mtime > previous_mtime`. Millisecond mtime
  granularity collides on same-turn/same-second writes — and on
  coarse-granularity filesystems — so a file an agent wrote and then re-scanned
  in the same instant was silently treated as unchanged, leaving the index
  serving pre-edit symbol facts and feeding fuzzy-match-stale loops on weak
  local models. The delta now also flags a file as modified when its byte size
  differs from the cached record, an mtime-independent signal that catches the
  overwhelmingly common add/remove edit for free (the file metadata is already
  read for the mtime check). Length-preserving same-instant edits still rely on
  the explicit `changed_paths` bypass the agent loop already threads through
  after its own writes.
- **`pg_migrate` advisory lock now actually serializes, and the harn ledger
  verifies checksums.** Two correctness bugs in the `std/postgres` migration
  runner are fixed. (1) The Postgres advisory lock was taken, all migration
  work done, and the unlock run on *different* pooled connections. Because
  `pg_advisory_lock` is session-scoped (tied to one backend), concurrent
  `pg_migrate` callers did not mutually exclude, and the unlock usually ran on
  a connection that never held the lock — a no-op that leaked a session lock on
  a recycled connection. The runner now pins a single connection for
  lock → migrate → unlock (matching `sqlx migrate`), in both `harn` and `sqlx`
  ledger modes. (2) The default `harn` ledger wrote a SHA-256 checksum per
  migration but never read it back, so an edited (already-applied) migration
  file was silently skipped with no drift detection. The runner now re-hashes
  each already-applied file and errors with `checksum mismatch for migration
  <name>` when it differs from the recorded checksum, mirroring the `sqlx`
  mode's SHA-384 check. `pg_advisory_unlock`'s boolean result is now checked
  and a `false` (lock not held) is logged.
- **Postgres `nil` bind parameters no longer pin the TEXT type.** The
  `std/postgres` client previously bound a `nil` argument as `None::<String>`,
  which declared Postgres type OID `25` (TEXT) in the wire `Parse` message.
  Because sqlx caches prepared statements per pooled connection and sends
  params in binary, this caused two production failures: prepared-statement
  type-cache poisoning (a slot first seen as `nil` was cached as TEXT, so a
  later non-null integer was UTF-8-validated against TEXT and failed with
  `invalid byte sequence for encoding "UTF8": 0x00`), and wrong NULL typing
  (binding `nil` into an `integer`/`jsonb` column or cast failed with
  `column is of type integer but expression is of type text`). `nil` now binds
  as a Postgres NULL with type OID `0` (unspecified), so the server infers the
  parameter's type from the query context — the cast, the target column — just
  like a bare SQL `NULL`. Non-null binds are unchanged.
- **A tool call that the provider cut off mid-emit when the model hit its
  output-token cap is now auto-continued with a raised cap instead of burning
  the turn.** When a value model exhausts `max_tokens` partway through a tool
  call, the provider returns a length-truncation stop reason (`length` for
  OpenAI/OpenRouter/Ollama, `max_tokens` for Anthropic) and the partial output
  carries a truncated, unparseable call. The agent loop previously treated that
  as a malformed/missing call and dropped the turn to parse-guidance — a
  silent-corruption class that wastes a turn even on capable models that were
  mid-correct-action. The loop now detects this specific condition
  deterministically (no model cooperation, no abuse surface): a length
  truncation that resolved zero usable tool calls AND shows a partial-call
  signal (a parser truncation diagnostic or a tool-call opener prefix) is
  re-issued with a higher output cap so the model can finish the call. The
  re-issue is bounded (two continuations by default, each clamped to a ceiling)
  and does not consume a loop iteration; once the cap is exhausted the loop
  falls back to the existing parse-guidance path, so it can never loop forever.
  The gate keys on the normalized finish reason, so it generalizes across
  providers, and it fires ONLY on a real length truncation — a clean stop with
  a genuinely malformed call still flows through the parse-tolerance and
  reasoning-leak paths unchanged, with no overlap.

## v0.8.88

### Added

- **Eval statistics stdlib.** Added `std/eval/stats` with deterministic
  bootstrap confidence intervals, macro pass@1/pass^k, reliability and
  skip/timeout breakdowns, paired delta/regression gates, routing calibration,
  and trial aggregation for generic eval rows (#3117).

- Eval packs now support trial counts, held-out split validation, deterministic
  case and harness fingerprints, and per-case trial reliability summaries.

- **Flow-sensitive type refinement now narrows reference paths and
  `if`-expression branches, at parity with bare-variable narrowing.** Every
  refinement form that narrowed a variable now also narrows an *identifier-
  rooted reference path* — a chain of constant `.`/`?.` property accesses and
  constant `[…]` subscripts (`entry.arguments`, `cfg.opts.mode`, `xs[0]`,
  `m["k"]`):
  - `type_of(path) == "T"`, `path != nil`, and a bare `if path` (truthiness)
    narrow the path; a path whose type is the top type (`unknown`/`any`, e.g. a
    `json_parse` / `llm_call` boundary field) narrows to the tested kind.
  - `schema_is(path, S)` / `is_type(path, S)` and `path.has("k")` narrow the
    path.
  - A tagged-shape-union discriminant narrows the object path
    (`o.msg.kind == "ping"` narrows `o.msg`), gated so it never mangles a
    `dict`/`unknown` object.
  - `match type_of(subject) { "T" -> … }` now narrows the subject — variable
    **or** path — in each arm (previously this narrowed nothing, even for a
    bare variable).
  - The `unknown`-exhaustiveness lint (incomplete `type_of` chain reaching
    `unreachable()` / `throw`) now also covers `unknown`-typed paths.
  - An `if`/`else` used as an expression now narrows its branches like the
    ternary, so `let xs = if type_of(p) == "list" { p } else { [] }` infers
    `list` rather than widening back to `list?`.

  Narrowing is dropped when the base variable or path is reassigned. A
  *dynamic* subscript (`xs[i]` with a non-literal index) is intentionally never
  narrowed — it is not a stable reference. This is a static type-checker
  feature: the runtime is dynamically typed and `type_of` always reflects the
  concrete value, so no runtime change is needed.

### Changed

- **`tool_format` is now reject-or-work-well: a bad value fails loudly instead
  of silently degrading.** The agent-loop `tool_format` knob
  (`agent_tool_format_resolution` in `std/agent/options`) previously accepted
  any explicit string verbatim — a typo like `"nativ"` or a wrong value like
  `"json"` / `"tool_use"` flowed straight through as `source: "explicit"` with
  no warning, and every downstream branch that gates on `tool_format ==
  "native"` read it as `false`, so the agent silently ran the text protocol.
  Resolution now throws on any value that is not `native`, `text`, `auto`, or
  omitted. It also rejects requesting the side the capability matrix marks
  *impossible* for a model (`native` on a `text_only` model, or `text` on a
  `native_only` model); pass `tool_format_override_reason` to force the marked
  side deliberately (probe/matrix use). `*_unreliable` parity stays a
  recoverable warning, not a hard reject.
- **The provider-catalog validator now rejects alias `tool_format` pins that
  the target model cannot serve.** An alias may only pin `tool_format =
  "native"` / `"text"`, and only when the model's `tool_support` advertises
  that side. This caught a real shipped footgun: the
  `ollama-devstral-small-2-native` alias pinned `native` on a model the
  capability matrix marks `native_tools = false` (`text_only`). That alias has
  been removed — Devstral Small 2 on Ollama is text-tool-only.
- Added a deterministic tool-calling boot-camp battery
  (`crates/harn-vm/tests/tool_calling_bootcamp.rs`) that exercises the real
  resolution layer across a pairwise sample of {capability-profile ×
  requested-format × config-source} and asserts the reject-or-work-well
  invariant with zero live LLM calls.

### Fixed

- **Tool-call argument grammar now accepts the code-bearing value shapes weak
  value models naturally emit**, instead of dropping the turn to
  parse-guidance. Three shapes from the transcript corpus now parse and
  canonicalize back to `name({ ... })` on replay: (1) `+`-concatenated
  string/template fragments — including the multi-line backtick template
  literals and `` `…` + "`json:\"x\"`" + `…` `` struct-tag concatenation Go
  forces — collapse into one string value; (2) a heredoc whose closing tag is
  indented, misspelled, or omitted but whose call is structurally closed (a
  trailing `})`/`)` call-tail) is implicitly terminated at that tail; and (3)
  `=` is accepted as a synonym for `:` as the object key/value separator
  (`{ new_body= <<EOF … }`). A flat JSON-RPC/MCP envelope
  (`[{"name":"read","arguments":{…}}]` or a single object with `parameters`)
  also maps to the matching call. The recover/reject boundary stays sharp: a
  `+` with a non-string right operand, a heredoc body truncated mid-token with
  no structural tail, an ambiguous bare-`}` code close, and prose JSON that
  merely has a `name` key all still error loudly.

- **Text tool-call parser no longer wastes a turn on narration wrapped in
  `<tool_call>` tags.** Weak value models (DeepSeek) wrap their thinking in
  `<assistant_prose>` *inside* a `<tool_call>` block. The parser previously
  treated this as a malformed call, dropped it, and emitted a "could not be
  parsed" diagnostic — costing the model its whole turn for merely narrating.
  Such a block is now reclassified as assistant narration: the inner text is
  preserved as prose, no tool call and no parse error are emitted, and a
  prose-only turn surfaces to the loop as "said X but took no action" so the
  normal no-tool-call nudge applies. If the same wrapper also carries a real
  `name({ ... })` / nested-XML call, that call is still recovered and
  dispatched. The allowance is scoped to a small narration allowlist
  (`assistant_prose`, `thinking`, `reasoning`); unknown wrapped tags that look
  like attempted calls (e.g. `<frobnicate>{...}`) are still rejected.

- **Transcript compaction now honors the host `[no-compact]` pin so an agent's
  live grounding survives a compaction pass.** Both compaction surfaces in the
  `observation_mask` strategy — archived-window masking and kept-window
  `clamp_tool_outputs` length-clamping — now treat any tool-output or message
  body that contains the literal `[no-compact]` marker (emitted by the host
  around the current file view and the just-edited window) as pinned: masking
  preserves it verbatim and clamping leaves it intact. Previously the marker was
  ignored, so on long sessions the model lost sight of the file it was editing,
  then drifted, re-read, and stalled. The pin is bounded to the most recent
  `MAX_PINNED_SEGMENTS` (3) pinned bodies — older duplicate snapshots from
  earlier in the session compact normally — so a pin can never accumulate
  unbounded and overflow the context window. With no pins present, compaction
  behaves exactly as before.

- **Text tool-call parser now recovers more sloppy-but-unambiguous shapes from
  weak value models.** Building on the nested-XML wrapper acceptance, the
  `<tool_call>` parser now also recovers: a nested XML tool tag whose inner
  close is mismatched (`</edit_call>`) or absent and whose outer `</tool_call>`
  is missing entirely (terminating the body at the JSON object's closing
  brace); a missing inner close paired with a duplicate/trailing `</tool_call>`
  (the orphan close tag is swallowed silently); and leading-dot decimal literals
  (`.100` → `0.100`) inside an otherwise-valid `name({ ... })` argument object.
  Recovery stays constrained to registered/implicit tool names with JSON-object
  arguments and canonicalizes back to `<tool_call>name({ ... })</tool_call>` on
  replay; unknown inner tags are still rejected.

- **`harn providers export` / `providers validate --check-artifacts` now
  generate the `spec/provider-catalog/*` artifacts hermetically.** Generation
  reads only the compiled-in embedded provider config and capability matrix,
  ignoring the developer's `~/.config/harn/providers.toml`, environment
  overrides (`HARN_PROVIDERS_CONFIG`, `HARN_DEFAULT_PROVIDER`, `HARN_LLM_*`),
  the process runtime-catalog overlay, and ambient thread-local user overrides.
  Previously, artifact generation merged the effective (home/env-aware) config,
  so a developer's personal aliases/providers could leak into the shipped
  catalog and clean CI would then flag the artifacts as drifted. Runtime catalog
  presentation is unchanged and still reflects the host's live configuration; an
  explicit `--overlay` file remains honored because it is a declared,
  reproducible input rather than ambient machine state.

- **Reasoning-only turns that also call a tool no longer leak the model's
  private chain-of-thought into the visible message channel.** OpenAI-compatible
  normalization (both the non-streaming `normalize_openai_message_text` and the
  streaming transport path) promoted a turn's extracted reasoning into `.text`
  whenever the content channel was empty — intended for models that legitimately
  answer inside the reasoning channel. But gpt-oss / harmony models route their
  analysis channel into `reasoning_content` and emit a tool call with no
  committed content, so that promotion surfaced their intermediate
  chain-of-thought ("We need to inspect parser.rs first…") as the assistant
  message. That contaminated both the user-facing transcript and the
  transcript-mined eval grader. Promotion is now suppressed when the turn
  carries a tool call (the tool call is the action, the reasoning is not a final
  answer); the reasoning stays under `thinking`. Reasoning-as-answer promotion
  on tool-call-free clean stops is unchanged.

## v0.8.87

### Added

- **LLM calls can opt into catalog-equivalent provider failover (#3135).**
  Passing `equivalent_failover: true` now builds a first-class routing policy
  from compatible same-logical-model catalog routes, preserving routing receipts,
  budget checks, and transcript metadata while failing over on provider outages
  and rate limits.

### Fixed

- **Postgres event logs are represented as host-provided event-log metadata.**
  Managed Harn deployments can now report a `postgres` event-log backend without
  pretending the host-provided log is one of the built-in file or SQLite logs.

## v0.8.86

### Fixed

- **Text-tool parsing now recovers registered XML-wrapped JSON tool calls.**
  Harn accepts this value-model output shape for known tools, rewrites replay
  to canonical text-tool syntax, and still rejects unknown XML tags.

## v0.8.85

### Fixed

- Added a terminal `done_judge.max_invocations` / `max_feedback` cap with
  structured run-record counters so repeated done-judge veto loops can stop as
  `verify_capped` instead of running to the iteration budget.
- Ignored `.target-inspect` generated package artifacts in markdownlint so the
  parallel release audit cannot lint cargo package output as authored docs.

## v0.8.84

### Fixed

- Published the native-JSON parser robustness fixes that landed on `main`
  after the `v0.8.83` tag was cut. This patch release makes the UTF-8-safe
  preview parser and native-JSON detection fixes available to downstream
  `.harn-version` pins instead of requiring consumers to track Harn `main`.

## v0.8.83

### Breaking

- **Member-access nil safety is now uniform across `.`, `[]`, and `.()`.**
  Subscript (`obj[key]`) and method-call (`obj.method(..)`) receivers are
  now held to the same standard the checker already applied to property
  reads: a statically-`nil` or `T | nil` receiver is an **error**, and an
  `unknown` receiver is a **warning**. Previously only `obj.field` was
  diagnosed, so `obj[key]` / `obj.method()` on a possibly-nil value passed
  `harn check` and failed at runtime instead. Migrate with the matching
  optional operator (`?[…]`, `?.method()`), a `!= nil` guard, or a `??`
  default. `any` receivers remain a deliberate, undiagnosed escape hatch,
  and the ambient dict-literal idiom (`let d = {a: 1}; d["b"]`) stays loose.

  To keep the stricter rule pleasant, two long-standing narrowing gaps were
  closed alongside it: an `o?.field != nil` (or `?[]` / `?.()`) guard now
  narrows the **base** identifier `o` to non-nil on the matching branch, and
  the `??` coalesce operator now drops the nil arm even when its left operand
  is a **named type alias** that expands to a nilable union (previously only
  inline `T | nil` unions were narrowed). The conformance harness also labels
  failures by stage — `type error` / `compile error` / `runtime error` —
  instead of calling every pre-runtime failure a "runtime error".

### Added

- **Row polymorphism: open record types and row-polymorphic generics.** Shape
  types may now carry a trailing **row tail** — `{id: string, ...R}` is an open
  record (the listed fields plus a row variable `R` standing for any other
  fields), and `{...R1, ...R2}` is the right-biased merge of two rows. A
  function generic over rows types record merge precisely and soundly:

  ```harn
  pub fn merge<R1, R2>(a: {...R1}, b: {...R2}) -> {...R1, ...R2}
  ```

  `merge({a: 1}, {b: "x"})` now returns `{a: int, b: string}` — every field
  preserved with its real type, `b` overriding `a` on overlap — instead of
  failing to unify a single value type or collapsing to `dict`. Open-record
  parameters (`fn f(x: {id: string, ...rest})`) accept any record that has the
  required fields and carry the rest through. Row variables bind one-sidedly
  from the actual record's leftover fields; gradual tails (`dict`, `any`)
  interoperate, and absence reasoning stays restricted to closed shapes. `std`'s
  `merge` and `deep_merge` are re-typed with row signatures.

### Changed

- **Record merge and spread now infer the precise merged shape.** `{...a, k: v}`,
  `{...a, ...b}`, and `a + b` on record shapes now produce the right-biased
  merged shape — every field carried through with its real type, later fields
  overriding earlier ones — instead of collapsing to an untyped `dict`. On an
  overlap the result is required if either side is required, and its type is the
  overriding (right) field's type, or the union of both when the right field is
  optional. Spreading a non-closed source (a `dict`, `dict<K,V>`, union, or
  unknown) still degrades to `dict` rather than inventing fields. This is the
  structural foundation for full row-polymorphism support. Generic functions
  also now bind a type parameter from a **named-alias** argument the same way
  they already did from an inline shape literal (`type Opts = {…}` arguments to
  `dict<string, V>` parameters no longer fail to infer `V`).

### Fixed

- **Three parse-robustness fixes for the agent tool-call path.** The
  native-JSON salvage path no longer panics on multi-byte UTF-8
  (emoji/accents/CJK) in trailing prose after a `[{"id":...}]` array — it now
  parses the first JSON value with a boundary-safe forward
  `serde_json::Deserializer` instead of an O(n^2) backward byte scan that could
  slice mid-codepoint and abort the turn. The tagged-protocol fence-parity
  check no longer treats an earlier *unbalanced* ` ``` ` as fencing a later
  legitimate `<tool_call>` block (which dropped the call and injected a spurious
  protocol violation); an open fence only encloses a tag when a matching close
  follows. And `agent_loop` now injects `parse_guidance` on partial-success
  turns (some calls parsed, one malformed) flagged `has_partial_success` with
  the dispatched-call count, so the model gets a signal to re-emit the dropped
  call instead of zero feedback — while the no-progress stall suppression stays
  gated on full parse drops only.
- The release publish step (`scripts/publish.sh`) now treats cargo's "timeout
  while waiting for published dependencies" / "timed out waiting for … to be
  available" as a retryable index-propagation condition. Previously a slow
  crates.io index could leave the last crate (e.g. `harn-cli`, waiting on
  `harn-lsp`) unpublished and abort the run as a "non-retryable error" without
  even trying the per-crate fallback; it now retries and falls back, so a
  propagation lag no longer leaves a release half-published.
- **Fixed four latent stdlib type bugs surfaced by precise record-merge typing.**
  Now that `merge` infers the exact merged shape, the type checker caught
  mismatches the old untyped `dict` return had hidden:
  - `github.enable_auto_merge` reads `method`/`merge_method` options that
    `GitHubCallOptions` never declared — added them.
  - `github.wait_until_deploy_succeeds` / `wait_until_ci_green` /
    `wait_until_pr_merged` built monitor options with GitHub's millisecond
    field names (`timeout_ms`, `poll_interval_ms`, `max_wait_ms`), which
    `wait_for` silently dropped — and since the monitor requires a `timeout`
    duration, those calls would have **thrown at runtime**. They now translate
    the millisecond cadence into the duration-typed `MonitorWaitOptions` so the
    caller's timing is honored.
  - The git-forge pull-request event builder is now annotated so its
    `filter_nil`-projected fields type as the declared `GitForge*` structs.
  - `graphql_parse_schema`'s `current` accumulator no longer trips a
    narrow-to-`never` reassignment error.
- **The entire shipped stdlib now passes `harn check` cleanly.** Closing the
  loop on the precise-typing work, eight more latent type bugs were
  root-caused and fixed: nilable option bags narrowed before reaching
  non-nil `dict` builtins (`waitpoint`), missing/mis-typed fields on the
  `context_artifact`, `TriageEvent`, and agent option-bag shapes corrected to
  match what the builders actually emit, a nilable `provider` defaulted before
  a non-nil use (`agent/options`, `agent/sitrep`), a nilable hook registry
  narrowed (`tool_hooks`), and an always-throwing `__fact_error` typed
  `-> never`. The type checker's `.reverse()` was also fixed to return the
  receiver's own type (list-reversing a `list<T>` yields `list<T>`, not
  `string`).

## v0.8.82

### Breaking

- **`substring(s, start, end)` now takes an exclusive end index, not a length.**
  The free `substring(...)` builtin previously treated its third argument as a
  **length**, while the `.substring(...)` method, the `s[start:end]` slice
  operator, `list.slice`, `bytes_slice`, and the language spec all use an
  exclusive **end** index. The builtin now matches that single convention, so
  `substring("hello world", 6, 9)` returns `"wor"` and the two call forms agree.
  Both forms share one implementation, so they can no longer drift. Migrate any
  length-style calls: a "last N chars" `substring(s, len(s) - n, n)` becomes
  `substring(s, len(s) - n)` (omit the end to run to the string end), and a
  fixed-length slice `substring(s, i, n)` becomes `substring(s, i, i + n)`.

### Added

- **Experimental `harn-codegen` crate: a Cranelift-backed native compiler for
  Harn's scalar-compute subset.** Lowers `int`/`float`/`bool` functions
  (arithmetic, comparisons, logical ops, branches, loops, locals) from VM
  bytecode to native machine code, with an in-process JIT (`NativeFunction`),
  an object-file backend (`emit_object`), a pure-Rust reference interpreter,
  and a `harn-nativec` CLI. It is `publish = false` and is not a dependency of
  `harn-cli`/`harn-vm`, so the distributed binary never links Cranelift; build
  it explicitly with `-p harn-codegen`. See
  `docs/src/dev/native-codegen.md`.
- `harn pg codegen` now formats its rendered output, so generated type files
  are `harn fmt`-clean by construction.
- `harn lint` and `harn check` skip style and unused-declaration lints for
  machine-generated `*.generated.harn` files (type diagnostics still apply, and
  `harn fmt` still formats them). The signal is the filename, not an in-file
  `@generated`/`DO NOT EDIT` comment, so a generated marker cannot be pasted in
  to silence lints on a hand-written file.
- `std/postgres/query` projection helpers (`uuid_text`, `timestamptz_json`,
  `nullable_timestamptz_json`) now accept table-qualified column names such as
  `timestamptz_json("vaults.created_at")`. Each dot-separated segment is
  validated as an identifier and the output alias is the trailing segment
  (`created_at`), so projections from joined queries compose through
  `columns([...])` without `unsafe_sql(...)` or brace escaping.
- `std/postgres/query` projection helpers (`uuid_text`, `timestamptz_json`,
  `nullable_timestamptz_json`, `select_clause`) now return trusted
  `PgSqlFragment`s, and a new `columns(parts)` helper joins projection
  fragments/strings into one `{projection}` fragment. Column projections now
  drop into `sql(...)` placeholders without an `unsafe_sql(...)` wrapper, and
  carry the literal `'{}'` JSON path safely.

### Fixed

- **`hostlib_code_index_*` `line_count` no longer overcounts files that end in a newline.**
  The code-index file scanner counted lines with `content.split('\n').count()`,
  which reports one phantom extra line for any file with a trailing newline (the
  common case) — e.g. a two-line file ending in `\n` was reported as 3 lines.
  Line counting is now shared through a single `count_lines` helper, matching the
  scanner and process-artifact surfaces that already counted correctly, so the
  `line_count` field surfaced to scripts is accurate.
- Conformance fixture `.harn/` dirs now ignore every runtime SQLite DB tests drop in (e.g. the rate-limit
  store `llm-rate-limits.sqlite`), not just the event log, so a stray DB no longer trips the release guard.
- Fixed unsound typed-opcode specialization for reassignable bindings. The
  compiler trusted a `var` / `for`-item binding's initializer-inferred primitive
  type when emitting typed fast-path opcodes (`AddInt`, `LessInt`, …), even
  though such a binding can be reassigned through an `any`-typed value of a
  different runtime primitive. Because typed opcodes hard-error on an operand
  type mismatch, the optimized build could throw a spurious
  `Typed int add expected int operands, got int and float` on a program the
  unoptimized build runs correctly. The compiler now keeps the typed fast path
  only for bindings a new monomorphism analysis can prove keep a single
  primitive type across their initializer and every reassignment in scope; all
  others fall back to the generic adaptive path, which re-checks operand shapes
  at runtime. The common loop-counter and accumulator idioms stay fully
  specialized.
- **Streaming `output_schema` validation tolerates markdown code fences.**
  The incremental JSON validator behind `schema_stream_abort` (and the
  `std/json/stream*` builtins) now strips a leading triple-backtick fence
  (with an optional language tag such as `json`) and a trailing closing fence
  around the JSON body, surviving arbitrary chunk boundaries. Local Ollama
  structured-output calls that wrap their JSON in a code fence no longer abort
  with `schema_stream_aborted`. Genuine non-JSON leads and schema violations
  inside the fence still fail as before. A root scalar (number / `true` /
  `false` / `null`) is now framed at the first trailing whitespace or backtick,
  so trailing junk after the value (e.g. `42 garbage`, `4 2`) is correctly
  rejected as invalid rather than silently dropped.
- Typed fast-path opcodes (`AddInt`, `LessInt`, `EqualString`, …) now guard
  their operands and fall back to generic semantics on a type miss instead of
  hard-erroring. The compiler emits these from a static type guess, but a guess
  can be wrong at runtime — an `any`-typed value flowing through a typed
  parameter or an annotated binding initializer (`let x: int = <any>`) is not
  runtime-checked, so the operand may be a different primitive than the
  annotation claims. The optimized build previously threw a
  specialization-internal error (e.g. `Typed int add expected int operands, got
  int and float`) on programs the unoptimized build runs correctly; it now
  produces the same result as the unoptimized build by construction. The hot
  path where the guess holds is unchanged, and genuinely incompatible operands
  still error with the same generic message in both builds.
- **Sandboxed builds no longer poison the shared `sccache` daemon.** `sccache`
  runs as a single long-lived per-user server; if a sandboxed cargo build was the
  first caller to spawn it, the daemon inherited harn's `sandbox-exec`
  confinement permanently (even after reparenting to launchd) and then failed
  *every* later build machine-wide with `Operation not permitted` — unable to
  read build inputs outside the sandbox root or write its cache dir under
  `~/Library/Caches`. Sandboxed process spawns now bypass the rustc wrapper
  (empty `CARGO_BUILD_RUSTC_WRAPPER` / `RUSTC_WRAPPER`, which overrides
  `build.rustc-wrapper` in `.cargo/config.toml`), so a per-command sandbox can
  never confine the cross-workspace daemon. The on-disk cache and all
  unsandboxed builds are unaffected.

## v0.8.81

### Added

- **Natural-language tool binder schema middleware.** `std/llm/tool_binder`
  now exposes a paired schema transform for advertising optional natural-language
  fallback intents alongside the existing binder caller.
- Added `std/agent/transcript` helpers for canonical agent transcript normalization and tool call/result extraction.

### Fixed

- **Agent loop recovery and provider cooldowns are more robust (#3094).**
  Agent loops now turn zero-dispatch tool-call protocol violations into
  actionable retry feedback, and provider `Retry-After` rate-limit signals
  temporarily cool down matching provider/model routes across subsequent calls.
- **Bytecode cache fingerprints are now checkout-path stable.** Harn no longer
  bakes absolute compiler source paths into `HARN_CODEGEN_FINGERPRINT`, so
  precompiled `.harnbc` artifacts generated from the same Harn source match
  across Linux, macOS, and separate local worktrees.

## v0.8.80

### Fixed

- **LLM retry-after handling now parses provider messages with trailing
  punctuation.** Rate-limit retries such as Cerebras
  `(retry-after: 60))` now honor the full provider delay instead of falling
  back to short exponential retries that can immediately hit another 429.

## v0.8.79

### Added

- **Durable cross-process rate-limit admission.** Harn now exposes
  `durable_rate_limit_acquire(options)` for SQLite-backed sliding-window quota
  reservations across processes, with atomic multi-bucket admission, structured
  timeout results, and mock-clock-friendly tests (#1873).

### Changed

- LLM provider rate limiting now consumes catalog `rate_limits` metadata,
  including model-specific RPM/TPM and route concurrency, with environment and
  `llm_rate_limit` overrides for paid/custom quotas.
- **Durable LLM rate-limit admission.** Catalog and runtime LLM RPM/TPM
  limits now use shared SQLite admission by default across Harn processes, so
  parallel eval runners and worker fleets respect one provider/model quota
  without relying on per-process sleeps or ad hoc environment-only guardrails
  (#1873).

## v0.8.78

### Added

- **Postgres query templates.** `std/postgres/query` now includes
  `sql(...)` and `named_sql(...)` helpers that turn readable `{name}` SQL
  templates into `$n` parameterized query records, plus explicit identifier and
  source-controlled fragment helpers for SQL structure.
- Added tree-sitter string-content nodes and SQL language injections for
  `sql(...)` and `named_sql(...)` template strings from `std/postgres/query`.

## v0.8.77

### Added

- **Postgres query ergonomics.** `std/postgres/query` now provides Harn-native
  named query records, `one`/`many`/`exec`/`run` wrappers over `std/postgres`,
  and safe static projection helpers for UUID and timestamp columns (#3072).
- **`std/sqlite` hostlib.** Harn scripts can now `import "std/sqlite"` to
  open in-memory or file-backed SQLite databases, run parameterized queries and
  statements, use transactions/savepoints/migrations, and test SQL
  deterministically with fixture-backed mocks (#3073).
- Added Harn-owned `session/rollback` and `session/redo` ACP primitives that
  move transcript checkpoints and hostlib filesystem snapshots together for
  completed agent turns.
- **Live session attach/detach lifecycle.** Harn sessions now expose
  observer/controller live-client ownership, takeover, detach, heartbeat,
  controller prompt injection, and permission-route audit semantics for
  interactive clients (#3076).

### Changed

- **Provider/model catalog authoring is now fragment-based and
  model-equivalence aware (#1875).** Harn now generates its embedded provider
  and capability snapshots from smaller TOML source files, exports
  rate-limit/architecture/API-dialect/wire-model metadata to downstream catalog
  artifacts, and exposes equivalent-model routes for providers that serve the
  same logical model under different wire IDs.

## v0.8.76

### Fixed

- **Windows release binaries.** Fixed the Windows release binary build by
  importing the process-sandbox preset type wherever the shared sandbox preset
  helpers are compiled (#3068).
- **Windows process sandbox cwd handling.** Sandboxed foreground process
  launches now choose an allowed workspace cwd when the caller does not provide
  one, avoiding inherited host cwd access that could stall AppContainer-backed
  `shell()`/`exec()` calls on Windows (#3069).
- **Windows process sandbox launches.** AppContainer-backed `shell()`/`exec()`
  calls now run with AppContainer-owned `LOCALAPPDATA`, `TEMP`, and `TMP`
  directories, no console desktop binding, and no implicit recursive ACL churn
  over shared home-scoped Cargo/Rustup/package-manager caches, preventing
  sandboxed process calls from hanging on Windows CI and release smoke jobs
  (#3070).

## v0.8.75

### Added

- Added first-party CircleCI and Buildkite connector contract fixtures and
  discovery docs, including signed webhook normalization coverage and connector
  parity matrix entries.
- Added a format-aware tool-call paradigm (`agent_tool_call_paradigm`,
  `agent_render_tool_call_exemplar`, and a `body_hint` prompt binding) so
  prompts and tool descriptions reference an abstract paradigm that maps to the
  current model's format -- heredoc-bodied text or native JSON -- giving weak
  models an escape-free body channel by default. `agent_progress` no longer
  aborts a turn on an out-of-enum status/priority, and the value parser recovers
  an object string value that closes early on an embedded unescaped quote.

### Fixed

- Expanded the default process sandbox `developer_toolchains` preset to cover
  common home-dir runtimes such as `uv`, `rustup`/`cargo`, `pyenv`, `nvm`,
  `volta`, and `~/go`, and extended the same home-scoped preset logic to the
  Windows AppContainer backend so sandboxed child processes can load
  user-managed toolchains without widening Harn's file builtins.
- Kept `harn rule test` project discovery aligned with rule-pack loading:
  missing `[rules] ruleDirs` now fail clearly, and nested utility directories
  are no longer swept into the default rule-test gate.
- **Cheap models no longer loop on JSON-escaped heredoc bodies; parse errors now
  reach the model.** Two fixes for the failure where a model's `edit(...)` turn
  yielded zero tool calls and then re-emitted the identical malformed call until
  the loop exhausted. (1) Parser recovery: a `<<EOF` heredoc whose body uses
  literal `\n` line breaks (the JSON-escaped one-liner form cheap models like
  qwen3.6 emit) is now decoded and dispatched instead of hard-rejected with
  "expected newline after heredoc tag"; real-newline heredocs are parsed exactly
  as before. (2) Feedback fidelity: a turn whose tool calls were all dropped by
  the parser now gets the purpose-built `parse_guidance` feedback (which names
  the exact diagnostic and shows the heredoc syntax) and is excluded from the
  no-progress stall streak, instead of the misleading "emit one well-formed tool
  call" nudge. Both fire purely on the syntactic parse-error condition, so strong
  models that emit clean calls never trigger them.
- **Unused-symbol linting now counts references from callable defaults and
  type-only positions.** Imports, parameters, locals, and types referenced from
  default parameter expressions, keyed mutex expressions, binding annotations,
  pipeline return annotations, closure parameter annotations, typed catch
  clauses, explicit generic call type arguments, or `schema_of(T)` are no
  longer reported as unused.
- **Policy, parser, host process, and OAuth edge cases are now handled more
  strictly.** Unix-socket JSON requests and provider file uploads now
  participate in network/file policy gates and handoff effects, malformed
  loopback OAuth requests no longer abort a pending valid callback, background
  command handles preserve unavailable process-group IDs as `nil`, background
  feedback peeks no longer restamp unrelated inbox entries, and generic and
  `where` lists reject empty/trailing-comma forms.

## v0.8.74

### Changed

- **Cerebras model catalog tracks the current public endpoint set.**
  `gpt-oss-120b` now uses Cerebras's public discovery pricing, `zai-glm-4.7`
  is cataloged as the current public preview coding/agentic route with native
  tools and `reasoning_effort="none"` support, and the stale Cerebras Llama row
  is marked dedicated-only so clients do not present it as a one-click
  serverless option.
- Harn now models provider-specific `reasoning_effort` value sets so
  Cerebras-hosted `gpt-oss-120b` floors `reasoning_policy: "off"` to the
  endpoint's supported `low` effort instead of sending unsupported `none` or
  `minimal` values, and structured LLM calls accept the same documented routing,
  reasoning, timeout, fast-mode, and prompt-assembly option keys as `llm_call`.

### Fixed

- **Compaction honors its `tool_output_max_chars` / `compress_callback` policy.**
  The compaction engine parsed, defaulted (16k), and documented these fields but
  never applied them, so oversized tool-result bodies in the kept context window
  stayed at full length. They are now clamped during compaction — via a custom
  `compress_callback` when set, otherwise the built-in microcompactor — while
  each message's `role` and `tool_call_id` are preserved so tool-call pairing
  stays intact.

## v0.8.73

### Added

- **Behavioral tips to cut cheap/local-model toolless churn and format
  leakage.** The agent loop now recovers from the most common weak-model
  failure habits without changing the wire protocol. New TEXT-mode corrective
  nudges fire on the turns the native-gated completion confirmation never
  reached: a `fenced_call_attempt` nudge when a call is wrapped in a
  ```` ```tool_code ````/`call`/`edit`/`python` Markdown fence the parser
  ignores, and a `named_tool_not_called` nudge when the model narrates a bound
  tool ("I'll use `edit`...") but emits no call. A decaying "turns since
  meaningful progress" counter drives an escalating `no_progress_streak` nudge
  for pure-prose churn; it does not fully reset on a single dispatch, and the
  content-specific nudges take precedence so a turn is never double-nudged. The
  text response-protocol prompt also hoists an anti-fence rule, an
  object-literal-vs-Python-kwarg rule, a heredoc-close reminder, and a
  "no code in `<user_response>`" rule. All detectors are conditioned on observed
  output shape, not on any model name.

### Changed

- **Provider catalog.** OpenRouter DeepSeek V4 Flash and V4 Pro now use the
  current OpenRouter 1,048,576-token context windows, cache-read prices, and
  standard rate cards while retaining the direct DeepSeek V4 entries and legacy
  alias metadata.
- **`harn fmt`, `harn lint --fix`, and the LSP on-save fixer share one autofix
  apply/dedup policy.** The "drop overlapping fixes and splice right-to-left"
  logic now lives in one place (`FixEdit::apply_all` / `dedupe_overlapping`), so
  the three surfaces can no longer drift on which conflicting fixes win.
- **List/dict accumulators are now O(1) amortized.** The common
  `xs = xs + [item]` / `xs += [item]` and dict-merge accumulator patterns no
  longer clone the whole collection on every step. A compiler optimization
  clears the binding's reference before the concat so the runtime extends the
  existing allocation in place. Building a 40 k-element list this way drops from
  about 18 s to about 0.5 s. Behavior is unchanged, including aliasing like
  `x = x + x`, and the scalar `i += 1` fast path is untouched.

### Fixed

- **Release finalization writes generated GitHub release notes inside the repo
  workspace (#3046).** `release_ship.sh --finalize` no longer asks Harn to
  write its default notes file under `/tmp`, avoiding sandbox rejection in the
  publish-release recovery workflow.
- **Corrected gemma-4 / vision / Opus capability declarations.** The local
  (vLLM/SGLang) `gemma-4*` rule now declares native tools and native structured
  output instead of silently degrading to text tools; the Ollama `bakllava`,
  `llama3.2-vision`, and `gemma3` rules resolve to
  `thinking_block_style = "none"` so caption models no longer emit a spurious
  "## Reasoning" scaffold; both Ollama `gemma4` rules add
  `structured_output = "format_kw"` plus explicit text tools so JSON/schema
  output is no longer blocked; and the two Opus 4.6 rules use the canonical
  `structured_output = "tool_use"` instead of the deprecated `json_schema`
  alias. A new audit test walks every catalogued provider alias so future
  tool-capability omissions trip in CI.
- **Vertex honors the modern `output_format` for structured output.** It
  previously read only the legacy `response_format`/`json_schema` mirror, so a
  call using `output_format: {kind: "json_schema", schema}` silently produced no
  structured-output directive on the Vertex backend.
- **`number` is now a usable static type.** The runtime already accepted
  `number` as `int | float`, but the static type checker treated it as an opaque
  name, so `fn f(x: number) -> number { x + 1 }` raised spurious type errors at
  every use and arithmetic site. `number` now resolves to `int | float`
  everywhere (assignment, argument, return, and arithmetic), exactly like an
  explicit `int | float` annotation.
- **Ollama truncation visibility and cache mislabeling.** The `/api/chat` NDJSON
  done-frame parser now captures Ollama's `done_reason` into `stop_reason`, so
  length-truncation is visible on the most-used local chat path. A
  `done_reason == "length"` cut-off no longer surfaces as the retryable
  `[ollama_empty_content_parser_bug]` error; it returns cleanly with
  `stop_reason: "length"`, a non-retryable signal, so the retry loop no longer
  spins re-truncating a deterministic token cap. Native Ollama responses now
  report cache as `cache_visibility: "unsupported"` with a null
  `cache_hit_ratio` instead of a fabricated `0.0` ratio.
- **OpenRouter structured calls to non-reasoning models no longer 404.** When a
  model declares no reasoning capability, Harn no longer emits a
  `reasoning: {enabled: false}` disable directive alongside
  `require_parameters: true`; that combination made OpenRouter drop every
  endpoint that does not support the reasoning param, such as
  `qwen/qwen3-coder` JSON-schema calls.
- **Truncated reasoning no longer leaks into the final answer.** On an
  OpenAI-compatible response cut off at `finish_reason: "length"` with empty
  content, Harn no longer promotes the partial reasoning trace into
  `.text`/`.visible_text`; the surfaced answer stays empty/flagged and the
  partial trace is exposed via `thinking` instead.
- **Unknown-model errors classify uniformly.** OpenRouter reports an unknown
  model as an HTTP-400 `"<id> is not a valid model ID"` body; Harn now maps that
  prose to `NotFound`/`model_unavailable`, matching Cerebras's 404 path.
- **Reserved-token tool-call delimiter remapping now runs in the shared
  transport funnel.** The `[[CALL]]` to `<tool_call>` rewrite is no longer
  limited to the registered OpenAI-compatible path, so unregistered providers
  such as `llamacpp` get the same canonical parser input across streaming and
  non-streaming calls.
- **Streaming no longer idle-times-out during a slow prefill.** The time to the
  first token is processed with no SSE bytes, so a slow model on a large context
  could trip the short inter-token idle timeout before its first token. The
  first token now gets a more generous budget: default 4x the idle timeout, min
  120s, still bounded by the overall stream deadline and tunable with
  `HARN_LLM_FIRST_TOKEN_TIMEOUT`.

## v0.8.72

### Added

- Add a shared git-forge PR/MR lifecycle event contract for connector packages,
  including GitHub/GitLab/Gitea normalization helpers and a provider-independent
  status-comment writeback request.
- **`index_of(haystack, needle, from?)` string builtin** - the missing sibling
  of `starts_with`/`ends_with`/`contains`, char-indexed to pair with
  `substring` (returns `-1` when absent).
- **`error_is(error, category)` and `error_is_transient(error)` testing
  builtins** - parameterized over the full error-category taxonomy, so a harness
  can assert any category (`cancelled`, `budget_exceeded`, `server_error`, ...)
  or the retry oracle directly. `is_timeout`/`is_rate_limited` are now the two
  pre-wired spellings of `error_is`.
- **MiniMax M3 provider catalog support.** Adds direct MiniMax and OpenRouter
  catalog rows for MiniMax M3, exposes video input as a first-class LLM
  capability/content block, and keeps static pricing on MiniMax's standard
  non-promotional rate card.
- **Hashed raw strings `r#"..."#`.** Raw string literals can now embed literal
  double quotes using Rust-style `#` delimiters (`r#"..."#`, `r##"..."##`, ...),
  so quote-heavy regexes and patterns no longer need backslash escaping. The
  formatter picks the narrowest safe delimiter automatically.
- **`regex_captures` reports match positions.** Each match record now carries
  `start`/`end` (character offsets) and `line` (1-based), and the builtin
  accepts an optional `flags` argument (`i`, `m`, `s`, `x`) for parity with
  `regex_match`. This makes positional diagnostics (the equivalent of Python's
  `m.start()` / line-of-offset) expressible without re-scanning the input.

### Fixed

- **Hostlib local sandbox npm smoke coverage (#2994).** Added a golden
  integration test proving `LocalSandbox` can run `npm install --offline`
  against a project `.npmrc` and a vendored `file:` tarball dependency without
  requiring registry access.
- **Docs CI now checks internal documentation links (#2995).** Broken local
  links under `docs/src` are caught by a fast CI gate, including the prompt
  assembly page's stale `agent_loop` target.
- Inject prerendered website pages with per-page title, description, canonical,
  social metadata, and JSON-LD.
- **Portable user home-directory resolution (#3032).** Harn now resolves the
  user home directory through a single `user_dirs` path that falls back to
  `%USERPROFILE%` when `$HOME` is unset, fixing a class of silent Windows
  degradations: a cwd-relative bytecode/pack cache, an unloaded
  `~/.config/harn/providers.toml` overlay, and unresolved Bedrock AWS profiles.
  It also collapses five drifted home-dir helpers into one tested
  implementation.
- **Cross-platform process timeout and liveness (#3033).** A timed-out child
  process is now terminated through the cross-platform
  `std::process::Child::kill()` so `wait_with_timeout` can no longer hang on
  Windows, and supervisor liveness checks use `sysinfo` instead of shelling out
  to `kill -0` (collapsing the prior `#[cfg(unix)]`/stub split into one portable
  function).
- **Tool-call parsing no longer shreds calls whose arguments contain the
  protocol's own tags.** A literal `</tool_call>`, a `<<TAG ... TAG` heredoc, or
  a bash `<<EOF` inside a quoted string argument is now treated as content, not
  as the structural close, across the buffered parser, the streaming detector,
  and the wrapper-stripper. Two `<tool_call>` blocks in one turn also get
  turn-unique ids instead of both colliding on `tc_0`.
- **`base64`/`base64url`/`base32`/`hex` encoding and the `sha2`/`md5` hashes are
  lossless for `bytes` input.** They previously routed `bytes` through the
  display form, silently truncating binary payloads at 32 bytes; they now accept
  `string | bytes` and hash/encode the raw bytes.
- **Durable `step.run` replay is no longer quadratic.** Replay detection uses an
  indexed idempotency lookup instead of rescanning the whole step topic on every
  step, so a K-step workflow stops doing O(K^2) work.
- **Anthropic structured-output requests stop silently discarding a
  caller-supplied `tool_choice`/tool set.** Structured output still wins, but it
  warns once and preserves the caller's tools instead of dropping them with no
  signal.
- **Import errors name the directory a relative import was resolved against,**
  so it's clear whether resolution was relative to the importing file or the
  CWD.
- **Gemini/Vertex thinking tokens + Vertex/Bedrock cache tokens in usage
  accounting.** The Gemini and Vertex adapters now fold
  `usageMetadata.thoughtsTokenCount` into `output_tokens`, so thinking-enabled
  models no longer under-report billed output and cost. Vertex now also reads
  `cachedContentTokenCount` into `cache_read_tokens` (previously dropped), and
  the Bedrock Converse adapter surfaces `cacheReadInputTokens` and
  `cacheWriteInputTokens` as `cache_read_tokens` / `cache_write_tokens`.

### Security

- **Run-event payloads are redacted at the bus boundary.** The redaction policy
  is now applied once, centrally, as every `RunEvent` is emitted, so a hook
  payload (and any future variant carrying free-form JSON) cannot leak secrets by
  an emitter forgetting to scrub it first.

## v0.8.71

### Added

- Added a golden recall@5 eval for client-side tool-search ranking over a realistic host-tool registry.
- Added a release binary-size gate for the shipped `harn` binary, including a
  `cargo bloat` report artifact for release-build runs.
- Added a landing-page example gallery backed by runnable `harn demo`
  scenarios.
- Added live ACP workspace-anchor methods for host clients:
  `harn.session_workspace_roots`, `harn.session_add_root`, and `harn.session_reanchor`.

### Fixed

- **OTel span exports now bound span-end attribute key cardinality (#2991).**
  Non-allowlisted span metadata keys are folded into `harn.meta_json` instead
  of being emitted as unique top-level OTel attributes.
- **Agent loop now emits a final wrap-up turn on budget/iteration exhaustion.**
  When the loop terminated because it ran out of iterations or budget *while the
  model was still calling tools*, the surfaced final assistant text was whatever
  the last tool-call turn produced — a dangling tool call with no clean
  `<user_response>` or completion sentinel, so output-contract / done-sentinel
  checks failed even when the work succeeded. The loop now fires exactly one
  tool-less LLM call on exhaustion/cap (`budget_exhausted` / `verify_capped` /
  `verify_exhausted` / `stuck`) to elicit the model's final answer + sentinel and
  records it as the final assistant response, so the run ends with a real summary
  instead of a dangling tool call. The wrap-up never changes
  `final_status`/`stop_reason`, is skipped on clean completion / suspension /
  terminal errors, and is opt-out via `final_wrapup: false`.

## v0.8.70

### Added

- **Observability:** Added `HARN_OTEL_SAMPLE_RATIO` so OTLP trace export can
  downsample root traces at the source while keeping the default at full
  fidelity.
- **ACP staged-fs discard (#3017).** ACP clients can now call
  `session/fs_discard_staged` to drop all pending staged filesystem writes, or
  only selected `paths`, using the same session-scoped hostlib staged-FS store
  as `session/fs_commit_staged`.

### Fixed

- **Homepage hero snippet validation (#2986).** The marketing-site hero now
  renders a checked-in `.harn.txt` example that passes `harn check`, and the
  site snippet fixtures are covered by a dedicated parse guard.
- **Docs CLI flag validation (#2987).** Bash/sh Harn examples in the docs now
  check their long flags against the live CLI help, and stale doctor/test/try
  examples were updated to current syntax.
- **Package-manager sandbox config (#2988).** OS-hardened process sandboxes now
  read common per-user npm, pip, cargo, git, and CA config/cache roots by
  default without making those paths writable or readable by Harn file builtins.
- **Documented division- and modulo-by-zero semantics accurately (#3007).** The
  language spec and `language-basics` tutorial previously claimed "division by
  zero returns `nil`", which is wrong on every count. Integer division by zero
  raises a catchable runtime error; float division by zero follows IEEE-754
  (`±inf`, or `NaN` for `0.0 / 0.0`); and modulo by any zero divisor always
  raises. The docs now state this, and new conformance fixtures
  (`errors/runtime/{int_div,int_mod,float_mod}_by_zero` plus
  `language/division_by_zero_float`) lock the runtime contract in.
- **Release runbooks now document the tag-push contract (#3009).** After the
  release-pipeline modernization (#2971–#2973), `publish-release.yml` no longer
  auto-tags `main` HEAD — the `vX.Y.Z` tag must be pushed at the release commit,
  and that tag push (not the merge) triggers `cargo publish` + binary builds. The
  four agent-facing runbooks (`.claude/commands/release-harn.md`,
  `.codex/commands/harn-release.md`, and both `.codex/skills/*/SKILL.md`) still
  described the old auto-tag behavior; they now document the explicit
  signed-tag-push step.
- **Agent loop no longer drops recovered tool calls on protocol violations (#3011).**
  When a model narrated prose before a tool call, the parser recovered the
  call but flagged the stray prose as a protocol violation, and the structural
  validator vetoed the whole turn — silently dropping the dispatchable call and
  looping until the stall detector fired. The well-formed check now only vetoes
  when no dispatchable call was recovered; genuine parse/schema errors still
  veto. Also: decode multi-byte UTF-8 in quoted/template tool-argument string
  values (was mojibaked one Latin-1 char per byte); flag the ollama `qwen3.6*`
  and openrouter `qwen/qwen3-coder*` text routes as `reserved_tool_call_token`;
  and soften the done-sentinel prompt's false "the runtime rejects it" claim.
- **Truncated tool calls are flagged instead of silently stalling (#3016).**
  When a model hit its output-token cap mid-argument while streaming a large
  tool call, the closing `</tool_call>` never arrived; the parser treated the
  unclosed open tag as an "unknown top-level tag" and the partial call body as
  stray prose, so the turn produced zero tool calls and the agent loop stalled
  (re-emitting until the stall detector fired, with no file written). The
  tagged parser now detects an unclosed `<tool_call>` open, recovers the tool
  name, and emits an actionable `TOOL CALL TRUNCATED … likely hit max output
  token limit` error so the loop/model can react instead of silently looping.
- **Harnpack and trigger budgets.** `harn run <bundle.harnpack>` now rejects
  manifest entrypoints that are absolute or escape the unpacked source tree,
  and trigger predicate cost averaging no longer overflows on large samples.

## v0.8.69

### Added

- **`[[contributes]]` host-surface extension manifest block (#2969).** A package
  manifest can now declare editor/host contributions (languages, preview panes,
  build profiles, commands, themes, …) alongside the agent-layer blocks. Harn
  treats `kind` as a host-owned, namespaced string and validates only the
  envelope plus that each contribution's `scopes` are a subset of
  `[package].permissions`, so new contribution kinds need no Harn release.
  `harn pack` carries the `[[contributes]]` block, package identity, and any
  contribution-referenced assets into the signed bundle (surfaced via
  `WorkflowBundlePreview.metadata`) so a host can discover and gate extensions
  from the verified artifact alone.
- Added `first_token_ms` to run profiles so `harn run --profile-json` and
  profile rendering expose LLM time-to-first-token when streaming deltas are
  observed.
- **Gemma 4 in the model catalog — local and hosted (#3000).** Google's Gemma 4
  (encoder-free unified multimodal, Apache 2.0) is now wired across the board:
  - **Local (Ollama):** the 12B on-device model as `gemma4:12b-mlx` (Apple
    Silicon), `gemma4:12b-nvfp4` (NVIDIA Blackwell), and `gemma4:12b-mxfp8`. The
    Ollama 12B quants are text-only at 128K (vision projector dropped, verified
    via `ollama show`), so a `gemma4:12b*` rule keeps them out of vision routing.
  - **Hosted:** the multimodal 26B MoE and 31B dense via the Gemini API
    (`gemini-gemma4-26b`/`-31b`), OpenRouter (`google/gemma-4-26b-a4b-it`,
    `google/gemma-4-31b-it` + aliases), and Together (`together-gemma4-31b`).
  - Fixes a latent bug where the `google/gemma-4*` OpenRouter/Together capability
    rules omitted `vision_supported`, so hosted Gemma 4 now correctly advertises
    image input. The 12B is on-device only and is intentionally not given a
    hosted route.

### Changed

- **`harn`, `harn-lsp`, and `harn-dap` are now one multi-call binary (#2972).**
  `harn-lsp` and `harn-dap` previously shipped as separate binaries that each
  re-linked nearly the same dependency closure as `harn`. They are now linked
  into `harn` and selected by the name the binary is invoked as (`argv[0]`), so
  the release ships a single binary with `harn-lsp` / `harn-dap` as symlinks to
  it (copies on Windows). `install.sh` and `harn upgrade` place them as on-disk
  symlinks too. Editors that spawn `harn-lsp` / `harn-dap` by path are
  unaffected; the change cuts the uncompressed install footprint by ~210 MB per
  Unix target and runs the release LTO link once per target instead of three
  times.
- **Release archives are now published per-architecture as each build finishes (#2973).**
  The GitHub release is created up front as a prerelease and each platform's
  archive is attached the moment that build leg completes, instead of waiting
  for the slowest leg and a final aggregate step. `SHA256SUMS`, the asset
  manifest, and the canonical "latest" flag are still applied only once every
  build succeeds, so `install.sh` and `harn upgrade` (which resolve "latest")
  are unaffected — but tooling that fetches a specific tag + architecture can
  now grab it as soon as it lands.
- **The default `release` profile builds faster (#2973).** `[profile.release]`
  moved from fat LTO + 1 codegen unit to thin LTO + 16 codegen units — the same
  optimization the release pipeline already ships — so local
  `cargo build --release` no longer pays for a fat-LTO link that never reached
  a published binary.
- **`harn testbench run --process-wasi` is now an opt-in build (#2974).** wasmtime
  and the cranelift JIT (~36 crates, ~8.6 MB of the stripped binary) are no longer
  linked into the default `harn` binary; they only powered the WASI subprocess
  toolchain mode. Every other testbench path — record/replay tapes, filesystem
  overlay, paused clock, LLM fixtures, fidelity scoring, annotations, and the
  conformance-test sidecars — is unchanged. To use `--process-wasi`, build with the
  feature: `cargo install harn-cli --features testbench-wasi`. Without it the flag
  returns a clear "requires the testbench-wasi Cargo feature" error.
- **`harn check` now runs the bytecode-compilation pass** (#3002), making it a
  true "will this run?" gate. Errors the type checker does not model but that
  stop `harn run` — unsupported nested `match` patterns, `break`/`continue`
  outside a loop, `try*` outside a function, malformed string interpolation —
  are now reported by `harn check` under the new `CMP` diagnostic category
  (`HARN-CMP-001`), instead of passing `check` and only failing at run time.
  Compilation runs only once a file is type-clean, so type errors still surface
  first.

### Fixed

- **`harn-lsp --version` / `harn-dap --version` print a version again (#2976).**
  After the multi-call binary collapse (#2972) the argv[0] shim dispatched a
  `--version`/`-V` probe straight into the stdio server, which hung waiting on
  input. The shim now answers `--version`/`-V` and `--help`/`-h` before starting
  the server, printing `<name> <version>` (matching `harn --version`). Normal
  editor LSP/DAP usage, which speaks the protocol rather than passing flags, is
  unchanged.
- **NaN comparisons and `.sum()` integer overflow (#2977).** Relational
  operators involving a floating-point NaN (`NaN <= x`, `NaN >= x`, and the
  int/float-mixed forms) now correctly evaluate to `false` instead of `true`
  at runtime and during constant folding, matching IEEE-754. `list.sum()` and
  `iter().sum()` now promote to a float when an integer sum exceeds the i64
  range — matching `abs`/`pow` — instead of silently wrapping (or panicking in
  debug builds for `iter().sum()`).
- **Models that reserve `<tool_call>` as a special token no longer collapse on the text tool format (#2978).**
  Qwen3.x and other Hermes-tool finetunes encode `<tool_call>`/`</tool_call>` as
  single special tokens. Reusing those exact strings as harn's text tool-call
  delimiters — embedded as instructional and wrapper text throughout the system
  prompt — drove such models into degenerate opener repetition
  (`<tool_call>\n<tool_call>\n…`). A new `reserved_tool_call_token` capability
  remaps the two colliding delimiters to a non-special bracket form on the wire
  and maps the response back (across streamed chunk boundaries) before the
  parser and transcript see it, so the canonical format is unchanged everywhere
  except the bytes sent to the model. Flagged for the llamacpp Qwen3 / Qwen3.6
  rules. Measured on Qwen3.6-35B-A3B: `<tool_call>` 0/5 no-collapse vs the
  remapped delimiter 5/5.
- **`list.unique()` now agrees with the `==` operator (#2979).** The
  structural hash key is consistent with `values_equal`: a float that is
  numerically an integer (and `-0.0` vs `0.0`) hashes like the equal integer,
  and numeric normalization now propagates into pairs, enum variants, and
  struct instances. De-duplication confirms hash hits with `values_equal`, so
  `[1, 1.0].unique()` collapses to `[1]` while two distinct `NaN`s are both
  kept — matching `==`, `contains`, and set membership.
- **`lint --fix` no longer unsoundly folds float self-comparisons (#2980).**
  The `pointless-comparison` lint (HARN-LNT-031) rewrote `x == x` to `true` and
  `x != x` to `false` unconditionally. That is wrong for floats: when `x` is
  NaN, `x == x` is `false` and `x != x` is `true` — and `x != x` is the
  idiomatic NaN test — so `harn lint --fix` could silently flip program
  behavior. The autofix is now gated on a conservative `is_nan_free` operand
  check and only fires when the operand provably cannot be a NaN float (a
  non-float literal, or a list/dict built only from such literals). For any
  operand that could be a float the lint still warns — a self-comparison is
  usually a typo — but leaves the source untouched and points at `is_nan(...)`.
- **The adaptive agent loop no longer cuts a productive task off on a planning turn (#2983).**
  The default loop-control policy extended the iteration budget only when a turn
  both made progress *and* issued a tool call this turn. But "progress" already
  includes visible text, so an interleaved planning/narration turn (no tool call)
  at the budget boundary was treated as no-progress and denied an extension —
  stopping a methodical model mid-way through a large multi-file task. The
  extension now keys off a single `progress` definition (the only place that term
  is defined), and the policy is expressed as an explicit, named, ordered rule
  table instead of inline compound conditions, so a second competing notion of
  "progress" can't be silently ANDed onto a rule again. Degenerate narration that
  never acts is still bounded by the iteration max and the stall detector.
- **String interpolation holes are parsed correctly (#2983).** An interpolation
  hole `${ ... }` holds exactly one expression, but several cases were handled
  wrongly:
  - **Trailing tokens were silently dropped.** The compiler parsed only the
    first expression and discarded the rest, so `"${a b}"` rendered just `a`,
    `"${40 + 2 zzz}"` rendered `42`, and `"${1e20}"` rendered `1` (`e20` lexes as
    a separate identifier — scientific notation is not a float literal).
    `Parser::parse_single_expression` now requires the whole hole to be consumed,
    and the VM compiler reports a malformed hole as a hard error
    (`invalid interpolation \`${...}\`: ...`) instead of silently rendering the
    raw text — `\${...}` already escapes a literal dollar-brace, so any
    unescaped `${...}` is intended as an expression.
  - **A `}` inside a nested string literal ended the hole early.** Hole capture
    is now string-literal aware, so `"${ items["a}b"] }"` and `"${ x ?? "a}b" }"`
    work instead of terminating at the first inner `}`.
  - **Escaping the nested quotes is reported clearly.** Inside a hole, string
    literals use bare double quotes (`${x ?? "y"}`); the escaped form
    (`${x ?? \"y\"}`) is now reported precisely at the backslash rather than
    being silently rendered as raw text.

  Valid single-expression holes are unaffected.
- **The stall detector now catches `agent_progress` spam (#3001).** A turn whose
  only tool call is a soft progress-report tool (`agent_progress`) reports status
  without advancing the task, but it was counted as real activity — so a model
  that emitted `agent_progress` repeatedly after getting stuck never tripped the
  no-progress detector. Such turns now count toward the no-progress streak (the
  same as a text-only monologue), so a run of them trips and the loop can recover
  or hard-stop. A turn that also makes a real tool call is unaffected.
- **Seven correctness fixes from a cross-referenced bug hunt (#3002).** Each
  mirrors a real defect in a comparable language runtime and was reproduced
  against the spec before fixing:
  - **Integer literals out of `i64` range are now an error** (`HARN-PAR-006`)
    instead of silently degrading to a lossy float. `9223372036854775808`
    previously became `9223372036854776000` typed `float`, and distinct
    out-of-range literals collapsed onto the same value. Write floats with a
    decimal point and the most-negative `int` as `-9223372036854775807 - 1`.
  - **`-0.0` and `0.0` are kept distinct in the constant pool**, so the sign of
    `1.0 / -0.0` (`-inf`) vs `1.0 / 0.0` (`inf`) no longer depends on which
    literal appeared first in the source.
  - **Out-of-range negative list-assignment is an error.** `a[-100] = x` used to
    silently clamp to index 0 and overwrite an unrelated element; it now raises
    the same out-of-bounds error as a too-large index. Reads and slices still
    clamp as before.
  - **`return`/`break`/`continue`/`throw` inside a `finally` no longer aborts
    the process.** It previously recursed forever at compile time and crashed
    with a stack overflow; non-local transfers now run each pending finally with
    that finally masked, and `return` in a `finally` overrides the body's
    outcome (Java/JS semantics).
  - **A `retry` expression evaluates to its body's value.** `let r = retry 3 { 42 }`
    returned `nil`; it now returns `42`, like `if`/`match`/`try`.
  - **A default parameter may reference an enclosing binding of the same name.**
    `let n = 7; fn f(n = n * 2)` threw `Undefined variable: n`; the default now
    reads the enclosing `n`. Defaults may still reference earlier parameters.
  - **Nested list/dict patterns in `match` arms are rejected with a clear error**
    (`HARN-CMP-001`) instead of being silently miscompiled to an equality
    comparison that bound nothing (or bound the wrong values). Bind the element
    with an identifier and match it in a nested `match`.

## v0.8.68

### Added

- Live LSP diagnostics now run configured rule packs and expose rule fixes
  through `harn.applyRepair`.
- Canonical connector event schemas are now authored once as Harn `type`
  declarations (`crates/harn-stdlib/src/stdlib/stdlib_event_schemas.harn`),
  and the Rust normalized-event structs are generated from them by the new
  hidden `harn connector-schema-codegen` command into a vendored
  `crates/harn-vm/src/triggers/event/schemas_generated.rs`. This inverts the
  source of truth so a `.harn` connector's output matches the Rust struct by
  construction. Wired into CI via `make gen-connector-schemas` /
  `make check-connector-schemas`. Proven end-to-end for GitHub with a
  round-trip parity test against the hand-written `GitHubEventPayload` family;
  the generated structs coexist with the hand-written ones, and switching the
  trigger boundary to produce the typed payloads is a follow-up.
- **`harn guard` - downloadable on-device injection-detection models (Layer 2,
  management).** A new `harn-guard` crate and `harn guard
  {list,install,status,remove}` CLI manage prompt-injection classifier models
  under `~/.harn/guard/`. The catalog points at already-hosted upstream models
  (Harn hosts nothing, bundles no weights); `install` fetches on the user's
  machine, verifies SHA-256 against the catalog's pinned digests, and requires
  explicit `--accept-license`. The default model
  (`deberta-v3-prompt-injection-v2`) is Apache-2.0 and ungated; gated models
  are opt-in and require the user's own `HF_TOKEN`. The neural inference
  runtime is behind the off-by-default `guard-neural` cargo feature, so the
  default binary stays lean and falls back to the built-in heuristic classifier.
- **On-device neural injection classifier (Layer 2, inference).** `harn-guard`
  gained the ONNX inference backend behind the off-by-default `guard-neural`
  cargo feature: it loads an installed model package (`~/.harn/guard/<name>/` -
  ONNX graph + `tokenizer.json` + `config.json`) and scores untrusted content
  with a transformer sequence-classifier, superseding the built-in heuristic for
  better recall. The runtime resolves the model named by the new
  `[security] guard_model` config key lazily on the first scored span via a new
  `harn-vm` loader seam (`set_injection_classifier_loader` /
  `ensure_neural_classifier`) that keeps `harn-vm` free of any inference
  dependency. A transient inference error degrades to the heuristic rather than
  dropping detection. The default binary links no model runtime; CI never
  downloads weights.

### Fixed

- **harn-guard store tests no longer share temp directories under nextest
  (#2961).** The model-store tests now use owned temporary directories, avoiding
  parallel release-gate collisions that could make corruption checks fail
  nondeterministically.
- **Multi-line `/* ... */` block comments now report their span at the opening
  `/*` instead of the closing `*/`.** The lexer had stamped the span's start
  line with the comment's end line, so every consumer that keys off it - the
  `harn fmt` comment map, LSP positions, and the `legacy-doc-comment` lint
  rule - misattributed multi-line block comments. The span now records the open
  line/column with `end_line` at the close, mirroring how multi-line strings are
  recorded.
- **`harn fmt` no longer drops a trailing same-line comment on a top-level
  statement or import.** A comment like `let x = 1 // note` at the top level was
  silently discarded; block bodies already preserved these, but `format_program`
  never attached them. Top-level items now keep their trailing comment inline.
- **Pool `max_concurrent` could transiently over-admit under concurrent
  dispatch.** A worker-pool dispatcher popped a queued task under the pool lock
  but inserted it into the active set under a separate lock hold, so a `submit`
  racing a finishing task's `finalize_task` could both admit into the same free
  slot. Dispatchers now reserve the slot at pop time and the admission check
  counts in-flight reservations, so the cap holds strictly.

### Security

- **On-device injection detection (`mode = "local-ml"`).** Untrusted content is
  now scored by a pluggable injection classifier and the verdict (`model`,
  `score`, `flagged`) is recorded on its taint record, so approval UI and audit
  trails can show why a span looks risky. The built-in `heuristic-v1` classifier
  is always available, dependency-free, and precision-first; a downloadable
  neural model (`harn-guard`) can supersede it via
  `register_injection_classifier` without the default binary linking a model
  runtime. A flagged verdict tightens the trifecta gate: in addition to
  exfil/destroy/secret-read, a flagged injection plus a workspace-mutating tool
  is now gated too. Detection never weakens the gate. Configure via
  `[security]` (`detect_injection`, `guard_threshold_percent`) or
  `std/security::local_ml()`.

## v0.8.67

### Breaking

- **`-2 ** 2` now evaluates to `-4`, not `4`.** A unary minus on the base of an
  exponentiation now binds looser than `**`, so `-2 ** 2` parses as `-(2 ** 2)`,
  matching Python, Ruby, and ordinary math notation rather than the spreadsheet
  `(-2) ** 2` reading. The exponent operand still accepts a unary prefix
  (`2 ** -3` is `2 ** (-3)`), and `**` stays right-associative. Wrap the base in
  parentheses — `(-2) ** 2` — to keep the old result.

### Fixed

- **The tree-sitter grammar now matches the canonical parser's operator
  precedence.** `??` was mis-ordered as looser than `||`/`&&`/`+`/`*` (it binds
  tighter than `+ -` and looser than `* / %`), and unary prefixes were tied with
  `**`. Structural tooling (Neovim highlighting, AST-based edits) now groups
  `a ?? b + c` as `(a ?? b) + c` and `-2 ** 2` as `-(2 ** 2)`, the same as the
  interpreter.
- Tree-sitter highlighting now covers `break`, `continue`, `require`, and the
  HITL keywords (`ask_user`, `dual_control`, `escalate_to`,
  `request_approval`). They are valid grammar keywords but were silently
  rendered as plain identifiers in Neovim and other tree-sitter editors.
- The VS Code TextMate grammar now nests block comments, so a `*/` inside a
  `/* ... */` no longer ends the comment early, and recognizes raw string
  literals (`r"..."`), which previously orphaned the `r` prefix and
  mis-highlighted backslashes as escape sequences.

### Security

- **Prompt-injection defense substrate (`[security]` config + `std/security`).** The runtime now
  spotlights untrusted external content and gates the lethal trifecta. Tool/MCP output that crossed a
  trust boundary (an external MCP server, or a `Fetch`-kind tool reaching the open internet) is framed
  in datamarked delimiters with a provenance banner so the model treats it as data, never as
  instructions (Microsoft "spotlighting"). A per-session taint ledger records that untrusted content
  entered context; when it has, an auto-allowed tool that can exfiltrate (network/fetch), destroy
  state, or read a secret file is upgraded to an interactive confirmation — but only where an approval
  policy is installed, so headless embedders are unaffected. MCP tool schemas are pinned and hashed on
  `tools/list`; a description/inputSchema that changes after first sighting is flagged
  (`_schema_changed`) for re-approval (rug-pull defense), and `session/request_permission` now carries
  the full tool descriptor so hosts can render the complete model-visible tool text at approval time
  (closing the tool-poisoning visibility gap). Configure via `[security]` (`mode = off | spotlight |
  strict | local-ml`, `trifecta_gate`, `pin_mcp_schemas`, `gate_secret_reads`, `trusted_mcp_servers`)
  or `std/security::configure`. Defaults are on (spotlight + gate + pinning).

## v0.8.66

### Changed

- **Docs deploys are decoupled from the rest of CI.** The `deploy-docs` job now gates on the
  `docs-site` build succeeding instead of the whole `ci-status` graph, so a docs/site change
  publishes to harnlang.com whenever the site builds green — independent of unrelated backend or
  test lanes. Previously a docs change that merged while `main` was red elsewhere (e.g. a flaky
  Windows test) had its Render deploy silently skipped and never re-fired. Added a `website/README.md`
  documenting the site's stack, build, and deploy flow.
- **harnlang.com is now a custom Vite + React + Tailwind site, replacing mdBook (full cutover).** The
  Diataxis-structured Markdown under `docs/src/` is unchanged — it is now rendered by a new app in `website/`
  with a Mintlify-style layout: a marketing landing page, full-width section tabs, a scoped sidebar, an
  on-this-page TOC, ⌘K full-text search, light/dark themes, and a redesigned look-and-feel (teal + amber brand).
  Harn code blocks are syntax-highlighted at build time using the same Rust-generated keyword table
  (`docs/theme/harn-keywords.js`), and every page is statically prerendered to crawlable HTML with the raw `.md`
  mirror and legacy redirects preserved. `scripts/build_docs_site.sh` now drives the Node build; mdBook
  (`book.toml`, the bundled theme JS/CSS) is removed. Render must build with `./scripts/build_docs_site.sh`
  (publish dir `docs/dist/`).

### Fixed

- De-flaked and de-slowed several tests that relied on wall-clock timing or
  process-global state. The `ResourceGate` scheduler tests now assert gate
  state in-process via a non-blocking `try_acquire` instead of
  `thread::sleep`-coaxed thread ordering, the worker-snapshot round-trip test
  uses an explicit path instead of mutating the global `HARN_WORKER_STATE_DIR`
  env var, and diagnostic color in tests is forced off via a thread-local
  override instead of the process-global `NO_COLOR` env var.
- `harn test` can now fail passing tests that exceed explicit wall-clock or execute-phase budgets.
- Increased `harn test --parallel` worker thread stack size so deep agent/runtime tests do not crash with a Rust stack overflow.
- **File event log `flush()` no longer fails on Windows.** `sync_tree` opened each topic/consumer
  file read-only and called `sync_all()`, which on Windows lowers to `FlushFileBuffers` — and that
  requires a write-access handle, so it failed with "Access is denied" (on Unix, `fsync` on a
  read-only descriptor is fine, which masked the bug). `flush()` now fsyncs through a hardened
  `fsync_file` helper that opens for write, with a read-only fallback so a durability flush can never
  hard-error on a genuinely read-only file. Fixes the `session_timeline::persisted_file_log_reads_agent_events`
  failure that was red on the Windows CI lane (and blocking docs auto-deploys).

## v0.8.65

### Fixed

- Fixed the built-in `destructure-defaults` codemod path so
  `harn codemod --rule-pack std/rules/destructure-defaults` folds Harn
  statement runs, including aliased bindings.
- **Loop guard break/continue scope preservation.** Harn no longer loses
  same-loop `let` bindings after an `if { break }` or `if { continue }` guard,
  preventing spurious undefined-variable runtime errors.

## v0.8.64

### Added

- Added `harn rule test` and the `harn_rules::testing` harness (Rule Engine Epic B) — a first-class way to test
  structural rules with **inline annotations** (the Semgrep convention, made language-agnostic): a fixture line
  preceded by `// ruleid: <id>` must match the rule, a `// ok: <id>` line must not, and any un-annotated match is a
  false positive. A rule `foo.toml` pairs with a fixture `foo.<ext>`; `harn rule test <dir>` runs every rule against
  its fixture, prints per-fixture pass/fail (or a `--json` envelope), and exits non-zero on failure (a CI gate).
- Added project-local rule discovery (Rule Engine Epic B) — declare `[rules] ruleDirs = ["rules"]` in `harn.toml` and
  `harn scan` / `harn codemod` load every `*.toml` rule from those directories when no `--rule`/`--rule-pack` is given.
  `harn scan src` runs the project's rules over `src` (no inline pattern needed — an inline pattern is signalled by
  `--lang`); `harn codemod src` applies the codemod rules in the pack and silently skips lint/search rules. Resolved
  relative to the manifest directory; `utilDirs`/`testConfigs` keys are parsed for forward-compat.
- Added the rule-pack registry surface (Rule Engine Epic B): `harn rule publish` validates `[rules] ruleDirs`, marks the
  package-index row with rule-pack metadata, and delegates to the existing package publish flow; `harn rule search`
  filters the package registry to rule packs and shows languages, rule count, safety summary, and descriptions. `harn
  scan` / `harn codemod --rule-pack <name>` now resolves both installed package aliases and canonical registry names
  such as `@scope/name` from `harn.lock`, while local rule-pack directories still work.
- `harn codemod` now runs a **`harn fmt` post-pass** on rewritten `.harn` files (Rule Engine Epic C), so a codemod
  batch lands fmt-stable — dict-pattern key order, spacing, and trailing commas are normalized and a later `harn fmt`
  is a no-op. Built into `rules.apply` (on by default; `format: false` opts out; the result reports a `formatted`
  flag), so the dry-run preview shows the final formatted output and every caller — the CLI and the cloud worker —
  gets it for free. Only `.harn` is formatted (it is the only language with a bundled formatter). The lint-`--fix`
  interplay half of #2847 (running the linter after fmt) is tracked as a follow-on now that engine rules are lint
  rules (#2849).
- Rule-engine rules now run as **lint rules** (Rule Engine Epic C). A declarative `harn-rules` rule (a `*.toml`
  pattern) placed in a project's `[rules] ruleDirs` and targeting `language = "harn"` shows up in `harn lint` output
  indistinguishably from a built-in lint — same `disable_rules` filtering, same `--fix` plumbing, reported under
  `HARN-LNT-059` with the rule's own message/severity and id. Built on the new Harn tree-sitter grammar (#2888), so
  structural rules finally match `.harn` source. (`harn lint` / `harn lint --fix` / `harn fix` / the JSON report all
  load them.)
- Custom lint rules can now be authored in Harn (Rule Engine Epic C, #2850) — the ESLint-plugin equivalent. Drop a
  `*.lint.harn` module exporting `pub fn lint(source) -> [finding]` into a `[rules] ruleDirs` directory and `harn lint`
  discovers it, runs it over every linted file, and merges its findings into the normal output (same exit code, same
  `--json` report, same `disable` filtering). Rules run in a read-only sandbox — the language, stdlib, and the
  structural rule engine, but no filesystem / network / process access — and a buggy rule fails safe: its error
  becomes a diagnostic instead of crashing the linter.
- Exposed linting to the VM and gave projects per-rule control (Rule Engine Epic C, #2851). A new `lint.run` host
  builtin (`std/lint` `lint_run`) lints a Harn source string and returns structured diagnostics — `{code, rule,
  message, severity, line, column, …}`, the same findings `harn lint` emits — so an agent / IDE / cloud worker can
  lint without shelling out. And `[lint.severity]` in `harn.toml` promotes/demotes any rule (built-in or engine) —
  `unnecessary-parentheses = "error"` — applied after disable-filtering and reflected in both the CLI and `lint.run`.
- **Native lint rule libraries (#2852).** `harn lint` can now load trusted
  dynamic rule libraries from `[rules] nativeRuleDirs`, run their diagnostics
  through the same lint output/JSON/fix path as built-ins, and expose the
  authoring ABI as `harn_lint::native`.
- Added Harn-only semantic rule captures with `resolvesTo` / `type` `[[where]]`
  filters and `capture_metadata` output for resolved binding identity and
  simple static type labels.
- **Harn structural rule scanning.** The rule engine now ships the Harn
  tree-sitter grammar through `harn-hostlib`, so `harn scan` and saved TOML
  rules can structurally match `.harn` source with `language = "harn"` (#2888).
- **Session timelines can now be queried and streamed from Harn-owned
  observability data (#2913).** Harn projects persisted run spans, agent events,
  and channel lifecycle/audit receipts into a redacted timeline shape with
  parent/child span links, channel emit→match links, and ACP query/subscribe
  methods for live clients.

### Changed

- Dogfooded destructuring-with-defaults across the `.harn` stdlib (Rule Engine): 26 files where runs of consecutive
  `let x = src?.x ?? d` sharing one source are folded into a single `let { … } = src ?? {}` (net −45 lines).
  Behavior-preserving — the `?? {}` guards a nil source (bare destructure of nil throws), conformance stays at
  1548/0, and no new lints. Powered by a new reusable fold codemod: `harn_rules::fold::fold_destructure_defaults` +
  the `rules.fold` host builtin / `std/rules` `rules_fold` (the engine can't fold statement runs declaratively, so
  this is a specialized transform — the same one that drives burin-code#1629). Closes #2824.
- `harn rule test` with no path now discovers the project's rules from `[rules] ruleDirs` in `harn.toml` (Rule Engine
  Epic B) — `harn rule test` becomes a zero-config CI gate for a project's rule pack. Scoped to the declared dirs, so a
  stray non-rule `*.toml` elsewhere in the tree is not swept up. An explicit `harn rule test <path>` is unchanged.
- **Reminder prompts now preserve local model prefix-cache stability (#2903).**
  Non-Anthropic reminder text is appended after the message history instead of
  mutating the leading system prompt, so llama.cpp-style prefix caches stay warm
  when reminder sets change.
- DRY / leaky-abstraction cleanup alongside the fixes above:
  - `harn-cli` now depends on the `hex` crate (as the rest of the workspace already does) and the two hand-rolled hex
    encoders (`registry::hex_bytes`, `skill_provenance::hex_encode`) were removed in favour of `hex::encode`.
  - `outline`'s `extract_rust` now delegates to the shared `extract_with_prefixes` helper instead of inlining a
    verbatim copy of its prefix-matching loop, matching every other per-language extractor.
  - `fs_watch` dropped its private re-implementation of `optional_string_list` and reuses `value_args::optional_string_list`.
  - Removed a dead, fully-subsumed early-return branch in the LSP `line_byte_range` helper.

### Removed

- **Provider catalog no longer advertises the broken qwen3.6 Ollama routes (#2901).**
  Local qwen3.x recommendations now point at the validated llama.cpp provider
  path instead of Ollama aliases whose server-side tool-call parser fails.

### Fixed

- **Project script lint rules now stay scoped to their owning package (#2826).**
  Multi-target `harn lint` runs no longer apply a sibling package's
  `*.lint.harn` rules to unrelated files, and script rules can return direct
  findings or `rules_diagnostics(...)` results without losing fail-safe
  diagnostics.
- Cross-cutting correctness + performance sweep:
  - `fs_snapshot`: a snapshot whose captured bytes exceed the whole session byte cap is no longer evicted by the very
    call that created it — `enforce_byte_cap` now protects the snapshot currently being written, fixing a panic in
    `snapshot()` (it re-fetched the just-evicted snapshot) and silent loss of rollback for an in-flight write.
  - `fs_snapshot`: `atomic_write` no longer leaks its temp file when both the rename and the
    remove-then-rename retry fail.
  - Package registry: `harn add <pkg>` with no version constraint now resolves the highest **stable** release rather
    than letting an `x.y.z-rc.1` prerelease shadow it (matching cargo/npm); packages that have only ever published
    prereleases still resolve to the highest prerelease.
  - `harn scan` regex rules: row/column for each match is now computed with a single forward cursor instead of
    rescanning the document from byte 0 per match — the matcher was O(matches × file length) on files with many hits.
  - Deterministic `search` tool: the compiled `RegexMatcher` is now borrowed per file instead of deep-cloned, so a
    repo-wide scan no longer re-copies the compiled regex program once per file.
- Release container publishing now verifies packaged Linux archives without `pipefail` false negatives.

## v0.8.63

### Breaking

- **Synchronization permits and channel close now fail closed (#2800).**
  Permits returned by `sync_*_acquire` are scope-owned RAII guards that release
  on scope exit, `return`, and `throw`; `sync_release` remains idempotent for
  earlier release. Direct blocking `send` now raises structured
  `ChannelClosed` (`channel_closed`) after close, and direct `receive` drains
  buffered values before raising `ChannelClosed` on a closed empty channel.

### Added

- Added the `harn-rules-hostlib` crate and the `std/rules` stdlib module. The
  rule engine is now callable from Harn: `rules.search` returns capture-bound
  matches, `rules.report` returns a data table, and `rules.apply` applies
  dry-run-default, safety-aware codemods from TOML rule source.
- `harn-rules` patterns gained **typed placeholders** `$VAR:kind` for syntax
  class constraints such as `expr`, `stmt`, `ty`, and `ident`, including the
  full declarative path through `rules.search` and `rules.apply`.
- Added `harn scan`, a read-only structural search and lint command over
  gitignore-aware filesets. It accepts inline patterns, saved rules, and rule
  packs, and can print grep-style matches, per-file reports, or a JSON envelope.
- Added `harn codemod`, a dry-run-default command for applying codemod rule
  fixes across filesets. `--apply` writes changes behind the deterministic-tools
  capability gate, and unsafe rules require `--allow-unsafe`.
- Added a curated `rules/std` seed rule pack with the `destructure-defaults`
  codemod and `no-console-log` lint, including annotation fixtures and seed-pack
  tests.
- Added the embedded `harn-rules` skill and rule engine cookbook for authoring,
  searching, applying, and running rules from Harn scripts.
- `std/rules` gained the imperative `rules.visit` visitor for per-match Harn
  callbacks, plus `rules.diagnostics` for declarative rule diagnostics.
- VM `match` list patterns now support a trailing `...rest` element, mirroring
  `let` destructuring for at-least-N matches.

### Fixed

- **Release gates are more deterministic (#2887).** Unix-socket invalid-JSON
  tests avoid client/server write races, hook-session file edit tests use
  isolated temp files, release PR drift checks reject stale main pins, and the
  `harn-rules-hostlib` lockfile entry matches the workspace version.
- `harn-rules` codemod `apply` no longer panics or corrupts files when a rule
  pattern produces nested or overlapping matches; the engine keeps the
  outermost match and rewrites each region once.
- VM fixed-arity `match` list patterns are now exact-length. Use `[a, ...rest]`
  for at-least-N matching.
- VM string-repeat, padding, and related repeat helpers now share an allocation
  guard and return a clean runtime error for oversized output instead of
  exhausting memory or panicking.
- `harn-hostlib` session component sanitization no longer lets all-dot session
  ids (`.`, `..`) pass through verbatim.

## v0.8.62

### Added

- `harn-rules`: added the relational + composite matching algebra (Rule Engine Epic A). A rule's `[rule]` block is now
  a recursive node that combines an atomic leaf (`pattern` / `kind` / `regex`) with relational constraints (`inside`,
  `has`, `follows`, `precedes` — each tuned by `stopBy` and `field`) and composite combinators (`all`, `any`, `not`,
  and `matches` referencing a `[utils]` utility rule); every key is ANDed. A tree-walking evaluator threads metavar
  bindings through the relational context. Metavar-free patterns are now valid literal patterns (`foo()` matches calls
  to `foo`).
- `harn-rules`: added the predicate + rewrite layer (Rule Engine Epic A). Rules can now narrow matches with `where`
  constraints (metavar regex, numeric/string comparison, and recursive sub-pattern), synthesize new metavars with a
  `transform` pipeline (regex `replace`, `substring`, and case `convert`), and rewrite with a `fix` template that
  interpolates `$VAR` / `${VAR}` from captured and transformed metavars. `CompiledRule::apply` runs a codemod —
  filtering by constraints and splicing per-match fixes format-preservingly, the same byte-splice as `ast.batch_apply`.
- `harn-rules`: added fix safety classification and the apply gate (Rule Engine Epic A). A rule declares a `safety`
  tier (`format-only` → `behavior-preserving` → `scope-local` (default) → `surface-changing` →
  `capability-changing` → `needs-human`); the two safest are machine-applicable, the rest suggestions. `auto_apply`
  refuses to silently apply anything riskier than `behavior-preserving`, `apply_checked` additionally asserts the fix
  is idempotent (re-running it reaches a fixed point), and `diagnostics()` emits per-match diagnostics (message,
  severity, span, applicability, interpolated fix) — the surface the linter and LSP convert into
  `LintDiagnostic` / `FixEdit`.
- `harn-rules`: added the whole-project scan → accumulate → edit lifecycle (Rule Engine Epic A), adapted from
  OpenRewrite's `ScanningRecipe`. A `ScanningRecipe` reads the entire fileset into a typed accumulator in a `scan`
  pass (deterministic, path-sorted), then a `generate` pass turns that state into `FileChange`s — edit, **create**, or
  **delete** — so rules can act on a whole-project view (import insertion, codegen, cross-file dead-code removal), not
  just in-place edits. Per-file declarative codemods plug in via the `RuleRecipe` adapter.
- `harn-rules`: added report-only data tables (Rule Engine Epic A), adapted from OpenRewrite's first-class data
  tables. `data_table(rule, files)` runs a rule across a project without editing and returns a columnar `DataTable` —
  one row per match (path, position, matched text, metavar bindings) plus a metrics summary (total findings, files
  touched, per-file counts). The envelope serializes to JSON (`to_json` / `to_json_value`) for inventory, impact
  analysis, and audit — e.g. the destructuring rule's "sites / files / alias-count" measurement, produced
  automatically.
Added `std/command` helpers for waiting on background command handles and teeing command output while a process runs.
- Added `std/net.unix_socket_json_request` for script-level JSON line requests over local Unix-domain sockets.

### Changed

`tools.cancel_handle` can now optionally wait for the background waiter to finish
draining command artifacts and return the final canceled command result.
Locked compact and pretty JSON artifact output to stable sorted object-key order with conformance coverage.

### Fixed

- **Contextual closure typing now checks closure bodies against expected function slots (#2859).**
  Closures assigned to typed bindings, returned from typed functions, stored in typed
  fields, or passed to typed callbacks now inherit parameter and return expectations
  without making partially inferred `_` collection elements noisy.
- **Cerebras GPT-OSS accepts `reasoning_effort="none"`.**
  The Cerebras `gpt-oss-*` capability rule advertised `reasoning_effort_supported`
  without `reasoning_none_supported`, so an "off" reasoning level floored at
  `minimal` — which Cerebras rejects with `HTTP 400 reasoning_effort: Input
  should be 'none', 'low', 'medium' or 'high'`. That broke every no-tools /
  summarize turn on `cerebras/gpt-oss-120b` (e.g. the release-harness
  audit-finalize turn). The route now advertises `none` as the true
  reasoning-off value.
- **Release artifact packaging now works on macOS Bash with strict shell mode.**
  The release binary workflow no longer expands an empty optional-binaries array
  when packaging non-Linux archives.

## v0.8.61

### Breaking

- **`mutex { ... }` blocks are no longer one process-wide lock.** A bare
  `mutex { ... }` now keys on its own lexical call-site, so two *distinct*
  `mutex {}` blocks run concurrently instead of silently serializing against
  each other. To guard a shared resource, name it: `mutex(resource) { ... }`
  acquires a lock keyed on the resource's structural value, so every block
  naming the same resource mutually excludes regardless of where it appears.
  Code that relied on every `mutex {}` contending on a single global lock must
  switch to an explicit shared key. Re-acquiring the *same* key on one task
  still raises `HARN-ORC-011` (self-deadlock), and locks are still released
  automatically on scope exit and on `throw`.

### Added

- **`harn run` now accepts `--read-only-root`.** CLI now accepts one or
  more `--read-only-root <path>` arguments to allow read-only access to
  additional filesystem roots while preserving default run sandbox and
  network egress guards. This enables maintenance scripts to consume
  resources outside the workspace without `--no-sandbox`. (#2779)
- **Tool-policy and permission denials now carry a structured `denial`
  record (#2780).** When a capability/policy ceiling, argument allow-list,
  dynamic permission rule, approval decision, or pre-tool hook refuses an
  agent tool call, the denied `tool_result` and the `PermissionDeny`
  transcript event now include a `denial` object with the refusing `gate`
  (`tool_ceiling`, `capability_ceiling`, `side_effect_ceiling`,
  `arg_constraint`, `dynamic_permission`, `approval_policy`,
  `approval_unavailable`, `host_rejected`, `hook_deny`), the exceeded
  `capability`, any `denied_paths`, and a `retryable` flag (always `false`
  today — these gates are terminal). Host harnesses can fail or pivot on a
  terminal denial without spending another model call re-parsing prose, and
  the loop's stall detector counts repeated terminal denials as a loop.
- **Built-in `chars(text)` for linear-time source scanning (#2790).** Strings
  are stored as UTF-8, so `s[i]`, `s[a:b]`, `s.count`, and `substring(...)` are
  each O(n) in the string length — a per-character cursor loop built from them is
  O(n²) and stalls on multi-kilobyte source files. `chars(text)` (and the
  existing `text.chars()` method) materializes a string into a list of
  single-character strings in one linear pass; ASCII characters are interned, so
  the materialization is allocation-free and `s[i]` / `text.char_at(i)` no longer
  allocate per access. Scan the resulting list with O(1) indexing. See the new
  "Scanning large text" guidance in the builtins reference and cheatsheet.
- **`verify_completion_judge` now caps repeated vetoes per session (#2791).**
  A `max_invocations` (alias `max_feedback`, default `5`) ceiling stops a weak
  model from burning paid completion-judge calls forever. Once the cap is hit
  the judge stops firing and the loop ends with status `verify_capped` and
  stop_reason `completion_judge_cap_reached`. The result now carries a
  structured `completion_judge` block (`invocations`, `vetoes`,
  `max_invocations`, `cap_reached`) so harnesses can report judge churn without
  transcript mining. Set `max_invocations: 0` to disable the cap.
- Detect provable channel wait-for deadlocks across tasks and raise `HARN-ORC-012` instead of hanging forever.
- Added native `find_text` / `harness.fs.find_text` source-tree text search for pure-Harn lint and guard scripts.
- Added `ast.search` / `std/ast` `ast_search` — read-only structural search that runs a tree-sitter query against a
  file or inline source and returns every match with every capture (`@name`) bound (text + byte/row/col range). The
  structural complement to `fs.find_text` and the read primitive of the rule engine.
- Added `ast.batch_apply` / `std/edit` `edit_batch_apply` — a multi-file codemod runner that applies one
  tree-sitter query→`replacement` across a list of paths, returning a per-file preview plus a roll-up summary.
  Dry-run is the default (writing requires `dry_run: false`), the byte-splice preserves formatting outside each
  match, per-file failures are isolated rather than aborting the batch, and re-running an applied codemod reports
  zero further changes (the idempotency hook). Shares its span/select/splice machinery with `ast.apply_node`.
- Added the `harn-rules` crate — the declarative structural rule engine core (Rule Engine Epic A). It ships the
  atomic matching tier: a serde rule model (`id` / `language` / `severity` / `message` / `rule` block / `fix`), a
  pattern compiler that lifts `$VAR` metavariable snippets into tree-sitter queries (operator-precise, with repeated
  metavars unifying), `kind` and `regex` matchers, and a TOML rule-file / directory loader. Rules run through the same
  tree-sitter machinery as `ast.search`, producing matches with metavariable bindings.
- **Self-deadlock detection (`HARN-ORC-011`) now spans inline async-builtin
  boundaries.** Acquiring a `mutex` that an ancestor already holds — when that
  ancestor is parked awaiting the async builtin or closure you're running inline
  — is a provably-unresolvable self-deadlock (the sole holder is blocked waiting
  on you). The VM now propagates an ancestor's held-lock keys into such inline
  children and raises `HARN-ORC-011` instead of hanging forever. New concurrent
  tasks (`spawn`, `parallel`, triggers) deliberately do *not* inherit, since
  blocking on a parent-held lock there is legitimately resolvable.
- **The VM now detects deterministic self-deadlocks instead of hanging
  forever (HARN-ORC-011).** Re-entering a `mutex { … }` block on a lock this
  task already holds — directly or through a called function — and `await`ing
  a task's own join handle previously blocked the VM indefinitely with no
  diagnostic. Both now raise a clear, catchable error before blocking. Run
  `harn explain HARN-ORC-011` for guidance. (Cross-task wait-for-graph and
  builtin-`sync_*`-path detection remain follow-ups.)
- **Single source of truth for generated-artifact drift checks.**
  `scripts/generated_artifacts.toml` now registers every "source of truth ->
  generated file + drift check" pair in one place, and `make
  check-generated-registry` fails the build when the registry disagrees with
  the Makefile `all:` recipe, the CI workflows, or the declared output files.
  A new `gen-*`/`check-*` pair can no longer silently skip CI. Added a
  cross-crate `make check-tree-sitter-keywords` guard (with `make
  gen-tree-sitter-keywords`) so the tree-sitter grammar's reserved-word set
  cannot drift from the lexer `KEYWORDS` const. Both guards run in CI and in
  the pre-commit / pre-push hooks.
- **`harn lint` and `harn fmt --check` now point you at the auto-fix flag.**
  When findings are machine-fixable, lint prints an ESLint-style summary
  ("All N finding(s) are auto-fixable — run `harn lint --fix` to apply them.",
  or "M of N …" when only some are), and `fmt --check` reports that N files
  would be reformatted and to re-run `harn fmt` without `--check`. The hint is
  stderr-only and never prints once the fixes have been applied; the `--json`
  report shape is unchanged.
- harn-lsp adds a `harn.applyRepair` `workspace/executeCommand` that resolves a
  code action's `repair_id` into a `WorkspaceEdit` on demand, so editors can
  apply repair-backed fixes that ship without an inline edit.
- **Structured concurrency: `scope { ... }` nurseries.** Tasks spawned inside a
  `scope { }` block are joined when the block exits — so a spawned task can no
  longer outlive its scope unnoticed, and an error in any of them is no longer
  silently swallowed. At scope exit the first failing task's error propagates
  out of the block (catchable with `try`) after its siblings are cancelled; on a
  `throw`/`return`/`break` out of the block the bound tasks are cancelled rather
  than leaked. Explicitly `await`-ing a task removes it from the nursery (no
  double-join). A bare `spawn` with no enclosing `scope { }` keeps its previous
  detached behavior, so this is additive. `scope` is a contextual keyword —
  existing identifiers, dict keys, and properties named `scope` are unaffected.
- **`std/testing` gains LLM-mock builders, error assertions, and
  filesystem fixtures.** New helpers cut the boilerplate that kept most
  fixtures on the raw, unscoped form:
  - LLM turn builders — `llm_text`, `llm_done`, `llm_error`,
    `llm_tool_call`, `llm_tool_calls`, and `with_llm_script` — make the
    already-scoped `with_llm_mocks` readable, replacing the
    `llm_mock_clear()` + sequential `llm_mock({...})` pattern.
  - Error assertions — `assert_throws`, `assert_error_contains`, and
    `assert_no_throw` — collapse the
    `try {...}` + `is_err` + `unwrap_err` + `to_string` + `contains`
    chain into one call.
  - Filesystem fixtures — `with_temp_dir`, `with_fs`, and the unified
    `with_scenario` — give a scoped temp workspace (optionally seeded
    from a `{path: contents}` dict) with guaranteed recursive cleanup,
    the fs counterpart to `with_host_mocks`.

### Changed

- **Much faster repo-scale text scans (#2796).** The regex builtins cached the
  compiled pattern but handed each call a *deep clone* of `regex::Regex`, which
  re-copies the compiled program and its match-cache pool (~3.5us/call) — the
  dominant cost when running `regex_match` over every line of a source tree.
  The cache now shares each compiled pattern via `Rc`, so a repeated lookup is
  a refcount bump, and a single-slot memo skips the cache-key allocation when
  the same pattern is reused in a loop. `regex_match` / `regex_replace` /
  `regex_captures` / `regex_split` and the `contains` / `starts_with` /
  `ends_with` / `split` / `replace` / `index_of` string methods also borrow
  their subject/needle instead of cloning it. A line-oriented scan of the
  repository (~740k `regex_match` calls) drops from ~4.0s to ~1.2s, making
  TypeScript-style source-tree scanners practical to port to Harn.
- **Iterator/collection combinators now infer their element type, and `map`/`flat_map`
  thread the closure's return type.** Previously `xs.map(...)`, `xs.filter(...)`,
  `xs.sort()` and `dict.keys()`/`values()`/`entries()` collapsed to the opaque
  `list`/`dict` type — only the lazy `Iter` combinators carried `T`, so a single
  eager combinator erased element typing for the rest of a chain. Now eager
  combinators preserve or transform the element type (`list<T>.filter(…) →
  list<T>`, `list<T>.map(f) → list<R>` where `R` is `f`'s inferred return,
  `dict<K,V>.entries() → list<Pair<K,V>>`, …), matching what the equivalent
  `.iter()`-bridged chain already produced. `map`/`flat_map` infer the closure
  body's return type with the closure parameter bound to the receiver's element
  type, so `[1,2,3].map({ n -> "v${n}" })` is now `list<string>`. Applies to both
  eager (`list`/`dict`) and lazy (`iter`) receivers.
- **Destructured rest patterns now preserve the source's element/value type.**
  `let [head, ...rest] = xs` types `rest` as `list<T>` (was the opaque `list`),
  and `let { a, ...rest } = d` keeps `dict<K, V>` when the source dict is
  parameterized. Completes the destructuring type inference so iterating or
  indexing a rest binding recovers a precise element type.
- **Destructuring binds now infer the same types as the `?.`/`??` form they
  desugar to.** A destructured binding such as `let { path = "", retries = 0 }
  = opts` previously left `path` and `retries` untyped; the type checker now
  infers each binding from `field + default` exactly as the equivalent
  `opts?.path ?? ""` / `opts?.retries ?? 0` would — present shape fields keep
  their declared type, a `nil` default stays optional (`T | nil`), and the
  default's type carries through when the source dict is untyped. This makes
  migrating the pervasive `let x = input?.field ?? default` idiom to a single
  destructuring bind lossless under the type checker. Applies to `let`, `var`,
  and `for`-`in` patterns. See the new "Destructure with defaults" cookbook.
  (Positional/tuple-precise element types for list patterns remain a
  follow-up.)
- **Un-annotated function return types are now inferred from the body**, so
  calling a helper without a `-> T` annotation recovers a precise type instead
  of going untyped: `fn area(w: int, h: int) { w * h }` now returns `int` at its
  call sites. Inference is sound by construction — it assigns a return type only
  when *every* return path (and any implicit fall-through value) is concretely
  known, otherwise the function stays dynamic. Recursion is self-guarding: a
  self/mutual/forward call resolves to the hoisted placeholder signature, so the
  function simply stays dynamic rather than looping. Inferred return types drive
  call-site inference only; they never trigger the declared-return diagnostics
  (fall-through / mismatch), which remain reserved for explicit annotations.
- **`+`/`-`/`*`/`/` no longer report a spurious "can't …" error when an operand
  has a gradual static type** (`any` / `unknown` / `_`). A gradual operand is
  compatible with every operator and the real check is deferred to runtime,
  matching how untyped operands were already treated. The gradual-top-type set
  is now centralized in one `is_gradual_type_name` predicate.
- **A local binding shadows a same-named function even when its static type is
  unknown.** `var x = …` / `let x = …` that reuses a function's name now
  resolves to the local, not the function reference, fixing a case where a
  shadowing local with an unknown type was mis-typed as `fn(…) -> …`.
- **Faster local-variable reads.** `GetLocalSlot` no longer clones the
  binding's name `String` on every successful read — that per-instruction
  heap allocation, used only to build a cold error message, is now deferred
  to the error path. This removes the dominant allocation in tight
  local-variable loops with no behavioral change.

### Fixed

- **In-process ACP `session/load` restores persisted saved sessions (#2793).**
  A `session/load` for an id the running `harn serve` instance never created now
  reconstructs it from persisted replay events — registering a live, promptable
  session and replaying its history — instead of rejecting it as `unknown
  session`. This is the in-process analogue of the WebSocket hub's persisted
  fallback and unblocks the Rust TUI saved-session picker and `burin --continue
  <id>`. Ids with no live session and no persisted events still fail loudly.
- **Release container publishing now reuses released Linux archives (#2794).**
  GHCR image assembly no longer recompiles Harn inside Docker after the release
  binaries already exist, and the container publish leg has its own timeout.
- **VM try/catch now observes throws from return expressions (#2821).**
  `return f()` and `return value |> f` inside a `try` block no longer elide
  the handler frame, so local catches run and typed-catch mismatches still
  propagate correctly.
- **Hostlib deterministic tools now reject malformed scalar payloads and report
  process/file-watch edge cases correctly.** Shared VmValue parsing now rejects
  non-finite or out-of-range numeric inputs, `run_command` no longer ignores
  malformed `cwd`/`stdin` fields or wait failures, directory listing reports
  symlinks without following their targets, inline command output preserves
  invalid UTF-8 lossily, and file watches can subscribe to `access`/`other`
  events.
- **The public-function doc-comment auto-fixer now covers every wrong-format
  case and no longer double-reports.** A `pub fn` preceded by a `//` / `///`
  comment used to surface both the fixless `missing-harndoc` warning and the
  auto-fixable `legacy-doc-comment` one; `missing-harndoc` is now suppressed
  whenever an adjacent migratable comment exists, so you see a single
  fixable finding. Plain `/* … */` block comments (single- and multi-line)
  directly above a public item are now migrated to canonical `/** … */`
  too, matching the existing `//` handling.
- **Seven generated-artifact drift guards that never ran in CI now run on
  every PR.** `check-protocol-artifacts`, `check-session-bundle-schema`,
  `check-provider-catalog`, `check-provider-catalog-drift`,
  `check-docs-workflow-quickstart`, `check-receipt-structs`, and the new
  `check-tree-sitter-keywords` existed only in `make all`, so a contributor
  could edit `spec/openapi.yaml` or the `SessionBundle` DTO and ship stale
  bindings with green CI. They are now wired into the appropriate CI lanes.
  Wiring `check-docs-workflow-quickstart` surfaced a pre-existing stale pin:
  the workflow-authoring quickstart's `graph_digest` had drifted from the
  runtime; the docs page and check are re-pinned to the current value.

## v0.8.60

### Added

- **Feature-sliced the in-process embedding surface (#2781).** `harn-serve`
  now ships lean by default: the code-intelligence grammars (tree-sitter +
  the ~27 `ast`/`code_index` language families) and the VM's Postgres
  (`pg.*`) builtins are opt-in. Embedders pick `hostlib-lean` (deterministic
  tools, no grammars), `hostlib` (full code intelligence), `vm-postgres`, or
  `full` (CLI parity). `harn-hostlib` exposes per-family grammar features
  (`grammar-web`, `grammar-systems`, `grammar-scripting`, `grammar-jvm`,
  `grammar-enterprise`, `grammar-data`), and `harn-vm` gates sqlx behind its
  `postgres` feature. A lean `harn-serve` build now links 60 fewer crates
  than the full CLI; `scripts/measure_lean_embedding.sh` reports and gates
  the delta. `harn-cli` keeps the full feature set, so CLI behavior is
  unchanged.
- Add an ACP `session/set_budget` control frame so embedders can re-arm live
  per-session LLM cost and token ceilings without restarting the engine.

### Changed

- **Harness dispatch hot paths (#2785).** Cached `harness.*` sub-handle reads
  and cheap synchronous harness method calls now use VM inline-cache fast paths,
  reducing overhead in tight orchestration loops while preserving mock and null
  harness audit semantics.

### Fixed

- **Static Harn CLI checks no longer initialize Tokio's Unix signal driver in sandboxed child processes (#2777).**
  `harn check`, `harn lint`, `harn fmt`, `harn parse`, and `harn tokens` now use a no-I/O runtime so
  nested static-analysis commands can run under process-exec-only sandboxes without tripping denied
  `socketpair` setup.
- **Runtime, ACP, DAP, OAuth, and stdlib edge cases.** Fixed integer
  division overflow panics, channel-select hangs on closed empty channels,
  duplicate in-flight ACP request IDs, stale DAP output flushing, OAuth token
  expiry validation, OAuth error-body secret leakage, and inconsistent
  duration option coercion across stdlib modules.
- Allow macOS process sandboxes to run Xcode/Swift toolchain commands that use
  `xcrun`, and route SwiftPM cache/security state into the workspace while
  Harn's outer sandbox remains active.

## v0.8.59

### Added

- **Graceful subagent stop handoffs (#2724).** Added
  `agent_stop(worker, {graceful: true})` and the `subagent_stop` lifecycle tool,
  returning recursive typed handoff summaries before stopping subagents.

### Changed

- **ACP embedding helpers (#2636).** Expose typed Rust ACP request/result
  helpers and direct ACP output hooks for in-process embedders.

### Fixed

- **MCP OAuth token handoff for ACP clients (#1430).** ACP MCP clients now
  resolve Harn-stored OAuth tokens for HTTP servers and can import legacy
  client-owned tokens into Harn's MCP OAuth store.
- **Agent scratchpad + reachability GC integration (#2708).** Agent loops that
  enable both session scratchpads and reachability-GC transcript projection now
  pass the live scratchpad as a root and scratchpad-version write barrier, so
  `require_write_barrier: true` can reclaim stale tool-result bodies only after
  current working memory has been externalized.
- **Skill induction with held-out sibling traces (#2709).** Fixed
  `skill_induce(...)` so a single source trace with held-out sibling traces can
  pass the replay-gated skill induction path instead of falling through to
  repeated-trace mining.
- **Agent tool-surface narrowing keeps write/control tools safe by default
  (#2773).** The default policy only prunes unused read-only tools, keeps
  mutating, approval, session-control, progress, result-polling, and unknown
  host tools visible through long discovery windows, and adds an explicit
  aggressive mode for callers that want usage-only pruning.

## v0.8.58

### Added

- **Fast-apply edits with merge model roles (#2666).** Added
  `std/edit::edit_fast_apply` / `fast_apply`, plus
  `llm_call(..., {model_role: "merge"})` defaults from
  `[model_roles.merge]` or `HARN_LLM_MERGE_*`, so broad edit intent can
  route through a dedicated merge model before dry-run preview, syntax
  validation, and hash-guarded commit.
- **Agent loop scratchpads (#2715).** `agent_loop` can now keep a small
  session-local scratchpad, recite it at the prompt tail every turn, and run a
  periodic cited reorganization pass with a separate reorganizer model.
- **Agents API permission spec.** The canonical OpenAPI spec now includes the
  permission policy/rule/check endpoints already exposed by `harn-serve`.
- **Project context profiles now auto-activate scoped agent context (#2719).**
  `project_context_profile(...)` resolves Git/GitHub, Rust, Node, Python, and
  Swift signals into reducer-ready prompt fragments, skill IDs, tool groups,
  MCP preset candidates, redacted provenance, and token-delta metadata;
  `agent_loop_options(...)` resolves the profile automatically from the active
  project root unless a caller supplies or disables it.

### Fixed

- **Agent loop lifecycle calls recover from invalid self-suspension arguments.**
  Malformed `agent_await_resumption` calls now inject corrective feedback and
  let the model continue instead of aborting the whole turn with
  `HARN-SUS-002`.
- Release smoke now waits for the official GitHub release assets on tag pushes
  and tests those downloaded binaries instead of recompiling native release
  binaries in a second workflow.

## v0.8.57

### Added

- Added replay-gated skill induction to crystallization reports and bundles,
  including generated `SKILL.md` artifacts, evidence refs, gate receipts, and
  the `skill_induce(...)` helper.
- **External-agent delegation contract (#2723).** Added `std/external_agent`
  wrappers and the VM-side `harn.external_agent.v1` A2A delegation contract with
  capability discovery, pre-dispatch checkpoints, hard budget/idempotency
  checks, local checkpoint fallback, idempotent replay, and reviewable
  handoff/diff envelopes.

### Fixed

- **Harn DAP now drives debuggee execution on a multi-threaded Tokio runtime with
  a persistent local task set (#2691).** Debug sessions can run `parallel`
  blocks and lifecycle pool workers without diverging from the VM concurrency
  paths used by normal execution.
- **DAP source requests now parse `file:` URIs with the standard URL parser.**
  This keeps source lookup correct for platform-native paths, including Windows
  drive-letter paths and `localhost` file URIs.
Allow standard process device files such as `/dev/null` through the macOS and
Linux process sandboxes without granting broad `/dev` access, and mediate Linux
device ioctls on kernels that support Landlock ABI 5. Workflow verifier
commands also avoid login-shell dotfile reads under those sandboxes.

## v0.8.56

### Added

- **Replay counterfactual chains.** `harn replay --counterfactual` can now
  chain repeated plan files into one cumulative dry-run divergence report,
  evaluates plans only after the requested replay cutoff is valid, and
  isolates accidental plan-side filesystem writes from the workspace (#2611).
- **MCP mock and simulated-world eval harness (#2710).** `harn mcp mock`
  can now record redacted stdio MCP cassettes, replay them without
  credentials, verify schema/behavior drift, serve seeded stateful
  worlds with fault injection, and score final states for goal match,
  collateral damage, pass rate, and pass^k reliability.
- **Web grounding helpers in `std/web` (#2711).** Added provider-agnostic
  `web_search`, deterministic `verify_imports`, and `web_grounding_tools` for
  provenance-carrying search, post-edit import verification, and
  capability-gated model guidance.
- **Stall detector now catches more degenerate-loop patterns (#2712).** The
  deterministic detector in `std/agent/stall` gained four new trip conditions
  alongside the existing repeated-identical-call check: same action → same
  error, no-progress monologue (consecutive assistant turns with text but no
  tool call), ping-pong alternation between two actions, and repeated
  context-window/token-limit errors. Each is behind a named, tunable threshold
  (`repeat_same_observation`, `repeat_same_error`, `no_progress_messages`,
  `ping_pong_cycles`, `repeat_context_window_error`, `hard_stop_after_trips`).
  A principled polling exemption replaces brittle allowlists: a repeated
  identical action only counts toward a trip when its observation is
  byte-identical across repeats, so legitimate polling (a status that advances,
  a watched file that changes) never trips. `exempt_tools` still works as a
  secondary escape hatch. All trips route through the existing
  `agent_loop_stall_warning` event (now enriched with `pattern`, `signature`,
  `count`, `threshold`, and `consecutive_trips`), the advisory judge stays
  gated on a deterministic trip, and a hard stop after repeated un-recovered
  trips surfaces through the loop's existing terminal stuck /
  `loop_control_decision` path — no new hook or event surface was added.
- Added the `grounded_review` reminder provider, which injects advisory
  code-review reminders only from concrete verifier, diagnostic, test, or
  runtime-error evidence.
- **Transcript projection can now reclaim stale tool-result bodies by
  reachability (#2714).** The new `reachability_gc` policy preserves raw
  transcript/audit history while replacing unreachable projected tool-result
  bodies with redaction pointers, reclaimed-token estimates, and root-set audit
  metadata.
- Added `harn eval skill-gate` for contamination-safe held-out
  skill/guidance gates with context-cost accounting, regression checks,
  immutable grader hashes, Pareto variant reporting, and
  `harn.skill_gate.receipt.v1` receipts.
- Add bounded MCP Code Mode fan-out with trusted retry gates,
  idempotency-key replay, per-server bulkheads, and outputSchema validation.
- **Structural diff stdlib and hostlib surface (#2720).**
  `std/diff` now exposes `structural_diff(path_a, path_b, language_or_options?)`,
  backed by a new `hostlib_ast_structural_diff` method that parses both files with
  tree-sitter, returns changed syntax-node spans for human review, reports pure
  moves separately from line churn, and falls back to a line diff on parse errors
  or size limits.
- Added `artifact_emit(kind, spec, options?)` for validated Vega-Lite, Mermaid, and table artifact events on agent sessions.
- **Provider catalog model-family metadata and complementary reviewer
  selection (#2722).** Catalog exports now include normalized `family` and
  `lineage` fields, optional reviewer-diversity hints, and a new
  `llm_complementary_reviewer` builtin plus
  `std/llm/catalog.complementary_reviewer` wrapper for selecting a
  different-family reviewer with fallback reasoning and estimated cost.
- Expose the normalized provider/model catalog at runtime through harn-serve
  `GET /v1/provider-catalog` and the ACP `_harn/providerCatalog` extension
  method.

### Changed

- Added an observability processor pipeline with a stock redaction processor, and
  surfaced session event/receipt signature metadata as declared `harn.session.*`
  span attributes.
- **VM dispatch, inline caches, and pool fan-out are now `Send`-ready (#2690, #2691).**
  Builtin ABIs, VM shared state, host bridges, isolate-local inline caches, and
  the first pool worker path now use `Send`/`Sync` handles, with pool workers
  scheduled on `tokio::spawn` and scoped through a VM-owned pool registry for
  multi-thread execution.
- **Pool multithreading now has an enforceable thread-local audit guard and CPU fan-out benchmark (#2691, #2692).**
  The VM keeps a checked-in inventory for every `thread_local!` site, tests that
  new sites are classified before landing, documents the `tokio::spawn`
  pool-worker boundary that must stay task-local or explicitly scoped, and
  avoids hot shared-closure refcount contention in adaptive arithmetic dispatch.
- **Harnlang documentation navigation and first-run guidance.** The mdBook docs
  now add a top-level section nav, narrow the sidebar to the active
  documentation mode, clarify LLM provider/model resolution and remote MCP
  login, stop suggesting Notion MCP OAuth scopes, and publish raw Markdown
  companions for docs pages during the docs build. CI now also fails when
  hand-written Sonnet examples drift from the live provider catalog alias.

### Fixed

- `edit_dry_run` now previews `rename_symbol` plan operations through the
  shared code-index graph when the full hostlib surface is installed, so
  multi-step edit plans can include cross-file renames in the unified diff
  bundle.
- Emitted Burin compass routing decisions as live agent events and aligned router coverage with the AST language registry.
- **Agent pool multithreading docs and diagnostics now match the Send VM closeout (#2688).**
  The public pool references describe the VM-scoped registry, `tokio::spawn`
  worker boundary, pipeline reload semantics, and host-managed tenant/org
  scopes without leaking private host issue links.
- **Portal and filesystem helpers are more robust.** Portal launches now use
  collision-resistant IDs and real RFC 3339 timestamps, portal run analysis
  handles case-insensitive search and large duration values safely, and
  stdlib filesystem copy/move/delete/mkdir operations now honor the
  testbench overlay and mutation notifications consistently.
- **Release publishing now fails closed without a pre-pushed tag.** The
  push-to-main release workflow refuses to tag main HEAD when Cargo.toml is
  ahead of the latest release tag, and `release_ship.sh --prepare` now routes
  through the tag-first release harness.
- Break module closure reference cycles and add a VM RSS soak gate.

## v0.8.55

### Fixed

- **Sandbox no longer over-restricts standard I/O device files or out-of-scope
  existence probes (HARN-CAP-201).** Two legitimate non-workspace operations
  that the read-only-sandbox-roots feature (#2726, v0.8.53) began rejecting are
  allowed again, unblocking eval/setup pipelines on v0.8.53+ sandboxes on both
  the Rust and Node surfaces. Writing to `/dev/stdout`, `/dev/stderr`,
  `/dev/null`, and the numeric `/dev/fd/<N>` descriptors is permitted under a
  restricted profile — these target the process's own I/O streams, not the
  sandboxed tree — while every other out-of-root path stays denied. A presence
  probe (`exists`/`file_exists`/`harness.fs.exists`) for a path outside the
  sandbox now reads as "absent" (`false`) instead of throwing a violation,
  matching how OS sandboxes make out-of-jail paths appear non-existent. Reading
  file *content* outside the configured roots is still denied.

## v0.8.54

### Changed

- **VM value handles are now Send-safe shared values (#2689).** Owned runtime
  values now use thread-safe shared ownership so `VmValue` can cross worker
  boundaries without special host-side wrapping.

### Fixed

- **TypeScript protocol bindings now emit a defined type for
  `ACPPermissionToolCall._meta`.** The TS artifact referenced the Python-only
  `HarnExtensionMeta` name, which is undefined in TypeScript and broke
  downstream `tsc` in consumers that vendor the bindings. It now emits
  `ACPExtensionMeta<ACPObject>`, matching every other tool-call `_meta` field.
  A new `dump-protocol-artifacts` self-consistency test fails the harn build on
  any dangling `ACP`/`Harn`/`A2A`/`MCP`-prefixed type reference in the emitted
  TypeScript, so this class of bug can no longer escape harn's CI.

## v0.8.53

### Added

- Finish the Postgres hostlib v2 surface with named read-replica routing
  policies, `pg.jsonb.*` helpers, and structured range/geometric row-codegen
  mappings.
- **Postgres prepared-statement cache invalidation (#2581).** `std/postgres`
  now exposes `pg_stmt_cache_clear(pool)` to clear SQLx prepared-statement
  caches on idle primary and replica connections without closing the pool.
- **MCP OAuth now defaults to CIMD before dynamic registration (#2645).** Harn
  publishes a stable Client ID Metadata Document at
  `https://harnlang.com/.well-known/oauth-client.json`, selects it for MCP
  OAuth when the authorization server advertises CIMD support, falls back to
  dynamic client registration otherwise, and adds a unified `auth = { mode =
  "cimd" | "dcr" | "static" | "byo", ... }` MCP config shape. Static bearer
  tokens reuse `harn connect api-key` secrets; BYO client secrets are referenced
  from the same secret store instead of being embedded in host-generated config.
Generalized MCP OAuth so any MCP server can be authorized over ACP, not just
named connectors. A new harn-owned flow engine (`harn_vm::mcp_oauth`) does
discovery, client resolution (DCR/CIMD/BYO), PKCE, the code exchange, the
single-flight refresh (in-process mutex + cross-process file lock,
per-`client_id` keyed), and keyring storage in one place; `harn mcp login` and
the ACP path now share it instead of each carrying a copy. When a server
answers `401` mid-session harn emits an additive
`mcp_auth_required` agent event (server + canonical resource + challenge scope)
as a cue, and two new ACP requests complete the loop: `mcp/authorize` mints the
browser URL + `state`, and `mcp/oauth_callback` exchanges the returned
`code`/`state` (or a captured client-scheme redirect URL). Token exchange and
storage stay in harn; thin clients only open the URL and forward the callback.
(#2646)
- Added MCP-focused Code Mode helpers that build/search typed Harn binding APIs from
  discovered MCP tools and execute snippets through the normal agent tool dispatcher.
- **`harn serve acp` now passes the ACP Agent Registry auth gate (#2664).** Two changes make the bare
  `harn serve acp` command launchable and conformant the way the agentclientprotocol/registry validator
  expects:
  - The positional `<FILE>` is now **optional**. With no file, `harn serve acp` boots a file-less ("attach")
    ACP stdio server that waits for `initialize` / `session/new` from the connecting editor instead of
    requiring a script path up front (previously it exited code 2 before the ACP loop started). The existing
    file-provided mode is unchanged.
  - `initialize` always advertises a **non-empty, spec-conformant `authMethods` array**. Each credentialed
    method (API key / HMAC / OAuth 2.1) now carries an explicit top-level `type: "agent"`, and a policy with
    no configured credentials (the local attach default) advertises a single `agent`-type `"none"` method
    representing harn's local/anonymous flow. Authenticating against that `"none"` method id is honoured as a
    no-op success. Verified against the registry's own `client.py` auth-check: harn now reports
    `Found 1 valid auth method(s)`. Unblocks the deferred registry submission (#2672).
- **ACP is now available as both an attach endpoint and an outbound provider route (#2664).**
  `harn serve acp --transport websocket` exposes the packaged ACP adapter over
  WebSocket text frames, and `[llm.providers.<name>] protocol = "acp"` lets
  `llm_call` drive an external ACP agent over stdio while keeping host
  permission requests denied unless an explicit host integration owns them.
- **Runtime provider catalog refresh.** `harness.llm.catalog_refresh()`,
  `llm_catalog_refresh()`, and `harn provider-catalog --refresh` can fetch a
  validated provider/model catalog overlay, cache it under the Harn state root
  with ETag/TTL metadata, and expose refreshed model rows through the CLI, Harn
  catalog builtins, and ACP model selectors while preserving the bundled offline
  fallback (#2671).
- **ACP sessions can now own prompt budgets (#2681).** Embedders can pass
  `AcpServerConfig::with_budget(...)`, and ACP clients can re-arm or disable the
  session budget through `session/set_config_option(configId="budget")`.
- **ACP read-only sandbox roots for bundled embedder assets.**
  `AcpServerConfig::with_read_only_roots` lets an in-process ACP host register
  read-only sandbox roots outside the user's `workspace_roots`. The configured
  roots are unioned into the per-turn capability policy at
  `ModePolicyGuard::enter`, so `check_fs_path_scope` permits reads under them
  while still denying writes, deletes, and reads outside every root. This
  unblocks embedders (e.g. burin's Rust TUI) that ship bundled pipelines and
  their `@partials` outside the user's project tree.

### Changed

- **Async builtins now receive an explicit `AsyncBuiltinCtx` handle (#2668).**
  The async-builtin ABI (`VmAsyncBuiltinFn`, `AsyncHandler`) and the
  `#[harn_builtin(kind = "async")]` macro thread a `ctx` parameter from the
  dispatch loop into every async handler. VM-backed helpers now receive that
  context explicitly, mint child VMs with `ctx.child_vm()`, and forward captured
  closure output with `ctx.forward_output(..)`, removing the async-builtin
  task-local lookup path entirely.

### Fixed

- **Sandbox mount targets reject parent traversal (#2516).** Hostlib-backed
  sandbox sessions now reject guest mount and cwd targets containing `..`
  components before resolving them against host paths.
- Align ACP `session/request_permission` generated bindings and bridge tests with
  the canonical v0.12.2 `{toolCall, options}` request and
  `{outcome: {outcome: "selected" | "cancelled"}}` response shape.
- **MCP OAuth resource indicators are now canonical and shared across protocol
  requests (#2643).** `harn mcp login` sends the same no-fragment,
  no-trailing-slash resource indicator to authorization and token endpoints and
  uses that canonical resource when finding stored MCP OAuth credentials.
- Prevent concurrent MCP OAuth refreshes from spending the same rotated refresh
  token by coordinating refreshes per issuer/resource/client across threads and
  processes.
- **Fail loud on a model-less agent turn.** When `agent_loop` finalizes a
  completed turn that never actually called the provider (zero iterations and
  zero tokens for a `done`/empty status), it now returns a clear `no_llm_call`
  terminal error — "agent turn made no LLM call: no model resolved / empty
  input" — instead of a silent success-with-empty-text. Intentional pauses
  (`suspended`, `blocked`, `cancelled`, waitpoints) and already-errored turns
  are unaffected.
- **Execute `<tool_call>`-wrapped calls in text/bare mode.** Text-format models
  (e.g. OpenRouter `qwen/qwen3-coder`) wrap their bare `name({ ... })` calls in
  `<tool_call>...</tool_call>` tags even when the prompt asks for bare calls. The
  bare parser now strips those wrapper tags up front, so a same-line
  `<tool_call>run({...})</tool_call>` is executed instead of dropped
  (`tool_calls: []`) and a trailing `</tool_call>` no longer leaks into the
  visible assistant text as a `_call>` fragment.
- **Harn preflight reuses the existing module graph for re-export checks.**
  `harn check` avoids rebuilding a module graph for every file while scanning
  re-export conflicts.
- **Harn source discovery now skips generated and local cache directories.**
  `harn check`, `harn lint`, and `harn fmt` no longer recursively scan
  dependency caches, build outputs, local run artifacts, or worktree copies
  when a project directory is passed as the target.

## v0.8.52

### Added

New harn-owned MCP enable/disable allowlist (`harn_vm::mcp_allowlist`) covering
tools, resources, and prompts from one persisted config plus an optional
per-project overlay, and an effective-catalog projection (servers → items +
`enabled`) exposed over ACP via the `mcp/catalog` request. A new additive
`mcp_catalog_changed` agent event cues thin clients (the burin-code TUI / GUI)
to re-fetch and re-render their MCP toggle UI, so no client stores toggle state
of its own. (#2647)
The macOS `sandbox-exec` renderer now emits a trailing `(deny file-write*
(subpath "<root>"))` for every `read_only_roots` entry after the broad
workspace write allow, so a read-only root nested under a writable workspace
root stays hermetically unwritable (`sandbox-exec` is last-match-wins,
defeating a chmod-back write). The Linux Landlock renderer's read-only access
bits (read + dir-read + execute, never write/create/remove) are extracted into
a tested `read_only_access()` helper; Landlock rules are additive (no deny), so
the two root lists must stay disjoint when targeting the Linux backend. No-op
when `read_only_roots` is empty. (#2654)

### Changed

MCP tools now use **progressive disclosure by default** when bootstrapped into
an agent's tool surface. Each MCP tool ships a lightweight catalog entry (name +
one-line description) up front, and its full JSON input schema is held back until
the agent surfaces the tool via `tool_search` or calls it directly
(`defer_loading: true`). This cuts the upfront context/token cost for MCP servers
that expose many tools.

This is a behavior-default change and is opt-out-able: set `eager_schemas: true`
on an `mcp_servers` spec entry to ship that server's full schemas eagerly (the
previous behavior). A per-tool `defer_loading` that a server advertises itself is
always preserved.

### Fixed

- **Per-test reset now drains several registries that previously accumulated across a reused worker thread
  (#2660).** `reset_thread_local_state` (and the harn-cli test runner) now clear the workflow run-state map,
  the agent inbox, the LLM routing-policy registry, the tool-call cancellation registry, and `harn-hostlib`'s
  filesystem-snapshot sessions. Each of these was only drained on a specific normal-completion path, so a
  workflow, inbox, policy, cancellation, or snapshot session abandoned mid-run (test timeout, early error,
  host teardown) leaked one entry per case. Regression tests assert each registry is empty after a reset.
- **Async-builtin VM context is now cancel-safe and multi-thread-correct (#2667).** The per-call child VM is
  propagated through a `tokio::task_local` scope instead of a manually push/pop'd `thread_local!` stack. A cancelled
  or panicked async builtin can no longer strand a child `Vm` (which `Rc`-pins the compiled module graph + env
  snapshot) — the binding is dropped with the future — so long-running and cancellation-heavy agent loops no longer
  accumulate stranded contexts, and the context is read correctly when a task resumes on another worker thread.
  Removes the deprecated `take_async_builtin_child_vm`/`restore_async_builtin_child_vm` shims and the
  `AsyncBuiltinChildVmGuard`. Groundwork for explicit context passing (#2668).
- **Fixed an unbounded memory leak in the test runner and long-running agent loops (#2660).** Transcript and daemon
  event-log appends were dispatched as detached `tokio::runtime::Handle::spawn` tasks. The agent loop and
  `harn test` drive their runtime with `LocalSet::run_until`, which stops polling once the driving future resolves,
  so those detached append tasks were never run to completion — each stranded task pinned its transcript-sized
  `LogEvent` payload plus an `Arc<AnyEventLog>` clone for the lifetime of the runtime. Across a
  `harn test --parallel` worker this accumulated roughly one transcript per test (~18 MB) and OOM'd CI. The appends
  now run synchronously via a private `futures::executor::block_on`; no event-log backend yields to the tokio
  reactor on `append`, so this is leak-free and does not touch the ambient runtime. A counting-allocator probe
  measured the regression at ~18 MB/round of steady-state growth before the fix; a regression test now asserts the
  append lands in the log the instant the producing `run_until` future resolves, proving no detached task can
  strand the payload.

## v0.8.51

### Breaking

- **ACP `session/request_permission` now uses the canonical ACP v0.12.2 wire
  shape (#2639).** The request the agent sends is
  `{ sessionId, toolCall: <ToolCallUpdate>, options: [{ optionId, name, kind }] }`,
  and the only accepted responses are `{ outcome: { outcome: "selected",
  optionId } }` and `{ outcome: { outcome: "cancelled" } }`. The pre-#2639
  harn shapes — the top-level `toolName`/`rawInput` request fields and the
  `{ outcome: "approved" }` / `{ granted: true }` responses — are no longer
  emitted or honored; a `selected` outcome on the `allow` option grants the
  call, `reject` and `cancelled` stop it (fail-closed on anything else). Harn's
  internal permission *policy* decision (allow / deny / suspend), the
  `ApprovalPolicy` receipt, and the out-of-band `harn.hitl.respond` HITL path
  are unchanged — they now ride alongside the canonical fields as a vendor
  extension under `toolCall._meta.harn`. ACP clients (e.g. the Burin IDE) must
  send the canonical `selected`/`cancelled` response.

### Added

- **`harn replay --counterfactual <plan.harn>` answers "what if the agent had
  edited differently?" (#2611).** After rehydrating a session at the `--at`
  cutoff, the flag evaluates an alternate `.harn` edit plan through
  `edit.dry_run` against a throw-away staged-fs overlay (#1722) and reports the
  divergent file set — the files the plan's edits would touch, each tagged
  `created`/`modified`/`deleted` with line deltas — in both human and `--json`
  (`data.counterfactual`) modes. The recorded session and the on-disk tree are
  never mutated. Closes #2522.
- **Burin compass tool-rewrite router + `harn.compass.*` counters (#2612).** The compass steer
  (#2521) now has an active routing layer: a per-tool-call hook in the agent loop observes freeform /
  whole-file edit calls (`str_replace`, `write_file`, `edit_safe_text_patch`-shape) on parseable files
  and steers them toward the AST-precise primitives. On by default in `suggest` mode — it injects an
  advisory reminder naming the structural tool (`edit_apply_node` / `edit_rename_symbol` /
  `edit_safe_text_patch`) and leaves the original call untouched. In `rewrite` mode it silently
  substitutes the provably-equivalent hash-guarded `edit_safe_text_patch` for a raw text replace, and
  falls back to a suggestion otherwise. Each decision increments `harn.compass.suggested`,
  `harn.compass.rewritten`, or `harn.compass.fell_back`, tagged with the persona and tool names.
  Configure per session/persona via the `compass` option (`compass: false` /
  `compass: {mode: "rewrite"|"suggest"|"off", prefer: [...]}`); the router consumes the
  `edit_strategy.prefer` signal from `personas/fixer/manifest.harn`. The hook is additive and inert
  when the compass is disabled or the call is not a freeform edit. Closes #2521.
- **`harn dump-protocol-artifacts` now emits a Rust binding (#2640).** A
  dependency-free `spec/protocol-artifacts/harn-protocol.rs` of ACP method-name,
  session-update discriminator, content/tool-lifecycle extension-key, and
  protocol-version `pub const`s (+ published slices). It is the first binding to
  publish the complete *dispatched* ACP method surface (not just the stable
  subset), so Rust hosts (Burin Code) vendor it as `protocol/src/generated.rs`
  instead of hand-maintaining a parallel method list.
- **`harn-serve` gains an in-process ACP embedding API (#2636).** `run_acp_channel_server_with_handle`
  returns the (`!Send`) server future plus a `Send` `AcpChannelHandle` exposing graceful `shutdown()`,
  `wait_ready()`, and `wait_terminated()`; and `EmbeddedAgent` packages the dedicated-thread +
  current-thread-runtime setup an in-process host otherwise hand-rolls, handing back the request sender,
  response receiver, and handle. Existing `run_acp_channel_server` / `run_acp_server` signatures and
  behavior are unchanged.
- **MCP OAuth now sends the RFC 8707 `resource` indicator (#2643).** The MCP authorization profile
  requires the client to send `resource=<canonical MCP server URI>` in both the authorization request
  and the token request — regardless of whether the authorization server advertises support. A new
  `harn_vm::mcp_auth::canonical_resource_indicator` helper canonicalizes the server URL (lowercased
  scheme/host, default ports and the query/fragment dropped, lone trailing slash stripped), and the
  `harn mcp login` flow now threads that canonical resource into the authorization URL, the
  authorization-code exchange, and refresh-token requests.
`harn mcp presets [--json]` prints a harn-owned catalog of well-known MCP
server presets (Notion, Linear, GitHub, filesystem) — transport,
command/url template, auth kind, and required placeholders — so thin clients
render one identical, drift-free "one-click server" list. The `--json`
envelope is a versioned stable contract. (#2657)
`harn mcp status` with no target now reports every `[[mcp]]` server in the
nearest `harn.toml` — transport, derived state
(connected/disconnected/auth_required/error), url, lazy flag, and live
tool/resource/prompt counts when known — with a versioned `--json` envelope.
A new additive `mcp_notification` agent event also surfaces server-to-client
MCP messages (progress, log, and inbound elicitation/sampling requests) over
the ACP `_harn/agentEvent` channel, so a thin client can render them live
without parsing the agent inbox. (#2658)

### Changed

- Prompt assembly: the agent's per-turn primary system block is now decomposed
  into individually auditable fragments. `agent_build_turn_system_fragments`
  emits one `{id, source, body}` per internal part (system text, MCP advisory,
  active skills, skill catalog, progress nudge, loop/tool contracts); the agent
  loop forwards them to `llm_call` over the `_system_fragments` channel so the
  whole primary block — not just host parts and reminders — shows up in
  `prompt_explain` provenance under `primary:<part>` ids. The assembled prompt
  is byte-for-byte unchanged; only its provenance is finer-grained.
`harn test --parallel` now sizes its default worker count by available
memory as well as CPU count, capping at
`min(cores, (available - reserve) / per-worker-budget)`. Available memory is
the lesser of the host figure and (on Linux) this process's cgroup-v2 slice
headroom, so a container or a self-hosted runner that shares a box no longer
oversubscribes RAM and triggers swap-thrash runner-loss. The cap only ever
lowers the core-based default; explicit `--jobs` / `HARN_TEST_JOBS` still win,
and the per-worker budget is tunable via `HARN_TEST_WORKER_MEMORY_MB`. (#2659)

### Security

- **`harn serve site` no longer trusts `X-Forwarded-For` / `X-Real-IP` by default (#2624).** `req.client_ip`
  now defaults to the real transport peer, so a direct caller can no longer forge it with a spoofed header.
  Pass `--trusted-proxy <CIDR>` (repeatable; or `HARN_SERVE_SITE_TRUSTED_PROXIES`) with your reverse proxy's
  ranges to opt back in — the forwarded chain is then honoured only when the connection arrives from a trusted
  proxy, and the client is taken as the rightmost hop that is not itself trusted. The `req` dict also gains a
  populated `remote_addr` (`ip:port` of the transport peer), wired through from the listener.
- **`CapabilityPolicy` can express read-only workspace roots, closing a sandbox-escape where "read-only"
  mounts were writable (#2634).** A new `read_only_roots` list sits alongside `workspace_roots`: a path
  resolving under one is in scope for `read_text`/`list`/`exists` but rejected for `write_text`/`delete`
  with a `tool_rejected` "read-only workspace root" violation, and every OS sandbox backend (Linux
  Landlock, macOS `sandbox-exec`, Windows AppContainer ACLs, OpenBSD `unveil`) grants the root read but
  never write — even when the policy otherwise allows workspace writes. The local hostlib sandbox now
  lowers each `FilesystemAccess::ReadOnly` mount into `read_only_roots`, so a snippet or persona can no
  longer `echo x > /mnt/memory/file` or delete bundle files under a mount the caller declared read-only.
  Nested-execution and workflow-patch ceilings reject a child that widens read-only scope past its parent,
  and `intersect` narrows `read_only_roots` to the common set.

## v0.8.50

### Added

`harn serve site <file>` hosts a script's own HTTP routes: each routed
`pub fn` (via a `@route("METHOD", "/path")` attribute or the `handler_*`
convention) answers a path, receiving a request dict and returning a
value or an `http_*` response envelope. WebSocket handlers upgrade via
`http_upgrade_ws(req, { on_message })`. (#2574)
`llm_call(..., { fast: true })` wires the catalog's accelerated-serving
("fast mode") tier into the live request path (#2619). The knob is
provider-agnostic: Harn reads `fast_mode.param`/`value` from the model
catalog and injects `speed: "fast"` for Anthropic (plus the
`fast-mode-2026-02-01` beta header) or `service_tier: "fast"` for OpenAI —
no hardcoded provider quirks. Requesting `fast` on a model with no
`fast_mode` tier, or a deprecated one, fails fast with a clear diagnostic.
Cost accounting bills at the premium `fast_mode.pricing` only when the
provider confirms it served fast (echoed `speed`/`service_tier`), surfaced
as `result.served_fast`; a capacity downgrade bills at standard rates.
`provider_capabilities()` now reports `fast_mode_supported` plus a
`fast_mode` metadata dict. (#2619)

- **`run_command` preserves a slow command's full output past a `| tail`-style filter (#2625).** When an
  agent runs a `producer | tail/wc/grep/sort/…` shell pipeline, the new `std/agent/command_capture`
  recognizer transparently rewrites it to `producer | tee '<capture>' 2>/dev/null | filter` so the model
  still sees exactly the filtered output while the producer's complete output is preserved to a temp file —
  a post-run `output_capture` field points the model at it so it never has to re-run the slow command. The
  rewrite is conservative (it leaves `head`, `grep -q`/`-m`, command substitution, subshells, and anything
  it can't parse safely untouched) and on by default; set `preserve_filtered_output: false` on
  `agent_command_tools` / `agent_host_tools` to disable it.
- **Tool definitions can carry `guidance` — a system-prompt instruction co-located with the tool
  (#2631).** When the tool is in the active tool set, the runtime injects the guidance as a
  capability-gated system-prompt fragment, so an instruction like "always update the TODO tracker
  when working from a plan" appears only when the tool is available and is omitted otherwise.
  Instruction and tool share one source of truth and cannot drift.
- **New `prompt_explain(options)` builtin** assembles the system prompt from `agent_loop`-shaped
  options and returns the final string plus per-fragment provenance (`id`, `source`, `bucket`,
  `included`, `reason`, `bytes`) — so you can audit exactly why each piece is in the prompt and what
  changes when a tool is absent. See `docs/src/prompt-assembly.md` and the `harn demo
  prompt-guidance` scenario.

### Changed

- **System-prompt assembly is now a single fragment reducer (#2631).** Host-provided parts
  (`system_preamble`/`prefix`/`context`/`parts`/`appendix`/`suffix`), the primary system text,
  capability-gated tool guidance, and rendered system reminders all flow through one deterministic
  `assemble()` that records per-fragment provenance, replacing the previous two-stage string
  concatenation (`append_system_prompt_parts` / `push_system_prompt_part`, both removed). Output is
  byte-identical for existing prompts.

### Fixed

Defining an attribute-decorated top-level function — e.g. a
`@route("GET", "/path") pub fn` handler for `harn serve site` — no longer
crashes at runtime with "Stack underflow". In script mode (a file with a
top-level `fn main`), the compiler classified an attributed declaration by
the catch-all expression rule and emitted a spurious module-level `Pop`,
underflowing the operand stack even when the function was never called.
Attributed declarations are now classified by their inner declaration, so
they leave the stack balanced like any other top-level `fn`. (#2610)
Bytecode cache entries are now keyed on a build-time fingerprint of the compiler
front-end, so a compiler change that alters emitted bytecode for unchanged source
invalidates stale `.harnbc` artifacts automatically — no version bump or manual
cache wipe required.
A `produces_value` misclassification — the compiler's "did this statement
leave a value on the operand stack?" rule that drives every value-discarding
`Op::Pop` — is now caught at compile time in debug builds instead of
surfacing only as a runtime "Stack underflow" (the class of bug fixed in the
attributed-decl regression, often masked further by the bytecode cache). A
lightweight balance model folds each emitted opcode's net stack effect into
the chunk and asserts that straight-line statements net exactly what their
classification predicts; the seven duplicated "compile node, then pop its
value" loops are unified behind one helper that carries the check. The model
surfaced two pre-existing imbalances, now fixed: metadata-only declarations
(`enum`/`type`/`interface`/`override`/nested pipelines) and `defer`
statements each emitted an unpopped placeholder `Nil`, leaking an
operand-stack slot per execution — unbounded in a loop body for `defer`.
(#2622)
`harn serve site` now diagnoses malformed `@route(...)` / `@scopes(...)`
attributes instead of silently dropping them. A `@route` with a
non-string argument, zero arguments, or more than a method and a path
emits a `HARN-SRV-001` / `HARN-SRV-002` warning at startup and leaves the
handler unmounted (rather than mis-routing it, e.g. collapsing
`@route("GET", some_var)` to `/GET`); a non-string `@scopes` argument
emits `HARN-SRV-003` and is dropped, flagging the unintended loosening of
the route's scope requirement. The same parsing bug also meant
`harn serve site <file>` itself never reached the site server — the
legacy `serve`→`a2a` argument shim didn't recognize the `site`
subcommand and rewrote it; the shim now derives the transport list from
the command tree so new transports are handled automatically. (#2623)

## v0.8.49

### Added

- **Claude Opus 4.8 and GPT-5.5 in the provider catalog (#2616).** Adds
  `claude-opus-4-8` (Anthropic's flagship — adaptive thinking, `effort`
  defaulting to `high`, $5/$25 per MTok) as the new `opus` alias target, plus
  a `gpt-5.5` row (OpenAI Responses / Chat Completions).
- **Structured `superseded_by` model metadata (#2616).** Deprecated catalog
  rows can now point at their replacement in a machine-readable field instead
  of only in free-text `deprecation_note`; surfaced through the catalog
  artifact (JSON / Schema / TypeScript / Swift) and `llm_provider_catalog()`.
- **Provider-agnostic `fast_mode` catalog metadata (#2616).** Describes a
  model's accelerated, premium-priced serving tier (Anthropic `speed="fast"`,
  OpenAI `service_tier`) — its opt-in knob, premium pricing, and lifecycle.
  Off by default (metadata only); populated for Claude Opus 4.6/4.7/4.8 and
  GPT-5.5.

### Changed

- **The `opus` alias now resolves to `claude-opus-4-8` (#2616).** Earlier Opus
  rows (4 / 4.1 / 4.6 / 4.7) declare `superseded_by = "claude-opus-4-8"`.
- **Corrected Claude Opus 4.6 / 4.7 catalog pricing to $5/$25 per MTok
  (#2616)** to match Anthropic's published rates (the rows were carrying the
  older $15/$75 Opus figures).

### Deprecated

- **Claude Opus 4.6 and 4.7 are marked deprecated (#2616),** superseded by
  Claude Opus 4.8. Soft deprecation with no announced sunset — both remain
  fully usable, so switch when convenient.

## v0.8.48

### Added

- **Standardized HTTP response codec for `.harn` handlers (#2501).**
  New builtins — `http_ok`, `http_created`, `http_no_content`,
  `http_error`, `http_reply`, `http_stream`, `http_sse` — produce a
  tagged `HttpResponse` envelope (`__http_response__: "v1"`) that
  `harn-serve`'s new `http_codec` module renders into a full
  `axum::Response`: status code (1xx-5xx), single- and multi-value
  headers, JSON or buffered-stream or SSE body, and a standard error
  envelope `{ code, message, request_id, details? }`. Untagged handler
  returns degrade to `200 OK + application/json` so existing scripts
  keep working. `DispatchError`s render through the same envelope so
  auth/validation failures look identical to handler-declared errors.
- **Supervised MCP host primitive (`harn.mcp.*`).** Adds a single
  authoritative external-tool host that owns MCP server lifecycle
  (`spawn`/`tools`/`call`/`stop`/`discover`/`reload`/`status`) on top of
  the existing lazy registry. Layers automatic restart with exponential
  backoff (capped restart budget per window), per-server circuit-breaker
  state, a (server, tool, args-hash) response cache that honors MCP
  cache hints, cross-server tool discovery, and pluggable per-tenant
  allowlists via a new `AuthPolicy.mcp_allowlist` field and
  `AuthPolicy::authorize_mcp` decision path. Tracing spans (`harn.mcp.*`)
  emitted at the dispatch boundary so observability backends can
  attribute supervision events. Closes #2504 (epic #2496, A.7).
- **`edit.rename_symbol` stdlib primitive (#2508).** Safe cross-file rename
  driven by the typed symbol graph (#2434). Resolves a seed symbol, walks
  every file in scope (`"file" | "module" | "workspace"`) with tree-sitter
  to collect identifier-context spans (skipping comments and string
  literals), and refuses to write if `new_name` already exists as an
  identifier in any rewritten file. Languages: Rust, TypeScript/TSX,
  JavaScript/JSX, Python, Swift, Go. Writes route through staged-fs
  (#1722) when a `session_id` is supplied so all touched files succeed
  or none do; without a session the host still buffers every plan in
  memory and only writes after pre-flight passes. Exposed as
  `hostlib_code_index_rename_symbol` plus the Harn-side
  `edit_rename_symbol(params)` shim. Cookbook: "Rename a symbol across
  the workspace". Demo: `harn demo edit-rename-symbol`.
- **`edit_dry_run` — preview a multi-op edit plan without touching disk.**
  New `hostlib_ast_dry_run` builtin + `std/edit` wrapper that accepts a
  list of edit ops (`apply_node`, `insert_at_anchor`, `safe_text_patch`,
  `rename_symbol`), runs them through a transient staged-fs (#1722)
  overlay, and returns one unified diff per touched file plus a
  per-op outcome list. The diff format is `git apply --check`
  compatible (`---`/`+++` headers, `@@ -a,b +c,d @@` hunks, three lines
  of context, `\ No newline at end of file` markers). Plan ops share
  the transient session, so cumulative edits to the same file collapse
  to one diff. The overlay is discarded before returning — the working
  tree is byte-identical before and after the call. Lands the
  preview-only renderer child of the AST-precise edit primitive epic
  (#2497).
- **Cookbook chapter "Precise edits with AST tools" + agent-loop pattern recipe.**
  Wraps the five `std/edit` primitives (`edit_apply_node`,
  `edit_insert_at_anchor`, `edit_rename_symbol`, `edit_dry_run`,
  `edit_safe_text_patch`) into a Diataxis "How to X" chapter under
  [`docs/src/cookbook.md`](docs/src/cookbook.md). Adds the
  shape → primitive decision table, a "How to rename a symbol
  cross-file" recipe, a "When AST tools won't work" fallback section,
  and a "How to nudge an agent toward AST tools" recipe that lifts the
  canonical edit-strategy `system_reminder` body and `inject_reminder`
  wiring so other agent loops can adopt the same default. Cross-linked
  from [`docs/src/llm/agent_loop.md`](docs/src/llm/agent_loop.md) and
  the repo [`AGENTS.md`](AGENTS.md). Demonstrated end-to-end in the
  [fixer persona](personas/fixer/lib/remediation_plan.harn): a pure
  `plan_for_atoms` mapper that turns remediation atom shapes into
  `edit_dry_run`-shaped plan ops, with 7 deterministic tests under
  [`personas/fixer/tests/`](personas/fixer/tests/). Lands the docs
  child of the AST-precise edit primitive epic (#2497).
- **Postgres hostlib v2: advisory locks + LISTEN/NOTIFY + pool/circuit
  observability + read replicas + introspection + partitions + array
  decoding (#2512).** Production-readiness sweep building on the v1 MVP
  (#2500). Adds:
  - `pg_advisory_xact_lock` / `pg_try_advisory_xact_lock` /
    `pg_with_advisory_lock` — transaction-scoped locks with optional
    cross-tenant key namespacing.
  - `pg_listen` / `pg_listener_recv` / `pg_listener_close` /
    `pg_notify` — sqlx-backed PgListener bridge with auto-reconnect and
    opt-in republish onto the in-process channel bus.
  - `pg_pool_stats` — size/idle/in_use/max + statement-cache capacity +
    replica count + circuit-breaker state.
  - `circuit_breaker` option on `pg_pool` — per-pool failure budget +
    cooldown + single half-open probe.
  - `replicas` option + `{read_only: true}` per-query routing —
    round-robin against the configured read replicas, writes always go
    to the primary.
  - `pg_introspect_tables` / `pg_introspect_columns` /
    `pg_introspect_indexes` — read-only schema discovery.
  - `pg_partition_attach` / `pg_partition_detach` /
    `pg_partition_prune` — declarative range/list/default partition
    management with dry-run support.
  - Row decoder now handles `BOOL[]`, `INT2[]`, `INT4[]`, `INT8[]`,
    `FLOAT4[]`, `FLOAT8[]`, `TEXT[]`, `VARCHAR[]`, `UUID[]`, `JSON[]`,
    and `JSONB[]` natively.
- **Standardised observability primitive (`harness.obs.*`).** Adds the
  cross-cutting observability surface every harn-serve primitive
  (sessions, permissions, MCP host, compaction, pg, http codec) routes
  through. The typed sub-handle exposes `span` / `start_span` / `end_span`,
  `counter` / `histogram` / `gauge` instruments, `log`, and the ambient
  `request_id()` pushed by the dispatching host. A published vocabulary
  (`harn.session.*`, `harn.permission.*`, `harn.mcp.*`, `harn.compaction.*`,
  `harn.pg.*`, `harn.http.*`) is validated at emit time — typos fail with
  `HARN-OBS-002` instead of drifting silently into dashboards. The new
  `harn-obs-audit` conformance gate replays captured events back through
  the same schema (every metric tagged with an instrument, every span_end
  carrying a trace_id) so cloud endpoint ports in epic E can fail CI on
  schema regressions. `harn serve --obs <auto|stdout|stderr|otel|off>`
  pins the routing for local dev without touching env vars. `CallRequest`
  gains a `request_id: Option<String>` field that the A2A and MCP
  adapters mint per ingress, and the existing A.4 error envelope already
  round-trips it through `request_id`. Closes #2513 (epic #2496, A.10).
- **Rate-limiting + backpressure + per-call budget primitive on `harn-serve` (#2514).**
  Declarative `@limits(per_tenant: "100/min", per_scope: "1000/min",
  per_route: "5000/min", burst: 50, algorithm: "token_bucket",
  in_flight_max: 20)` and `@budget(llm_cost_usd: 0.50, llm_tokens:
  10000)` attributes on exported `.harn` handlers feed a new
  `LimitRegistry` that owns three pluggable algorithms (token bucket,
  sliding window, leaky bucket), an in-flight watermark for
  backpressure, per-tenant multipliers, and a rejection stats surface
  for the observability primitive (A.10). Mid-dispatch exhaustion of
  the declared LLM cost or token ceilings reuses harn-vm's per-thread
  counters via new `install_llm_cost_budget` /
  `install_llm_token_budget` RAII guards, raised as
  `BudgetExceeded`-categorised runtime errors. Both rate-limit
  rejections and budget exhaustion render through `http_codec` as HTTP
  429 with a `Retry-After` header and structured `{ code, message,
  details, request_id }` body that every adapter (mcp, a2a, api)
  shares. Subsumes the per-tenant / per-IP / queue-depth enforcement
  that `harn-cloud-gateway::http_utils::check_rate_limits` encoded as
  bespoke middleware. `@budget(mcp_calls, pg_queries)` enforcement
  lands in the follow-up #2576. Closes #2514 (epic #2496, A.11).
- **A.12 transport completeness for `harn-serve` (#2515).** Adapters
  now compose `harn_serve::apply_transport_layers(router, &config)` to
  enable gzip/brotli/zstd response compression, conditional GET via
  strong ETag + `If-None-Match` → `304 Not Modified`, and declarative
  CORS — all driven by the request's `Accept-Encoding`, `If-None-Match`,
  and `Origin` headers without each handler having to wire them
  manually. A new `harn_serve::ws_route(handler, WsConfig)` mounts a
  WebSocket route on the same axum router with subprotocol
  negotiation, automatic ping-based heartbeat, and a per-frame size
  cap. `.harn` handlers reach the same surface through four new
  builtins: `http_etag(body)` (strong hex-sha256 ETag),
  `http_choose(accept, offers, default?)` (q-value-aware content
  negotiation), `http_not_modified(etag?, headers?)` (the 304
  envelope), and `http_upgrade_ws(req, options)` (101 envelope with
  subprotocol negotiation already applied). A new
  `crates/harn-serve/tests/transport_conformance.rs` exercises the
  full WS-echo + multipart-upload + gzip + 304 + CORS-preflight cycle
  end-to-end through axum.
- **Sandbox runtime arm for the permission primitive (#2516).** A
  sandbox is now the runtime answer to a declared permission policy.
  `harn-hostlib` gains a `sandbox` module — the pluggable
  `SandboxBackend` contract (`SandboxSpec`, `ExecRequest`/`ExecResult`,
  `NetworkPolicy`, mounts, limits) plus a `LocalSandbox` backend that
  confines every command through `harn-vm`'s process sandbox
  (Landlock/seccomp, `sandbox-exec`, Job Objects, `pledge`/`unveil`)
  rather than reimplementing OS confinement. `harn-serve`'s
  `permissions::enforcement` lowers a `PermissionPolicy` into the
  enforcement vocabularies it already uses: `to_capability_policy`
  derives the in-VM `CapabilityPolicy` ceiling (read/write/exec →
  capabilities + `side_effect_level`), and `to_sandbox_spec` /
  `to_network_policy` (with the `hostlib` feature) turn the `net`
  allowlist into an egress policy. This is the canonical home for the
  enforcement contract that `harn-cloud-sandbox` previously duplicated,
  and the abstraction the planned Modal/E2B/Daytona/Fly backends build
  on.
- **AST-precise edit language coverage: data/markup grammars + capability matrix (#2519).**
  The `ast`/`std/edit` edit primitives now cover seven data, markup, and
  config grammars — **JSON, YAML, TOML, CSS, HTML, SQL, and Markdown** —
  on top of the existing general-purpose set. The query-driven
  `edit_apply_node` and `edit_insert_at_anchor` work against every
  registered grammar with no per-language code. A new `ast.capabilities`
  builtin (`edit_capabilities` in `std/edit`) reports the per-language
  matrix — `{apply_node, insert_at_anchor, rename_symbol, symbols}` —
  so the agent loop can pick an AST primitive or fall back to text
  without a hard-coded language list. Every `unsupported_language`
  response now carries a `fallback_suggestion`. The per-language
  onboarding contract is consolidated onto the `Language` enum
  (`rename_identifier_kinds`, `edit_capabilities`, `primary_extension`),
  documented in the [Edit stdlib](stdlib/edit.md) capability matrix, and
  locked in by a cross-language edit-correctness conformance suite plus
  the `edit-language-coverage` demo.
- **Structured refactorings in `std/edit` (#2520).** Eight compound,
  language-aware refactorings composed on top of the B.1–B.5 AST-precise
  edit primitives: `edit_extract_variable`, `edit_extract_function`,
  `edit_change_signature`, `edit_add_parameter`, `edit_reorder_parameters`,
  `edit_change_return_type`, `edit_inline`, and `edit_move_decl`. Each
  resolves structure with tree-sitter, captures free variables for
  extract-function via `hostlib_ast_undefined_names`, and updates call
  sites across every caller (e.g. `add_parameter` fills the new argument
  at all sites under `callsite_strategy: "default_fill"`). All share one
  result shape and one driver: pass `dry_run: true` for a per-file unified
  diff preview that touches no bytes, or apply atomically through a
  staged-fs (#1722) transaction — every file flips together, or none do on
  the first conflict. A per-(operation, language) capability matrix returns
  `result: "unsupported"` with a reason rather than producing an unsafe
  edit. Ships the `edit-refactor` demo scenario and a "Structured
  refactorings" cookbook chapter.
Burin compass: a built-in, opt-in `compass_ast_edits` system-reminder
provider that steers the agent loop toward the AST-precise edit
primitives (`edit_apply_node`, `edit_rename_symbol`, the structured
refactors, `edit_dry_run`) over freeform text edits at session start.
Enable it with `reminders.providers.compass_ast_edits = true`. (#2521)
`harn replay --session-id <id> --events-db <path> --at <event-id>`
time-travels a recorded agent session, rehydrating and replaying it as
it stood at a past event. (#2522)
- **`@budget(mcp_calls, pg_queries)` enforcement on `harn-serve` (#2576).**
  Completes the A.11 budget surface scaffolded in #2570: declaring
  `@budget(mcp_calls: 20, pg_queries: 50)` on an exported `.harn`
  handler now installs per-dispatch ceilings backed by new harn-vm
  `install_mcp_call_budget` / `install_pg_query_budget` RAII guards.
  Each `mcp.call` charges one slot at the MCP host's call entry point
  (ahead of the response cache, so a runaway tool loop is capped even
  when it keeps hitting cached results), and each `pg_query` /
  `pg_query_one` / `pg_execute` charges one slot at the Postgres
  hostlib's query/execute entry points (mock pools included).
  Crossing a ceiling raises a structured `BudgetExceeded` error whose
  `limit` field names the dimension, which `http_codec` renders as
  HTTP 429 with `code = "budget_exceeded"` and
  `details.category = "mcp_calls"` / `"pg_queries"`. The dispatcher's
  per-class rejection telemetry now also distinguishes `llm_tokens`
  from `llm_cost_usd` exhaustion instead of collapsing both to the
  latter. Closes #2576 and the last open acceptance items of #2514
  (epic #2496, A.11).
- **Typed Postgres rows: `harn pg codegen` (#2577).** Generate one Harn
  record type per table from a directory of `.sql` migrations so query
  results can be type-checked without a live database. Replays every
  forward migration (`.sql`, excluding `.down.sql`) in lexicographic
  order — the same discovery rule `pg_migrate` uses — applying
  `CREATE TABLE`, `ALTER TABLE … ADD/DROP/ALTER/RENAME COLUMN`,
  `RENAME TO`, and `DROP TABLE` so the emitted `type <Table>Row = {…}`
  always mirrors the live schema. SQL types map to the same Harn types
  the runtime row decoder produces (`int`/`float`/`string`/`any`/`bytes`,
  `T[]` → `list<T>`); nullable columns become optional (`field: T?`).
  Annotate a result (`let r: ReceiptsRow = pg_query_one(...)`) and the
  type-checker proves every field access against the schema on disk.
  `--check` verifies the generated `--out` file is current for CI.
  Completes the typed-row-mapping bullet deferred from the A.9 hostlib
  v2 sweep (#2512).
- **Postgres hostlib loadgen harness (#2578).** New `perf/postgres/`
  crate and `make loadgen-postgres` target that drive the hostlib's
  primary-key-read path under sustained concurrency and assert
  p99 ≤ 5 ms at ≥10k req/s. Gated on `HARN_TEST_POSTGRES_URL`; self-skips
  when unset, so the wired nightly E2E step is a no-op until a Postgres
  instance is provisioned. Optionally composes with the real
  `harn-cloud-store` migrations via `HARN_TEST_CLOUD_MIGRATIONS_DIR`.
- **Postgres partition helpers grow hash + sub-partition + retention
  support (#2580).** `pg_partition_attach` now accepts a
  `{modulus, remainder}` bound shape for HASH partitions alongside the
  existing RANGE / LIST / DEFAULT forms. `pg_partition_prune` walks the
  partition tree recursively, so two-level layouts (range-by-day →
  hash-by-tenant, or the inverse) prune correctly regardless of which
  level carries the time column. Two new builtins round out the
  pg_partman-style surface: `pg_partition_retain(pool, parent,
  {keep_days: 90})` drops everything older than the retention window in
  one call, and `pg_partition_create_for_window(pool, parent,
  {interval: "day", ahead: 7})` pre-creates the next N day/hour
  partitions (the `run_maintenance` equivalent). Both accept
  `{dry_run: true}`.
- **A.12 follow-on: per-route compression opt-out + push-hint builder
  (#2515).** Two more knobs land on top of the transport stack from
  #2571. Handlers can now mark an individual response as
  uncompressible by setting `x-compress: never` — useful for SSE
  routes where chunked compression breaks flushing semantics, or for
  already-compressed binary downloads where re-encoding wastes CPU.
  A new outer middleware strips the marker before the response leaves
  the server so clients never see the implementation detail. The
  default `tower-http::DefaultPredicate` is still consulted, so SSE /
  gRPC / image filtering continues to work for routes that don't
  opt out. The corresponding constants (`COMPRESSION_OPT_OUT_HEADER`,
  `COMPRESSION_OPT_OUT_VALUE`, `HeaderOptOutPredicate`) are re-exported
  from `harn-serve` for adapters that build their own predicate
  pipelines. `.harn` handlers gain `http_push_hints(envelope, paths)`
  for emitting HTTP/2 server-push hints via `Link: <path>; rel=preload;
  as=...` headers, with `as=` inferred from the asset extension
  (`.css` → `style`, `.js`/`.mjs` → `script`, image/font/json
  extensions handled, unknown extensions emit a bare `rel=preload`).
  As a drive-by, `http_codec::merge_headers` now correctly preserves
  every value of a multi-valued envelope header (Link, Set-Cookie)
  instead of silently dropping continuation values.
- **A.12 streaming uploads for `harn-serve` (#2515).** Two new
  Rust-level primitives let adapter handlers process large inbound
  bodies without buffering the whole payload, closing the streaming
  gap the buffered `harn_vm::stdlib::multipart::multipart_parse` left
  open. `harn_serve::MultipartStream::start(multipart, config)` walks
  `axum::extract::Multipart` field-by-field and yields each
  `MultipartField { name, filename, content_type, bytes }` with a
  bounded inner bytes channel — fields stream straight into hashers,
  disk, or forwarded requests with a per-field byte cap. Companion
  `harn_serve::RequestBodyChannel::start(body, config)` exposes
  `Body::into_data_stream()` as a `mpsc` receiver for
  `Transfer-Encoding: chunked` consumers. A new
  `crates/harn-serve/tests/streaming_conformance.rs` proves both
  primitives walk a 50 MiB payload while the peak in-flight chunk
  stays bounded to the wire-shaped size (≤4× the source chunk for
  multipart, ≤2× for raw body), so the 50 MiB allocation cliff that
  `multipart_parse` would hit on the same upload is avoided. The
  `.harn` channel bridge for `http.multipart(req)` and
  `req.body_channel()` builtins lands together with the future `.harn`
  HTTP handler host (the same hosting pivot blocking the `WsSession`
  bridge per `#1870`).
- **New lint `HARN-LNT-058` (vacuous condition).** The typechecker now flags
  `if` / `while` / `guard` conditions whose result is statically determined,
  covering two patterns: (1) compound expressions that fold to a constant
  via short-circuit / negation rules — `if (false && cond)`,
  `if (true || cond)`, `if !!true`, etc. — using nil / bool / numeric /
  string literal leaves; and (2) `schema_is(x, S)` / `is_type(x, S)` whose
  answer is fixed by `x`'s static type. Bare `if true { … }` / `if false
  { … }` / `while true { … }` are intentionally skipped — they're the
  canonical Harn block-scope / disable-block / infinite-loop idioms, and
  the conformance suite (plus typical user code) relies on them. The
  schema case uses the same `intersect_types` / `types_compatible`
  machinery the narrower already uses, with a strict optional-vs-required
  check on shapes (a `{b: string?}` value can lack `b` at runtime, so it
  is *not* a guaranteed subtype of `{b: string}`). `unknown` and `any` are
  excluded — `schema_is` is genuinely informative on open-world top types.
  Modelled after Flow's `unnecessary-invariant` and typescript-eslint's
  `no-unnecessary-condition` (with `checkTypePredicates`).

### Changed

- `harn-serve`: collapsed the transport adapters' edge glue onto shared
  primitives. HTTP ingress now builds its `AuthRequest` through one
  `AuthRequest::from_http` constructor (was copied across the `api`, `a2a`,
  and `mcp` adapters), and the `a2a`/`mcp` adapters share a single
  `DispatchRuntime` for off-thread `.harn` execution (was two byte-identical
  `ExecutionRuntime` impls). Adapter wire behavior is unchanged.
- **CI lint unblocker (#2573).** Inlined the `linkme_distributed_slice_populates_with_all_builtins`
  test's `format!` arguments so it satisfies Rust 1.95's `clippy::uninlined_format_args`
  under `-Dwarnings`. The previous positional form (`"...={}, ...={}", a, b`) was the only
  diff between local and CI lint outcomes, so every open PR was red on Rust lint until this
  fix landed.
**vm:** collapsed the VM's three hand-maintained opcode dispatch tables
(`execute_op_sync`, `execute_op_async`, `Chunk::disassemble`) and the
classification helpers (`op_reads_outer_name`, `is_adaptive_binary_op`)
into a single declarative table driven by a new `define_opcodes!`
proc-macro. Adding or renaming an opcode is now a one-line edit; coverage
drift across the previously hand-maintained matches is gone. Surfaced two
pre-existing disasm bugs as part of the migration — `Op::CheckType` and
`Op::GetArgc` now render properly instead of `UNKNOWN(0x..)`, and
`Op::MethodCallSpread` reads its name-index operand at the correct
offset.
- **`HookEvent`: single-source-of-truth via `hook_events!` macro.**
  Collapses the four-place drift surface — enum definition,
  `as_str`, `parse_session_event`, `parse_provider_event` — behind
  one declarative macro entry per variant. Adding a hook event now
  takes one line; every routing table is derived. `HookEventKind`
  becomes a public primitive (`Tool` / `AgentTurn` / `Worker` /
  `Step` / `Notification` / `Session`) that drives reminder support,
  session-scope filtering, and the parser routes from a single
  declared category. As polish, redundant `#[serde(rename = "...")]`
  attributes were dropped (serde's default unit-variant encoding
  already matches each PascalCase identifier), the giant
  `clear_session_hooks` match collapsed to
  `is_session_lifecycle()`, and the duplicate
  `reminder_providers::parse_provider_event` shim was deleted in
  favor of the `HookEvent::parse_provider_event` method. Seven new
  lockdown tests enforce that future variants compile only if
  every routing table is consistent. No behavior change.
- **`harn-serve` session-store: polish + acceptance gap-fill on issue
  #2502.** Follow-up to the #2535 landing closes four acceptance items
  that were left as TODOs in the initial primitive:
  - **`ArchiveSink` wired into the retention sweep.** `StoreHooks` now
    carries an optional `archive_sink`, and the default
    `SessionStore::sweep_retention` ships archived sessions and
    tombstone records through it before the rows leave primary
    storage. Closed sessions that cross `min_age_before_archive_seconds`
    are emitted via `ArchiveSink::archive(session, events)` before
    soft-delete; hard-deletes fire `ArchiveSink::tombstone(...)` with
    the final chain root hash so the audit pipeline keeps a permanent
    record of the deletion. `SweepReport` gained `archived` +
    `tombstoned` counters; a new `RetentionPolicy::should_archive`
    predicate keeps the archive-trigger condition out of the
    soft-delete decision.
  - **SQL-level tag index + filter** (acceptance: "Per-event tag index
    for filtered list queries"). New `session_tags(session_id, tag)`
    table with a `(tag, session_id)` index. SQLite `list` now JOINs
    the tag table when `filter.tag` is set instead of post-filtering
    in Rust, and applies the cursor as keyset pagination on
    `(created_at_ms, id)` so paging through 10⁸ sessions doesn't load
    every prior row into memory.
  - **Incremental chain root hash on append.** The append hot path
    previously re-folded every event's record_hash on each commit
    (O(N) on every write). The chain root is now a versioned Merkle
    chain (`v2`) built by `chain_root_init` + `chain_root_fold`, so
    append only folds the new event's hash into the stored running
    root. `chain_root_hash(events)` still replays from genesis for
    verification; the equivalence is exercised by a new test that
    cross-checks `describe.chain_root_hash` against
    `verify.chain_root_hash`.
  - **Tracing instrumentation on every API call** (acceptance: "Every
    API call emits A.10 spans + metrics with `harn.session.*`
    attributes"). Every axum handler in `sessions::api` is now wrapped
    with `#[tracing::instrument]` emitting `harn.session.<verb>` spans
    carrying `harn.session.id`, `harn.session.tenant_id`,
    `harn.session.event_kind`, etc. The default `sweep_retention` adds
    its own span recording `harn.session.sweep.archived` /
    `soft_deleted` / `hard_deleted` counts; A.10 (#2513) will export
    them through its OTLP pipeline without further changes here.
  - **Fork chain bug fix.** Adversarial review found `fork` produced
    a broken chain on the SQLite backend (session_id rewritten on
    copied events but `record_hash` not recomputed → `verify` failed
    with HashMismatch) and a divergent shape on the memory backend
    (events kept parent's session_id, so reads on the child returned
    rows that looked like they belonged to the parent). Both backends
    now route copied events through a shared `re_anchor_events`
    helper that rewrites `session_id`, recomputes `prev_hash` +
    `record_hash` sequentially, and drops the parent's per-event
    signatures (which no longer attest the re-anchored canonical
    bytes). The child's chain stands alone as a verifiable session;
    lineage is preserved via `parent_session_id` on the meta.
  - **DRY**: removed the duplicate `chain_root_hash_from_hashes`
    helper in the SQLite backend (folded into the public
    `chain_root_fold` so memory + sqlite share one
    chain-construction primitive); collapsed `hooks()` from an
    inherent method into the new `SessionStore::hooks` trait method
    so the default sweep impl can read the archive sink without
    backend-specific plumbing; collapsed the four
    `format!("sha256:{}", hex::encode(...))` call sites in `signing`
    onto one `finalize_sha256` helper; switched the SQLite list
    `args` vec to `&'static str` parameter names so per-request
    String allocation drops to zero.

  New tests: tag-filter roundtrip, keyset cursor pagination, sweep
  archive + tombstone flow against a recording sink, chain-root
  incremental equivalence, and fork chain verifiability regression.
  Existing 34 tests still pass.
- **`edit_safe_text_patch`: polish + correctness pass.** Follow-up to
  the #2509 / #2542 landing fixes several issues caught in adversarial
  review:
  - **H2**: `create_parents: false` is now honored on the direct-disk
    path — previously `atomic_write`'s unconditional `create_dir_all`
    silently created the parent. The disk path now pre-checks the
    parent and returns a structured error with the right remediation
    hint when the directory is missing.
  - **H3**: latent precedence bug in the `hunk_conflict` error message
    fixed — `+ outcome?.error_code ?? "no_match"` parses as
    `(... + outcome?.error_code) ?? "no_match"` so the fallback never
    fires. Parenthesized + hoisted to a `let hunk_error_code` so the
    same value flows into both the top-level `failed_hunk_error_code`
    and the per-error `hunk_error_code`.
  - **M1**: new `AgentEvent::SafeTextPatchResult` carrying
    `{session_id, path, result, hunks_count, bytes_written,
    failed_hunk_index?}` fires from every terminal return path
    (applied / no_op / stale_base / hunk_conflict). Hosts subscribe to
    stream-aggregate stale-base / hunk-conflict rates and average
    hunks-per-patch without polling. The ACP adapter translates the
    event into a `progress` extension with
    `_meta.harn.kind = "safe_text_patch_result"`. New
    `hostlib_fs_emit_safe_text_patch_result` builtin routes the event
    from the Harn wrapper; silently no-ops outside a session.
  - **M3**: dropped a redundant SHA-256 pass on the commit path —
    `__edit_sha256(working)` was computed even though the hostlib
    commit echoes the same digest. Now only computed on the dry-run
    and hunk-conflict paths where the commit isn't called.
  - **M5/M6**: dropped redundant `result.changed` (it always equalled
    `result == "applied"`). Aligned `dry_run` semantics with
    `edit_apply_node` — `applied: true` now means "matcher succeeded"
    regardless of whether bytes were written, and a new top-level
    `result.dry_run` boolean disambiguates.
  - **L1/L2**: small DRY win — new `hash_label(&[u8]) -> String`
    helper collapses 4 copies of the `format!("sha256:{}", hex::...)`
    pattern.
  - **L3**: schemas tightened — `expected_hash` / `current_hash` /
    `before_sha256` / `after_sha256` now carry the
    `^sha256:[0-9a-f]{64}$` regex pattern via a shared `$defs/sha256Label`
    schema reference. `expected_hash` is now required in the response
    (was nullable but always emitted).
  - **L5**: dropped dead `failed_hunk_message` field — the error list
    already carries the same string under `errors[0].hunk_message`.
  - **L6/L7**: docs gain a bounded `stale_base` retry loop example
    and a dry-run → apply workflow example mirroring how
    `edit_apply_node` documents the same flag.
  - **Tests**: added integration coverage for non-UTF8 read
    rejection, ~1.5 MB content roundtrip, `create_parents: false`
    rejection, and the new agent-event wiring.

  H1 (sandbox-gating of the un-gated `fs/*` and `ast/*` edit
  primitives) is filed as #2548 — cross-cutting concern with sibling
  primitives out of scope here.
- **LLM builtin signatures now have a single source of truth in
  `harn-builtin-meta`, eliminating the parser/runtime drift behind #2588.** The
  rich `Ty::Shape` contracts (`LLM_CALL_OPTIONS`, `LLM_CALL_RESULT`,
  `TRANSCRIPT`, `SESSION_SNAPSHOT`, `SUB_AGENT_RESULT`, …) moved from
  `harn-parser` into the dep-free `harn_builtin_meta::shapes`, and the
  `#[harn_builtin]` sig grammar gained an `@NAME` shape-injection form. With
  shapes expressible from a single annotation, the LLM/agent builtins dropped
  `runtime_only = true` and now publish their full signatures through the macro
  — the macro is the authoritative, sole definition. Roughly thirty redundant
  hand-written static parser entries (the `provider_*`/`llm_*` config family,
  `llm_mock*`, `agent_trace*`, `__cache_*`, `with_rate_limit`, …) were deleted
  outright.

  The handful of LLM builtins the typechecker treats as first-class
  (`llm_call`, `llm_call_safe`, `llm_completion`, `llm_call_structured{,_safe,_result}`,
  `schema_recover`, plus `llm_catalog`/`llm_provider_status` reachable via
  `harness.llm.*`) are referenced by `harn-parser`'s own unit tests, which run
  without a driver-installed registry and cannot depend on `harn-vm` (it
  compiles later). Their `BuiltinSignature`s are now defined **once** as
  `pub const`s in `harn_builtin_meta::signatures` and referenced by *both* the
  parser's static table and the macro (via `sig_expr`) — a single definition
  shared across the layer boundary, with no dependency cycle and no second
  place to drift. A `runtime_only`-shadow guard test prevents the original
  drift class from returning, and `sig_expr` builtins still surface a signature
  to `harn explain`/LSP by rendering the parsed `BuiltinSignature` via `Display`.
- **VM and typechecker polish pass: DRY out the 0/1/many type-collapse
  fan-out and remove a per-call hot-path allocation.** The typechecker
  gains `collapse_members` / `collapse_members_opt` helpers that
  centralise the recurring empty→sentinel / single→member / multi→wrap
  pattern; `simplify_union`, `remove_from_union`, `narrow_to_single`,
  `intersect_union_with`, and three inference helpers now share one
  implementation. `json_schema_to_type_expr` gets the same treatment via
  a sibling helper in `type_expr.rs`. `TriggerEvent` exposes
  `qualified_kind()` so the five `format!("{}.{}", provider, kind)`
  open-codes in the dispatcher/audit/predicate paths converge on one
  source. `default_sensitive_path_patterns` becomes a `&'static
  [&'static str]` instead of a `Vec<String>` allocated on every approval
  check, and `is_sensitive_path_candidate` takes a borrowed iterator so
  custom and default patterns avoid cloning. The empty-fence stripper in
  `llm/tools/parse/syntax.rs` caches its `Regex` in a `OnceLock` instead
  of recompiling on every model turn.
- **Docs + Display + drift test follow-ups to the `#[harn_builtin]`
  cutover.** Three small polish wins on the registry shipped in PR #2575:
  - **Doc sweep.** AGENTS.md, CONTRIBUTING.md, and module-level docstrings
    in `crates/harn-vm/src/stdlib.rs`, `crates/harn-vm/src/stdlib/macros.rs`,
    and `crates/harn-builtin-macros/src/lib.rs` no longer claim the legacy
    `SyncBuiltin` / `BuiltinGroup` / `register_builtin_group` DSL still
    survives — it was deleted, and the docs now reflect that. The
    "Looking ahead" linkme section in CONTRIBUTING.md is replaced with
    a "Captured-state pattern" note that points readers at the
    `thread_local!`-backed examples in `crates/harn-vm/src/checkpoint.rs`
    and `crates/harn-vm/src/metadata.rs`.
  - **`Display` for `BuiltinSignature` and `Ty`.** Renders a parsed
    sig back into the `#[harn_builtin]` `sig = "..."` grammar — recovers
    the `T?` and `number` sugars (the sig parser desugars both into
    unions). Lets downstream tools (LSP hover, `harn explain`, error
    formatting) emit a canonical form regardless of how the macro author
    typed the original sig string.
  - **Round-trip drift test.** New
    `crates/harn-vm/tests/builtin_signature_text_drift.rs` walks
    `ALL_BUILTIN_DEFS`, renders each parsed `BuiltinSignature` through
    `Display`, canonicalizes both sides (whitespace squash + sugar
    normalization), and asserts no drift. Catches future parser tweaks
    that would silently change how `a | b | c` associates or how
    `...rest` is parsed.

  Larger follow-ups filed as separate issues for later evaluation:
  #2584 (collapse VM opcode dispatch tables via `#[harn_opcode]`),
  #2585 (collapse `HookEvent` enum/parse/render), #2586 (measure +
  decide whether the `deferred_builtin` registration path is dead
  weight post-linkme).

### Removed

- **Non-stdlib `register_builtin` callsites cut over to `#[harn_builtin]`.**
  Migrated `crates/harn-vm/src/metadata.rs` (14 builtins:
  `metadata_get` / `metadata_resolve` / `metadata_entries` / `metadata_set`
  / `metadata_save` / `metadata_stale` / `metadata_refresh_hashes` /
  `metadata_status` / `compute_content_hash` / `invalidate_facts` /
  `path_metadata_get` / `path_metadata_set` / `path_metadata_entries` /
  `scan_directory`) and `crates/harn-vm/src/step_runtime.rs`
  (`__register_step` / `__register_persona`). Per-VM captured state
  (`MetadataState`) now flows through a thread-local cell using the same
  pattern as `checkpoint.rs`, so the macro-emitted free fns stay
  signature-aligned by construction. Deleted the matching hand-maintained
  `BuiltinSignature` literals from
  `crates/harn-parser/src/builtin_signatures/signatures/project.rs`
  (metadata sigs + the 6 stale `checkpoint_*` entries left behind by
  the prior checkpoint migration).
- **DSL holdouts migrated to `#[harn_builtin]`** (phase 5).
  `crates/harn-vm/src/stdlib/path_scope_guard.rs` (2 builtins:
  `register_path_scope_guard` / `clear_path_scope_guard`) and
  `crates/harn-vm/src/stdlib/tui.rs` (3 builtins: `__tui_page` /
  `__tui_clear` / `__tui_terminal_width`) cut over to annotated free
  fns. Removes the matching `BuiltinSignature` literals from
  `crates/harn-parser/src/builtin_signatures/signatures/stdlib.rs`
  plus the now-unused `PAGER_OPTIONS` shape from `shapes.rs`.

  Three legacy-DSL modules remain (`workflow_messages` / `pool/mod.rs` /
  `workflow/register.rs`) before `stdlib::registration` can be deleted
  outright.
- **`workflow_messages.rs` migrated to `#[harn_builtin]`** (phase 6).
  11 mailbox primitives — `workflow.signal` / `workflow.query` /
  `workflow.update` (async) / `workflow.publish_query` /
  `workflow.receive` / `workflow.respond_update` / `workflow.pause` /
  `workflow.resume` / `workflow.status` / `workflow.continue_as_new`
  plus the top-level `continue_as_new` alias — cut over from the
  legacy `BuiltinGroup`/`SyncBuiltin`/`async_builtin!` DSL onto
  annotated free fns. The macro sig parser handles dotted builtin
  names (`workflow.signal`), so no `BuiltinRef` shim changes were
  needed. Deletes the corresponding `BuiltinSignature` literals from
  `workflow.rs` plus the now-unused `TY_WORKFLOW_TARGET` helper.

  Remaining DSL holdouts: `crates/harn-vm/src/stdlib/workflow/register.rs`
  (workflow executor primitives across 4 sibling modules) and
  `crates/harn-vm/src/stdlib/pool/mod.rs`. Both block deletion of the
  `stdlib::registration` module.
- **Pool + workflow/compact migrated to `#[harn_builtin]`** (phase 7).
  `crates/harn-vm/src/stdlib/pool/mod.rs` (8 builtins: `__pool_create` /
  `__pool_get` / `__pool_list` / `__pool_size` / `__pool_snapshot` /
  `__pool_simulate_restart` / `__pool_submit` (async) / `__pool_wait`
  (async)) cut over off the legacy `BuiltinGroup` DSL onto annotated
  free fns; all marked `runtime_only = true` to match the pre-migration
  intent (host helpers exposed via `RUNTIME_ONLY_EXCEPTIONS` rather than
  user-facing parser sigs).

  `crates/harn-vm/src/stdlib/workflow/compact.rs` (4 builtins:
  `select_artifacts_adaptive` / `estimate_tokens` / `microcompact` /
  `transcript_auto_compact` (async)) migrated. The remaining ~30
  workflow primitives (hooks / host / inspect) keep the DSL for now —
  follow-up will land them as the cutover continues.

  Net effect: `stdlib::registration` retains 3 callers (workflow/register.rs
  for the hooks/host/inspect primitives; everything else is `#[harn_builtin]`).
- **Workflow inspect + host + compact migrated to `#[harn_builtin]`** (phase 8).
  27 builtins move off the legacy `BuiltinGroup`/`SyncBuiltin`/`async_builtin!`
  DSL onto annotated free fns colocated with their implementations:

  - `crates/harn-vm/src/stdlib/workflow/inspect.rs` (14 builtins:
    `workflow_graph` / `workflow_validate` / `workflow_inspect` /
    `workflow_policy_report` / `workflow_clone` / `workflow_insert_node` /
    `workflow_replace_node` / `workflow_rewire` /
    `workflow_set_{model,context,auto_compact,output_visibility}_policy` /
    `workflow_diff` / `workflow_commit`).
  - `crates/harn-vm/src/stdlib/workflow/host.rs` (9 builtins: all
    `__host_workflow_*` primitives, `runtime_only = true`).
  - `crates/harn-vm/src/stdlib/workflow/compact.rs` (4 builtins:
    `select_artifacts_adaptive` / `estimate_tokens` / `microcompact` /
    `transcript_auto_compact` async).

  `register_workflow_builtins` keeps the `BuiltinGroup` shim for
  `hooks.rs` (~15 builtins) while iterating a new `MODULE_BUILTINS`
  slice for the migrated set. Once `hooks.rs` finishes,
  `crate::stdlib::registration` can be deleted outright.

  Deletes the matching `BuiltinSignature` literals from
  `workflow.rs`, `stdlib.rs`, `agents.rs` plus the dead-code
  `HOST_WORKFLOW_*_BUILTIN` constants that only powered the DSL
  signature strings.
- **`workflow/hooks.rs` migrated to `#[harn_builtin]`** (phase 9, the last
  `register_builtin_group` DSL holdout in stdlib). 15 builtins move off
  the legacy DSL onto annotated free fns colocated with their
  implementations:

  - `register_tool_hook` / `clear_tool_hooks`
  - `register_persona_hook` / `register_step_hook` / `clear_persona_hooks`
  - `register_session_hook` / `clear_session_hooks`
  - `register_checkpoint_hook`
  - `register_reminder_provider` / `clear_reminder_providers`
  - `pipeline_on_finish` / `pipeline_lifecycle_audit_log_take` /
    `pipeline_lifecycle_audit_log_snapshot`
  - `__host_settlement_agent_active` / `notify_file_edited`
  - `__host_fire_session_hook` (async) / `__host_drain_file_edits` (async)

  `workflow/register.rs` is now a thin slice + dispatcher: no more
  `SyncBuiltin` / `AsyncBuiltin` / `register_builtin_group` calls — just
  a `MODULE_BUILTINS: &[&VmBuiltinDef]` slice referencing the DEFs
  emitted by `#[harn_builtin]` annotations across `compact.rs` /
  `hooks.rs` / `host.rs` / `inspect.rs`. Deletes the matching
  `BuiltinSignature` literals from `stdlib.rs`.

  After this lands, the only remaining `stdlib::registration` consumers
  are the `crates/harn-vm/src/llm/*.rs` modules (~37 kLOC, host-side
  LLM internals — outside the user-facing stdlib surface). Migrating
  those + deleting the registration module + the `linkme` capstone are
  tracked as the remaining follow-ups.
- **linkme distributed-slice capstone for `#[harn_builtin]` registry.**
  Every `#[harn_builtin]`-annotated fn now auto-registers into a
  workspace-global `linkme::distributed_slice` (`ALL_BUILTIN_DEFS` in
  `crates/harn-vm/src/stdlib/macros.rs`). Replaces the hand-maintained
  ~80-line `out.extend_from_slice(foo::MODULE_BUILTINS)` aggregator in
  `crates/harn-vm/src/stdlib.rs` with a one-liner that returns
  `&ALL_BUILTIN_DEFS`. Adding a new builtin module no longer requires
  editing `stdlib.rs` — the macro plus linkme handle aggregation
  automatically at link time.

  Per-module `MODULE_BUILTINS` slices still exist where ordered
  registration matters (e.g. `clock::register_clock_builtins` must
  override `process::timestamp`/`elapsed` registered earlier — see
  `register_io_stdlib` for the precise sequence). Only the truly-unused
  slices (`collections::MODULE_BUILTINS`, `process::MODULE_BUILTINS`)
  were deleted.

  **rlib dead-code stripping guard** (linkme issue #36): every binary
  that exercises builtins (`harn-cli`, `harn-lsp`, `harn-dap`) now
  calls `harn_vm::stdlib::force_link()` at startup. The fn touches
  `ALL_BUILTIN_DEFS.len()` with `std::hint::black_box` (preventing LLVM
  constant-folding) and asserts the slice is non-empty — surfacing a
  silent stripping regression as a panic at first builtin call instead
  of confusing `HARN-NAM-002` errors deep in user code. A new alignment
  test, `linkme_distributed_slice_populates_with_all_builtins`,
  catches the same regression in CI.
- **`stdlib::registration` module deleted.** Final follow-up of the
  `#[harn_builtin]` cutover. The remaining 8 LLM-internal files
  (`crates/harn-vm/src/llm/*.rs` — cache, rerank, trace_builtins,
  mock_builtins, agent_host_primitives, agent_config, mod, agent_session_host,
  config_builtins) migrated off the legacy `register_builtin_group` DSL
  onto annotated free fns. With no remaining consumers, `SyncBuiltin` /
  `AsyncBuiltin` / `BuiltinGroup` / `register_builtin_group` /
  `async_builtin!` are deleted outright. Every builtin in the workspace
  now flows through `#[harn_builtin]`.
- **Deferred-builtin registration path collapsed to eager-only.** The
  lazy `register_deferred_*` machinery — added pre-linkme so the LLM stack
  stayed out of cold-start — only delayed ~74 `HashMap` inserts once per VM.
  Measured delta was 0.15 ms per VM construction (1369 µs eager vs. 1217 µs
  deferred), negligible against process spawn, while the path imposed a
  `deferred_builtin_registrars` `BTreeMap` lookup on *every* builtin
  dispatch plus a hand-maintained name list per LLM module that had to be
  kept in sync with eager registration. Deleted
  `register_vm_stdlib_with_deferred_llm`, `register_deferred_builtin_defs`,
  `Vm::register_deferred_builtin` / `ensure_deferred_builtin`, the
  `deferred_builtin_registrars` VM field, every `register_deferred_*`
  module helper, and the duplicate `CONVERSATION_BUILTIN_NAMES` /
  `COST_BUILTIN_NAMES` lists they fed. All call sites
  (`harn run`, `harn bench`, the MCP test harness) now use the eager
  `register_vm_stdlib`.

### Fixed

- **LSP signature expectations updated after #2575.** The LSP unit test
  for runtime-only `provider_capabilities` / `llm_available_providers`
  builtins was still pinned to the old untyped form. Updated to match
  the typed sigs published by the `#[harn_builtin]` migration. No
  user-visible runtime behavior change.
- **LSP builtin signatures sourced from one canonical place.** The language
  server's `builtin_details()` previously read each builtin's signature and
  doc straight from VM runtime metadata, whose per-name entry is whatever code
  path registered last. A builtin carrying both a DSL registration and a typed
  `#[harn_builtin]` descriptor could surface either spelling depending on
  registration order, which differed between local `cargo test` and CI nextest
  runs (the `provider_capabilities` flake on #2568). The LSP now anchors on the
  link-time `#[harn_builtin]` descriptor slice (`all_builtin_defs()`) — which
  includes `runtime_only` builtins with their authored `signature_text` — and
  falls back to runtime metadata only for legacy DSL-only registrations.
  Curated `BUILTINS` overrides still win. A new
  `lsp_signature_matches_descriptor_text_except_curated_overrides` test pins the
  invariant so the surface can never drift back to a registration-order-
  dependent spelling.
- **LLM config builtin signatures now agree between the runtime descriptor
  and the parser registry (found while fixing #2588).** The `#[harn_builtin]`
  cutover (#2575) left several `runtime_only = true` LLM builtins with a
  hand-written static parser signature that had drifted from the authored
  `sig`. `provider_capabilities`' `model` parameter is now `string|nil`
  (matching the runtime, which accepts a nil model) instead of the narrower
  `string`, and the coarse runtime `sig` strings that `harn explain` / LSP
  surface are corrected to match actual return values:
  `provider_capabilities_clear`, `provider_capabilities_install`, and
  `provider_register` return `bool`, `llm_config` returns `dict|nil`, and
  `llm_rate_limit` returns `bool|int|nil`. No runtime behavior changes — only
  the advertised types. `runtime_only` is retained because the parser entries
  for the richer LLM builtins (`llm_call`, transcript helpers, …)
  intentionally carry typed shapes the `sig` grammar cannot express.
- **Typechecker: `intersect_types` now handles `iter<T>` and `owned<T>`.**
  Both kinds had no entry in the intersection table, so any `schema_is(x,
  S)` whose static `x` happened to be an `iter` or `owned` value silently
  dropped the relevant union member and left `x` un-narrowed in the
  truthy branch. `iter<T>` now intersects with `Named("iter")` and with
  another `iter<T>` the same way `list<T>` does. `owned<T>` is
  transparent at the equality boundary but the annotation survives the
  intersection — `owned<channel> ∩ channel = owned<channel>` — so the
  HARN-OWN-005 leak lint keeps tracking the narrowed binding.

- **Typechecker cleanup: collapse `Named ↔ parameterised` arms in
  `intersect_types`.** Each `(Named, T) / (T, Named)` pair produces the
  same intersection regardless of operand order, so the 12 individual
  arms (`Shape`, `DictType`, `List`, `Iter`, `Generator`, `Stream`, plus
  `unknown`/`any`) now share a single OR-pattern per kind. The width-
  subtyping merge for `Shape ∩ Shape` and the Union-distributes-over-
  intersection logic both move into named helpers (`intersect_shapes`,
  `intersect_union_with`) so the top-level match reads as a kind table
  without lossy duplication.
- **`orchestrator_worker_claim_expiry_requeue` conformance test
  de-flaked under load.** The original `sleep(120ms)` after the failing
  drain assumed the first subprocess's 25ms heartbeat (TTL/2 with
  TTL=50ms) would have stopped by the time the second drain spawned.
  Under heavy parallel cargo load on the same host the second `harn`
  process could cold-start and call `claim_next` while the last
  heartbeat was still alive, see zero ready jobs, and trip the
  reclaim assertion. Replaced with a poll-with-budget loop that
  re-runs `drain` (idempotent — a no-op drain returns
  `drained:0` without consuming state) at 50ms intervals up to 5s
  until `drained == 1 && acked == 1 && deferred == 0`. Same code
  shape as `wait_for_readyz` in
  `orchestrator_recover_stranded_envelopes.harn`.
- **`publish-release.yml` self-heals when tag-push run misses publish.**
  Two failures observed during the v0.8.47 cut: the tag-push run of
  `publish-release.yml` was cancelled by the shared `publish-release`
  concurrency group (the simultaneous main-push run for the same SHA
  was still queued), and the subsequent main-push runs all no-op'd
  because `Cargo.toml` already matched the latest tag. Two fixes:
  - Concurrency group now scopes by `github.ref_name` so tag-push
    and main-push runs of the same release commit can run in
    parallel without contending. Different versions (the only case
    where main-push would actually publish) are already in different
    groups; same-version contention is the only thing the old
    unscoped group prevented, which is exactly the case crates.io
    publish-skip already makes idempotent.
  - Drift detection now treats "tag exists but GitHub release has
    fewer than 5 assets" as drift, forcing the publish job to re-fire
    from the existing tag (`PUBLISH_REF` pinned to `$LATEST_TAG`).
    `release_ship.sh --finalize` skips already-published crates, so
    the recovery is a no-op for crates that landed and a real
    recovery for any that didn't.

### Security

- **Gate the `std/edit` file-mutation builtins behind `tools:deterministic` (#2548).**
  `hostlib_fs_safe_text_patch`, `hostlib_fs_read_text`,
  `hostlib_ast_apply_node`, and `hostlib_ast_insert_at_anchor` now require
  `hostlib_enable("tools:deterministic")` before they run, matching the
  existing gate on the `hostlib_tools_*` file I/O builtins. Previously a
  sandboxed script denied `tools:deterministic` could still read and mutate
  arbitrary host paths through the `edit_*` helpers. The gating policy is
  now a single shared `permissions::gated_handler` used by the `tools`,
  `fs`, and `ast` capabilities. Pure telemetry routing
  (`hostlib_fs_emit_safe_text_patch_result`) stays un-gated.
- **Workspace-root path-scope enforcement for the hostlib filesystem
  builtins (#2600).** Follow-up to the coarse `tools:deterministic` gate
  (#2548). `harn-vm` now exposes a public, `VmError`-free
  `process_sandbox::check_fs_path_scope(path, FsAccess)` entrypoint, and
  every path-accepting hostlib builtin — `tools` `read_file` /
  `write_file` / `delete_file` / `list_directory` / `search` /
  `get_file_outline`, `fs` `safe_text_patch` / `read_text`, and `ast`
  `apply_node` / `insert_at_anchor` — now refuses paths that resolve
  outside the active policy's `workspace_roots` under a restricted
  `SandboxProfile`, via one shared `enforce_path_scope` helper. The
  staged-fs commit flush validates each entry's working-tree *target*
  path (not the in-workspace overlay path), so a write staged under a
  looser policy can never escape the roots active at commit time.
  Rejections surface as a new `HostlibError::SandboxViolation`
  (`tool_rejected`) carrying the offending path, matching the message the
  VM-native `harness.fs.*` surface already produces for the same
  out-of-root path.

## v0.8.47

### Added

- **`tenant_id` propagation through `harn-serve`.** `AuthRequest`,
  `AuthenticatedPrincipal`, `OAuthClaims`, `ApiKeyEntry`, `HmacAuthConfig`,
  `OAuth21AuthConfig`, and `CallRequest` now carry an optional
  `TenantId`. `DispatchCore` resolves it (request override → principal →
  none), installs a `harn_vm::enter_tenant` scope for the dispatched
  callable, records it as a trust-graph metadata field, and surfaces it
  as a `tenant_id` span attribute on `harn_serve.dispatch`.
- **`harness.tenant.id()` ambient.** New `HarnessTenant` sub-handle
  (typechecked, dispatched through `HarnessKind::Tenant`) returns the
  active tenant id string and raises a typed `ErrorCategory::Auth`
  runtime error when no tenant is bound. `harness.tenant.try_id()`
  returns `nil` instead for branchable callers.
- **DispatchCore auto-injects the `harness` capability handle.**
  Exported `pub fn foo(harness: Harness, ...)` now receives the runtime
  handle without the caller having to encode one through `CallArguments`
  — closes a pre-existing gap where dispatched scripts could not reach
  any `harness.*` surface.
- **Postgres bindings reach v1 (issue #2500).** `std/postgres` now exposes
  explicit savepoint control (`pg_savepoint`, `pg_release_savepoint`,
  `pg_rollback_to_savepoint`) with double-quoted identifier validation,
  a `pg_migrate(pool, {dir, table?, dry_run?})` runner that applies
  `.sql` files in lexicographic order under a Postgres advisory lock and
  records `(name, applied_at, checksum)` in a configurable
  `harn_migrations` ledger, NUMERIC column decoding via
  `rust_decimal::Decimal::to_string` so callers round-trip lossless
  precision through JSON, and `duration_ms` telemetry on every
  `pg_execute` and `pg_migrate` result.
- **`harn-serve::sessions` session-store primitive (#2502).** New
  `crates/harn-serve/src/sessions/` module owns the agent-session /
  transcript primitive: typed event taxonomy (`Message`, `ToolCall`,
  `ToolResult`, `Plan`, `Compaction`, `SystemReminder`, `Hypothesis`,
  `Receipt`, `Reminder`, `PermissionDecision`, and arbitrary
  `Custom { type, payload }`), append-only event log with
  monotonic-per-session `event_id`s and a sha256 record-hash chain,
  fork/truncate, snapshot/replay, optional per-event and per-receipt
  Ed25519 signatures, a redaction processor hook plugged into
  `harn_vm::redact::RedactionPolicy`, declarative per-tenant
  `RetentionPolicy` with soft-delete + grace + sweep, and a composable
  axum `sessions_router` mounting create/list/read/append/fork/truncate/
  snapshot/replay/close/verify/hard_delete under any prefix the caller
  picks. Two backends ship today — `MemorySessionStore` and
  `SqliteSessionStore` — both passing the same behavioural test suite.
  The Postgres backend lands once `harn-hostlib::postgres` (#2500) is
  available; the trait surface is wide enough to drop a `PgSessionStore`
  in without changing callers.
- **harn-serve permission primitive (#2503).** A single authoritative
  permission module (`harn_serve::permissions`) now owns declared
  [`PermissionPolicy`] (read/write/exec/net globs, llm provider list
  with optional cost ceiling, redaction patterns, content-hashed
  versioning, parse-time lint), persistent [`RememberRule`]s with
  per-tenant + session/workspace/user/always scope and optional
  `expires_at` auto-expiry, the [`PermissionStore`] trait + in-memory
  implementation that ties evaluation and audit together, and a
  REST surface under `/v1/permissions/*` (policy / rules / history /
  check). The existing `/v1/permission-requests/{id}/respond`
  endpoint accepts new `scope`, `expires_at`, `remember`,
  `action_pattern`, and `target_pattern` fields so an approver can
  pin the verdict as a remember-rule at any scope in one round-trip.
  Subsumes the parallel permission stacks in
  `burin-code/tui/util/permission-policy.ts`,
  `BurinApp/Services/Agent/IDEAgentDelegate.swift`, and the cloud
  gateway middleware once consumers migrate (siblings C/D/E).
- **`compaction.{policy,check,run}` primitive in harn-serve (#2505).**
  Lifts compaction *policy* (when + how to compact) into the runtime so
  every consumer surface (TUI, IDE, harn-cloud) sees the same trigger
  semantics and telemetry shape instead of reimplementing thresholds.
  `compaction.policy(opts)` declares per-session strategy + thresholds
  (`max_tokens`, `max_turns`, `safety_ratio`, `keep_last`,
  `summarize_fn`, ...); `compaction.check(session_id?)` returns a
  `{action: compact_now | defer | abandon, ...}` decision without
  mutating state; `compaction.run(session_id?, plan?)` drives the
  canonical #2323 lifecycle and returns the compacted transcript.
  Strategy registry — `summarize | summarize-then-prune | head+tail |
  window | observation_mask | custom(fn)` — lowers to existing engine
  strategies, with `summarize-then-prune` declaring a deterministic
  truncate fallback. Telemetry rides on the existing
  `AgentEvent::TranscriptCompacted`; latency lands on the result dict.
- **`edit_insert_at_anchor` — AST-precise insert relative to an anchor (#2507).**
  New `std/edit` primitive (backed by `hostlib_ast_insert_at_anchor`) for
  splicing a sibling or child node next to a Tree-Sitter anchor. `position`
  picks `before` / `after` / `first_child` / `last_child`; the anchor must
  match exactly one node; content is re-indented to the inferred target
  depth and validated by re-parsing. Companion to `edit_apply_node` from
  #2506 — where `apply_node` *replaces* a span, `insert_at_anchor` *adds*
  one. Routes through staged-fs (#1722) so the insert is atomic alongside
  siblings. Lands the second stdlib child of epic #2497; the shared edit
  helpers (`read_source`, `write_source`, `first_syntax_error`,
  `resolve_target_capture`, …) now live in `ast/edit_common.rs` instead
  of being duplicated across primitives.
- **`std/edit`: `edit_safe_text_patch` with staged-fs collision
  rejection.** New `edit_safe_text_patch({path, expected_hash, hunks,
  ...})` helper reads the target file through the staged-fs overlay
  (#1722), runs each `{old_text, new_text}` hunk through the
  `edit_apply_old_new_patch` matcher against the running post-image,
  and commits all-or-nothing through a new atomic
  `hostlib_fs_safe_text_patch` builtin. When the observed pre-image
  hash diverges from `expected_hash` the call returns
  `result == "stale_base"` with the actual `current_hash` echoed back
  so callers re-read and retry instead of blind-writing over a
  concurrent agent's edit. Per-call `telemetry` carries `applied`,
  `stale_base`, `hunk_conflict`, and `no_op` counters plus
  `hunks` count so hosts roll up collision rates without log
  scraping. Paired with a new un-gated `hostlib_fs_read_text` that
  returns `{content, sha256, size, exists}` so Harn callers can
  snapshot the overlay pre-image without enabling the deterministic
  tools feature. Closes #2509 under the AST-precise edit primitive
  epic [#2497](https://github.com/burin-labs/harn/issues/2497).
- **Local OTLP exporter auto-registration.** `harn-vm` ships an
  `events::install_otel_sink_from_env` helper that wires the existing
  `OtelSink` (`crates/harn-vm/src/events.rs`) into the thread-local
  event-sink chain whenever `HARN_OTEL_ENDPOINT` (or the standard
  `OTEL_EXPORTER_OTLP_ENDPOINT`) is set. `harn-cli`'s `async_main` calls
  it once at startup so every subcommand — `harn run`, `harn serve acp`,
  evals — exports spans through the OTLP/HTTP collector at the
  configured endpoint. Honors `HARN_OTEL_SERVICE_NAME` /
  `OTEL_SERVICE_NAME` (default `"harn"`) and `HARN_OTEL_HEADERS` /
  `OTEL_EXPORTER_OTLP_HEADERS` for header-based auth (e.g. Honeycomb
  team ID). The auto-registered batch processor uses the host's tokio
  runtime; a paired `events::shutdown_otel_sink` drains the queue
  before the runtime exits so short-lived `harn run` invocations never
  drop tail-end spans. Redaction stays under the active policy
  (`crate::redact::current_policy`), unchanged from the pre-existing
  `OtelSink` contract.
- **`#[harn_builtin]` proc-macro for stdlib registration.** Three new crates
  (`harn-builtin-meta`, `harn-builtin-registry`, `harn-builtin-macros`)
  replace the two-sided builtin registration system. A single annotated
  function now emits both the runtime `vm.register_builtin_def` entry and
  the parser `BuiltinSignature`. ~25 stdlib modules and ~412 builtins
  ported in this PR; remaining modules continue to work via the parser's
  static signature fallback during the incremental migration.

### Changed

- **Towncrier-style changelog fragments.** PRs can now drop a single
  `changelog.d/<id>.<category>.md` file (categories: `breaking`, `added`,
  `changed`, `deprecated`, `removed`, `fixed`, `security`) instead of
  hand-editing `## Unreleased`. At release time the bump fleet's
  `release_harn.harn` assembles the fragments into the Unreleased block
  (preserving any operator-authored bullets) and stages the fragment files
  for deletion in the same release commit. Removes `## Unreleased` as a
  merge-conflict hot spot for parallel PRs. Direct edits to `CHANGELOG.md`
  remain accepted (legacy path). A soft `Changelog fragment` CI gate flags
  PRs that change user-visible code without a fragment; the
  `no-changelog-needed` label bypasses it.

### Removed

- **Static builtin signature cleanup.** Deleted 506 dead static
  `BuiltinSignature` literals from
  `crates/harn-parser/src/builtin_signatures/signatures/*.rs` that were
  duplicates of `#[harn_builtin]`-installed sigs. The merge logic in
  `all_signatures()` already prefers runtime-installed entries over
  the static fallback, so these duplicates were unreachable. Deleted
  `flow.rs` and `schema.rs` entirely (100% migrated). LOC delta:
  -2191 net.
- **`regex_replace` and `regex_split` return type fix.** PR #2531
  inadvertently widened `regex_replace(...) -> string | nil` and
  `regex_split(...) -> list | nil`, breaking downstream `.harn`
  callers that expected the original `string` / `list` returns. The
  nil return is only reached on arity-violation (caught at the
  arity check before the impl in normal use). Tightened both back
  to match the pre-migration contracts.

### Fixed

- **Typechecker: `schema_is(x, S)` on a shape value no longer drops fields the
  variable was already known to have.** When `x` was typed as a shape, the
  truthy branch previously narrowed to the schema's shape verbatim, discarding
  every field the existing annotation declared — so e.g. `if schema_is(x, {b:
  string}) { x.a }` on `x: {a: int, b: string}` falsely reported `a` missing.
  Width subtyping says the value still has those fields after the check, so the
  intersection now keeps the current shape's fields, intersects overlapping
  field types, and appends schema-only required fields that the matched check
  proves are present.

## v0.8.46

### Added

- **Built-in `jsonrpc_batch(url, calls, options?)`.** JSON-RPC 2.0 batch
  client — sends an array of `{method, params?, id?, notify?}` envelopes
  and returns results aligned with input order. Per-call errors arrive
  as `{jsonrpc_error: true, ...}` dicts inside the result list so
  partial failures don't tear down the whole batch.
- **`from_xml(text, options?)` with `preserve_repeated_tag`.** Opt-in
  lossless mode that keeps the inner tag when an element has repeated
  children with the same tag (e.g.
  `<addresses><a>1</a><a>2</a></addresses>` → `{addresses: {a: [1, 2]}}`
  instead of the default `{addresses: [1, 2]}`). Use for general-purpose
  XML ingestion; default behavior is unchanged for the
  `<list><item>...</item></list>` convention.

### Fixed

- **DAP `modules` request with `moduleCount: 0`.** The previous
  `.max(1)` clamp silently returned a single module for clients that
  disable paging by passing `moduleCount: 0`. Per the DAP spec that
  value (and an omitted field) both mean "return all remaining from
  startModule"; we now honour it.
- **XML parser depth guard.** Adversarial deeply-nested XML (e.g.
  10 000 nested elements) no longer risks blowing the Rust stack.
  `from_xml` enforces a 256-deep ceiling and surfaces a structured
  "max nesting depth N exceeded" error.
- **JSON-RPC default headers case-sensitivity.** A user-supplied
  `content-type` header now overrides the default `Content-Type`
  instead of being sent alongside it. Same fix for `Accept`.
- **VS Code task provider cache invalidation.** The provider now
  flushes its cached `vscode.Task[]` when the user changes
  `harn.path` or adds/removes a workspace folder, so the next
  `Tasks: Run Task` invocation reflects the new state.

### Internal

- **XML escape fast-path.** `to_xml` now spans-copies unescaped runs
  instead of pushing chars one at a time. ASCII-dominant payloads
  (the typical case) see fewer than one `push_str` per escaped byte.
- **Dedup builtin-alias registrations.** The `to_xml`/`__to_xml`,
  `from_xml`/`__from_xml`, `deep_merge`/`__deep_merge`,
  `unique`/`__list_unique`, `dict_from_pairs`/`__dict_from_pairs`
  pairs share one closure via a `register_alias_pair` helper.
- **DAP prompt-span serialization.** `serialize_parent_chain` and
  the per-handler JSON shapes now both go through a single
  `serialize_span` helper so `burin/promptProvenance` and
  `burin/promptConsumers` can never drift.

## v0.8.45

### Added

- **Built-in `clone`, `deep_clone`, `deep_merge`.** Shallow and recursive
  copies for dicts/lists/primitives, plus a dict deep-merge with
  right-wins on conflict and recursive descent into nested dicts.
  Lifts a repeated hand-coded for-loop pattern out of every userspace
  script that wanted a decoupled copy of a context object.
- **Built-in `unique`, `dict_from_pairs`, `index_by`.** First-seen-order
  list dedupe, pair-list → dict conversion, and a fluent
  `index_by(records, { r -> r.id })` for building lookup tables from
  result lists.
- **Built-in `repeat`, `indent`, `dedent`, `word_wrap`.** Cover the
  string-formatting gaps that show up when assembling system-prompt
  text or release fact sheets — heredoc-style content can be authored
  flush-left and dedented at render time without manual stripping.
- **Built-in `to_xml` / `from_xml` (and `import "std/xml"`).** Convert a
  Harn value to an XML document and back. Dicts become tag trees,
  lists become repeated `<item>` children, primitives become text
  nodes. Designed for building tagged context blocks for LLM system
  prompts (e.g. `{previous_chats: ["x.jsonl", "y.jsonl"]}` →
  `<previous_chats><item>x.jsonl</item>...</previous_chats>`) without
  manual concatenation.
- **Built-in `jsonrpc_call(url, method, params?, options?)`.** Generic
  JSON-RPC 2.0 client over HTTP that goes through the existing egress
  allowlist and mock plumbing. Unwraps `result` automatically and
  raises a structured error dict when the server responds with an
  `error` envelope. Useful for talking to non-MCP RPC peers without
  hand-crafting envelopes through `http_post`.
- **DAP `terminate`, `modules`, `loadedSources` requests.** The IDE's
  Modules and Loaded Scripts panels are now populated, and graceful
  terminate is wired alongside `disconnect`. Capabilities advertise
  `supportsModulesRequest` and `supportsLoadedSourcesRequest` so VS
  Code surfaces the panels automatically.
- **LSP `textDocument/typeDefinition`, `textDocument/declaration`,
  `textDocument/implementation`, `textDocument/documentHighlight`.**
  Symbol-occurrence highlighting in the current document, plus the
  navigation triplet the editor's "Go to" submenu expects. For Harn
  these largely converge on `goto_definition` today, but the LSP
  surface is now spec-complete enough that VS Code stops showing a
  "command not implemented" toast.
- **VS Code task provider + problem matchers.** `Tasks: Run Task`
  surfaces `harn run/check/fmt/lint/test` entries, and two problem
  matchers (`$harn`, `$harn-lint`) route diagnostics into the
  Problems panel.

### Breaking

- **The typechecker now enforces declared generic and return contracts
  more strictly (#2492).** Generic type parameters are no longer treated
  as wildcard-compatible values, typed functions and pipelines must
  satisfy their declared return types across bare returns, fallthrough,
  nested returns, and exhaustive final matches, and top-level forward
  placeholders promote to their concrete binding types. Code that relied
  on unconstrained generics behaving like `any` or on implicitly falling
  through a typed function/pipeline now receives a type error.

### Fixed

- **Unused-variable lint now recognizes `parallel ... with { ... }`
  option expressions (#2493).** Locals used only in options such as
  `max_concurrent` are no longer reported as unused.
- **Schema-aware LLM result data inference now follows `Schema<T>`
  output declarations (#2492).** Result `data` remains optional unless
  `output_validation: "error"` is selected, matching runtime behavior
  while preserving the stronger type information.

## v0.8.44

### Breaking

- **`tally` now requires a list of strings.** `xs.tally()` previously
  used `value.display()` to derive the bucket key, which silently
  collapsed `Int(1)` and `String("1")` into the same bucket — the
  exact issue that v0.8.43's #2467 fixed for `count_by`/`group_by`.
  `tally` now matches that contract and raises a `TypeError`
  ("expected a list of strings") for non-string items. Migrate
  by mapping the items first:
  `xs.map({ x -> to_string(x) }).tally()`.

### Added

- **`harn replay` can read sessions directly from `events.sqlite`
  (#2474).** `harn replay --session-id <id> --events-db <path>`
  reconstructs a replayable run record from the session's
  sanitized `observability.agent_events` stream, and `--runs N`
  compares allowlist-normalized event material across repeated replay
  reads.
- **Agent-loop tool-surface narrowing (#2473).** `agent_loop` now narrows
  model-visible tools between turns after a rolling five-turn inactivity
  window. The `tool_surface_narrowing` option accepts `enabled`,
  `window_turns`, and `hard_keep`; explicit skill activation widens back
  to the skill-scoped surface. Narrowing emits `skill_narrow` agent
  events and ACP `session/update` extensions carrying `reason`,
  `removedTools`, and `remainingTools`.
- **`std/semver` stdlib module.** Promotes semver helpers that every
  release-tooling repo reinvents (`harn-bump-fleet/lib/semver.harn`,
  `harn/scripts/detect_bump_type.harn`) into one canonical surface:
  `strip_v`, `add_v`, `is_v_semver`, `parse`,
  `next(current, bump)`, `bump_type(current, target)`,
  `version_from_release_branch`, `version_from_tag`. `parse` returns
  `nil` for malformed input so callers can match-on-nil for soft
  validation; `next` and `bump_type` throw on non-semver input.
- **`std/text` regex + pad helpers.** Six additions remove the
  ~5-line `regex_captures + len-check + group-index` boilerplate from
  every report/parse script and stop dependent repos from
  reinventing zero-pad / table-padding loops:
  - `regex_first_capture(pattern, text)` — first group of first
    match (or `nil`).
  - `regex_capture_groups(pattern, text)` — every group of the first
    match.
  - `regex_all_first_captures(pattern, text)` — first group of every
    match.
  - `pad_right(value, width, fill = " ")`,
    `pad_left(value, width, fill = " ")` — coercing front-doors for
    the existing `.pad_right`/`.pad_left` string methods, so
    non-string callers don't reach for `to_string` first.
  - `repeat_string(text, count)` — clearer than the
    `" ".repeat(width)` method form in table-separator code.

### Changed

- **`scripts/detect_bump_type.harn` cuts over to `std/semver`.**
  Removed its private `parse(...)` / `detect(...)` semver math and
  routes through `std/semver::bump_type`, gaining stricter input
  validation (negative components reject) and a single source of
  truth for release-tooling repos.

### Fixed

- **`tally` no longer collapses `Int(1)` and `String("1")`.** See the
  breaking-change note above for the contract — this is the same
  `display()`-derived bucket-key collision that v0.8.43 fixed for
  `count_by`/`group_by`, applied to the callbackless `tally` shape.
- **`truncate_middle` head/tail bias was inverted.** The intent of
  `let head = ceil(keep / 2)` was to bias the head when `keep` is
  odd, but `keep / 2` is integer division so `ceil` was a no-op and
  the tail ended up larger than the head — opposite of the intent.
  Fixed by switching to integer ceiling division
  `(keep + 1) / 2`. With `max_chars = 6` and a 10-char input, the
  output now reads `01...9` (head=2, tail=1) instead of `0...89`
  (head=1, tail=2).
- **`std/config::parse_model_id` recognizes every runtime provider
  again.** Its prefix allowlist had drifted from
  `crates/harn-vm/src/llm/provider.rs::PROVIDER_NAMES`, so
  `parse_model_id("minimax:M2.5")` returned `provider: "auto"`
  instead of `provider: "minimax"`. Added the missing entries
  (cerebras, dashscope, huggingface, minimax, mlx, tgi, vllm, zai)
  and pointed the inline comment at the runtime list so future
  drift is visible at the call site.
- **Budget-pressure compaction now uses the orchestration compaction ladder
  (#2472).** Session transcript budget enforcement now delegates through the
  shared lifecycle path with `reason: "budget_pressure"`, tries the default LLM
  summary strategy, and falls back to deterministic truncation within the
  configured session budget when summarization is unavailable. Ordinary
  threshold compactions now persist and publish `reason: "threshold"` so live
  events and transcript metadata distinguish the trigger.

## v0.8.43

### Breaking

- **`count_by` and `group_by` now require a string discriminator.** The
  callbacks passed to `xs.count_by(...)` and `xs.group_by(...)` previously
  stringified non-string returns via `display()`, which silently merged
  `Int(1)` and `String("1")` into the same bucket. They now raise a
  `TypeError` ("callback must return a string discriminator…") when the
  return value is anything other than a `string`. Migrate by wrapping
  the callback body with `to_string(...)`:
  `xs.count_by({ x -> to_string(x.id) })` instead of
  `xs.count_by({ x -> x.id })`. Coercion is explicit, no silent merges.
- **Root-level `stdlib/*.harn` removed.** Nine stale duplicates of the
  canonical embedded stdlib (`crates/harn-stdlib/src/stdlib/stdlib_*.harn`)
  were deleted: `collections.harn`, `math.harn`, `text.harn`, `path.harn`,
  `json.harn`, `schema.harn`, `vision.harn`, `graphql.harn`,
  `prompt_library.harn`. These files predated the embedded catalog
  refactor and had no live loader. Any external script that copied
  these paths into a Harn import should switch to the canonical
  `import "std/<module>"` form, which has been the supported surface
  since v0.6.

### Performance

- **Closure walk-skip on pure inline callbacks (#2468).** Every
  compiled `Chunk` now carries a `references_outer_names: bool` flag
  set at emit time iff the bytecode emits an env-reading opcode
  (`GetVar`, `SetVar`, `CallBuiltin`, `CallBuiltinSpread`,
  `CallSpread`, `Call`, `TailCall`, `Pipe`, `CheckType`). The
  closure-call hot path (`Vm::closure_call_env` +
  `…_for_current_frame`) short-circuits both caller-scope late-bind
  walks when the flag is false, so pure-arithmetic / comparison
  callbacks like `{x -> x * 2}` or `{x -> x % 2 == 0}` (the hot
  `.map` / `.filter` element shape) pay zero per-element walk cost.
  This is the same compile-time classification V8 / SpiderMonkey use
  to decide whether a closure needs its outer scope materialized,
  and it's the highest-impact win still on the table after the
  #2095 dispatch / call-flattening / inline-cache series.

### Fixed

- **Bytecode cache invalidates when embedded stdlib changes.** The
  per-user-file cache key (`CacheKey::from_source`) hashes the
  transitive *user* import graph but `collect_user_imports`
  deliberately drops `std/*` paths. That meant edits to any embedded
  stdlib module (e.g., adding a new helper module, tweaking a
  selector) would leave user-file bytecode cached against the *prior*
  stdlib snapshot. On disk the stale chunk could reference function
  slots that no longer matched the rebuilt stdlib, surfacing as
  spurious errors like `Generator has no method 'keys'` partway
  through an agent loop. The fix folds a stable
  `embedded_stdlib_digest()` (sha256 over every `STDLIB_SOURCES`
  entry, cached in a `OnceLock`) into `hash_transitive_user_imports`
  so any stdlib edit busts user-file caches automatically — without
  having to bump `HARN_VERSION` between developer rebuilds.
- **Module init no longer wipes the active `@step` stack.** When a
  builtin (e.g., `agent_loop`) lazy-loaded a stdlib module from
  inside a `@step`-decorated function body, the module's
  `init_chunk` ran with `self.frames` emptied. The frame-pop prune at
  end-of-chunk then called `prune_below_frame(0)` against the shared
  thread-local `STEP_STACK`, popping the *caller's* still-active
  step. PostStep hooks for that step then silently never fired.
  `vm/modules.rs::instantiate_module` now snapshots the persona/step
  context (`step_runtime::take_active_context`) around the init-chunk
  run and restores it after, so module loading is invisible to the
  step-tracking surface.

### Added

- **`HarnCatalogModel` bridges expose the v0.8.42 typed catalog fields
  (#2466).** The Swift and TypeScript code-generated bridges
  (`spec/provider-catalog/HarnProviderCatalog.swift`,
  `spec/provider-catalog/harn-provider-catalog.ts`) now surface
  `tier`, `open_weight`, `strengths`, and `benchmarks` as typed
  properties instead of unstructured JSON. Swift gets an explicit
  `init(from decoder:)` that backfills `strengths` and `benchmarks`
  to empty defaults so consumers stay buildable against
  pre-migration catalog snapshots. TypeScript narrows `tier` to the
  literal union `"small" | "mid" | "frontier" | "reasoning"`.
- **`artificialanalysis_adapter` refresh source (#2465).** New entry in
  `scripts/provider_catalog_sources.harn` parses
  `data-aa-intelligence-index`, `data-aa-swe-bench-verified`, and
  `data-aa-swe-bench-pro` attributes from
  artificialanalysis.ai's model overview block and emits a
  benchmarks-only observation. AA is aggregator-owned so the merge
  layer keeps provider-published pricing / context-window claims
  authoritative; this adapter only contributes to the `benchmarks`
  dict added in #2463. Stays out of the default `--live` source list
  until a follow-up rewires the drift-report golden.

### Changed

- **`std/llm/catalog` selector cleanups.** Three internal refactors with
  identical user-visible behavior:
  - `__capability_field` (the long `if capability == "X"` chain that
    mapped capability names to schema field names) is now a two-table
    lookup keyed off `__CAPABILITY_BOOL_FIELDS` and
    `__CAPABILITY_LIST_FIELDS`. Adding a new capability is now a
    one-line addition to whichever table fits.
  - Tier scoring weights are hoisted into a single `__TIER_WEIGHTS`
    const (`frontier: 40, reasoning: 35, mid: 20, small: 5`), plus
    named constants for the strength/open-weight/non-deprecated
    bonuses. Easier to audit and tune.
  - `best_available_models` drop order moved from positional
    `step >= N` checks into a single `__RELAX_ORDER` list. Reordering
    the relaxation policy is now a one-line edit instead of a
    fragile index shuffle.
- **Judge plumbing DRY pass.** Extracted
  `std/agent/judge_internals` with two private helpers:
  `__judge_apply_llm_overrides` (replaces the
  `for key in ["temperature", "max_tokens", "top_p", "tool_format", "reasoning_effort"]`
  block that was copy-pasted between `step_judge.harn` and
  `judge.harn`) and `__judge_classify_verdict` (the
  pass-allowlist → `{vetoed, feedback}` normalization that both
  judges needed).

## v0.8.42

### Added

- **`std/agent/sitrep` post-turn summary primitive (#2463).** New stdlib
  module exposes `agent_sitrep(messages_or_transcript, opts?)`,
  `agent_sitrep_append(...)`, and `agent_sitrep_preferred_routes()` for
  TUI / IDE hosts that want an ephemeral 3-sentence
  CURRENT STATE / WORK DONE / NEXT ACTION rundown at the end of each
  agent turn. The helper owns prompt engineering and provider
  selection: a curated reverse-chronological route list (Anthropic
  Haiku 4.5 → GPT-4o-mini → Gemini 2.5 Flash → DeepSeek V4-Flash →
  Groq → Cerebras → Together → Ollama → local → mock) lands the call
  on whichever provider has env credentials first. Returns
  `{skipped: true, reason: "no_available_provider"}` instead of
  throwing when nothing matches, so the caller can render
  "(sitrep unavailable)" without crashing the turn. Companion
  burin-code wiring tracked at burin-labs/burin-code#1266.
- **Open-weight frontier catalog wave (#2463).** Added two new
  providers — `minimax` (`api.minimax.io/v1`, `MINIMAX_API_KEY`) and
  `zai` (`api.z.ai/v1`, `ZAI_API_KEY` / `ZHIPU_API_KEY` fallback) —
  with capability rules covering native tools, thinking blocks, and
  prompt caching where supported. Catalog rows for MiniMax M2 / M2.5
  / M2.7 (+ highspeed variants) / Text-01, GLM-5 / GLM-5.1, DeepSeek
  V4-Flash / V4-Pro / `deepseek-chat` / `deepseek-reasoner`, plus
  OpenRouter mirrors (`minimax/minimax-m2{,.7}`,
  `z-ai/glm-5{,.1,v-turbo}`, `deepseek/deepseek-v4-{flash,pro}`)
  verified against `openrouter.ai/api/v1/models`. Kimi K2.6 row
  corrected to 262K context with up-to-date $0.73 / $3.49
  OpenRouter pricing.
- **Richer per-model catalog metadata (#2463).** Every cataloged
  model now carries `tier` (enum: `small` / `mid` / `frontier` /
  `reasoning`), `open_weight: bool`, `strengths: list<string>`
  (e.g. `coding`, `summarization`, `long_context`, `tool_use`,
  `reasoning`, `vision`, `speed`, `cheap`, `agentic`), and
  `benchmarks: dict<string, float>` (SWE-bench Verified + Pro and
  AA intelligence index where published). Numbers fact-checked May
  2026 against `artificialanalysis.ai`,
  `morphllm.com/claude-benchmarks`, `vellum.ai`, and
  `marc0.dev/en/leaderboard`. Exposed on `llm_catalog()` entries and
  on the JSON / TS / Swift catalog exports.
- **`std/llm/catalog` selectors (#2463).**
  `models_with({tier?, strengths?, min_benchmark?, open_weight?,
  provider?, max_input_per_mtok?, exclude_deprecated?,
  available_only?})` returns ranked candidates;
  `best_available_models(opts)` progressively relaxes constraints
  (`min_benchmark` → `max_input_per_mtok` → `strengths` → `tier`
  → `open_weight` → `provider`) until at least one model matches,
  so degenerate environments with few accessible providers still
  get a usable fallback; `pick_model(opts)` returns the top
  candidate with `_relaxed_step` metadata so callers can see how
  much the filter had to relax.
- **List helpers `take_while` / `drop_while` / `count_by`
  (#2463).** Registered as sync + async builtins, method-dispatched
  on lists (`xs.take_while(...)`), and signed up in the parser
  builtin signature table so the typechecker can reason about them.
- **`release-harn` is now a first-class embedded skill (#2463).**
  `harn skills list` and `harn skills get release-harn --full`
  resolve the merge-queue-safe release workflow alongside
  `harn-agent` / `harn-language` / `harn-providers` / etc. The body
  documents the one-PR-carries-everything shape, recovery entry
  points (`release_ship.sh --finalize`, `gh workflow run
  publish-release.yml`), and the hard rules around never pushing to
  a PR already in the merge queue. Previously the workflow was
  Claude Code session-skill-only and invisible to direct CLI users.
- **Provider catalog refresh schema carries v0.8.42 fields (#2463).**
  `observation()` in `scripts/provider_catalog_refresh.harn` now
  threads `benchmarks`, `strengths`, and `open_weight` through the
  refresh pipeline so a future live source adapter
  (`artificialanalysis.ai`, `swebench.com`) can refresh them
  alongside pricing and context window. Existing adapters that
  don't populate the new fields keep working — the helper reads
  them through `fields?.<key>` and stores nil when absent.

### Changed

- **Tier moved from pattern-rule table to per-model field (#2463).**
  The legacy `[[tier_rules]]` table in `providers.toml` (which
  classified models by glob / substring / exact match) has been
  removed. Each model row now declares its own tier directly
  (`tier = "small" | "mid" | "frontier" | "reasoning"`). The
  rule-evaluation code path remains as a runtime fallback for any
  model that does not self-declare, but the catalog is now the
  single source of truth. Aliases `frontier` / `mid` / `small` are
  retained as explicit ergonomics — they point at specific models,
  not patterns. Hosts that called `model_tier(id)` see no behaviour
  change for models with declared tiers; rows without a declaration
  now return `tier_defaults.default` (`"mid"`).

### Fixed

- **`llm_provider_status()` returned an incomplete provider set on
  first call (#2463).** The thread-local registered-provider set
  was populated by `reset_llm_state()` (called between test runs)
  but not by the CLI's `run` path, which meant `mock` (and any
  other registered-only provider) was silently absent from the
  status table until something else seeded the registry.
  `llm_provider_status_value` now calls
  `register_default_providers()` before snapshotting, so every
  caller sees the full set.
- **Conformance flake: process-spawning tests intermittently failed
  under full-suite load (#2463).** `wait_for_log_contains` in
  `conformance/tests/_common.harn` had a 15-second budget; the
  related `wait_for_exit` was already 30 seconds for exactly this
  reason. Under the full conformance suite the freshly spawned
  child could take 5–15 seconds to write its `ready` line,
  intermittently tripping `signal_process_sigterm`,
  `agent_state_resume_process`, `durable_step_run`, and
  `agent_probe`. The budget is now 30 seconds to match
  `wait_for_exit`.

## v0.8.41

### Added

- **Code-graph Cypher executor (#2434).** New `hostlib_code_index_cypher`,
  `hostlib_code_index_branch_overlay`, and `hostlib_code_index_freshness`
  builtins layer a typed Tree-Sitter symbol graph (`Function`/`Type`/
  `Module`/`Import`/`CallSite`/`Macro` nodes; `CALLS`/`REFS`/`IMPORTS`/
  `CONTAINS`/`OVERRIDES` edges) on top of the existing flat dep index, with
  a Cypher subset (`MATCH … WHERE … RETURN`, variable-length hops up to
  depth 4, no writes) and per-branch CDC delta overlays so worktree caches
  stay near-free. Capped at 10 000 rows per query and 3 disjoint patterns
  per `MATCH` so scripts can't trigger N⁴ cartesian enumeration.
- **`std/code_librarian` library (#2438).** Single-import Harn surface
  wrapping the Cypher executor and the 28 existing `hostlib_code_index_*`
  builtins: `code_librarian_query`, `code_librarian_outline`,
  `code_librarian_who_calls`, `code_librarian_what_imports`,
  `code_librarian_recent_changes`, `code_librarian_freshness`,
  `code_librarian_branch_overlay`. Documented at
  `docs/src/stdlib/code-librarian.md`; worked example at
  `examples/code_librarian_explore.harn`.
- **`code_librarian_query_nl` NL→Cypher (#2439).** Natural-language query
  layer: a small-tier LLM compiles NL questions to typed Cypher; bounded
  graph enumeration (depth ≤ 4, expansions ≤ 200) is the fallback when the
  compile path returns zero rows. 5-minute in-process result cache.
  Achieves 100% recall on the 30-question fixture corpus shipped with
  #2434.
- **Crystallization trajectory tap (#2436).** Agent-loop turn-level
  trajectories are now a candidate source for the crystallization
  pipeline, alongside Code Mode composition snippets. Replay-verifies
  before promoting; rejects candidates whose generated Harn diverges from
  the original trace outputs. `TrajectoryTap` accepts a custom replay
  allowlist via `.with_replay_allowlist(…)`.
- **`routing_policy` v2 (#2435).** Verifier-signal-driven escalation: the
  routing chain now reads verifier outcomes per attempt and escalates on
  lint/typecheck/test signals rather than just on HTTP status.
- **Demo gate CI workflow (#2437).** PRs that add new public primitives
  (stdlib builtins, host capabilities, orchestrator surfaces, language
  constructs) must also register a `harn demo` scenario; the
  `no-demo-needed` label opts out hygiene/refactor PRs. Canary demo added
  for `routing_policy`. Documented in `CONTRIBUTING.md`.

### Fixed

- **Silent literal-alias collisions in Cypher `RETURN`.** Default literal
  aliases now suffix as `value`, `value_2`, `value_3` (`RETURN 1, 2, 3`
  used to silently drop earlier literals); duplicate explicit aliases now
  parse-error.
- **Silent trace loss in trajectory crystallization.** All collected
  traces now ride through to the candidate result; dropped trace ids are
  surfaced via `tracing::warn!` when the synthesizer can only consume the
  first.
- **`IndexedFile.symbols` empty after symbol-graph rebuild (#2456).** The
  symbol-graph rebuild now feeds the same Tree-Sitter parse into the flat
  outline structure that `hostlib_code_index_outline_get` returns, so the
  outline reflects the indexed symbols on the current corpus.
- **Three pre-existing correctness bugs (#2430).** Behind-the-scenes fixes
  from a sweep of recent diffs.

### Changed

- **bincode 1.3 → 2.0 migration.** Migrated harn-vm's bytecode-cache
  serializer to bincode 2's `serde::encode_to_vec` / `decode_from_slice`
  API; bumped on-disk `SCHEMA_VERSION` 3→4 so stale caches transparently
  regenerate.
- **`fmt-harn` covers root-level `.harn` fixtures (#2451).** `stdlib/`,
  `personas/`, `tests/bridge/`, `examples/`, and `evals/` are now in
  scope; fixed `stdlib/json.harn` which had been silently broken since
  v0.4.7 (used since-removed `var` + spread-with-computed-key syntax).
- **Variable-length `*lo..` defaults to depth cap.** `*1..` (no high
  bound) now resolves to `(1, 4)` matching open-bound Cypher semantics,
  rather than the previous silent `(1, 3)`.
- **`BranchOverlay` storage is LRU-capped.** `OverlayState.overlays` is
  capped at 8 entries (FIFO eviction; active slot cleared when the live
  overlay is evicted) so long-running daemons don't accumulate full
  base-graph clones.

## v0.8.40

### Added

- **Pending bridge reminder controls.** ACP clients and Harn scripts can now
  inspect queued bridge injections with `session/pending_injections` /
  `agent_session_pending_injections(session_id)` and revoke queued reminders
  before delivery with `session/revoke_reminder` /
  `agent_session_revoke_reminder(session_id, reminder_id)`.

## v0.8.39

### Added

- **Workflow state channels design (#2219).** Added the v0 design record for
  typed workflow state channels with deterministic reducers, including the
  adoption boundary relative to artifacts, transcripts, map/join/reduce nodes,
  replay, and resume.

### Fixed

- **Release gate fresh-worktree bootstrapping.** `release_gate.sh audit` now
  installs the tree-sitter CLI before grammar checks, and `smoke-audit`
  respects `CARGO_TARGET_DIR` when locating the debug `harn` binary.
- **Windows merge-queue secret-store flake.** The Windows CI unit slice now
  forces the deterministic file backend for secret-store tests so hosted
  Credential Manager transient misses do not block unrelated releases.

## v0.8.38

### Added

- **Cross-project handoff primitives (#2208).** Finishes the workspace-anchor
  epic by layering three opt-in primitives on top of the typed anchor +
  permission matcher + cache-stable rendering:
  - `agent_session_reanchor(id, new_anchor, opts?)` atomically swaps the
    session's primary anchor mid-run. `opts.carry_transcript` (default true)
    keeps the transcript; `false` forks into a fresh empty session.
    `opts.compact: true` runs compaction before the swap (requires
    `carry_transcript: true`). Emits an `AnchorChanged` transcript event and
    a live `AgentEvent::AnchorChanged` notification.
  - `register_path_scope_guard(opts?)` / `clear_path_scope_guard()` install a
    singleton PreToolUse hook that denies (or emits a `<scope-alert>`
    reminder for) tool calls whose path-shaped args escape the session
    anchor. The reminder body lists the three handoff options the model has:
    `agent_session_add_root`, `agent_session_reanchor`, or `spawn_agent`
    against a sub-agent anchor.
  - `sub_agent_run` accepts an `anchor` option. The runtime rejects a child
    anchor that escapes the parent's anchor + mounted roots and applies the
    anchor to the child session on spawn.
- **ACP attach discovery by workspace anchor (#2413).** `session/list` now
  accepts workspace-anchor, cwd, and live-state filters, returns attach metadata
  for live, detached-retained, and replay-only sessions, and `session/load`
  reports replay-only state after retained WebSocket workers expire.
- **ACP WebSocket multi-client attach (#2411).** Live retained sessions now
  accept explicit `observer` and `controller` attach roles alongside the single
  `host_owner`. Harn routes host JSON-RPC requests only to the host owner,
  broadcasts session notifications and presence updates, and rejects read-only
  observer controls with structured role errors.
- **ACP control arbitration (#2412).** ACP WebSocket permission responses are
  first-valid-response-wins with idempotent same-actor replays and structured
  `already_decided` rejects. Session cancel and pending-inject controls now
  return stable lifecycle outcomes, carry actor metadata, and emit
  audit-visible `control_outcome` events.
- **Structural validator schema prechecks (#2365).** The
  `tool_calls_well_formed` rule now validates model-emitted native and text
  tool calls against the bound tool registry before dispatch, catching unknown
  tools, missing required arguments, and simple argument type mismatches in the
  deterministic pre-dispatch feedback path.
- **Task-plan IR comparison v2 controls (#2403).** The head-to-head eval driver
  now defaults to per-strategy output budgets, can opt into provider-native
  structured output, records finish reasons, bounded retry history, truncated
  raw output, and cost telemetry, and can run one length continuation plus one
  schema-repair retry for fairer typed-IR comparisons.
- **Pre-turn scope triage (#2210, #2226, #2227, #2229).** `agent_loop` accepts
  an opt-in `pre_turn_scope_classifier` hook from
  `std/llm/scope_classifier`. The default live classifier targets the local
  `ollama:qwen3:1.7b` model, low-confidence labels escalate to the main model,
  confident `out_of_scope` labels skip the heavy turn and synthesize the
  canonical `<scope-alert>` handoff prompt. `harn eval scope_triage` runs the
  100-case synthetic measurement harness and writes summary artifacts.
- **Workflow state channels design (#2219).** Added the v0 design record for
  typed workflow state channels with deterministic reducers, including the
  adoption boundary relative to artifacts, transcripts, map/join/reduce nodes,
  replay, and resume.

## v0.8.37

### Changed

- **Iteration-boundary runtime rename ships in the published patch (#2209).**
  The `v0.8.36` release notes documented the `turn_*` to `iteration_*`
  migration, but the implementation merged immediately after the `v0.8.36`
  tag. This patch is the first published version whose runtime, ACP extension
  stream, protocol artifacts, conformance fixtures, docs, and merge-captain
  examples all emit and accept the `iteration_start` / `iteration_end`
  vocabulary described in those notes.
- **VM string and name hot paths are cheaper (#2410).** The compiler now
  deduplicates repeated string constants per bytecode chunk, VM name operands
  avoid repeated string allocations in common paths, and interpolation writes
  stack values into one pre-sized buffer instead of allocating intermediate
  display strings. Focused Criterion probes cover many-part interpolation,
  builtin-reference lookup, and repeated compiler string constants.

## v0.8.36

### Changed

- **Iteration-boundary event and seam names (#2209).** Agent-loop boundary
  events and steering-seam checkpoints renamed from `turn_*` to `iteration_*`
  so the names match the inner-loop unit they describe and stop colliding
  with ACP's outer `prompt_turn`. Migration map:
  - Transcript events `turn_start` / `turn_end` → `iteration_start` /
    `iteration_end` (`AgentEvent::TurnStart` / `TurnEnd` Rust variants
    likewise renamed, with the wire `type` tag tracking the rename).
  - `turn_end.turn_info` payload field → `iteration_end.iteration_info`.
  - Steering seam kinds drained by `__agent_loop_checkpoint` and accepted
    by `register_checkpoint_hook(kinds, ...)`: `turn_start` / `turn_end`
    → `iteration_start` / `iteration_end`. The other seams (`pre_compact`,
    `post_compact`, `pre_tool_dispatch`, `post_tool_dispatch`,
    `daemon_idle_pre`, `daemon_idle_post`, `loop_exit`) are unchanged.
  - ACP `_harn/agentEvent` extension stream emits `iteration_start` /
    `iteration_end` (with camelCase `iterationInfo` on the end event).
    The protocol-artifact schema (`acp-session-update.schema.json`) and
    the published TypeScript / Swift / Python / Go bindings reflect the
    new vocabulary.
  - Internal `_turn_iteration` agent-loop option key renamed to
    `_iteration`. Callers that build llm-caller / tool-caller envelopes
    by hand must update the key name; the `call.turn.iteration` /
    `envelope.turn.iteration` middleware contract (the grouping field
    name `turn`) is unchanged.
  - The agent-loop's per-iteration local counter `turn_index` is now
    `iteration_index`. This is an internal rename, but anyone reading
    `loop.harn` source will see the new name.

  The doc-level glossary aliasing (`iteration` preferred over
  `turn`/`round-trip` for the inner unit, `prompt turn` reserved for the
  ACP outer cycle) was already shipped with #2231; this release brings
  the implementation in line with that vocabulary.

### Fixed

- **Step judges now skip when regeneration budget is exhausted (#2369).**
  `agent_loop(..., {step_judge: ...})` now defaults
  `skip_when_iterations_remaining` to `1`, so single-turn and final-turn
  loops do not spend their last turn on a veto that cannot be regenerated.
  The skip is emitted as a `step_judge_decision` event with `skipped: true`
  and `reason: "low_iteration_budget"`.
- **Workspace anchors now stay out of stored session prompts (#2224).**
  Agent sessions now render the active workspace anchor through a canonical
  `workspace_anchor` reminder provider instead of relying on callers to splice
  workspace/project fields into persisted system prompt text. Re-anchoring
  updates the next reminder body while leaving `agent_session_system_prompt(id)`
  byte-stable. Debug builds now assert `HARN-CACHE-001` if a caller records a
  session system prompt containing `{{workspace_*}}` or `{{project_*}}`
  template tokens.

### Added

- **Workspace path-scope permissions (#2216).** Dynamic permission policies now
  support a `path_scope` matcher via `std/tools.path_scope(...)`. It checks
  path-bearing tool args against the active session `workspace_anchor`, can
  include mounted roots by mount mode, and emits structured denial reasons.
  Sessions also expose `workspace_policy.default_mount_mode` through
  `agent_session_open(..., {workspace_policy: ...})`,
  `agent_session_workspace_policy(id)`, and
  `agent_session_set_workspace_policy(id, policy)`; the runtime default is
  `read_only`.
- **CLI path helpers (#2341).** Added `std/cli/paths` with
  `xdg_config_home(app_name)`, `xdg_data_home(app_name)`, and
  `xdg_cache_home(app_name)`. The pure-Harn helpers honor absolute XDG
  env vars, ignore relative XDG values, validate app-name segments, use
  macOS Library fallbacks, and leave directory creation to callers.
- **`harness.process.spawn_captured` (#2338).** Added a `process`
  sub-handle to `Harness` so Harn-native CLI scripts can run synchronous
  subprocess captures through the explicit capability surface. The existing
  `spawn_captured(opts)` builtin now shares the same implementation as the
  harness method.
- **LLM catalog harness handle (#2342).** Added `harness.llm.catalog()` and
  `harness.llm.providers()` as the canonical read-only model/provider catalog
  surface. The legacy `llm_catalog()` and `llm_provider_status()` builtins
  remain aliases for scripts that do not receive a `Harness` parameter.
- **Provider tool-capability coverage audit (#2362).** Added
  `harn provider capabilities audit` plus a VM unit gate that fails when a
  priced catalog model lacks explicit `native_tools` and
  `preferred_tool_format` coverage in the capability rules.
- **`harness.crypto.sha256(...)` host capability (#2337).** Added the
  `harness.crypto` sub-handle with `sha256(value)` for lowercase SHA-256 hex
  over strings or bytes. `sha256_hex(...)` remains as a compatibility alias,
  and `harn graph --json` now surfaces `harness.crypto.sha256` in
  `host_calls`.
- **Harness terminal capability sub-handle (#2339).** Added
  `harness.term.width()`, `harness.term.height()`, and
  `harness.term.read_password(prompt?)` so terminal dimensions and no-echo
  password reads are available through the typed harness surface. The existing
  `term_width()` and `term_height()` free builtins now share the same
  implementation and remain aliases for compatibility.

## v0.8.35

### Added

- **Typed workspace anchor on `SessionState` (#2215).** Sessions now carry a
  typed `WorkspaceAnchor { primary, additional_roots, anchored_at }` field
  instead of the soft `RunRecord.metadata.{workspace_id, project_root,
  workspace_root}` convention. New stdlib builtins
  `agent_session_open(id?, opts?)` (with `opts.workspace_anchor`),
  `agent_session_workspace_anchor(id)`, and
  `agent_session_set_workspace_anchor(id, anchor)` expose getters and setters.
  `additional_roots` entries describe `MountedRoot { path, mount_mode,
  mounted_at }` where `mount_mode` is `read_only` (default), `extend`, or
  `sandboxed`. The anchor travels through `agent_session_fork`, surfaces in
  `agent_session_snapshot`, and rides along with transcript metadata so
  `session_bundle` exports rebuild `BundleWorkspace { primary,
  additional_roots, anchored_at, policy }` without consulting the live
  session store. The legacy `metadata.workspace_id` /
  `metadata.project_root` / `metadata.workspace_root` read path is dropped;
  hosts populating the old keys must move to
  `agent_session_open(..., {workspace_anchor: ...})` or
  `agent_session_set_workspace_anchor(...)`. Foundation for the
  cross-project handoff epic (#2208).
- **MCP RC authorization hardening (#2186).** Centralized MCP OAuth/OIDC
  discovery helpers for protected-resource metadata, `WWW-Authenticate`
  parsing, OAuth/OIDC authorization-server discovery, issuer binding, scope
  selection, registration-mode selection, and native-app dynamic registration.
  `harn mcp login` and `harn connect` now share the helper surface, validate
  authorization-response issuers, and keep HTTP OAuth guidance separate from
  local stdio credential handling.
- **Reusable context-engineering eval primitives (#2195).** Added
  `harn eval context` plus portable `harn.context_eval.manifest.v1` and
  `harn.context_eval.report.v1` shapes for deterministic pack, projection,
  compaction, and tool-disclosure experiments. The smoke manifest exercises
  three tasks across three context modes and writes stable JSON/JSONL/Markdown
  artifacts that hosted eval tooling and downstream products can ingest.
- **Typed facts over memory (#2251).** Added `std/agent/fact` with
  `harn.fact.v1` normalization, stable `fact_...` ids,
  `store_fact(...)`, `recall_facts(...)`, and `invalidate_facts(...)`
  wrappers over `std/memory`. Facts store as `MemoryRecord.value` under
  reserved `fact:<kind>:<id>` keys, carry evidence/confidence/provenance,
  add canonical evidence tags for invalidation, and surface `HARN-FACT-NNN`
  validation codes.
- **Opt-in MCP RC client profile (#2184).** `mcp_connect(..., options)` and
  `[[mcp]] protocol_mode = "rc"` now use `server/discover`, per-request MCP
  metadata, stateless Streamable HTTP headers, unsupported-version retry,
  cache-hint capture, `input_required` retries for roots/elicitation/sampling,
  and `x-mcp-header` HTTP parameter mirroring while keeping legacy
  `2025-11-25` initialize/session behavior as the default.

- **First-class compaction instructions (#2190).** Manual session compaction,
  transcript auto-compaction, host-triggered compaction, and `agent_loop`
  auto-compaction now accept a typed `CompactionPolicy` with optional
  `instructions`, `mode`, `scope`, `preserve`, `drop`,
  `extend_default_instructions`, and `author` fields. Compaction events and
  audit metadata record the instruction mode/source, and
  `std/agent/autocompact` adds helper policies for bug-fix resumption, failing
  test preservation, and retaining the current plan.
- **Empirical coding-agent provider benchmark.** Added
  `harn eval coding-agent`, a packaged minimal repair-agent harness that
  exercises provider/model selectors across native and text tool modes, records
  normalized JSON/JSONL/Markdown artifacts, snapshots local model cleanup, and
  generates follow-up issue candidates for provider/preset abstraction leaks.
  Local runs now also emit `local_readiness.json`, and
  `harn providers recommend` exposes the same readiness evidence and local
  preset ordering that `harn quickstart` consumes.
- **Native/text tool-mode parity reporting (#2242).** `harn eval coding-agent`
  summaries now compare native and text runs by fixture, verifier result, tool
  sequence, rejected-call recovery, and linked transcript evidence so provider
  divergences are visible in the benchmark report.

- **Provider matrix lifecycle command.** Added `harn providers matrix` so the
  capability matrix docs can be regenerated and checked through the same
  provider command group that owns catalog refresh, validation, and export.
- **Generated provider support recommendations (#2198).** Added
  `harn providers support`, `docs/src/provider-support.md`, and
  `docs/provider-support.json` so catalog metadata, capability rules,
  curated provider caveats, and optional coding-agent benchmark summaries
  produce one checkable recommendation surface for docs and downstream apps.

- **Safer starter agent defaults.** The `harn new --template agent` starter now
  uses scoped read-only `agent_host_tools`, the standard `audit_agent` preset,
  and a README that calls out when to add mutating tools. `harn quickstart`
  also prefers the empirically stronger local Devstral coding preset when it is
  available.
- **Unified steering-seam catalog and pre-tool-dispatch checkpoint
  (#2211).** Every drain in the agent loop now routes through a single
  `__agent_loop_checkpoint(kind)` helper covering nine named seams —
  `turn_start`, `pre_compact`, `post_compact`, `pre_tool_dispatch`,
  `post_tool_dispatch`, `turn_end`, `daemon_idle_pre`, `daemon_idle_post`,
  `loop_exit`. The new `pre_tool_dispatch` seam fires between the LLM
  returning a tool call and the dispatcher firing it: if a host pushed an
  `interrupt_immediate` bridge injection, the pending tool batch is
  **skipped** and the reminder lands in the next iteration's prompt. This
  honors the ACP `session/remind` (#1829) contract that
  `interrupt_immediate` means "stop before the next tool fires" rather
  than "land at the next iteration boundary anyway." Plugin authors get
  one canonical extension point — `register_checkpoint_hook(kinds,
  handler)` — and every pass emits a `LoopCheckpoint` event for
  debuggers/replay. The race-window described in the issue
  ("model wants tool, host injects stop, tool fires anyway") is now
  closed and covered by `crates/harn-vm/tests/agent_loop_steering_seams.rs`.
- **Mid-tool preemption via `cancel_in_flight_tool_call` (#2213).** New
  stdlib builtin `cancel_in_flight_tool_call(session_id, call_id, opts?)`
  and matching ACP method `session/cancel_tool_call` let a host (or a
  Harn script) abort one in-flight tool call without tearing down the
  session — perfect for "stop the `git push --force` I just saw the
  model emit" without losing the rest of the agent state. The cancelled
  call returns to the loop shaped as `status: "cancelled"` (distinct
  from `status: "error"`) so the model can tell "the host stopped me"
  from "the tool errored." Options cover the human-facing `reason`
  (surfaced to the model on resumption), `inject_reminder` (default
  `true`, queues a system reminder explaining the cancel), and
  `timeout_ms` (default `5000`, returns `status: "timeout"` if the
  dispatch hasn't unwound). Both surfaces share a per-call cancellation
  registry keyed by `(session_id, call_id)`; the bridge stdio path also
  honors `session/cancel_tool_call` notifications.
- **`agent_session_push_bridge_injection(session_id, options)`.** Inverse
  of `agent_session_drain_bridge_injections` — lets a Harn-driven host
  (custom CLI, conformance test, trigger connector) queue a reminder onto
  the session's bridge without going through ACP. Returns the reminder
  id; mode defaults to `audit_only` and accepts the same delivery modes
  as `session/remind`.
- **Hard per-session transcript budgets (#2205).** First-class agent sessions
  now enforce retained message and event caps through the session store itself,
  independent of optional auto-compaction. Budget pressure rejects by default or
  applies an explicit trim/compact recovery policy, records
  `transcript_budget` audit metadata, and exposes the last budget action in
  session snapshots for host UIs.

### Changed

- **Skill hook frontmatter now requires trusted provenance (#2265).**
  Filesystem-backed skills whose detached signature chain is missing,
  tampered, untrusted, or missing endorsements still keep their compact
  catalog metadata and lazy-loadable body by default, but Harn strips
  command-bearing frontmatter (`hooks`, `command`, `run`) from both the
  startup registry and lazy hydration path unless provenance verifies.
- **`harn run` is sandboxed by default (#2258).** Direct runs now push a
  `worktree` capability policy before user code executes. The policy roots
  filesystem and subprocess cwd access at the project/cwd root and denies
  network side effects unless the operator explicitly passes `--no-sandbox`.
- **Schema traversal hardening (#2202).** Runtime schema canonicalization,
  JSON Schema/OpenAPI export, validation, runtime parameter assertions,
  sub-agent return-schema validation, and JSON-stream schema setup now share
  explicit traversal limits: 128 nested schema nodes and 256 local `$ref`
  expansions. Cyclic local references such as `$ref: "#"` now fail
  deterministically with a Harn-level schema error instead of relying on Rust
  stack depth.
- **Provider capability data owns tool-mode defaults (#2242).** Capability rows,
  `provider_capabilities(...)`, the generated provider matrix, and provider
  catalog bindings now expose `preferred_tool_format`, `tool_mode_parity`, and
  optional parity notes. Presets use that data to route known unreliable native
  tool modes to text tools without provider-specific branches.
- **Stdlib typed shape audit (#2193).** High-value stdlib helpers now expose
  reusable type aliases for their public record contracts instead of leaving
  callers to pass free-form dicts. The pass covers run artifacts, calendar,
  UI resource envelopes, waitpoints, replay corrections, tool registries, and
  trigger helper plans while preserving raw provider payload envelopes where
  hosts own the external schema.
- **Bridge inject mode `wait_for_completion` renamed to `audit_only`
  (#2212).** The previous name was a footgun: reminders queued with this
  mode drain at `loop_exit` and land in the transcript audit, but the
  model never sees them — no further LLM call runs after `loop_exit`.
  `audit_only` is truth-in-advertising. The internal
  `QueuedUserMessageMode::WaitForCompletion` variant is renamed to
  `AuditOnly`; the bridge JSON-RPC `mode` field, the
  `agent_session_drain_bridge_injections(session_id, checkpoint)`
  checkpoint argument, and the ACP `session/inject_reminder` and
  `session/inject` mode mapping all use the new name. The ACP
  `session/inject.mode = "queue"` alias now maps to `audit_only`
  (same semantics, no client-visible change). Hosts that need the model
  to see a reminder before the agent terminates should use
  `finish_step`, which drains at every iteration boundary — including
  the final `turn_end` before the loop breaks.
- **`std/timing` — first-class scoped timing spans (#2199).**
  New stdlib module replaces hand-rolled `let started =
  harness.clock.now_ms()` subtraction with a documented observability
  primitive. `timed(name, attrs, callback)` is the docs-forward callback
  shape; `start_timing` / `timing_event` / `end_timing` cover imperative
  flows. Returned `TimingSegment` carries `duration_ms` (monotonic),
  `started_at_ms` / `ended_at_ms` (wall-clock, mock-aware), status,
  attributes, and sub-phase events. Timing spans flow through the VM
  span collector under a new `SpanKind::UserTiming` so `trace_spans()`
  and `harn run --profile-json` report them as their own bucket and OTel
  exporters surface them as INTERNAL spans rather than mislabeled
  GenAI/tool spans. `with_cache_envelope` and `retry_with_result` are
  re-platformed onto the new primitive.
- **Runtime introspection tool bundle (#2188).** Optional, opt-in tools
  the model can call to answer identity questions
  (`current_model()`, `current_provider()`, `current_context_window()`,
  `current_harn_version()`, `current_harness()`,
  `available_runtime_capabilities()`, `current_compaction_policy()`)
  with the facts the runtime actually resolved for the current turn —
  not training-prior guesses or stale prompt prose. Each tool is
  `executor: "harn"` and dispatches through the VM stdlib
  short-circuit. Wire the bundle in with
  `runtime_introspection_tools(reg)` from
  `std/agent/introspection`; minimal harnesses that omit the call get
  no introspection surface. `HARN_HARNESS=<name>` configures the host
  identity reported by `current_harness()`; defaults to `"harn"`. A
  parallel `runtime_introspection()` builtin returns the full snapshot
  dict for tests and observability. See
  [Runtime introspection tools](docs/src/stdlib/runtime-introspection.md)
  for the full allowlist and integration recipes.
- **Transcript projection policies (#2189).** New
  [`transcript_project`](docs/src/llm/transcript-projection.md) builtin and
  `agent_loop(transcript_projection: ...)` option derive a clean
  model-visible prefix from an immutable raw transcript without rewriting
  audit lineage. Ships five policies: `raw`, `clean_tool_repair`,
  `squash_failed_calls`, `summary_prefix`, and `custom` (closure-driven).
  Each call appends a `transcript.projection` event (with the policy,
  reason, kept/dropped indices, and a SHA-256 prefix hash) so replay can
  reconstruct both the raw audit and the projected view deterministically.
  A signed-reasoning guardrail refuses to drop Anthropic `thinking` blocks
  that carry a `signature` (opt out with
  `respect_provider_signatures: false`). The runtime emits a typed
  `TranscriptProjected` agent event surfaced over ACP as the
  `transcript_projected` session update so Burin Code and other hosts can
  render raw vs. projected side-by-side.
- **Internal: trimmed CLI dependencies after self-host epic (#2293).**
  Audited every direct dependency in `crates/harn-cli/Cargo.toml` against
  the post-port handler tree (G1-G6, W1-W12 ports + W13 partial). All
  current deps remain in use — legacy Rust handler paths kept behind
  `HARN_CLI_IMPL=rust` for the parity-test contract still exercise them,
  per the C1 ratchet. No dependency drops shipped in this pass; rerun the
  audit after C1's follow-up promotes the `.harn` implementations to
  default-everywhere and the legacy Rust paths can be deleted.

### Fixed

- **Legacy SHA-1 webhook HMACs now require explicit opt-in (#2260).**
  `webhook_intake_register(..., algorithm: "sha1")` and
  `std/connectors/shared::verify_hmac_signature(..., "sha1")` now require
  `allow_legacy_sha1: true`; SHA-256 remains the default for new connectors.
- **Secret redaction and scanning now share one token catalog.**
  `secret_scan`, token redaction, and provider-error sanitization now use the
  same high-confidence secret patterns. JWTs, Bearer tokens, and full private
  key blocks are consistently reported or scrubbed instead of drifting across
  runtime surfaces.
- **Agent-loop iteration budgets now reject invalid numeric fields.**
  Explicit `max_iterations`, `iteration_budget.initial`,
  `iteration_budget.max`, and adaptive `iteration_budget.extend_by` values must
  be positive integers, and `iteration_budget.initial` cannot exceed
  `iteration_budget.max`. Invalid workflow stage budgets now fail with a clear
  `agent_loop` diagnostic before any provider call.
- **Skill provenance now fails closed where policy requires it (#2259).**
  Skills that declare `require_signature`, or runs started with
  `HARN_REQUIRE_SIGNED_SKILLS=1`, are omitted from the startup registry unless
  their detached signature chain verifies. User and system layer skills are also
  dropped on failed provenance checks, and unverified skills no longer surface
  executable hook frontmatter.

- **Provider tool-mode normalization is stricter.** Text-mode tool calls no
  longer leak native provider schemas or orphan native tool-result messages,
  recoverable Mistral and DeepSeek tool-call markers are normalized into Harn
  text-tool calls, and OpenRouter DeepSeek V3.2 defaults to text tools after
  empirical native-mode failures.

## v0.8.34

### Fixed

- **Qwen3 thinking + native tools regression (QwenLM/Qwen3.6 #89, #2178).**
  When `thinking` is enabled and a model is asked to call native tools,
  the Qwen3 family narrates tool intent in the reasoning trace but emits
  an empty `tool_calls` array. This produced 5+ minute single-turn
  finalize stalls and burned release-audit attempts on OpenRouter
  `qwen/qwen3.6-35b-a3b`. Two fixes: (1) every thinking-capable Qwen3
  rule in `capabilities.toml` now declares
  `auto_reasoning_overrides = { agent = "off", verify = "off", code =
  "off" }`, so the reasoning policy disables thinking for tool-using
  tasks at the data layer — replacing the prior hard-coded
  `local_qwen_route()` Rust branch with a generalizable per-route map.
  (2) `openrouter_reasoning_config(Disabled)` now emits
  `{"enabled": false}` on the wire instead of dropping the field, so
  OpenRouter actually honors the off intent (it silently ignores
  `effort: "minimal"` for Qwen).

### Changed

- **Native-tool prose completion now requires confirmation (#2178).**
  In a `loop_until_done` flow with native tools and no explicit
  `done_sentinel` / `done_judge`, the agent loop previously accepted
  prose-without-tool_calls on turn 1 as natural completion. That
  optimistic heuristic conflated "I'm done" with "I tried to call a
  tool but the channel dropped." The engine now narrows the auto-
  complete path: if ZERO tool_calls have been emitted in the session
  (successful or rejected), it injects one feedback message asking the
  model to either call a tool or restate its final answer, then
  accepts prose-only completion on the next turn. After any tool_call
  the classic heuristic applies, so existing harness behavior is
  preserved. Bounded by `max_nudges`.

- **TurnStart/TurnEnd events carry model + latency (#2178).**
  `TurnStart` now serializes `provider` and `model` alongside
  `iteration`. `TurnEnd.turn_info` projects `provider`, `model`,
  `response_ms`, `input_tokens`, `output_tokens`, `thinking_chars`
  from the LLM result so live pulse-check consumers (ACP clients,
  fleet hooks) can attribute latency and surface "still working"
  indicators without re-parsing transcript JSONL.

## v0.8.33

### Added

- **`harn test --junit`/`--json-out` now produce reports for user Harn
  tests (#2146).** The CLI accepted both flags before but silently
  dropped them outside the conformance path, leaving CI and perf-audit
  consumers parsing colourised terminal output. The writer is now shared
  across user tests and conformance: a `<testsuites>`-wrapped JUnit XML
  with `classname`/`file` attributes for each case, and a versioned JSON
  report (schemaVersion 1) with per-case `name`/`file`/`classname`/
  `outcome`/`duration_ms` plus suite-level `summary`. A missing or
  unwritable report directory now fails the run with a clear diagnostic
  instead of succeeding silently. `--watch` rejects both flags up-front
  since the watch loop never terminates.
- **`harn test --parallel` now uses an adaptive scheduler with a bounded worker
  pool (#2144).** Tests are scheduled at per-pipeline granularity rather than
  per file, so a single slow file no longer holds the run hostage. Worker count
  is set with `--jobs/-j` (or `HARN_TEST_JOBS`) and defaults to available
  parallelism capped at 8 to bound system load. The runner front-loads the
  slowest tests using a persisted `.harn/test-timings.json` cache so future
  runs balance themselves. Two new attributes tune the scheduler when isolation
  is required: `@serial(group: "name")` serializes tests sharing a fixture, and
  `@heavy(threads: N)` reserves `N` worker permits so expensive tests do not
  oversubscribe the pool. The selected worker count and scheduling mode are
  printed at the top of every run.
- **`harn test` per-test phase diagnostics (#2145).** Every `TestResult`
  now carries a `PhaseTimings { setup, compile, execute, teardown }`
  breakdown, and the summary exposes an `AggregateTimings` roll-up.
  `--timing` appends a one-line `Phase totals: …` to the existing
  slowest tests/files report. A new `--diagnose` flag (also honored via
  `HARN_TEST_DIAGNOSE=1`) prints one machine-readable
  `[harn test diag] …` line per test to stderr so downstream consumers
  (eg. burin-code's preflight) can attribute cold-start vs. assertion
  cost without external instrumentation. The phase breakdown lands on
  top of the #2164 bounded scheduler; together they replace the older
  "filtered single-test runs take ~2-4s of unattributed time" footgun
  on package-heavy suites.

### Fixed

- **`harn fix` now finishes recursive migrations with invalid fixtures
  present (#2134).** Directory-mode `fix --plan --json` and `fix --apply
  --json` continue past read, lex, and parse failures, report those files in
  `skippedFiles[]` with diagnostics, and return a nonzero exit only after all
  parseable `.harn` files have been planned or repaired.

### Changed

- **Stdlib agent prompt polish.** Clarifies loop-until-done, text-tool,
  native-tool, completion-judge, workflow-stage, and prompt-refinement
  instructions with plainer wording, explicit high-risk action caution,
  subagent delegation boundaries, and provider-agnostic JSON-only guidance.
- **`harn fix` Harness migration defaults preserve helper signatures (#2133).**
  Ambient stdio/fs/env/clock/random/net repairs now default to
  `--harness-threading local-global`, rewriting calls through the VM-level
  `harness` binding without adding `harness: Harness` to helper APIs. Use
  `--harness-threading thread-params` to opt into explicit parameter threading.
  `harn fix --plan --json` repair plans are now schema version 2 and include
  Harness threading plus impact metadata that distinguishes local rewrites from
  public signature changes and flags cross-module caller updates.

## v0.8.32

### Added

- **ACP thought-level session config.** Adds a provider-aware `thought_level`
  ACP session option and matching `reasoning_policy` / `thinking_policy`
  `llm_call` abstraction so harness authors can choose `auto`, `off`,
  `minimal`, `low`, `medium`, `high`, or `xhigh` without leaking
  provider-specific `reasoning_effort`, thinking-budget, adaptive-thinking, or
  local Qwen `/no_think` details into scripts.

### Changed

- **Pre-commit hook: scope clippy to changed packages.** Restores the
  intent of 63b3ebdc — `make lint` was re-invoking
  `cargo clippy --workspace --all-targets` on every Rust touch via the
  pre-commit hook, even for a one-file edit inside a single crate. The
  hook now runs `cargo clippy -p <pkg> -- -D warnings` for the staged
  crate(s) only, with a workspace-scope fallback when `Cargo.toml`,
  `Cargo.lock`, or the root `Makefile` change. Workspace `--all-targets`
  clippy still runs in the `Rust lint` CI job and on ad-hoc `make lint`.
- **Pre-commit hook: deduplicate the `lint-no-rust-prompt-prose` ratchet
  step** when both the Rust pattern and the ratchet pattern match the
  staged change set.
- **Pre-push hook: batch per-package `cargo check` into one invocation.**
  When multiple crates changed, the hook now runs
  `cargo check -p A -p B ... --tests` once instead of looping
  N invocations, so cargo only resolves the dep graph and re-evaluates
  fingerprints once.
- **CI: speed up the Windows Rust gate (compile+test ~8:29 → ~6:21 warm).**
  Drop the `mozilla-actions/sccache-action` wrapper on the `windows-latest`
  job. PR #2114 already documented a 0% hit rate on the harn-vm Windows
  compile plus intermittent `os error 10054` failures from the GHA backend
  dropping long rustc uploads; `build-release-binaries.yml` and
  `release-smoke.yml` already skip sccache on Windows. Swatinem rust-cache
  continues to warm `target/` across runs. Benchmarked on bench PRs
  #2127/#2128/#2129: Windows compile+test step drops from ~8:29 warm to
  ~6:21 warm. A paired `CARGO_PROFILE_DEV_DEBUG=line-tables-only` override
  was tested and actually regressed compile time, so it is not applied.

## v0.8.31

### Added

- **Path-scoped metadata builtins (#2112).** Adds `path_metadata_get(path,
  namespace?, opts?)`, `path_metadata_set(path, namespace, data, opts?)`, and
  `path_metadata_entries(namespace?, opts?)` for addressing metadata at exact
  file paths. File entries do not inherit from parent directories; pass
  `{kind: "dir"}` to fall back to the existing hierarchical directory
  resolution. Namespace shards on disk now include an optional `files` section
  alongside `entries`, and shards without it continue to load unchanged.

### Fixed

- **CI: release binary builds now actually save the Swatinem rust-cache.**
  `build-release-binaries.yml` gated `save-if` on `refs/heads/main`, but the
  workflow only runs on tag pushes (`refs/tags/v*`), so the per-target
  `release-<triple>` cache was never populated. Subsequent releases were
  always cold on every matrix leg. Saving by default lets back-to-back tag
  pushes restore the previous `target/` instead of rebuilding all deps from
  scratch.

### Changed

- **CI: split the Linux Rust gate into parallel `Rust lint` and `Rust test`
  jobs.** `make lint` and `make test` previously serialized in one ~11-min
  job; splitting them into parallel jobs with their own rust-cache shared
  keys (`workspace-lint` / `workspace-test`) lets them complete in roughly
  half the wall time. Promoted `lint-no-xfail-regression` from `make lint`
  to the `Harn conformance + audit` job, which already has `target/debug/harn`
  warm from `make conformance`, so the ratchet runs in seconds instead of
  paying for a fresh `cargo run` compile on the lint critical path. The
  required `CI status` gate now waits on both `Rust lint` and `Rust test`.

## v0.8.30

### Fixed

- **ACP pipeline VMs now install the `harness` capability handle (#2118).**
  File-backed ACP sessions now match `harn run`/`harn test` by installing a fresh
  `harness` global before each prompt execution, so stdlib helpers such as
  `std/config::env_int` and migrated pipeline code that calls
  `harness.stdio.*` work under embedded Burin lightweight pipelines.

## v0.8.29

### Fixed

- **OpenRouter structured-output routing (#2113).** Refreshes OpenRouter
  capability rules for current DeepSeek V4, Devstral, Llama 4, and Gemma 4
  families; preserves `schema_stream_aborted` error categories across
  off-thread LLM calls; emits `top_k` only when the selected route supports
  it; and requests OpenRouter `provider.require_parameters` whenever schemas
  or `top_k` must be honored by the routed backend.
- **CI: Windows release-binary builds no longer race the GHA sccache backend
  (#2114).** Skips `mozilla-actions/sccache-action` and the
  `RUSTC_WRAPPER=sccache` export on the `x86_64-pc-windows-msvc` matrix leg of
  `build-release-binaries.yml`. The GHA cache backend dropped the TLS
  connection partway through the single rustc invocation that compiles
  `harn-vm` on Windows (`os error 10054`), failing the v0.8.28 release run
  twice in a row on the same step. Other targets keep sccache, and the
  Swatinem `target/` cache still warms repeat Windows builds.

## v0.8.28

### Added

- **Command JSON helpers and ordered fallback probes (#2090).**
  `std/command` now exposes `command_json(...)` and
  `command_json_step(...)` for argv-first JSON-emitting CLIs. They reuse
  the existing command runner, artifact-backed tails, retry policies,
  classification, and recovery hints while turning non-zero exits, empty
  output, and malformed JSON into debuggable thrown errors or
  `{ok:false,error,step}` records. The new `command_try(...)` helper
  covers the narrow connector-then-CLI style of equivalent probe fallback,
  returning ordered attempt summaries plus `fallback_index` /
  `fallback_total` without adding a provider framework or second retry
  system.
- **Minimal AWS SigV4 signing helper (#2083).** Adds the pure
  `aws_sigv4_headers(spec)` builtin so connector packages can sign one AWS
  REST/JSON request with explicit credentials and pass the returned headers to
  `harness.net.request(...)`. Bedrock now reuses the same signer, temporary
  credentials emit and sign `X-Amz-Security-Token`, and tests cover fixed
  vectors, query/path canonicalization, Bedrock shape parity, mocked HTTP
  usage, and credential-safe errors.
- **Connector-safe HTTP policy helpers (#2082).** `std/connectors/shared` now
  exposes `connector_http_request`, `connector_http_json`,
  `connector_http_header`, and `connector_http_rate_limit` so Harn package
  authors can wrap `harness.net.request` with stable error envelopes,
  idempotency-aware unsafe write retries, capped `Retry-After` handling, JSON
  parse categorization, and standard rate-limit header extraction without
  hand-rolling provider loops.
- **Run artifact directory helpers (#2089).** Adds `std/run_artifacts`
  for harness-local output directories under `runtime_paths().run_root`
  or an explicit `{root}`. The module opens/reopens `.harn-runs/<kind>/<run_id>`
  directories, provides traversal-checked artifact paths, JSON/text
  read-write helpers that reuse `std/fs` fallback and newline conventions,
  transcript sidecar path helpers, standard artifact path dictionaries, and
  newest-first recent-run listing for recovery/review flows.
- **OpenAPI SDK package scaffold (#2084).** Adds
  `harn package scaffold openapi` to turn a local or HTTPS OpenAPI 3.1 spec
  into a focused Harn SDK package with generated `src/lib.harn`,
  `harn.toml`, a copied spec fixture, `scripts/regen.harn`, docs, README,
  smoke tests, and package CI. The scaffold delegates SDK source generation
  to `harn-openapi`, declares that dependency for regeneration, and documents
  when package authors should use generated helpers instead of hand-written
  `harness.net` calls.
- **`harness.net.*` access policy (#1913).** Adds the `std/net_policy`
  stdlib module so scripts can attach a per-harness allow/deny policy
  to `harness.net.*` requests: `NetPolicy.create({allow, deny,
  default, on_violation})` plus the rule constructors
  `NetPolicy.domain(host)`, `NetPolicy.domain_wildcard(pattern)`,
  `NetPolicy.cidr(range)`, and `NetPolicy.host(host_name, [ports])`.
  `harness.with_net_policy(policy)` returns a new harness whose
  `net.*` calls are gated against the rules. `on_violation` accepts
  `"error"` (throws a typed `NetPolicyViolation`), `"audit_only"`
  (allows the request but records an audit), `"quarantine"` (denies
  and flips the new sticky `harness.is_quarantined()` flag), or a
  user closure `fn(req) -> string` that picks one of the three per
  request. Every evaluation — including the `HARN_NET_POLICY_BYPASS=1`
  short-circuit — emits a `harness.net.policy.audit` event so the
  trust graph keeps an evidence trail. Mock-mode dispatch enforces the
  policy ahead of the canned-response lookup, so conformance fixtures
  can exercise the matcher without touching the network. Tracked
  through the E4.9 row of epic #1765 and lays the runtime groundwork
  for the harn-cloud `workspaces.network_policy` schema.
- **Unified user observability API (#1915).** Adds
  `std/observability` with `obs()` / direct `span`, `log`, `metric`,
  and `event` helpers over the existing VM span machinery. Runtime
  configuration uses named backend handles and factories for
  `Backend.auto`, OTel, Splunk HEC, Honeycomb, pretty stderr, and
  `compose([...])`, with routing rules by `kind`, `level`, or default
  backend. Span attributes merge into emitted log and metric fields,
  `import { obs } from "observability"` is accepted as the short stdlib
  alias, and conformance covers the unified API, backend fan-out, and
  routing payload shapes.
- **`--json` everywhere + `schemaVersion` envelope (#1753).** Extends the
  versioned `JsonEnvelope` contract to the highest-leverage commands that
  previously had no machine-readable mode: `harn lint --json`,
  `harn replay --json`, `harn version --json`, and `harn upgrade --json`
  (pairs naturally with `--check` for a self-update probe). Every new
  envelope is registered in the `harn --json-schemas` catalog with a
  stable `schemaVersion` so agents can dispatch per-command. The full
  contract — envelope shape, error layout, and per-command notes — lives
  at `docs/src/cli-json-contract.md`; a condensed cheatsheet ships in
  `docs/llm/harn-quickref.md`. Stdout stays single-document parseable;
  progress, warnings, and human-readable logs route to stderr.
- **Streaming partial-JSON validation builtin (E5.1, #1773).** New
  `std/json/stream_validate_create`, `stream_validate_chunk`, and
  `stream_validate_finalize` builtins expose the incremental JSON
  validator that already powers `llm_call`'s mid-stream
  `schema_stream_abort` (E5.2) as a standalone user-callable. Each
  call returns a plain-dict verdict
  `{verdict: "pending"|"valid"|"invalid", reason?, path?}` so streaming
  agents (SSE chunks, partial WebSocket frames, custom transports)
  can dispatch on a stable string verdict without pattern-matching enum
  variants. `stream_validate()` returns the same trio bundled as a
  namespace record (`stream_validate.create`/`.chunk`/`.finalize`).
  `finalize` transitions a still-pending validator with a partial
  document to `"invalid"` with
  `reason: "incomplete JSON document at end of stream"`; an already
  `"valid"` or `"invalid"` verdict is returned unchanged. Both the
  closure-bag `stream_validator` API and the new functions share the
  same underlying `JsonStreamValidator` storage, so there is no
  duplicate parser or validator implementation.
- **Experimental MCP file inputs (#1916).** Adds a default-off
  `harn.mcp.configure({experimental: {file_upload: ...}})` opt-in for the
  current draft MCP file-input proposal, SEP-2356. Client code can call
  `harn.mcp.upload_file(client, path, options?)` to encode a local file as an
  RFC 2397 `data:` URI for an `x-mcp-file` schema field, while Harn MCP
  servers can declare those fields with `harn.mcp.file_input(...)`. Both the
  VM MCP server and `harn-serve` MCP adapter validate incoming `data:` URI
  scheme, media-type, and decoded-size constraints before dispatching the tool
  handler. The implementation redacts inline file payloads from replay keys
  and is explicitly tagged as speculative until upstream MCP ratifies the final
  file-input shape.
- **`harn pack` signed content-addressed run bundles (#1779).** Promotes
  `harn pack` to a parent command with a `verify` subcommand and adds
  `--exclude-secrets` to the build path. `harn pack verify <bundle.harnpack>`
  reads a `.harnpack`, recomputes the canonical bundle hash, runs the
  embedded Ed25519 signature check (refusing unsigned bundles without
  `--allow-unsigned`), and cross-checks every per-module
  `source_hash_blake3` / `harnbc_hash_blake3` against the in-archive payload.
  Exits non-zero on any mismatch with stable structured error codes
  (`verify.signature_failed`, `verify.source_mismatch`,
  `verify.bytecode_mismatch`, `verify.recorded_hash_mismatch`,
  `verify.archive_failed`, `verify.unsigned`). `--exclude-secrets` refuses
  to bundle entrypoints whose path matches a conservative secret-bearing
  glob (`.env`, `.env.*`, `*.pem`, `*.key`, `credentials*`, anything under
  `secrets/`); pass `--include-secrets` to be explicit about the historical
  default. Both surfaces emit `JsonEnvelope` payloads under `--json` and
  register with `harn --json-schemas` (`pack verify` schema v1). The
  verifier also accepts `--trust-policy <policy.json>` plus
  `--require-trusted-signer` so compliance pipelines can reject bundles
  signed by unknown or allowlist-mismatched keys with the structured
  `verify.untrusted_signer` error code. The local/registry trust model is
  reused from the existing skill-provenance signer workflow. The
  conformance suite covers happy path, byte-deterministic re-pack, signed
  bundle roundtrip, signed-bundle tamper detection, secrets gate, and the
  JSON envelope contract under `conformance/tests/harn_pack/`.
- **fs / env / random / net `harness.*` sub-handles (E4.4, #1769).**
  The `harness.fs.*`, `harness.env.*`, `harness.random.*`, and
  `harness.net.*` method surfaces are now wired end-to-end in real
  mode. Each sub-handle delegates to the existing ambient builtin
  (and through it, to the existing sandbox path enforcement, egress
  allowlist, transcript tagging, and tape replay machinery), so a
  script that rewrites `read_file("notes.txt")` as
  `harness.fs.read_text("notes.txt")` keeps every guard rail. The
  parser gains four new lint codes (`HARN-LNT-054`..`HARN-LNT-057`)
  paired with four new repair templates
  (`bindings/thread-harness-{fs,env,random,net}`); `harn fix` rewrites
  every call site once `harness` is in scope and points users at the
  surface-changing `bindings/thread-harness` repair otherwise. A new
  `HARN-CAP-201` diagnostic (`sandbox capability denied by active
  sandbox profile`) is attached to runtime errors raised by harness
  sub-handle calls when the active `CapabilityPolicy` rejects the
  path or URL — both the `CategorizedError` (`sandbox violation: ...`)
  shape and the `Thrown(Dict { type: "EgressBlocked", ... })` shape
  pick up the code so scripts can pattern-match on it. `harn-ir`
  attributes `harness.<sub_handle>.<method>` calls back to the
  canonical ambient builtin so `harn graph --json` and the
  routes/invariants pipelines see them as having the same effect
  surface as the legacy ambient call. Conformance fixtures cover the
  null-deny path for `random` (the previously-missing piece in
  `conformance/tests/harness/null_*_denies`), a real-mode
  filesystem roundtrip, deterministic random ranges, and missing-env
  defaults. Actual removal of the ambient `read_file`, `env`,
  `random`, and `http_*` builtins is intentionally deferred to E4.6,
  which migrates the conformance corpus in lockstep.

### Security

- **Template render sandbox hardening.** File-backed `render(...)`,
  `render_prompt(...)`, `render_with_provenance(...)`, `{{ include ... }}`,
  and `host_call("template.render", ...)` now enforce the active
  `workspace_roots` read boundary before reading template files. This closes
  a policy bypass where template rendering could read arbitrary readable files
  even when `read_file(...)` was correctly blocked.

- **Vision OCR sandbox and audit hardening.** Path-backed `vision_ocr(...)`
  inputs now enforce the same active `workspace_roots` read boundary before
  loading image bytes, the Tesseract backend runs through the runtime process
  sandbox, and `audit.vision_ocr` records image metadata and hashes without
  retaining the raw image payload.

- **A2A access-control hardening.** `harn serve a2a` now enforces configured
  API-key/HMAC auth before any non-discovery A2A RPC or REST operation creates,
  reads, cancels, subscribes to, or mutates task state. Unauthenticated callers
  receive HTTP 401 plus `WWW-Authenticate` instead of leaving rejected task
  history behind or reaching task-management and push-callback operations.
  The A2A listener also defaults to loopback; use `--bind 0.0.0.0:PORT`
  explicitly for public deployments behind auth and TLS or a trusted edge.

- **MCP HTTP session teardown hardening.** Script-driven MCP Streamable HTTP
  now applies the same Origin and `mcp-protocol-version` checks to `DELETE
  /mcp` as it already applied to POST/GET, preventing cross-origin session
  teardown when a browser-visible session id is present.

### Changed

- **Stdio now requires the `Harness` capability handle (#1767).** Removes
  the ambient `print`, `println`, `eprint`, `eprintln`, `read_line`, and
  `prompt_user` stdlib builtins in favor of `harness.stdio.*`. `harn fix`
  now plans `bindings/thread-harness` when an existing harness binding can be
  threaded through local helper calls, and downgrades to the
  `bindings/thread-harness-needs-param` surface-changing repair when a new
  `harness: Harness` parameter is required.

- **Linear typecheck scaling on large files (#2093).** Restructures
  `TypeScope.parent` from `Box<TypeScope>` (deep-cloned on every scope
  entry) to `Rc<TypeScope>` so child scope creation is an `Rc::clone`
  rather than a recursive copy of the parent chain. Each function /
  pipeline body now enters its scope via a refcount bump against the
  shared root, and non-generic call sites skip the per-call
  `call_scope.clone()` by borrowing the caller's scope directly.
  Cold-start typecheck phase on a synthetic file with one-line fns and
  500 call sites in the main pipeline drops from previously
  super-linear to flat O(n): 500 fns 69 ms → 3 ms (23×), 2000 fns
  438 ms → 6 ms (73×), 5000 fns 2.18 s → 14 ms (156×), 10000 fns
  8.27 s → 29 ms (285×). LSP re-typecheck on save and `harn check` in
  pre-commit on large projects pick up the same speedup.

## v0.8.27

### Added

- **OpenTrustGraph schema v0.1 (#1778).** Bumps the trust-record schema
  discriminator from `opentrustgraph/v0` to `opentrustgraph/v0.1` and
  reserves three additional keys under `TrustRecord.metadata`:
  `effects_grant` (typed `EffectRecord` list extended by the parent),
  `effects_used` (typed `EffectRecord` list the action actually
  exercised), and `parent_record_id` (pointer at the parent record's
  `record_id`). Verifiers can now prove that a child agent's
  `effects_used ⊆ parent.effects_grant`, closing the receipt-chain
  inclusion proof flagged in E5.5. Backwards compatible at the record
  layer: `TrustRecord` deserialization still accepts the older
  `opentrustgraph/v0` discriminator for one patch release window per
  the new [`opentrustgraph-spec/CONFORMANCE.md` §5.1][otg-v0-1] before
  v0 is retired. The chain hash inputs and chain-export envelope
  (`opentrustgraph-chain/v0`) are unchanged because metadata was
  already hash-covered. Ships the new `v0.1` JSON Schema at
  `opentrustgraph-spec/schemas/trust-record.v0.1.schema.json`, a new
  3-agent fixture
  `opentrustgraph-spec/fixtures/valid/effect-inheritance-chain.json`
  demonstrating the inheritance invariant end-to-end, and extends the
  pure-Python reference verifier (`examples/python/verify_chain.py`) to
  enforce the subset check. New helpers on `TrustRecord`
  (`with_effects_grant`, `with_effects_used`, `with_parent_record_id`,
  plus matching getters and setters) keep callers from poking at the
  reserved metadata keys by hand. Exposes new public constants
  (`OPENTRUSTGRAPH_SCHEMA_V0_1`, `OPENTRUSTGRAPH_ACCEPTED_SCHEMAS`,
  `METADATA_KEY_EFFECTS_GRANT`, `METADATA_KEY_EFFECTS_USED`,
  `METADATA_KEY_PARENT_RECORD_ID`) and the matching re-exports from
  `harn_vm`. The harn-cloud receipt-store replatform is tracked
  separately (paired ticket under epic #1773); both versions are
  accepted on ingest until then.

  [otg-v0-1]: https://github.com/burin-labs/harn/blob/main/opentrustgraph-spec/CONFORMANCE.md#51-v01-reserved-metadata-keys-1778

- **Suspend/resume conformance suite (S-11, #1847).** Closes the
  remaining gap in the suspend/resume executable spec (epic #1836) by
  adding four new fixtures under `conformance/tests/agents/` to pair
  with the eight already shipped. `suspend_self_park_open.harn` pins
  the open-park contract: `agent_await_resumption(reason)` with no
  `conditions` reports `initiator: "self_initiated"`,
  `suspension.conditions == nil`, and `auto_resume_trigger == nil`,
  and only an explicit `resume_agent(handle)` wakes the worker.
  `suspend_self_park_with_input.harn` asserts the resume-continuity
  reminder embeds the verbatim resume input string and the suspend
  reason, that the reminder is present on the resumed turn's system
  prompt, and that it is consumed within one turn (the turn after the
  resumed one no longer sees it). `suspend_conditioned_trigger.harn`
  pins the `conditions.trigger` auto-resume contract: the suspension
  carries a non-nil `auto_resume_trigger` handle, `trigger_fire(...)`
  reports `dispatched`, and the loop drives itself to `completed`
  without any explicit `resume_agent` call from the pipeline.
  `suspend_cold_restart.harn` pins the cross-process `harn run --resume
  <snapshot>` contract: a top-level `agent_loop(...)` self-park writes
  a non-empty snapshot file, the process exits with
  `status=suspended`, and a second `harn run --resume <snapshot>
  --json` invocation emits a `done` event with
  `value.status=completed` and `value.has_transcript=true`. The other
  eight fixtures from the S-11 ticket
  (`suspend_midloop_basic`, `suspend_nested`,
  `suspend_continue_transcript_false`, `suspend_close_while_suspended`,
  `suspend_double_resume_race`, `suspend_timeout_resume_with_summary`,
  `suspend_timeout_fail`, `suspend_daemon_step_parity`) were already in
  place; the new four bring the suite to the full 12-fixture contract
  table in the issue. All fixtures are deterministic — they use
  `llm_mock` / `mock_time` / `trigger_fire`, no wall-clock sleeps —
  and ride the existing `cargo run --bin harn -- test conformance
  --filter suspend_` runner.

- **Effect inheritance enforcement (E5.4, #1777).** Adds the dispatcher-side
  ⊆ check for typed handoff effects against the parent's declared effect
  set. New diagnostic code `HARN-CAP-301`
  (`Code::EffectInheritanceViolation`) surfaces from `harn check`'s
  preflight pass when a `spawn_agent({...})` call inside a parent
  function/pipeline body grants effects the parent does not declare;
  parent and child sets are derived from the same `compute_handoff_effects`
  analyzer the runtime guard uses. New runtime guard
  `enforce_spawn_handoff_effects(handoff, parent_effects)` in
  `crates/harn-vm/src/orchestration/handoffs.rs` returns a typed
  `EffectInheritanceViolation` payload (with stable `_type`,
  `handoff_id`, `source_persona`, `target_label`, `violations`,
  `diagnostic_code`, `repair_id`, `repair_safety`, `message` fields) that
  the dispatcher emits as an `EffectInheritanceViolation` deny event;
  `report_effect_inheritance_violation(...)` emits the matching
  structured log under the `policy.effect_inheritance` category. New
  shared library helpers in `crates/harn-vm/src/orchestration/policy/effects.rs`:
  `effect_subset_violations(parent, child)` (the core ⊆ check),
  `effect_kind_label(...)` and `effect_record_summary(...)` (used by
  both diagnostic messages and deny payloads). Both static and runtime
  paths suggest the same repair (`policy/narrow-child-effects`,
  `safety: surface-changing`). Coverage: 16 new policy/effect unit
  tests, 4 new preflight integration tests in
  `crates/harn-cli/src/commands/check/tests.rs`, and two conformance
  fixtures (`conformance/tests/agents/effects_inheritance_subset_allowed.harn`,
  `…/effects_inheritance_empty_child.harn`) exercising the subset-allowed
  and empty-child paths.
- **Suspend/resume docs (S-13, #1849).** Adds the user-facing
  reference for the agent suspend/resume primitive (epic #1836). New
  mdBook page `docs/src/agent-lifecycle.md` covers the lifecycle
  builtins (`suspend_agent`, `resume_agent`,
  `parse_resume_conditions`, `agent_await_resumption`,
  `agent_lifecycle_tools`), the `ResumeConditions` shape (trigger /
  timeout / on_event), self-park mid-loop, parent-driven
  pause/resume via `subagent_pause` / `subagent_resume`, conditioned
  resume with trigger + timeout, the four `ResumeBy.*` callbacks and
  the `default_resume_by(...)` policy, transcript continuity (the
  single-shot `resume_continuity` system reminder vs
  `continue_transcript: false`), top-level loop cold-restore via
  `harn run --resume <snapshot_path>`, daemon idle as a degenerate
  case, the `HARN-SUS-*` diagnostic codes, and the cooperative-pause
  gotchas. Replaces the terse "Agent lifecycle tools" subsection in
  `docs/llm/harn-quickref.md` with an expanded "Agent lifecycle:
  pause, resume, self-park" section that lists the three model-facing
  tools, ships three executable snippets (self-park, parent-driven,
  conditioned), summarizes resume responsibility and `ResumeBy.*`,
  documents transcript continuity, and surfaces the gotchas.
  Adds an "Agent lifecycle (suspend/resume)" section to
  `spec/HARN_SPEC.md` with the formal state machine
  (`running -> suspended -> running / closed -> done`), the
  `Suspension` / `ResumeConditions` /
  `AgentAwaitResumptionRequest` shapes, the four canonical
  `ResumeBy.*` presets with `default_resume_by(...)` decision tree,
  the `PreSuspend` / `PostSuspend` / `PreResume` / `PostResume` hook
  contract, the top-level cold-restore contract, the daemon-idle
  reduction, and the cooperative cancellation contract. Wires the
  new mdBook page into `docs/src/SUMMARY.md` under Agent runtime
  next to Agent state.
- **Pipeline lifecycle docs (P-10, #1863).** Documents the callback-first
  pipeline lifecycle (epic #1853). New mdBook page
  `docs/src/pipeline-lifecycle.md` covers the lifecycle event order
  (`PreFinish` → `on_finish` → `OnUnsettledDetected` → `PostFinish`),
  the harness read/write surface (`unsettled_state` plus the 11 drain
  methods), the four `OnFinish` presets (`on_finish_abandon`,
  `on_finish_drain`, `on_finish_block_until_settled`,
  `on_finish_handoff_to`), the six composable combinators (`compose`,
  `first_available`, `with_telemetry`, `with_timeout`, `if_unsettled`,
  `when`), the three `OnBudget` strategies, the full hook-event tables
  for the finish / suspend-resume / drain gates, and a Design rationale
  section that explains the callback-first vs enum-dispatch decision
  with cross-refs to Temporal `wait_condition` and Restate sagas.
  New `docs/src/cookbooks/lifecycle.md` ships five end-to-end recipes:
  nightly settlement handoff with `on_finish_handoff_to`, custom audit
  per drain decision via an `on_drain_decision` session hook +
  `with_telemetry(on_finish_drain)`, long-paused agent with a
  `post_resume` operator telemetry hook, business-hours `pre_suspend`
  deny chain with a paired 1-turn reminder, and a replay-deterministic
  multi-suspend test harness using `mock_time` /
  `flush_trigger_aggregations` / `pipeline_lifecycle_audit_log_take`.
  Adds a "Pipeline lifecycle: drain, on_finish, composable handlers"
  section to `docs/llm/harn-quickref.md` with preset / combinator /
  budget / hook-event tables, three common-pattern snippets (nightly
  handoff, custom audit, clean abort), and cross-refs to the
  suspend/resume primitive (harn#1836). Adds a Pipeline lifecycle
  subsection to `spec/HARN_SPEC.md` with formal definitions of the
  `Pipeline.on_finish` semantic, the `Harness` type, the `DrainAgent`
  constrained tool surface + `HARN-DRN-001` ordering enforcement, the
  40-event lifecycle event taxonomy, and the four replay-determinism
  rules (cached resume input, memoized drain decisions, signed
  timestamps, one-shot registration). Adds a "Pipeline lifecycle" +
  "Lifecycle cookbook" entry pair to `docs/src/SUMMARY.md` and
  re-syncs `docs/src/language-spec.md` via `make sync-language-spec`.
  Closes the docs gap for the lifecycle epic; user-facing GA is now
  unblocked.
- **Tool hooks docs + harn-canon contribution guide (TH-08, #1901).**
  Adds the user-facing reference, cookbook, and contributing guide for
  the `preset_run_command` wrapper shipped under epic #1884. New
  mdBook page `docs/src/tool-hooks.md` covers the full
  `preset_run_command(config)` surface: the `stacks` / `registry` /
  `custom_rules` / `mode` / `inner` / `llm_classifier` keys, the
  decision-envelope shape, the three shipped modes
  (`tool_hooks_mode_rewrite_with_audit` / `_deny_with_explanation` /
  `_passthrough_only_audit`), custom-mode authoring with
  `tool_hooks_emit_audit` and `tool_hooks_inject_reminder`, the
  `ToolRule` / `Catalogue` / `Registry` schemas with field-level
  tables, the seven shipped catalogues (`harn-canon/universal`,
  `/rust`, `/python`, `/typescript`, `/swift`, `/sql`, `/harn`) with
  their rule rosters, the LLM classifier opt-in (verdict shape,
  threshold dispatch, cache TTL, transport-error degradation, trust
  contract), the lifecycle audit kinds (`tool_rewrite`, `tool_denied`,
  `tool_rule_warning`, `tool_hook_classifier_verdict`,
  `tool_hooks.reminder_injected`), and composition with the broader
  `register_tool_hook` PreToolUse / PostToolUse surface. New
  `docs/src/cookbooks/tool-hooks.md` ships ten recipes: per-stack
  agents (rust, python, typescript, swift, sql, harn), a polyglot
  agent with classifier fallback, audit-only rollout, no-`inner`
  preview, and composition with `register_tool_hook`. New
  `docs/src/contributing/preset-hooks.md` documents the harness-local
  → per-package → harn-canon promotion ladder, the harn-canon bar
  (stack-canonical / universally beneficial / cheap / stable /
  recoverable), the rule-add / new-stack workflows, the local gate
  checklist (`make conformance`, `make lint-md`,
  `make check-docs-snippets`, `mdbook build docs`), the catalogue
  authoring gotchas (no regex lookaround, leading-command anchor,
  prefix-extension overlap, `env_or` escape hatches, idempotent
  rewrites, no `now()`/`shell()` in predicates), and a reviewer
  checklist. Expands the "Catalogue-driven `run_command` hooks"
  section of `docs/llm/harn-quickref.md` with the catalogue auto-seed
  contract, the full classifier opt-in, and cross-references to the
  new pages; replaces the stale TH-05-is-reserved sentence with the
  shipped classifier docs. Adds a "Preset `run_command` tool hooks"
  subsection to `spec/HARN_SPEC.md` (formal `ToolRule` / `Catalogue`
  field tables, `preset_run_command` config keys, the three shipped
  modes' side effects, the classifier verdict shape, and the uniform
  decision-envelope schema). Wires the new pages into
  `docs/src/SUMMARY.md` under "Hooks (tool, persona, session
  lifecycle)" so the reference, cookbook, and contributing guide
  appear together.
- **Pool docs + cookbook (PL-08, #1893).** Adds the user-facing
  reference + cookbook for agent pools (epic #1883). New mdBook page
  `docs/src/agent-pools.md` covers pool creation (`pool_create`
  options, the handle shape), submit semantics (priority, fairness
  key, idempotency, task-handle shape), the four queue strategies
  (`fifo`, `priority`, `lifo`, `fair_round_robin`), backpressure
  policies (`backpressure_queue` with `block_submitter` /
  `drop_oldest` / `drop_newest` / `fail_submitter`, `fail_fast`,
  `ring_buffer`), the four durability scopes (`session`, `pipeline`,
  `tenant`, `org`) with the pipeline reload + `stale_after_ms`
  contract, the `SpawnToPool` trigger handler, inspection surface
  (`pool.size`, `pool.snapshot`, `pool_get`, `pool_list`,
  `harness.unsettled_state().pool_pending_tasks`), the
  `lifecycle.pool.audit` topic + OTel spans, the `HARN-POL-*`
  diagnostic codes, a pick-the-right-primitive table
  (`parallel each` vs pool vs scope tiers), and a SOTA comparison
  table (Temporal, Inngest, Restate) explaining the design rationale.
  New `docs/src/cookbooks/pools.md` ships four end-to-end recipes:
  rate-limited webhook processor with per-source fair queue +
  ring-buffer overflow, GPU-routed inference pool sized to the GPU
  count with `scope: "tenant"` for harn-cloud worker-tier routing,
  cross-customer fairness with 20-wide pool +
  `fair_round_robin("tenant_id")`, and a burst absorber for nightly
  batch jobs with a 50000-deep queue + idempotency keyed by
  `(date, customer)`. Expands the "Agent pools" section in
  `docs/llm/harn-quickref.md` with the full submit-option set,
  queue-strategy + backpressure tables, scope tiers, the
  `SpawnToPool` snippet, and cross-references. Adds an "Agent pools"
  subsection to `spec/HARN_SPEC.md` (with formal `PoolHandle`,
  `PoolTaskHandle`, `QueueStrategy`, `Backpressure`, `PoolScope`, and
  `SpawnToPoolHandler` shapes) plus the `HARN-POL-*` block in the
  diagnostic appendix. Wires the new pages into `docs/src/SUMMARY.md`
  under Orchestration next to Agent channels.
- **`harness.system.*` host introspection (E4.8, #1912).** Adds a seventh
  capability slice under the `Harness` handle that lets scripts read
  cross-platform host metadata without ambient OS access. New methods on
  `harness.system`: `cpu()` returns `{count, physical_count, model,
  frequency_mhz, usage_pct}`; `memory()` returns
  `{total_bytes, used_bytes, available_bytes, total_gb, used_gb,
  available_gb, pressure}` with `pressure` bucketed to
  `low|medium|high|unknown`; `gpus()` returns a (currently empty) list
  so scripts can write `if !gpus.is_empty()` portably while richer
  NVML / Metal / OpenCL detection lands as a follow-up; `temperature()`
  returns `{components: [{label, celsius, max_celsius,
  critical_celsius}, ...]}` with an empty list on hosts whose `sysinfo`
  backend doesn't expose thermal sensors (notably Apple Silicon and
  most containers); `platform()` returns `{os, arch, version, kernel,
  long_os_version, hostname}`; `processes()` returns a list of
  `{pid, name, ...}` entries with the current Harn process always
  included and tagged `is_self: true` / `is_harn_owned: true`, plus
  `parent_pid`, `cpu_pct`, `mem_bytes` on harn-owned entries. Peer
  processes appear as bare `{pid, name, is_harn_owned: false}` entries
  — we deliberately do **not** leak `command_line`, `environ`, or
  `cwd` for other processes to avoid exfiltrating credentials and
  prompts from peer agents. New `harness_system::register_harn_owned_pid`
  / `unregister_harn_owned_pid` helpers let subprocess spawners tag
  detached children so they keep their harn-owned status even after
  the parent->child link is broken. The surface is gated by the harness
  handle just like the other sub-handles: `Harness::null()` denies
  every call and records a `DenyEvent`; `Harness::mock()` returns
  deterministic synthetic snapshots so conformance fixtures can
  exercise the shape without observing the real host. Wires
  `HarnessKind::System` into the typechecker (`harness.system` maps to
  `HarnessSystem`), the effect-policy analyzer (system reads produce
  no `EffectRecord` since they're pure host reads gated by the
  capability handle), the property-access error message, and the
  null-deny test fixture. New backing crate dependency: `sysinfo`
  (default-features-off). Conformance: `harness/system_basic`,
  `harness/system_gpus_temperature`, `harness/system_processes`, and
  `harness/null_system_denies`.
- **Suspend/resume protocol contribution RFCs (S-12, #1848).** Authors
  two new upstream-proposal documents under
  `docs/src/protocol-contributions/`: ACP `session/suspend` +
  `session/await_resumption` (the suspend-side companion to the
  already-shipped `session/resume`, [ACP #1726][acp-1726]) and A2A
  explicit `TaskState.PAUSED_BY_CLIENT` / `PAUSED_BY_AGENT` with
  matching `tasks/pause` / `tasks/await_resumption` / `tasks/resume`
  JSON-RPC methods. Each RFC describes the proposed wire format,
  migration path from our existing `_meta.harn.suspend` /
  `metadata.harn.pause` envelopes, reference-impl status against the
  shipped `__host_worker_suspend` + `WorkerSuspension` primitive,
  and open questions for upstream maintainers. Both RFCs deliberately
  share field names (`handle`, `reason`, `conditions`, `cause`,
  `continueTranscript`) so cross-protocol bridges can round-trip
  pauses verbatim. Wires the two new RFCs into
  `docs/src/SUMMARY.md` and the
  `docs/src/protocol-contributions/README.md` index (now grouped by
  RFC family: reminder injection under #1829, suspend/resume under
  #1848). Upstream filings remain a maintainer action — see #1848 for
  the outstanding work.

[acp-1726]: https://github.com/agentclientprotocol/agent-client-protocol/discussions/1726

- **Channels docs + cookbook (CH-09, #1880).** Adds the user-facing
  reference + cookbook for durable agent channels (epic #1870). New
  mdBook page `docs/src/agent-channels.md` covers the emit surface
  (scopes, idempotency, signed timestamps), the `channel.emit`
  trigger source, the `batch { count, window, key, expire_action }`
  aggregation primitive, the `ReminderInject` handler variant, the
  guardrails middleware layer (CH-11), three observability topics,
  the `HARN-CHN-*` diagnostic codes, and a SOTA comparison table
  (Inngest, Temporal, Restate, A2A) explaining the design rationale.
  New `docs/src/cookbooks/channels.md` ships five end-to-end recipes:
  release handshake (PR agent → batched release agent →
  merge-captain subscribers), periodic check-in via batched tool-call
  counting + `ReminderInject`, multi-agent feedback loop (planner +
  reviewers), tenant-scoped pipeline progress dashboard, and
  cross-pipeline coordination via drain handoff. Extends the "Durable
  agent channels" section in `docs/llm/harn-quickref.md` with the
  trigger + batch + ReminderInject surface, a four-row pick-the-right-
  primitive comparison table (handoffs vs triggers vs suspend/resume
  vs channels vs channels+batch+reminder), the `HARN-CHN-*` and
  `HARN-REP-CHN-*` code lists, and a guardrails one-liner. Adds a
  "Durable agent channels" subsection to `spec/HARN_SPEC.md` (with
  formal `ChannelScope`, `BatchFilter`, `ReminderInjectHandler`
  shapes) plus the `HARN-CHN-*` block in the diagnostic appendix.
  Wires the new pages into `docs/src/SUMMARY.md` under Orchestration.
- **`needs-human` autonomy class (T8, #1792).** Formalizes `needs-human` as
  a first-class, transverse autonomy discipline on top of the existing
  `AutonomyTier` (`Shadow` / `Suggest` / `ActWithApproval` / `ActAuto`).
  Any operation tagged `needs-human` is now denied with the structured
  `HARN-AUT-NEEDS-HUMAN` deny code regardless of the resolved tier —
  even an `ActAuto`-tier caller cannot auto-apply a `needs-human`
  side effect. `AutonomyPolicy` grows three additive fields for tagging:
  `requires_human: bool` (blanket), `requires_human_agents: BTreeSet<String>`
  (per-agent), and `requires_human_actions: BTreeSet<String>` (per-builtin
  or per-action-class, e.g. `"write_file"` or `"fs.write"`). The dispatcher
  emits a non-blocking approval-request with `detail.autonomy_class =
  "needs-human"`, `detail.requires_human = true`, and `detail.deny_code =
  "HARN-AUT-NEEDS-HUMAN"` so approval surfaces (Slack-approval, IDE,
  portal, `hitl_pending`) can render the row distinctly from a normal
  tier-driven approval ask. The corresponding `TrustRecord` is appended
  with `outcome = "denied"`, `metadata.autonomy_class = "needs-human"`,
  `metadata.requires_human = true`, `metadata.deny_code =
  "HARN-AUT-NEEDS-HUMAN"`, and `metadata["autonomy.enforcement"] =
  "needs_human_denied"`. The autonomy-class string mirrors
  `RepairSafety::NeedsHuman.as_str()` from `harn-parser` so the
  autonomy-surface and the repair-safety surface stay in lockstep with
  E1.2 (#1747). Conformance: `conformance/tests/autonomy/autonomy_needs_human_basic.harn`
  (per-action tag denies under `act_auto`, approval-request and trust
  record carry the tag) and `autonomy_needs_human_blanket.harn` (blanket
  and per-agent shapes).
- **OAuth docs + provider cookbook (OA-08, #1909).** Adds
  `docs/src/oauth.md` as the user-facing reference for the full OAuth
  stack (`std/oauth/{providers,storage,client,device_flow,dynamic_registration,redaction}`),
  with per-provider cookbook recipes for GitHub, Slack, Linear, Notion,
  Google, Microsoft, Atlassian, Discord, GitLab (cloud + self-hosted),
  and Bitbucket, plus cross-cutting recipes for headless CI agents
  (device flow), org-shared bots (`harn_cloud_org` storage), and
  custom enterprise OIDC providers. Each recipe includes a
  provider-specific gotcha (Slack single-use refresh, Linear
  comma-separated scopes + GraphQL userinfo, Notion page-picker scoping,
  Google offline_access + prompt=consent, Microsoft Graph delegated
  scopes vs OIDC claims, Atlassian rotating refresh + cloudid lookup,
  Discord bot-vs-user token split, GitLab dual-rotation, Bitbucket
  workspace + client-credentials guidance). Adds an `Authentication`
  pointer + dedicated device-flow / dynamic-registration / redaction
  subsections to `docs/llm/harn-quickref.md`, an OAuth subsection
  (including the `HARN-OAU-*` diagnostic code appendix) to
  `spec/HARN_SPEC.md`, and wires the new page into `docs/src/SUMMARY.md`.
- **Channel guardrails middleware (CH-11, #1911).** Inter-agent
  `emit_channel(...)` calls now run through a pluggable middleware
  layer before the durable journal append. Each guardrail returns one
  of three verdicts — `allow` (silent passthrough), `warn` (emit
  proceeds and a `channel_guardrail_warning` lifecycle audit is
  recorded), or `block` (emit is dropped, the caller receives a
  synthetic `{blocked: true, block_reason, guardrail_fired}` receipt,
  and a `channel_guardrail_blocked` audit is persisted to the
  `lifecycle.channel.audit` topic AND the in-process lifecycle audit
  log so the block itself is durable + observable). Worst verdict
  across every registered guardrail wins; a `block` short-circuits
  remaining guardrails so they cannot suggest the emit went through.
  New Rust builtins: `channel_guardrail_register(config_dict) ->
  id_string`, `channel_guardrail_unregister(id) -> bool`,
  `channel_guardrail_list() -> [string]`, `channel_guardrail_clear()`.
  The registry is thread-local so peer pipelines on other threads
  cannot poison each other; `reset_channel_state()` clears the
  registry alongside the per-thread channel log. Two scanner kinds
  ship in v1: `prompt_injection_signature` (heuristic regex sweep
  over every string in the payload tree — recursively walks dicts /
  lists so nested adversarial text cannot hide; eleven seed patterns
  cover instruction-override, system-prompt spoof, role redefinition,
  data-exfil request, and jailbreak-banner families; the audit
  records a coarse family label rather than the matched regex so the
  attack fingerprint does not leak back to the producer) and
  `custom` (user-supplied Harn closure returning `nil` /
  `"allow"|"warn"|"block"` / `{verdict, reason?}`; evaluated via the
  async-builtin child VM, so closure-bound side effects work the
  same as in `tool_hooks_match`). A new `std/channel_guardrails`
  module exposes ergonomic presets: `prompt_injection_scanner(opts)`,
  `llm_risk_classifier(opts)` (mirrors the TH-05 #2017 pattern with a
  JSON-verdict meta-prompt, sub-threshold-degrades-to-warn safety
  net, and transport-error-degrades-to-allow so a flaky model cannot
  DoS the channel), and `register_guardrail(opts)` for raw custom
  closures. Conformance: four new fixtures under
  `conformance/tests/triggers/trigger_channel_guardrail_*` cover the
  signature scanner blocking adversarial payloads while letting safe
  ones through, custom-closure verdict honoring across all three
  shapes, LLM classifier mock with confident-block + sub-threshold +
  explicit-allow, and the allow-passthrough silence contract +
  register/list/unregister surface. Rust-side coverage adds eight
  unit tests exercising nested-string walking, registration replace-
  on-duplicate, applies_to filtering, and short-circuit-on-block.
- **Bounded sandboxed const-evaluator (T7, #1791).** New `const NAME [: Type] = EXPR`
  binding form runs its initializer through a strict compile-time
  evaluator in `crates/harn-parser/src/const_eval/`. Folding is
  allowlist-based: literal arithmetic, string concatenation, literal
  lists/dicts, ternary / if-else, subscript access, and a small set of
  pure stdlib builtins (`len`, `format`, `min`, `max`, `abs`, `floor`,
  `ceil`, `round`, `lowercase`, `uppercase`, `trim`, `concat`, `join`)
  are accepted. Step (`MAX_STEPS = 100_000`) and recursion
  (`MAX_DEPTH = 256`) caps are checked on every reduction, never
  amortized. Sandbox violations (`harness.*`, fs / net / env / process /
  spawn / parallel / select / try / yield / emit, user functions,
  mutation, loops) are rejected with new diagnostic codes:
  `HARN-MET-001` (disallowed expression shape), `HARN-CST-001` (step
  budget), `HARN-CST-002` (recursion depth), `HARN-CST-003` (sandbox
  violation), `HARN-CST-004` (value-level runtime error during folding).
  At runtime the binding lowers to a `let`-equivalent — the same
  expression is re-evaluated by the VM and byte-equality with the
  compile-time fold is guaranteed by construction (pure subset).
  Pre-positions compile-time prompt template specialization, schema
  derivation from types, and `harn graph --json` static facts; no
  immediate consumer in this release.
- **DAP subagent threads + suspend/resume integration (S-17, #1868).**
  `harn-dap` now bridges spawned subagents onto DAP's `Thread`/`stopped`/
  `continued` event model. Each `agent_loop` / `sub_agent_run` worker
  becomes its own DAP thread (ids start at 100 to keep them visually
  distinct from `main`=1 and ACP-session threads in the 2.. range);
  `parent_worker_id` propagates onto a `parentId` extension so IDEs can
  render lineage trees; `last_status` / `suspend_reason` ride alongside
  on the `threads` response. `WorkerSuspended` raises a per-thread
  `stopped { reason: "suspend", description: <suspension reason>,
  allThreadsStopped: false }` so the suspended subagent shows as paused
  without freezing the user's debug session, and `WorkerResumed` emits
  a matching `continued`. `WorkerWaitingForInput` maps to
  `stopped(reason: "pause")`, `WorkerCompleted`/`WorkerCancelled` emit
  `thread { reason: "exited" }`, `WorkerFailed` emits both a
  `stopped(exception)` and an exit, and `WorkerProgressed` is silent so
  the IDE doesn't flood with no-op churn. The `initialize`
  capabilities advertise three new `exceptionBreakpointFilters`:
  `break-on-suspend`, `break-on-resume` (forces a main-thread pause so
  the user can step through the resume continuation), and
  `break-on-drain-decision` (registered for issue spec parity; drain
  decisions today flow through the lifecycle-hook path rather than
  `AgentEvent`, so the filter currently surfaces ambient events
  without forcing a stop — documented as a known limitation).
  Privacy: only worker id, name, mode, status, parent id, and the
  short human-readable suspend reason flow through DAP — transcripts,
  conditions, and arbitrary metadata blobs are deliberately filtered
  per issue #1867. New module `crates/harn-dap/src/debugger/subagents.rs`
  owns the `SubagentTracker` (queue + `worker_id` → DAP-thread map)
  and the `SubagentEventSink` that captures `AgentEvent::WorkerUpdate`
  observations between VM steps; the registration drops with the
  debugger so a session never leaks a wildcard sink. Powers the
  burin-code#940 (B-06) consumer side. Coverage: nine new unit tests
  exercising filter registration, drain → DAP event mapping, suspend
  description propagation, `break-on-resume` pause arming, parent
  lineage in `handle_threads`, `WorkerFailed` two-event sequence,
  `WorkerProgressed` silence, and direct sink-to-tracker handoff.
- **Cross-session wildcard `AgentEventSink` (#1868 support).**
  New `harn_vm::agent_events::register_wildcard_sink` / `unregister_wildcard_sink`
  / `WildcardSinkHandle` API lets a process-global observer subscribe
  to every emitted `AgentEvent` regardless of `session_id`. Wildcards
  fan out *after* the session-scoped registry so per-session ordering
  stays intact for existing consumers. Test-only thread-owner filtering
  mirrors the per-session sink registry so parallel tests don't
  cross-pollute each other. The DAP debugger is the first consumer
  (issue #1868); other cross-session observers (the upcoming portal
  live view, audit ledger sinks) can drop in without further plumbing.
- **Settlement-agent drain loop (P-03, #1856).** Lands the real
  implementation behind `harness.spawn_settlement_agent` that
  `on_finish_drain` (P-02) and the `PreDrain` / `PostDrain` / `OnDrainDecision`
  hook seam (P-06) were already wired against. The loop is a bounded,
  deterministic walk over the unsettled-state snapshot in the documented
  order — suspended subagents → queued triggers → partial handoffs →
  in-flight LLM calls → pool pending — applying a default disposition
  per item (cancel suspended subagents, acknowledge stale triggers,
  acknowledge partial handoffs as `deferred`, drain in-flight LLM calls,
  defer pool tasks). Each disposition records a `drain_decision`
  lifecycle audit and fires the `OnDrainDecision` hook chain (Allow /
  Block / Modify), so VM-side hooks observe the disposition before it
  persists. The loop terminates when the snapshot drains or when the
  per-call budget (default 5, configurable via the third arg to
  `spawn_settlement_agent`, hard-capped at 20) is exhausted; on
  exhaustion a `drain_unsettled_remaining` audit captures the remainder.
  HARN-DRN-001 ordering enforcement now ships alongside the loop:
  `harness.acknowledge_trigger` and `harness.acknowledge_handoff` reject
  out-of-order calls with the documented `HARN-DRN-001` reason so a
  caller (or a future LLM-driven settlement variant) cannot finalize a
  later category while earlier work is still pending. A new
  `__host_settlement_agent_active()` builtin exposes the
  constrained-surface flag so conformance fixtures and IDE hosts can
  observe when the loop is in scope. Five new conformance fixtures pin
  the contract: `pipeline_drain_settlement_audits` (per-item
  `drain_decision` audits + terminal `pipeline_finalized`),
  `pipeline_drain_settlement_ordering` (FIFO bucket processing),
  `pipeline_drain_settlement_max_iterations` (budget enforcement + the
  `drain_unsettled_remaining` audit), and
  `pipeline_drain_settlement_constrained_surface` (active flag scoped
  to the loop body). The previously-`@xfail`ed
  `pipeline_drain_ordering_enforcement` fixture now gates the real
  HARN-DRN-001 contract (xfail threshold drops from 1 to 0). Part of
  epic #1853 (declarative agent lifecycle).
- **Tool-hook conformance suite (TH-07, #1900).** Adds nine focused
  `conformance/tests/stdlib/preset_hooks_*` fixtures that gap-fill the
  TH-* surface against the spec's named scenarios:
  `preset_hooks_basic_rewrite`, `preset_hooks_deny_mode`,
  `preset_hooks_passthrough_audit`, `preset_hooks_no_match_passthrough`,
  `preset_hooks_catalogue_composition`,
  `preset_hooks_custom_rule_priority`,
  `preset_hooks_per_stack_filtering`,
  `preset_hooks_llm_classifier_confidence`, and
  `preset_hooks_universal_rules_always_apply`. Each fixture isolates a
  single contract (mode dispatch, rule-priority resolution across
  catalogues, custom-rule precedence, stack filtering, LLM-classifier
  threshold gating, universal-rule stack independence) so a regression
  in any one surface points at the right TH-0X fix rather than a
  broad-coverage test. The pre-existing
  `tool_hooks_catalogue_{rust,python,typescript,swift,sql,harn,universal}`
  fixtures already satisfy the spec's "two real-command fixtures per
  catalogue" rider with multi-probe coverage of every shipped rule.
  Combined suite runtime stays well under the 40-second budget
  (~4.5s for the 9 new fixtures; ~25 fixtures total in the
  tool/preset hook namespace). Part of epic #1884 (Preset tool hooks
  library).
- **Channel conformance suite (CH-08, #1879).** Closes the channels
  primitive's executable-spec gap with five new paired fixtures that
  fill out the CH-01..CH-07 coverage map: `channel_emit_idempotent`
  (duplicate `emit_channel` with the same explicit `id` is a no-op for
  trigger fan-out — first emit fires the handler exactly once, second
  returns `duplicate=true`), `channel_scope_pipeline_siblings`
  (`scope: "pipeline"` with an explicit `pipeline_id` routes through a
  shared topic so sibling readers see each other's emits; distinct
  pipeline ids stay isolated), `trigger_channel_reminder_inject_current`
  (`ReminderInject` with `target: "current"` gracefully drops with a
  `target_missing` audit when no agent session is on the current-session
  stack — the documented failure-mode contract), `trigger_channel_reminder_inject_batched`
  (a batched channel trigger paired with `ReminderInject` lands exactly
  one `SystemReminder` per batch dispatch, not per constituent event,
  and the batch counter resets cleanly after a fire), and
  `trigger_channel_replay_batch_determinism` (constituent
  `payload_hash` values match byte-for-byte across "first" and
  "replay" walls of identical batched emits, while distinct payloads
  inside a single batch keep distinct hashes so the replay oracle can
  pinpoint which constituent drifted). All fixtures use the deterministic
  `mock_time(...)` / `advance_time(...)` / `flush_trigger_aggregations()`
  pattern — no wall-clock sleeps. Brings the `cargo run --bin harn --
  test conformance --filter channel` count from 23 to 28.
- **Suspend/resume conformance gap-fill (S-11, #1847).** Adds seven
  paired `.harn` / `.expected` fixtures under
  `conformance/tests/agents/` covering the spec table from #1847 that
  was not already exercised by existing fixtures:
  `suspend_midloop_basic` (parent-driven `subagent_pause` /
  `subagent_resume` against a self-parked child),
  `suspend_nested` (two simultaneously-suspended workers resumed
  independently), `suspend_timeout_resume_with_summary` and
  `suspend_timeout_fail` (auto-resume timeout firing under
  `mock_time` + `advance_time`, exercising both `on_timeout` actions),
  `suspend_continue_transcript_false` (agent-loop level transcript
  reset on resume), `suspend_close_while_suspended` (close + resume
  rejects with HARN-SUS-010), `suspend_double_resume_race`
  (serialised double-resume rejects with HARN-SUS-003 — the pure-Harn
  observable surface of the HARN-SUS-006 race that Rust unit tests
  cover concurrently), and `suspend_daemon_step_parity` (post-S-07
  daemon snapshot field-set). All fixtures are deterministic — no
  wall-clock sleeps; the timeout path uses the unified mock clock
  through `clock::sleep`.
- **`InterruptAndSuspend` trigger handler variant (#1910).** Adds a new
  `TriggerHandlerSpec::InterruptAndSuspend` variant + the matching
  `InterruptAndSuspend({...})` constructor in `std/triggers`, completing the
  CH-10 leg of epic #1870 (Agent channels & periodic prompts). Unlike
  `Local`/`Worker`/`SpawnToPool` (which spawn new work) and
  `ReminderInject` (which delivers at the next turn boundary), this is the
  org-scoped "panic" broadcast: when the trigger matches, every running
  worker in the resolved `target_agents` scope is cooperatively suspended
  synchronously via the same `suspend_signal` + `WorkerSuspension` pipeline
  used by `suspend_agent` (#1837), bypassing the normal turn-boundary
  approval gate. `target_agents` accepts the string `"all"` (every worker
  in the local registry — the default), a list of concrete worker-id
  strings, or a closure `event -> list<string>` for dynamic resolution.
  `reason` is propagated to every suspended worker's `WorkerSuspension`
  envelope, the `WorkerSuspended` lifecycle event, and the per-suspension
  `triggers.interrupt_and_suspend.audit` audit entry. Already-suspended
  and terminal workers are skipped as `already_suspended` / `not_running`
  (no double-suspend, no overwritten reason), and stale ids returned by a
  closure are skipped as `unknown` rather than failing the broadcast.
  Empty target lists record a single roll-up audit and return a
  successful `status: "broadcast"` with `suspended_count: 0`. The
  per-worker iteration walks the registry in `BTreeMap` (sorted) order so
  the panic broadcast is deterministic across replay. Conformance:
  `conformance/tests/triggers/trigger_interrupt_and_suspend_{empty_scope_records_audit,all_scope_with_empty_registry,closure_scope}.harn`;
  Rust unit tests in `crates/harn-vm/src/stdlib/agents.rs` cover the
  per-worker `Suspended` / `AlreadySuspended` / `NotRunning` / `Unknown`
  outcomes and the deterministic registry-enumeration contract.
- **Pool tasks wired into `harness.unsettled_state()` (#2007).** Closes
  the PL-07 follow-up from #2008: `UnsettledStateSnapshot` now carries a
  `pool_pending_tasks` bucket fed by a new
  `crate::stdlib::pool::snapshot_pending_tasks()` helper that walks the
  thread-local pool registry and emits one entry per queued or running
  task (terminal tasks are skipped). The bucket exposes `pool_id`,
  `pool_name`, `task_id`, `status`, `priority`, optional `key` /
  `idempotency_key`, `submitted_at` / `submitted_at_ms`,
  `submitted_by`, optional `started_at`, and an `age_ms` derived from
  the wall clock at snapshot time. `harness.is_empty(state?)`,
  `harness.counts(state?)`, and `harness.summary(state?)` learn the
  new `pool_pending` count; `std/lifecycle`'s `is_empty(state)`,
  `counts(state)`, and `summary(state)` helpers learn the same field
  so script-land callbacks observe pool work alongside suspended
  sub-agents, queued triggers, partial handoffs, and in-flight LLM
  calls. Drops the `@xfail` marker on
  `conformance/tests/pool_unsettled_state_integration.harn` (now
  asserting `pool_pending=3` end-to-end) and decrements
  `scripts/xfail_threshold.txt` from 2 to 1.
- **Tool-hook LLM classifier (TH-05, #1898).** Adds the opt-in
  `llm_classifier` config to `ToolHooks.preset_run_command(...)` so
  ad-hoc commands that don't match any deterministic rule can be
  reviewed by a small model. The classifier is given the command +
  configurable `meta_prompt` and must return a one-line JSON verdict
  `{kind: "rewrite"|"deny"|"allow", confidence, rewritten?,
  explanation?}`. Confident `rewrite` / `deny` verdicts dispatch
  through the existing `tool_hooks_mode_*` callbacks (so audit log and
  reminder hooks fire exactly as for catalogue-matched rules);
  sub-threshold and `allow` verdicts audit + pass through to `inner`.
  Every invocation — including transport errors and cache hits — emits
  a `tool_hook_classifier_verdict` lifecycle audit entry
  (`{scope, model, command, normalized_command, kind, confidence,
  threshold, cache_hit, action, error?}`) so operators can observe
  classifier decisions in `pipeline_lifecycle_audit_log_take()` and
  event-log replay. Verdicts are cached in a thread-local map keyed by
  `<scope>:<sha256(normalized command)>` with optional
  `cache.ttl_ms` / `cache.ttl_seconds` so repeat commands skip the
  extra model call entirely. Transport errors (e.g. provider 5xx) are
  handled via `llm_call_safe` and degrade silently to passthrough
  rather than throwing. Trust contract: the raw command text + the
  meta-prompt are sent to the configured model, so callers must redact
  secrets in the same way they already do for `run_command`. Budget:
  each classifier call counts against the session's standard LLM cost
  telemetry (`peek_total_cost`, autonomy budget) — keep the classifier
  model small (the spec defaults to `claude-haiku-4-5-20251001`). The
  TH-02 rejection check is replaced by a config validator that throws
  on empty `model` or out-of-range `threshold` at wrapper-build time.
  New conformance fixtures: `tool_hooks_llm_classifier_rewrite`,
  `tool_hooks_llm_classifier_deny`,
  `tool_hooks_llm_classifier_passthrough` (covers below-threshold,
  explicit `allow`, and cache replay), and
  `tool_hooks_llm_classifier_error_recovery`. Part of epic #1884
  (Preset tool hooks library).
- **Channel replay determinism receipts + diagnostics (#1878).** CH-07 of
  the channels epic (#1870) closes the audit/replay gap by giving every
  `emit_channel(...)` and every channel-source trigger match a durable
  receipt on the new `lifecycle.channel.audit` event-log topic. Adds
  two receipt types: `ChannelEmitReceipt` carries the resolved name,
  scope, scope id, the full emit payload, a canonical-JSON SHA-256
  `payload_hash`, the signed `emitted_at` timestamp (reusing the
  per-session HMAC salt from CH-01 / #1872), the active emit span id,
  and the pipeline/session/tenant correlation fields; and
  `ChannelMatchReceipt` carries the same `event_id` (so the cached
  match keys off the emit's id rather than re-evaluating the filter
  spec on replay), the recorded `trigger_id` + `binding_key` +
  `handler_kind`, a `handler_result` summary recorded after the
  dispatcher returns (`status` / `attempt_count` / `error` /
  `dispatch_failed`), the signed `matched_at` timestamp bound to the
  `(event_id, trigger_id)` pair, and a `batch.constituent_event_ids`
  list for aggregation/batched triggers (CH-04 / #1875) so replay
  reconstructs the batch from those ids. Each audit append rides the
  active event log (`active_event_log()`) so receipts inherit the
  pipeline's durability — in-memory in tests, durable in production.
  The match dispatch path also resolves `event_id` from the trigger
  event's `dedupe_key` (which carries the channel's emit id) rather
  than the freshly-minted `trigger_evt_*` UUID, so the receipt chain
  links emit -> match by stable id. In parallel, extends the
  replay-oracle (`crates/harn-vm/src/orchestration/replay_oracle.rs`)
  with a first-class `channel_receipts` section on `ReplayTraceRun`
  (and matching `ReplayTraceRunCounts` + `replay_bench.rs` runtime-cost
  metrics + section list) so multi-agent channel exchanges round-trip
  byte-identically across two runs of the same workload; and adds the
  typed `ChannelReplayDiagnostic` enum surfaced via
  `diagnose_channel_replay_drift(...)` with three codes:
  `HARN-REP-CHN-001` (replay matched an `event_id` with no recorded
  emit), `HARN-REP-CHN-002` (replay emit `payload_hash` drifted from
  the recorded hash — producer code drift), and `HARN-REP-CHN-003`
  (batched-match `constituent_event_ids` composition drifted between
  runs). Three new conformance fixtures under
  `conformance/tests/triggers/trigger_channel_replay_*` cover the
  emit-receipt shape + signed timestamp, the emit -> match
  `event_id` linkage with `handler_result.status="succeeded"`, and the
  payload-hash stability/drift contract (identical payloads produce
  identical hashes; flipped fields produce different hashes; dict key
  ordering is canonicalized so iteration order can't poison the
  oracle). Five new unit tests under `replay_oracle::tests` cover the
  diagnostic codes end-to-end. Unblocks CH-08 conformance and the
  end-to-end multi-agent replay / audit / compliance use cases the
  epic was driving toward.
- **Lifecycle replay determinism (P-08, #1861).** Closes the
  pipeline-lifecycle epic's #1853 replay-determinism leg with the
  journal entry types every SOTA replay engine (Temporal, Restate,
  Inngest, Azure Durable, Cadence) ships: `SuspensionReceipt`,
  `ResumptionReceipt`, and `DrainDecisionReceipt`, each carrying a
  `SignedLifecycleTimestamp` whose HMAC binds
  `(kind, at_ms, subject_id, initiator_id)` under a per-process salt.
  `ResumptionReceipt` records both the cached resume input and its
  canonical-JSON `input_hash` so replay can re-feed the same payload
  into the suspended worker instead of re-prompting; map-key order
  drift is absorbed by the canonical hash. `DrainDecisionReceipt`
  captures the settlement agent's chosen `action` and an optional
  `prompt_hash` so replay can short-circuit the LLM call. New Rust
  builtins `lifecycle_receipt_record_{suspension,resumption,drain_decision}`,
  `lifecycle_receipts_snapshot`, `verify_lifecycle_receipt_signature`,
  `lifecycle_resume_input_hash`, `lifecycle_drain_decision_prompt_hash`,
  `lifecycle_replay_resume_input`, and `lifecycle_replay_drain_decision`
  wire the receipt model into Harn pipelines. Drift surfaces three new
  diagnostic codes — `HARN-SUS-011` (resume input hash mismatch),
  `HARN-SUS-012` (drain decision prompt hash mismatch), and
  `HARN-SUS-013` (lifecycle signature verification failure) — and the
  replay oracle treats `lifecycle_receipts` as first-class trace
  material alongside event log entries and llm interactions. Three
  conformance fixtures (`lifecycle_replay_record_and_replay`,
  `lifecycle_replay_cached_resume_input`,
  `lifecycle_replay_signed_timestamps`) pin the determinism contract:
  byte-identical receipt snapshot across record/replay, cached resume
  input round-trip with canonical-hash drift detection, and signed
  timestamps that stay pinned to the original wall clock even after
  the live clock has advanced. Privacy: `ResumptionReceipt` supports
  an optional `RedactionPolicy` that scrubs sensitive paths from the
  persisted `input` while leaving the hash computed against the
  original — replay still works, secrets do not land in the journal.
- **OTel suspend-end / resume-link wiring (S-18, #1867).** Wires the
  `SpanKind::Suspension` and `SpanKind::Resume` spans (added in P-05,
  #1858) into the cooperative `suspend_agent` / `resume_agent` paths.
  The suspension span is now closed before the snapshot is persisted
  (Temporal community best-practice — never carry one root span across
  a multi-day pause) and the resume path opens a fresh detached
  `SpanKind::Resume` span that links — not parents — back to the prior
  suspension span and the pipeline span that was active at suspend
  time. Adds the canonical attribute bag to both spans: suspend carries
  `handle`, `reason`, `initiator`, `has_conditions`, `pipeline_id`
  (alias of `pipeline_span_id`), and `parent_worker_id`; resume carries
  `handle`, `initiator`, `continue_transcript`, `had_resume_input`, and
  `linked_suspension_count`. Privacy: the `resume_input` value itself
  is deliberately never serialised onto resume-span attributes — only a
  boolean flag — and the existing `WorkerSuspension` JSON round-trip
  already preserved `prior_span_link` + `pipeline_span_link` from
  P-05, so cross-process cold resume continues to link to the closed
  pre-restart span. New `suspend_otel_links` conformance fixture
  exercises the warm suspend → resume cycle from script land. Part of
  epic #1836 (Agent suspend/resume).
- **Pipeline lifecycle conformance gap-fill (#1862).** P-09 closes the
  pipeline-lifecycle epic (#1853) by landing the spec-named umbrella
  fixtures on top of the per-preset coverage that already shipped with
  P-01..P-07: `pipeline_lifecycle_default` (default `on_finish: drain`,
  no unsettled → `pipeline_finalized` audit + PreFinish/PostFinish fire
  around the callback), `pipeline_lifecycle_abandon` (`on_finish_abandon`
  surfaces a `pipeline_abandoned_unsettled` audit with the typed counts),
  `pipeline_drain_decision_callback` (settlement-agent `emit_audit(
  "drain_decision", ...)` records the documented shape AND the
  `OnDrainDecision` hook dispatches with `action`/`item` payload),
  `pipeline_composed_finish_handlers` (`compose([log, drain, audit])`
  threads through `pipeline_on_finish` in the declared order), and
  `pipeline_replay_equivalence` (lifecycle audits persisted to the
  `pipeline.lifecycle.audit` event-log topic round-trip byte-identical
  through `event_log.subscribe(from_cursor, kind_prefix)` with monotonic
  seq). `pipeline_drain_ordering_enforcement` lands as `@xfail`
  referencing #1856 (P-03 settlement-agent loop) — the fixture pins the
  post-wiring HARN-DRN-001 contract so dropping the marker after #1856
  lands is the only edit needed. Together with the existing
  `on_finish_preset_*`, `combinator_*`, `lifecycle_hook_events_*`,
  `lifecycle_event_shape_*`, and `on_budget_*` fixtures the lifecycle
  surface is now executable spec end-to-end.
- **OAuth dynamic client registration + auto-hosted metadata (#1906).**
  Closes OA-05 on epic #1885 with the worker-side dual of `std/oauth/client`:
  a new `std/oauth/dynamic_registration` module that builds RFC 7591
  client metadata, RFC 8414 authorization-server metadata, and runs an
  in-process RFC 7591 dynamic registration store. `client_metadata(opts)`
  validates + defaults a candidate document for serving at
  `/.well-known/oauth-client.json`; `authorization_server_metadata(provider,
  overrides?)` derives the server document from a `std/oauth/providers`
  record for `/.well-known/oauth-authorization-server.json`;
  `dynamic_registration_store()` + `register_client(store, metadata)` issue
  fresh `client_id` / `client_secret` pairs (256-bit URL-safe base64) with
  RFC 7591 §5.1 server-side latitude to reject non-HTTPS redirect URIs
  (except RFC 8252 §7.3 loopback), fragments, and non-spec
  `grant_types` / `response_types` / `token_endpoint_auth_method` values.
  All rejections thread through a stable `HARN-OAU-005:` diagnostic
  prefix. The `client_secret` is only returned by `register_client`;
  subsequent `get_client` reads omit it, and the `oauth.dynreg.audit`
  `client_registered` event carries only the redacted shape
  (`redirect_uri_count`, `grant_types`, `has_client_secret`) — never the
  credential material. `well_known_paths()` + `well_known_response()`
  return the canonical URL paths and an HTTP envelope that embedders
  (harn-cloud, future `harn serve --oauth-resource ...`) can mount
  directly. New Rust builtins (`__oauth_dynreg_store_handle`,
  `__oauth_dynreg_register`, `__oauth_dynreg_get`, `__oauth_dynreg_list`,
  `__oauth_dynreg_validate_metadata`, `__oauth_dynreg_build_client_metadata`,
  `__oauth_dynreg_build_authorization_server_metadata`) are registered
  in the parser signature table and live in
  `crates/harn-vm/src/stdlib/oauth_dynreg.rs`; `client_id_issued_at`
  routes through `stdlib::clock` for mock-clock-deterministic
  conformance. Three new fixtures under `conformance/tests/stdlib/`
  (`oauth_dynamic_registration_metadata`, `..._register`, `..._rejects`)
  cover well-known builder defaulting, happy-path registration with
  audit-shape redaction, and the RFC 7591 §5.1 / §2 rejection matrix.
  Intentional cut: the harn-serve port-wiring (a `/well-known` route
  group + `POST /register` handler) is deferred to a follow-up so this
  PR can ship the stdlib + validation surface that both harn-cloud and
  harn-serve will share without coupling the two. Embedders consume the
  artifacts produced here as opaque `{status, content_type, headers,
  body}` envelopes.
- **Channel emit/match transcript events + OTel spans (#1877).** CH-06 of
  the channels epic (#1870) closes the debuggability gap for channels by
  giving every emit and match first-class observability on the same
  primitives the rest of the runtime uses. Adds two new `SpanKind`
  variants — `ChannelEmit` (opened at `emit_channel(...)`, closed once
  the durable append + trigger fan-out finishes) and `ChannelMatch`
  (opened just before the channel-source trigger dispatcher invokes the
  handler, closed when dispatch returns) — surfaced through
  `trace_spans()` and the OTel exporter. `ChannelMatch` spans link back
  to the originating `ChannelEmit` span via `set_span_link` (P-05 /
  #1858) using the Harn `harn.channel.emit_trace_id` /
  `harn.channel.emit_span_id` headers stashed on the trigger event; for
  aggregation/batched triggers (CH-04 / #1875) the match span
  multi-links to every constituent emit span so the trace tree shows
  the full fan-in. In parallel, emits two new transcript event kinds —
  `transcript.channel.emit` (every append, whether fresh or
  idempotent-duplicate, with `payload_summary`, `inserted`, `duplicate`,
  `emitted_at`, `emitted_by`, scope/session/pipeline/tenant correlation,
  and the live `span_id`) and `transcript.channel.match` (every handler
  dispatch, with `trigger_id`, `handler_kind`, `matched_at_ms`,
  `matched_in_session_id`, the live `span_id`, and a `batch` summary
  carrying `count` + `constituent_event_ids` for aggregated dispatch) —
  on the new `transcript.channel.lifecycle` event-log topic, mirroring
  the `transcript.reminder.lifecycle` pattern from S-15 (#1865).
  Window-expire flushes of aggregation buffers (`fire_partial` /
  `discard` paths) reconstruct the `ResolvedChannel` from the first
  buffered event so the match span carries the right scope + name even
  when the flush dispatches off a background sweep. Three new
  conformance fixtures under
  `conformance/tests/triggers/trigger_channel_otel_*` cover the
  single-event emit/match link, the batched multi-link multi-emit
  case, and the idempotent re-emit (transcript event still fires with
  `inserted=false`, but fan-out is suppressed so no follow-on match
  event lands). Unblocks CH-07 audit, CH-08 conformance, and B-08 IDE
  rendering.
- **Pool conformance gap-fill: `pool_unsettled_state_integration` (#1892).**
  PL-07 (epic #1883) is the comprehensive pool conformance suite. Eight of
  the ten spec scenarios were already covered by fixtures landed alongside
  PL-01..PL-06: concurrency cap (`pool/max_concurrent_caps_in_flight`),
  priority + LIFO + fair round-robin (`pool/priority_submit`,
  `pool/queue_strategy_submit`), the full backpressure matrix including
  block/drop-oldest/drop-newest/fail-fast/fail-submitter/ring-buffer plus
  drop-receipt audit (`pool/backpressure_policies`), durable pipeline-scope
  restart (`pool_durability/pipeline_survives_restart`), and trigger →
  pool fan-in (`triggers/trigger_spawn_to_pool_*`). The new
  `conformance/tests/pool_unsettled_state_integration.harn` covers the last
  scenario — pool tasks visible in `harness.unsettled_state()` so on_finish
  presets can act on them — and is marked `@xfail` referencing follow-up
  #2007 until the runtime wiring lands.
- **OAuth conformance suite + explicit revoke + `HARN-OAU-002` (#1908).**
  Closes the OA-07 gap-fill on top of the OA-01..OA-06 surface: adds
  three new conformance fixtures — `oauth_refresh_token_expired`
  (server returns `invalid_grant` on the refresh-grant call, raises
  the dedicated `HARN-OAU-002` diagnostic and leaves the stored
  TokenSet untouched), `oauth_revoke` (RFC 7009 best-effort POST to
  the provider's `revoke_url`, authoritative local storage delete,
  `oauth.client.audit` `token_revoked` event with redacted prev shape,
  and a forced `HARN-OAU-002` on the next `token(cli)` call), and
  `oauth_pkce_validates` (mocks the server's mismatched-verifier
  rejection and asserts the error_description threads through with
  no audit and no persisted TokenSet). Promotes the previously
  private `cli.revoke()` storage-delete shim into a real
  `pub fn revoke(cli)` that issues the RFC 7009 token-revocation POST
  on a best-effort basis (network failures never block the local
  discard) and emits an audit event. Tags both the
  no-refresh-token-in-storage and the failed-refresh-grant paths in
  `std/oauth/client` with the new `HARN-OAU-002` diagnostic prefix so
  scripts can pattern-match on a single, stable string instead of
  parsing freeform error text. The fifteen `oauth_*` conformance
  fixtures together cover authorization-code + PKCE, device flow
  (happy/pending/slow-down/expired), refresh on 401, pre-emptive
  refresh at 75 % TTL, refresh-token expiry, revoke, PKCE mismatch,
  storage round-trip across all four backends (memory/file/cloud
  session/cloud org/custom), the provider catalogue against ten
  preconfigured providers, and the OA-06 redaction patterns. Part of
  epic #1885 (OAuth stdlib).
- **`ReminderInject` trigger handler variant (#1876).** Adds a new
  `TriggerHandlerSpec::ReminderInject` variant + the matching
  `ReminderInject({...})` constructor in `std/triggers`, completing the
  CH-05 leg of epic #1870 (Agent channels & periodic prompts). Unlike
  Local/Worker/SpawnToPool handlers, ReminderInject does not spawn a
  task: when the trigger matches, the dispatcher resolves a target
  session (`"current"`, `"parent"`, a literal session id, or a closure
  `event -> string?`), renders a `.harn.prompt` body template against
  `{{ event }}` / `{{ match }}` / `{{ batch }}`, builds a
  `SystemReminder` (#1815) carrying the rendered body plus the
  binding's `tags` / `ttl_turns` / `dedupe_key` / `propagate` /
  `role_hint` / `preserve_on_compact` metadata, and injects it via
  `agent_sessions::inject_reminder`. The reminder surfaces at the
  target session's next turn boundary — same path as
  `transcript.inject_reminder` — so existing dedupe, TTL, and
  capability-aware rendering all apply. Missing target sessions are
  recorded as `triggers.reminder_inject.audit` lifecycle audit entries
  (outcome `dropped`, reason `target_missing` or
  `target_unknown_session`) and the dispatch returns a `dropped`
  result rather than failing the trigger. Combined with CH-04 batching
  this enables declarative "every N events OR every T time" reminder
  injection without user-side counter state. Conformance:
  `conformance/tests/triggers/trigger_reminder_inject_{targets_concrete_session,only_targets_named_session,missing_target_drops_with_audit,closure_target}.harn`.
- **Aggregation triggers — `batch { count, window, key, expire_action }` filter (#1875).**
  Implements CH-04 from epic #1870. New optional `batch` field on the
  trigger DSL accumulates matching channel events into a per-(binding,
  partition_key) buffer; the handler fires with a batched
  [`TriggerEvent`](https://github.com/burin-labs/harn) (`event.batch`
  populated) once `count` is reached or the `window` elapses. `key` is a
  dot-path into the channel payload (e.g. `"repo"`,
  `"pull_request.user.login"`); missing path = single global counter for
  the binding. `expire_action` defaults to `"fire_partial"` (handler
  invoked with the partial batch) and can be set to `"discard"` to drop
  the buffer silently. Window expiration is driven by an implicit sweep
  during the next `emit_channel(...)` and by the explicit
  `flush_trigger_aggregations()` builtin (deterministic + paired with
  `mock_time` / `advance_time` for replay-clean tests). Buffers are
  capped at 1024 events per partition and overflow is reported as a
  structured `triggers.aggregation.buffer_overflow` warning. Bad config
  (count ≤ 0, missing/unparseable `window`, unknown `expire_action`,
  wrong types) raises `HARN-CHN-005` at registration. Inngest-shape
  primitive — no other major durable-execution or agent system has
  first-class fire-after-N-events. Conformance:
  `conformance/tests/triggers/trigger_batch_{count_fires_on_threshold,window_expire_fires_partial,window_expire_discards,key_partitions_independently,back_to_back_resets_buffer,malformed_count_errors}.harn`.
- **Channel scope resolver completes the four-tier hierarchy (#1874).**
  Formalises the `session < pipeline < tenant < org` scope chain for
  `emit_channel(...)` and `channel_events(...)`, building on the producer
  (#1871) and trigger-source consumer (#1872) halves. The resolver is now
  fully deterministic: bare names default to tenant scope; explicit prefixes
  are honoured; `pipeline:` outside a pipeline context fails with
  `HARN-CHN-001`; cross-tenant emits without a grant and any `org:` use fail
  with `HARN-CHN-002`; malformed scope strings fail with `HARN-CHN-003`; and
  a new `HARN-CHN-004` fires when `options.session_id` or
  `options.pipeline_id` explicitly disagrees with the active runtime
  context (previously, an explicit option would silently override the
  context — a quiet path to cross-session leakage). Cross-scope isolation is
  enforced by the topic shape (`channels.<scope>.<scope-id>.<name>`), so
  readers against a different `tenant_id`, `session_id`, or `pipeline_id`
  see an empty view rather than a leaked event. Conformance coverage:
  `conformance/tests/stdlib/channel_scope_{bare_defaults_to_tenant,pipeline_outside_pipeline_errors,cross_tenant_isolation,cross_session_isolation}.harn`
  plus the existing `emit_channel_scope_resolution.harn`. Part of epic
  #1870.
- **Callback-first `ResumeBy.*` exports (#1864).** The new
  `std/agent/resume_by` module replaces implicit resume-responsibility
  inference with four explicit `(harness, suspension) -> dict`
  callbacks: `ResumeBy().parent_llm` (surface the suspension to the
  parent transcript and let the parent's LLM resume),
  `ResumeBy().local_runtime` (register conditions with the in-process
  #152 dispatcher; declines with `no_conditions` when none are set),
  `ResumeBy().cloud_harness` (register with the harn-cloud webhook
  receiver; declines with `no_cloud_session` until HC-07..HC-09 wire
  the back end), and `ResumeBy().pipeline_drain` (defer to the
  enclosing pipeline's drain step). Each callback emits a
  `resume_by_dispatched` or `resume_by_declined` audit through
  `harness.emit_audit` for observability, and the
  `agent_await_resumption(reason, conditions, resume_by)` request now
  carries the callback for the agent loop to invoke at suspend time
  with `invoke_resume_by(...)` (with a `parent_llm` safety-net
  fallback). `first_handled([...])` and `default_resume_by({...})`
  helpers ship in the same module so chains like
  `first_handled([ResumeBy().cloud_harness, ResumeBy().local_runtime])`
  compose with the existing `std/lifecycle/combinators`. Conformance:
  `conformance/tests/agents/resume_by_{parent_llm,local_runtime,cloud_harness,pipeline_drain,composed,default}.harn`.
  Part of epic #1836 (agent suspend/resume) and #1853 (callback-first).
- **`OnBudget.*` callback strategies (#1914).** Adds three named callback
  strategies for the existing `OnBudgetThreshold` lifecycle event in a
  new `std/lifecycle/on_budget` module: `OnBudget.terminate` (emits
  `budget_exceeded` audit, throws a structured `budget_exceeded`
  exception so the surrounding agent loop / pipeline unwinds),
  `OnBudget.graceful_exit` (emits `budget_graceful_exit` audit, returns
  a deterministic `{status: "budget_exhausted", strategy:
  "graceful_exit", reason, budget_state, message}` envelope so the
  `on_finish` chain can drain in-flight work and surface the envelope
  as the pipeline's value), and `OnBudget.warn_and_continue` (emits
  `budget_warn_and_continue` audit, injects a 1-turn `budget_warning`
  system_reminder via `tool_hooks_inject_reminder`, and returns the
  original `budget_state` unchanged so combinator chains see a
  passthrough). All three follow the same `(harness, budget_state) ->
  result` shape that the rest of the lifecycle layer uses, so they
  compose freely with `std/lifecycle/combinators` —
  e.g. `compose([OnBudget.warn_and_continue, custom_logger])` threads
  the dispatcher's snapshot through both entries. A
  `OnBudget()` namespace factory mirrors the `QueueStrategy()` /
  `Backpressure()` factories in `std/lifecycle/pool` for dotted-access
  callers. Conformance coverage at
  `conformance/tests/on_budget_{terminate,graceful_exit,warn_and_continue,compose}.harn`.
  Part of epic #1853 (Pipeline lifecycle).
- **`std/oauth/device_flow` RFC 8628 device authorization grant (#1903).**
  Adds `OAuth.device_flow(provider, opts)` for headless contexts (CI
  runners, daemons, IDE side panes) where a browser redirect is
  impractical. Posts to the provider's device authorization endpoint,
  hands `(user_code, verification_uri)` to a caller-supplied
  `on_user_code` handler (defaults to `eprintln(...)` with the URL and
  code), polls the token endpoint at the server-supplied `interval`,
  honors `slow_down` by adding 5 s to the cadence, treats
  `authorization_pending` as a soft retry, and raises a deterministic
  error on `expired_token` / `access_denied`. On success, builds a
  TokenSet (mirroring the `std/oauth/client` exchange path) and persists
  it through the OA-03 storage handle so `OAuth.client(...)` reads the
  same token + refresh metadata back. Emits an
  `oauth.device_flow.audit` `token_obtained` event log entry whose
  payload includes provider, storage key, and token presence flags but
  never the access token, refresh token, device code, or user code. The
  polling sleep routes through the standard `sleep(ms)` builtin so
  tests under `mock_time(...)` / `advance_time(...)` exercise the
  cadence without real wall-clock waits. Conformance:
  `conformance/tests/stdlib/oauth_device_flow_{happy_path,pending,slow_down,expired}.harn`.
  Part of epic #1885 (OAuth stdlib).
- **Pool OTel spans + audit receipts (#1891).** Adds two new
  `SpanKind` variants — `PoolSubmit` (fires at `pool.submit()`, carries
  `pool` / `pool_id` / `priority` / `key` / `idempotency_key` /
  `task_id` / `status` metadata) and `PoolDequeue` (fires when the
  dispatcher pulls a task out of the queue, carries the same
  `pool` / `pool_id` / `task_id` plus `queued_for_ms` and `slot_index`,
  and links back to the originating `PoolSubmit` span via
  `set_span_link` from P-05 / #1858 so submit → dequeue stays
  correlated across the async boundary). Three audit receipts now land
  on the existing `lifecycle.pool.audit` topic: `pool_submit`
  (`harn.pool_submit.v1`), `pool_dequeue` (`harn.pool_dequeue.v1`),
  and `pool_drop` (`harn.pool_drop.v1`, already shipped in PL-03).
  All three respect the disabled-tracing fast path (the span guards
  return id 0 when tracing is off). The `event_log.subscribe`
  `kind_prefix` filter lets observers narrow the topic to a specific
  receipt kind (the existing backpressure-audit fixtures use
  `kind_prefix: "pool_drop"` so they ignore the new receipts).
  Conformance coverage: `conformance/tests/pool_otel/*.harn`. Part of
  epic #1883.
- **Pool state durability (#1890).** `Pool.create({scope: ...})` now
  routes to three durability backends following the channel scope
  contract: `session` (default, in-memory only — lost on session close);
  `pipeline`, which appends task metadata to a crash-safe JSONL log
  under `.harn/pools/<pipeline_id>__<name>__<hash>.jsonl` and reloads
  the queue + terminal state on next `pool_create`; and `tenant` / `org`,
  reserved for the harn-cloud host (see harn-cloud#306) and rejected
  today with a clear host-routed diagnostic. Submissions accept a new
  `idempotency_key` option that dedupes both within a session and
  across pipeline-scope reloads. In-flight tasks observed on reload are
  classified as `failed` with a stale-restart marker (closures cannot
  cross a process boundary); idempotent re-submission lets the new
  process re-run the work. A `pool_simulate_restart()` builtin drops
  the in-process registry so conformance fixtures can exercise the
  "kill process → restart → verify completion" path without forking.
  Crash safety uses atomic temp-file rename for compaction and
  `sync_data()` per append. See
  `conformance/tests/pool_durability/*.harn`. Part of epic #1883.
- **Tool-hook seed catalogues for the preset library (#1897).** Ships
  the initial "command faux-pas" rule sets that back
  `preset_run_command(stacks: [...])`. Seven catalogues land under
  `crates/harn-stdlib/src/stdlib/stdlib_tool_hooks_catalogues.harn`:
  `harn-canon/rust` (cargo target-dir, test capture, clippy workspace,
  fmt --check), `python` (find vs rg, pip install --user, pytest
  capture), `typescript` (find vs rg, npm install --force /
  --legacy-peer-deps), `swift` (swift build/package clean), `sql`
  (unbounded `SELECT *`), `harn` (cargo run --bin harn missing
  --quiet), and a stackless `universal` catalogue carrying two
  severity=error deny rules — `git push --force` against `main` /
  `master` (with a `HARN_TOOL_HOOKS_ALLOW_FORCE_PUSH=1` escape hatch
  and `--force-with-lease` allowlist) and recursive `rm -rf` against
  root-adjacent paths (`/`, `~`, `..`, `$HOME`, `${HOME}`, `*`,
  resistant to quoting bypasses). A new
  `tool_hooks_seed_registry(stacks)` helper composes the requested
  catalogues plus universal; `preset_run_command(...)` auto-seeds from
  it when no explicit `registry:` override is passed, so opting into a
  stack is a one-liner. Each rule carries an `explanation` the agent
  can paraphrase and a `references` list pointing at upstream docs /
  RFCs / post-mortems. Conformance coverage:
  `conformance/tests/stdlib/tool_hooks_catalogue_{rust,python,typescript,swift,sql,harn,universal,auto_seed}.harn`.
  Part of epic #1884 (preset tool hooks library).
- **Channel-source triggers (#1872).** The trigger DSL now recognizes a
  `provider: "channel"` source with a `match.events: ["channel:<scope>:<name>"]`
  selector, completing the consumer half of the durable channel API shipped
  in #1871. Supported selector shapes: `channel:<name>` (tenant-default),
  `channel:session:<name>`, `channel:pipeline:<name>`,
  `channel:tenant:<tenant-id>:<name>`, `channel:tenant:*:<name>` (tenant
  wildcard), and `channel:org:<org-id>:<name>` (rejected until org grants
  ship, mirroring the producer-side `HARN-CHN-002`). Each `emit_channel(...)`
  that successfully appends to the event log synchronously fans out to all
  matching active bindings; idempotent duplicates short-circuit the
  fan-out. An optional dict-shaped `filter:` (e.g. `"{\"repo\": \"harn\"}"`)
  evaluates dot-path equality against the emit payload before dispatch.
  Conformance coverage:
  `conformance/tests/triggers/trigger_source_channel_{basic,session,wildcard,filter,fan_out}.harn`.
  Part of epic #1870.
- **OAuth token redaction in transcripts and audit (#1907).** Persisted
  transcripts, audit receipts, OTel span attributes, and system reminders
  now route through a named token-pattern catalog so a leaked OAuth bearer,
  GitHub PAT (classic or fine-grained), Slack `xox*`, AWS `AKIA…`, Stripe
  key, OpenAI `sk-…`, JWT, GitLab `glpat-…`, or npm `npm_…` token is
  scrubbed to `<redacted:<pattern>:<len>>` on every display sink. The
  underlying tool/host call still receives the raw token — redaction is
  display-only. Each redaction synchronously records an `HARN-OAU-001`
  audit entry in a per-thread ring drainable via the new
  `std/oauth/redaction` stdlib (`register_pattern`, `redact`,
  `default_patterns`, `custom_patterns`, `clear_custom_patterns`,
  `drain_audit`). When a multi-threaded Tokio runtime is available the
  default sink also appends to the `audit.token_redaction` event-log
  topic. Inputs over 256 KiB short-circuit to a passthrough as defense
  against catastrophic regex behavior. Conformance:
  `conformance/tests/stdlib/token_redaction_{default_patterns,custom_pattern,tool_passthrough}.harn`.
  Part of epic #1885 (OAuth stdlib).
- **Tool-hook mode callback side effects (#1896).** The three shipped
  `tool_hooks_mode_*` callbacks (`rewrite_with_audit`,
  `deny_with_explanation`, `passthrough_only_audit`) now emit lifecycle
  audit entries (`tool_rewrite`, `tool_denied`, `tool_rule_warning`)
  observable via `lifecycle_audit_log_take()`. The rewrite mode also
  queues a 1-turn `tool_rewritten` system reminder via the new
  `tool_hooks_inject_reminder(...)` primitive so the agent's next turn
  learns the corrected command shape. Without an active agent session
  the reminder still records a `tool_hooks.reminder_injected` audit
  entry, so headless pipelines and conformance fixtures can verify
  every side effect deterministically. Two new top-level builtins,
  `tool_hooks_emit_audit(kind, payload)` and
  `tool_hooks_inject_reminder(options)`, are exposed so user-defined
  mode callbacks can hook into the same plumbing. See
  `conformance/tests/stdlib/tool_hooks_mode_callbacks.harn` for the
  executable spec. Part of epic #1884 (preset tool hooks library).
- **Protocol contribution RFCs for reminders (#1829).** Authored three
  upstream-proposal documents under `docs/src/protocol-contributions/`:
  ACP `session/inject_reminder` + `SessionUpdate::ReminderEmitted` (the
  schema-edit follow-up to [ACP #1224][acp-1224]), A2A
  `tasks/inject_reminder`, and MCP `notifications/reminder`. Each RFC
  describes the proposed wire format, migration path from our existing
  `_meta.harn.reminder` envelope, reference-impl status, and open
  questions for upstream maintainers. Upstream filings remain a
  maintainer action — see #1829 for the outstanding work.

[acp-1224]: https://github.com/agentclientprotocol/agent-client-protocol/discussions/1224

- **Lifecycle hook events for suspend / resume / drain phases (#1859).**
  Adds seven new `HookEvent` variants — `PreSuspend`, `PostSuspend`,
  `PreResume`, `PostResume`, `PreDrain`, `PostDrain`, `OnDrainDecision`
  — wired through `register_session_hook` (snake_case names) and the
  shared dispatcher. The control surface gains `HookControl::Modify`
  so lifecycle gates can rewrite the dispatched payload before
  downstream consumers see it (rewrite suspend reason, amend resume
  input, amend drain spec, rewrite drain tool call, amend unsettled
  snapshot). `Block` cancels the gated operation where applicable
  (`PreSuspend`, `PreResume`, `PreDrain`, `OnDrainDecision`,
  `OnUnsettledDetected`); `PreFinish` explicitly rejects `Block` and
  surfaces a runtime error pointing the user at
  `OnFinish.block_until_settled`. Reminder effects continue to be
  inject-only. See `docs/llm/harn-quickref.md` for the per-event
  return-semantics table and `conformance/tests/hooks_session/
  lifecycle_hook_events_*` for executable spec coverage. Settlement
  agent spawning itself remains the P-03 deferred stub (harn#1856).
- **`std/lifecycle/combinators` callback combinator stdlib (#1860).**
  Six pure factories for wrapping `(harness, return_value)`-shaped
  callbacks (hook handlers, `resume_by`, `on_finish`, anywhere a
  function-shaped value reaches user code). `compose(callbacks)` runs
  callbacks sequentially and threads the return value through the
  chain; `first_available(callbacks)` returns the first non-nil
  responder; `with_telemetry(cb, span_name?)` opens a
  `SpanKind::FnCall` OTel span (via new `__lifecycle_span_start` /
  `__lifecycle_span_end` bridge builtins) and emits paired
  `{span_name}_started` / `_completed` / `_errored` audits;
  `with_timeout(cb, ms)` is a soft, clock-aware deadline that
  measures via `harness.clock.now_ms()` and returns a
  `{__timed_out, timeout_ms, elapsed_ms, return_value}` sentinel on
  overrun; `if_unsettled(cb)` only fires when `harness.unsettled_state()`
  has pending work (one snapshot per call); `when(predicate, cb)`
  guards on an arbitrary predicate. Conformance coverage at
  `conformance/tests/combinator_*.harn`. Part of the pipeline
  lifecycle epic (#1853, P-07).
- **`SpawnToPool` trigger handler variant (#1889).** Triggers now compose
  with named agent pools (#1883) via a new `TriggerHandlerSpec::SpawnToPool`
  variant, surfaced through the Harn DSL as
  `handler: SpawnToPool({pool: "pr-review", priority_from: "headers.priority",
  key_from: "tenant_id", task_factory: { event -> { -> review(event) } }})`.
  The dispatcher invokes `task_factory(event)` per matched event, extracts
  priority + fair-queue key via simple dotted JSON paths into the event
  payload (missing paths fall back to defaults rather than erroring), and
  submits the resulting closure to the pool under its configured queue
  strategy + backpressure policy. `drop_newest` rejections reuse the existing
  `lifecycle.pool.audit` channel, and the dispatch result is shaped as a
  `pool_task` handle so handlers can call `pool_wait(dispatch.result)`
  directly. Conformance:
  `conformance/tests/triggers/trigger_spawn_to_pool_*`.
- **`std/oauth/client` PKCE-S256 client + transparent refresh (#1902).**
  Adds `OAuth.client(provider, opts)` and `OAuth.token(client)` per
  RFC 6749 (authorization-code) + RFC 7636 (PKCE S256, always
  enforced) + RFC 9700 BCP refresh guidance. The client builds on the
  OA-04 `std/oauth/providers` catalogue and the OA-03
  `std/oauth/storage` backends so token persistence and rotation use
  the same primitives as the rest of the stack. `start_authorization`
  builds the PKCE-protected `/authorize` URL plus a one-shot
  `code_verifier / state` pair; `exchange_code` validates state, sends
  the code + verifier, and writes the resulting TokenSet to storage.
  `token(cli)` always reads storage as the source of truth so two
  in-process callers see the latest refresh, and pre-refreshes when
  the stored TokenSet is past 75% of its TTL. `request(cli, method,
  url, opts?)` injects a Bearer token and retries exactly once after a
  401, forcing a refresh between attempts. Every successful refresh
  or exchange emits a `token_refreshed` / `token_exchanged` event on
  the `oauth.client.audit` topic; audit payloads carry only presence
  flags + timestamps and never include the new access/refresh tokens.
  New supporting builtins in `std/crypto`: `bytes_to_base64url`,
  `sha256_base64url`, and `crypto_random_bytes(n)` (CSPRNG, capped at
  1024 bytes). Conformance coverage:
  `conformance/tests/stdlib/oauth_client_auth_code.harn`,
  `oauth_client_refresh_on_401.harn`,
  `oauth_client_refresh_near_ttl.harn`.
- **`std/oauth/storage` token storage backends (#1904).** Five
  interchangeable backends for the OAuth client (#1885 OA-03):
  `memory()` (per-VM, ephemeral), `file(path, encryption_key)` (single
  AES-256-GCM envelope with HKDF-SHA256 key derivation, atomic
  temp-file write + rename), `harn_cloud_session()` /
  `harn_cloud_org()` (route through the new `oauth_storage`
  `cloud_get / cloud_set / cloud_delete` host capability so the
  embedder enforces RLS), and `custom({get, set, delete, id?})` for
  vault / KMS integrations. Each backend exposes the same
  `store.get / store.set / store.delete` closures on the handle dict,
  letting the upcoming OAuth client treat storage as a single
  protocol. File-handle encryption keys live in a thread-local map
  off the user-visible handle so a stray `to_string(handle)` cannot
  exfiltrate the secret. See
  [`docs/src/stdlib/oauth-storage.md`](docs/src/stdlib/oauth-storage.md)
  for the full reference and conformance coverage at
  `conformance/tests/stdlib/oauth_storage.harn`.
- **Durable agent channel emits (#1871).** Adds `emit_channel(name, payload,
  options?)` with tenant-default scope resolution, session/pipeline/tenant
  prefixes, signed `emitted_at` timestamps, and idempotent `options.id`
  handling backed by the event log. A `channel_events(...)` inspection helper
  covers conformance and local diagnostics; org-scoped channels resolve but
  return `HARN-CHN-002` until org grants land.
- **Lifecycle span links for suspend/resume and drain scaffolding (#1858).**
  VM tracing now has `suspension`, `resume`, `drain`, and
  `drain_decision` span kinds plus causal span links. Worker suspension
  snapshots persist `prior_span_link` plus the closed pipeline span link
  so cold resumes can link the fresh resume span to closed lifecycle spans
  instead of parenting across process boundaries. The OTel helper layer
  now exposes `set_span_link` beside `set_span_parent`, with no-op
  behavior in non-OTel builds.
- **Resume-continuity system reminder provider (#1845).**
  `agent_loop(...)` now fires the canonical `resume_continuity` provider
  before the first turn after `resume_agent(...)`. The one-shot reminder
  identifies the suspension turn and reason, resume cause, optional
  resumer input, and the pre-suspend digest when resuming with
  `continue_transcript: false`; it is dedupe-keyed, non-propagating, and
  honors the standard reminder provider opt-out list.
- **Reminder lifecycle telemetry and ACP updates (#1828).** Reminder
  injections, render-time firings, dedupe replacements, TTL/compaction/
  clear expiry, malformed drops, sub-agent inheritance, and provider
  evaluation now emit `transcript.reminder.*` EventLog records with
  `session_id`, `task_id`, and `agent_id` correlation fields. Rendered
  reminders also surface through ACP `session/update` as
  `reminder_emitted` under `_meta.harn.reminder`, and hook-origin
  reminder metadata is carried on tool-call receipts.

### Changed

- **Natural-language tool binder defaults reflect the empirical run
  envelope (#1696).** `with_natural_language_executor` now defaults to a
  `500ms` wall-clock budget and `1024` response tokens. The larger token
  budget gives reasoning binders room to emit structured JSON after their
  reasoning preamble; the timeout remains a bounded opt-in hop and still
  degrades to passthrough on overrun.

### Fixed

- **`harness.system.platform()` mock arch leak.** The mock-mode
  `platform()` snapshot in `vm/methods/harness.rs` was reading the host
  CPU architecture via `std::env::consts::ARCH` instead of a fixed
  placeholder, so the `harness/system_basic` conformance fixture passed
  on the Apple Silicon dev machines that recorded it (`aarch64`) but
  failed on the x86_64 Linux CI runners. Mock mode now returns a
  deterministic `"arch": "mock"` matching the rest of the snapshot
  (`os`, `kernel`, `hostname`, …), so the fixture is portable across
  architectures. The real (non-mock) `platform_snapshot()` still
  returns the live host arch.

## v0.8.26

### Added

- **Sub-agent reminder propagation via handoff envelopes (#1824).**
  Handoff artifacts and sub-agent requests now carry
  `reminder_propagation` beside `policy_override`; `handoff(...)` derives
  it from the active session's pending reminders when omitted. Child
  sessions seed inherited reminders before their first turn, rewrite
  `source` to `"inherited"`, and retain `originating_agent_id` for
  audit. `propagate: "all"` continues through descendants,
  `propagate: "session"` stops after direct children, and
  `propagate: "none"` remains local. Canonical `idle_nudge` and
  `tool_output_truncated` reminders now default to `propagate: "none"`.
- **Agent lifecycle tools for self-parking and parent resume control (#1840).**
  `agent_loop(...)` now registers `agent_await_resumption(reason,
  conditions?)` on every loop and handles it structurally when the loop is
  running inside a worker, reusing the worker suspend checkpoint path and
  `parse_resume_conditions(...)`. Parent loops can opt into
  `subagent_pause(handle, reason)` and `subagent_resume(handle, input?,
  continue_transcript?)` with `subagents: true`, `subagent_tools: true`, or
  `agent_lifecycle_tools(registry, {subagents: true})`; lifecycle tool calls
  emit `tool_call_audit` metadata with the initiator and reason.
- **`with_audit_log` local sink emits a `file://` `receipt_uri` (#1693).**
  When `with_audit_log({sink: "local"})` or `"both"` is active, the
  middleware now attaches a `file://` URI pointing at
  `.harn/receipts/<session_id>.jsonl` to both the in-process
  `result.audit` envelope and the typed `ToolCallReceipt`'s embedded
  `audit` dict. The portal's RunDetail view already rendered an
  `audit.receipt_uri` deep-link when present; with this change the link
  resolves to the persisted JSONL for any session that opts into the
  local sink. Cloud-only sinks omit the URI (nothing wrote to disk).
  Closes #1693.
- **`harn run <bundle.harnpack>` verify + replay + execute (#1784).**
  Closes the `.harnpack` epic (#1779) by wiring the `harn run` command
  to accept a signed content-addressed bundle. Detection is by
  `.harnpack` extension or zstd magic header; the runner verifies the
  embedded Ed25519 signature through the OpenTrustGraph trust store,
  re-derives the bundle hash, checks `harn_version` compatibility
  (patch mismatch warns, minor/major refuses), replays the archive into
  `$HARN_CACHE_DIR/packs/<bundle_hash>/` (atomic, so concurrent runs
  share a cache slot), and executes the manifest entrypoint with
  `Harness::real()`. Unsigned bundles are refused by default; pass
  `--allow-unsigned` to override (intended for local development).
  `--dry-run-verify` performs verification and replay without executing,
  useful for deployment gates. A new `pack_run` event flows through the
  `harn run --json` NDJSON stream carrying the bundle hash, signature
  outcome, signing key id, cache-hit status, and dry-run flag so agents
  can audit pack execution without rereading the archive.
- **Typed effect inheritance on `HandoffArtifact` (E5.3, #1776).** Adds
  `EffectRecord { kind, scope, resource }` with `EffectKind` (`Stdio`,
  `Fs`, `Net`, `Llm { provider, model }`, `Tool { name }`,
  `Hostcall { name }`, `Persona { id }`, `Spawn`) and `EffectScope`
  (`Read`, `Write`, `Mutate`, `Observe`) and wires an
  `effects: Vec<EffectRecord>` field onto `HandoffArtifact` envelopes.
  A new `compute_handoff_effects(source, ceiling)` utility reuses the
  same capability analysis that backs `harn graph --json` (#1758) and
  walks the AST for harness sub-handle method calls
  (`harness.net.get`, `harness.fs.write_file`, …) that the IR doesn't
  see directly. `attach_spawn_handoff_effects` and the
  `handoff_effects(source, ceiling?)` Harn builtin let spawn shims
  populate the field at child-spawn time, clamping the computed set
  to the spawn-config declared ceiling. Enforcement of the ⊆ relation
  lands in E5.4 (`HARN-CAP-301`); the OpenTrustGraph receipt-chain
  embedding lands in E5.5.
- **`owned<T>` + scope-exit auto-drop (#1789).** A new `owned<T>` type
  modifier marks a binding as carrying sole ownership of a drop-able stdlib
  handle (channels, sync permits, future MCP/ACP/file handles). The compiler
  emits an implicit `defer { drop(<binding>) }` at the binding's enclosing
  block so the resource closes deterministically on normal exit, early
  `return`, `break` / `continue`, and uncaught throws — no manual
  `close_channel` / `mcp_disconnect`. `drop()` is a new builtin that
  dispatches on the runtime value tag; unknown values are a silent no-op so
  callers can hand it any value. The existing `defer { ... }` block is now
  also block-scoped (it previously leaked to function scope), running at the
  end of its innermost `{ ... }` and at each loop-body iteration. A new
  `channel_is_closed(channel) -> bool` builtin makes channel lifecycle
  observable from tests. `HARN-OWN-003` (`OwnershipEscape`) warns when an
  `owned<T>` binding escapes via `return` without the function declaring
  `owned<T>` as its return type; widening the return type signals an
  explicit ownership transfer.
- **Transcript reminder transforms (#1817).** Adds
  `transcript.inject_reminder(transcript, options)` and
  `transcript.clear_reminders(transcript, selector)` over pending
  `system_reminder` events. Injection validates reminder options with
  `HARN-RMD-001`, appends a typed reminder event, replaces older
  pending reminders with the same `dedupe_key`, and emits
  `transcript.reminder.deduped` when an EventLog is active. Agent
  post-turn processing now decrements finite `ttl_turns`, removes
  expired reminders, and emits `transcript.reminder.expired`.
- **Hook-emitted system reminders (#1819).** Tool, persona, step, and
  session hooks can now return typed reminder effects using
  `{reminder: {body, tags?, dedupe_key?, ttl_turns?,
  preserve_on_compact?, propagate?, role_hint?}, then?}`, a bare
  reminder spec, or a session-level effect list. Hook reminders inject
  into the active session transcript as `system_reminder` events, use
  `source: "hook"`, honor `dedupe_key`, and compose with existing
  allow/deny/modify/block/decision return shapes. `register_tool_hook`
  also accepts `pre` and `post` closures alongside the legacy `deny`
  and `max_output` shortcuts.
- **Reminder provider registry (#1820).** `agent_loop(...)` now enables
  canonical stdlib reminder providers for token pressure, daemon idle
  nudges, truncated tool output, and post-compaction recaps. Loop callers
  can opt out with `reminders: {providers: ["-token_pressure"]}` or
  disable providers with `reminders: false`; bare `llm_call(...)` does
  not auto-fire providers. `register_reminder_provider({id,
  subscribes_to, evaluate})` lets Harn scripts add provider callbacks
  that return the same reminder effects used by hooks, and
  `clear_reminder_providers()` clears user-defined providers for tests
  and isolated runs.
- **Bridge `session/remind` reminder injection (#1821).** Bridge and ACP
  hosts can queue typed system reminders with the same
  `interrupt_immediate` / `finish_step` / `wait_for_completion` delivery
  modes as queued user messages. `session/input`, `user_message`, and
  `agent/user_message` remain user-role only; reminders validate the
  reminder-spec payload, report malformed payloads as `HARN-RMD-002`, and
  enter the transcript with `source: "bridge"`.
  reminder-spec payload and enter the transcript with `source: "bridge"`.
- **Capability-aware system-reminder rendering (#1822).** Pending
  `system_reminder` events now render at LLM-call time based on the
  resolved provider capability row: OpenAI developer-role routes receive
  separate `developer` messages, Anthropic user-block hints prepend a
  `<system-reminder>` content block with prompt-cache metadata when
  supported, Gemini XML routes fold reminders into the system prompt with
  XML scaffolding, and local fallback routes use plain system text. `harn
  lint` emits `HARN-RMD-003` when a pipeline hardcodes
  `role_hint: "user_block"` alongside a route that cannot preserve that
  shape.
- **Compaction-aware reminder lifecycle (#1823).** `transcript_compact`
  now treats pending `system_reminder` events as first-class lifecycle
  state: finite `ttl_turns` values are decremented at the pre-compaction
  boundary, expired reminders emit `transcript.reminder.expired`,
  duplicate `dedupe_key` reminders collapse to the newest event, and only
  `preserve_on_compact: true` reminders are copied verbatim into the
  compacted transcript. Custom compactors receive surviving reminder
  payloads as a second closure argument, and `harn lint` emits
  `HARN-RMD-004` for discardable reminder literals with no finite TTL.
- **OAuth provider catalogue records (#1905).** Adds
  `std/oauth/providers` with ten preconfigured OAuth provider records
  (GitHub, Slack, Linear, Notion, Google, Microsoft, Atlassian,
  Discord, GitLab, Bitbucket), the `github_enterprise(base_url, ...)`
  and `custom(config, ...)` factories, and override-friendly endpoint
  fields for future OAuth orchestration work. The catalogue is static
  and offline-only; it does not start a live OAuth flow.
- **Pipeline `on_finish` callback + finish lifecycle hooks (#1854).**
  Adds the `pipeline_on_finish(callback)` builtin and three new
  session-level hook events — `PreFinish`, `PostFinish`, and
  `OnUnsettledDetected` — emitted by `Vm::execute` after the script's
  declared steps complete. The callback receives `(harness,
  return_value)` and its return value replaces the pipeline's return
  value, so post-completion logic (transcript wrap-up, audit emission,
  settlement decisions) has a typed boundary instead of an abandoned
  trailing edge. `std/lifecycle` exports `on_finish_abandon` (the
  legacy no-op preset) and `on_finish_drain` (a stub the P-02
  settlement-agent epic replaces). `OnUnsettledDetected` only fires
  when a future unsettled-state snapshot is non-empty; P-01 ships an
  always-empty snapshot so the wiring is exercised without depending
  on the suspend/resume, trigger-queue, handoff-envelope, or
  in-flight-LLM-call producers that follow under harn#1853. Foundation
  ticket for the Pipeline Lifecycle Framework epic.
- **Tool-hook catalogue primitive: `tool_rule`, `catalogue`,
  `tool_hooks_registry`, `tool_hooks_register`, `tool_hooks_unregister`,
  `tool_hooks_list`, `tool_hooks_match` (#1894).** Foundational TH-01
  schema + registry for the preset tool-hooks epic (#1884). `ToolRule`
  values carry an id, regex-or-callable pattern, `applies_to` stack list,
  severity, optional rewrite closure, explanation, references, and
  priority. `Catalogue` bundles rules with stack/version/source
  provenance, and the registry composes catalogues with predictable
  ordering. `tool_hooks_match` sweeps catalogues × rules linearly and
  sorts results by rule priority, catalogue priority, then declaration
  order so downstream mode callbacks (TH-03) and the
  `preset_run_command` wrapper (TH-02) layer cleanly on top.
- **Harness unsettled-state snapshot foundation (#1857).** Adds
  `harness.unsettled_state()`, `harness.is_empty(state?)`,
  `harness.counts(state?)`, and `harness.summary(state?)`, plus matching
  `std/lifecycle` helpers for `on_finish` callbacks. Suspended subagents
  and in-flight LLM calls now enumerate from live VM registries; trigger
  and handoff buckets are stable typed empty lists until their per-item
  registries land. Root harness action methods are present, with worker
  resume/cancel delegated to host worker primitives and not-yet-backed
  actions returning typed unsupported results.
- **Named agent thread pools — `std/lifecycle/pool` (#1886).** Adds
  `pool_create({name, max_concurrent, ...})` plus per-handle
  `pool.submit(closure, options?)`, `pool.size()`, and `pool.snapshot()`
  for bounding concurrent agent work behind a named, queue-backed pool.
  Submissions accept `key` and `priority` hints (higher priority dequeues
  first; ties FIFO). `pool_wait(handle)` and `wait_agent(handle)` both
  block on pool task handles. Foundation for the agent pool epic
  (#1883); queue strategies, backpressure, durability, channel
  composition, and OTel spans land in #1887..#1893.
- **Agent pool queue strategies (#1887).** Extends
  `std/lifecycle/pool` with `fifo()`, `priority()`, `lifo()`,
  `fair_round_robin(key?)`, and the `QueueStrategy()` namespace helper.
  `pool_create({queue: ...})` now selects deterministic FIFO, LIFO,
  priority, or key-partitioned fair round-robin dispatch while preserving
  priority dispatch as the default for existing pools.
- **Agent pool backpressure policies (#1888).** Adds
  `Backpressure()` plus `backpressure_queue(max_depth, on_full)`,
  `fail_fast()`, and `ring_buffer(capacity)` descriptors for bounding pool
  queues. Full queues can block submitters, drop oldest/newest tasks,
  fail immediately with `HARN-POL-001`, or reject fail-fast submissions
  with `HARN-POL-002`; drop paths return rejected task handles and emit
  `pool_drop` audit events on `lifecycle.pool.audit`.
- **Per-symbol stdlib metadata contract (#1790).** Every public stdlib
  function declares an `@effects`, `@allocation`, `@errors`,
  `@api_stability`, and `@example` block above its `pub fn`. A new
  `harn_parser::stdlib_metadata` module parses those fields, LSP hover
  surfaces them as a markdown panel beneath the existing doc comment,
  `harn graph --json` exposes the parsed block on each `public_symbols`
  entry, and the `HARN-STD-101` lint (`missing-stdlib-metadata`) gates
  the embedded `crates/harn-stdlib/src/stdlib/` tree so new public
  surfaces cannot land without a declared contract. `make lint-harn`
  blocks the release gate on any HARN-STD-101 warning. Coverage at
  landing: 803 of 864 public stdlib functions (92.9%); the remaining
  61 functions are exempt because their preceding doc block is missing
  (separately tracked by HARN-LNT-024).
- **`harn check --json` and `harn fmt --json` (#1759).** Adds
  standard `JsonEnvelope` reports for static checks and formatter
  runs, including per-file status, diagnostics, summary counts, and
  schema catalog entries. Provider and connector matrix JSON output
  now uses the same envelope; `--format=json` remains as a deprecated
  alias for the matrix commands for one patch release. Merge Captain
  ladder, iterate, and audit commands now accept `--json` with
  `--format=json` kept as a deprecated alias.
- **`harn parse --json` and `harn tokens --json` (#1757).** Adds
  parser and lexer inspection commands for tooling. `harn parse` prints
  the AST in text mode or a tagged `Program` JSON tree with byte spans
  in `--json` mode. `harn tokens` prints the lexer stream in text mode
  or `{ kind, lexeme, start, end, line, column }` entries in `--json`
  mode. Both structured surfaces use the standard `JsonEnvelope` and
  register in `harn --json-schemas`.
- **LSP repair metadata for quick fixes (#1750).** Harn LSP diagnostics now
  include flat `data.code`, `data.repair_id`, and `data.safety` fields
  alongside the existing nested repair envelope. Repair-backed code actions use
  `quickfix.harn.<safety>` kinds and carry `{ repair_id, safety,
  diagnostic_code }` data so IDE clients can dispatch without parsing
  diagnostic prose.
- **Diagnostic-code catalog: generated `docs/src/diagnostics.md` +
  `docs/diagnostics-catalog.json` sidecar (#1751).** New
  `harn explain --catalog [--format markdown|json|text]` dumps the
  in-binary `HARN-<CAT>-<NNN>` registry as a complete catalog so agents,
  editors, and hosted error pages dispatch on `schemaVersion: 1`
  metadata (`code`, `category`, `summary`, `repairs[]`, `related[]`,
  `explanationPresent`, `apiStability`) without parsing prose. The mdBook
  catalog page is regenerated via `make sync-diagnostics-catalog`; a new
  `make check-diagnostics-catalog` CI gate fails on drift between the
  committed catalog and the in-binary registry. The hand-written tour of
  shape diagnostics moved to `docs/src/reading-shape-diagnostics.md`.
- **`harn test conformance --json` conformance report (#1756).** The
  conformance runner now emits a versioned `JsonEnvelope` with a stable
  `snapshotKey`, per-fixture outcomes, summary counts, diagnostic-code
  extraction, and machine-enforced `@xfail` handling where stale markers
  surface as `xfail_unexpected_pass` failures.

## v0.8.25

### Added

- **`harn time run [--json]` — phase-timed run with cache hit/miss
  and per-LLM/tool latency (#1787).** New top-level `harn time`
  subcommand wraps an existing subcommand and emits a structured
  timing breakdown. `harn time run script.harn` prints a human
  table; `harn time run script.harn --json` emits a versioned
  `JsonEnvelope` with five fixed phases (`parse`, `typecheck`,
  `bytecode_compile`, `run_setup`, `run_main`), a `cache: "hit" |
  "miss"` marker on `bytecode_compile`, per-LLM-call entries
  (model, latency, tokens), per-tool-call entries (name, latency),
  and totals (`wall_ms`, `cpu_ms`, `cache_hits`, `cache_misses`).
  The envelope is registered under `time run` in `harn
  --json-schemas`. Pair with `--no-cache` to force a cold compile
  and `-e <code>` to time an inline script. Script `stdout` is
  routed to stderr in `--json` mode so the envelope is alone on
  stdout for `jq`-style consumption.
- **`harn doctor --json` capability matrix (#1785).** Doctor now emits a
  `JsonEnvelope` (`schemaVersion: 2`) whose `data` carries four structured
  sections in addition to the existing `checks` / `hardware` / `summary`
  keys consumed by Burin Code preflight: `host` (os, arch, harn version,
  rust toolchain), `targets[]` (per-Rustup triple `installed` +
  `buildable` flag, with reasons), `providers[]` (per-provider
  `configured` / `reachable` / `latency_ms` / `errors`), and
  `capabilities[]` (each stdlib effect mapped to the sandbox profiles
  that permit it on this host). `summary` adds the spec-aligned
  `blocking` / `warning` counts. New opt-in flags `--check-providers`
  and `--check-targets` fan out active HTTP / `cargo check` probes
  (parallel-fanned for providers; only configured providers are probed
  so no-credential rows don't pollute the report with 0ms
  `unreachable`). The legacy `--no-network` flag is removed — doctor is
  now offline-clean by default, so the flag was redundant. Text output
  is regenerated from the same `DoctorReport`, so JSON and human
  surfaces never drift.

- **`harn dev --watch` incremental loop gated by interface fingerprints
  (#1786).** New `harn dev --watch [<root>]` watches every `.harn` file
  under `<root>` (defaulting to the cwd) and re-type-checks only the
  modules whose public surface actually moved. Each module carries a
  BLAKE3 **interface fingerprint** over its public types, function /
  pipeline / tool signatures, struct + enum shapes, and `pub import`
  re-exports — bodies, doc comments, and private helpers are
  intentionally excluded. An edit that flips a fingerprint transitively
  invalidates importers via `ModuleGraph::importers_of`; an edit that
  leaves the fingerprint stable only re-checks the changed file.
  `--json` emits an NDJSON event stream (`ready` /
  `fingerprint_changed` / `rerun` / `diagnostics` / `tests`) wrapped in
  the canonical `JsonEnvelope`, registered under `dev` in
  `harn --json-schemas`. `--with-tests` extends the loop to also
  re-run `test_*` / `@test`-attributed pipelines in each invalidated
  module. New `harn_modules::fingerprint` module exposes
  `fingerprint_program` / `fingerprint_file` / `fingerprint_source` for
  reuse by editors and the bytecode cache.
- **`harn routes <root> [--json]` static trigger inventory (#1788).** New
  top-level command audits declarative trigger projects without executing
  handlers, reporting route paths, handler modules, declared budgets, inferred
  host capabilities, vendor-lock disclosure, and template framework-overhead
  tokens. The JSON mode uses the standard `{ schemaVersion, ok, data, error,
  warnings }` envelope and is registered in `harn --json-schemas`.
- **`harn explain HARN-<CAT>-<NNN>` text + `--json` envelope (#1748).** The
  `explain` subcommand now dispatches on registered stable diagnostic codes
  in addition to its original control-flow invariant form. `harn explain
  HARN-TYP-014` prints an embedded markdown explanation plus a curated
  `See also:` list; `harn explain HARN-TYP-014 --json` emits a stable
  envelope `{ schemaVersion, code, category, summary, body, repairs,
  related, apiStability }` that LSPs, IDEs, hosted error-page mirrors,
  and agents can dispatch on without parsing prose. Every entry in the
  diagnostic-code registry (#1746) carries an explanation markdown body
  under `crates/harn-parser/src/diagnostic_codes/explanations/HARN-<CAT>-
  <NNN>.md`, embedded via `include_str!` so a missing file fails the
  build — the CI gate required by the epic. The legacy
  `harn explain --invariant <NAME> <FUNCTION> <FILE>` form continues to
  work; both forms share one dispatcher.
- **`harn run --json` event stream (#1755).** Adds a versioned NDJSON
  surface to `harn run`: one `JsonEnvelope` per line, each carrying a
  typed `RunEvent` with a strictly monotonic `seq`. Variants cover
  `stdout`, `stderr`, `transcript`, `tool_call`, `tool_result`, `hook`,
  `persona_stage`, plus a terminal `result` / `error` event. `--quiet`
  drops the noisy `stdout` / `stderr` events while keeping the
  structured stream intact. The `run` command is now listed in
  `harn --json-schemas` with `schemaVersion: 1`. Foundation for the
  `--json` epic (#1753) and the burin-code TUI replatform.
- **`llm_call` mid-stream `output_schema` abort (#1775).** When an
  `llm_call` carries `output_schema` and `schema_stream_abort` is on
  (the new default for schema-bearing calls), the streaming transport
  feeds every visible text delta through the incremental validator from
  `std/json/stream`. The first delta that makes the partial JSON unable
  to satisfy the schema short-circuits the provider stream, emits a
  `schema_stream_aborted` transcript event (`provider`, `model`,
  `reason`, `path`, `chunks_consumed`), increments
  `harn_llm_schema_stream_aborted_total{provider,model}`, and surfaces
  an `ErrorCategory::SchemaStreamAborted` to the schema-retry loop.
  The loop consumes one `schema_retries` slot per abort and folds the
  abort `path` + `reason` into the corrective nudge, so the next
  attempt gets a sharper prompt than a generic stream failure. Opt out
  by passing `schema_stream_abort: false` to let the stream complete
  and rely on `schema_retries` for post-hoc recovery. SSE
  (`consume_sse_lines`), NDJSON (`consume_ollama_ndjson_lines`), and
  the in-process `FakeLlmProvider` all route through the shared
  `StreamSchemaWatch` helper so script-driven and test-driven paths
  exercise the same logic.
- **`harn skills list / get / dump` over the embedded corpus (#1762).**
  Wires the `harn-skills` bundled corpus (#1761) through the CLI:
  `harn skills list [--json]` returns the canonical Harn skills shipped
  with this build, `harn skills get <name> [--full] [--json]` prints
  one skill's frontmatter (and SKILL.md body when `--full` is passed),
  and `harn skills dump --all [--out <dir>] [--force]` writes every
  skill to disk as byte-stable copies of the embedded source. `--json`
  output uses the canonical `JsonEnvelope` shape from #1754, with a
  `skill_not_found` error envelope and `error.details.available`
  payload on unknown lookups. The legacy FS-discovery list view moves
  to `harn skills resolved` (same flags as the old `list`), and
  `harn skills list / get` register in the `harn --json-schemas`
  catalog as schema-version 1.
- **Repair classifier on diagnostics (#1747).** `TypeDiagnostic` grows a
  structured `repair: Option<Repair>` field carrying a namespaced repair
  id (`bindings/make-mutable`, `imports/fix-path`, …), a one-line
  summary, and a six-level `RepairSafety` taxonomy
  (`format-only` → `behavior-preserving` → `scope-local` →
  `surface-changing` → `capability-changing` → `needs-human`). Agents
  and IDEs dispatch on `repair.safety` to decide whether to auto-apply,
  propose, or escalate. The catalog maps ≥20 diagnostic codes to
  templates via `Code::repair_template()`; `LintDiagnostic` and
  `FormatError` expose the same handle through derived
  `.repair()` methods. The CLI now renders a `repair: ID [SAFETY] —
  SUMMARY` line, and the LSP attaches the same payload to
  `Diagnostic.data` as `{"repair": {"id", "summary", "safety"}}`.
- **`harn pack <entrypoint>` builds a `.harnpack` run bundle (#1781).**
  New top-level CLI subcommand that walks the entrypoint's transitive
  imports, precompiles every module into the bytecode-cache header
  format, snapshots the provider catalog hash + stdlib pin, generates a
  minimal SBOM (one entry per module + stdlib dep), and assembles a v2
  `WorkflowBundle` manifest. The result is a deterministic tar.zst
  archive written to `<entrypoint>.harnpack` (overridable with
  `--out <path>`). `--json` emits the canonical `JsonEnvelope`
  containing `bundle_hash` (BLAKE3 over the canonical manifest + sorted
  content hashes), `output_path`, `size_bytes`, and the full manifest;
  the catalog row is registered with `harn --json-schemas`.
  `--upgrade <old.harnpack>` reads an older bundle (v1 JSON or v2
  archive) and re-emits it under the v2 manifest, preserving the prior
  bundle's id, name, version, triggers, workflow graph, and prompt
  capsules while populating the new v2 fields from the entrypoint walk.
  `--sign --key <path>` signs the canonical bundle hash with an
  Ed25519 PKCS#8 PEM key, embeds the manifest signature, and emits an
  OpenTrustGraph `release` record; `--unsigned` skips the manifest
  signature but still emits the release record at autonomy tier
  `suggest`. Full SBOM enumeration (E6.4) lands in a follow-up.
  Companion: `harn-vm` exposes
  `read_workflow_bundle_manifest_any_version` and
  `load_workflow_bundle_any_version` for relaxed-schema reads, and
  `bytecode_cache::serialize_chunk_artifact` /
  `serialize_module_artifact` so in-memory consumers can package
  bytecode bytes byte-identical to the on-disk `.harnbc`/`.harnmod`
  files.
- **`Harness` capability handle + `main(harness: Harness)` entrypoint
  (#1766).** First step of the E4 epic that replaces ambient stdio /
  clock / fs / env / random / net globals with an explicit handle threaded
  through `main`. Introduces the public `harn_vm::Harness` Rust type and
  its six sub-handles (`HarnessStdio`, `HarnessClock`, `HarnessFs`,
  `HarnessEnv`, `HarnessRandom`, `HarnessNet`), each carrying the same
  refcounted `HarnessInner` and exposed at the Harn-language level via
  field access (`harness.stdio`, `harness.clock`, etc.). `Harness::real()`
  wires `harn-clock::RealClock` as the production backing; the
  filesystem, environment, random, and network surfaces are stubbed and
  land alongside the ambient-builtin migrations in E4.2-E4.4. The VM
  auto-invokes `fn main` when present with the `harness` global set by
  the run path (CLI, conformance, bench, playground), and the
  typechecker enforces the new convention via `HARN-NAM-101`: any
  `fn main` with a signature other than `main(harness: Harness)` or the
  unused-capability opt-out `main(_harness: Harness)` is rejected at
  parse time. `harness.stdio.println` / `print` / `eprintln` / `eprint`
  and `harness.clock.now_ms` / `monotonic_ms` / `sleep_ms` are wired
  end-to-end so `examples/hello_v2.harn` runs against the new shape
  today.
- **`harn fix --plan --json` repair plans (#1749).** Adds plan-only
  `harn fix` mode for diagnostics with registered repair classifiers.
  `harn fix --plan <path>` prints a summary table and
  `harn fix --plan --json <path>` emits a `RepairPlan` with
  `schemaVersion: 1`, `diagnostics[]`, `repairs[]`, and
  `safetyLevels[]`. Plan mode mirrors the check path across type-check,
  lint, and preflight diagnostics, reports concrete `FixEdit` entries
  when available, marks overlapping edits via `applies_cleanly: false`
  plus `conflicts_with`, and never writes files. `--safety <class>`
  filters proposals by maximum autonomy class.
- **`harn fix --apply --safety <class>` guarded repair application
  (#1752).** Extends `harn fix` with the apply half of the repair
  contract. Apply mode requires an explicit ceiling from `format-only`
  through `capability-changing`, rejects `needs-human` as propose-only,
  filters out conflicts and above-ceiling repairs, writes accepted
  `FixEdit`s bottom-up per file, then reruns the same diagnostic passes
  to report `post_apply_diagnostics_count`. `--dry-run` reports the same
  apply set without writing, and `--apply --json` emits a
  schema-versioned result with `applied[]`, `skipped[]`, skip reasons,
  and the post-apply diagnostic count. The `harn --json-schemas` catalog
  now includes `fix apply`.
- **`with_scoped_executor` tool-caller middleware (#1702).** Narrows the
  active `CapabilityPolicy` for the duration of one tool dispatch:
  `compose_tool_callers([..., with_scoped_executor({stage, allowed_tools,
  side_effect_level?, capabilities?}), ...])`. The middleware pushes the
  scoped policy via `with_execution_policy(...)` (now intersected with
  the ambient policy so it can only tighten the surface, never widen
  it), preemptively rejects out-of-allowlist tool names with the new
  `status: "scope_violation"`, and decorates results with
  `audit.scope = {stage, allowed_tools, ...}`. Pairs with
  `PersonaRuntimeBinding.stages` for per-stage tool scoping outside a
  persona manifest. Opt-in `on_violation: "raise"` throws instead of
  short-circuiting so `try { agent_loop(...) }` can route on the
  exception.
- **In-turn parallel tool dispatch (#1699).** `agent_loop` now accepts a
  `max_concurrent_tools: N` option (default 1). When a planner emits
  multiple tool calls in a single turn, siblings dispatch concurrently
  capped at `N`; middleware-backed calls use `parallel settle`, and the
  host batch path honors the same cap. Each middleware sibling invokes
  its own caller chain in a fresh scope so `audit.layers` histories
  never cross-talk between concurrent calls. Results inject in source
  order regardless of completion order so text tool-call parsers keyed
  on declaration order keep working. The tool-call envelope grows
  `turn.tool_call_index`, and typed `ToolCallReceipt` records grow an
  `emit_order` field equal to that index so completion-ordered receipts
  can be re-sorted to source order. The new `prefetch_next_turn: true`
  option starts the next planner turn after tool results are recorded
  while local/custom audit receipt sinks flush in the background; the
  loop drains those flushes before returning. Parallel tool dispatch
  now scopes `current_tool_call_id()` task-locally across `.await`
  points and propagates cancellation instead of turning it into an
  ordinary tool error.
- **Natural-language tool binder middleware (#1698).** New experimental
  `std/llm/tool_binder` module exporting
  `with_natural_language_executor({provider, model, timeout_ms, ...})`.
  Composes via `compose_tool_callers` to intercept a tool call, hand
  the planner-emitted intent + JSON Schema to a sub-100ms binder LLM
  (Cerebras GPT-OSS-120B is the primary substrate), and replace
  `tool_args` with the binder's structured output before dispatch.
  OFF by default — activation requires explicit composition. Hard
  wall-clock budget per the parent epic (#1696): overruns drop the
  binder hop and pass through unchanged with
  `audit.binder.status = "timeout"`. Surfaces a typed audit slot at
  `audit.binder = {provider, model, status, latency_ms, reason?, attempts}`
  alongside the existing `with_audit_log` receipt machinery.
- **Per-tool-call FS snapshot + restore hostlib primitives (#1720).**
  Adds `hostlib_fs_snapshot`, `hostlib_fs_restore`,
  `hostlib_fs_list_snapshots`, and `hostlib_fs_drop_snapshot` builtins
  under the existing `fs/` schema bucket. Snapshots are keyed by the
  caller-supplied `scope_id` (canonically the ACP `toolCallId`) and
  stored content-addressed under
  `.harn/state/snapshots/<session>/<scope>/`. `tools/write_file` and
  `tools/delete_file` lazy-capture pre-images into any open snapshot
  bound to the current `harn_vm::agent_sessions::current_tool_call_id`,
  so a single `hostlib_fs_snapshot({session_id, scope_id})`
  registration is enough to roll the next mutation back. Session
  bundles evict oldest-first past a configurable byte cap (default
  1 GiB).
- **ACP `session/restore_tool_call` method (#1720).** The ACP server now
  advertises `{ sessionCapabilities: { restoreToolCall: {} } }` in the
  initialize response and exposes a `session/restore_tool_call` method
  that drives `harn_hostlib::fs_snapshot::restore`. Each restore emits
  a canonical `session/update` with `sessionUpdate: "tool_call_update"`,
  `status: "restored"`, and `_meta.harn.kind = "tool_call_restored"`.

### Changed

- **`harn eval tool-calls` binder schema works under Cerebras strict mode
  (#1698 follow-up).** The binder's JSON Schema response shape used
  `arguments: {"type": "object"}` with no `properties` list, which
  Cerebras's strict OpenAI-compat structured-output endpoint rejects with
  HTTP 400 (`Object fields require at least one of: 'properties' or
  'anyOf'`). The harness now asks the binder for a JSON-stringified
  `arguments_json` field with all top-level fields unconditionally
  required (strict OpenAI-compat providers also reject conditional
  `if/then/else` requirement schemas), and the scorer parses the string
  back. The legacy inline `arguments` object shape is still accepted as a
  fallback so non-strict providers keep working. This unblocked the
  empirical go/no-go cell against Cerebras GPT-OSS-120B for #1698.
- **`llm_call` now recognizes `timeout_ms` (#1698 follow-up).** The
  option surface gains `timeout_ms` as a first-class alias for
  `timeout` so `with_timeout`-style middleware, the natural-language
  tool binder, and other callers that already think in milliseconds
  stop having their value silently dropped at the parse layer. The
  underlying HTTP transports still consume `Duration::from_secs(u64)`,
  so a `timeout_ms` value is rounded UP to the nearest whole second
  for the network-level backstop; sub-second budgets must additionally
  be enforced at the caller (the binder middleware already does this
  via a wall-clock post-check). When both `timeout` and `timeout_ms`
  are set, the explicit `timeout` (seconds) wins.
- **WorkflowBundle schema v2 / `.harnpack` foundation (#1780).** Portable
  workflow bundles now use schema v2 with canonical package metadata
  (`entrypoint`, transitive module hashes, Harn/stdlib versions, provider
  catalog hash, tool manifest, SBOM, signature slot, and parent trust record
  linkage). The VM can build/read deterministic `.harnpack` `tar.zst`
  archives with `harnpack.json` at the root, computes BLAKE3 bundle hashes
  over canonical manifest bytes plus sorted content hashes, and rejects v1
  manifests with a typed schema-version error.
- **Dependency refresh.** Bulk dependency audit and bump across the
  workspace. Notable Cargo bumps: `jsonwebtoken` 9 → 10 (now selects
  the `aws_lc_rs` crypto backend explicitly to match the rustls
  provider already in use), `zip` 2 → 8 (the `flate2` Cargo feature is
  folded into `deflate-flate2`), `sha1` 0.10 → 0.11 and `sha3` 0.11 →
  0.12 to align with the rest of the RustCrypto family. Workspace
  `cargo update` rolled minor/patch upgrades for aws-lc, hyper-rustls,
  jsonschema, rustls-pki-types, tokio, tonic, tower-http, wasm-bindgen,
  and the wasm-* tool crates. npm: portal jumps `vitest` 3 → 4 and
  bumps `react-router-dom`, `react-intl`, `@codemirror/view`, `eslint`,
  `vite`, and `typescript-eslint` to current. Root `@redocly/cli` and
  vscode `@vscode/vsce` bumped to clear high/moderate `npm audit`
  vulnerabilities in their transitive trees (`protobufjs`,
  `fast-xml-builder`, `fast-uri`). All workspaces now report
  `0 vulnerabilities`.

## v0.8.24

### Fixed

- **Swift protocol artifact CaseIterable synthesis.** Generated Swift
  wire-value enums now include explicit `allCases` arrays so deprecated
  cases such as ACP `session/stop` do not break downstream Swift builds.

## v0.8.23

### Added

- **Staged filesystem hostlib mode (#1722).** Adds the `fs/` hostlib
  capability with `hostlib_fs_set_mode`, `hostlib_fs_staged_status`,
  `hostlib_fs_commit_staged`, and `hostlib_fs_discard_staged`.
  Sessions can switch to deferred writes stored under
  `.harn/state/staged/<session_id>/`; hostlib file reads, directory
  listings, outlines, AST parse-file calls, and code-index file reads
  see pending changes before disk, while commit/discard controls apply
  or drop the accumulated diff. ACP hosts can drive the same mode with
  `session/fs_mode` and `session/fs_commit_staged`, and receive
  `session/update` progress notifications tagged
  `_meta.harn.kind = "staged_writes_pending"`.
- **Imported-module bytecode cache + correctness fixes (#1710 follow-up).**
  Extends the v0.8.22 entry cache to imported modules — both stdlib and
  user files now persist a `.harnmod` artifact alongside the entry
  `.harnbc`, eliminating per-process re-parse and re-compile of every
  imported function pool. `harn precompile` emits both artifact families
  per source so shipped libraries hit the cache whether the user runs
  them as an entry or imports them. Cache key now folds in active
  `CompilerOptions` (so flipping `HARN_DISABLE_OPTIMIZATIONS` between
  runs no longer reuses a chunk compiled under the opposite setting),
  and the on-disk format gains a kind discriminant so the two artifact
  families coexist in one cache dir without filename collisions. Header
  schema bumped to `2`; older v0.8.22-written cache files are rejected
  fail-closed and recompiled.
- **MCP client notification relay.** The MCP client transport now
  routes inbound `notifications/progress`, `notifications/message`,
  and `*/list_changed` server-to-client notifications into the
  originating session's `agent_inbox` (kinds `mcp_progress`,
  `mcp_log`, `mcp_resource_change`). Correlation is by per-call
  progress token: every outgoing `tools/call` registers a fresh token
  with the new `client_progress` registry and injects it as
  `_meta.progressToken`; inbound notifications carrying that token
  push to the issuing session even when the transport reader runs on
  a different task. Previously these notifications were silently
  dropped at the stdio read loop.
- **`agent_session_post_event(id, kind, content, source?)` builtin.**
  Triggers, connectors, and external host integrations can now post
  events directly into a running session's inbox — the canonical way
  to wire "GitHub PR you've been waiting on just merged" or "your
  remote build finished" nudges into a mid-loop agent. Paired with
  `agent_session_drain_inbox(id)` for explicit drain in scripted
  control flow. Both available at the host-builtin layer
  (`__host_agent_session_post_event`) and as Harn-side wrappers in
  `std/agent/state`.

### Changed

- **Unified per-session agent inbox.** Folded the cross-thread
  `GLOBAL_PENDING_FEEDBACK` queue, its paired `Condvar`, and the
  bespoke push/drain/wait surface into a single per-session inbox in
  `crate::orchestration::agent_inbox`. Producers (long-running tool
  workers, command-policy post-hooks, MCP server notifications,
  triggers, host-side `run_command` background output) call
  `agent_inbox::push(session_id, kind, content, source)` with a typed
  source label; consumers drain in FIFO order with monotonic
  per-session sequence numbers. Synchronous callers still get a
  Condvar-backed `wait_sync`; async callers get the new clock-aware
  `wait_async(session_id, timeout, &dyn Clock)` that composes with
  `harn_clock::PausedClock`. The legacy
  `push_pending_feedback_global` / `drain_global_pending_feedback` /
  `wait_for_global_pending_feedback` exports were removed — breaking
  change for out-of-tree consumers, who should switch to the new API
  one-to-one.
- **Compaction-time event delivery race fixed.** `std/agent/loop` now
  drains the agent inbox **before** `agent_autocompact_if_needed` so
  the summary reflects events that landed between turns, and drains
  **again afterwards** so any push that landed during a Tier-2 LLM
  summarization call (typically 5–30 s wall-clock) is injected into
  this turn's prompt instead of waiting an extra turn. Closes a
  silent-loss class for tool completions, MCP progress
  notifications, and trigger-driven nudges that arrived mid-
  compaction.

### Fixed

- **`wait_async` cross-thread notify race.** The per-session
  `agent_inbox::wait_async` waiter was checking `pending_count`
  *before* creating its `tokio::sync::Notify::notified()` snapshot.
  A producer thread that completed its entire `push` (entry append +
  `notify_waiters`) between those two steps left the new `Notified`
  with a counter snapshot already at the post-increment value, so the
  await would park indefinitely even though an entry was sitting in
  the queue. The waiter now captures the `Notified` first and re-
  checks `pending_count` against it: any push that completed before
  the snapshot is visible via the entry count, and any push that
  completes after the snapshot triggers the `Notified`.
- **ACP now exposes upstream `session/close` (#1725).** The stdio and
  WebSocket ACP adapters advertise `sessionCapabilities.close`, route
  `session/close` through session cleanup and active-prompt cancellation,
  and keep `session/stop` as a one-release deprecated alias with a warning.
  Generated TypeScript and Swift protocol bindings expose `session/close`
  as the canonical method and mark `session/stop` deprecated.

## v0.8.22

### Fixed

- **`.githooks/` cover the same incremental-cache corruption #1707
  fixed in the release scripts.** `release_gate.sh` and
  `release_ship.sh` export `CARGO_INCREMENTAL=0` for the prepare
  phase, but `.githooks/pre-commit` and `.githooks/pre-push` run
  cargo from fresh subprocesses the export does not reach — committing
  the "Release vX.Y.Z" change still hit `cached cgu ... should have an
  object file, but doesn't` against the stale incremental dir from the
  audit. A new helper in `lib.sh`
  (`hook_disable_cargo_incremental_if_release_bump`) inspects the
  staged/push diff for a `^version =` change in `Cargo.toml` or
  `crates/*/Cargo.toml`; on a real workspace bump it exports
  `CARGO_INCREMENTAL=0` and purges `target/debug/incremental` for the
  hook run only. Day-to-day commits keep incremental cache enabled.
- **`release_ship.sh --prepare` auto-renames `## Unreleased` →
  `## vX.Y.Z` instead of erroring after the audit.** The accumulator
  convention between releases is to drop new bullets under
  `## Unreleased`; the strict heading check in
  `require_changelog_top_matches` rejected that exact shape and only
  surfaced after burning ~7 min of audit wall time. The check now
  promotes a top `## Unreleased` heading to `## v$expected` in place
  (subsequent `git add -u` in the staging step picks the rename up).
  Mismatched `## vX.Y.Z` headings still error with the same hint.
- **Lint: for-loop iteration bindings no longer trigger
  `undefined-function` false positives (#1704).** `for entry in
  callables { entry(arg) }` is now valid — the iteration binding is
  tracked as a callable local. Affects only the linter; runtime
  behavior is unchanged.

### Added

- **Bytecode cache + `harn precompile` (#1710).** `harn run` now persists
  compiled bytecode under `$HARN_CACHE_DIR` (defaulting to
  `~/.cache/harn/bytecode`) and reloads it whenever the entry source and
  every transitively-imported user file are unchanged. Header-stable
  format records magic, schema version, harn version, source hash, and
  import-graph hash; mismatches transparently fall back to recompile.
  `harn precompile <path>` precomputes artifacts for shipping
  pre-compiled `.harnbc` files adjacent to source — the loader picks
  those up before consulting the shared cache. Cold-start cost on small
  pipelines collapses from full parse+typecheck+compile to a single
  `fopen` + `bincode::deserialize`. Gates the burin-code thin-rust
  cutover at burin-code#814. See `docs/perf/bytecode-cache.md`.
- **`secret_store` hostlib capability (#1714).** Adds
  `hostlib_secret_store_{get,set,delete,list}` to `harn-hostlib`, a
  generic per-application credential primitive any Harn-hosted
  application can compose. Picks an Apple Keychain backend on
  macOS/iOS, a Credential Manager backend on Windows, and a
  `0o600`-permissioned JSON file at
  `$XDG_CONFIG_HOME/<account>/credentials.json` everywhere else.
  `HARN_SECRET_STORE_BACKEND=file` forces the file backend on every OS
  for sandboxed CI and eval harnesses. The file-backend path layout is
  byte-compatible with burin-code's existing credentials file so
  existing deployments migrate without data movement, and JSON schemas
  under `crates/harn-hostlib/schemas/secret_store/` keep `harn check`
  preflight covering the new builtins. Docs at
  `docs/src/hostlib/secret_store.md`.
- **ACP `session/set_config_option(configId="model")` (#1721).**
  Generalized the config-option dispatch table from a hardcoded
  `mode`-only branch to a registry covering `mode` and `model`.
  Pinning a model writes a per-session selector that the LLM resolver
  (`vm_resolve_model` / `vm_resolve_provider`) honours for subsequent
  prompts without touching the transcript, tool-call audit log, or
  memory context. Per-call `model:` options still win, and clearing
  the pin (`value: "@inherit"`) reverts to the ambient default. Closes
  the editor `/model` workaround that hinted "takes effect after
  `/clear`" — the new wire path matches the mid-session model swap
  Crush / OpenCode / Codex already ship.
- **Cerebras provider (#1705).** Added a first-class `cerebras` provider
  routed through the OpenAI-compatible stack with a `provider_family =
  "openai"` capability inheritance row. Catalog ships canonical
  `gpt-oss-120b`, `llama-3.3-70b`, and `qwen-3-coder-480b` model entries
  (wire IDs match Cerebras's `/v1/models`); selectors of the form
  `cerebras/<model>` route to the provider and normalize to the bare
  wire ID. `scripts/smoke_cerebras.harn` is the latency-floor smoke for
  the NL-binder substrate work tracked under #1696/#1698 — empirical
  warm-state p50 of ~150ms from a residential network, p99 ~230ms.
- **Tool-call telemetry: standardized spans + built-in Langfuse / OTel /
  stderr / noop sinks (#1704).** `with_telemetry` (in
  `std/llm/tool_middleware`) now builds a standardized tool-call span
  for every dispatch — including timing, args hash, executor, layered
  child spans, and `dispatched` / `result_returned` /
  `scope_violation` events — and fans it out to one or more sinks.
  The new `std/llm/tool_telemetry` module exposes
  `tool_call_span(call, result, start_ms, end_ms, extras?)` plus
  `langfuse_sink`, `otel_sink`, `stderr_sink`, `noop_sink`, and
  `resolve_telemetry_sink` for custom middleware. `with_telemetry`
  accepts a callable, a built-in name string, or a config dict
  (`{sink: "langfuse", project: "..."}` /
  `{sinks: [...]}` for fan-out). The Langfuse sink POSTs to
  `${LANGFUSE_BASE_URL}/api/public/ingestion` using the standard
  `LANGFUSE_PUBLIC_KEY` / `LANGFUSE_SECRET_KEY` env shape, with
  per-span `on_error` opt-in. Captain presets renamed
  `telemetry_sink` → `telemetry` to take the new config shape
  directly. Schema reference: `docs/src/observability/tool-call-spans.md`.

### Removed

- **`prefer_prefill_done` model field.** Dropped the vestigial flag from
  `ModelDef`, the `model_info(...)` builtin dict, the generated provider
  catalog (Rust struct + JSON schema + JSON / TS / Swift artifacts), and
  the two `providers.toml` rows (`gemma-4-e2b-it`, `gemma-4-e4b-it`)
  that set it. The flag's only consumer in burin-code now gates on the
  per-model `supports_assistant_prefill` capability added in #1665
  (burin-labs/burin-code#787), so the field carried no behavior.

## v0.8.21

### Added

- **Portable `harn.context_artifact.v1` envelope in `std/context`
  (#1687).** Host-neutral envelope carries kind / scope / language /
  applicability / freshness / confidence / provenance / source-hash /
  token / redaction metadata, plus ranking, dedupe, merge, and
  budget/select helpers. Provider-capability-aware rendering covers
  Markdown / XML / plain / compact shapes, and a Burin digest adapter
  preserves `.burin/context-digests` compatibility. Docs and
  conformance coverage land alongside the envelope so downstream
  IDE/eval hosts can pin against a stable shape.
- **Prompt context-quality gates for `harn eval prompt` (#1682).**
  `harn eval prompt` now accepts repeatable `--context-fixture <json>`
  inputs. Each fixture case supplies candidate repository artifacts plus
  `assemble_context` options and expectations for selected artifact ids,
  stale/noisy rejection, token budget adherence, and logical-section
  envelopes across the fleet. JSON/HTML/terminal output includes a
  `context_eval` report with per-case scores and per-model section
  shapes, giving Burin CI/eval dashboards a deterministic gate for
  context-engineering changes before live prompt runs.
- **Context maintenance hook recipes (#1681).** Session hooks now include
  `post_turn`, and `std/context/maintenance` provides the portable
  `harn.context_maintenance.job_receipt.v1` shape for non-blocking
  `context.refresh` / `context.crystallize` jobs. The new docs and
  `examples/triggers/context-maintenance` package cover file-edited,
  session-idle, pre-compact, post-turn, and session-end scheduling,
  plus deterministic replay include/skip behavior.

### Changed

- **LLM provider/model catalog defaults now live in an embedded
  `crates/harn-vm/src/llm/providers.toml` (#1692).** Roughly 1,100
  lines of Rust literals in `default_config()` collapse into a single
  TOML asset that deserializes through the same `ProvidersConfig`
  schema already used by `HARN_PROVIDERS_CONFIG`,
  `~/.config/harn/providers.toml`, `harn.toml [llm]`, and package
  manifests — one source of truth. The catalog refresh registers
  `claude-sonnet-4-5/4-6/4-7`, `claude-opus-4-6/4-7`,
  `qwen/qwen3-coder`, `deepseek/deepseek-v3.2`,
  `moonshotai/kimi-k2.6`, `openai/gpt-oss-120b`, and the
  OpenRouter-routed `google/gemini-2.5-flash`; flags
  `claude-sonnet-4-20250514` (sunset 2026-06-15),
  `claude-sonnet-4-5` (2026-05-15), `claude-opus-4` (2026-06-15),
  `claude-opus-4-1`, `gpt-4o` (API sunset 2026-02-17), and
  `gpt-4-turbo` as deprecated with replacement notes; and bumps the
  `frontier` tier alias from `claude-sonnet-4-20250514` to
  `claude-sonnet-4-6`. Brittle per-value assertions are replaced by
  invariant tests (`llm_config::tests::embedded_*`) that verify the
  TOML parses, every deprecated model carries a `deprecation_note`,
  every alias/model targets a known provider, every `qc_default`
  resolves, pricing rates stay non-negative, and the
  `frontier`/`mid`/`small` tier aliases all resolve to active catalog
  entries.

### Fixed

- **Release binary artifact publishing is more robust (#1691).** The
  macOS release binary job now retries transient Apple notary
  polling/upload failures, the complete expected release asset set is
  validated before checksumming/uploading, and `publish-release` no
  longer creates a visible GitHub release before binary assets are
  ready — `build-release-binaries` owns release notes and assets so
  the GitHub release flips public only once binaries are attached.
- **Release `--prepare` no longer corrupts cargo incremental cache
  (#1707).** Audit lanes populate `target/debug/incremental/` keyed to
  the pre-bump crate hashes; once `bump_version` rewrites
  `Cargo.toml`, the workspace rebuilds with fresh hashes and the
  stale incremental directory leaves dangling `.o` references that
  abort `make gen-protocol-artifacts` mid-build. `release_gate.sh
  cmd_prepare` and `release_ship.sh prepare_here` now export
  `CARGO_INCREMENTAL=0` for the prepare phase (covering both the
  inner cargo invocations and the fresh shell that runs
  `regenerate_derived_files`). The Ctrl-C first-signal message is
  also expanded to "[harn] signal received, interrupting VM (give it
  a moment to unwind in-flight async ops; Ctrl-C again to
  force-exit)..." so operators don't reach for a second Ctrl-C
  during the brief cancel-grace window.

### Internal

- **Long Harn modules split into focused files (#1690).** Trigger
  event core types, payloads, catalog, normalization, utilities, and
  tests now live under `triggers/event/`. The LLM façade is split
  into call execution, agent host primitives, and
  stream/mock/trace-builtin glue while preserving existing
  re-exports. `harn connect` is split into OAuth, status, store,
  callback, GitHub, Linear, workspace, and test modules, and
  reflected OAuth callback HTML is now escaped on the way out.

## v0.8.20

### Fixed

- **`if`-without-`else` expression value matches its static type.** The
  VM compiler emitted the falsy-branch `Pop; Nil` cleanup at the end of
  the truthy branch too, so `let x = if true { 42 }` left `nil` on the
  stack instead of `42`. The truthy path now jumps past the cleanup
  unconditionally. The type checker is also corrected to infer the
  expression as `T | nil` (rather than just `T`) when there is no
  `else` arm, so the compile-time type now agrees with the runtime
  value — `let x: int = if cond { 1 }` is rejected, and `int?` /
  `?? default` are the supported recoveries.
- **`type_of(x)` narrows through parameterised types.** The flow
  narrower only recognised `Named("list")` / `Named("dict")` /
  `Named("closure")` etc. as members of a union, so
  `list<int> | int` refused to narrow on `type_of(x) == "list"` and
  the falsy branch retained the original union. `narrow_to_single` and
  `remove_from_union` now treat `List(_)`, `DictType` / `Shape`,
  `FnType`, `Iter(_)`, `Generator(_)`, `Stream(_)`, and literal
  refinements as the corresponding runtime kind. `generator`, `stream`,
  and `iter` are added to the known-typeof set, and a single
  non-union variable also participates in the narrowing.
- **Closure return types are inferred in a scope with their params.**
  `{ x: int -> x + 1 }` previously failed return-type inference because
  the body referenced `x`, which was not bound when the closure
  literal was probed for its `fn(...)` shape. The closure then
  collapsed to the opaque `closure` type, which was compatible with
  any `fn(...)` slot, so `let g: fn(int) -> string = { x: int -> x + 1 }`
  silently type-checked. The inference call now happens inside a
  child scope populated with the declared param types.
- **`else if` chains survive `harn fmt` inside expressions.** Only the
  top-level statement formatter unwound a nested `IfElse` in an
  `else` body back into `else if`. The expression-position formatters
  (closure bodies, `let x = if … else if …`, function-call arguments)
  did not, so each `else if` round-tripped through `fmt` as
  `else { if … }` and gained an indent level. The expression formatter
  now matches the statement formatter.
- **`match` with a wildcard arm counts as a definite exit.** When every
  arm of a `match` ends in `return` / `throw` / `break` / `continue`
  and one arm is an unguarded `_ -> { ... }`, the lint and type
  checker now treat code after the `match` as unreachable. Previously
  `match x { _ -> { return 0 } }` did not satisfy the exit-detection
  predicate, so trailing statements escaped the dead-code lint and the
  type checker refused to refine the surrounding flow as `never`.
- **`session_idle` lifecycle hook now fires.** `register_session_hook("session_idle", …)`
  was registrable but had no firing path, so handlers never ran. The
  daemon-mode agent loop now fires it once per `wake_interval_ms` wait
  with `{session, iteration, wake_interval_ms, consolidate_on_idle}`,
  matching the placement documented in `docs/src/extensibility/hooks.md`.
  Conformance coverage at `conformance/tests/hooks_session/session_idle.harn`.
- **LLM provider behavior dispatch now uses capabilities.** Rust call
  sites that previously branched on provider strings for Anthropic-style
  messages, native tool schemas, image URL support, reasoning request
  shapes, and file-upload transports now read `provider_capabilities`.
  Bedrock-hosted Claude and custom proxy routes can opt into the same
  behavior through the capability matrix instead of pretending to be the
  canonical provider name.
- **Trailing comments on body statements are preserved.** `harn fmt`
  previously dropped same-line comments that followed a statement inside
  any block body (`fn`/`pipeline`/`if`/`else`/`while`/`for`/`try`/`catch`/
  `finally`/`match` arms), shoving them to the end of the surrounding
  scope or, worst-case, to end-of-file. The formatter now splices the
  trailing `// …` or `/* … */` back onto the source line where it was
  written. Idempotent across repeated formatting passes.
- **Multi-line ternary expressions parse correctly.** Wrapping a ternary
  with `?` or `:` at either the end of the previous line or the start of
  the next line is now accepted in all three layouts (`cond ?\n a : b`,
  `cond\n ? a\n : b`, `cond ? a\n : b`). Previously the parser
  misclassified `?` at the end of a line as a postfix-try operator,
  producing confusing `expected separator (\`;\` or newline), found :`
  errors.
- **Arity errors pluralize correctly.** "Builtin function 'len' expects 1
  argument, got 0" instead of "expects 1 arguments". The runtime
  `ArityMismatch` similarly drops the awkward `argument(s)` rendering in
  favor of conditional `argument`/`arguments`.
- **Statement spans no longer absorb trailing blank lines.** `let`/`var`
  bindings, assignments, and `if`/`try`-`catch` statements whose body is
  followed by blank lines and standalone comments previously reported a
  `span.end_line` that walked past those blank lines. The formatter's
  trailing-comment splicer was then attaching the standalone comments
  onto the closing brace. A new `last_non_newline_span()` parser helper
  keeps node end-spans pinned to the last significant token.

### Added

- **`template.render` transcript events for variant resolution (#1668).**
  Every `render()` / `render_prompt()` call performed under an
  LLM-aware frame now emits a `template.render` event into the run's
  `llm_transcript.jsonl`. The event captures the resolved `llm` snapshot
  (`provider`, `model`, `family`, `capabilities`), a per-`{{ if }}`
  /`{{ elif }}` /`{{ section }}` branch trace with line + column
  anchors, the template URI + content hash, and the rendered byte count.
  The portal surfaces a new "Variant resolution" panel showing which
  capability branches fired for the active model, so an on-call engineer
  can answer "which prompt did this model actually see?" without
  re-running the script. Renders outside any LLM frame (doc-gen, CI)
  emit no event. The branch trace is deterministic across repeated
  renders of the same template + bindings, which is what makes replay
  reproducible.
- **`template-provider-identity-branch` lint rule (#1669).** `harn lint`
  now also walks `.harn.prompt` (and bare `.prompt`) files and warns
  when a template branches on `llm.provider`, `llm.model`, or
  `llm.family` directly. Vendor-identity strings are the trap every
  prompt-variant framework before #1663 fell into — the diagnostic
  carries a per-comparison capability-flag replacement suggestion
  (e.g. `provider == "anthropic"` →
  `llm.capabilities.prefers_xml_scaffolding`).
- **`template-variant-explosion` lint rule (#1669).** Companion rule
  that warns when a single `.harn.prompt` carries more than `N`
  capability-aware conditionals — combinatorial explosion + drift
  between materializations is the #1 failure mode for prompt-variant
  systems per the literature. Default threshold is 3, configurable via
  `[lint] template_variant_branch_threshold = N` in `harn.toml`.
- **`harn eval prompt <file> --fleet <models>` (#1670).** New CLI
  subcommand that renders a single `.harn.prompt` template against a
  fleet of models so authors can validate that capability-adapted
  envelopes stay equivalent across the provider mix. Three modes:
  `--mode render` (the default) pushes a per-model
  `LlmRenderContext` and resolves the template — no LLM calls,
  byte-deterministic across runs. `--mode run` extends render by
  invoking each model and collecting outputs; unauthenticated providers
  are skipped with a warning (override with `--fail-on-unauthorized`).
  `--mode judge` adds a final LLM-as-judge call (default
  `claude-opus-4-7`, override with `--judge-template` /
  `--judge-model`). Output via `--output terminal|json|html` and
  optionally redirected with `-o <path>`. Fleets accept ad-hoc
  comma-separated selectors (`alias` or `provider:model`) or named
  groups declared as `[eval.fleets.<name>]` in `harn.toml` via
  `--fleet-name`. Run / judge modes synthesize a thin Harn driver and
  reuse the existing `llm_call` pipeline so credentials, the provider
  catalog, and `HARN_LLM_PROVIDER=mock` work exactly as in `harn run`.
- **Provider format-preference capabilities (#1665).**
  `provider_capabilities(...)`, `llm_model_info(...)`, the provider
  catalog, and the generated provider matrix now expose prompt-format
  preferences separately from transport feature gates: XML vs. Markdown
  section scaffolding, native JSON vs. delimited/XML-tagged output
  preference, assistant prefill support, developer-role instructions,
  XML text-tool rendering, and thinking-block style. The shipped matrix
  is populated for Anthropic, OpenAI/Azure, Gemini/Vertex, Qwen/local
  routes, Ollama, and Bedrock so prompt renderers can adapt without
  provider-name dispatch. This bumps the generated provider catalog
  schema to v2 because model entries now require `format_preferences`.
- **Capability-aware prompt templates: auto-injected `llm` scope (#1664).**
  When `render()` / `render_prompt()` / `render_string()` is invoked
  from inside an LLM-aware frame (`llm_call`, `default_llm_caller`,
  `agent_loop`), the engine now auto-injects a reserved
  `llm = {provider, model, family, capabilities: {...}}` binding so a
  single logical `.harn.prompt` can branch on
  `llm.capabilities.native_tools` and friends without manual option
  threading. Bare `render()` calls outside any LLM frame leave
  `llm = nil`; templates guard with `{{ if llm }}` for the CI / doc-gen
  path. User bindings that already supply an `llm` key win for
  back-compat and trigger a one-shot `template.llm_scope` lint warning.
  This is the load-bearing primitive for the capability-adaptive prompt-
  rendering epic (#1663) — sibling tickets layer logical sections,
  format-preference capabilities, and cross-model eval on top.
- **Logical prompt-template sections (#1666).** `.harn.prompt` assets
  now support `{{ section "task" }} ... {{ endsection }}` and the
  built-in logical roles `task`, `examples`, `output_format`, `tools`,
  `thinking_scaffold`, `chain_of_thought`, and `system_framing`.
  Rendering is driven by provider capability flags such as
  `prefers_xml_scaffolding`, `prefers_markdown_scaffolding`,
  `structured_output_mode`, `prefers_xml_tools`, and
  `thinking_block_style`, so a single logical prompt can materialize as
  Claude XML, GPT-style Markdown/native JSON, local text-tool
  instructions, or a generic fallback without provider-string branches.
- **One-line installer + signed binaries.** `install.sh` (served from
  `harnlang.com/install.sh`) now detects the host OS/CPU, resolves the
  latest GitHub release tag, downloads the matching pre-built archive,
  and verifies it against the new `SHA256SUMS` release asset before
  extracting `harn`, `harn-dap`, and `harn-lsp`. The destination is
  picked from `$HARN_INSTALL_DIR`, `$XDG_BIN_DIR`, `$HOME/bin`,
  `$HOME/.local/bin`, or `$HOME/.harn/bin` in that order — so the
  default install path no longer requires `sudo`. macOS binaries
  remain signed + notarized via the existing release workflow, and
  Linux/Windows tarballs/zips are now hash-pinned end-to-end.
- **`harn upgrade` subcommand.** Resolves the latest GitHub release
  (or a specific `--version vX.Y.Z`), downloads the matching archive,
  verifies it against `SHA256SUMS`, and atomically replaces the
  currently running binary in-place. Use `--check` to print the
  resolved versions without downloading, `--force` to reinstall the
  same version, and `--no-verify` only as an escape hatch.
- **`harn demo` subcommand for cold-start aha (#1650).** A new
  top-level CLI command ships three bundled, fully-offline scenarios
  that demonstrate the Harn moat — `merge-captain` (persona-supervised
  PR triage with structured receipts), `review-captain` (HITL
  clarifying-question loop), and `provider-race` (latency-aware
  provider racing with cost attribution). Each scenario embeds a
  `.harn` script and a JSONL `--llm-mock` tape into the binary, so
  `harn demo merge-captain` finishes in well under a second on a fresh
  install with zero API keys, zero network, and zero project setup.
  `harn demo --list` prints the menu; `harn demo --json` emits a
  machine-readable summary; `harn demo <id> --live` opts into the
  configured provider. The README `Quick Start` now leads with the
  demo. CI exercises every scenario via the new `demo_cli`
  integration test, which catches drift between the script and tape.
- **First-class `routing_policy` primitive (#1649).** `routing_policy({...})`
  builds a reusable handle that drives a chain of providers with
  failover, latency-aware racing, and per-call / session budget caps.
  Pipe it through `llm_call(... routing: policy ...)`:

  ```harn,ignore
  let policy = routing_policy({
    chain: [
      {provider: "anthropic", model: "claude-opus-4-20250514"},
      {provider: "openai",    model: "gpt-4o"},
      {provider: "ollama",    model: "llama4:70b"},
    ],
    failover: {on_status: [429, 500, 502, 503, 504], max_attempts: 3},
    latency: {race_after_ms: 5000},
    budget:  {per_call_usd: 0.5, on_exceed: "abort"},
    observe: {emit_event: "billing.routing_decision"},
  })

  let result = llm_call("Summarize this PR.", nil, {routing: policy})
  ```

  Each chain attempt rides on the result envelope's `routing.attempts`
  block and emits structured tape events
  (`<dispatch>.{decision,attempt,race_started,race_won,race_lost,budget_exceeded,exhausted}`)
  so transcripts and replay can attribute every outcome to a specific
  link. Migrates the previous BYO composition of `with_routing` +
  `with_retry` + `with_fallback` to a single typed primitive while
  reusing the catalog pricing in `std/llm/economics` for budget
  enforcement. See `docs/llm/harn-quickref.md` and
  `docs/src/stdlib/llm-handlers.md`.
- **Production OS sandbox profiles (#1647).** `CapabilityPolicy`
  now carries a `sandbox_profile: SandboxProfile` field
  (`unrestricted` / `worktree` / `os_hardened` / `wasi`). The
  default `worktree` matches today's behavior — workspace-root path
  enforcement plus best-effort OS confinement. Pipelines that run
  untrusted code opt into `os_hardened`, which makes the OS
  confinement *required* (Linux Landlock + seccomp, macOS
  sandbox-exec, Windows AppContainer + Job Object): spawns return
  `tool_rejected` if the platform mechanism is unavailable,
  regardless of `HARN_HANDLER_SANDBOX`. The `host_call("process",
  "exec", ...)` host call accepts `sandbox_profile: "..."` to
  promote a single spawn without rewriting the surrounding policy.
  Three Harn builtins surface backend identity for diagnostics:
  `sandbox_active_backend()`, `sandbox_backend_available()`, and
  `sandbox_active_profile()`. The 1.2k-line `stdlib/sandbox.rs`
  was split into a `stdlib/sandbox/` directory with one
  `SandboxBackend` impl per OS, so callers no longer branch on
  `cfg(target_os)` and adding a backend is one new file. Full
  per-platform capability → kernel-knob mapping table lives in
  [docs/src/sandboxing.md](docs/src/sandboxing.md).
- **Session-level lifecycle hooks (#1648).** `register_session_hook(event, handler)`
  registers callbacks for the whole-session turn lifecycle:
  `session_start`, `session_end`, `user_prompt_submit`, `pre_compact`,
  `post_compact`, `permission_asked`, `permission_replied`,
  `file_edited`, `session_error`, and `session_idle`. The
  `user_prompt_submit` and `pre_compact` events accept
  `{block: true, reason}` to veto the operation;
  `permission_asked` additionally accepts
  `{decision: "allow"|"deny"|"ask", reason}` to short-circuit the
  dynamic permission policy. Hook invocations are captured on the
  active session transcript under `hook_call`, `hook_returned`, and
  `hook_vetoed` event kinds, so replay tooling reproduces the same
  control flow. `write_file`, `append_file`, and `write_file_bytes`
  queue `file_edited` notifications automatically (drained at each
  turn boundary); call `notify_file_edited(path, metadata?)` to emit
  one explicitly. Manifest `[[hooks]]` entries accept the new event
  names. See `docs/src/extensibility/hooks.md`.

## v0.8.19

### Changed

- **`provider_capabilities` exposes text-tool capability.** The dict
  returned by `provider_capabilities(...)` and `llm_model_info(...)` now
  includes `text_tool_wire_format_supported` (mirrors the rule field) and a
  derived `tools` boolean (`native_tools || text_tool_wire_format_supported`)
  that matches the VM's own tool-capability gate at
  `effective_model_capability_tags`. Scripts gating on "can this model do
  tools" should prefer `caps.tools` — gating on `caps.native_tools` alone
  was rejecting tool-capable local routes like
  `ollama/qwen3.6:35b-a3b-coding-nvfp4` that use Harn's text wire format.
- **`tool_choice` accepted on text-format tool routes.** The VM's
  capability gate rejected `tool_choice` whenever `caps.native_tools`
  was false, even for routes whose text-format tool wire is fully
  supported. Agent scripts that pass `tool_choice: "none"` to suppress
  further tool calls (e.g. release_harn's finalization turn) hit
  "option `tool_choice` is not supported by ..." mid-loop. The gate
  now permits tool_choice whenever either `native_tools` or
  `text_tool_wire_format_supported` is true, mirroring the relaxed
  `tools` gate.
- **`agent_preset` derives `tool_format` from the model capability matrix.**
  Tool-using preset kinds (`audit`, `repair`, `merge_captain`,
  `review_captain`, …) used to hardcode `tool_format: "native"`, which the
  VM rejected with "option `tools` is not supported by ..." for tool-capable
  text-format routes (e.g. `ollama/qwen3.6:35b-a3b-coding-nvfp4`) before
  `native_tool_fallback` could engage. The preset now calls
  `llm_resolve_model` to resolve `tool_format` from the capability matrix,
  picking `"native"` for native-capable routes and `"text"` for text-only
  routes. Callers that explicitly set `tool_format` still win.

## v0.8.18

### Added

- **Agent progress reporting (#1628).** `std/agent/progress` adds
  `agent_progress(...)`, which emits a structured `progress_reported`
  agent event for the current session with optional narration, task-list
  entries, replacement semantics, and metadata. `agent_loop` keeps the
  model-facing tool off by default; opt in with `progress_tool: true` or
  customize `{name, description, system_prompt_nudge}`. ACP extension
  notifications and protocol artifacts now include the new event kind.
- **ACP progress plan routing (#1627).** ACP renders `agent_progress`
  entries as canonical `sessionUpdate: "plan"` updates so clients replace
  their structured task list directly, while message-only progress reports
  continue to use Harn's vendor `progress` extension with `phase:
  "narration"`.
- **A2A progress status updates (#1629).** A2A task streams now translate
  `progress_reported` events into non-terminal `working` status updates
  with agent message text. Entry lists render as deterministic markdown
  checklists, while terminal completion status emission remains unchanged.
- **Provider telemetry envelope (#1614).** `llm_call` results now carry a
  normalized `provider_telemetry` block that preserves server-side timings
  local runtimes already report and represents missing fields explicitly.
  Ollama's `/api/chat` NDJSON and `/api/generate` raw stream lift
  `total_duration`, `load_duration`, `prompt_eval_duration`, and
  `eval_duration` (rounded to milliseconds) plus the prompt-eval / eval
  token counts and the daemon-resolved model id. OpenAI-compatible
  responses expose `prompt_tokens` / `completion_tokens` even when no
  durations are reported, and llama.cpp's `usage.timings` extension is
  promoted to `llamacpp_timings` with the prefill / decode breakdown.
  Anthropic Messages responses preserve the response id alongside usage
  counts. Every call also records `client_wall_ms` so end-to-end latency
  can be decomposed from the network and streaming overhead the
  server-side counters omit. The same envelope flows through the
  structured-output wrapper so eval aggregators get the data without
  provider-specific decoders. The `OllamaPsModel` parser is shared with
  `harn local list` / `status`, picking up `context_length` from the
  `/api/ps` payload where the daemon reports it.
- **`harn provider probe <provider>` command (#1614).** Machine-readable
  one-shot snapshot of provider readiness plus the loaded-model state
  Ollama surfaces via `/api/ps` (size, VRAM, expiry, context window).
  JSON output by default for eval pipelines; pair with `harn local
  status --json` to inspect lifecycle state across every local runtime.
- **Done-judge cadence policy (#1631).** `agent_loop` now accepts
  `done_judge.cadence` with `every`, `when`, `max_invocations`, and
  `min_iterations_before_first` gates so completion checks can run on
  explicit cadence or stall signals instead of every completion
  candidate. Accepted judge calls still use transcript projection
  isolation: judge prompts and structured responses emit events but do
  not mutate the worker session transcript.
- **Stall-gated done judge (#1632).** `agent_loop` now runs a configured
  `done_judge.cadence.when: "stalled"` judge when stall diagnostics emit
  `agent_loop_stall_warning`. A `done` verdict stops the loop with
  `stalled_done_judge` before repeating the stalled tool call; a
  `continue` verdict falls back to the existing stall feedback path.
  Judge events now include an optional `trigger`, set to `"stalled"` for
  stall-fired calls.

## v0.8.17

### Added

- **`harn local` runtime lifecycle commands (#1599).** New top-level
  command surface for managing local LLM runtimes: `harn local list`
  surveys every local provider Harn knows about (Ollama, llama.cpp, MLX,
  generic OpenAI-compatible, vLLM) with base URL, port, served models,
  loaded models, and memory footprint; `harn local status` shows the
  currently-active selection plus a brief per-provider summary;
  `harn local switch <alias>` warms a model on its provider, evicts
  conflicting local runtimes (drains Ollama's loaded set, stops tracked
  llama.cpp/MLX PIDs), re-checks `/v1/models` after the first success,
  and persists the selection to `<state>/local/selection.json`;
  `harn local stop [--all]` unloads loaded Ollama models via
  `keep_alive=0` and SIGTERMs Harn-managed llama.cpp/MLX PIDs.
  Defaults for `--ctx` and `--keep-alive` come from a machine profile
  derived from RAM and accelerator presence so a 48 GB Apple Silicon
  laptop picks a wider context window than a low-RAM Linux box.

## v0.8.16

### Added

- **`HarnWorkerStatus` protocol artifact (#1604).** `dump-protocol-artifacts`
  now emits the canonical worker-status wire vocabulary
  (`running`, `progressed`, `awaiting_input`, `completed`, `failed`,
  `cancelled`) as a typed enum in every binding (TypeScript, Swift,
  Python, Go) and a `harnWorkerStatuses` field on the manifest. Hosts
  consuming `worker_lineage[].status` or `worker_update` payloads can now
  vendor this enum instead of maintaining hand-rolled synonym buckets
  for statuses Harn never emits (e.g. `succeeded`, `error`, `timed_out`).
  `WorkerEvent::ALL` exposes the same set programmatically.

## v0.8.15

### Added

- **Current local coding model aliases and setup guidance (#1601).** Catalog
  now ships aliases for Qwen3.6 35B A3B Coding (NVFP4), Gemma 4 26B MoE,
  Devstral Small 2 24B, llama.cpp Qwen3.6 GGUF, and Apple MLX Qwen3.6 27B.
  Local Qwen3.6/Devstral routes default to the text-tool contract (with
  `*-native` siblings kept for targeted parser A/B tests). `harn quickstart`,
  `harn models recommend`, and `harn models install` prefer/present the new
  local coding models and now print concrete setup plans for llama.cpp,
  MLX, and generic local OpenAI-compatible servers. Documented local
  provider setup and known MLX server flag variance.
- **`/api/ps` observability in model-info (#1600).** `harn model-info
  <alias> --verify` (and `--warm`) now hits `/api/ps`, surfaces the
  loaded runner's `context_length`, and reports the `num_ctx` Harn would
  request as `expected.num_ctx`. When the two diverge, `context_drift`
  explains the load-time semantics and points at `ollama stop <model>`
  for recovery. Also documented in `docs/src/llm/providers.md`.

### Fixed

- **Ollama warmup now applies `num_ctx` (#1600).** The `ollama_readiness`
  warmup path previously sent `/api/generate` with only `keep_alive` set,
  which caused Ollama to load the runner at the model's declared maximum
  context (e.g. `262144` for `qwen3.6:35b-a3b-coding-nvfp4`) regardless of
  `HARN_OLLAMA_NUM_CTX` or the catalog's `runtime_context_window`. The
  warmup body now derives from `OllamaRuntimeSettings::warmup_body`, so
  the runner is loaded at the same `num_ctx` chat/completion paths
  request.

## v0.8.14

### Added

- **Local/A2A dispatch replay proof (#1586).** Trigger dispatch now records
  trust-boundary and remote identity metadata on action-graph nodes, outbox
  success records, and TrustGraph terminal records. A2A dispatch also
  classifies cleartext/authority denials, timeouts, incompatible response
  schemas, and remote `rejected` task states distinctly, with a new
  `examples/triggers/local-a2a-dispatch` reference flow and replay-oracle
  fixture proving local and A2A handlers can preserve logical output.
- **Replay determinism benchmark harness (#1585).** Added
  `harn bench replay`, a canonical replay benchmark suite, OpenCode-inspired
  JSONL trace adapter, Harn Cloud ingest-shaped JSON report, replay benchmark
  schema, and CI workflow template. The report includes replay-fidelity,
  permission-preservation, tool-call drift, transcript drift, observed-cost,
  and first-divergence triage metrics.
- **Generated provider catalog contract (#1584).** Added `harn providers`
  commands to refresh, validate, and export the provider/model catalog,
  checked-in JSON/JSON Schema artifacts, and TypeScript/Swift downstream
  bindings for aliases, variants, pricing, deprecation metadata, capabilities,
  and provider endpoint/auth metadata.

## v0.8.13

### Changed (breaking)

- **`daemon_spawn` config canonical field names.** Dropped the deprecated
  aliases `prompt` (use `task`), `state_dir` (use `persist_path`), and
  `queue_capacity` (use `event_queue_capacity`). The previous fallback
  chain that accepted either name has been removed; calls using the old
  names now error with `daemon_spawn: config must include \`task\`` (etc.).
  Migration: rename the keys in your `daemon_spawn({...})` calls. No
  in-repo callers used the deprecated aliases; this only affects external
  scripts.
- **Strict option-bag parsing for remaining stdlib agent/IO configs (#1568).**
  `spawn_agent`, `sub_agent_run`'s normalized host request, worker carry
  policy bags, worker snapshots, and `std/io.read_line` now reject unknown
  closed-schema option keys instead of silently ignoring typos.

### Added

- **Reusable ACP VM baselines (#1563).** File-backed `session/prompt` turns now
  cache a prepared VM baseline separately from the compiled bytecode cache, so
  repeated turns reuse stable stdlib/project/source setup while instantiating a
  clean execution VM for prompt globals, output, bridge state, tasks,
  cancellation, sync primitives, and shared state. ACP profile rollups now
  include a `vm_setup` bucket for prompt-turn setup diagnostics.
- **Profiling for ACP and benchmark workflows (#1559).** `harn serve acp` now
  supports `--profile`, `--profile-json`, and `--trace`, with
  `HARN_PROFILE`, `HARN_PROFILE_JSON`, and `HARN_TRACE` aliases. ACP profile
  JSON is appended as one NDJSON object per prompt turn. `harn bench` now shares
  the profile flags, reports p50/p95/stddev wall-time stats, and can write a
  JSON benchmark report with per-iteration profile rollups.
- **Typed stdlib option-bag + return-value records.** Started declaring
  structural `Ty::Shape` aliases for the stdlib's well-known dicts so
  literal-call sites get autocomplete and typo detection at parse time
  (matches the precedent set by `schema_recover`'s envelope). Applied to:
  `daemon_spawn` config + return (`DAEMON_CONFIG`, `DAEMON_SUMMARY`),
  `__io_read_line` (`READ_LINE_OPTIONS`, `IO_RESULT_ENVELOPE`),
  `__tui_page` (`PAGER_OPTIONS`), `__signal_on_interrupt`
  (`SIGNAL_HANDLER_OPTIONS`), `agent_session_seed_from_jsonl`
  (`AGENT_SESSION_SEED_OPTS`), `agent_session_compact`
  (`AGENT_SESSION_COMPACT_OPTS`), `agent_session_ancestry`
  (`SESSION_ANCESTRY` return), and `spawn_agent` (`WORKER_SUMMARY`
  return), `spawn_agent` config (`AGENT_SPAWN_CONFIG`), and
  `sub_agent_run` / `sub_agent_request` options (`SUB_AGENT_OPTIONS`).
  Literal worker/sub-agent option bags now reject unknown keys at `harn check`
  time, so misspelled worker/sub-agent options fail before runtime.
  `LLM_CALL_OPTIONS` remains defined and documented, with full
  enforcement deferred until internal callers (`agent_loop`, `agent_turn`)
  converge on the documented shape.

### Changed

- **Optional shape fields treat explicit `nil` as "missing".** Both the
  runtime type-check (`crates/harn-vm/src/typecheck.rs`) and the static
  type-checker (`crates/harn-parser/src/typechecker/inference/subtyping.rs`)
  now accept `{flag: nil}` against an optional shape field whose declared
  type doesn't include `nil`. Previously an explicit `nil` value had to
  match the declared field type; with this change, optional fields are
  treated uniformly as "the caller may omit the key OR pass `nil`",
  matching user intuition for option bags. Required fields still reject
  `nil` unless the declared type permits it.

### Internal

- **Shared option-bag parsing helpers** at
  `crates/harn-vm/src/stdlib/options.rs`. `OptionsParser` plus
  `dict_arg`/`required_string_arg`/`optional_string_arg`/`required_int_arg`
  let stdlib builtins extract option dicts without rebuilding
  `dict.get().map().filter().ok_or_else()` chains, with strict
  unknown-key detection available for closed-schema bags. `agents_daemon`
  and `agent_sessions` consolidated onto these helpers; the previous
  duplicate `require_dict_arg` / `required_string_arg` / `optional_string`
  / `opt_string` / `arg_string_required` / `arg_int_required` helpers
  with mismatched signatures and error kinds were removed.
- **Shared structural-record `Ty::Shape` aliases** at
  `crates/harn-parser/src/builtin_signatures/signatures/shapes.rs` for
  agent / sub-agent / daemon / LLM / IO / TUI / signal option bags and
  worker / daemon / session return contracts. See the "Added" entry above
  for which slots are wired up today.
- **Refactored `spawn_agent_builtin` and `sub_agent_run_builtin`** to share
  a single `finalize_and_run_worker` helper that handles
  persistence-snapshot, registry insertion, task spawn, optional terminal
  wait, and summary emission. Replaces ~14 duplicated lines of
  WorkerState lifecycle.

## v0.8.12

### Added

- **Cooperative process signal handlers (#1547).** Added `std/signal`
  with `on_interrupt`, `off_interrupt`, `interrupted`, and
  `with_interrupt`. `harn run` now routes SIGINT/SIGTERM/SIGHUP into VM
  interrupt dispatch with a graceful timeout, second-signal hard exit,
  and LIFO handler stacking, so long-running harnesses (chat loops,
  agent supervisors, polling jobs) can opt into clean shutdown without
  losing the panic-on-stuck escape hatch.
- **`std/dashboard/jobs` event envelopes (#1508).** Added
  `std/dashboard/jobs` with typed dashboard job event envelopes,
  validation, EventLog emission receipts, dedupe, and ordered Jobs view
  reduction. Local and cloud fixture streams cover runs, approvals,
  receipts, replay fixtures, and DLQ actions, and the host-facing
  dashboard contract is documented and wired through stdlib
  registration / conformance.
- **`std/tui::select_from` picker (#1544).** Added
  `select_from(items, opts?)` to `std/tui` so harness scripts stop
  hand-rolling fzf detection plus numbered-menu fallbacks. The picker
  auto-detects `fzf` then `gum choose` at runtime, falls back to a
  numbered `read_line` menu (which `mock_stdin` can drive in tests),
  and always returns a stable `{ok, value, status}` envelope. Supports
  `multi: true`, `default_index`, `cancel_value`, per-item `display`
  and `preview` callbacks, and
  `prefer_external: "auto" | "fzf" | "gum" | "none"` for forcing a
  backend.
- **JSONL-seeded agent sessions (#1546).** Added
  `agent_session_seed_from_jsonl(path, opts?)` to create a new
  first-class session from replayable `llm_transcript.jsonl` sidecars.
  The importer supports exact prompt-visible `message` events and older
  full request snapshots, with `truncate_to_last`, `drop_tool_calls`,
  `rename_session`, `validate`, `provider`, and `model` options. Agent
  loop sidecars now emit redacted `message` records as turns are
  appended, so future run artifacts can be lifted directly into a new
  prefix-cache-friendly session.
- **Structured `std/io` terminal input (#1542).** Added `std/io` with
  `is_tty(fd?)`, structured `read_line({prompt, timeout_ms, trim, echo, raw})`,
  `read_password`, and `write_stderr`. `read_line` returns explicit
  `ok`/`eof`/`timeout`/`interrupt`/`error` statuses, writes prompts to stderr
  with ANSI preserved, supports sub-second Unix timeouts without shelling out
  to `bash`, and disables terminal echo for password-style prompts.
- **Terminal pager helpers (#1543).** Added `std/tui` with `page(...)` for
  long text/markdown artifacts, `terminal_width()`, `rule()`, and `clear()`.
  `page` prints directly when stdout is not interactive, `no_pager` is set, or
  `$PAGER=cat`; otherwise it uses `$PAGER` with `less -R -F -X` defaults and
  falls back to print output when the pager binary is unavailable.
- **Interactive agent chat loop primitive (#1545).** Added `std/agent/chat`
  with `agent_chat_loop(...)`, `agent_chat_route_input(...)`, and
  `agent_chat_wait_for_user_tools(...)` so harnesses can share one
  operator-message / model-turn loop instead of reimplementing session
  management, slash-command routing, and `wait_for_user` turn stops. A
  post-turn callback can now set a typed `stop_reason` when it stops the
  loop, and `agent_session_close(id, status?)` records an
  `agent_session_closed` event before evicting the session so timeout and
  interruption closes are visible in event logs.

### Changed

- **`command_run` shell mode defaults to the host shell (#1549).**
  Shell-mode invocations of `command_run` (and downstream tools like
  `std/command::command_step`) no longer error when the caller omits
  both `shell` and `shell_id`. The host now resolves
  `discover_shells().default_shell_id` automatically (`bash` on macOS /
  Linux, `pwsh` on Windows). Malformed explicit `shell` / `shell_id`
  fields still error rather than silently falling back. Simplifies the
  common `mode: "shell"` call site to a single `command` field.
- **Wasmtime bumped to 44.0.1 (#1539).** Internal: harn-vm's optional
  Wasmtime/WASI stack moved from 29 to 44.0.1 with `wasmtime-wasi` on
  the current p1 feature. Sync WASIp1 modules now run on a dedicated
  host thread so testbench WASI subprocesses work from inside Harn's
  Tokio runtime while preserving mock clock and overlay state.

### Fixed

- **Escaped `${name}` in triple-quoted strings now passes through
  literally (#1548).** Multi-line / triple-quoted strings used to
  interpolate `${...}` even when escaped as `\${...}`, breaking any
  harness that wanted to emit literal shell variable references inside
  a multi-line script string. The lexer now treats `\${...}` as a
  literal `${...}` (matching single-line behavior) and preserves
  non-interpolation escapes like `\$PATH` unchanged.
- **Tail-call inside `for` loops leaked iterator state across the
  caller.** `return f(...)` from inside a `for x in xs { ... }` body
  is tail-call-optimized, but the new frame was capturing the current
  iterator depth instead of the popped frame's depth. The caller's
  outer iterator then iterated against the inner loop's leaked state,
  producing extra phantom items. The tail-called frame now inherits
  the popped frame's `saved_iterator_depth` and the iterator stack is
  truncated to match.

## v0.8.11

### Added

- **MCP Apps-compatible UI resource envelopes (#1505).** Added
  `std/ui_resource` for declaring `ui://` resources, tool-meta blocks, text
  and structured fallbacks, host capability negotiation, and the JSON-RPC
  `tools/call` / `context/update` message envelopes hosts proxy through
  `postMessage`. Resources validate through `std/artifact/web` (with
  `allow_host_bridge: true` by default since MCP Apps use `parent.postMessage`
  by contract) and ship with a CSP/sandbox dict that `ui_resource_csp_header`
  and `ui_resource_sandbox_attr` project into host headers/attrs.
  `ui_tool_result` always carries a non-empty text fallback so plain-text
  hosts still get useful output, and `ui_select_for_host` picks
  `ui_resource`, `structured_fallback`, or `text_fallback` based on host
  capability advertisements (MCP, OpenAI Apps SDK, or bare flags).
  `std/artifact/web` gained an `allow_host_bridge` option so the same safety
  rules apply to general artifacts while UI resources can keep their
  postMessage-based bridge. Examples ship under `examples/ui_resource/` for
  a dashboard widget and a multi-step review form.
- **Provider catalog refresh workflow and drift report (#1497).** Added
  `scripts/update_provider_catalog.harn`, a Harn-native workflow that
  collects model availability, pricing, and capability signals from
  provider sources, normalizes them with explicit provenance, and emits
  a markdown drift report + a TOML candidate patch under
  `.harn-runs/provider_catalog/`. The workflow never mutates shipped
  catalogs. New pure-logic library `scripts/provider_catalog_refresh.harn`
  handles observation merging (provider-owned beats aggregator-owned),
  drift detection against `llm_provider_catalog()`, and report
  rendering. Source adapters live in
  `scripts/provider_catalog_sources.harn` with three canonical shapes:
  `html_pricing_table_adapter` for keyless HTML pricing pages,
  `json_api_adapter` for public JSON APIs, and `key_required_adapter`
  that records `status: "skipped"` (instead of silently looking like a
  removal) when no API key is present. The bundled `--check` mode
  replays bundled HTTP fixtures and verifies the rendered report and
  candidate patch against committed goldens under
  `scripts/provider_catalog_fixtures/`. New
  `make check-provider-catalog-drift` runs the gate from `make all`.
  Coverage: 14 `@test` pipelines in
  `scripts/tests/provider_catalog_refresh_test.harn` cover the
  pure-logic helpers and every adapter shape (HTML, JSON, skipped
  key-required, key-required-with-key, conflicting observation,
  context-window-only change, fetch error).
- **Transparent profile bulletin schema and tool contract (#1506).** Added
  `std/personas/bulletins` with the `harn.profile_bulletin.v1` envelope for
  durable persona/user/project/team facts. Proposals carry stable id, scope,
  scope key, subject, persona, context key, assertion, status, confidence,
  structured evidence, source provenance, privacy/sync flags, and optional
  TTL/review timestamps. `bulletin_emit` always writes status `proposed` to
  `personas.bulletins.proposed`; hosts emit `harn.profile_bulletin_decision.v1`
  envelopes (`accept`, `reject`, `expire`, `supersede`) on
  `personas.bulletins.decisions` so review history is replayable.
  `bulletin_apply_decisions`, `bulletin_partition`, and `bulletin_active` keep
  prompt context derived from accepted, non-expired bulletins; the
  `bulletin_render_for_prompt` renderer visibly separates accepted facts from
  pending proposals so models cannot confuse them.
- **`std/edit` hardening for agent-authored mutations (#1499).** Made
  structural matching conservative by default: a structural fallback
  match now requires at least 3 non-blank needle lines and both the
  first and last anchor lines must carry a distinctive 4+ character
  alphanumeric token, so bare braces or short idents can no longer drive
  a wrong patch. The looser pre-existing behavior is opt-in via
  `structural_require_anchored_lines: "either" | "none"`,
  `structural_min_nonblank_lines: N`, and `structural_anchor_chars: N`.
  Expanded `lazy_placeholder` detection to cover `// ... rest`,
  `// TODO: implement|fill|add|complete`, `# ... rest`, `/* ... */`,
  `pass # ...`, and "unchanged" / "omitted for brevity" phrases. Added
  three new helpers — `edit_strip_line_number_prefixes` (strip leading
  `<spaces>N<space><pipe><space>` prefixes when ≥60% of non-empty lines
  carry them, with a matching `strip_line_numbers: true` option on
  `edit_apply_old_new_patch`), `edit_explain_whitespace_difference`
  (diagnose tabs-vs-spaces, base indent, or blank-line drift between a
  needle and the matched span), and `edit_check_lazy_truncation`
  (catch whole-file rewrites that shrank a file below `min_keep_pct`
  while still containing lazy placeholders). Successful line/structural
  matches surface a `whitespace_explanation` field, and `candidate_contexts`
  on ambiguous matches now uses a uniform `{start_line, end_line, snippet}`
  shape across exact, line, and structural modes. Also fixed a
  duplicate-candidate bug where structural matching would start from a
  leading blank line and report the same logical region twice. Locked in
  by an expanded `conformance/tests/stdlib/edit_patch.harn`.

### Fixed

- **Refresh `workflow-authoring-quickstart` pinned `graph_digest`.** The
  pinned bundle digest in `docs/src/workflow-authoring-quickstart.md`
  and `scripts/check_docs_workflow_quickstart.sh` had drifted from the
  current canonical encoding, breaking `make all`. Repinned to the
  current value so the doc's copy-paste path stays accurate.

## v0.8.10

### Added

- **`std/llm/economics` first-class cost helpers and explicit unknown
  pricing.** New stdlib module exposes `pricing_for`,
  `estimate_call_cost`, `estimate_session_cost`, `compare_model_costs`,
  `cache_break_even`, `volume_cost`, and `format_usd`, backed by new
  Rust builtins `llm_pricing`, `llm_compare_costs`, and `llm_format_usd`.
  The previous `model_pricing_per_million` table in
  `crates/harn-vm/src/llm/cost.rs` — annotated "as of early 2026" and
  prone to silent drift — has been removed; its canonical Anthropic /
  OpenAI / Gemini / Mistral entries are now exact-id catalog rows in
  `crates/harn-vm/src/llm_config.rs::canonical_priced_models()`,
  editable in one place under `git blame`. Pricing for unknown
  commercial models now surfaces as `pricing_known: false` /
  `cost_usd: nil` instead of silently coercing to $0; only providers
  explicitly configured with $0 rates (ollama, local, llamacpp, mlx,
  vllm, tgi) report cost=$0 with pricing_known=true. The CLI cost
  explainer (`harn run --explain-cost`) already routed through
  `llm_pricing_per_1k`, so its "unpriced" cell now reflects this
  stricter truth. New conformance tests under
  `conformance/tests/integration/llm_economics_*.harn` cover the
  helpers; existing `llm_cost`, budget, and routing behavior is
  unchanged for catalog-known models.
- **Safe HTML/CSS/JS artifact patching helpers (#1504).** Added `std/edit`
  for pure old/new text patches with exact, line-normalized, and structural
  matching, deterministic changed-region metadata, hashes, ambiguity
  diagnostics, and guardrails for no-op, whitespace-only, lazy-placeholder, and
  excessive-growth edits. Added `std/artifact/web` on top for small generated
  HTML artifacts: fragment extraction, validated patch application, text
  fallbacks, machine-readable reports for host approval UIs, and checks for
  obvious network calls, external resources, host bridge calls, dangerous
  navigation, inline secrets, and broken core tags.

## v0.8.9

### Fixed

- **Workflow stages now thread `iteration_budget` through to per-stage
  `agent_loop`.** `ModelPolicy` carried a `max_iterations` field but
  not `iteration_budget`, so any workflow node policy authored through
  `agent_preset(...)` / `agent_budget(...)` silently lost its adaptive
  budget at the serde boundary in `workflow_commit`. The per-stage
  `agent_loop` then fell back to the static `max_iterations: 16`
  default and never emitted `loop_control_decision` events, defeating
  the v0.7.61 adaptive-budget contract for any harness that authored
  per-stage policies through the std presets. The fix adds a typed
  `iteration_budget: Option<serde_json::Value>` field to `ModelPolicy`
  in `crates/harn-vm/src/orchestration/policy/types.rs`, and threads
  the same key through the `loop_option_keys` list in
  `std/workflow/options.harn` so `workflow_stage_agent_options` passes
  it to the per-stage `agent_loop`. `agent_loop`'s
  `__normalize_iteration_budget` already prefers `iteration_budget`
  over `max_iterations` when both are present, so callers that supply
  both still get the adaptive shape and `loop_control_decision`
  visibility. Locked in by
  `conformance/tests/agents/workflow_stage_options_iteration_budget.harn`.

## v0.8.8

### Added

- **Persona supervision-runtime hooks (#1480).** `harn_vm::personas`
  now exposes a `PersonaSupervisionSink` mirroring the harn-cloud
  multiplexed `persona/update` feed for the runtime-sourced
  `update_kind`s. The runtime emits typed `queue_position` events on
  every enqueue/drain transition, `receipt` events at the tail of
  every `run_for_envelope` call, and `repair_worker_status` events
  through a new `report_repair_worker_status` entry point that is
  append-only and idempotent on `(repair_worker_id, lifecycle)`. A
  typed `restore_persona_checkpoint` entry point acks supervision-API
  restore requests by emitting a `Checkpoint(action: RestoreAcked)`
  envelope carrying the resume coordinates the runtime actually
  resumed from (`run_id`, `lease_id`, `last_run_ms`,
  `queued_work_keys`). All emissions accept `now_ms` from the caller,
  so wiring through `harn_clock::RecordedClock` produces
  byte-identical sink sequences across replays — locked in by a new
  determinism test in `crates/harn-vm/src/personas.rs`.

## v0.8.7

### Fixed

- **Preserve `->` arrow on zero-arg closure literals in `harn fmt`.**
  The formatter was silently rewriting `{ -> expr }` into `{ expr }`,
  which changes meaning from "closure that returns expr" to "block
  expression evaluated immediately." Any call site like `f()` then
  failed at runtime because `f` was no longer callable. The fix emits
  `->` for closure literals regardless of whether the parameter list
  is empty, in both the inline and multi-line code paths. Locked in
  by new round-trip tests in `crates/harn-fmt` and the conformance
  fixture
  `conformance/tests/fmt/zero_arg_closure_arrow_preserved.harn`.

### Added

- **`std/cache` levels up to a cost-moat substrate (#1473).** The
  module now ships three composable backends —
  `mem_cache(opts?)` (thread-local LRU; per-VM, does not survive
  `harn run`), `fs_cache(path, opts?)` (content-addressed JSON files
  with atomic writes), and `sqlite_cache(path, opts?)` (one sqlite
  file, many namespaces, TTL + LRU eviction inside the put
  transaction). All three accept the same `namespace`/`ttl`/
  `ttl_seconds`/`max_entries` options and surface a uniform
  `{hit, value?, backend, namespace}` envelope. A generic
  `with_cache(key, compute, options?)` helper wraps any 0-arity
  closure (with `with_cache_envelope` for inline hit/miss/metrics),
  and `cache_stats(options?)` / `cache_stats_reset(options?)` expose
  in-process hit/miss counters per namespace. Cache TTL now reads
  from the unified clock (`mock_time` / `advance_time` honored), so
  testbench fixtures can reproduce expiry windows without wall-clock
  flakiness. When `options.session_id` is set, `with_cache` emits
  `cache_hit` / `cache_miss` events on the agent event tape (now
  registered as first-class `AgentEvent` variants so ACP and closure
  subscribers see them) with cost-moat receipts
  (`model_calls_avoided`, `tokens_saved` from `usage.input_tokens` +
  `usage.output_tokens`, `latency_saved_ms` from `latency_ms`) for the
  persona value ledger (harn-cloud#58) and crystallization receipts.
  Both the `with_cache(next, opts?)` middleware form and the direct
  `with_cache(prompt, system, opts)` form in `std/llm/handlers` cache
  and emit receipts; they share one cache key. Docs at
  [`docs/src/stdlib/cache.md`](docs/src/stdlib/cache.md).
- **`std/agent/presets` captain preset pack and active
  `require_successful_tools` gate.** Closes #1472. The four captain
  presets — `merge_captain`, `review_captain`, `oncall_captain`,
  `release_captain` — package the persona-shaped service contracts
  adopters were re-deriving by hand: a long-enough adaptive iteration
  budget, the cheap-default + frontier-escalation `with_routing`
  scaffolding, and the per-captain governance defaults (`merge_captain`
  ships a default `with_consent` that auto-approves tools annotated
  `read`/`search`/`fetch`/`think` and denies everything else;
  `oncall_captain` ships a default `with_rate_limit({max_calls: 50})`
  cap; `release_captain` accepts an opt-in `with_dry_run` shadow-run
  layer). Each captain composes the
  [`std/llm/handlers`](docs/src/stdlib/llm-handlers.md) handler stack and
  the [`std/llm/tool_middleware`](docs/src/stdlib/tool-middleware.md)
  middleware stack from caller-supplied `audit_sink` /
  `telemetry_sink` / `consent` / `rate_limit` / `handoff_sink` /
  `cheap_caller` / `frontier_caller` / `escalate_predicate` /
  `logging_sink` options, so persona manifests (#460) reference
  contracts by name instead of duplicating wiring. Each captain ships
  a corresponding `*_captain_agent(prompt, options?)` wrapper that
  spreads the preset directly into `agent_loop`.
  `require_successful_tools` is now an active gate that returns a
  structured `error_envelope` (consumable by harn-cloud receipts) and
  emits a `tool_gap` friction event onto the friction sink so
  context-pack suggestions and dashboards pick it up alongside
  natively-emitted events. The agent_loop tool envelope now carries
  `annotations` (verbatim from the tool entry) so `with_consent` and
  other middleware can policy-gate by tool kind without crawling
  `schema.annotations`. Five new conformance tests under
  `conformance/tests/agents/`: `agent_preset_captains`,
  `agent_preset_captain_cheap_route`,
  `agent_loop_require_successful_tools_friction`, and the existing
  `agent_loop_adaptive_budget` plus a new
  `agent_loop_adaptive_budget_mock_time` (proves the budget extension
  decisions don't quietly observe wall-clock time). Documented in
  `docs/src/llm/agent_loop.md`. (#1472)

## v0.8.6

### Added

- **Annotation tape format for `harn test-bench`.** A new sidecar
  format (`<tape>.annotations.jsonl`) attaches structured human
  judgment — `correct`, `incorrect`, `alternative`, `note`, `marker`,
  `mute`, `hypothesis`, `friction`, `crystallize_here` — to specific
  events on a recorded testbench tape. Annotations are versioned JSONL
  with a header carrying an optional `tape_content_hash` so the
  validator catches tape edits that invalidate `event_id` references.
  `friction` annotations adapt directly to `FrictionEvent` records so
  they feed `orchestration::generate_context_pack_suggestions`
  alongside natively-emitted events; `crystallize_here` annotations
  surface as `CrystallizeAnchor` records ready for the
  candidate-detection pipeline. Three new CLI surfaces:
  `harn test-bench replay --annotations <path>` (validates + surfaces
  annotations inline during replay), `harn test-bench
  validate-annotations` (structured JSON report, exits `2` on any
  problem), and `harn test-bench export-annotations --kind ... --format
  jsonl|friction` (filter + re-emit for downstream pipelines). The
  conformance runner picks up `<name>.annotations.jsonl` sidecars
  automatically and gates the test on validation success; covered by
  `conformance/tests/testbench/testbench_replay_fidelity.annotations.jsonl`
  plus four `harn-cli` integration tests under
  `tests/test_bench_cli.rs::annotations`. Documented in
  `docs/src/dev/annotation-tape-format.md`. (#1474)
- **`std/llm/handlers` cost-moat handlers — `with_repair`,
  `with_coerce`, `with_timeout`, `with_routing`.** Closes the persona
  platform's "cost moat substrate" gap from #1470: the four handlers
  let any caller compose cheap-model-by-default with frontier
  escalation (`with_routing`), per-call deadlines that honor the
  unified clock and forward `timeout_ms` to providers
  (`with_timeout`), one-shot schema-validation repair with a
  deterministic corrective nudge (`with_repair`), and uniform
  case-insensitive key normalization on the success envelope
  (`with_coerce`). `safe_structured_call`'s structured-output dance is
  now documented as the canonical preset equivalent to
  `compose([with_coerce({})])(structured_caller)`, with judge-friendly
  defaults baked in. Each handler ships with a conformance gate under
  `conformance/tests/integration/llm_handlers_with_*` that exercises
  the success and edge-case paths without reaching for a live
  provider; the persona-shaped composition is documented in
  `docs/src/stdlib/llm-handlers.md` and the quickref. (#1470)
- **Partial-application form for `(next, opts)` handler middleware.**
  `with_retry`, `with_logging`, `with_budget`, `with_circuit_breaker`,
  `with_repair`, `with_coerce`, and `with_timeout` now accept either
  `with_X(next, opts)` (direct) or `with_X(opts)` (curried — returns a
  wrapper for `compose`). The auto-currying makes the canonical
  `compose([with_logging({...}), with_retry({...})])(base)` pattern
  documented in the quickref and llm-handlers reference actually work
  — previously the opts dict was silently bound as `next` and the
  composition failed at first invocation. A new
  `llm_handlers_persona_compose` conformance fixture pins the
  end-to-end persona-shaped chain (routing + budget + logging) so the
  cost-moat substrate stays exercised. (#1470)
- **Testbench clock-leak audit.** New `crate::clock_mock::leak_audit`
  shim records every `capability_id` that observes the OS wall or
  monotonic clock while a testbench mock is installed.
  `TestbenchSession::finalize` now returns a `clock_leaks` vector
  alongside `fs_diff` / `recorded_subprocesses` / `tape`; `harn
  test-bench run` prints `[testbench] clock leak: <capability>
  (count=N)` to stderr for each unique entry; scripts introspect via
  the new `testbench_clock_leaks()` builtin. Three demonstration call
  sites (`stdlib/date_iso`, `host_call/process.exec.{started_at,
  ended_at}`) are migrated to the audited helpers, and the new
  `conformance/tests/testbench/testbench_clock_leak_warns.harn` case
  pins the contract. Closes the testbench-mode epic (#1438) and its
  fidelity-audit follow-up (#1466).
- **String escape sequences `\r` and `\0`.** Double-quoted string
  literals now recognize `\r` (carriage return) and `\0` (NUL) in
  addition to the existing `\n`, `\t`, `\\`, `\"`, `\$`. Triple-quoted
  multiline strings remain literal. The formatter emits `\r` / `\0`
  back out for those bytes when re-rendering string literals so
  round-trips stay readable. Spec updated in
  `spec/HARN_SPEC.md#single-line-strings`.
- **Repo automation scripts ported to Harn.** Five one-off
  Python/Bash scripts (`check_xfail_count`, `compare_generated_text`,
  `sync_language_spec`, `detect_bump_type`, `check_openapi_snapshot`)
  are now `.harn` files under `scripts/`, with companion
  `@test`-pipeline coverage in `scripts/tests/`. Wired into a new
  `make test-harn-scripts` target and into `make all`; the existing
  pre-commit `harn fmt` / `harn lint` stanza now covers `scripts/`
  too, so these scripts stay typecheck-clean and warning-free as
  they evolve.
- **`harn test-bench run --runtime des` (single-threaded testbench).**
  Opt-in flag that swaps the testbench's default multi-thread Tokio
  runtime for a `current_thread` runtime so all VM tasks, I/O
  callbacks, and timer firings share one OS thread. Eliminates
  inter-thread scheduling races and yields bit-exact event tapes for
  scripts that stay within the DES-safe primitive set. Adds three
  `testbench_*` conformance fixtures under
  `conformance/tests/testbench/` plus three `des_runtime_*` Rust
  integration tests covering paused-sleep, byte-identical concurrent
  settle, and parity with `--runtime paused-tokio`. The constraint
  surface, benchmark methodology, and the decision to ship as opt-in
  (rather than the default) are written up in
  `docs/src/dev/des-mode.md`. (#1444)
- **`harn fmt` normalizes trailing commas, and the `trailing-comma`
  rule is now AST-aware.** Previously the rule walked tokens with a
  brace-vs-block heuristic that fired stray "missing trailing comma"
  diagnostics on `eval_pack { cases: ... summarize { ... } }` and
  multi-field `struct Foo { bar: int fn name() ... }`, sometimes
  inserting commas that broke the source. Trailing-comma detection
  now runs against the parsed AST so it only fires on confirmed
  comma-separated bracket pairs (call args, list/dict/struct
  literals, selective imports), and `harn fmt` applies the same
  normalization as a post-pass — `xs.filter(...).to_list()`-style
  multi-line constructs round-trip cleanly without relying on `harn
  lint --fix`. The traversal lives in `harn_parser::visit` and is
  reused by the linter.
- **Iter-style sinks (`.to_list()`, `.to_set()`, `.to_dict()`) work
  on already-realized collections.** `xs.filter(...).to_list()` used
  to silently return `nil` because list method dispatch had no
  handler for `to_list` and fell through a silent-`Nil` catch-all.
  List/dict/set now expose `to_list` / `to_set` / `to_dict` as the
  obvious identity/conversion (dict→list yields key/value entry
  dicts to match `.entries()`), so the same chain works whether
  `filter` returned an `iter` or a `list`.
- **Method dispatch raises a clear error on unknown methods instead
  of returning `nil`.** Every per-type method handler (list, dict,
  set, string, generator, stream, iter, plus the outer dispatch)
  used to bottom out in `Ok(VmValue::Nil)`, which made typos in
  method names fail silently far from the call site. Unknown method
  calls now throw a runtime error of the form ``list has no method
  `whatever``` so issues surface at the offending line. *Breaking*
  for any script that relied on the silent-`nil` fallthrough.
- **`cargo build` self-heals git hook configuration.** The
  `harn-cli` build script now resets `core.hooksPath` to `.githooks`
  whenever it drifts (the most common cause of CI surprises being
  pre-commit / pre-push hooks that *would* have caught a `harn fmt
  --check` regression but never ran because the user's hook config
  pointed elsewhere). Set `HARN_DISABLE_AUTO_HOOK_SETUP=1` to opt
  out. Skipped automatically when not building inside the Harn
  source tree.
- **`websocket_connect` retries transient TCP errors during
  handshake.** Connection-reset / broken-pipe / unexpected-EOF on
  the very first attempt — typically caused by the OS recycling an
  ephemeral port still in TIME_WAIT — now self-heal with up to two
  retries within the user's `timeout_ms` budget. Permanent failures
  (DNS, refused, TLS, protocol) bypass the retry path and surface
  immediately.
- **Package manager maturity: provenance, outdated, audit, and
  generated-artifact contract checks.** Bumped `harn.lock` to version 2
  with top-level `generator_version` / `protocol_artifact_version` and
  per-entry `package_version`, `harn_compat`, `manifest_digest`, and
  `[package.registry]` provenance for entries originally added through
  the registry index (the manifest now also stores `registry`,
  `registry_name`, `registry_version` so the source survives round-trips).
  Three new subcommands give downstream automation a single Harn binary
  to call: `harn package outdated` (with `--remote` for branch-tracking
  git deps), `harn package audit` (stable JSON `code` per finding for
  yanked-version, content-hash, manifest-digest, and harn-range
  violations), and `harn package artifacts manifest|check` for vendoring
  and drift-checking the protocol-artifact manifest. `harn install` and
  `harn update` now accept `--json`. v1 lockfiles migrate transparently
  on the next install. Documented the cross-repo bump workflow in
  `docs/src/package-authoring.md`. (#1428)
- **Workflow patch proposals + safe Harn function tools.** Agents can now
  author bounded, auditable changes to a workflow bundle through a flat
  patch JSON (`insert_node`, `add_edge`, `upsert_prompt_capsule`,
  `update_node_policy`, `update_bundle_policy`) instead of regenerating
  the whole bundle. Three new `harn workflow patch
  {validate,apply,preview}` subcommands apply the patch to a copy,
  re-run the bundle validator, emit a structural diff, and reject
  anything that widens the parent capability ceiling along *tools*,
  *capabilities*, *side-effect level*, *workspace roots*, *connector
  scopes*, *command gates*, or *autonomy tier*. `harn workflow
  function-tools` enumerates an allowlist of read-only / pure-think
  Harn functions an agent may call from inside the patch loop, each
  carrying an ACP-aligned `ToolAnnotations` block hosts can wire
  straight into a model surface. `harn workflow nested-ceiling`
  exposes the same scanner used internally so hosts can reject nested
  Harn invocations (workflow bundles, Harn scripts, Burin harness
  manifests) that would widen the active execution policy. The
  workflow-authoring skill pack and `docs/src/workflow-bundles.md`
  cover the contract; a new
  `crates/harn-cli/tests/workflow_patch_cli.rs` gate exercises the
  validate / apply / preview / function-tools / nested-ceiling surfaces
  end-to-end. (#1423)
- **Python and Go protocol bindings.** Extended
  `harn dump-protocol-artifacts` to emit a stdlib-only Python 3.9+ module
  (`spec/protocol-artifacts/python/harn_protocol.py`) and a Go package
  (`spec/protocol-artifacts/go/harnprotocol/`) mirroring the existing
  TypeScript and Swift surface: ACP session updates, JSON-RPC envelopes,
  Harn tool lifecycle metadata, A2A task structures, and MCP tool/resource
  records. `manifest.json` now exposes a `bindings` block with per-language
  stability and module-path metadata so downstream consumers (Burin Code,
  Harn Cloud, Python integrators, Go workers) can detect generator/runtime
  mismatch without bespoke compatibility checks. A new `make check-bindings`
  target round-trips a checked-in JSON fixture
  (`spec/protocol-artifacts/fixtures/round_trip.json`) through both bindings
  and runs in CI to catch wire-vocabulary drift before downstream consumers
  see it. (#1429)
- **Workflow-authoring skill pack and small-model evals.** Added
  `examples/skill-packs/workflow-authoring/` with a top-level `SKILL.md`,
  small-model prompting guide, validated PR-monitor and PR-repair recipe
  bundles, eval cases with structural assertions, and an `eval.harn` driver
  that feeds a configurable provider through the validate → preview → run
  pipeline. A new `crates/harn-cli/tests/workflow_authoring_eval.rs`
  regression gate fails CI when a recipe golden or a case's structural
  assertions drift.
- **Workflow authoring quickstart.** New tutorial at
  `docs/src/workflow-authoring-quickstart.md` walks `validate` →
  `preview` → `run` → connector status/setup-plan → supervisor in one
  copy-paste path with no paid credentials. Backed by checked-in
  fixtures (`docs/fixtures/workflow-bundles/quickstart-{minimal,agentic}.bundle.json`,
  `docs/fixtures/connect-demo/`) and a CI gate
  (`make check-docs-workflow-quickstart`) that pins the deterministic
  bundle digest, executed-node sequence, and connector-status shape so
  the snippets cannot drift.
- **Cross-platform release smoke matrix.** Added
  `.github/workflows/release-smoke.yml`, `scripts/release_smoke.sh`,
  and `tests/smoke/` fixtures so every release-relevant PR runs the
  user-visible CLI surface (`--help`, `check`, `fmt --check`,
  `package check`, `--provider-matrix`, `run`, `command_run`,
  no-credentials mock workflow, `harn watch` boot) on macOS, Linux,
  and Windows. Failures surface as
  `::error::release-smoke (<platform>): <capability> failed`
  annotations and the smoke audit lane is now part of
  `release_gate.sh audit`.
- **Platform compatibility docs.** Added
  [docs/src/dev/platform-compatibility.md](docs/src/dev/platform-compatibility.md)
  with a per-capability support matrix and rationale for the
  Windows-deferred features (POSIX-signal drain, `unveil`/`pledge`).
- **Tag-first publish trigger.** `publish-release.yml` now also fires
  on `push: tags: ['v*']`, in addition to the existing `push: main`
  drift trigger. Detect-drift recognizes the tag-push event and sets
  `publish_ref=$tag, drift=true` so the publish job checks out the
  tagged commit (detached) and ships from there. Lets the bump-fleet
  `release_harn.harn` harness push `vX.Y.Z` at a pinned commit BEFORE
  the Release PR merges — what gets shipped to crates.io and the
  GitHub release is anchored to that exact commit, and commits that
  land on `main` between PR-open and merge cannot leak into the
  published artifact. The `push: main` drift trigger remains as the
  legacy/recovery path for releases authored without the harness;
  workflow_dispatch with no drift still recovers from an existing
  tag.

### Fixed

- **Recognize `/** */` doc comments in `harn package check`.** The
  publish-readiness check previously saw only `///` doc comments, so
  the canonical HarnDoc form preferred by the linter would surface
  spurious "no doc comment" warnings on otherwise-documented public
  symbols. Both forms now produce identical `docs` bodies.
- **Typed stdlib option/result shapes.** `std/collections.filter_nil` and
  `pick_keys`, plus `std/json.merge`/`pick`/`omit`, are now generic over the
  value type — a `dict<string, V>` (or homogeneous shape literal) projects back
  to a dict that still carries `V`. Introduced `PickKeysOptions` and the
  workflow `WorkflowAutonomyPolicyConfig`/`WorkflowModelPolicy`/
  `WorkflowStageOptionsConfig`/`WorkflowStageAgentOptions` and connector
  `GitHubConnectorConfig`/`GitHubCallOptions`/`GitHubWaitOptions` shapes so
  the high-traffic agent/workflow and connector paths advertise their
  contract instead of accepting freeform `dict`.
- **Testbench conformance suite.** Added `conformance/tests/testbench/`
  covering the deterministic axes of `harn test-bench`: paused-clock
  sleep, 30 simulated cron days under one `trigger_test_harness` call,
  100 concurrent mocked agents settling deterministically, recorded
  subprocess replay, copy-on-write fs overlay diff, deny-by-default
  network egress, and byte-identical replay fidelity against a checked-in
  event tape. The conformance runner now activates the testbench session
  automatically when sidecar files (`.process-tape.json`, `.fs-overlay/`,
  `.testbench-tape`) are present next to a `.harn` test, and two new
  script-side builtins (`testbench_is_active`, `testbench_fs_diff`)
  expose the active overlay diff to assertions. The eighth case from
  the original issue (a runtime audit warning when a host capability
  bypasses the unified mock clock) is filed against #1466 since it
  depends on a runtime feature that doesn't exist yet. (#1442)

### Changed

- **Type-checker generics.** `dict<string, V>` parameter slots now bind `V`
  from a heterogeneous shape literal (union of field types) so generic
  stdlib helpers preserve element typing through projection.
  Optional shape fields validate the value type when supplied — a
  `{drop_nil?: bool}` parameter rejects `{drop_nil: "yes"}` instead of
  silently accepting it.
- **Cross-module type aliases.** Selectively importing a function (e.g.
  `import { pick_keys } from "std/collections"`) now also pulls every
  exported type alias / struct / enum / interface from the same module
  into scope so call-site contract checks resolve referenced shapes
  instead of seeing phantom `Named("PickKeysOptions")`.
- **Return-type checking scope.** `fn` return-type validation now runs
  against the post-body scope (with narrowing rolled back) so values bound
  by `let`/`var` inside the body resolve correctly when reused in a
  structural return literal.

## v0.8.5

### Added

- **Python and Go protocol bindings.** Extended `harn dump-protocol-artifacts`
  to emit Python and Go bindings mirroring the existing TypeScript and Swift
  surface. The `manifest.json` now exposes a `bindings` block with per-language
  stability and module-path metadata so downstream consumers can detect
  generator/runtime mismatch without bespoke checks. A new `make check-bindings`
  target round-trips a JSON fixture through both bindings to catch wire-vocabulary
  drift. (#1429)
- **Workflow authoring skill pack and evals.** Added `examples/skill-packs/`
  with a top-level `SKILL.md`, small-model prompting guide, PR-monitor and
  PR-repair recipe bundles, and eval cases with structural assertions. A new
  `crates/harn-cli/tests/workflow_authoring_eval.rs` regression gate fails CI
  when a recipe golden or a case's structural assertions drift. (#1435)
- **Workflow authoring quickstart.** New tutorial at `docs/src/workflow-authoring-quickstart.md`
  walks `validate` → `preview` → `run` → connector status/setup-plan → supervisor
  in one copy-paste path with no paid credentials. Backed by checked-in fixtures
  and a CI gate that pins the deterministic bundle digest, executed-node sequence,
  and connector-status shape. (#1436)
- **Cross-platform release smoke matrix.** Added `.github/workflows/release-smoke.yml`,
  `scripts/release_smoke.sh`, and `tests/smoke/` fixtures so every release-relevant
  PR runs the user-visible CLI surface on macOS, Linux, and Windows. Failures
  surface as `::error::release-smoke` annotations in the smoke audit lane. (#1434)
- **Platform compatibility docs.** Added `docs/src/dev/platform-compatibility.md`
  with a per-capability support matrix and rationale for Windows-deferred
  features (POSIX-signal drain, `unveil`/`pledge`). (#1434)
- **Testbench composition primitive.** Wires Harn's deterministic substrate —
  virtual time, mocked LLM, filesystem overlay, recorded subprocess, and a
  deny-by-default network — behind a single `Testbench` handle and a new
  `harn test-bench` CLI surface. Production wires the real implementations;
  tests/demos pick a config and get an audit trail of every host boundary
  crossing. (#1440)
- **Unified redaction policy.** Added a unified redaction policy across
  persistence surfaces. Includes `crates/harn-vm/src/redact/` and `patterns.rs`
  for consistent masking in transcripts, receipts, and event logs. (#1445)
- **Unified clock crate.** Extracted a `harn-clock` crate providing the `Clock`
  trait with `RealClock`, `PausedClock`, and `RecordedClock`. Unifies
  deterministic clocks across VM, stdlib, conformance, and CLI. (#1446, #1449,
  #1450)
- **Tag-first publish trigger.** `publish-release.yml` now fires on `push: tags:
  ['v*']` in addition to `push: main`. Detect-drift recognizes the tag-push
  event and sets `publish_ref=$tag, drift=true` so the publish job checks out
  the tagged commit (detached) and ships from there. This anchors what reaches
  crates.io and the GitHub release to the pinned commit, preventing commits
  landing on `main` between PR-open and merge from leaking. (#1456)

### Changed

- **Type-checker generics.** `dict<string, V>` parameter slots now bind `V`
  from a heterogeneous shape literal (union of field types) so generic stdlib
  helpers preserve element typing through projection. Optional shape fields
  validate the value type when supplied — a `{drop_nil?: bool}` parameter
  rejects `{drop_nil: "yes"}` instead of silently accepting it. (#1448)
- **Cross-module type aliases.** Selectively importing a function now also pulls
  every exported type alias / struct / enum / interface from the same module
  into scope so call-site contract checks resolve referenced shapes. (#1448)
- **Return-type checking scope.** `fn` return-type validation now runs against
  the post-body scope (with narrowing rolled back) so values bound by `let`/`var`
  inside the body resolve correctly when reused in a structural return literal.
  (#1448)
- **Typed stdlib option/result shapes.** `std/collections.filter_nil` and
  `pick_keys`, plus `std/json.merge`/`pick`/`omit`, are now generic over the
  value type — a `dict<string, V>` projects back to a dict that still carries
  `V`. Introduced `PickKeysOptions` and workflow/connector shape types so high-
  traffic paths advertise their contract instead of accepting freeform `dict`.
  (#1448)

### Fixed

- **Recognize `/** */` doc comments in `harn package check`.** The publish-
  readiness check previously saw only `///` doc comments, so the canonical
  HarnDoc form preferred by the linter would surface spurious "no doc comment"
  warnings on otherwise-documented public symbols. Both forms now produce
  identical `docs` bodies. (#1448)
- **Improve diagnostics for shape, property, and stdlib contract failures.**
  Common authoring failures around structured values now surface with actionable,
  span-anchored messages instead of bottoming out as generic nil/property
  runtime errors. Property access on `nil` includes `?.` / nil-guard
  suggestions; `__assert_shape` and `schema_recover` validator append actually-
  present keys to missing-field errors. (#1455)
- **Expand `harn doctor`.** Expanded `harn doctor` for one-command environment
  readiness checks, improving the initial setup and troubleshooting experience.
  (#1437)
- **Add Bitbucket entry to connector parity matrix.** Added Bitbucket entry to
  the connector parity matrix, ensuring parity tracking is up to date. (#1451)

## v0.8.4

### Added

- Preserve structured initial user content in agent sessions so ACP multimodal
  prompt blocks reach the agent transcript.
- Mark Ollama Gemma 4 routes as vision-capable in the Harn capability matrix.
- Export normalized workflow bundle graphs as JSON, including graph nodes/edges,
  node-scoped diagnostics, editable field metadata, and embedded Mermaid
  diagrams.
- Add `harn workflow preview --mermaid` for a debug diagram view of workflow bundles.
- Generate complete Swift protocol contracts from Harn, including ACP
  method/notification enums, MCP/A2A constants, JSON-RPC IDs, JSON values,
  response/error helpers, and executor display labels.

### Changed

- Standardize workflow message handling in the stdlib to support the new
  multimodal session input flow.
- Align generated protocol README with the artifact ownership boundary for
  downstream hosts.

## v0.8.3

### Added

- **Portable workflow bundle CLI.** Added the `harn workflow validate`,
  `harn workflow preview`, and `harn workflow run` commands for JSON workflow
  bundle validation, graph preview, and deterministic local run receipts,
  alongside the v1 bundle contract docs and a GitHub PR monitor fixture.
- **Agent runtime reliability profiles.** Added runtime context-window metadata
  to provider/model catalogs, exposed it through `model_info` and CLI catalog
  dumps, and taught Ollama calls plus stdlib token budgeting to prefer runtime
  context windows when local serving capacity differs from model maximums.

### Changed

- **Agent option forwarding.** Consolidated stdlib agent option forwarding with
  `pick_keys`, so worker and simulated-user helpers carry reasoning policy,
  scale, task, and runtime options without repetitive per-field plumbing.
- **Manual skill routing.** Added a manual skill-matching strategy for callers
  that already select active skills, avoiding redundant catalog prompt/scoring
  while still loading explicitly selected skills.
- **Provider capability profiles.** Added Qwen3-Coder-specific provider rules
  across OpenRouter, Hugging Face, and Together so agent defaults avoid known
  broken thinking-mode behavior.

### Fixed

- **Reasoning disable semantics.** Made explicit `reasoning_effort: "none"`
  disable thinking even when the provider does not support effort levels, and
  reported disabled thinking consistently in agent observation metadata.
- **Agent loop resilience.** Let completion judges fail open on provider errors
  when configured, made transcript auto-compaction honor token thresholds, and
  applied command-policy gating to host-bridged `process.exec` calls.

## v0.8.2

### Added

- **Simulated users for eval harnesses.** Added `std/agent/user` with
  `agentic_user`, deterministic `scripted_user` / `fixture_user`, an `ask_user`
  tool adapter, post-turn callback adapter, reply/LLM-call guardrails, and
  audit events for harnesses that need a model or fixture to substitute for the
  user during agent evals.

### Changed

- Add provider-aware agent reasoning policy (#1406).
- Clean stale Harn dev target dirs (#1405).
- Add simulated user helpers (#1404).
- Harden agent completion and skill routing (#1403).
- Capture agent_loop provider failures (#1399).
- Add streaming-native trigger primitives (#1401).
- Harden persona runtime status (#1400).
- Add agent loop stall diagnostics (#1398).
- Make MCP server context first-class (#1395).
- Add compile-time capability policy invariants (#1393).
- Add workflow crystallization v2 shadow receipts (#1394).
- Add review and on-call persona packages (#1392).
- Reduce VM hot-path allocations (#1391).

## v0.8.1

### Added

- **Orchestrator replay determinism oracle.** A new conformance suite
  under `conformance/replay-oracle/` validates that orchestrator execution is
  deterministic across runs, covering tool calls, DLQ retries, LLM predicates,
  local triggers, and A2A worker paths.
- **Session system prompt composition.** Agents now support composing system
  prompts from multiple parts, with updated stdlib signatures in
  `harn-stdlib` and new helpers in the VM for session configuration and
  transcript management.
- **Compiler optimization pipeline.** The VM now includes an optimization pass
  (e.g., constant folding) with a new `crates/harn-vm/src/compiler/optimizer.rs`
  module and corresponding conformance tests.
- **Protocol conformance matrix.** Expanded fixture coverage for A2A, ACP,
  and MCP protocols, including agent card adapters, security constraints,
  session updates, JSON-RPC error handling, and tool/resource boundaries,
  alongside updated JSON schemas.
- **Generated protocol artifacts for downstream bindings.** Added
  `harn dump-protocol-artifacts`, Make targets for generation and drift checks,
  and checked-in ACP/A2A/MCP schema copies plus TypeScript and Swift bindings
  under `spec/protocol-artifacts/`.
- **Split protocol adapters into focused modules.** A2A, ACP, and MCP adapters
  in `harn-serve` are reorganized into dedicated submodules (auth, events,
  schema, tasks, sessions, transport) with dedicated test suites, improving
  maintainability without changing external behavior.

### Fixed

- **Package and listener test hermeticity.** Made package and listener tests
  hermetic by tightening test support fixtures and lockfile/manifest/registry
  handling in `harn-cli`.
- **Tree-sitter parsing for A2A regex fixtures.** Fixed parsing issues in
  A2A-related conformance test fixtures.
- **Ollama tool argument replay.** Corrected replay behavior for Ollama tool
  arguments in the LLM tool messages module.

### Changed

- **Removed manifest provider static leaks.** Cleaned up static state leaks in
  manifest providers and trigger event handling, improving memory safety in
  `harn-vm` and `harn-cli` package extensions.

## v0.8.0

### Added

- **Composable tool middleware (`std/llm/tool_middleware`).** New parallel
  seam to `llm_caller`: `agent_loop` now accepts a `tool_caller:` closure that
  wraps every tool dispatch (across all executors — `harn`, `host_bridge`,
  `mcp_server`, `provider_native`). Paired with a schema-time decorator
  (`tools_use_middleware(registry, transform)` + `tool_inject_param`) so
  middleware can also augment the tool schemas the model sees. Ships
  `compose_tool_callers([..])`, `default_tool_caller()`, and a bundled
  middleware library: `with_required_reason` (force a `reason` arg on every
  tool call and surface it as a user-facing summary), `with_audit_log`,
  `with_consent`, `with_dry_run`, `with_redaction`, `with_idempotency`,
  `with_rate_limit`, `with_telemetry`, and `with_summary`. Middleware-attached
  audit metadata rides on the dispatch result `audit` field (free-form dict
  aligned with A2A `metadata` / ACP `kind` / OpenAI `summary_text` / OTel
  `gen_ai.tool.description` conventions) and is also fanned out as a new
  `tool_call_audit` AgentEvent for live ACP/A2A consumers. See
  [`docs/src/stdlib/tool-middleware.md`](docs/src/stdlib/tool-middleware.md)
  for the full reference.

- **`llm_caller` seam on `agent_loop`.** New option accepts a closure
  with the canonical shape `fn(call) -> {ok, value | status, error?}`
  that owns each turn's `llm_call(...)`. The loop validates the
  envelope shape, threads `_turn_iteration` through `call.turn`, and
  rejects writes to underscore-prefixed runtime-private keys.
- **`std/llm/handlers`.** Composable middleware: `default_llm_caller`,
  `with_retry`, `with_fallback`, `with_shadow`, `with_prompt_rewrite`,
  `with_logging`, `with_budget`, `with_cache`, `with_circuit_breaker`,
  `compose([...])`. `compose` takes a single list (Harn does not yet
  support user-defined variadics).
- **`std/llm/ensemble`.** `best_of_n`, `self_consistency`,
  `parallel_judge`, `debate`. Cites Wang et al. 2022
  (arxiv:2203.11171) and Du et al. 2023 (arxiv:2305.14325).
- **`std/llm/refine`.** `refine_prompt`, `refine_caller` —
  meta-prompt-based prompt rewriting with a `DIFF:` summary trailer.
  Best-effort session cache (Harn closures capture by value).
- **`std/llm/budget`.** `estimate_text_tokens` (heuristic; not named
  `estimate_tokens` to avoid colliding with the workflow builtin),
  `context_window_for`, `recommend_max_output_tokens`,
  `budget_summary`, `fits_in_context`.
- **`std/llm/defaults`.** `pack_for(opts)` and convenience wrappers
  (`pack_chat`, `pack_agent`, `pack_refine`, `pack_judge`,
  `pack_summarize`, `pack_code`, `pack_json`). Calibrated for
  Anthropic Sonnet/Opus/Haiku 4.x, OpenAI GPT-5/5.5/4o/4.1, Gemini
  2.5 Pro/Flash, Ollama Qwen3/Llama 3.x.
- **`std/llm/safe`.** `safe_call`, `safe_field`, `dict_get_ci`,
  `with_case_insensitive_keys`, `structured_envelope_or_default`,
  `judge_payload`, `verdict_normalize`, `schema_retry_nudge_for`.
- **`std/llm/prompts`.** `system_prelude`, `tool_use_prelude`,
  `structured_output_preface`.
- **`std/llm/catalog`.** `model_info(selector)`,
  `resolved_options(opts)`, `has_capability(model, cap)`,
  `family_of(model_id)`. Harn-side names are deliberately shorter than
  the underlying builtins to avoid shadowing.
- **New Rust builtins.** `llm_resolved_options(opts)`,
  `llm_model_defaults(model_id)`. `llm_model_info(model_id)` already
  existed and is reused. Implemented in
  `crates/harn-vm/src/llm/config_builtins.rs`.
- **`harn models list [--provider NAME] [--json] [--installed-only]`.**
- **`harn models install <model> [--yes] [--keep-alive VALUE]`.**
- **`harn try "<prompt>"`.** One-shot prompt against the configured
  provider with `with_retry`-wrapped `default_llm_caller`.
- **`harn doctor --json`.** Doctor now also checks Ollama, hardware,
  Harn version, and prints a "Next step" suggestion.
- **Orchestration hook dispatch benchmark.** Added `harn-orchestration-perf`
  and `make bench-orchestration` to measure no-op VM lifecycle hook fanout
  costs across 1, 8, 32, and 128 trigger-shaped events.
- **Lint rule `deprecated_llm_options`.** Warns on `llm_retries` /
  `llm_backoff_ms` in dict literals passed to `llm_call` /
  `llm_call_safe` / `llm_call_structured` / `llm_call_structured_result` /
  `agent_loop`.
- **`harn quickstart` setup wizard (#1331).** Added an interactive and
  non-interactive setup flow that detects provider credentials, local Ollama,
  free disk space, and local GPU availability, then writes starter
  `providers.toml`, `harn.toml`, and `.env` files.
- **Tiktoken-grade token counting.** Added `tiktoken_count_tokens(text, model)`
  and `std/llm/budget` helpers so budget checks use exact OpenAI tiktoken
  counts when available, labeled tiktoken approximations for Claude/Gemini
  model families, and the heuristic fallback only for unknown model IDs.

### Changed

- `harn doctor` output splits credentials and network checks.
- Improved no-providers-configured error message names every supported
  env var dynamically and points at `harn doctor` and (when available)
  `harn models recommend`.
- **`std/async` predicate retry rename.** Renamed `retry_with_backoff` to
  `retry_predicate_with_backoff` and removed the old export, with lint autofix
  support for stale call sites.

### Deprecated

- `llm_retries` and `llm_backoff_ms` on `llm_call` family + `agent_loop`
  — still functional in v0.8.x, removed in v0.9.0. Replace with
  `with_retry(default_llm_caller(), {max_attempts: K + 1})` from
  `std/llm/handlers`. `harn lint` warns on deprecated usage (rule:
  `deprecated_llm_options`).
- `llm_options` patch field on `post_turn_callback` outcomes —
  soft-deprecated; emits one event per session when used. Replace by
  wrapping `llm_caller` with a per-turn opts mutator (e.g.
  `with_prompt_rewrite`).

### Migration

`llm_retries: K` historically meant **K retries after the first
attempt** = K + 1 total attempts. `with_retry`'s `max_attempts: N`
counts **total attempts**. Adjust the number when migrating.

Before:

```harn,ignore
llm_call(prompt, sys, {llm_retries: 3, llm_backoff_ms: 250})
```

After:

```harn,ignore
import {default_llm_caller, with_retry} from "std/llm/handlers"

let caller = with_retry(default_llm_caller(), {
  max_attempts: 4,        // NOTE: total attempts; llm_retries: 3 == 4 attempts
  base_ms: 250,
  backoff: "exponential",
})
caller({prompt: prompt, system: sys, opts: {},
        turn: {iteration: 0, session_id: "", attempt: 1}})
```

Or pass through the new `llm_caller:` option on `agent_loop`:

```harn,ignore
agent_loop(task, sys, {
  llm_caller: with_retry(default_llm_caller(), {max_attempts: 4}),
})
```
