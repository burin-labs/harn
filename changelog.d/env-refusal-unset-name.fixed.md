A refused `harness.env` read warns only when the process has the variable, so the
refusal hid a value. Reading a name the process does not have now logs at debug,
which keeps hosts that grant every variable they set from reporting a warning
for each optional setting a script probes.
