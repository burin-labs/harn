- Ported the seven remaining queued Bash validation checks to self-hosted Harn
  scripts with byte-identical output parity: `lint_test_patterns`,
  `check_docs_cli_flags`, `check_binary_size`, `check_docs_snippets`,
  `check_docs_workflow_quickstart`, `check_docs_model_refs`, and
  `check_site_snippets`. Each has a paired `scripts/tests/<name>_test.harn`
  suite (104 new tests) and the `.sh` originals are deleted.
