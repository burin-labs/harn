Run-tape branches can apply revision-checked literal prompt edits after their
recorded prefix, including included templates and embedded standard-library
assets, without changing source files. Branch reports distinguish prepared
renders from proven use in a model request and refuse edits never loaded.

Migration:

Rust hosts constructing `BranchPlan` must set `prompt_edits`; use an empty vector
for an unedited control. `BranchReport` adds `prompt_edits`, containing each
source location and its `prepared_renders` count. Consumers must not treat that
count as treatment liveness or infer model-quality improvements from one branch.
