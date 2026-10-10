require("../../../scripts/ci/rust_cache_generation.cjs")
  .finish()
  .catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
