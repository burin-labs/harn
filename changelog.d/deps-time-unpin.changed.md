- Migrate the three `harn-cli` date parsers off the deprecated
  `time::format_description::parse` to `parse_borrowed::<1>` (behavior-preserving
  version 1), letting the `time` crate float forward to 0.3.53 instead of being
  pinned back to 0.3.47 to dodge the deprecation warning under `-D warnings`.
