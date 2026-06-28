import { useEffect } from "react"
import { LANDING_PAGE_META } from "../lib/metadata"
import { HeroSection } from "../components/landing/HeroSection"
import { ExamplesSection } from "../components/landing/ExamplesSection"
import { FeaturesSection } from "../components/landing/FeaturesSection"
import { PathsSection } from "../components/landing/PathsSection"
import { FinalCta } from "../components/landing/FinalCta"

export function LandingPage() {
  useEffect(() => {
    document.title = LANDING_PAGE_META.title
  }, [])

  return (
    <div>
      <HeroSection />
      <ExamplesSection />
      <FeaturesSection />
      <PathsSection />
      <FinalCta />
    </div>
  )
}
