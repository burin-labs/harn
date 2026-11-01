import { landingPageMeta } from "../lib/metadata"
import { usePageMetadata } from "../hooks/usePageMetadata"
import { HeroSection } from "../components/landing/HeroSection"
import { ExamplesSection } from "../components/landing/ExamplesSection"
import { FeaturesSection } from "../components/landing/FeaturesSection"
import { PathsSection } from "../components/landing/PathsSection"
import { FinalCta } from "../components/landing/FinalCta"

export function LandingPage() {
  usePageMetadata(landingPageMeta)

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
