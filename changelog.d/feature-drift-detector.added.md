Added `scripts/check_feature_drift.sh`, an advisory diagnostic that reports
Cargo feature drift a lockfile diff cannot see: packages whose version is
unchanged between a baseline ref and the working tree but whose resolved
feature set differs. Cargo unifies features across the whole graph, so a
dependency bump can change the behavior of a crate whose version never moved.
