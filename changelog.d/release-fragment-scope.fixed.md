Release finalization now uses the immutable candidate range to distinguish
candidate-owned changelog fragments from later fragments that belong to the
next release. A busy main branch no longer strands an already-tagged release,
while unresolved or candidate-owned fragments still fail closed.
