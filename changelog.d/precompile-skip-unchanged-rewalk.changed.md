Precompiling a source that imports packages walks its imports once
when package setup leaves the installed generation unchanged.
On a large package-importing tree this cuts CPU by about a fifth; artifacts are byte-identical.
