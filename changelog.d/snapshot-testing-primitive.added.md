- `std/testing` gained `assert_snapshot(name, actual, options?)`, a golden-file
  snapshot assertion in the style of Jest's `toMatchSnapshot` / insta. Goldens
  live at `__snapshots__/<name>.harn.snap` next to the test by default (override
  with `options.dir`); running with `HARN_UPDATE_SNAPSHOTS=1` writes them and
  drift fails with a unified diff. In CI (`CI`/`HARN_CI` set) the
  `HARN_UPDATE_SNAPSHOTS` trigger is ignored — the primitive compares only and
  never writes, so a leaked update flag can't turn the gate into a silent no-op.
  The explicit `options.update = true` seam stays honored (deliberate in-source
  code, used to drive the write path in tests). `options.redact` accepts
  `{pattern, replacement?}` regex scrubs applied before write/compare.
