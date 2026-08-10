The hosted release audit now applies CI's runner-tier partition, keeping
Landlock-only process-sandbox tests on GitHub Ubuntu instead of failing a
healthy release on Blacksmith's Landlock-free image. Merge-group Rust proof
also has enough bounded runner time to build both release artifacts and finish
the complete workspace suite.
