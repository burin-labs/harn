use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub(crate) struct TestBenchArgs {
    #[command(subcommand)]
    pub command: TestBenchCommand,
}

#[derive(Debug, Subcommand)]
pub(crate) enum TestBenchCommand {
    /// Execute a .harn script under a hermetic testbench: paused clock,
    /// optional LLM fixtures, optional filesystem overlay, optional
    /// subprocess tape, and a deny-by-default network policy.
    Run(TestBenchRunArgs),
    /// Replay a previously recorded subprocess tape against a script
    /// and assert the run produces a byte-identical tape.
    Replay(TestBenchReplayArgs),
    /// Score replay fidelity. Pass two recorded tapes to diff them, or
    /// pass `--against <tape> <script>` to re-run the script and compare
    /// the new tape against the recorded one.
    Fidelity(TestBenchFidelityArgs),
    /// Validate an annotation sidecar (`<tape>.annotations.jsonl`)
    /// against its target tape. Surfaces schema errors, unknown
    /// `event_id` references, and digest drift between tape and
    /// annotations.
    ValidateAnnotations(TestBenchValidateAnnotationsArgs),
    /// Export annotations filtered by kind. Feeds friction roll-ups,
    /// crystallization candidate detection, and persona eval rubrics
    /// from the same JSONL.
    ExportAnnotations(TestBenchExportAnnotationsArgs),
}

