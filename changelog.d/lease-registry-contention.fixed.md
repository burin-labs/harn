After store initialization, host lease acquisition attempts report contention as
a typed deferral even when opening their SQLite connection encounters the lock.
Delivered guards retain cleanup across
busy releases, including nonexpiring leases, while cancellation and guard drop
leave the session thread responsive.
