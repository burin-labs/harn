Remove the external `dtolnay/rust-toolchain` workflow action dependency so
GitHub Actions hygiene cannot fail unrelated PRs when the upstream `stable`
action ref advances.
