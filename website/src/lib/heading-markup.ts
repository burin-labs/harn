// Shared identity for the permalink control appended to section headings.
//
// The build-time plugin emits it, the client hook upgrades it, the stylesheet
// reveals it, and the content test asserts it. Keeping the strings here means a
// rename cannot leave three of those four agreeing.

/** Class on the anchor element itself. */
export const HEADING_ANCHOR_CLASS = "heading-anchor"

/** Marks a heading that carries an anchor, so CSS can scope the hover reveal. */
export const HEADING_LINKED_CLASS = "heading-linked"

/** Set on the anchor while the copied-to-clipboard confirmation is showing. */
export const HEADING_ANCHOR_COPIED_ATTR = "data-copied"

/** Heading depths that get a permalink. h1 is the page title and never does. */
export const HEADING_ANCHOR_DEPTHS = [2, 3, 4] as const

export function headingAnchorLabel(text: string): string {
  return `Permalink to “${text}”`
}
