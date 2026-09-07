The CLI regression suite now renders the package and connector templates and
strict-lints their generated sources. This catches lint regressions hidden in
template strings before a newly scaffolded package fails strict verification.
