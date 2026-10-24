- The Harn bump runtime skips a base-sync merge on the bump branch only when
  its tree matches a mechanical merge of its parents. A merge that carries a
  hand conflict resolution or another edit is refused with `repair_conflict`,
  naming the paths that differ, instead of being silently dropped.
