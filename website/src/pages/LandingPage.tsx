import type { ReactNode } from "react"
import { Link } from "react-router"
import { ExampleGallery } from "../components/ExampleGallery"
import { HarnMockup } from "../components/HarnMockup"

const GITHUB = "https://github.com/burin-labs/harn"

export function LandingPage() {
  return (
    <div>
      {/* Hero */}
      <section className="relative overflow-hidden border-b border-border">
        {/* Single restrained brand wash — no drifting neon blobs. */}
        <div
          aria-hidden="true"
          className="pointer-events-none absolute inset-x-0 top-0 h-[420px] bg-gradient-to-b from-accent-50/70 to-transparent dark:from-accent-950/30"
        />
        <div className="relative mx-auto max-w-5xl px-4 pt-20 pb-16 sm:px-6 sm:pt-28 lg:px-8">
          <div className="mx-auto max-w-3xl text-center">
            <div className="mb-6 inline-flex animate-fade-up items-center gap-2 rounded-full border border-border bg-surface-secondary px-3.5 py-1.5 text-sm font-medium text-foreground-secondary">
              <span className="h-1.5 w-1.5 rounded-full bg-accent-500" />
              Open source · Built in Rust
            </div>
            <h1 className="animate-fade-up-delay-1 text-4xl font-bold tracking-tight text-foreground sm:text-5xl lg:text-[3.5rem] lg:leading-[1.07]">
              Build and operate AI agents in one language.
            </h1>
            <p className="mx-auto mt-6 max-w-2xl animate-fade-up-delay-2 text-lg leading-relaxed text-foreground-secondary">
              Harn is a pipeline-oriented language for AI agents. LLM calls, tools, capability
              checks, durable steps, and deterministic replay are language and standard-library
              features, not SDKs you wire together yourself.
            </p>
            <div className="mt-9 flex animate-fade-up-delay-3 flex-col items-center gap-3 sm:flex-row sm:justify-center">
              <Link
                to="/getting-started.html"
                className="inline-flex w-full items-center justify-center rounded-lg bg-accent-600 px-6 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-accent-700 sm:w-auto dark:bg-accent-500 dark:hover:bg-accent-400 dark:text-accent-950"
              >
                Get started
              </Link>
              <Link
                to="/introduction.html"
                className="inline-flex w-full items-center justify-center rounded-lg border border-border-strong px-6 py-2.5 text-sm font-semibold text-foreground transition-colors hover:bg-surface-tertiary sm:w-auto"
              >
                Read the docs
              </Link>
              <a
                href={GITHUB}
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center justify-center gap-2 px-2 py-2.5 text-sm font-semibold text-foreground-secondary transition-colors hover:text-foreground"
              >
                <svg className="h-4 w-4" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
                  <path d="M12 .5C5.73.5.5 5.73.5 12c0 5.08 3.29 9.39 7.86 10.91.58.11.79-.25.79-.56v-2c-3.2.7-3.88-1.54-3.88-1.54-.53-1.34-1.3-1.7-1.3-1.7-1.06-.72.08-.71.08-.71 1.17.08 1.79 1.2 1.79 1.2 1.04 1.79 2.73 1.27 3.4.97.11-.75.41-1.27.74-1.56-2.55-.29-5.23-1.28-5.23-5.69 0-1.26.45-2.29 1.19-3.1-.12-.29-.52-1.46.11-3.05 0 0 .97-.31 3.18 1.18a11.1 11.1 0 015.79 0c2.2-1.49 3.18-1.18 3.18-1.18.63 1.59.23 2.76.11 3.05.74.81 1.19 1.84 1.19 3.1 0 4.42-2.69 5.4-5.25 5.68.42.36.79 1.08.79 2.18v3.23c0 .31.21.68.8.56A11.51 11.51 0 0023.5 12C23.5 5.73 18.27.5 12 .5z" />
                </svg>
                GitHub
              </a>
            </div>
          </div>
          <div className="mt-16 animate-fade-up-delay-4">
            <HarnMockup />
          </div>
        </div>
      </section>

      {/* Examples */}
      <section className="border-b border-border bg-surface-secondary">
        <div className="mx-auto max-w-6xl px-4 py-20 sm:px-6 lg:px-8">
          <div className="mb-10 max-w-2xl">
            <h2 className="text-3xl font-bold tracking-tight text-foreground">
              Runnable examples with real receipts
            </h2>
            <p className="mt-3 text-foreground-secondary">
              The same checked scenario files ship in the CLI demo bundle and run locally with
              deterministic fixtures.
            </p>
          </div>
          <ExampleGallery />
        </div>
      </section>

      {/* Features */}
      <section className="mx-auto max-w-6xl px-4 py-20 sm:px-6 lg:px-8">
        <div className="mb-12 max-w-2xl">
          <h2 className="text-3xl font-bold tracking-tight text-foreground">
            The agent runtime, built into the language
          </h2>
          <p className="mt-3 text-foreground-secondary">
            Orchestration, safety, and observability are primitives in Harn and its standard
            library — so they compose instead of fighting each other.
          </p>
        </div>
        <div className="grid gap-px overflow-hidden rounded-xl border border-card-border bg-border sm:grid-cols-2 lg:grid-cols-3">
          {FEATURES.map((f) => (
            <FeatureCard key={f.title} {...f} />
          ))}
        </div>
      </section>

      {/* Diataxis paths */}
      <section className="border-t border-border bg-surface-secondary">
        <div className="mx-auto max-w-6xl px-4 py-20 sm:px-6 lg:px-8">
          <div className="mb-12 max-w-2xl">
            <h2 className="text-3xl font-bold tracking-tight text-foreground">Find your path</h2>
            <p className="mt-3 text-foreground-secondary">
              The documentation follows Diátaxis — organized by what you are trying to do.
            </p>
          </div>
          <div className="grid gap-5 sm:grid-cols-2 lg:grid-cols-4">
            {PATHS.map((p) => (
              <PathCard key={p.title} {...p} />
            ))}
          </div>
        </div>
      </section>

      {/* CTA */}
      <section className="border-t border-border">
        <div className="mx-auto max-w-5xl px-4 py-20 text-center sm:px-6 lg:px-8">
          <h2 className="text-2xl font-bold tracking-tight text-foreground sm:text-3xl">
            Write your first pipeline
          </h2>
          <p className="mx-auto mt-3 max-w-xl text-foreground-secondary">
            Install the CLI, write a few lines of Harn, and run a real agent in minutes.
          </p>
          <div className="mt-8 flex flex-col items-center gap-3 sm:flex-row sm:justify-center">
            <Link
              to="/getting-started.html"
              className="inline-flex items-center justify-center rounded-lg bg-accent-600 px-6 py-2.5 text-sm font-semibold text-white transition-colors hover:bg-accent-700 dark:bg-accent-500 dark:hover:bg-accent-400 dark:text-accent-950"
            >
              Get started
            </Link>
            <Link
              to="/cookbook.html"
              className="inline-flex items-center justify-center rounded-lg border border-border-strong px-6 py-2.5 text-sm font-semibold text-foreground transition-colors hover:bg-surface-tertiary"
            >
              Browse the cookbook
            </Link>
          </div>
        </div>
      </section>
    </div>
  )
}

