- The budget governor now counts progress the way the agent loop does.
  Re-reading the same thing no longer keeps a run alive past its
  checkpoint, and a turn that only reports no longer cuts it. The post-turn
  payload carries this as `progressing`.
