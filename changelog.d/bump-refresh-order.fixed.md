Runtime bump workflows now regenerate a consumer's target-version files before
applying generic Harn migrations, preventing the migrator and the repository's
generator from competing over stale generated sources.

Swift provider-catalog bindings now initialize optional model data controls in
their custom decoder, so the generated source is compile-valid after a repin.
