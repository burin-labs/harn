- Workspace guidance discovery walks the subtree once through the ignore-aware
  `fs.walk` instead of listing and stat-ing every entry, and reads only the
  instruction files that walk saw. Directories the project ignore stack excludes
  are no longer searched for `AGENTS.md` / `CLAUDE.md`.
