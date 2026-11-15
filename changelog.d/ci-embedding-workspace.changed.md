- Foreground embedding checks reuse the workspace dependency feature selection while still building only the standalone
  example and running its initialization controls. A Cargo unit-graph guard rejects duplicate dependency compilation.
