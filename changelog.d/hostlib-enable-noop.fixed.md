`hostlib_enable` is a no-op compatibility stub again after the typed
`HarnessTools` cutover, so legacy ambient callers do not fall through to an
embedder host bridge.
