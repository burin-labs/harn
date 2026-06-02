import { Link } from "react-router"

export function NotFound() {
  return (
    <div className="mx-auto flex min-h-[60vh] max-w-xl flex-col items-center justify-center px-4 text-center">
      <div className="text-6xl font-bold text-accent-500">404</div>
      <h1 className="mt-4 text-2xl font-bold text-foreground">Page not found</h1>
      <p className="mt-2 text-foreground-secondary">
        That page doesn&apos;t exist. It may have moved — try searching, or head back to the docs.
      </p>
      <div className="mt-8 flex gap-3">
        <Link
          to="/introduction.html"
          className="rounded-lg bg-gradient-to-r from-accent-600 to-accent-500 px-5 py-2.5 text-sm font-semibold text-white shadow-md shadow-accent-600/20 transition-all hover:-translate-y-0.5 dark:from-accent-500 dark:to-accent-400"
        >
          Go to docs
        </Link>
        <Link
          to="/"
          className="rounded-lg border border-accent-300 px-5 py-2.5 text-sm font-semibold text-accent-700 transition-colors hover:bg-accent-50 dark:border-accent-700 dark:text-accent-300 dark:hover:bg-accent-900/30"
        >
          Home
        </Link>
      </div>
    </div>
  )
}
