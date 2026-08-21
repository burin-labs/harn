The fenced-JSON tool dialect now accepts the everyday heredoc opener `<<TAG`
alongside the count-anchored `<<TAG:N`, and the contract teaches it. A
multi-line argument — a file body, a replacement span — can be written raw
after the JSON object inside the same ```` ```tool ```` fence and closed with a
line that is exactly `TAG`, with no escaping and no counting. `<<TAG:N` stays
the disambiguating form for a body that contains a line equal to its own
terminator.

This removes the dialect's worst failure: to write a file, a model had to
hand-serialize the whole body into one JSON string, and a single unbalanced
brace or missed `\"` discarded the entire call with no partial credit. The
capability to avoid that already existed, but only in the counted form, and the
taught contract said "never use a heredoc" — so the prompt, the exemplars, and
the parse-failure guidance all steered models into the failure. All three now
teach the verbatim body, from the one paradigm record they render from.

Two behavior notes. An argument value that is exactly `<<TAG` is now a body
declaration, so a missing body is a loud parse error instead of a literal
string — binding it as a literal would write the text `<<TAG` into the file the
call meant to fill. And `<<TAG` carries the text dialect's newline contract (the
newline before the terminator is the delimiter, not content), while `<<TAG:N`
keeps its own (each of the N lines includes its terminator); the opener says
which, and the same opener now means the same bytes in both dialects.

No parser leniency was added: every ambiguous shape — a body that never closes,
a declaration with no body, a body no argument declared, a wrong count — still
fails with zero calls.
