// The single source of truth for how Harn compares with adjacent orchestration
// systems.
//
// Three surfaces read this file and nothing else: the `{{#comparison-matrix}}`
// directive that renders the table into `docs/src/how-harn-compares.md`, the
// legend that lets a reader pick columns, and the reference list at the foot of
// that page. Before this file existed the roster of compared systems was kept
// by hand in all three places and drifted between them.
//
// Nothing here is generated and nothing here is committed twice, so there is no
// drift check to run: `tsc` is the check. A rating that is not one of the four
// literals below, or a system id that no capability rates, fails the build.

export type Rating = "yes" | "partial" | "no" | "unknown"

export interface System {
  id: string
  /** Column header. Keep short; the table has to stay scannable. */
  name: string
  /** Primary public documentation, linked from the reference list. */
  href: string
  /** One line of positioning, shown in the legend and the reference list. */
  summary: string
  /** Shown before the reader picks columns. Harn is always pinned. */
  shownByDefault: boolean
  /**
   * When this column's claims were last checked against public documentation.
   * Rendered on the page so a stale column is visible rather than implied.
   */
  verified: string
  /**
   * Anchor of this system's section in `concepts/sota-comparison.md`, where its
   * vocabulary is mapped onto Harn's.
   *
   * That page and this table are not the same artifact and neither contains the
   * other: it maps terminology for systems we never rate (OpenAI, Anthropic,
   * Mastra, Cloudflare, Strands), and we rate systems it has no section for.
   * So the roster is not unified here. What is owned here is the pointer, and
   * the content plugin rejects a dangling anchor, which makes a renamed heading
   * on that page a build failure rather than a dead link.
   */
  comingFrom?: string
}

export interface Capability {
  /** Must match the `###` heading slug in how-harn-compares.md. */
  id: string
  /** Row label in the table. */
  label: string
  /**
   * Set when a row is one Harn does not win. The legend uses it to show that
   * the row set is not selected to flatter Harn.
   */
  favorsOthers?: boolean
  /**
   * Work that is tracked and intended to change this row.
   *
   * The issue number is required by the type, and that is the whole point:
   * a published plan has to have something tracking it. Prose alone would let
   * an aspiration outlive the intent behind it, which is how a comparison page
   * turns into marketing. If work is dropped, the row loses its plan when the
   * issue closes unfixed, rather than quietly reading as a live promise.
   */
  plan?: { note: string; issue: number }
}

export interface Cell {
  rating: Rating
  /** One sentence on why, shown on hover and to screen readers. */
  note?: string
}

// ---------------------------------------------------------------------------
// Systems
// ---------------------------------------------------------------------------

export const SYSTEMS: System[] = [
  {
    id: "harn",
    name: "Harn",
    href: "https://harnlang.com",
    summary: "A language and runtime for agent orchestration.",
    shownByDefault: true,
    verified: "2026-08",
  },
  {
    id: "inngest",
    name: "Inngest / AgentKit",
    href: "https://www.inngest.com/ai",
    summary: "Durable steps and flow control for SDK-defined AI workflows.",
    shownByDefault: false,
    verified: "2026-08",
    comingFrom: "inngest",
  },
  {
    id: "temporal",
    name: "Temporal",
    href: "https://docs.temporal.io/",
    summary: "Durable workflow execution with deterministic replay.",
    shownByDefault: true,
    verified: "2026-08",
  },
  {
    id: "langgraph",
    name: "LangGraph",
    href: "https://docs.langchain.com/oss/python/langgraph/overview",
    summary: "Graph-structured agent state machines with checkpointing.",
    shownByDefault: true,
    verified: "2026-08",
    comingFrom: "langgraph",
  },
  {
    id: "baml",
    name: "BAML",
    href: "https://boundaryml.com/",
    summary: "A language for typed LLM calls that generates clients for your codebase.",
    shownByDefault: true,
    verified: "2026-08",
    comingFrom: "baml",
  },
  {
    id: "cursor",
    name: "Cursor Automations",
    href: "https://cursor.com/docs/cloud-agent/automations",
    summary: "Scheduled and triggered cloud coding agents.",
    shownByDefault: false,
    verified: "2026-08",
  },
  {
    id: "flue",
    name: "Flue",
    href: "https://flueframework.com/docs/guide/building-agents/",
    summary: "A TypeScript agent framework with a durable event stream.",
    shownByDefault: false,
    verified: "2026-08",
    comingFrom: "flue",
  },
]

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

