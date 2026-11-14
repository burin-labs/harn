Release requests wait within their existing deadline when a newly dispatched
workflow has not reported its run purpose yet. Unreported purpose stays pending;
warm builds, mismatched promotion sources, and completed runs without purpose
remain refused.