#[derive(Debug, Args, Clone)]
pub(crate) struct TestBenchRunArgs {
    /// Path to the .harn script to execute.
    pub file: String,
    /// Pin the unified mock clock to this UNIX-epoch millisecond value
    /// before the script runs. Defaults to a fixed deterministic
    /// timestamp when `--clock paused` is requested without `--start-at`.
    #[arg(long = "start-at", value_name = "UNIX_MS")]
    pub start_at_ms: Option<i64>,
    /// `paused` (default) or `real`. Selects whether the clock is
    /// pinned at all.
    #[arg(long = "clock", default_value = "paused", value_name = "MODE")]
    pub clock: String,
    /// Replay LLM responses from a JSONL fixture (same format as
    /// `harn run --llm-mock`).
    #[arg(
        long = "llm-fixture",
        value_name = "PATH",
        conflicts_with = "llm_record"
    )]
    pub llm_fixture: Option<String>,
    /// Record executed LLM responses into a JSONL fixture for a future
    /// replay.
    #[arg(
        long = "llm-record",
        value_name = "PATH",
        conflicts_with = "llm_fixture"
    )]
    pub llm_record: Option<String>,
    /// Mount a copy-on-write filesystem overlay rooted at the given
    /// worktree path. Reads pass through; writes stay in memory until
    /// the run ends.
    #[arg(long = "fs-overlay", value_name = "DIR")]
    pub fs_overlay: Option<String>,
    /// Replay subprocess invocations from a tape produced by a previous
    /// `--process-record` run.
    #[arg(
        long = "process-replay",
        value_name = "PATH",
        conflicts_with_all = ["process_record", "process_wasi"]
    )]
    pub process_replay: Option<String>,
    /// Record subprocess invocations to a tape file. The tape captures
    /// (program, args, cwd, stdout, stderr, exit, virtual Δt) tuples.
    #[arg(
        long = "process-record",
        value_name = "PATH",
        conflicts_with_all = ["process_replay", "process_wasi"]
    )]
    pub process_record: Option<String>,
    /// Resolve subprocess invocations against a directory of WASI
    /// (`wasm32-wasi`) modules. Each `program` resolves to
    /// `<dir>/<program>.wasm`; the module runs under wasmtime with the
    /// testbench's mock clock virtualized into `clock_time_get` and
    /// `poll_oneoff`. Programs with no matching `.wasm` fall through to
    /// the native spawn path.
    #[arg(
        long = "process-wasi",
        value_name = "DIR",
        conflicts_with_all = ["process_replay", "process_record"]
    )]
    pub process_wasi: Option<String>,
    /// Network policy. `deny` (default) blocks every outbound request
    /// unless `--allow-host` matches; `real` reverts to the host's
    /// configured policy.
    #[arg(long = "network", default_value = "deny", value_name = "MODE")]
    pub network: String,
    /// Allow outbound traffic to a host or CIDR. Repeatable. Equivalent
    /// to a comma-separated `HARN_EGRESS_ALLOW`. Only effective with
    /// `--network deny`.
    #[arg(long = "allow-host", value_name = "HOST_OR_CIDR")]
    pub allow_host: Vec<String>,
    /// Emit a unified-style diff of overlay filesystem writes to this
    /// path. Requires `--fs-overlay`.
    #[arg(long = "emit-diff", value_name = "PATH", requires = "fs_overlay")]
    pub emit_diff: Option<String>,
    /// Emit a unified event tape (clock reads, sleeps, LLM calls, FS
    /// writes, subprocess spawns) to `PATH`. Large payloads spill to a
    /// content-addressed sidecar at `PATH.cas/`. Documented in
    /// `docs/src/dev/tape-format.md`.
    #[arg(long = "emit-tape", value_name = "PATH")]
    pub emit_tape: Option<String>,
    /// Tokio runtime mode.
    ///
    /// `paused-tokio` (default): multi-threaded runtime with a paused mock
    /// clock. Adequate for most testbench workloads.
    ///
    /// `des`: single-threaded `current_thread` runtime with a paused mock
    /// clock. All tasks, I/O callbacks, and timer firings are coalesced onto
    /// one OS thread, eliminating inter-thread scheduling non-determinism.
    /// Produces bit-exact tape replays for scripts that stay within the
    /// DES-safe primitive set. See `docs/src/dev/des-mode.md` for the
    /// constraint surface and benchmark data.
    #[arg(long = "runtime", default_value = "paused-tokio", value_name = "MODE")]
    pub runtime: String,
    /// Positional script arguments. Pass after `--`:
    /// `harn test-bench run script.harn -- a b c`.
    #[arg(last = true)]
    pub argv: Vec<String>,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct TestBenchReplayArgs {
    /// Path to the .harn script to replay.
    pub file: String,
    /// Subprocess tape produced by a prior `harn test-bench run
    /// --process-record` invocation. The script must request the same
    /// (program, args, cwd) tuples in the same order.
    #[arg(long = "process-tape", value_name = "PATH")]
    pub process_tape: String,
    /// Pin the unified mock clock to this UNIX-epoch millisecond value
    /// before replay. Default matches the testbench-run default.
    #[arg(long = "start-at", value_name = "UNIX_MS")]
    pub start_at_ms: Option<i64>,
    /// LLM JSONL fixture to replay alongside the subprocess tape.
    #[arg(long = "llm-fixture", value_name = "PATH")]
    pub llm_fixture: Option<String>,
    /// Filesystem overlay root for replay (matches the run-side flag).
    #[arg(long = "fs-overlay", value_name = "DIR")]
    pub fs_overlay: Option<String>,
    /// Emit a fresh unified event tape during replay so a fidelity diff
    /// against the recorded tape is one command away.
    #[arg(long = "emit-tape", value_name = "PATH")]
    pub emit_tape: Option<String>,
    /// Annotation sidecar (`<tape>.annotations.jsonl`) to surface during
    /// replay. The runner validates the file against the recorded tape
    /// before replay starts and prints each annotation alongside its
    /// referenced event in the run-summary block. Documented in
    /// `docs/src/dev/annotation-tape-format.md`.
    #[arg(long = "annotations", value_name = "PATH")]
    pub annotations: Option<String>,
    #[arg(last = true)]
    pub argv: Vec<String>,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct TestBenchFidelityArgs {
    /// Two-tape diff form: pass the recorded tape here and the replay
    /// tape as the second positional. Re-run-and-diff form: pass the
    /// recorded tape via `--against` and the .harn script here.
    pub primary: String,
    /// Replay tape to diff against `primary`. Required unless
    /// `--against` is set.
    pub replay: Option<String>,
    /// Recorded tape to re-run a script against. When set, `primary`
    /// is treated as the .harn script path and the runner re-executes
    /// it under testbench replay before computing fidelity.
    #[arg(long = "against", value_name = "PATH")]
    pub against: Option<String>,
    /// `byte-identical` (default), `semantic`, `outcome`, or
    /// `phase-aware`. See `docs/src/dev/tape-format.md` for the
    /// per-mode semantics.
    #[arg(long = "mode", default_value = "byte-identical", value_name = "MODE")]
    pub mode: String,
    /// Write the structured fidelity report (JSON) to this path.
    /// Defaults to stdout.
    #[arg(long = "report", value_name = "PATH")]
    pub report: Option<String>,
    /// Filesystem overlay root used when re-running the script under
    /// `--against`. Ignored without `--against`.
    #[arg(long = "fs-overlay", value_name = "DIR")]
    pub fs_overlay: Option<String>,
    /// Pin the mock clock to this UNIX-epoch millisecond value when
    /// re-running under `--against`. Defaults to the recorded tape's
    /// `started_at_unix_ms`.
    #[arg(long = "start-at", value_name = "UNIX_MS")]
    pub start_at_ms: Option<i64>,
    /// Positional script arguments forwarded to the replayed script
    /// under `--against`. Pass after `--`.
    #[arg(last = true)]
    pub argv: Vec<String>,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct TestBenchValidateAnnotationsArgs {
    /// Tape the annotations target. Used to check `event_id` references
    /// and the optional `tape_content_hash` digest in the annotation
    /// header.
    #[arg(long = "tape", value_name = "PATH")]
    pub tape: String,
    /// Annotation sidecar (`<tape>.annotations.jsonl`) to validate.
    pub annotations: String,
    /// Write the structured validation report (JSON) here. Defaults to
    /// stdout. Either way, the command exits non-zero (status `2`) when
    /// any problems are found.
    #[arg(long = "report", value_name = "PATH")]
    pub report: Option<String>,
}

#[derive(Debug, Args, Clone)]
pub(crate) struct TestBenchExportAnnotationsArgs {
    /// Annotation sidecar to read.
    pub annotations: String,
    /// Annotation kind to filter on. One of: `correct`, `incorrect`,
    /// `alternative`, `note`, `marker`, `mute`, `hypothesis`,
    /// `friction`, `crystallize_here`. Repeatable; multiple kinds union
    /// the result.
    #[arg(long = "kind", value_name = "KIND")]
    pub kind: Vec<String>,
    /// Output format. `jsonl` (default) emits one annotation per line —
    /// drop-in input for downstream pipelines. `friction` re-emits
    /// matching annotations as `FrictionEvent` JSON for the friction
    /// roll-up consumer (see `crates/harn-vm/src/orchestration/friction.rs`).
    #[arg(long = "format", default_value = "jsonl", value_name = "FORMAT")]
    pub format: String,
    /// Write the export to this file. Defaults to stdout.
    #[arg(long = "output", value_name = "PATH")]
    pub output: Option<String>,
}
