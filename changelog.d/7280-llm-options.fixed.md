`removed-llm-options` (`HARN-LNT-050`) now reports removed LLM option keys
on calls written in the typed `harness.llm.*` spelling, not only on the ambient
`llm_call`-family globals. A call site that adopted the spelling
`HARN-LNT-071` asks for previously stopped being checked without any signal.
