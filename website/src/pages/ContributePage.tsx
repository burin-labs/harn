import { format, useMessages } from "../i18n"
import { CONTRIBUTION_COMPANY } from "../i18n/en"
import { usePageMetadata } from "../hooks/usePageMetadata"
import { contributionPageMeta } from "../lib/metadata"

const GITHUB = "https://github.com/burin-labs/harn"
export const CONTRIBUTION_CHECKOUT_URL = "https://buy.stripe.com/fZubIUaPo6gv2hu5d34c800"

export function ContributePage() {
  const { contribute: t } = useMessages()
  usePageMetadata(contributionPageMeta)
  const sections = [
    { title: t.reportTitle, body: t.reportBody, link: t.reportLink, href: `${GITHUB}/issues` },
    { title: t.codeTitle, body: t.codeBody, link: t.codeLink, href: `${GITHUB}/blob/main/CONTRIBUTING.md` },
    { title: t.discussTitle, body: t.discussBody, link: t.discussLink, href: `${GITHUB}/discussions` },
  ]
  return (
    <article className="mx-auto max-w-3xl px-4 py-16 sm:px-6">
      <h1 className="text-4xl font-bold text-foreground">{t.title}</h1>
      <p className="mt-4 text-lg text-foreground-secondary">{t.intro}</p>
      <div className="mt-12 space-y-10">
        {sections.map((section) => (
          <section key={section.href}>
            <h2 className="text-xl font-semibold text-foreground">{section.title}</h2>
            <p className="mt-2 text-foreground-secondary">{section.body}</p>
            <a className="mt-3 inline-block text-accent-600 underline dark:text-accent-400" href={section.href} target="_blank" rel="noopener noreferrer">{section.link}</a>
          </section>
        ))}
      </div>
      <section className="mt-12 border-t border-border pt-8">
        <h2 className="text-xl font-semibold text-foreground">{t.tipTitle}</h2>
        <p className="mt-2 text-foreground-secondary">{format(t.tipBody, { company: CONTRIBUTION_COMPANY })}</p>
        <a className="mt-3 inline-block text-accent-600 underline dark:text-accent-400" href={CONTRIBUTION_CHECKOUT_URL} target="_blank" rel="noopener noreferrer">{t.tipLink}</a>
      </section>
    </article>
  )
}
