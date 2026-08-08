// English message catalog: the single source of every user-facing string on
// harnlang.com. Add a sibling locale file (e.g. `fr.ts`) with the same shape and
// register it in `index.ts` to localize the site; nothing else needs to change.
//
// Conventions:
//   - Keys are grouped by surface (nav, footer, landing, …), not by component.
//   - `{name}` placeholders are filled with `format()` from `index.ts`.
//   - Code identifiers, CLI commands, URLs, and file paths are NOT here. They
//     are not translatable content and live with their structural data.
export const en = {
  common: {
    siteName: "Harn",
    github: "GitHub",
  },

  banner: {
    label: "Pre-release",
    prerelease: "Harn is pre-1.0 — the language, standard library, and CLI may change between releases.",
    changelogLink: "See the release notes",
  },

  nav: {
    brandHomeAria: "Harn home",
    docs: "Docs",
    reference: "Reference",
    search: "Search",
    searchAria: "Search documentation",
    cmdK: "⌘K",
    githubAria: "Harn on GitHub",
  },

  theme: {
    toLight: "Switch to light mode",
    toDark: "Switch to dark mode",
  },

  search: {
    placeholder: "Search the docs…",
    esc: "esc",
    loading: "Loading…",
    noResults: "No results.",
  },

  footer: {
    tagline: "The pipeline-oriented language for building and operating AI agents.",
    docsTitle: "Documentation",
    projectTitle: "Project",
    communityTitle: "Community",
    introduction: "Introduction",
    gettingStarted: "Getting started",
    languageReference: "Language reference",
    cookbook: "Cookbook",
    github: "GitHub",
    releases: "Releases",
    playground: "Playground",
    issues: "Issues",
    discussions: "Discussions",
    contributing: "Contributing",
    copyright: "© {year} Burin Labs. Harn is open source.",
  },

  notFound: {
    code: "404",
    title: "Page not found",
    body: "That page doesn’t exist. It may have moved, so try searching or head back to the docs.",
    toDocs: "Go to docs",
    toHome: "Home",
  },

  docs: {
    previous: "Previous",
    next: "Next",
    onThisPage: "On this page",
    editOnGitHub: "Edit this page on GitHub",
    sectionsAria: "Documentation sections",
  },

  mockup: {
    aria: "A Harn pipeline definition running as a live agent",
    fileLabel: "review.harn",
    runLabel: "harn run",
    thought: "Reading diff for crates/harn-vm…",
    toolCall: "llm_call · claude-opus-4-8 · 2 tools",
    // The finding renders {symbol} as inline code.
    findingPrefix: "Flagged 3 issues: an unguarded unwrap in ",
    findingSymbol: "call_closure",
    findingSuffix: ", a missing bounds check, and one stale doc snippet.",
    spawn: "spawn agent · deadline 30s",
    replay: "deterministic replay on",
    findings: "3 findings",
  },

  landing: {
    hero: {
      headline: "Build and operate AI agents in one language.",
      subhead:
        "Harn is a pipeline-oriented language for AI agents. LLM calls, tools, capability checks, durable steps, and deterministic replay are language and standard-library features, not SDKs you wire together yourself.",
      getStarted: "Get started",
      readDocs: "Read the docs",
      github: "GitHub",
      facts: [
        "Open source, written in Rust",
        "Deterministic replay",
        "Capability-safe by default",
        "Speaks MCP, ACP & A2A",
      ],
    },

    examples: {
      sectionTitle: "Runnable examples with real receipts",
      sectionBody:
        "The same checked scenario files ship in the CLI demo bundle and run locally with deterministic fixtures.",
      tablistAria: "Runnable Harn examples",
      filesAria: "Scenario files",
      multiFileNote:
        "This scenario ships more than one file. The prompts live in sibling .harn.prompt templates and load with render_prompt, the way a real Harn project is laid out.",
      copy: "Copy",
      copied: "Copied",
      copyAria: "Copy file source",
      viewSource: "View source",
      readDocs: "Read the docs",
      // Keyed by scenario slug (see examples/gallery.ts).
      scenarios: {
        "review-captain": {
          tab: "Code review agent",
          title: "Review a pull request and return a verdict.",
          outcome:
            "A reviewer persona scans the diff, asks one clarifying question, then returns a structured verdict with reasoning and a cost rollup. It runs fully offline against a recorded tape.",
        },
        "merge-captain": {
          tab: "PR triage",
          title: "Triage a queue of pull requests.",
          outcome:
            "Three mocked PRs become a structured merge, handoff, or block receipt using the bundled LLM tape.",
        },
        "mcp-host": {
          tab: "MCP server",
          title: "Host supervised MCP servers.",
          outcome:
            "Lazy registration, status snapshots, and graceful stop paths stay runnable without a live server in the fixture set.",
        },
        "stdlib-toolkit": {
          tab: "Prompt toolkit",
          title: "Build a prompt context from stdlib primitives.",
          outcome:
            "Clone, merge, dedupe, XML round-trip, wrap, and indent steps compose into a checked prompt receipt.",
        },
      },
    },

    features: {
      sectionTitle: "The agent runtime, built into the language",
      sectionBody:
        "Orchestration, safety, and observability are primitives in Harn and its standard library, so they compose instead of fighting each other.",
      pipelines: {
        title: "Pipelines are first-class",
        description:
          "Compose work with the |> operator. Data and control flow read top to bottom, and the compiler tracks the shape of every stage.",
      },
      llms: {
        title: "LLMs and tools, built in",
        description:
          "llm_call, agent_loop, tool vaults, MCP, reranking, and ensembles are language primitives, not a bolt-on SDK you assemble by hand.",
      },
      capabilities: {
        title: "Compile-time capability safety",
        description:
          "Filesystem, network, and process access are capabilities checked before a single line runs. No surprise side effects inside an autonomous loop.",
      },
      replay: {
        title: "Deterministic replay",
        description:
          "Every run records and replays. Step back through an agent's decisions, diff two runs, and debug non-determinism out of the system.",
      },
      durable: {
        title: "Durable steps and triggers",
        description:
          "Checkpoint long-running work and resume after a crash. Fire pipelines from cron, webhooks, GitHub, Slack, and more.",
      },
      protocols: {
        title: "Protocols, natively",
        description:
          "Speak MCP, ACP, and A2A out of the box. Embed Harn in Rust, or run it as a server with harn serve.",
      },
    },

    paths: {
      sectionTitle: "Find your path",
      sectionBody: "The documentation follows Diátaxis, organized around what you are trying to do.",
      explore: "Explore",
      tutorials: {
        kicker: "Tutorials",
        title: "Learn by building",
        description:
          "Start from zero and build a working agent, MCP server, or eval pipeline step by step.",
      },
      guides: {
        kicker: "How-to guides",
        title: "Get a task done",
        description:
          "Recipes for the things you actually need: hooks, channels, pools, refactors, and more.",
      },
      reference: {
        kicker: "Reference",
        title: "Look up the details",
        description:
          "The complete language, runtime, standard-library, protocol, and CLI reference.",
      },
      explanation: {
        kicker: "Explanation",
        title: "Understand the design",
        description:
          "The reasoning behind the host boundary, sandboxing, and Harn's architectural decisions.",
      },
    },

    cta: {
      title: "Write your first pipeline",
      body: "Install the CLI, write a few lines of Harn, and run a real agent in minutes.",
      getStarted: "Get started",
      browseCookbook: "Browse the cookbook",
    },
  },

  meta: {
    landingTitle: "Harn: the pipeline-oriented language for AI agent orchestration",
    landingDescription:
      "Harn is a pipeline-oriented language for building, orchestrating, and operating AI agents. Tutorials, guides, and a complete language and runtime reference.",
    notFoundTitle: "Page not found | {siteName}",
    notFoundDescription: "The requested Harn documentation page could not be found.",
    docTitle: "{title} | {siteName}",
  },
}
