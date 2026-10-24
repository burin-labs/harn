Mini-model suite, reasoning-matrix, and planner drivers now use Harn instead of shell orchestration.
They reuse the repository runtime, keep output paths and task state explicit, and report measured failures.
Matrix environment files use literal values, and the runner no longer unloads shared model servers.
