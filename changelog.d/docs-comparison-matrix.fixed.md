**`npm run dev` renders the docs site again.** The client entry hydrated
whenever the root element had any child node, and in development the unreplaced
`<!--app-html-->` placeholder is itself a child, so React hydrated against
markup that was not there and left a blank page. It now tests for an element.
The Python comparison snippet in `why-harn.md` called `harness.stdio.log`, a
Harn builtin, instead of `print`, and the Harn snippet beside it carried an
unused parameter.

**Docs fact-check pass.** `why-harn.md` and `builtins.md` both named a stale
short list of built-in LLM providers — they omitted Gemini, Groq, and DeepSeek
while naming HuggingFace, which is supported but not featured. Both now give
the real count (44) and link the capability matrix. The AWS Strands link in
`sota-comparison.md` 404'd after Strands restructured its docs.
