Session-backed run records preserve the completion owner's receipt from durable
session attributes, including after re-projection. Records explicitly report
`metadata.completion_receipt: null` when no receipt was recorded.
