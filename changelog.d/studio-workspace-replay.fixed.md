Replay workspace-location reads and resolve recorded filesystem paths into the
restored workspace. Refuse mapped paths that escape through symlinks or parent
components, and refuse live commands in relocated replay workspaces until a host
filesystem namespace can preserve their path semantics.
