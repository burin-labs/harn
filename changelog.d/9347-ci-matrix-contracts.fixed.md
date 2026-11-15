- CI cache checks retain typed matrix rows when expanding cache families and
  checking audit jobs. Checks that require static rows now refuse matrix and
  include expressions; runner checks still inspect known axes when include
  rows are dynamic.
