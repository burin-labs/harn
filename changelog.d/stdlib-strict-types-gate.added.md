- Added a CI ratchet (`make check-stdlib-strict-types`) that fails when any
  stdlib `.harn` field-accesses an unvalidated boundary value (HARN-OWN-004)
  outside a documented frontier exclusion list.
