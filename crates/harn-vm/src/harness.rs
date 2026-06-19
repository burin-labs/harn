//! Capability handle threaded into every Harn script as the `harness`
//! parameter of `main`.
//!
//! `Harness` is the Harn-language analog of an explicit-capability handle: a
//! single value the runtime hands to a script's `main` so that stdio,
//! terminal, clock, filesystem, environment, randomness, network, process,
//! crypto, system, secrets, and LLM catalog access become surface in the type system
//! instead of ambient globals. Each sub-handle (`stdio`, `term`, `clock`, `fs`,
//! `env`, `random`, `net`, `process`, `crypto`, `system`, `secrets`, `llm`,
//! `tenant`, `auth`, `obs`) is a distinct named type that anchors the surface
//! for its capability slice.
//!
//! This module defines:
//!   * The runtime [`Harness`] value and its sub-handle wrappers.
//!   * [`Harness::real`], the production constructor that installs the backing
//!     state used by concrete sub-handle methods.
//!   * [`VmHarness`], the compact `VmValue` payload that carries the same
//!     state through the bytecode VM and distinguishes the root handle from
//!     its sub-handles via [`HarnessKind`].

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harn_clock::{Clock, PausedClock, RealClock};
use time::OffsetDateTime;

/// Capability slices exposed by a [`Harness`].
///
/// `Root` is the parent handle; the others are the typed sub-handles users
/// reach through field access (`harness.stdio`, `harness.clock`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HarnessKind {
    Root,
    Stdio,
    Term,
    Clock,
    Fs,
    Env,
    Random,
    Net,
    Process,
    Crypto,
    System,
    Secrets,
    Llm,
    /// Tenant sub-handle exposing the ambient `TenantId` (if any) that
    /// the dispatching host bound to this call. See
    /// [`crate::harness_tenant`].
    Tenant,
    /// Auth sub-handle exposing the ambient authenticated principal (if
    /// any) — subject, scheme, granted scopes, and optional principal
    /// kind — that the dispatching host bound to this call. See
    /// [`crate::harness_auth`].
    Auth,
    /// Observability sub-handle: spans, counter/histogram/gauge
    /// instruments, structured logs, and the ambient request_id
    /// surfaced by the dispatching host. See
    /// [`crate::observability::request_id`] and
    /// [`crate::observability::vocabulary`].
    Obs,
}

impl HarnessKind {
    /// The Harn-language type name for this kind (`Harness`, `HarnessStdio`,
    /// etc.). Used by the typechecker primitive registration and by
    /// `VmValue::type_name`.
    pub const fn type_name(self) -> &'static str {
        match self {
            HarnessKind::Root => "Harness",
            HarnessKind::Stdio => "HarnessStdio",
            HarnessKind::Term => "HarnessTerm",
            HarnessKind::Clock => "HarnessClock",
            HarnessKind::Fs => "HarnessFs",
            HarnessKind::Env => "HarnessEnv",
            HarnessKind::Random => "HarnessRandom",
            HarnessKind::Net => "HarnessNet",
            HarnessKind::Process => "HarnessProcess",
            HarnessKind::Crypto => "HarnessCrypto",
            HarnessKind::System => "HarnessSystem",
            HarnessKind::Secrets => "HarnessSecrets",
            HarnessKind::Llm => "HarnessLlm",
            HarnessKind::Tenant => "HarnessTenant",
            HarnessKind::Auth => "HarnessAuth",
            HarnessKind::Obs => "HarnessObs",
        }
    }

    /// Field name a parent `Harness` exposes for this sub-handle (e.g. the
    /// `stdio` in `harness.stdio`). Returns `None` for the root.
    pub const fn field_name(self) -> Option<&'static str> {
        match self {
            HarnessKind::Root => None,
            HarnessKind::Stdio => Some("stdio"),
            HarnessKind::Term => Some("term"),
            HarnessKind::Clock => Some("clock"),
            HarnessKind::Fs => Some("fs"),
            HarnessKind::Env => Some("env"),
            HarnessKind::Random => Some("random"),
            HarnessKind::Net => Some("net"),
            HarnessKind::Process => Some("process"),
            HarnessKind::Crypto => Some("crypto"),
            HarnessKind::System => Some("system"),
            HarnessKind::Secrets => Some("secrets"),
            HarnessKind::Llm => Some("llm"),
            HarnessKind::Tenant => Some("tenant"),
            HarnessKind::Auth => Some("auth"),
            HarnessKind::Obs => Some("obs"),
        }
    }

    /// Parse the field name a script uses to reach a sub-handle.
    pub fn from_field_name(name: &str) -> Option<Self> {
        match name {
            "stdio" => Some(HarnessKind::Stdio),
            "term" => Some(HarnessKind::Term),
            "clock" => Some(HarnessKind::Clock),
            "fs" => Some(HarnessKind::Fs),
            "env" => Some(HarnessKind::Env),
            "random" => Some(HarnessKind::Random),
            "net" => Some(HarnessKind::Net),
            "process" => Some(HarnessKind::Process),
            "crypto" => Some(HarnessKind::Crypto),
            "system" => Some(HarnessKind::System),
            "secrets" => Some(HarnessKind::Secrets),
            "llm" => Some(HarnessKind::Llm),
            "tenant" => Some(HarnessKind::Tenant),
            "auth" => Some(HarnessKind::Auth),
            "obs" => Some(HarnessKind::Obs),
            _ => None,
        }
    }

    /// All sub-handle kinds, in the canonical field order.
    pub const SUB_HANDLES: &'static [HarnessKind] = &[
        HarnessKind::Stdio,
        HarnessKind::Term,
        HarnessKind::Clock,
        HarnessKind::Fs,
        HarnessKind::Env,
        HarnessKind::Random,
        HarnessKind::Net,
        HarnessKind::Process,
        HarnessKind::Crypto,
        HarnessKind::System,
        HarnessKind::Secrets,
        HarnessKind::Llm,
        HarnessKind::Tenant,
        HarnessKind::Auth,
        HarnessKind::Obs,
    ];

    /// Every kind a Harn-script type annotation may reference.
    pub const ALL: &'static [HarnessKind] = &[
        HarnessKind::Root,
        HarnessKind::Stdio,
        HarnessKind::Term,
        HarnessKind::Clock,
        HarnessKind::Fs,
        HarnessKind::Env,
        HarnessKind::Random,
        HarnessKind::Net,
        HarnessKind::Process,
        HarnessKind::Crypto,
        HarnessKind::System,
        HarnessKind::Secrets,
        HarnessKind::Llm,
        HarnessKind::Tenant,
        HarnessKind::Auth,
        HarnessKind::Obs,
    ];
}

/// Shared, refcounted state backing every sub-handle of a single `Harness`.
///
/// Method implementations (in `crate::vm::methods::harness`) borrow this to
/// reach the concrete OS-backed primitives. Wrapped in `Arc` so handles are
/// `Send + Sync` for VM contexts that move work onto other tasks.
pub struct HarnessInner {
    clock: Arc<dyn Clock>,
    mode: HarnessMode,
    /// Per-harness `harness.net.*` access policy. `None` means the
    /// handle inherits the legacy unrestricted behaviour (subject to
    /// the process-wide `crate::egress` allowlist, if configured).
    /// See `Harness::with_net_policy` and `crate::harness_net`.
    net_policy: Option<crate::harness_net::NetPolicy>,
    /// Optional provider backing `harness.secrets.*`. Runtime embedders install
    /// the managed provider that owns custody, audit, leases, and rotation.
    secret_provider: Option<Arc<dyn crate::secrets::SecretProvider>>,
    /// `true` once a request denied under `OnViolation::Quarantine`
    /// has fired. Sticky for the lifetime of the underlying
    /// `Arc<HarnessInner>` so downstream consumers can pin on the
    /// signal even after the originating call has returned. The flag
    /// is per-`Arc` (i.e. per-`Harness` build) so unrelated harnesses
    /// stay independent.
    quarantined: Mutex<bool>,
}

