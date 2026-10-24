- The Harn bump runtime skips a base-sync merge on the bump branch only when
  skipping it loses nothing: every path where the merge departs from a
  mechanical merge of its parents must match the refresh output or the base.
  A merge that keeps any other content, such as a hand edit, is refused with
  `repair_conflict`, naming those paths, instead of being silently dropped.
