Crate publication stops immediately when registry state cannot be read, retains
accepted upload progress, and confirms each uploaded version is publicly visible
before advancing or reporting completion. A later recovery reuses confirmed
versions without uploading them again.
