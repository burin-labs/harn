Generate Swift and TypeScript provider-catalog field shapes from the existing JSON Schema.
Swift public properties, unsigned fee allowances and decoding defaults remain unchanged.
TypeScript now includes the supported Bedrock batch format, permits omitted completion-review scrutiny,
and restricts recurring pricing days to the seven weekday literals already accepted by the runtime.

Migration: Read optional review scrutiny with `review.scrutiny ?? "standard"`.
Annotate weekday arrays as `HarnRecurringPricingWindow["days"]` instead of `string[]`;
the accepted values remain `mon`, `tue`, `wed`, `thu`, `fri`, `sat`, and `sun`.
