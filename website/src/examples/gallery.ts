import reviewCaptainSource from "../../../crates/harn-cli/assets/demo/review-captain/scenario.harn?raw"
import mergeCaptainSource from "../../../crates/harn-cli/assets/demo/merge-captain/scenario.harn?raw"
import mcpHostSource from "../../../crates/harn-cli/assets/demo/mcp-host/scenario.harn?raw"
import stdlibToolkitSource from "../../../crates/harn-cli/assets/demo/stdlib-toolkit/scenario.harn?raw"

const GITHUB_DEMO_ROOT = "https://github.com/burin-labs/harn/blob/main/crates/harn-cli/assets/demo"

// The bundled scenarios open with a long /** … */ design note that buries the
// code below the fold. Strip that leading block comment for display and copy so
// the runnable code leads; the full file (header and all) is one click away via
// "View source". The result is still valid, runnable Harn.
function leadWithCode(src: string): string {
  const body = src.replace(/^﻿/, "").replace(/^\s*\/\*[\s\S]*?\*\/\s*/, "")
  return body.trimEnd()
}

// Structural data only. Display copy (tab, title, outcome) lives in the i18n
// catalog under `landing.examples.scenarios`, keyed by `slug`. The `command` is
// a literal CLI invocation, not translatable prose, so it stays here.
export type ExampleScenario = {
  slug: keyof typeof import("../i18n/en").en.landing.examples.scenarios
  command: string
  sourcePath: string
  sourceHref: string
  docsHref: string
  source: string
}

const DEMO_DOCS_HREF = "/cli-reference.html#harn-demo"

export const exampleScenarios: ExampleScenario[] = [
  {
    slug: "review-captain",
    command: "harn demo review-captain",
    sourcePath: "crates/harn-cli/assets/demo/review-captain/scenario.harn",
    sourceHref: `${GITHUB_DEMO_ROOT}/review-captain/scenario.harn`,
    docsHref: DEMO_DOCS_HREF,
    source: leadWithCode(reviewCaptainSource),
  },
  {
    slug: "merge-captain",
    command: "harn demo merge-captain",
    sourcePath: "crates/harn-cli/assets/demo/merge-captain/scenario.harn",
    sourceHref: `${GITHUB_DEMO_ROOT}/merge-captain/scenario.harn`,
    docsHref: DEMO_DOCS_HREF,
    source: leadWithCode(mergeCaptainSource),
  },
  {
    slug: "mcp-host",
    command: "harn demo mcp-host",
    sourcePath: "crates/harn-cli/assets/demo/mcp-host/scenario.harn",
    sourceHref: `${GITHUB_DEMO_ROOT}/mcp-host/scenario.harn`,
    docsHref: DEMO_DOCS_HREF,
    source: leadWithCode(mcpHostSource),
  },
  {
    slug: "stdlib-toolkit",
    command: "harn demo stdlib-toolkit",
    sourcePath: "crates/harn-cli/assets/demo/stdlib-toolkit/scenario.harn",
    sourceHref: `${GITHUB_DEMO_ROOT}/stdlib-toolkit/scenario.harn`,
    docsHref: DEMO_DOCS_HREF,
    source: leadWithCode(stdlibToolkitSource),
  },
]
