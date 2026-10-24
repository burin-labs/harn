- The Harn bump runtime skips a base-sync merge whose conflict resolution
  matches the refresh output or the base, instead of refusing it. A merge that
  keeps content neither the refresh nor the base has is still refused with
  `repair_conflict`, naming those paths.