function FeatureCard({ icon, title, description }: Feature) {
  return (
    <div className="bg-card-bg p-6">
      <div className="mb-3 text-accent-600 dark:text-accent-400">{icon}</div>
      <h3 className="mb-1.5 text-base font-semibold text-foreground">{title}</h3>
      <p className="text-sm leading-relaxed text-foreground-secondary">{description}</p>
    </div>
  )
}

function PathCard({ title, description, to, kicker }: Path) {
  return (
    <Link
      to={to}
      className="group flex flex-col rounded-xl border border-card-border bg-card-bg p-5 transition-colors hover:border-accent-400"
    >
      <div className="text-[11px] font-semibold uppercase tracking-wider text-accent-600 dark:text-accent-400">
        {kicker}
      </div>
      <h3 className="mt-1 text-base font-semibold text-foreground">{title}</h3>
      <p className="mt-2 flex-1 text-sm leading-relaxed text-foreground-secondary">{description}</p>
      <span className="mt-4 inline-flex items-center gap-1 text-sm font-medium text-accent-700 dark:text-accent-300">
        Explore
        <svg
          className="h-4 w-4 transition-transform group-hover:translate-x-0.5"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
        >
          <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M9 5l7 7-7 7" />
        </svg>
      </span>
    </Link>
  )
}

