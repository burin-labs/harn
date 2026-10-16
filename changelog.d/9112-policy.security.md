Nested execution policies preserve outer host-process confinement when a child
requests the WASI profile. Flattened stage policies also preserve host read
denials and cannot replace or remove the host-selected namespace launcher.
