The completion judge's re-ask for an ungrounded quote now names the failure as
paraphrase and says what a quote must be. It requires `evidence_quote` to be a
span copied character for character from the transcript or tool output, sends
the judge's reading of that span to `detail` instead, and refuses an empty
quote as an answer, asking for either a span or a `continue` whose gap is named
without claiming evidence. The previous wording asked the judge to quote only
lines that were really there, which does not reach a judge that believes it is
describing what it saw.
