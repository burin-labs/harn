ACP session creation, loading, and resuming now bind ordered additional directory grants to the canonical workspace roots
and persist their listing metadata. Invalid replacements preserve the prior grant, omitted lists clear prior grants on
restore, and capacity pressure refuses new sessions instead of evicting a still-open client's runtime state.
