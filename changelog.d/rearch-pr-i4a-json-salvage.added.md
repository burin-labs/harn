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
