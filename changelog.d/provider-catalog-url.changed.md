- The provider/model catalog now refreshes from `https://harnlang.com/provider-catalog/provider-catalog.json`
  (served from Harn's own site) instead of a private repo's GitHub Pages. The catalog bundled in the binary
  remains the offline default; set `HARN_PROVIDER_CATALOG_URL` to point at a different catalog.
