- Added three composable harness primitives to the standard library: `with_semantic_cache` (embedding-similarity
  response cache) and `with_result_schema` (Instructor-style validate-and-repair of a caller's structured return
  value) in `std/llm/handlers`, and `faithfulness_guard` (RAGAS-style RAG groundedness scoring) in
  `std/llm/faithfulness`, plus a raw `embed` primitive in `std/memory` exposing the host `memory.embed` capability.
