// Annotation fixture for `harn rule test` (#2842): a line preceded by a
// ruleid marker must match the rule; an ok marker means it must not.

// ruleid: destructure-defaults
const timeout = cfg?.timeout ?? 30;

// ok: destructure-defaults
const renamed = cfg?.timeout ?? 30;

// ok: destructure-defaults
const plain = compute();
