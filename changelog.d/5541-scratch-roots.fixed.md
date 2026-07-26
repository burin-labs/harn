- Conformance cases that built scratch fixtures no longer write them into the
  repository root. Eleven cases named bare relative paths — `project_scan_*`,
  `polyglot_tree`, `meta_scratch`, `rfr_fixture.txt`, `vision_fixture.png`,
  `sample.pdf`, `media_helper_fixture.png` — which resolved against the runner's
  working directory; they now build under `harness.fs.temp_dir()`. The
  `.gitignore` block that had been hiding some of them (and two entries whose
  cases no longer exist) is gone with them.
- The sqlite init lock a conformance fixture's session store leaves behind is
  ignored alongside the `*.sqlite`, `-shm` and `-wal` files it sits next to,
  which were already ignored.
