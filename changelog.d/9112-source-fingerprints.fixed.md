Compiler and check-cache fingerprints use source contributions from the linked
crates instead of scanning workspace sibling directories. Published packages
retain their source identities and stop rebuilding on absent workspace paths.
CLI bytecode generation still refuses a compiler that does not match the requested
source checkout. The first build with the corrected identity refreshes older
compiler cache entries.