export const CAPABILITIES: Capability[] = [
  { id: "orchestration-language", label: "Own orchestration language" },
  { id: "runtime-replay-contract", label: "Runtime replay contract" },
  { id: "model-aware-trigger-predicates", label: "Model-aware trigger predicates" },
  { id: "open-source-and-self-hostable", label: "Open source and self-hostable" },
  { id: "one-program-across-environments", label: "One program across environments" },
  { id: "cost-limits", label: "Cost limits in program code" },
  { id: "human-review-and-trust", label: "Human review and trust records" },
  { id: "model-and-infrastructure-choice", label: "Model and infrastructure choice" },
  { id: "sandboxed-by-default", label: "Sandboxed by default" },
  { id: "signed-versioned-packages", label: "Signed, versioned packages" },

  // Rows Harn does not win. A comparison whose every row favours the system
  // publishing it is an advertisement, so these are load-bearing.
  {
    id: "callable-from-an-existing-codebase",
    label: "Callable from an existing codebase",
    favorsOthers: true,
  },
  {
    id: "reuse-your-languages-libraries",
    label: "Reuse your language's libraries",
    favorsOthers: true,
  },
  { id: "managed-hosting-from-the-vendor", label: "Managed hosting from the vendor", favorsOthers: true },
  { id: "proven-at-production-scale", label: "Proven at production scale", favorsOthers: true },
  { id: "third-party-integration-catalog", label: "Third-party integration catalog", favorsOthers: true },
  {
    id: "futures-that-outlive-their-scope",
    label: "Futures that outlive their scope",
    favorsOthers: true,
  },
  {
    id: "small-install",
    label: "Small install",
    favorsOthers: true,
    plan: {
      note: "The Windows archive is three times the size of the others because it carries three identical copies of one executable. Shipping one and creating the aliases at install time would bring it in line.",
      issue: 7175,
    },
  },
]

// ---------------------------------------------------------------------------
// Ratings
// ---------------------------------------------------------------------------
//
// `unknown` is a real state, not a placeholder to be tidied away. It means no
// one has checked that cell against the system's documentation. It renders as
// an em dash and reads as "not verified", which is honest; a guess would not
// be. Cells carrying `unknown` are the page's research backlog.

