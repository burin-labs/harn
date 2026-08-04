import type { ReactNode } from "react"
import { Link } from "react-router"
import { Logo } from "./Logo"
import { useMessages, format } from "../i18n"

const GITHUB = "https://github.com/burin-labs/harn"

export function Footer() {
  const t = useMessages()
  const year = new Date().getFullYear()
  return (
    <footer className="border-t border-border bg-surface-secondary">
      <div className="mx-auto max-w-6xl px-4 py-12 sm:px-6 lg:px-8">
        <div className="grid grid-cols-2 gap-8 sm:grid-cols-4">
          <div className="col-span-2 sm:col-span-1">
            <Link to="/" className="flex items-center gap-2" aria-label={t.nav.brandHomeAria}>
              <Logo className="h-7 w-7" />
              <span className="text-base font-semibold text-foreground">{t.common.siteName}</span>
            </Link>
            <p className="mt-3 max-w-xs text-sm text-foreground-muted">{t.footer.tagline}</p>
          </div>
          <FooterCol title={t.footer.docsTitle}>
            <FooterLink to="/introduction.html">{t.footer.introduction}</FooterLink>
            <FooterLink to="/getting-started.html">{t.footer.gettingStarted}</FooterLink>
            <FooterLink to="/language-basics.html">{t.footer.languageReference}</FooterLink>
            <FooterLink to="/cookbook.html">{t.footer.cookbook}</FooterLink>
          </FooterCol>
          <FooterCol title={t.footer.projectTitle}>
            <FooterExternal href={GITHUB}>{t.footer.github}</FooterExternal>
            <FooterExternal href={`${GITHUB}/releases`}>{t.footer.releases}</FooterExternal>
            <FooterLink to="/playground.html">{t.footer.playground}</FooterLink>
          </FooterCol>
          <FooterCol title={t.footer.communityTitle}>
            <FooterExternal href={`${GITHUB}/issues`}>{t.footer.issues}</FooterExternal>
            <FooterExternal href={`${GITHUB}/discussions`}>{t.footer.discussions}</FooterExternal>
            <FooterLink to="/contributing/preset-hooks.html">{t.footer.contributing}</FooterLink>
          </FooterCol>
        </div>
        <div className="mt-10 border-t border-border pt-6">
          <p className="text-center text-sm text-foreground-muted">
            {format(t.footer.copyright, { year })}
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
