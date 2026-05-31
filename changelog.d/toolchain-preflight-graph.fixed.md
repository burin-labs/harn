- **Harn preflight reuses the existing module graph for re-export checks.**
  `harn check` avoids rebuilding a module graph for every file while scanning
  re-export conflicts.
