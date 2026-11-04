# Provider conversation state

Harn owns canonical conversation history. A provider continuation is an optional
wire optimization over that history, scoped to one admitted route and account.
It must not become the source of truth for replay, compaction, branching,
delegation, pricing or policy.

The initial `harn_llm::wire::anthropic::thread` module is an offline protocol
primitive. Existing provider dispatch and sessions do not call it. Its presence
does not declare live capability, enable provider storage, change retention
policy, or qualify an account or model.

## Ownership

The host admits feature eligibility and retention consent before constructing a
`MessageThread`. The host supplies the full final Anthropic body, including all
provider-visible history and final generation settings. It also supplies opaque
route, credential, policy and beta epochs. These are host identity generations,
not credentials, bearer tokens or provider response IDs.

The codec compares the complete projected message prefix and fixed request
fields. A changed prefix or fixed field starts a new thread with full history.
The comparison treats a string as the API's shorthand for one plain text block;
otherwise it is exact. A changed cache marker may cause an extra create request,
but cannot silently omit changed history. Unknown request fields
are refused until their thread semantics are reviewed. MCP, containers,
compaction and server fallbacks remain outside this initial subset.

One prepared turn exclusively borrows its thread owner. The old cursor is
removed before preparation. A failed preparation, cancellation, dropped stream
or discarded turn therefore cannot leave a reusable old cursor. No thread state
is serialized, cloned, or included in a derived debug representation.

Only a complete JSON response or terminal `message_stop`, accepted by the host's
normal provider response decoder, may reach `complete`. The codec checks the
response envelope and content shape: content must be an array of objects whose
`type` fields are nonempty strings. It does not validate type-specific fields;
for example, a typed text or tool-use block with missing required fields can
pass this structural gate. The host must apply its semantic validation before
calling it. Refusal, truncation, missing identifiers, changed models, invalid
envelopes or content shapes, and provider iteration records do not advance the
cursor.
`pause_turn` permits an empty continuation and refuses new input on that cursor.
The returned cursor remains a candidate: the service does not attest that it
stored the response and may expire it at any time.

## Physical attempts and recovery

`PreparedTurn::body` is the physical request. `full_request` retains full history
for admission, receipts and recovery. `required_beta` supplies the thread beta
only for create and continue attempts. A transport must not mutate a prepared
body after admission or before sending it.

Recovery recognizes the documented structured thread error code and HTTP status;
an unrelated 404, authentication error, fingerprint mismatch, missing structured
code, network error or unknown provider error gets no automatic replay. A known
pre-generation continuation rejection can prepare one full-history create
attempt. A thread-unsupported rejection can prepare one byte-preserving
unthreaded attempt and disables threading for that scope. A second recovery is
refused.

Every physical attempt, including a replay, must pass the host's existing
inference and monetary admission and receive its own physical-call accounting.
The codec performs neither admission nor transport and must not bypass a refusal.
It does not retry dropped streams or execute tool side effects. Canonical local
tool execution and transcript publication remain under their existing owners.

## Activation and qualification

The following work remains before runtime adoption:

- Integrate one host-side continuation owner with session lifecycle and final
  wire preparation, without a second transcript or opaque-output serializer.
- Establish route/account identity epochs and explicit opt-in retention policy.
- Keep conservative admission refusals until the billing shape is qualified;
  sending a shorter body does not make earlier model context unbillable.
- Run the deterministic codec cases and the owning source/binary drift checks
  through the normal permitted build path, then obtain independent review.
- Probe an explicitly approved account/model with synthetic data before adding
  a live capability declaration. No credentials or cursor values belong in
  public evidence.

The following rollout gates remain open; the offline primitive does not qualify
them:

- Exercise actual cached tool loops through Harn's encoder, response decoder and
  session history, including cache markers, signed blocks and tool results.
- Verify canonical response/request model identity and alias resolution. The
  current gate requires exact model equality and may decline an alias response.
- Establish explicit storage consent and enforce the effective privacy posture
  and account eligibility before any threaded dispatch.
- Verify admission and accounting for full billable context on every physical
  attempt, including structured-error recovery; smaller uploads do not establish
  token or price savings.
- Verify terminal events through the actual transport: only an accepted JSON
  reply or completed stream can advance a cursor; cancellation, truncation and
  parser rejection must leave it invalidated.

The protocol reference is Anthropic's
[message threads documentation](https://platform.claude.com/docs/en/build-with-claude/message-threads).
Provider state, prompt caching and durable hosted-agent execution are different
contracts. A Responses-shaped API alone does not establish stateful behavior.
