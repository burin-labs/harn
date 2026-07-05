- **GitHub Actions spend reports now run through Harn.** The spend report
  helper keeps its existing shell entrypoint, but aggregation, sorting, repo
  filtering, JSON parsing, and table rendering now live in a Harn script with
  focused Harn tests instead of inline Python snippets.
