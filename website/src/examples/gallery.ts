import reviewCaptainSource from "../../../crates/harn-cli/assets/demo/review-captain/scenario.harn?raw"
import reviewCaptainReviewPrompt from "../../../crates/harn-cli/assets/demo/review-captain/review.harn.prompt?raw"
import reviewCaptainClarifyPrompt from "../../../crates/harn-cli/assets/demo/review-captain/clarification.harn.prompt?raw"
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

// `.harn.prompt` templates carry no design-note header, so they display as-is.
function trimmed(src: string): string {
  return src.replace(/^﻿/, "").trimEnd()
}

// `harn` gets full syntax highlighting; `prompt` files are render_prompt
// templates, highlighted only for their `{{ … }}` interpolation markers.
export type ExampleFileLang = "harn" | "prompt"

// One file in a scenario's bundle. Most scenarios are a single scenario.harn;
// multi-file scenarios (review-captain) add sibling `.harn.prompt` templates,
// which the demo runner materializes and resolves via `render_prompt`.
export type ExampleFile = {
  name: string
  path: string
  href: string
  lang: ExampleFileLang
  source: string
}

// Structural data only. Display copy (tab, title, outcome) lives in the i18n
// catalog under `landing.examples.scenarios`, keyed by `slug`. The `command` is
// a literal CLI invocation, not translatable prose, so it stays here.
export type ExampleScenario = {
  slug: keyof typeof import("../i18n/en").en.landing.examples.scenarios
  command: string
  docsHref: string
  files: ExampleFile[]
}

const DEMO_DOCS_HREF = "/cli-reference.html#harn-demo"

// Build a file entry from a scenario-relative path, wiring up its GitHub href.
function demoFile(slug: string, name: string, lang: ExampleFileLang, source: string): ExampleFile {
  const path = `crates/harn-cli/assets/demo/${slug}/${name}`
  return { name, path, href: `${GITHUB_DEMO_ROOT}/${slug}/${name}`, lang, source }
}

function scenarioFile(slug: string, source: string): ExampleFile {
  return demoFile(slug, "scenario.harn", "harn", leadWithCode(source))
}

export const exampleScenarios: ExampleScenario[] = [
  {
    slug: "review-captain",
    command: "harn demo review-captain",
    docsHref: DEMO_DOCS_HREF,
    files: [
      scenarioFile("review-captain", reviewCaptainSource),
      demoFile("review-captain", "review.harn.prompt", "prompt", trimmed(reviewCaptainReviewPrompt)),
      demoFile("review-captain", "clarification.harn.prompt", "prompt", trimmed(reviewCaptainClarifyPrompt)),
    ],
  },
  {
    slug: "merge-captain",
    command: "harn demo merge-captain",
    docsHref: DEMO_DOCS_HREF,
    files: [scenarioFile("merge-captain", mergeCaptainSource)],
  },
  {
    slug: "mcp-host",
    command: "harn demo mcp-host",
    docsHref: DEMO_DOCS_HREF,
    files: [scenarioFile("mcp-host", mcpHostSource)],
  },
  {
    slug: "stdlib-toolkit",
    command: "harn demo stdlib-toolkit",
    docsHref: DEMO_DOCS_HREF,
    files: [scenarioFile("stdlib-toolkit", stdlibToolkitSource)],
  },
]