interface Feature {
  icon: ReactNode
  title: string
  description: string
}
interface Path {
  kicker: string
  title: string
  description: string
  to: string
}

function Icon({ d }: { d: string }) {
  return (
    <svg
      className="h-6 w-6"
      fill="none"
      viewBox="0 0 24 24"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={d} />
    </svg>
  )
}

const FEATURES: Feature[] = [
  {
    icon: <Icon d="M5 7H4a2 2 0 00-2 2v6a2 2 0 002 2h1m14 0h1a2 2 0 002-2V9a2 2 0 00-2-2h-1M9 12h6m0 0l-2-2m2 2l-2 2" />,
    title: "Pipelines are first-class",
    description:
      "Compose work with the |> operator. Data and control flow read top to bottom, and the compiler tracks the shape of every stage.",
  },
  {
    icon: <Icon d="M9 3v2m6-2v2M9 19v2m6-2v2M5 9H3m2 6H3m18-6h-2m2 6h-2M7 7h10v10H7z" />,
    title: "LLMs and tools, built in",
    description:
      "llm_call, agent_loop, tool vaults, MCP, reranking, and ensembles are language primitives, not a bolt-on SDK you assemble by hand.",
  },
  {
    icon: <Icon d="M12 3l7 3v5c0 4.4-3 8.2-7 10-4-1.8-7-5.6-7-10V6l7-3z" />,
    title: "Compile-time capability safety",
    description:
      "Filesystem, network, and process access are capabilities checked before a single line runs. No surprise side effects inside an autonomous loop.",
  },
  {
    icon: <Icon d="M3 12a9 9 0 109-9 9 9 0 00-7 3.3M3 4v3.3h3.3M12 8v4l3 2" />,
    title: "Deterministic replay",
    description:
      "Every run records and replays. Step back through an agent's decisions, diff two runs, and debug non-determinism out of the system.",
  },
  {
    icon: <Icon d="M4 7h16M4 12h16M4 17h10M19 15l2 2-2 2" />,
    title: "Durable steps and triggers",
    description:
      "Checkpoint long-running work and resume after a crash. Fire pipelines from cron, webhooks, GitHub, Slack, and more.",
  },
  {
    icon: <Icon d="M14 7l3-3a3 3 0 014 4l-3 3m-9 9l-3 3a3 3 0 01-4-4l3-3m1-5l6 6" />,
    title: "Protocols, natively",
    description:
      "Speak MCP, ACP, and A2A out of the box. Embed Harn in Rust, or run it as a server with harn serve.",
  },
]

const PATHS: Path[] = [
  {
    kicker: "Tutorials",
    title: "Learn by building",
    description: "Start from zero and build a working agent, MCP server, or eval pipeline step by step.",
    to: "/getting-started.html",
  },
  {
    kicker: "How-to guides",
    title: "Get a task done",
    description: "Recipes for the things you actually need: hooks, channels, pools, refactors, and more.",
    to: "/common-tasks.html",
  },
  {
    kicker: "Reference",
    title: "Look up the details",
    description: "The complete language, runtime, standard-library, protocol, and CLI reference.",
    to: "/language-basics.html",
  },
  {
    kicker: "Explanation",
    title: "Understand the design",
    description: "The reasoning behind the host boundary, sandboxing, and Harn's architectural decisions.",
    to: "/host-boundary.html",
  },
]
