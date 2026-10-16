use harn_vm::llm::cache_conformance::{
    CACHE_CONFORMANCE_SCHEMA_VERSION, CacheConformanceBucketCounts, CacheConformanceFixture,
    CacheConformanceClassification, CacheConformanceReport, CacheConformanceRun,
    CacheControlProfile, CacheRequestIdentity, CacheVerdict, NormalizedCacheUsage,
    PromptCacheSupport, PromptCacheSupportStatus, classify_cache_run, report_from_runs,
};
use harn_vm::llm::usage::ReportedCacheUsage;

fn main() {
    let _: Option<(
        CacheConformanceBucketCounts, CacheConformanceClassification, CacheConformanceReport,
        CacheConformanceRun, CacheControlProfile, CacheRequestIdentity, CacheVerdict,
        NormalizedCacheUsage, PromptCacheSupport, PromptCacheSupportStatus,
    )> = None;
    let _ = classify_cache_run;
    let _ = report_from_runs;
    let _: u32 = CACHE_CONFORMANCE_SCHEMA_VERSION;
    let _ = ReportedCacheUsage { read_tokens: Some(0), write_tokens: None };
    let _ = CacheConformanceFixture::parse("openai", "gpt-5.4", "[]");
}
