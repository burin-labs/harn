- **Streaming no longer idle-times-out during a slow prefill.** The time to the
  first token (prefill) is processed with no SSE bytes, so a slow model on a
  large context could trip the short inter-token idle timeout *before its first
  token* (observed: a local 35B model idle-timing-out mid-prefill). The first
  token now gets a more generous budget (default 4x the idle timeout, min 120s,
  still bounded by the overall stream deadline; tune with
  `HARN_LLM_FIRST_TOKEN_TIMEOUT`); inter-token gaps keep the normal idle timeout.
