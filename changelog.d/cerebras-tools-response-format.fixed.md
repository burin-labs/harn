OpenAI-compatible chat-completions handling now avoids strict-provider failures by
omitting invalid request combinations, dropping orphaned native tool results, and
splitting concatenated JSON tool-argument objects into dispatchable calls.