impl fmt::Debug for HarnessInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HarnessInner")
            .field("clock", &"<dyn Clock>")
            .field("mode", &self.mode)
            .field("net_policy", &self.net_policy)
            .field(
                "secret_provider",
                &self
                    .secret_provider
                    .as_ref()
                    .map(|provider| provider.namespace().to_string()),
            )
            .field("quarantined", &self.is_quarantined())
            .finish()
    }
}

impl HarnessInner {
    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    pub(crate) fn mode(&self) -> &HarnessMode {
        &self.mode
    }

    pub fn net_policy(&self) -> Option<&crate::harness_net::NetPolicy> {
        self.net_policy.as_ref()
    }

    pub fn secret_provider(&self) -> Option<&Arc<dyn crate::secrets::SecretProvider>> {
        self.secret_provider.as_ref()
    }

    pub(crate) fn mark_quarantined(&self) {
        if let Ok(mut guard) = self.quarantined.lock() {
            *guard = true;
        }
    }

    pub fn is_quarantined(&self) -> bool {
        self.quarantined.lock().map(|guard| *guard).unwrap_or(false)
    }
}

#[derive(Debug)]
pub(crate) enum HarnessMode {
    Real,
    Null(NullHarnessState),
    Mock(Arc<MockHarnessState>),
}

#[derive(Debug, Default)]
pub(crate) struct NullHarnessState {
    deny_events: Mutex<Vec<DenyEvent>>,
}

impl NullHarnessState {
    pub(crate) fn record_deny(
        &self,
        sub_handle: HarnessKind,
        method: &str,
        args: &[crate::VmValue],
    ) {
        self.deny_events
            .lock()
            .expect("deny events poisoned")
            .push(DenyEvent::new(
                sub_handle,
                method,
                args.iter().map(crate::VmValue::display).collect(),
            ));
    }

