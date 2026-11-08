Pin conformance fixture sleep exceptions by exact finding count. New literal
sleeps no longer inherit a file-wide exception, and removing waits or moving
to a mock clock requires retiring the old baseline row.
