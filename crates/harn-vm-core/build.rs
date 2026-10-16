#[path = "build_support/compiler_sources.rs"]
mod compiler_sources;

fn main() {
    harn_source_fingerprint_build::emit(
        "HARN_CORE_COMPILER_SOURCE_FINGERPRINT",
        compiler_sources::CORE_COMPILER_PATHS,
    );
}
