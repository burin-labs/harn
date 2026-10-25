Release opening recovers automatically when an edited changelog fragment conflicts with a frozen release.
It waits for the correction to pass main CI, prepares a new immutable attempt, and resumes interrupted
replacement cleanup without rebuilding or changing an existing delivery grant. Source conflicts still
require inspection.
