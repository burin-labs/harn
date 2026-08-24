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
    mainNavAria: "Main",
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
    llmsTxt: "llms.txt",
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
    pagerAria: "Previous and next page",
    onThisPage: "On this page",
    editOnGitHub: "Edit this page on GitHub",
    sectionsAria: "Documentation sections",
    sidebarAria: "Documentation pages",
    breadcrumbAria: "Breadcrumb",
    onThisPageAria: "On this page",
    skipToContent: "Skip to content",
  },

  diagram: {
    expand: "Expand diagram",
    overlayAria: "Diagram viewer",
    zoomIn: "Zoom in",
    zoomOut: "Zoom out",
    resetZoom: "Fit diagram to the screen",
    resetZoomLabel: "Fit",
    close: "Close the diagram viewer",
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
      forAgents: "For agents",
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
      sectionTitle: "Complete programs, not snippets",
      sectionBody:
        "Read these for the shape rather than the subject. In each one the deterministic work is ordinary code, and the program decides when a step is worth a model call. They ship in the CLI demo bundle and run offline against recorded fixtures.",
      tablistAria: "Runnable Harn examples",
      filesAria: "Scenario files",
      multiFileNote:
        "This scenario ships more than one file. The prompts live in sibling .harn.prompt templates and load with render_prompt, the way a real Harn project is laid out.",
      copy: "Copy",
      copied: "Copied",
      copyAria: "Copy file source",
      viewSource: "View source",
      readDocs: "Read the docs",
      extendLabel: "Make it yours",
      // Keyed by scenario slug (see examples/gallery.ts). `title` names the
      // pattern rather than the fixture, because a title that describes the
      // sample data reads as a program that can only do the sample data.
      // `extend` is the answer to "how would this ever be my problem?".
      scenarios: {
        "review-captain": {
          tab: "Code review agent",
          title: "Spend the model only on the judgment call.",
          outcome:
            "Gathering the diff, counting the changed lines, and assembling the receipt are plain code. Two steps need an opinion, so only those two become model calls. The prompts live in separate template files.",
          extend:
            "Swap the hard-coded diff for a call to your forge and it reviews real pull requests. Edit the prompts without touching the program, or add a third stage that blocks the merge when the verdict comes back negative.",
        },
        "merge-captain": {
          tab: "PR triage",
          title: "Turn a queue into decisions you can act on.",
          outcome:
            "Each pull request is classified into merge, handoff, or block, and comes back as structured data rather than prose, so the next step is code and not another prompt.",
          extend:
            "Point it at your real queue and widen the verdict set. Because every decision is typed, you can route on it: auto-merge the safe ones and escalate the rest.",
        },
        "mcp-host": {
          tab: "MCP server",
          title: "Supervise tool servers as part of the program.",
          outcome:
            "Registration, status snapshots, and shutdown are ordinary statements. The lifecycle of a tool server is something the program owns rather than something the deployment does.",
          extend:
            "Register your own servers and gate them behind capability policy, so an autonomous loop cannot reach a tool you did not hand it.",
        },
        "stdlib-toolkit": {
          tab: "Prompt toolkit",
          title: "Build the context deterministically, then ask once.",
          outcome:
            "Clone, merge, dedupe, XML round-trip, wrap, and indent are library calls. The expensive part of a prompt is usually assembling it, and none of that assembly needs a model.",
          extend:
            "Feed it your own sources and keep the assembly in code. The less of the context you ask a model to reconstruct, the cheaper and more repeatable the run.",
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
      sectionBody: "The documentation is organized around what you are trying to do.",
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
        kicker: "Internals",
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