    pub(crate) fn deny_events(&self) -> Vec<DenyEvent> {
        self.deny_events
            .lock()
            .expect("deny events poisoned")
            .clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DenyEvent {
    pub sub_handle: HarnessKind,
    pub method: String,
    pub args: Vec<String>,
}

impl DenyEvent {
    fn new(sub_handle: HarnessKind, method: &str, args: Vec<String>) -> Self {
        Self {
            sub_handle,
            method: method.to_string(),
            args,
        }
    }
}

#[derive(Debug)]
pub(crate) struct MockHarnessState {
    calls: Mutex<Vec<HarnessCall>>,
    clock: Arc<PausedClock>,
    env: BTreeMap<String, String>,
    fs_reads: BTreeMap<String, Vec<u8>>,
    net_gets: BTreeMap<String, String>,
    random_u64: Mutex<VecDeque<u64>>,
    stdin_lines: Mutex<VecDeque<String>>,
    stdio: Mutex<String>,
    stderr: Mutex<String>,
}

impl MockHarnessState {
    pub(crate) fn record_call(
        &self,
        sub_handle: HarnessKind,
        method: &str,
        args: &[crate::VmValue],
    ) {
        self.calls
            .lock()
            .expect("calls poisoned")
            .push(HarnessCall::new(
                sub_handle,
                method,
                args.iter().map(crate::VmValue::display).collect(),
            ));
    }

    pub(crate) fn calls(&self) -> Vec<HarnessCall> {
        self.calls.lock().expect("calls poisoned").clone()
    }

    pub(crate) fn env_get(&self, key: &str) -> Option<&str> {
        self.env.get(key).map(String::as_str)
    }

    pub(crate) fn fs_read(&self, path: &str) -> Option<&[u8]> {
        self.fs_reads.get(path).map(Vec::as_slice)
    }

    pub(crate) fn net_get(&self, url: &str) -> Option<&str> {
        self.net_gets.get(url).map(String::as_str)
    }

    pub(crate) fn next_random_u64(&self) -> Option<u64> {
        let mut values = self.random_u64.lock().expect("random values poisoned");
        values.pop_front()
    }

    pub(crate) fn advance_clock(&self, duration: std::time::Duration) {
        self.clock.advance(duration);
    }

    pub(crate) fn push_stdio(&self, text: &str) {
        self.stdio
            .lock()
            .expect("stdio buffer poisoned")
            .push_str(text);
    }

    pub(crate) fn stdio(&self) -> String {
        self.stdio.lock().expect("stdio buffer poisoned").clone()
    }

    pub(crate) fn push_stderr(&self, text: &str) {
        self.stderr
            .lock()
            .expect("stderr buffer poisoned")
            .push_str(text);
    }

    pub(crate) fn stderr(&self) -> String {
        self.stderr.lock().expect("stderr buffer poisoned").clone()
    }

    pub(crate) fn pop_stdin_line(&self) -> Option<String> {
        self.stdin_lines
            .lock()
            .expect("stdin queue poisoned")
            .pop_front()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessCall {
    pub sub_handle: HarnessKind,
    pub method: String,
    pub args: Vec<String>,
}

impl HarnessCall {
    fn new(sub_handle: HarnessKind, method: &str, args: Vec<String>) -> Self {
        Self {
            sub_handle,
            method: method.to_string(),
            args,
        }
    }
}

#[derive(Debug)]
pub struct MockHarnessBuilder {
    clock: Arc<PausedClock>,
    env: BTreeMap<String, String>,
    fs_reads: BTreeMap<String, Vec<u8>>,
    net_gets: BTreeMap<String, String>,
    random_u64: Vec<u64>,
    stdin_lines: Vec<String>,
}

impl MockHarnessBuilder {
    fn new() -> Self {
        Self {
            clock: paused_clock_at_unix_ms(0),
            env: BTreeMap::new(),
            fs_reads: BTreeMap::new(),
            net_gets: BTreeMap::new(),
            random_u64: Vec::new(),
            stdin_lines: Vec::new(),
        }
    }

    pub fn clock_at_unix_ms(mut self, unix_ms: i64) -> Self {
        self.clock = paused_clock_at_unix_ms(unix_ms);
        self
    }

    pub fn clock_at(mut self, origin: OffsetDateTime) -> Self {
        self.clock = PausedClock::new(origin);
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    pub fn fs_read(mut self, path: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        self.fs_reads.insert(path.into(), data.into());
        self
    }

    pub fn net_get(mut self, url: impl Into<String>, body: impl Into<String>) -> Self {
        self.net_gets.insert(url.into(), body.into());
        self
    }

    pub fn random_u64(mut self, value: u64) -> Self {
        self.random_u64.push(value);
        self
    }

    /// Queue a line that `harness.stdio.read_line()` or
    /// `harness.stdio.prompt(...)` will return next. Lines are dequeued
    /// FIFO; once the queue is empty subsequent reads surface EOF
    /// (`nil` for the unstructured form, `{ok: false, status: "eof"}`
    /// for the structured form).
    pub fn stdin_line(mut self, line: impl Into<String>) -> Self {
        self.stdin_lines.push(line.into());
        self
    }

    pub fn build(self) -> Harness {
        let clock = self.clock;
        Harness::with_mode(
            clock.clone() as Arc<dyn Clock>,
            HarnessMode::Mock(Arc::new(MockHarnessState {
                calls: Mutex::new(Vec::new()),
                clock,
                env: self.env,
                fs_reads: self.fs_reads,
                net_gets: self.net_gets,
                random_u64: Mutex::new(self.random_u64.into()),
                stdin_lines: Mutex::new(self.stdin_lines.into()),
                stdio: Mutex::new(String::new()),
                stderr: Mutex::new(String::new()),
            })),
        )
    }
}

/// The runtime handle threaded into `main(harness: Harness)`.
///
/// Cheap to clone; sub-handles share the same `Arc` inner state.
#[derive(Debug, Clone)]
pub struct Harness {
    inner: Arc<HarnessInner>,
}

impl Harness {
    /// Build the production handle wired to wall-clock time. Filesystem,
    /// environment, randomness, and network access are layered on by the
    /// E4.2-E4.4 migration tickets; the constructor only needs to succeed
    /// without panicking today (per the E4.1 exit criteria).
    ///
    /// The production clock is wrapped in [`MockAwareClock`] so existing
    /// `mock_time(...)` / `advance_time(...)` test fixtures observe
    /// `harness.clock.*` reads identically to the ambient builtins. The
    /// shim is part of the E4.3-E4.6 migration window and goes away once
    /// the ambient `mock_time` test utility is retired by E4.5.
    pub fn real() -> Self {
        Self::with_mode(
            Arc::new(MockAwareClock::new(RealClock::new())),
            HarnessMode::Real,
        )
    }

    /// Build a deny-by-default test handle. Every sub-handle method records a
    /// [`DenyEvent`] and fails with a categorized VM error.
    pub fn null() -> Self {
        Self::with_mode(
            paused_clock_at_unix_ms(0) as Arc<dyn Clock>,
            HarnessMode::Null(NullHarnessState::default()),
        )
    }

    /// Build a record/replay test handle backed by a paused clock.
    pub fn mock() -> MockHarnessBuilder {
        MockHarnessBuilder::new()
    }

    /// Build a handle wired to a caller-supplied clock. Most callers want
    /// [`Self::test`] (which constructs the `PausedClock` for you);
    /// reach for this when an existing `Arc<dyn Clock>` is already in
    /// hand — e.g. a `RecordedClock` wrapper.
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self::with_mode(clock, HarnessMode::Real)
    }

    /// Construct a `Harness` from a pre-built `Arc<HarnessInner>`.
    /// Used by VM method dispatch when it needs to re-wrap a sub-handle's
    /// inner state into a root `Harness` (e.g. to invoke
    /// [`Self::with_net_policy`] from inside the method dispatcher).
    pub fn from_inner(inner: Arc<HarnessInner>) -> Self {
        Self { inner }
    }

    fn with_mode(clock: Arc<dyn Clock>, mode: HarnessMode) -> Self {
        let inner = Arc::new(HarnessInner {
            clock,
            mode,
            net_policy: None,
            secret_provider: None,
            quarantined: Mutex::new(false),
        });
        Self { inner }
    }

    /// Attach a per-harness `harness.net.*` access policy.
    ///
    /// Returns a new `Harness` value whose sub-handles share a fresh
    /// `Arc<HarnessInner>`. Existing handles built off the prior inner
    /// keep operating without the policy — so calling
    /// `harness.with_net_policy(...)` does NOT retroactively gate
    /// references to `harness` held elsewhere. Per issue #1913.
    ///
    /// The clock and mode are propagated verbatim. Mock canned
    /// responses (`net_gets`, `random_u64`, etc.) live behind the
    /// shared `HarnessMode::Mock` payload, so the new handle observes
    /// the same recorded calls and the same canned responses as the
    /// source handle.
    pub fn with_net_policy(&self, policy: crate::harness_net::NetPolicy) -> Self {
        let clock = Arc::clone(&self.inner.clock);
        let mode = self.clone_mode_for_child();
        // See `with_mode` for the rationale on this suppression.
        #[allow(clippy::arc_with_non_send_sync)]
        let inner = Arc::new(HarnessInner {
            clock,
            mode,
            net_policy: Some(policy),
            secret_provider: self.inner.secret_provider.clone(),
            quarantined: Mutex::new(self.is_quarantined()),
        });
        Self { inner }
    }

    /// Attach a provider for `harness.secrets.*`.
    ///
    /// The provider is intentionally embedder-supplied. Harn owns the typed
    /// method contract; the host owns custody details such as KMS wrapping,
    /// lease storage, audit sinks, and scope policy.
    pub fn with_secret_provider(&self, provider: Arc<dyn crate::secrets::SecretProvider>) -> Self {
        let clock = Arc::clone(&self.inner.clock);
        let mode = self.clone_mode_for_child();
        #[allow(clippy::arc_with_non_send_sync)]
        let inner = Arc::new(HarnessInner {
            clock,
            mode,
            net_policy: self.inner.net_policy.clone(),
            secret_provider: Some(provider),
            quarantined: Mutex::new(self.is_quarantined()),
        });
        Self { inner }
    }

    fn clone_mode_for_child(&self) -> HarnessMode {
        match &self.inner.mode {
            HarnessMode::Real => HarnessMode::Real,
            HarnessMode::Null(_) => HarnessMode::Null(NullHarnessState::default()),
            HarnessMode::Mock(state) => HarnessMode::Mock(Arc::clone(state)),
        }
    }

    /// `true` if the harness has been marked quarantined by an
    /// `OnViolation::Quarantine` deny event.
    pub fn is_quarantined(&self) -> bool {
        self.inner.is_quarantined()
    }

    pub fn deny_events(&self) -> Vec<DenyEvent> {
        match self.inner.mode() {
            HarnessMode::Null(state) => state.deny_events(),
            HarnessMode::Real | HarnessMode::Mock(_) => Vec::new(),
        }
    }

    pub fn calls(&self) -> Vec<HarnessCall> {
        match self.inner.mode() {
            HarnessMode::Mock(state) => state.calls(),
            HarnessMode::Real | HarnessMode::Null(_) => Vec::new(),
        }
    }

    pub fn captured_stdio(&self) -> String {
        match self.inner.mode() {
            HarnessMode::Mock(state) => state.stdio(),
            HarnessMode::Real | HarnessMode::Null(_) => String::new(),
        }
    }

    pub fn captured_stderr(&self) -> String {
        match self.inner.mode() {
            HarnessMode::Mock(state) => state.stderr(),
            HarnessMode::Real | HarnessMode::Null(_) => String::new(),
        }
    }

    /// Build a deterministic test handle wired to a fresh
    /// [`PausedClock`] pinned at the Unix epoch.
    ///
    /// Returns the harness paired with the underlying `PausedClock` so
    /// tests can drive virtual time through `PausedClock::advance`
    /// while passing the same `Harness` value into the VM. The two
    /// share the underlying `Arc<dyn Clock>`, so the harness reflects
    /// every advance immediately.
    ///
    /// Pairs with [`PausedClock::advance`] / [`PausedClock::set`] — see
    /// [`Self::with_paused_clock`] for picking a non-epoch origin.
    pub fn test() -> (Self, Arc<PausedClock>) {
        Self::with_paused_clock(OffsetDateTime::UNIX_EPOCH)
    }

    /// Like [`Self::test`], but pins the paused clock's wall origin to
    /// `origin`. Lets tests anchor virtual time to a meaningful date
    /// without manually advancing past the epoch first.
    pub fn with_paused_clock(origin: OffsetDateTime) -> (Self, Arc<PausedClock>) {
        let paused = PausedClock::new(origin);
        let as_dyn: Arc<dyn Clock> = paused.clone();
        (Self::with_clock(as_dyn), paused)
    }

    /// Field access for `harness.stdio`.
    pub fn stdio(&self) -> HarnessStdio {
        HarnessStdio {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.term`.
    pub fn term(&self) -> HarnessTerm {
        HarnessTerm {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.clock`.
    pub fn clock(&self) -> HarnessClock {
        HarnessClock {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.fs`.
    pub fn fs(&self) -> HarnessFs {
        HarnessFs {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.env`.
    pub fn env(&self) -> HarnessEnv {
        HarnessEnv {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.random`.
    pub fn random(&self) -> HarnessRandom {
        HarnessRandom {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.net`.
    pub fn net(&self) -> HarnessNet {
        HarnessNet {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.process`.
    pub fn process(&self) -> HarnessProcess {
        HarnessProcess {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.crypto`.
    pub fn crypto(&self) -> HarnessCrypto {
        HarnessCrypto {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.system`.
    pub fn system(&self) -> HarnessSystem {
        HarnessSystem {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.secrets`.
    pub fn secrets(&self) -> HarnessSecrets {
        HarnessSecrets {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.llm`.
    pub fn llm(&self) -> HarnessLlm {
        HarnessLlm {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.tenant`.
    pub fn tenant(&self) -> HarnessTenant {
        HarnessTenant {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.auth`.
    pub fn auth(&self) -> HarnessAuth {
        HarnessAuth {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Field access for `harness.obs`.
    pub fn obs(&self) -> HarnessObs {
        HarnessObs {
            inner: Arc::clone(&self.inner),
        }
    }

    /// Lower this handle into the `VmValue::Harness` payload.
    pub fn into_vm_value(self) -> crate::value::VmValue {
        crate::value::VmValue::harness(VmHarness {
            inner: self.inner,
            kind: HarnessKind::Root,
        })
    }
}

fn paused_clock_at_unix_ms(unix_ms: i64) -> Arc<PausedClock> {
    let nanos = (unix_ms as i128).saturating_mul(1_000_000);
    let origin =
        OffsetDateTime::from_unix_timestamp_nanos(nanos).unwrap_or(OffsetDateTime::UNIX_EPOCH);
    PausedClock::new(origin)
}

pub(crate) fn vm_string(value: impl Into<String>) -> crate::VmValue {
    crate::VmValue::String(std::sync::Arc::from(value.into()))
}

impl Default for Harness {
    fn default() -> Self {
        Self::real()
    }
}

/// stdio sub-handle: `print`, `println`, `eprint`, `eprintln`, `prompt`,
/// `read_line`.
#[derive(Debug, Clone)]
pub struct HarnessStdio {
    inner: Arc<HarnessInner>,
}

/// term sub-handle: `width`, `height`, `read_password`.
#[derive(Debug, Clone)]
pub struct HarnessTerm {
    inner: Arc<HarnessInner>,
}

/// clock sub-handle: `now`, `monotonic_now`, `sleep`.
#[derive(Debug, Clone)]
pub struct HarnessClock {
    inner: Arc<HarnessInner>,
}

impl HarnessClock {
    pub fn clock(&self) -> &Arc<dyn Clock> {
        self.inner.clock()
    }
}

/// fs sub-handle: `read_file`, `write_file`, `exists`, `list_dir`,
/// `delete_file`, ...
#[derive(Debug, Clone)]
pub struct HarnessFs {
    inner: Arc<HarnessInner>,
}

/// env sub-handle: `get`, `set`, `vars`.
#[derive(Debug, Clone)]
pub struct HarnessEnv {
    inner: Arc<HarnessInner>,
}

/// random sub-handle: `gen_u64`, `gen_range`, `gen_f64`, ...
#[derive(Debug, Clone)]
pub struct HarnessRandom {
    inner: Arc<HarnessInner>,
}

/// net sub-handle: `http_get`, `http_post`, ...
#[derive(Debug, Clone)]
pub struct HarnessNet {
    inner: Arc<HarnessInner>,
}

/// process sub-handle: `spawn_captured`.
#[derive(Debug, Clone)]
pub struct HarnessProcess {
    inner: Arc<HarnessInner>,
}

/// crypto sub-handle: deterministic digest helpers such as `sha256`.
#[derive(Debug, Clone)]
pub struct HarnessCrypto {
    inner: Arc<HarnessInner>,
}

/// system sub-handle: `cpu`, `memory`, `gpus`, `temperature`, `platform`,
/// `processes`. Read-only host introspection — no side effects on the host
/// system. Gated by the harness handle so scripts running under
/// `Harness::null()` or restricted policies cannot fingerprint the runner
/// without an explicit grant (issue #1912 / epic #1765).
#[derive(Debug, Clone)]
pub struct HarnessSystem {
    inner: Arc<HarnessInner>,
}

/// secrets sub-handle: `read`, `write`, `rotate`, `lease`.
#[derive(Debug, Clone)]
pub struct HarnessSecrets {
    inner: Arc<HarnessInner>,
}

/// llm sub-handle: `catalog`, `providers`.
#[derive(Debug, Clone)]
pub struct HarnessLlm {
    inner: Arc<HarnessInner>,
}

/// tenant sub-handle: `id`, `try_id`. Surfaces the ambient `TenantId`
/// bound by the dispatching host (see [`crate::harness_tenant`]). No
/// host state — the methods consult a thread-local stack — but the
/// handle still rides the shared `Arc<HarnessInner>` so null/mock-mode
/// gating in [`crate::vm::methods::harness`] applies uniformly.
#[derive(Debug, Clone)]
pub struct HarnessTenant {
    inner: Arc<HarnessInner>,
}

/// auth sub-handle: `is_authenticated`, `subject` / `try_subject`,
/// `scheme` / `try_scheme`, `kind`, `scopes`, `has_scope`. Surfaces the
/// ambient authenticated principal bound by the dispatching host (see
/// [`crate::harness_auth`]). Like [`HarnessTenant`] it holds no host
/// state — the methods consult a thread-local stack — but rides the
/// shared `Arc<HarnessInner>` so null/mock-mode gating in
/// [`crate::vm::methods::harness`] applies uniformly.
#[derive(Debug, Clone)]
pub struct HarnessAuth {
    inner: Arc<HarnessInner>,
}

/// obs sub-handle: `span` / `start_span` / `end_span` / `counter` /
/// `histogram` / `gauge` / `log` / `request_id`. Wraps the existing
/// `__obs_*` builtins and the request_id ambient pushed by the
/// dispatching host (see [`crate::observability::request_id`]) behind a
/// typed surface so handlers don't reach into the lower-level builtins
/// directly. Backend selection / exporter wiring still lives in
/// [`crate::events`] (OTel sink) and `std/observability` (`configure`,
/// backend factories) — the sub-handle is the *emit-side* surface that
/// every harn-serve primitive shares.
#[derive(Debug, Clone)]
pub struct HarnessObs {
    inner: Arc<HarnessInner>,
}

macro_rules! sub_handle_inner {
    ($($ty:ty),* $(,)?) => {
        $(
            impl $ty {
                #[allow(dead_code)]
                pub(crate) fn inner(&self) -> &Arc<HarnessInner> {
                    &self.inner
                }
            }
        )*
    };
}
sub_handle_inner!(
    HarnessStdio,
    HarnessTerm,
    HarnessFs,
    HarnessEnv,
    HarnessRandom,
    HarnessNet,
    HarnessProcess,
    HarnessCrypto,
    HarnessSystem,
    HarnessSecrets,
    HarnessLlm,
    HarnessTenant,
    HarnessAuth,
    HarnessObs,
);

impl HarnessClock {
    #[allow(dead_code)]
    pub(crate) fn inner(&self) -> &Arc<HarnessInner> {
        &self.inner
    }
}

/// Compact `VmValue` payload for a `Harness` or any of its sub-handles.
///
/// All handle variants share one `Arc<HarnessInner>`; `kind` discriminates the
/// surface the VM exposes for property access and method dispatch.
#[derive(Clone)]
pub struct VmHarness {
    inner: Arc<HarnessInner>,
    kind: HarnessKind,
}

impl VmHarness {
    pub fn kind(&self) -> HarnessKind {
        self.kind
    }

    pub fn type_name(&self) -> &'static str {
        self.kind.type_name()
    }

    pub fn inner(&self) -> &Arc<HarnessInner> {
        &self.inner
    }

    /// Get the sub-handle reached by a field name (`stdio`, `clock`, etc.).
    /// Returns `None` when the receiver is itself a sub-handle or the field
    /// is unknown.
    pub fn sub_handle(&self, field: &str) -> Option<VmHarness> {
        if self.kind != HarnessKind::Root {
            return None;
        }
        let kind = HarnessKind::from_field_name(field)?;
        self.sub_handle_kind(kind)
    }

    pub(crate) fn sub_handle_kind(&self, kind: HarnessKind) -> Option<VmHarness> {
        if self.kind != HarnessKind::Root || kind == HarnessKind::Root {
            return None;
        }
        Some(VmHarness {
            inner: Arc::clone(&self.inner),
            kind,
        })
    }
}

impl fmt::Debug for VmHarness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VmHarness")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// Clock wrapper that consults the crate-wide `clock_mock` thread-local
/// before delegating to an inner [`Clock`]. Used by [`Harness::real`] so
/// `harness.clock.*` reads honor `mock_time(...)` / `advance_time(...)`
/// during the E4.3-E4.6 migration. New tests should prefer
/// [`Harness::test`] / [`PausedClock`] directly.
#[derive(Debug)]
pub struct MockAwareClock<C: Clock + 'static> {
    inner: C,
}

impl<C: Clock + 'static> MockAwareClock<C> {
    pub fn new(inner: C) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl<C: Clock + 'static> Clock for MockAwareClock<C> {
    fn now_utc(&self) -> OffsetDateTime {
        if let Some(mock) = crate::clock_mock::active_mock_clock() {
            return mock.now_utc();
        }
        self.inner.now_utc()
    }

    fn monotonic_ms(&self) -> i64 {
        if let Some(mock) = crate::clock_mock::active_mock_clock() {
            return mock.monotonic_ms();
        }
        self.inner.monotonic_ms()
    }

    async fn sleep(&self, duration: Duration) {
        if duration.is_zero() {
            return;
        }
        if let Some(mock) = crate::clock_mock::active_mock_clock() {
            // Single-script tests under `mock_time(...)` rely on `sleep(...)`
            // advancing the mock and returning immediately — the same
            // semantics as the legacy ambient `sleep_ms` builtin. Waiting
            // on `mock.sleep` would deadlock because nothing else is
            // driving `advance(...)` in the same task.
            mock.advance_std_sync(duration);
            return;
        }
        self.inner.sleep(duration).await;
    }

    async fn sleep_until_utc(&self, deadline: OffsetDateTime) {
        if let Some(mock) = crate::clock_mock::active_mock_clock() {
            let now = mock.now_utc();
            if deadline > now {
                if let Ok(delta) = Duration::try_from(deadline - now) {
                    mock.advance_std_sync(delta);
                }
            }
            return;
        }
        self.inner.sleep_until_utc(deadline).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::SecretProvider;
    use async_trait::async_trait;

    #[derive(Clone, Debug, PartialEq, Eq)]
    struct SecretCall {
        operation: &'static str,
        id: crate::secrets::SecretId,
        scope: crate::secrets::SecretScope,
        request_id: Option<String>,
        actor_subject: Option<String>,
        actor_kind: Option<String>,
        duration_ms: Option<u64>,
        grace_ms: Option<u64>,
        ttl_ms: Option<u64>,
    }

    #[derive(Clone, Default)]
    struct RecordingSecretProvider {
        inner: Arc<RecordingSecretProviderInner>,
    }

    #[derive(Default)]
    struct RecordingSecretProviderInner {
        versions: Mutex<BTreeMap<crate::secrets::SecretId, Vec<Vec<u8>>>>,
        calls: Mutex<Vec<SecretCall>>,
    }

    impl RecordingSecretProvider {
        fn calls(&self) -> Vec<SecretCall> {
            self.inner
                .calls
                .lock()
                .expect("calls lock poisoned")
                .clone()
        }

        fn record(
            &self,
            operation: &'static str,
            id: &crate::secrets::SecretId,
            scope: &crate::secrets::SecretScope,
            audit: &crate::secrets::SecretAuditContext,
            duration_ms: Option<u64>,
            grace_ms: Option<u64>,
            ttl_ms: Option<u64>,
        ) {
            self.inner
                .calls
                .lock()
                .expect("calls lock poisoned")
                .push(SecretCall {
                    operation,
                    id: id.clone(),
                    scope: scope.clone(),
                    request_id: audit.request_id.clone(),
                    actor_subject: audit.actor_subject.clone(),
                    actor_kind: audit.actor_kind.clone(),
                    duration_ms,
                    grace_ms,
                    ttl_ms,
                });
        }

        fn read_latest(
            &self,
            id: &crate::secrets::SecretId,
        ) -> Result<(u64, Vec<u8>), crate::secrets::SecretError> {
            let versions = self.inner.versions.lock().expect("versions lock poisoned");
            let values = versions
                .get(id)
                .filter(|values| !values.is_empty())
                .ok_or_else(|| crate::secrets::SecretError::NotFound {
                    provider: self.namespace().to_string(),
                    id: id.clone(),
                })?;
            Ok((
                values.len() as u64,
                values.last().expect("non-empty").clone(),
            ))
        }

        fn write_version(
            &self,
            id: &crate::secrets::SecretId,
            value: &crate::secrets::SecretBytes,
        ) -> u64 {
            let mut versions = self.inner.versions.lock().expect("versions lock poisoned");
            let values = versions.entry(id.clone()).or_default();
            values.push(value.with_exposed(|bytes| bytes.to_vec()));
            values.len() as u64
        }
    }

    fn duration_ms(duration: Duration) -> u64 {
        duration.as_millis().min(u128::from(u64::MAX)) as u64
    }

    #[async_trait]
    impl crate::secrets::SecretProvider for RecordingSecretProvider {
        async fn get(
            &self,
            id: &crate::secrets::SecretId,
        ) -> Result<crate::secrets::SecretBytes, crate::secrets::SecretError> {
            self.read_latest(id)
                .map(|(_, value)| crate::secrets::SecretBytes::from(value))
        }

        async fn put(
            &self,
            id: &crate::secrets::SecretId,
            value: crate::secrets::SecretBytes,
        ) -> Result<(), crate::secrets::SecretError> {
            self.write_version(id, &value);
            Ok(())
        }

        async fn rotate(
            &self,
            id: &crate::secrets::SecretId,
        ) -> Result<crate::secrets::RotationHandle, crate::secrets::SecretError> {
            let (from_version, value) = self.read_latest(id)?;
            let to_version =
                self.write_version(id, &crate::secrets::SecretBytes::from(value.as_slice()));
            Ok(crate::secrets::RotationHandle {
                provider: self.namespace().to_string(),
                id: id
                    .clone()
                    .with_version(crate::secrets::SecretVersion::Exact(to_version)),
                from_version: Some(from_version),
                to_version: Some(to_version),
            })
        }

        async fn list(
            &self,
            _prefix: &crate::secrets::SecretId,
        ) -> Result<Vec<crate::secrets::SecretMeta>, crate::secrets::SecretError> {
            Ok(Vec::new())
        }

        async fn read_scoped(
            &self,
            request: crate::secrets::SecretReadRequest,
        ) -> Result<crate::secrets::SecretBytes, crate::secrets::SecretError> {
            self.record(
                "read",
                &request.id,
                &request.scope,
                &request.audit,
                None,
                None,
                None,
            );
            self.read_latest(&request.id)
                .map(|(_, value)| crate::secrets::SecretBytes::from(value))
        }

        async fn write_scoped(
            &self,
            request: crate::secrets::SecretWriteRequest,
        ) -> Result<crate::secrets::SecretWriteReceipt, crate::secrets::SecretError> {
            let ttl_ms = request.options.ttl.map(duration_ms);
            self.record(
                "write",
                &request.id,
                &request.scope,
                &request.audit,
                None,
                None,
                ttl_ms,
            );
            let version = self.write_version(&request.id, &request.value);
            Ok(crate::secrets::SecretWriteReceipt {
                provider: self.namespace().to_string(),
                id: request
                    .id
                    .with_version(crate::secrets::SecretVersion::Exact(version)),
                scope: request.scope,
                version: Some(version),
                expires_at_unix_ms: ttl_ms.map(|ttl| 1_700_000_000_000_i64 + ttl as i64),
            })
        }

        async fn rotate_scoped(
            &self,
            request: crate::secrets::SecretRotateRequest,
        ) -> Result<crate::secrets::SecretRotationReceipt, crate::secrets::SecretError> {
            let grace_ms = request.options.grace.map(duration_ms);
            let ttl_ms = request.options.ttl.map(duration_ms);
            self.record(
                "rotate",
                &request.id,
                &request.scope,
                &request.audit,
                None,
                grace_ms,
                ttl_ms,
            );
            let from_version = self
                .inner
                .versions
                .lock()
                .expect("versions lock poisoned")
                .get(&request.id)
                .map(|values| values.len() as u64);
            let to_version = self.write_version(&request.id, &request.value);
            Ok(crate::secrets::SecretRotationReceipt {
                provider: self.namespace().to_string(),
                id: request
                    .id
                    .with_version(crate::secrets::SecretVersion::Exact(to_version)),
                scope: request.scope,
                from_version,
                to_version: Some(to_version),
                grace_until_unix_ms: grace_ms.map(|grace| 1_700_000_000_000_i64 + grace as i64),
                expires_at_unix_ms: ttl_ms.map(|ttl| 1_700_000_000_000_i64 + ttl as i64),
            })
        }

        async fn lease_scoped(
            &self,
            request: crate::secrets::SecretLeaseRequest,
        ) -> Result<crate::secrets::SecretLeaseGrant, crate::secrets::SecretError> {
            let duration = duration_ms(request.duration);
            self.record(
                "lease",
                &request.id,
                &request.scope,
                &request.audit,
                Some(duration),
                None,
                None,
            );
            let (version, value) = self.read_latest(&request.id)?;
            Ok(crate::secrets::SecretLeaseGrant {
                provider: self.namespace().to_string(),
                id: request
                    .id
                    .with_version(crate::secrets::SecretVersion::Exact(version)),
                scope: request.scope,
                lease_id: format!("lease-{version}"),
                value: crate::secrets::SecretBytes::from(value),
                expires_at_unix_ms: 1_700_000_000_000_i64 + duration as i64,
            })
        }

        fn namespace(&self) -> &'static str {
            "recording"
        }

        fn supports_versions(&self) -> bool {
            true
        }
    }

    #[test]
    fn real_constructs_without_panic() {
        let _harness = Harness::real();
    }

    #[test]
    fn sub_handles_share_inner_state() {
        let harness = Harness::real();
        let stdio_inner = Arc::as_ptr(harness.stdio().inner());
        let clock_inner = Arc::as_ptr(harness.clock().inner());
        assert_eq!(stdio_inner, clock_inner, "sub-handles share Arc<Inner>");
    }

    #[test]
    fn kinds_round_trip_through_field_names() {
        for kind in HarnessKind::SUB_HANDLES {
            let field = kind.field_name().unwrap();
            assert_eq!(HarnessKind::from_field_name(field), Some(*kind));
        }
        assert!(HarnessKind::from_field_name("nope").is_none());
        assert!(HarnessKind::Root.field_name().is_none());
    }

    #[test]
    fn vm_harness_property_access_returns_sub_handle() {
        let root = match Harness::real().into_vm_value() {
            crate::value::VmValue::Harness(h) => h,
            other => panic!("expected Harness variant, got {}", other.type_name()),
        };
        let stdio = root.sub_handle("stdio").expect("stdio sub-handle");
        assert_eq!(stdio.kind(), HarnessKind::Stdio);
        assert!(stdio.sub_handle("clock").is_none(), "nested access denied");
        assert!(root.sub_handle("not_a_field").is_none());
    }

    #[test]
    fn test_constructor_clock_advances_under_paused_clock_advance() {
        let (harness, paused) = Harness::test();
        let clock = harness.clock();
        let start_wall = clock.clock().now_utc();
        assert_eq!(start_wall, OffsetDateTime::UNIX_EPOCH);
        assert_eq!(clock.clock().monotonic_ms(), 0);

        paused.advance(Duration::from_millis(1_500));
        assert_eq!(clock.clock().monotonic_ms(), 1_500);
        let after_wall = clock.clock().now_utc();
        assert_eq!(after_wall - start_wall, time::Duration::milliseconds(1_500));
    }

    #[test]
    fn with_paused_clock_pins_origin() {
        let origin = OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        let (harness, paused) = Harness::with_paused_clock(origin);
        assert_eq!(harness.clock().clock().now_utc(), origin);
        paused.advance(Duration::from_mins(1));
        assert_eq!(
            harness.clock().clock().now_utc() - origin,
            time::Duration::seconds(60)
        );
    }

    #[test]
    fn null_harness_records_deny_events_for_every_sub_handle() {
        let harness = Harness::null();
        for source in [
            r#"fn main(harness: Harness) { harness.stdio.println("blocked") }"#,
            r"fn main(harness: Harness) { harness.term.width() }",
            r"fn main(harness: Harness) { harness.clock.now_ms() }",
            r#"fn main(harness: Harness) { harness.fs.read_text("/x") }"#,
            r#"fn main(harness: Harness) { harness.env.get("KEY") }"#,
            r"fn main(harness: Harness) { harness.random.gen_u64() }",
            r#"fn main(harness: Harness) { harness.net.get("https://example.test") }"#,
            r#"fn main(harness: Harness) { harness.process.spawn_captured({cmd: "printf", args: ["x"]}) }"#,
            r#"fn main(harness: Harness) { harness.crypto.sha256("") }"#,
            r"fn main(harness: Harness) { harness.system.cpu() }",
            r#"fn main(harness: Harness) { harness.secrets.read("blocked") }"#,
            r"fn main(harness: Harness) { harness.llm.catalog() }",
            r"fn main(harness: Harness) { harness.tenant.id() }",
            r"fn main(harness: Harness) { harness.auth.subject() }",
            r#"fn main(harness: Harness) { harness.obs.log("blocked", "info", {}) }"#,
        ] {
            let error = run_harness_source(source, harness.clone()).expect_err("call denied");
            assert!(
                error.contains("NullHarness denied"),
                "unexpected deny error: {error}"
            );
        }

        let events = harness.deny_events();
        let observed: Vec<_> = events
            .iter()
            .map(|event| (event.sub_handle, event.method.as_str()))
            .collect();
        assert_eq!(
            observed,
            vec![
                (HarnessKind::Stdio, "println"),
                (HarnessKind::Term, "width"),
                (HarnessKind::Clock, "now_ms"),
                (HarnessKind::Fs, "read_text"),
                (HarnessKind::Env, "get"),
                (HarnessKind::Random, "gen_u64"),
                (HarnessKind::Net, "get"),
                (HarnessKind::Process, "spawn_captured"),
                (HarnessKind::Crypto, "sha256"),
                (HarnessKind::System, "cpu"),
                (HarnessKind::Secrets, "read"),
                (HarnessKind::Llm, "catalog"),
                (HarnessKind::Tenant, "id"),
                (HarnessKind::Auth, "subject"),
                (HarnessKind::Obs, "log"),
            ]
        );
        assert_eq!(events[0].args, vec!["blocked"]);
        assert_eq!(events[3].args, vec!["/x"]);
    }

    #[test]
    fn auth_sub_handle_reads_bound_principal() {
        use crate::harness_auth::{enter_auth_principal, AuthPrincipal};
        let _principal = enter_auth_principal(AuthPrincipal {
            subject: "k_123".to_string(),
            scheme: "apikey".to_string(),
            scopes: ["admin:dlq:write", "read:events"]
                .iter()
                .map(|scope| scope.to_string())
                .collect(),
            kind: Some("operator".to_string()),
        });
        let source = r#"
fn main(harness: Harness) {
  __io_println(harness.auth.is_authenticated())
  __io_println(harness.auth.subject())
  __io_println(harness.auth.scheme())
  __io_println(harness.auth.kind())
  __io_println(harness.auth.has_scope("admin:dlq:write"))
  __io_println(harness.auth.has_scope("missing:scope"))
  __io_println(len(harness.auth.scopes()))
}
"#;
        let output = run_harness_source(source, Harness::real()).expect("dispatch succeeds");
        assert_eq!(output, "true\nk_123\napikey\noperator\ntrue\nfalse\n2\n");
    }

    #[test]
    fn auth_sub_handle_without_principal_reports_anonymous() {
        // No `enter_auth_principal` guard — the dispatch is unauthenticated,
        // so the presence/scope getters degrade rather than error and
        // `subject()` raises the canonical Auth error.
        let source = r#"
fn main(harness: Harness) {
  if harness.auth.is_authenticated() { __io_println("auth") } else { __io_println("anon") }
  __io_println(harness.auth.has_scope("x"))
  __io_println(len(harness.auth.scopes()))
}
"#;
        let output = run_harness_source(source, Harness::real()).expect("dispatch succeeds");
        assert_eq!(output, "anon\nfalse\n0\n");

        let error = run_harness_source(
            r"fn main(harness: Harness) { harness.auth.subject() }",
            Harness::real(),
        )
        .expect_err("subject() requires a bound principal");
        assert!(
            error.contains("no principal bound"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn secrets_sub_handle_uses_provider_scope_and_audit_context() {
        use crate::harness_auth::{enter_auth_principal, AuthPrincipal};
        use crate::harness_tenant::enter_tenant;
        use crate::observability::request_id::enter_request_id;

        let provider = RecordingSecretProvider::default();
        let harness = Harness::real().with_secret_provider(Arc::new(provider.clone()));
        let _tenant = enter_tenant(crate::TenantId::new("tenant-a"));
        let _request = enter_request_id("req-499");
        let _principal = enter_auth_principal(AuthPrincipal {
            subject: "api-key-1".to_string(),
            scheme: "apikey".to_string(),
            scopes: ["secrets:read", "secrets:write"]
                .iter()
                .map(|scope| scope.to_string())
                .collect(),
            kind: Some("tenant_api_key".to_string()),
        });

        let source = r#"
fn main(harness: Harness) {
  let scope = {kind: "workspace", id: "workspace-a"}
  let written = harness.secrets.write("github.token", "v1", scope, 5000)
  __io_println(written.provider)
  __io_println(written.scope.kind)
  __io_println(written.scope.id)
  __io_println(written.id.namespace)
  __io_println(written.version)
  __io_println(harness.secrets.read("github.token", scope))
  let rotated = harness.secrets.rotate("github.token", { -> "v2" }, scope, {grace_ms: 250, ttl_ms: 7500})
  __io_println(rotated.from_version)
  __io_println(rotated.to_version)
  let grant = harness.secrets.lease("github.token", 1000, scope)
  __io_println(grant.value)
  __io_println(grant.scope.id)
}
"#;
        let output = run_harness_source(source, harness).expect("dispatch succeeds");
        assert_eq!(
            output,
            "recording\nworkspace\nworkspace-a\nharn.workspace.workspace-a\n1\nv1\n1\n2\nv2\nworkspace-a\n"
        );

        let calls = provider.calls();
        assert_eq!(
            calls.iter().map(|call| call.operation).collect::<Vec<_>>(),
            vec!["write", "read", "rotate", "lease"]
        );
        for call in &calls {
            assert_eq!(
                call.scope,
                crate::secrets::SecretScope::workspace("workspace-a")
            );
            assert_eq!(call.request_id.as_deref(), Some("req-499"));
            assert_eq!(call.actor_subject.as_deref(), Some("api-key-1"));
            assert_eq!(call.actor_kind.as_deref(), Some("tenant_api_key"));
        }
        assert_eq!(calls[0].ttl_ms, Some(5_000));
        assert_eq!(calls[2].grace_ms, Some(250));
        assert_eq!(calls[2].ttl_ms, Some(7_500));
        assert_eq!(calls[3].duration_ms, Some(1_000));
    }

    #[test]
    fn mock_harness_replays_canned_responses_and_records_calls() {
        let harness = Harness::mock()
            .clock_at_unix_ms(1_700_000_000_000)
            .env("KEY", "value")
            .fs_read("/x", b"data".to_vec())
            .random_u64(42)
            .net_get("https://example.test", "body")
            .build();

        let output = run_harness_source(
            r#"
fn main(harness: Harness) {
  harness.stdio.print("partial ")
  harness.stdio.println("line")
  __io_println(harness.term.width())
  __io_println(harness.term.height())
  __io_println(harness.clock.now_ms())
  harness.clock.sleep_ms(250)
  __io_println(harness.clock.now_ms())
  __io_println(harness.clock.monotonic_ms())
  __io_println(harness.env.get("KEY"))
  __io_println(harness.fs.read_text("/x"))
  __io_println(harness.fs.exists("/missing"))
  __io_println(harness.random.gen_u64())
  __io_println(harness.net.get("https://example.test"))
  __io_println(harness.crypto.sha256(""))
  __io_println(len(harness.llm.catalog()) > 0)
}
"#,
            harness.clone(),
        )
        .expect("mock harness run succeeds");

        assert_eq!(harness.captured_stdio(), "partial line\n");
        assert_eq!(
            output,
            "80\n24\n1700000000000\n1700000000250\n250\nvalue\ndata\nfalse\n42\nbody\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\ntrue\n"
        );
        let observed: Vec<_> = harness
            .calls()
            .into_iter()
            .map(|call| (call.sub_handle, call.method))
            .collect();
        assert_eq!(
            observed,
            vec![
                (HarnessKind::Stdio, "print".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Term, "width".to_string()),
                (HarnessKind::Term, "height".to_string()),
                (HarnessKind::Clock, "now_ms".to_string()),
                (HarnessKind::Clock, "sleep_ms".to_string()),
                (HarnessKind::Clock, "now_ms".to_string()),
                (HarnessKind::Clock, "monotonic_ms".to_string()),
                (HarnessKind::Env, "get".to_string()),
                (HarnessKind::Fs, "read_text".to_string()),
                (HarnessKind::Fs, "exists".to_string()),
                (HarnessKind::Random, "gen_u64".to_string()),
                (HarnessKind::Net, "get".to_string()),
                (HarnessKind::Crypto, "sha256".to_string()),
                (HarnessKind::Llm, "catalog".to_string()),
            ]
        );
    }

    #[test]
    fn mock_harness_records_repeated_cached_harness_method_calls() {
        let harness = Harness::mock().env("KEY", "value").build();

        run_harness_source(
            r#"
fn main(harness: Harness) {
  var i = 0
  while i < 3 {
    let _ = harness.clock.elapsed()
    let value = harness.env.get_or("KEY", "")
    harness.stdio.println(value)
    i = i + 1
  }
}
"#,
            harness.clone(),
        )
        .expect("mock harness run succeeds");

        assert_eq!(harness.captured_stdio(), "value\nvalue\nvalue\n");
        let observed: Vec<_> = harness
            .calls()
            .into_iter()
            .map(|call| (call.sub_handle, call.method))
            .collect();
        assert_eq!(
            observed,
            vec![
                (HarnessKind::Clock, "elapsed".to_string()),
                (HarnessKind::Env, "get_or".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Clock, "elapsed".to_string()),
                (HarnessKind::Env, "get_or".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
                (HarnessKind::Clock, "elapsed".to_string()),
                (HarnessKind::Env, "get_or".to_string()),
                (HarnessKind::Stdio, "println".to_string()),
            ]
        );
    }

    #[test]
    fn mock_harness_replays_random_values_fifo() {
        let harness = Harness::mock()
            .random_u64(7)
            .random_u64(11)
            .random_u64(u64::MAX)
            .build();

        let output = run_harness_source(
            r"
fn main(harness: Harness) {
  __io_println(harness.random.gen_u64())
  __io_println(harness.random.gen_u64())
  __io_println(harness.random.gen_u64())
}
",
            harness,
        )
        .expect("mock random succeeds");

        assert_eq!(output, "7\n11\n9223372036854775807\n");
    }

    #[test]
    fn mock_harness_reports_missing_canned_responses() {
        let cases = [
            (
                r#"fn main(harness: Harness) { harness.fs.read_text("/missing") }"#,
                "MockHarness has no fs_read response for /missing",
            ),
            (
                r"fn main(harness: Harness) { harness.random.gen_u64() }",
                "MockHarness has no random_u64 response",
            ),
            (
                r#"fn main(harness: Harness) { harness.net.get("https://missing.test") }"#,
                "MockHarness has no net_get response for https://missing.test",
            ),
            (
                r#"fn main(harness: Harness) { harness.process.spawn_captured({cmd: "printf", args: ["x"]}) }"#,
                "MockHarness has no process spawn response",
            ),
        ];

        for (source, expected) in cases {
            let error = run_harness_source(source, Harness::mock().build())
                .expect_err("missing mock response fails");
            assert!(
                error.contains(expected),
                "expected `{expected}` in `{error}`"
            );
        }
    }

    #[test]
    fn mock_harness_records_failed_calls() {
        let harness = Harness::mock().build();
        let error = run_harness_source(
            r#"fn main(harness: Harness) { harness.net.get("https://missing.test") }"#,
            harness.clone(),
        )
        .expect_err("missing mock response fails");

        assert!(error.contains("MockHarness has no net_get response"));
        assert_eq!(
            harness.calls(),
            vec![HarnessCall {
                sub_handle: HarnessKind::Net,
                method: "get".to_string(),
                args: vec!["https://missing.test".to_string()],
            }]
        );
    }

    #[test]
    fn mock_harness_captures_stderr_separately_from_stdout() {
        let harness = Harness::mock().build();
        run_harness_source(
            r#"
fn main(harness: Harness) {
  harness.stdio.println("stdout line")
  harness.stdio.eprint("err ")
  harness.stdio.eprintln("trail")
}
"#,
            harness.clone(),
        )
        .expect("stderr capture run succeeds");
        assert_eq!(harness.captured_stdio(), "stdout line\n");
        assert_eq!(harness.captured_stderr(), "err trail\n");
    }

    #[test]
    fn mock_harness_replays_stdin_lines_for_read_and_prompt() {
        let harness = Harness::mock()
            .stdin_line("first")
            .stdin_line("second")
            .build();
        let output = run_harness_source(
            r#"
fn main(harness: Harness) {
  harness.stdio.println(harness.stdio.read_line())
  harness.stdio.println(harness.stdio.prompt("answer: "))
  let eof = harness.stdio.read_line({trim: false})
  harness.stdio.println(eof.status)
}
"#,
            harness.clone(),
        )
        .expect("stdin replay succeeds");
        // All stdio writes route to the mock capture buffer; vm.output stays empty.
        assert_eq!(output, "");
        assert_eq!(harness.captured_stdio(), "first\nanswer: second\neof\n");
    }

    #[test]
    fn mock_harness_replays_password_input_without_stdout_echo() {
        let harness = Harness::mock().stdin_line("secret").build();
        let output = run_harness_source(
            r#"
fn main(harness: Harness) {
  __io_println(harness.term.read_password("password: "))
}
"#,
            harness.clone(),
        )
        .expect("stdin replay succeeds");

        assert_eq!(output, "secret\n");
        assert_eq!(harness.captured_stdio(), "");
        assert_eq!(harness.captured_stderr(), "password: ");
        assert_eq!(
            harness.calls(),
            vec![HarnessCall {
                sub_handle: HarnessKind::Term,
                method: "read_password".to_string(),
                args: vec!["password: ".to_string()],
            }]
        );
    }

    #[test]
    fn mock_harness_rejects_wrong_argument_types() {
        let error = run_harness_source(
            r"fn main(harness: Harness) { harness.fs.read_text(1) }",
            Harness::mock().build(),
        )
        .expect_err("wrong argument type fails");

        // A wrong-typed argument is rejected by one of two layers, and which
        // one fires depends on whether the process-global builtin-signature
        // registry was already populated (by any prior `register_vm_stdlib`
        // call in the test binary) when `compile_source` ran:
        //   - empty registry  -> the harness runtime guard (`string_arg`)
        //   - populated registry -> static type-check at compile time, which
        //     matches the `read_text` method against the same-named stdlib
        //     `read_text(path: string)` signature.
        // Both correctly reject the int, so accept either message.
        let runtime_rejection =
            error.contains("HarnessFs.read_text expects string argument 1, got int");
        let static_rejection = error.contains("argument 1 `path`: expected string, found int");
        assert!(
            runtime_rejection || static_rejection,
            "expected a string/int type rejection for read_text, got: {error}"
        );
    }

    #[test]
    fn real_harness_fs_write_outside_workspace_roots_surfaces_cap_201() {
        use crate::orchestration::{
            clear_execution_policy_stacks, push_execution_policy, CapabilityPolicy, SandboxProfile,
        };
        clear_execution_policy_stacks();
        let temp = tempfile::tempdir().unwrap();
        let policy = CapabilityPolicy {
            sandbox_profile: SandboxProfile::Worktree,
            workspace_roots: vec![temp.path().to_string_lossy().into_owned()],
            ..CapabilityPolicy::default()
        };
        push_execution_policy(policy);
        let outside = std::env::temp_dir().join("harn_e4_4_cap_201_outside.txt");
        let source = format!(
            r#"fn main(harness: Harness) {{ harness.fs.write_text("{}", "x") }}"#,
            outside.to_string_lossy().replace('\\', "/"),
        );
        let error = run_harness_source(&source, Harness::real())
            .expect_err("write outside workspace_roots must reject");
        clear_execution_policy_stacks();
        assert!(
            error.contains("HARN-CAP-201"),
            "expected HARN-CAP-201 prefix, got: {error}"
        );
        assert!(
            error.contains("sandbox violation"),
            "deny should keep the underlying sandbox-rejection message, got: {error}"
        );
    }

    fn run_harness_source(source: &str, harness: Harness) -> Result<String, String> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async move {
                    let chunk = crate::compile_source(source)?;
                    let mut vm = crate::Vm::new();
                    crate::stdlib::register_vm_stdlib(&mut vm);
                    vm.set_harness(harness);
                    vm.execute(&chunk)
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(vm.output().to_string())
                })
                .await
        })
    }
}
