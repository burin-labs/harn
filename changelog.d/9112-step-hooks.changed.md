Annotated-step hooks now run through the interpreter's required native runtime.
Ordinary function callbacks bypass the async hook adapters, while manifest and
registered hooks keep their existing order and effect accounting.
