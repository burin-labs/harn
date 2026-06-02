import type { ReactNode } from "react"
import { Link } from "react-router"
import { Logo } from "./Logo"

const GITHUB = "https://github.com/burin-labs/harn"

export function Footer() {
  const year = new Date().getFullYear()
  return (
    <footer className="border-t border-border bg-surface-secondary">
      <div className="mx-auto max-w-6xl px-4 py-12 sm:px-6 lg:px-8">
        <div className="grid grid-cols-2 gap-8 sm:grid-cols-4">
          <div className="col-span-2 sm:col-span-1">
            <Link to="/" className="flex items-center gap-2">
              <Logo className="h-7 w-7" />
              <span className="text-base font-semibold text-foreground">Harn</span>
            </Link>
            <p className="mt-3 max-w-xs text-sm text-foreground-muted">
              The pipeline-oriented language for building and operating AI agents.
            </p>
          </div>
          <FooterCol title="Documentation">
            <FooterLink to="/introduction.html">Introduction</FooterLink>
            <FooterLink to="/getting-started.html">Getting started</FooterLink>
            <FooterLink to="/language-basics.html">Language reference</FooterLink>
            <FooterLink to="/cookbook.html">Cookbook</FooterLink>
          </FooterCol>
          <FooterCol title="Project">
            <FooterExternal href={GITHUB}>GitHub</FooterExternal>
            <FooterExternal href={`${GITHUB}/releases`}>Releases</FooterExternal>
            <FooterLink to="/playground.html">Playground</FooterLink>
            <FooterExternal href="https://burincode.com">Burin Code</FooterExternal>
          </FooterCol>
          <FooterCol title="Community">
            <FooterExternal href={`${GITHUB}/issues`}>Issues</FooterExternal>
            <FooterExternal href={`${GITHUB}/discussions`}>Discussions</FooterExternal>
            <FooterLink to="/contributing/preset-hooks.html">Contributing</FooterLink>
          </FooterCol>
        </div>
        <div className="mt-10 border-t border-border pt-6">
          <p className="text-center text-sm text-foreground-muted">
            &copy; {year} Burin Labs. Harn is open source.
          </p>
        </div>
      </div>
    </footer>
  )
}

function FooterCol({ title, children }: { title: string; children: ReactNode }) {
  return (
    <div>
      <h3 className="text-sm font-semibold text-foreground">{title}</h3>
      <ul className="mt-3 space-y-2">{children}</ul>
    </div>
  )
}

function FooterLink({ to, children }: { to: string; children: ReactNode }) {
  return (
    <li>
      <Link
        to={to}
        className="text-sm text-foreground-secondary transition-colors hover:text-foreground"
      >
        {children}
      </Link>
    </li>
  )
}

function FooterExternal({ href, children }: { href: string; children: ReactNode }) {
  return (
    <li>
      <a
        href={href}
        target="_blank"
        rel="noopener noreferrer"
        className="text-sm text-foreground-secondary transition-colors hover:text-foreground"
      >
        {children}
      </a>
    </li>
  )
}
