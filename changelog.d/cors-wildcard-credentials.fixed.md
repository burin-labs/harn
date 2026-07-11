- Stop `harn serve` panicking at router build when a CORS config pairs
  `allow_origins: ["*"]` with `allow_credentials: true`. The `"*"` list
  wildcard now suppresses credentials just like `allow_any_origin`.
