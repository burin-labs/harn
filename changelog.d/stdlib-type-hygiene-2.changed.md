- `std/disclosure` now threads typed `DisclosureConfig` / `DisclosureContext`
  records through its internal render helpers instead of bare `dict`, while the
  raw TOML/env boundary parsers stay open. `std/connectors/github` narrows the
  `repo` (`string | dict`), `run_id`, and pull-number params (`int | string`)
  on its workflow, release, and PR helpers. No behavior change.
