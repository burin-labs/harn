Owned Linux CI runners now share a pinned Kache compiler cache across their private target directories.
Cache summaries distinguish measured compiler activity from missing reports, including when targets live
outside the checkout.
The shared Rust cache action pins the manifest's dev debug profile before both cache restores and builds,
preventing inherited runner settings from changing the artifact or separating cache readers from writers.
