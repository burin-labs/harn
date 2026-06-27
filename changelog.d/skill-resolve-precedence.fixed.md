`load_skill` and `skill_who_signed` now resolve a bare-name collision
deterministically by `source` layer precedence (project > user > host, matching
the `Layer` priority table) instead of failing the turn with an "ambiguous"
error. Hosts already collapse name collisions before building the registry, so
this is defense-in-depth: a registry that still contains two same-named entries
resolves to the higher-priority layer rather than erroring, keeping `load_skill`
consistent with the catalog the model sees.
