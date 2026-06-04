import mcpHostSource from "../../../crates/harn-cli/assets/demo/mcp-host/scenario.harn?raw"
import mergeCaptainSource from "../../../crates/harn-cli/assets/demo/merge-captain/scenario.harn?raw"
import stdlibToolkitSource from "../../../crates/harn-cli/assets/demo/stdlib-toolkit/scenario.harn?raw"

const GITHUB_DEMO_ROOT = "https://github.com/burin-labs/harn/blob/main/crates/harn-cli/assets/demo"

export type ExampleScenario = {
  slug: string
  tab: string
  title: string
  outcome: string
  command: string
  runHref: string
  sourcePath: string
  sourceHref: string
  source: string
}

export const exampleScenarios: ExampleScenario[] = [
  {
    slug: "mcp-host",
    tab: "MCP host",
    title: "Register supervised MCP servers without a live server fixture.",
    outcome: "The receipt proves lazy registration, status snapshots, and graceful stop paths stay offline-runnable.",
    command: "harn demo mcp-host",
    runHref: "/cli-reference.html#harn-demo",
    sourcePath: "crates/harn-cli/assets/demo/mcp-host/scenario.harn",
    sourceHref: `${GITHUB_DEMO_ROOT}/mcp-host/scenario.harn`,
    source: mcpHostSource.trimEnd(),
  },
  {
    slug: "merge-captain",
    tab: "Merge captain",
    title: "Triage a mock PR queue with deterministic replay.",
    outcome: "Three mocked PRs become a structured merge, handoff, or block receipt using the bundled LLM tape.",
    command: "harn demo merge-captain",
    runHref: "/cli-reference.html#harn-demo",
    sourcePath: "crates/harn-cli/assets/demo/merge-captain/scenario.harn",
    sourceHref: `${GITHUB_DEMO_ROOT}/merge-captain/scenario.harn`,
    source: mergeCaptainSource.trimEnd(),
  },
  {
    slug: "stdlib-toolkit",
    tab: "Stdlib toolkit",
    title: "Assemble an XML prompt context from local stdlib primitives.",
    outcome: "Clone, merge, dedupe, XML round-trip, wrap, and indent steps compose into a checked prompt receipt.",
    command: "harn demo stdlib-toolkit",
    runHref: "/cli-reference.html#harn-demo",
    sourcePath: "crates/harn-cli/assets/demo/stdlib-toolkit/scenario.harn",
    sourceHref: `${GITHUB_DEMO_ROOT}/stdlib-toolkit/scenario.harn`,
    source: stdlibToolkitSource.trimEnd(),
  },
]
