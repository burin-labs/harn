The executable now attaches build provenance to the linked runtime receipt.
`harn version --json` retains its compiled source revision, while the embedding
library returns an unstamped content receipt that hosts can annotate explicitly.
Changing only the build revision no longer invalidates the native runtime crate.
