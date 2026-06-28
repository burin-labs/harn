// One-line docs surfaced as hover tooltips over Harn keywords and built-ins in
// the on-page code snippets: a small "editor" touch that lets people learn the
// language by reading the examples. Keep each note to a single plain sentence.
//
// This is UI copy; when the site gains more locales, move these strings into the
// i18n catalog. They live here for now so the highlighter stays self-contained.
export const KEYWORD_DOCS: Record<string, string> = {
  pipeline: "Declares a named pipeline: a top-level, replayable unit of agent work.",
  fn: "Defines a function.",
  let: "Binds a local value.",
  parallel: "Runs the iterations concurrently and joins their results.",
  each: "Iterates over a collection; with parallel, fans the work out.",
  for: "Loops over a collection in order.",
  in: "Names the collection a loop draws from.",
  retry: "Re-runs the block on failure, up to the given count.",
  spawn: "Starts a concurrent task you can await later.",
  agent_loop: "Runs a tool-using agent until it finishes or hits a limit.",
  llm_call: "Calls a model. The first argument is the prompt, the second is the system prompt.",
  log: "Writes a value to the run log.",
  read_file: "Reads a file through the harness, a capability checked before the run.",
  read_text: "Reads a file's text through the harness capability layer.",
  tool_select: "Chooses which tool the model should call next.",
  if: "Runs the block when the condition holds.",
  else: "Runs when the matching if condition is false.",
  while: "Repeats the block while the condition holds.",
  return: "Returns a value from the function or pipeline.",
  break: "Exits the nearest loop.",
  continue: "Skips to the next loop iteration.",
}
