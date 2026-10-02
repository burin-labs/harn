- On Windows, `checkpoint_get`, `checkpoint_exists`, and `checkpoint_list`
  now refuse a checkpoint store whose directory has been replaced by a file,
  as they already did on Linux and macOS. Previously Windows read such a store
  as empty, so a resumed pipeline could repeat work its checkpoints recorded.
