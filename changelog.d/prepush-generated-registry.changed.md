`make check-generated-registry` now runs through a buildless Python auditor, so
hook-only pushes and release recovery paths no longer compile Harn just to check
Makefile/workflow/hook registry drift.
