**Skill frontmatter metadata survives startup discovery (#11423).**
`SkillManifest` now exposes `metadata: Option<BTreeMap<String, String>>`,
and both full skill entries and metadata-only discovery entries preserve it.
Metadata is an optional opaque string map; non-string values are rejected at
the frontmatter boundary. Metadata does not grant tool permissions.

Migration: add `metadata: None` to exhaustive `SkillManifest` struct literals,
or use `..SkillManifest::default()` when its remaining defaults are appropriate.
Skill authors must use string values in metadata, quoting numeric or boolean
values when they are intended as strings. Absent or null metadata stays absent.
