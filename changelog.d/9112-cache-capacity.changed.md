The CI cache budget allows 20 GiB so a replacement workspace generation can
upload while all three current merge-gate generations remain available.
Repository and enterprise cache eviction limits must permit this capacity.
Native test executables are retained for unchanged-code reuse, and the
writer reserves space using measured generation size with growth margin.
