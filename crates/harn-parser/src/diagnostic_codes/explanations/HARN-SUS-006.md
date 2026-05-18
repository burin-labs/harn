# HARN-SUS-006 — concurrent resume changed the worker before resume could complete

A resume operation started while the worker looked suspended, but another
operator, trigger, or timeout changed the worker state before the resume could
finish. Harn rejects the later resume so callers do not accidentally treat a
race as a successful wake-up.

Refresh the worker summary and retry only if the worker is still suspended.
For trigger-driven resumes, ensure only one trigger or timeout path remains
armed for the suspension.
