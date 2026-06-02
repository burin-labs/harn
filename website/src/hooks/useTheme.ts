import { useCallback, useEffect, useState } from "react"

type Theme = "light" | "dark"
const STORAGE_KEY = "harnlang-theme"

// SSR-safe: the pre-hydration inline script in index.html sets the `.dark` class
// before paint (no flash). This hook reads the real class after mount and keeps
// it in sync, so server and first client render agree (both default to light).
export function useTheme() {
  const [theme, setTheme] = useState<Theme>("light")

  useEffect(() => {
    setTheme(document.documentElement.classList.contains("dark") ? "dark" : "light")
  }, [])

  const toggleTheme = useCallback(() => {
    setTheme((prev) => {
      const next: Theme = prev === "dark" ? "light" : "dark"
      document.documentElement.classList.toggle("dark", next === "dark")
      try {
        localStorage.setItem(STORAGE_KEY, next)
      } catch {
        /* storage unavailable */
      }
      return next
    })
  }, [])

  return { theme, toggleTheme }
}
