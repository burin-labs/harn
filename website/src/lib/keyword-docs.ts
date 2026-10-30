import { en } from "../i18n/en"
import { getMessages } from "../i18n"

export const KEYWORD_DOCS: Record<string, string> = en.keywords

export function keywordDoc(word: string): string | undefined {
  const keywords: Record<string, string> = getMessages().keywords
  return keywords[word]
}
