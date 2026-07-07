- **Hostlib command floor.** Scoped build-directory cleanups wrapped in
  `sh -c` are no longer mistaken for project-root deletes when a later command
  in the same shell script references `.`.