export const RATINGS: Record<string, Record<string, Cell>> = {
  "orchestration-language": {
    harn: { rating: "yes", note: "Trigger policy, model calls, concurrency, budgets, and review live in one program." },
    inngest: { rating: "no", note: "Workflows are host-language callbacks registered with an SDK." },
    temporal: { rating: "no", note: "Workflows are ordinary code under deterministic constraints." },
    langgraph: { rating: "no", note: "Graphs are built with a Python or JavaScript library." },
    baml: { rating: "yes", note: "BAML is its own language for typed LLM functions, though scoped to the call boundary rather than the program around it." },
    cursor: { rating: "no", note: "Automations are configured, not programmed." },
    flue: { rating: "no", note: "Flue is a TypeScript framework." },
  },

  "runtime-replay-contract": {
    harn: { rating: "yes", note: "The runtime owns the transcript and event-log boundary, so replay sees the same model request, tool result, trigger event, and approval." },
    inngest: { rating: "partial", note: "Completed steps are memoized, so a model call inside a durable step isn't re-sent on resume." },
    temporal: { rating: "yes", note: "Workflow state replays when workflow code follows its deterministic constraints and side effects run in Activities." },
    langgraph: { rating: "partial", note: "Replay re-executes nodes after the selected checkpoint, including model calls, which may return different results." },
    baml: { rating: "no", note: "No durable execution or checkpointing; the journal is in-memory and per-run." },
    cursor: { rating: "no" },
    flue: { rating: "partial", note: "A durable event stream retains runtime events, without a deterministic effect-replay contract." },
  },

  "model-aware-trigger-predicates": {
    harn: { rating: "yes", note: "Predicates over events are runtime objects, including model-backed classifiers and budget policy." },
    inngest: { rating: "partial", note: "Flow control dispatches on events; a model-backed classifier is application code." },
    temporal: { rating: "no" },
    langgraph: { rating: "partial", note: "Conditional edges can call a model, but the application owns that classifier and its policy." },
    baml: { rating: "no", note: "BAML has no trigger system; dispatch belongs to the calling application." },
    cursor: { rating: "no" },
    flue: { rating: "unknown" },
  },

  "open-source-and-self-hostable": {
    harn: { rating: "yes", note: "Language, VM, orchestrator, EventLog contracts, connectors, and protocols are open, with a self-hostable deployment path." },
    inngest: { rating: "partial", note: "The core is open source; parts of the platform are a hosted product." },
    temporal: { rating: "yes", note: "Open-source server and SDKs, self-hostable." },
    langgraph: { rating: "partial", note: "The library is open source; the platform is a hosted product." },
    baml: { rating: "yes", note: "The compiler and runtime are open source." },
    cursor: { rating: "partial", note: "Self-hosted agent pools exist; the product isn't open source." },
    flue: { rating: "unknown" },
  },

  "one-program-across-environments": {
    harn: { rating: "yes", note: "One .harn program is the unit of review as a local script, CI job, orchestrator workflow, MCP server, ACP backend, or cloud workflow." },
    inngest: { rating: "partial", note: "One function runs across environments, inside the host runtime it was written for." },
    temporal: { rating: "partial", note: "Workers run anywhere, but each is bound to its host language SDK." },
    langgraph: { rating: "partial", note: "Graphs port across environments within Python or JavaScript." },
    baml: { rating: "partial", note: "One BAML definition generates clients for many languages, but it runs inside whichever application embeds it." },
    cursor: { rating: "no" },
    flue: { rating: "partial", note: "Flue targets Node.js and Cloudflare." },
  },

  "cost-limits": {
    harn: { rating: "yes", note: "Trigger budgets and runtime context put limits next to the workflow." },
    inngest: { rating: "partial", note: "Concurrency and throttling controls bound work, not spend." },
    temporal: { rating: "partial" },
    langgraph: { rating: "partial", note: "Recursion limits bound steps, not spend." },
    baml: { rating: "unknown" },
    cursor: { rating: "partial" },
    flue: { rating: "unknown" },
  },

  "human-review-and-trust": {
    harn: { rating: "yes", note: "A review step is recorded with agent session lineage and trust graph data, so it stays part of the audit trail." },
    inngest: { rating: "partial", note: "AgentKit documents tool-approval human-in-the-loop." },
    temporal: { rating: "partial", note: "Signals and updates can implement approval; the pattern is application code." },
    langgraph: { rating: "partial", note: "Interrupts pause a graph for review; the trust record is application code." },
    baml: { rating: "no", note: "No human-in-the-loop primitive." },
    cursor: { rating: "partial" },
    flue: { rating: "unknown" },
  },

  "model-and-infrastructure-choice": {
    harn: { rating: "yes", note: "Hosted providers, OpenAI-compatible endpoints, local model servers, and Ollama, including networks without public internet access." },
    inngest: { rating: "partial" },
    temporal: { rating: "partial" },
    langgraph: { rating: "partial" },
    baml: { rating: "yes", note: "A client registry with retry, fallback, and round-robin wrappers." },
    cursor: { rating: "partial" },
    flue: { rating: "unknown" },
  },

  // --- Rows Harn does not win ---------------------------------------------

  "callable-from-an-existing-codebase": {
    harn: { rating: "partial", note: "The Python and TypeScript SDKs are REST clients for a running Harn server, not in-process bindings. In-process means embedding the runtime in Rust." },
    inngest: { rating: "yes", note: "An SDK you import into the application you already have." },
    temporal: { rating: "yes", note: "SDKs for Go, Java, Python, TypeScript, .NET, and PHP." },
    langgraph: { rating: "yes", note: "A library you import." },
    baml: { rating: "yes", note: "Generates typed clients for Python, TypeScript, Go, Java, C#, Rust, and more. This is the clearest difference in distribution model." },
    cursor: { rating: "no" },
    flue: { rating: "yes", note: "A TypeScript library you import." },
  },

  "reuse-your-languages-libraries": {
    harn: { rating: "partial", note: "The stdlib and host capabilities cover a lot, but you can't reach for an arbitrary pip or npm package inside a workflow." },
    inngest: { rating: "yes", note: "Ordinary host-language code." },
    temporal: { rating: "yes", note: "Ordinary host-language code, within deterministic constraints." },
    langgraph: { rating: "yes", note: "Ordinary Python or JavaScript." },
    baml: { rating: "partial", note: "BAML owns the call boundary; surrounding logic stays in your language." },
    cursor: { rating: "no" },
    flue: { rating: "yes", note: "Ordinary TypeScript." },
  },

  "managed-hosting-from-the-vendor": {
    harn: { rating: "partial", note: "Self-hosting is a first-class path; managed hosting is early." },
    inngest: { rating: "yes", note: "Inngest Cloud." },
    temporal: { rating: "yes", note: "Temporal Cloud." },
    langgraph: { rating: "yes", note: "LangGraph Platform." },
    baml: { rating: "partial" },
    cursor: { rating: "yes", note: "The product is the hosted service." },
    flue: { rating: "unknown" },
  },

  "proven-at-production-scale": {
    harn: { rating: "no", note: "Pre-1.0. Surface-level breaking changes are possible between minor and patch releases." },
    inngest: { rating: "yes" },
    temporal: { rating: "yes", note: "Years of at-scale production operation." },
    langgraph: { rating: "yes" },
    baml: { rating: "partial", note: "Also pre-1.0." },
    cursor: { rating: "partial" },
    flue: { rating: "unknown" },
  },

  "third-party-integration-catalog": {
    harn: { rating: "no", note: "A small connector set, deliberately." },
    inngest: { rating: "partial" },
    temporal: { rating: "partial" },
    langgraph: { rating: "yes", note: "The LangChain integration catalog is the largest in this table." },
    baml: { rating: "no" },
    cursor: { rating: "partial" },
    flue: { rating: "unknown" },
  },

  "futures-that-outlive-their-scope": {
    harn: { rating: "no", note: "Concurrency is scoped: a task doesn't outlive the scope that created it. That makes lifetimes obvious and leaks harder, at the cost of the freedom in the next column." },
    inngest: { rating: "yes", note: "Host-language async." },
    temporal: { rating: "yes", note: "Detached child workflows outlive their parent." },
    langgraph: { rating: "yes", note: "Host-language async." },
    baml: { rating: "yes", note: "Green threads with spawn and await. BAML rejected structured concurrency deliberately: a future outlives its creating scope and there's no automatic cancellation on scope exit." },
    cursor: { rating: "unknown" },
    flue: { rating: "yes", note: "Host-language async." },
  },

  "sandboxed-by-default": {
    harn: { rating: "yes", note: "`harn run` confines a script to its project directory before the VM starts, and the OS confines any subprocess it spawns. Opting out is an explicit --no-sandbox flag that warns." },
    inngest: { rating: "no", note: "Functions run in your own process with your own permissions." },
    temporal: { rating: "no", note: "Workers run your code with your permissions. The Python SDK's workflow sandbox isolates global state and restricts non-deterministic calls; its own documentation says it is not completely isolated, and it does not confine the filesystem, network, or process." },
    langgraph: { rating: "no", note: "A library in your process, with your permissions." },
    baml: { rating: "no", note: "The generated client runs in your process, with your permissions." },
    cursor: { rating: "partial", note: "Each cloud agent gets its own Firecracker microVM in a separate AWS account, which is strong isolation. It is operated by the vendor for its own hosted agents, rather than a boundary your program declares and carries anywhere it runs." },
    flue: { rating: "unknown" },
  },

  "signed-versioned-packages": {
    harn: { rating: "yes", note: "Packages carry a lockfile and a verify contract, and filesystem-backed skills can require an Ed25519 signature chain before an agent loads their instructions." },
    inngest: { rating: "partial", note: "Inherits npm or PyPI packaging and signing; the workflow itself is not a separately versioned unit." },
    temporal: { rating: "partial", note: "Inherits host-ecosystem packaging and signing." },
    langgraph: { rating: "partial", note: "Inherits host-ecosystem packaging and signing." },
    baml: { rating: "partial", note: "Inherits host-ecosystem packaging for the generated client." },
    cursor: { rating: "no" },
    flue: { rating: "unknown" },
  },

  "small-install": {
    harn: { rating: "no", note: "One binary, 71 to 79 MB compressed on macOS and Linux as of v0.10.114. The Windows archive is 232 MB, which is a packaging defect rather than the runtime's real weight." },
    inngest: { rating: "yes", note: "An ordinary npm or PyPI package." },
    temporal: { rating: "partial", note: "The SDK is a small package; a self-hosted server and its datastore are not." },
    langgraph: { rating: "yes", note: "An ordinary PyPI or npm package." },
    baml: { rating: "yes", note: "An ordinary package plus a generator." },
    cursor: { rating: "partial", note: "Agents run server-side, so there is no local runtime to install, but the editor itself is a full desktop application." },
    flue: { rating: "yes", note: "An ordinary npm package." },
  },
}
