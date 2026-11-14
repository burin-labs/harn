Host lease acquisition now reports registry contention as a typed deferral even
when opening SQLite encounters the lock. Delivered guards retain cleanup across
busy releases, including nonexpiring leases, while cancellation and guard drop
leave the session thread responsive.
