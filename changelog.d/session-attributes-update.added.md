Session attributes can now be recorded after the session exists. They were
reachable only through the create call, which made every attribute a
create-time fact: a writer that learned one later had no way to store it, and
no way to notice, because the value simply never appeared and the read came
back null. An update now merges the attributes it names into the ones already
stored, leaving every key it does not name alone, so a caller that knows one
fact does not have to read the others back to avoid erasing them. Clearing an
attribute stays outside the update contract, exactly as it already does for the
typed fields.
