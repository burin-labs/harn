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
