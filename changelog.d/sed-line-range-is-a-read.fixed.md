- The command workspace-effect classifier now reads a line-addressed `sed`
  print (`sed -n '1,105p' file`, `sed 3q file`) as a read. It stays
  unrecognized with an in-place flag, a script file, a `w`/`e` command, a
  substitution, or a regex address, so any `sed` that can write or execute is
  still kept out of the observation phase.
